//! A re-enrolled phone takes over its predecessor's contact book through platform entries, as
//! the Android and iOS hosts send them. The successor has no source mapping for the inherited
//! book, so it must recognise the predecessor's contacts by content: every contact keeps one
//! identity, and a book that an earlier build already doubled is repaired once.
use crate::common::*;
use peppy_crypto::{EncryptedEnvelope, KeyPurpose, decrypt, derive_purpose_key, derive_root_key};
use rusqlite::Connection;
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::path::Path;
use tempfile::TempDir;

const BOOK: &str = "takeover-book";
const HINT: &[u8] = b"reinstall-stable-android-id";
/// More than the deletion policy lets one scan remove without approval (`max(10, 5%)`).
const CONTACTS: usize = 25;

#[test]
fn deleted_active_book_real_takeover_snapshot_observation() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let a_cfg = config(&dir, "n5-a", &vault);
    let b_cfg = config(&dir, "n5-b", &vault);
    let d_cfg = config(&dir, "n5-d", &vault);
    let a = unlocked(&a_cfg, &vault);
    let b = unlocked(&b_cfg, &vault);
    let d = unlocked(&d_cfg, &vault);
    let mut server = Server::default();
    call(a.capture_platform_contacts_json(
        &json!({"schema_version":1,"book":book(&a_cfg),"contacts":[entry(1)]}).to_string(),
    ));
    a.register_device_hint("android", HINT).unwrap();
    server.upload(&a);
    settle(&server, &b);
    settle(&server, &d);
    let adopted = ids(&b).into_iter().next().unwrap();
    let active_high_water = server.log.len();
    call(d.delete_contact_book(&json!({"book_id":BOOK,"revoked_device_ids":[a_cfg.device_id.to_string()],"sync_current":true}).to_string()));
    server.upload(&d);
    b.observe_server_high_water(Cursor(active_high_water as u64))
        .unwrap();
    b.register_device_hint("android", HINT).unwrap();
    assert!(matches!(
        b.contact_book_selection().unwrap(),
        ContactBookSelection::Inherited { .. }
    ));
    start_pass(&b, &b_cfg);
    scan(&b, "n5-takeover", &[entry(1), entry(2)]);
    assert!(
        ids(&b).contains(&adopted),
        "predecessor identity was adopted"
    );
    let new_id = named(&contacts(&b), "Person 02")[0]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let root = derive_root_key(PASSPHRASE, &vault.profile).unwrap();
    let key = derive_purpose_key(&root, &vault.profile, KeyPurpose::Event).unwrap();
    let payload = |e: &Envelope| {
        let (nonce, ciphertext) = e.ciphertext.split_at(24);
        let encrypted = EncryptedEnvelope {
            nonce: nonce.try_into().unwrap(),
            ciphertext: ciphertext.to_vec(),
        };
        serde_json::from_slice::<PrivatePayload>(
            &decrypt(&key, &e.aad_bytes().unwrap(), &encrypted).unwrap(),
        )
        .unwrap()
    };
    assert!(!b.pending_outbox().unwrap().iter().any(|e| matches!(payload(e), PrivatePayload::ContactUpserted { contact, .. } if contact["id"] == adopted)));
    server.upload(&b);
    settle(&server, &b);
    let b_fences: i64 = sqlcipher(&b_cfg.database_path)
        .query_row(
            "SELECT COUNT(*) FROM deleted_contact_books WHERE book_id=?",
            [BOOK],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(b_fences, 0, "B ignores A's deletion after succession");
    settle(&server, &d);
    let observe = |client: &Client| {
        let books = call(client.list_contact_books_json());
        (books["books"].clone(), ids(client))
    };
    let live = observe(&d);
    assert_eq!(live.0.as_array().unwrap().len(), 1);
    assert_eq!(live.0[0]["id"], BOOK);
    assert_eq!(live.0[0]["owner_device_id"], b_cfg.device_id.to_string());
    assert_eq!(live.1, BTreeSet::from([new_id.clone()]));
    let retained = crate::snapshot_projection::compact(&server.log, false);
    let session = d
        .begin_snapshot_with_compaction(
            Cursor(server.log.len() as u64),
            retained.len() as u64,
            SnapshotPurpose::Resync,
            Some(99),
        )
        .unwrap();
    for page in retained.chunks(2) {
        d.append_snapshot_page(session.generation, page).unwrap();
    }
    d.finish_snapshot(session.generation).unwrap();
    assert_eq!(crate::snapshot_projection::settle(&d).quarantined, 0);
    let snapshot = observe(&d);
    assert_eq!(snapshot, live);
}

fn call(result: Result<String, Error>) -> Value {
    serde_json::from_str(&result.unwrap()).unwrap()
}

fn book(owner: &ClientConfig) -> Value {
    json!({
        "id": BOOK,
        "owner_device_id": owner.device_id.to_string(),
        "generation": "1",
        "state": "active",
        "region": "US"
    })
}

fn phone(n: usize) -> String {
    format!("+12025550{:03}", 100 + n)
}

/// The platform entry of OS contact `n`. The OS contact database survives an app reinstall, so
/// both enrollments see the same raw ids and provider item ids.
fn entry(n: usize) -> Value {
    json!({
        "source_key": format!("raw:{n}"),
        "fields": {
            "display_name": format!("Person {n:02}"),
            "name": {"given": "Person", "family": format!("{n:02}")},
            "phones": [{"id": format!("data:{n}1"), "label": "mobile", "value": phone(n)}],
            "emails": [{"id": format!("data:{n}2"), "label": "home", "value": format!("person{n}@example.test")}],
            "addresses": []
        },
        "provenance": {
            "account_id": "account",
            "read_only": false,
            "raw_ids": [n.to_string()],
            "lookup_hint": format!("lookup-{n}")
        }
    })
}

fn entries(numbers: impl IntoIterator<Item = usize>) -> Vec<Value> {
    numbers.into_iter().map(entry).collect()
}

fn sqlcipher(path: &Path) -> Connection {
    let connection = Connection::open(path).unwrap();
    connection
        .execute_batch(
            "PRAGMA key = \"x'0707070707070707070707070707070707070707070707070707070707070707'\";",
        )
        .unwrap();
    connection
}

fn settle(server: &Server, client: &Client) {
    assert_eq!(server.sync(client).quarantined, 0);
    loop {
        let step = client.apply_pending(1000).unwrap();
        assert_eq!(step.quarantined, 0);
        if step.applied + step.drained + step.superseded == 0 && step.snapshot_remaining == 0 {
            return;
        }
    }
}

fn listed(client: &Client) -> Value {
    call(client.list_contact_books_json())["books"]
        .as_array()
        .unwrap()
        .iter()
        .find(|book| book["id"] == BOOK)
        .cloned()
        .unwrap()
}

/// Live contacts of the book, keyed by display name.
fn contacts(client: &Client) -> Vec<Value> {
    call(client.contact_book_view(
        &json!({"schema_version": 1, "book_id": BOOK, "limit": 200}).to_string(),
    ))["contacts"]
        .as_array()
        .unwrap()
        .clone()
}

fn ids(client: &Client) -> BTreeSet<String> {
    contacts(client)
        .iter()
        .map(|contact| contact["id"].as_str().unwrap().to_owned())
        .collect()
}

fn named<'a>(contacts: &'a [Value], name: &str) -> Vec<&'a Value> {
    contacts
        .iter()
        .filter(|contact| contact["display_name"] == name)
        .collect()
}

fn restorable(client: &Client) -> Vec<Value> {
    call(client.list_restorable_contacts_json(&json!({"book_id": BOOK}).to_string()))["contacts"]
        .as_array()
        .unwrap()
        .clone()
}

fn resolved(client: &Client, address: &str) -> Value {
    let request = json!({"addresses": [address]}).to_string();
    loop {
        let result = call(client.resolve_contact_addresses_json(&request));
        if result["more_indexing"] == false {
            return result;
        }
    }
}

fn photo(client: &Client, dir: &TempDir, name: &str, shade: u8) -> AttachmentId {
    let path = dir.path().join(name);
    image::RgbImage::from_pixel(64, 64, image::Rgb([shade, 10, 10]))
        .save_with_format(&path, image::ImageFormat::Png)
        .unwrap();
    client.prepare_contact_photo(&path).unwrap().attachment_id
}

fn with_photo(mut entry: Value, attachment: AttachmentId) -> Value {
    entry["fields"]["photo"] = json!({"attachment_id": attachment.to_string()});
    entry["provenance"]["photo_source_hash"] = json!("sha256:photo");
    entry
}

/// Completes the photo upload, which releases the contact record that references it.
fn upload_photo(client: &Client, attachment: AttachmentId) {
    client
        .mark_attachment_uploaded(attachment, &uuid::Uuid::new_v4().to_string())
        .unwrap();
    register_photo_references(client);
}

/// What a host does before it uploads records that reference a photo, removals included.
fn register_photo_references(client: &Client) {
    let state = call(client.contact_photo_transfer_state_json());
    for registration in state["registrations"].as_array().unwrap() {
        call(
            client.acknowledge_contact_photo_reference_json(
                &json!({"schema_version": 1, "envelope_id": registration["envelope_id"],
                "attachment_id": registration["attachment_id"]})
                .to_string(),
            ),
        );
    }
}

/// Predecessor A published the book, successor B inherited it and has not scanned yet, and
/// observer C holds the same history.
struct Succession {
    dir: TempDir,
    vault: Vault,
    b_cfg: ClientConfig,
    c_cfg: ClientConfig,
    b: Client,
    c: Client,
    server: Server,
    predecessor_ids: BTreeSet<String>,
}

/// `photo_on_first` gives the predecessor's first contact an uploaded photo.
fn succeeded(predecessor_entries: Vec<Value>, photo_on_first: bool) -> Succession {
    succeeded_after(predecessor_entries, photo_on_first, |_| {})
}

/// Like `succeeded`; `before_handover` runs on the predecessor after its first capture.
fn succeeded_after(
    mut predecessor_entries: Vec<Value>,
    photo_on_first: bool,
    before_handover: impl FnOnce(&Client),
) -> Succession {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let a_cfg = config(&dir, "predecessor", &vault);
    let b_cfg = config(&dir, "successor", &vault);
    let c_cfg = config(&dir, "observer", &vault);
    let a = unlocked(&a_cfg, &vault);
    let b = unlocked(&b_cfg, &vault);
    let c = unlocked(&c_cfg, &vault);
    let attachment = photo_on_first.then(|| photo(&a, &dir, "predecessor.png", 200));
    if let Some(attachment) = attachment {
        predecessor_entries[0] = with_photo(predecessor_entries[0].clone(), attachment);
    }
    call(
        a.capture_platform_contacts_json(
            &json!({"schema_version": 1, "book": book(&a_cfg), "contacts": predecessor_entries})
                .to_string(),
        ),
    );
    if let Some(attachment) = attachment {
        upload_photo(&a, attachment);
    }
    before_handover(&a);
    a.register_device_hint("android", HINT).unwrap();
    let mut server = Server::default();
    server.upload(&a);
    settle(&server, &b);
    b.observe_server_high_water(Cursor(server.log.len() as u64))
        .unwrap();
    b.register_device_hint("android", HINT).unwrap();
    assert_eq!(
        b.contact_book_selection().unwrap(),
        ContactBookSelection::Inherited {
            book_id: BOOK.into(),
            retire_book_ids: vec![]
        }
    );
    server.upload(&b);
    settle(&server, &c);
    let predecessor_ids = ids(&c);
    Succession {
        dir,
        vault,
        b_cfg,
        c_cfg,
        b,
        c,
        server,
        predecessor_ids,
    }
}

fn succession() -> Succession {
    succeeded(entries(1..=CONTACTS), false)
}

impl Succession {
    /// Uploads the successor's records and delivers everything to the observer.
    fn publish(&mut self) {
        register_photo_references(&self.b);
        self.server.upload(&self.b);
        settle(&self.server, &self.c);
    }
}

fn begin(client: &Client, scan_id: &str) {
    let generation = listed(client)["generation"].clone();
    call(
        client.begin_contact_scan(
            &json!({"schema_version": 1, "scan_id": scan_id, "book_id": BOOK,
            "generation": generation, "access": "full", "authoritative": true})
            .to_string(),
        ),
    );
}

fn observe(client: &Client, scan_id: &str, observed: &[Value]) {
    for entry in observed {
        call(
            client.observe_contact_scan(&json!({"scan_id": scan_id, "source": entry}).to_string()),
        );
    }
}

fn finish(client: &Client, scan_id: &str) -> Value {
    call(client.finish_contact_scan(
        &json!({"schema_version": 1, "scan_id": scan_id, "complete": true}).to_string(),
    ))
}

/// One complete authoritative full scan that observes `observed`.
fn scan(client: &Client, scan_id: &str, observed: &[Value]) -> Value {
    begin(client, scan_id);
    observe(client, scan_id, observed);
    finish(client, scan_id)
}

/// What a host does at the start of every contacts pass.
fn start_pass(client: &Client, owner: &ClientConfig) -> (Value, ContactBookSelection) {
    let retirement = call(client.retire_superseded_contact_books());
    let selection = client.contact_book_selection().unwrap();
    call(client.capture_platform_contacts_json(
        &json!({"schema_version": 1, "book": book(owner), "contacts": []}).to_string(),
    ));
    (retirement, selection)
}

fn pending_holds(config: &ClientConfig) -> i64 {
    sqlcipher(&config.database_path)
        .query_row(
            "SELECT COUNT(*) FROM contact_scan_holds WHERE book_id=? AND status='awaiting_approval'",
            [BOOK],
            |row| row.get(0),
        )
        .unwrap()
}

#[test]
fn takeover_keeps_every_predecessor_contact_identity() {
    let mut t = succession();
    start_pass(&t.b, &t.b_cfg);

    let finished = scan(&t.b, "takeover", &entries(1..=CONTACTS));

    assert_eq!(finished["held"], 0);
    assert_eq!(finished["deleted"], 0);
    assert_eq!(ids(&t.b), t.predecessor_ids);
    assert!(
        contacts(&t.b)
            .iter()
            .all(|contact| contact["revision"] == "1"),
        "an unchanged contact is not republished"
    );
    assert_eq!(
        t.b.contact_book_selection().unwrap(),
        ContactBookSelection::Owned {
            book_id: BOOK.into()
        }
    );
    // The successor now maps the provider contact and its items to the inherited identities.
    let predecessor = named(&contacts(&t.c), "Person 03")[0].clone();
    let context = call(t.b.contact_source_context_json(
        &json!({"schema_version": 1, "book_id": BOOK, "source_key": "raw:3"}).to_string(),
    ));
    assert_eq!(context["contact_id"], predecessor["id"]);
    let field_ids: BTreeSet<&str> = context["field_sources"]
        .as_array()
        .unwrap()
        .iter()
        .map(|field| field["id"].as_str().unwrap())
        .collect();
    assert!(field_ids.contains(predecessor["phones"][0]["id"].as_str().unwrap()));
    assert!(field_ids.contains(predecessor["emails"][0]["id"].as_str().unwrap()));

    t.publish();
    assert_eq!(listed(&t.c)["contact_count"], CONTACTS);
    assert_eq!(ids(&t.c), t.predecessor_ids);
    let matches = resolved(&t.c, &phone(3))["matches"].clone();
    assert_eq!(matches.as_array().unwrap().len(), 1);
    assert_eq!(matches[0]["contact_id"], predecessor["id"]);
}

#[test]
fn takeover_recognises_a_contact_whose_phone_copy_gained_content() {
    let mut t = succession();
    let mut observed = entries(1..=CONTACTS);
    observed[4]["fields"]["phones"][0]["label"] = json!("work");
    observed[4]["fields"]["notes"] = json!("Met at the conference");
    start_pass(&t.b, &t.b_cfg);

    let finished = scan(&t.b, "takeover", &observed);

    assert_eq!(finished["held"], 0);
    assert_eq!(finished["deleted"], 0);
    assert_eq!(ids(&t.b), t.predecessor_ids);
    let changed = named(&contacts(&t.b), "Person 05")[0].clone();
    assert_eq!(changed["revision"], "2");
    assert_eq!(changed["notes"], "Met at the conference");
    t.publish();
    assert_eq!(ids(&t.c), t.predecessor_ids);
    assert!(restorable(&t.c).is_empty());
}

#[test]
fn takeover_keeps_predecessor_content_the_phone_lacks_restorable() {
    let mut predecessor = entries(1..=CONTACTS);
    predecessor[0]["fields"]["notes"] = json!("Gate code 4411");
    let mut t = succeeded(predecessor, false);
    start_pass(&t.b, &t.b_cfg);

    let finished = scan(&t.b, "takeover", &entries(1..=CONTACTS));

    // The phone's copy has no note, so the predecessor's copy is not silently overwritten: it
    // is an ordinary removal (one contact, within the policy) that stays restorable.
    assert_eq!(finished["held"], 0);
    assert_eq!(finished["deleted"], 1);
    assert_eq!(listed(&t.b)["contact_count"], CONTACTS);
    t.publish();
    assert_eq!(named(&contacts(&t.c), "Person 01").len(), 1);
    let kept = restorable(&t.c);
    assert_eq!(kept.len(), 1);
    assert_eq!(kept[0]["display_name"], "Person 01");
}

#[test]
fn takeover_never_overwrites_a_note_that_differs_only_in_case() {
    let mut predecessor = entries(1..=CONTACTS);
    predecessor[0]["fields"]["notes"] = json!("Password: AbC");
    let mut observed = entries(1..=CONTACTS);
    observed[0]["fields"]["notes"] = json!("Password: abc");
    let mut t = succeeded(predecessor, false);
    start_pass(&t.b, &t.b_cfg);

    let finished = scan(&t.b, "takeover", &observed);

    // The two notes are different text, so the predecessor's copy is a removal that stays
    // restorable, not an older version of the phone's copy.
    assert_eq!(finished["deleted"], 1);
    t.publish();
    assert_eq!(
        named(&contacts(&t.c), "Person 01")[0]["notes"],
        "Password: abc"
    );
    assert_eq!(restorable(&t.c).len(), 1);
}

#[test]
fn a_contact_the_predecessor_deleted_stays_restorable_next_to_its_identical_twin() {
    let twin = |key: &str| {
        let mut twin = entry(1);
        twin["source_key"] = json!(key);
        twin
    };
    let mut both = entries(2..=CONTACTS);
    both.extend([twin("raw:101"), twin("raw:102")]);
    let kept_on_phone = both[..both.len() - 1].to_vec();
    // The user deleted one of two identical contacts before the phone was re-enrolled.
    let mut t = succeeded_after(both, false, |a| {
        assert_eq!(scan(a, "before-handover", &kept_on_phone)["deleted"], 1);
    });
    assert_eq!(restorable(&t.c).len(), 1);
    start_pass(&t.b, &t.b_cfg);

    let finished = scan(&t.b, "takeover", &kept_on_phone);

    assert_eq!(finished["relinked"], 0);
    assert_eq!(finished["deleted"], 0);
    assert_eq!(ids(&t.b), t.predecessor_ids);
    t.publish();
    assert_eq!(
        restorable(&t.c).len(),
        1,
        "the deleted contact was a contact of its own, not a copy of the one that remains"
    );
}

#[test]
fn takeover_mints_contacts_new_on_the_phone_and_removes_absent_ones() {
    let mut t = succession();
    start_pass(&t.b, &t.b_cfg);

    let finished = scan(&t.b, "takeover", &entries(2..=CONTACTS + 1));

    assert_eq!(finished["held"], 0);
    assert_eq!(finished["deleted"], 1);
    t.publish();
    let observer = contacts(&t.c);
    assert_eq!(observer.len(), CONTACTS);
    assert!(named(&observer, "Person 01").is_empty());
    let added = named(&observer, &format!("Person {:02}", CONTACTS + 1))[0];
    assert!(!t.predecessor_ids.contains(added["id"].as_str().unwrap()));
    assert_eq!(
        ids(&t.c).intersection(&t.predecessor_ids).count(),
        CONTACTS - 1
    );
    // The contact deleted on the phone is an ordinary removal.
    assert_eq!(restorable(&t.c).len(), 1);
}

#[test]
fn takeover_resumed_after_a_restart_claims_each_contact_once() {
    let t = succession();
    let all = entries(1..=CONTACTS);
    start_pass(&t.b, &t.b_cfg);
    begin(&t.b, "takeover");
    observe(&t.b, "takeover", &all[..12]);
    drop(t.b);

    let reopened = unlocked(&t.b_cfg, &t.vault);
    // The host resumes at its checkpoint and observes the last page again.
    observe(&reopened, "takeover", &all[9..]);
    let finished = finish(&reopened, "takeover");

    assert_eq!(finished["held"], 0);
    assert_eq!(finished["deleted"], 0);
    assert_eq!(ids(&reopened), t.predecessor_ids);
}

#[test]
fn a_platform_capture_during_the_takeover_keeps_the_predecessor_identity() {
    let t = succession();
    start_pass(&t.b, &t.b_cfg);

    // A host may capture a changed contact outside the scan, from OS change history.
    let captured = call(t.b.capture_platform_contacts_json(
        &json!({"schema_version": 1, "book": book(&t.b_cfg), "contacts": [entry(3)]}).to_string(),
    ))["contacts"][0]
        .clone();

    assert!(t.predecessor_ids.contains(captured["id"].as_str().unwrap()));
    assert_eq!(captured["changed"], false);
    assert_eq!(listed(&t.b)["contact_count"], CONTACTS);
}

#[test]
fn identical_phone_contacts_each_keep_one_predecessor_identity() {
    let twin = |key: &str| {
        let mut twin = entry(1);
        twin["source_key"] = json!(key);
        twin
    };
    let mut both = entries(2..=CONTACTS);
    both.extend([twin("raw:101"), twin("raw:102")]);
    let t = succeeded(both.clone(), false);
    start_pass(&t.b, &t.b_cfg);

    let finished = scan(&t.b, "takeover", &both);

    assert_eq!(finished["held"], 0);
    assert_eq!(finished["deleted"], 0);
    assert_eq!(ids(&t.b), t.predecessor_ids);
    assert_eq!(named(&contacts(&t.b), "Person 01").len(), 2);
}

/// The state an affected phone is in: the successor's takeover scan ran on a build without
/// content matching, so it minted a second identity for every contact, held the removal of
/// the predecessor's identities for approval and completed the takeover. Contact 1 has a
/// photo, uploaded once by each enrollment.
struct Doubled {
    t: Succession,
    phone_entries: Vec<Value>,
    hold_id: String,
}

fn doubled_with(predecessor: Vec<Value>) -> Doubled {
    let mut t = succeeded(predecessor, true);
    let successor_photo = photo(&t.b, &t.dir, "successor.png", 90);
    let mut phone_entries = entries(1..=CONTACTS);
    phone_entries[0] = with_photo(phone_entries[0].clone(), successor_photo);
    start_pass(&t.b, &t.b_cfg);
    // An earlier build neither matched by content nor tracked reconciliation. The second
    // statement fails on a build without the column, which is the same state.
    let db = sqlcipher(&t.b_cfg.database_path);
    db.execute("UPDATE inherited_contact_books SET takeover_complete=1", [])
        .unwrap();
    let _ = db.execute("UPDATE inherited_contact_books SET reconciled=1", []);
    let finished = scan(&t.b, "takeover-before-fix", &phone_entries);
    let _ = db.execute("UPDATE inherited_contact_books SET reconciled=0", []);
    assert_eq!(finished["held"], CONTACTS);
    let hold_id = finished["hold_id"].as_str().unwrap().to_owned();
    upload_photo(&t.b, successor_photo);
    t.publish();
    assert_eq!(listed(&t.c)["contact_count"], 2 * CONTACTS);
    Doubled {
        t,
        phone_entries,
        hold_id,
    }
}

fn doubled() -> Doubled {
    doubled_with(entries(1..=CONTACTS))
}

#[test]
fn a_doubled_book_is_repaired_by_the_next_contacts_pass() {
    let mut d = doubled();
    // Two candidates for one address are ambiguous: the name survives, the photo does not.
    let before = resolved(&d.t.c, &phone(1));
    assert!(before["matches"].as_array().unwrap().is_empty());
    assert!(
        before["name_matches"][0]
            .get("photo_attachment_id")
            .is_none()
    );

    let (retirement, selection) = start_pass(&d.t.b, &d.t.b_cfg);
    assert_eq!(retirement["reopened"], json!([BOOK]));
    assert_eq!(
        selection,
        ContactBookSelection::Inherited {
            book_id: BOOK.into(),
            retire_book_ids: vec![]
        },
        "the host runs the takeover full scan again"
    );
    let finished = scan(&d.t.b, "takeover-after-fix", &d.phone_entries);

    assert_eq!(finished["merged"], CONTACTS);
    assert_eq!(finished["held"], 0);
    assert_eq!(finished["deleted"], 0);
    assert_eq!(pending_holds(&d.t.b_cfg), 0);
    assert_eq!(listed(&d.t.b)["contact_count"], CONTACTS);
    d.t.publish();
    assert_eq!(listed(&d.t.c)["contact_count"], CONTACTS);
    assert!(
        ids(&d.t.c).is_disjoint(&d.t.predecessor_ids),
        "the identity the phone already maps is the one that stays"
    );
    assert!(
        restorable(&d.t.c).is_empty(),
        "a merged copy is not offered for restore: restoring it would duplicate the contact"
    );
    let observer = contacts(&d.t.c);
    let kept = named(&observer, "Person 01");
    assert_eq!(kept.len(), 1);
    assert!(kept[0]["photo"]["attachment_id"].is_string());
    let after = resolved(&d.t.c, &phone(1));
    assert_eq!(after["matches"].as_array().unwrap().len(), 1);
    assert_eq!(after["matches"][0]["contact_id"], kept[0]["id"]);
    assert_eq!(
        after["matches"][0]["photo_attachment_id"],
        kept[0]["photo"]["attachment_id"]
    );
}

#[test]
fn a_book_is_repaired_once_and_later_removals_follow_the_deletion_policy() {
    let d = doubled();
    start_pass(&d.t.b, &d.t.b_cfg);
    scan(&d.t.b, "takeover-after-fix", &d.phone_entries);

    let (retirement, selection) = start_pass(&d.t.b, &d.t.b_cfg);
    assert_eq!(retirement["reopened"], json!([]));
    assert_eq!(
        selection,
        ContactBookSelection::Owned {
            book_id: BOOK.into()
        }
    );
    // Twelve contacts disappear from the phone: more than one scan may remove unasked.
    let finished = scan(&d.t.b, "later", &d.phone_entries[12..]);
    assert_eq!(finished["held"], 12);
    assert_eq!(finished["deleted"], 0);
    assert_eq!(listed(&d.t.b)["contact_count"], CONTACTS);
}

#[test]
fn repair_keeps_predecessor_content_the_phone_lacks_restorable() {
    let mut predecessor = entries(1..=CONTACTS);
    predecessor[1]["fields"]["notes"] = json!("Gate code 4411");
    let mut d = doubled_with(predecessor);
    start_pass(&d.t.b, &d.t.b_cfg);

    let finished = scan(&d.t.b, "takeover-after-fix", &d.phone_entries);

    assert_eq!(finished["merged"], CONTACTS - 1);
    assert_eq!(finished["held"], 0);
    assert_eq!(finished["deleted"], 1);
    d.t.publish();
    assert_eq!(listed(&d.t.c)["contact_count"], CONTACTS);
    let kept = restorable(&d.t.c);
    assert_eq!(kept.len(), 1);
    assert_eq!(kept[0]["display_name"], "Person 02");
    // The copy that was not merged is still a predecessor leftover. It never opens the
    // takeover a second time.
    let (retirement, selection) = start_pass(&d.t.b, &d.t.b_cfg);
    assert_eq!(retirement["reopened"], json!([]));
    assert_eq!(
        selection,
        ContactBookSelection::Owned {
            book_id: BOOK.into()
        }
    );
}

#[test]
fn removals_of_an_approved_hold_are_linked_to_the_surviving_contacts() {
    let mut d = doubled();
    call(
        d.t.b
            .contact_approval_json(&json!({"scan_id": d.hold_id, "approve": true}).to_string()),
    );
    d.t.publish();
    // Every approved removal is offered for restore next to the contact that duplicates it.
    // The one with the predecessor's photo is still waiting for that photo before it uploads.
    assert!(restorable(&d.t.c).len() >= CONTACTS - 1);

    let (retirement, _) = start_pass(&d.t.b, &d.t.b_cfg);
    assert_eq!(retirement["reopened"], json!([BOOK]));
    let finished = scan(&d.t.b, "takeover-after-fix", &d.phone_entries);

    assert_eq!(finished["relinked"], CONTACTS);
    assert_eq!(finished["held"], 0);
    assert_eq!(finished["deleted"], 0);
    d.t.publish();
    assert_eq!(listed(&d.t.c)["contact_count"], CONTACTS);
    assert!(restorable(&d.t.c).is_empty());
}

#[test]
fn a_device_that_does_not_own_the_book_never_reopens_its_takeover() {
    let d = doubled();
    // Everything but ownership would let the observer reopen: its replay has caught up, its
    // row says the takeover completed, and it has no source mapping for any contact.
    d.t.c
        .observe_server_high_water(Cursor(d.t.server.log.len() as u64))
        .unwrap();
    assert!(d.t.c.initial_replay_caught_up().unwrap());
    sqlcipher(&d.t.c_cfg.database_path)
        .execute("UPDATE inherited_contact_books SET takeover_complete=1", [])
        .unwrap();

    let retirement = call(d.t.c.retire_superseded_contact_books());

    assert_eq!(retirement["reopened"], json!([]));
    let complete: i64 = sqlcipher(&d.t.c_cfg.database_path)
        .query_row(
            "SELECT takeover_complete FROM inherited_contact_books WHERE book_id=?",
            [BOOK],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(complete, 1);
}

#[test]
fn a_fresh_takeover_is_never_reopened() {
    let t = succession();
    start_pass(&t.b, &t.b_cfg);
    scan(&t.b, "takeover", &entries(1..=CONTACTS));

    let (retirement, selection) = start_pass(&t.b, &t.b_cfg);

    assert_eq!(retirement["reopened"], json!([]));
    assert_eq!(
        selection,
        ContactBookSelection::Owned {
            book_id: BOOK.into()
        }
    );
}
