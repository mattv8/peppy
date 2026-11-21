//! Vault-wide contact book deletion: the replicated fence, per-contact records, local cleanup,
//! late-record suppression, and the tag on consumed unknown event kinds.

use crate::common::*;
use peppy_crypto::{
    EncryptedEnvelope, KeyPurpose, decrypt, derive_purpose_key, derive_root_key, encrypt,
};
use rusqlite::{Connection, params};
use serde_json::{Value, json};
use std::path::Path;
use std::time::Instant;
use tempfile::TempDir;

const BOOK: &str = "address-book";
const PHOTO: &str = "00000000-0000-4000-8000-0000000000aa";

fn book(owner: DeviceId, generation: &str) -> Value {
    json!({
        "id": BOOK,
        "owner_device_id": owner.to_string(),
        "generation": generation,
        "state": "active",
        "region": "US"
    })
}

fn contact(id: &str, name: &str) -> Value {
    let phone = if id == "ada" {
        "+12025550100"
    } else {
        "+12025550101"
    };
    json!({
        "id": id,
        "book_id": BOOK,
        "revision": "1",
        "display_name": name,
        "name": {"given": name},
        "phones": [{"id": "phone", "value": phone}],
        "emails": [],
        "addresses": []
    })
}

pub(crate) fn sqlcipher(path: &Path) -> Connection {
    let connection = Connection::open(path).unwrap();
    connection
        .execute_batch(
            "PRAGMA key = \"x'0707070707070707070707070707070707070707070707070707070707070707'\";",
        )
        .unwrap();
    connection
}

fn count(path: &Path, sql: &str) -> i64 {
    sqlcipher(path)
        .query_row(sql, [], |row| row.get(0))
        .unwrap()
}

fn book_ids(client: &Client) -> Vec<String> {
    let raw: Value = serde_json::from_str(&client.list_contact_books_json().unwrap()).unwrap();
    raw["books"]
        .as_array()
        .unwrap()
        .iter()
        .map(|book| book["id"].as_str().unwrap().to_owned())
        .collect()
}

fn sync(envelopes: &[Envelope], client: &Client) -> ApplyReport {
    let start = client.receive_cursor().unwrap().0;
    for (index, envelope) in envelopes.iter().enumerate() {
        client
            .ingest(envelope, Cursor(start + index as u64 + 1))
            .unwrap_or_else(|error| panic!("failed to ingest envelope {index}: {error:?}"));
    }
    client.apply_pending(100).unwrap()
}

fn sealed(vault: &Vault, producer: DeviceId, sequence: u64, payload: PrivatePayload) -> Envelope {
    seal_plain(
        vault,
        producer,
        sequence,
        &serde_json::to_vec(&payload).unwrap(),
    )
}

/// Seals arbitrary plaintext as an authenticated Event, so tests can emit unknown kinds.
fn seal_plain(vault: &Vault, producer: DeviceId, sequence: u64, plain: &[u8]) -> Envelope {
    let root = derive_root_key(PASSPHRASE, &vault.profile).unwrap();
    let key = derive_purpose_key(&root, &vault.profile, KeyPurpose::Event).unwrap();
    let mut envelope = Envelope {
        protocol_version: 1,
        envelope_id: EnvelopeId::new(),
        command_id: None,
        vault_id: vault.id,
        producer_device_id: producer,
        producer_sequence: SourceSequence(sequence),
        key_epoch: 1,
        crypto_suite: vault.profile.crypto_suite,
        profile_fingerprint: vault.profile.fingerprint().unwrap(),
        purpose: EnvelopePurpose::Event,
        route: None,
        compaction: None,
        origin_tag: None,
        ciphertext: Vec::new(),
    };
    let encrypted = encrypt(&key, &envelope.aad_bytes().unwrap(), plain).unwrap();
    envelope.ciphertext = [encrypted.nonce.as_slice(), &encrypted.ciphertext].concat();
    envelope
}

/// A client that holds a book from `owner`, with two contacts, and no other local state.
fn observed_book(dir: &TempDir) -> (Vault, ClientConfig, Client, DeviceId) {
    let vault = Vault::new();
    let observer_config = config(dir, "observer", &vault);
    let observer = unlocked(&observer_config, &vault);
    let owner = DeviceId::new();
    let events = [
        sealed(
            &vault,
            owner,
            1,
            PrivatePayload::ContactBookState {
                book: book(owner, "1"),
            },
        ),
        sealed(
            &vault,
            owner,
            2,
            PrivatePayload::ContactUpserted {
                book: book(owner, "1"),
                contact: contact("ada", "Ada"),
            },
        ),
        sealed(
            &vault,
            owner,
            3,
            PrivatePayload::ContactUpserted {
                book: book(owner, "1"),
                contact: contact("bea", "Bea"),
            },
        ),
    ];
    assert_eq!(sync(&events, &observer).quarantined, 0);
    assert_eq!(book_ids(&observer), vec![BOOK.to_owned()]);
    (vault, observer_config, observer, owner)
}

fn fence(owner: DeviceId, generation: &str) -> PrivatePayload {
    PrivatePayload::ContactBookDeleted {
        book_id: BOOK.into(),
        owner_device_id: owner.to_string(),
        generation: generation.into(),
        deleted_at_ms: 1_000,
    }
}

#[test]
fn non_owner_delete_removes_book_contacts_pins_exclusion_and_photo_holders() {
    let dir = TempDir::new().unwrap();
    let (vault, config, observer, owner) = observed_book(&dir);
    sqlcipher(&config.database_path)
        .execute_batch(&format!(
            "INSERT INTO contact_name_pins(key,book_id,contact_id) VALUES('ada-pin','{BOOK}','ada');
             INSERT INTO contact_book_lookup_exclusions(book_id) VALUES('{BOOK}');
             INSERT INTO contact_photo_references(holder_kind,book_id,holder_id,attachment_id,retain_until) VALUES('contact','{BOOK}','ada','{PHOTO}',NULL),('edit','{BOOK}','req-1','{PHOTO}',NULL);"
        ))
        .unwrap();

    let report = sync(
        &[sealed(&vault, DeviceId::new(), 1, fence(owner, "1"))],
        &observer,
    );

    assert_eq!(report.quarantined, 0);
    assert!(book_ids(&observer).is_empty());
    let path = &config.database_path;
    assert_eq!(count(path, "SELECT COUNT(*) FROM contact_books"), 0);
    assert_eq!(count(path, "SELECT COUNT(*) FROM contacts"), 0);
    assert_eq!(count(path, "SELECT COUNT(*) FROM contact_name_pins"), 0);
    assert_eq!(
        count(path, "SELECT COUNT(*) FROM contact_book_lookup_exclusions"),
        0
    );
    assert_eq!(
        count(path, "SELECT COUNT(*) FROM contact_photo_references"),
        0,
        "contact and edit-request holders are released"
    );
    assert_eq!(
        count(
            path,
            "SELECT COUNT(*) FROM deleted_contact_books WHERE book_id='address-book' AND generation=1"
        ),
        1
    );
}

#[test]
fn deletion_naming_this_device_as_owner_is_quarantined() {
    let dir = TempDir::new().unwrap();
    let (vault, config, observer, _owner) = observed_book(&dir);

    let report = sync(
        &[sealed(
            &vault,
            DeviceId::new(),
            1,
            fence(config.device_id, "1"),
        )],
        &observer,
    );

    assert_eq!(report.quarantined, 1);
    assert_eq!(book_ids(&observer), vec![BOOK.to_owned()]);
    assert_eq!(
        count(&config.database_path, "SELECT COUNT(*) FROM contacts"),
        2
    );
}

#[test]
fn deletion_of_a_succeeded_owner_is_a_noop() {
    let dir = TempDir::new().unwrap();
    let (vault, config, observer, owner) = observed_book(&dir);
    let successor = DeviceId::new();
    sqlcipher(&config.database_path)
        .execute(
            "INSERT INTO device_successions(predecessor,successor,platform,hint_tag,key_epoch) VALUES(?,?,'android','hint',1)",
            params![owner.to_string(), successor.to_string()],
        )
        .unwrap();

    let report = sync(
        &[sealed(&vault, DeviceId::new(), 1, fence(owner, "1"))],
        &observer,
    );

    assert_eq!(report.quarantined, 0);
    assert_eq!(book_ids(&observer), vec![BOOK.to_owned()]);
    assert_eq!(
        count(
            &config.database_path,
            "SELECT COUNT(*) FROM deleted_contact_books"
        ),
        0
    );
}

#[test]
fn late_records_at_or_below_the_fence_are_noops_and_a_higher_generation_continues() {
    let dir = TempDir::new().unwrap();
    let (vault, config, observer, owner) = observed_book(&dir);
    sync(
        &[sealed(&vault, DeviceId::new(), 1, fence(owner, "1"))],
        &observer,
    );

    let late = [
        sealed(
            &vault,
            owner,
            4,
            PrivatePayload::ContactBookState {
                book: book(owner, "1"),
            },
        ),
        sealed(
            &vault,
            owner,
            5,
            PrivatePayload::ContactUpserted {
                book: book(owner, "1"),
                contact: contact("ada", "Ada"),
            },
        ),
        sealed(
            &vault,
            owner,
            6,
            PrivatePayload::ContactRemoved {
                book_id: BOOK.into(),
                contact_id: "bea".into(),
                tombstone: json!({
                    "contact_id": "bea",
                    "revision": "9",
                    "deleted_at": 5,
                    "restored_contact": contact("bea", "Bea"),
                    "book": book(owner, "1")
                }),
            },
        ),
    ];
    let report = sync(&late, &observer);
    assert_eq!(report.quarantined, 0, "fenced records are no-ops");
    assert!(book_ids(&observer).is_empty());
    assert_eq!(
        count(&config.database_path, "SELECT COUNT(*) FROM contacts"),
        0
    );

    let continued = sync(
        &[sealed(
            &vault,
            owner,
            7,
            PrivatePayload::ContactBookState {
                book: book(owner, "2"),
            },
        )],
        &observer,
    );
    assert_eq!(continued.quarantined, 0);
    assert_eq!(book_ids(&observer), vec![BOOK.to_owned()]);
}

#[test]
fn member_record_removes_only_that_contact() {
    let dir = TempDir::new().unwrap();
    let (vault, config, observer, owner) = observed_book(&dir);

    let report = sync(
        &[sealed(
            &vault,
            DeviceId::new(),
            1,
            PrivatePayload::ContactBookDeletedMember {
                book_id: BOOK.into(),
                contact_id: "ada".into(),
                owner_device_id: owner.to_string(),
                generation: "1".into(),
            },
        )],
        &observer,
    );

    assert_eq!(report.quarantined, 0);
    assert_eq!(book_ids(&observer), vec![BOOK.to_owned()]);
    let path = &config.database_path;
    assert_eq!(
        count(path, "SELECT COUNT(*) FROM contacts WHERE id='ada'"),
        0
    );
    assert_eq!(
        count(path, "SELECT COUNT(*) FROM contacts WHERE id='bea'"),
        1
    );
    assert_eq!(count(path, "SELECT COUNT(*) FROM deleted_contact_books"), 0);
}

#[test]
fn consumed_unknown_event_kind_is_tagged_on_its_journal_row() {
    let dir = TempDir::new().unwrap();
    let (vault, config, observer, _owner) = observed_book(&dir);

    let report = sync(
        &[seal_plain(
            &vault,
            DeviceId::new(),
            1,
            br#"{"kind":"future_unknown_event"}"#,
        )],
        &observer,
    );

    assert_eq!(report.quarantined, 0);
    assert_eq!(
        count(
            &config.database_path,
            "SELECT COUNT(*) FROM journal WHERE status='applied' AND reason='unknown_kind'"
        ),
        1
    );
    assert_eq!(
        count(
            &config.database_path,
            "SELECT COUNT(*) FROM journal WHERE status='applied' AND reason IS NULL"
        ),
        3,
        "ordinary applied records stay untagged"
    );
}

/// The owner captures `contacts` and publishes them; a second device syncs the book from the log,
/// so the observer's records carry the owner's compaction frontier.
struct Captured {
    vault: Vault,
    owner_config: ClientConfig,
    owner: Client,
    observer_config: ClientConfig,
    observer: Client,
    server: Server,
}

fn captured(dir: &TempDir, contacts: Vec<Value>) -> Captured {
    let vault = Vault::new();
    let owner_config = config(dir, "owner", &vault);
    let owner = unlocked(&owner_config, &vault);
    let observer_config = config(dir, "observer", &vault);
    let observer = unlocked(&observer_config, &vault);
    crate::snapshot_projection::capture(
        &owner,
        &crate::snapshot_projection::book(&owner_config, BOOK),
        contacts,
    );
    let mut server = Server::default();
    server.upload(&owner);
    server.sync(&observer);
    // `Server::sync` applies one bounded batch; drain the rest so every contact is visible.
    crate::snapshot_projection::settle(&observer);
    Captured {
        vault,
        owner_config,
        owner,
        observer_config,
        observer,
        server,
    }
}

fn two_contacts() -> Vec<Value> {
    vec![
        crate::snapshot_projection::contact(BOOK, "c1", "Ada"),
        crate::snapshot_projection::contact(BOOK, "c2", "Bea"),
    ]
}

fn delete_input(revoked: &[String]) -> String {
    json!({"book_id": BOOK, "revoked_device_ids": revoked}).to_string()
}

/// The decrypted payload of a sealed Event envelope.
fn opened(vault: &Vault, envelope: &Envelope) -> PrivatePayload {
    let root = derive_root_key(PASSPHRASE, &vault.profile).unwrap();
    let key = derive_purpose_key(&root, &vault.profile, KeyPurpose::Event).unwrap();
    let (nonce, ciphertext) = envelope.ciphertext.split_at(24);
    let encrypted = EncryptedEnvelope {
        nonce: nonce.try_into().unwrap(),
        ciphertext: ciphertext.to_vec(),
    };
    let plain = decrypt(&key, &envelope.aad_bytes().unwrap(), &encrypted).unwrap();
    serde_json::from_slice(&plain).unwrap()
}

/// Pending envelopes with their payloads. The key is derived once: deriving it per envelope is
/// a password-hash cost that dominates a thousand-record outbox.
fn outbox(client: &Client, vault: &Vault) -> Vec<(Envelope, PrivatePayload)> {
    let root = derive_root_key(PASSPHRASE, &vault.profile).unwrap();
    let key = derive_purpose_key(&root, &vault.profile, KeyPurpose::Event).unwrap();
    client
        .pending_outbox()
        .unwrap()
        .into_iter()
        .map(|envelope| {
            let (nonce, ciphertext) = envelope.ciphertext.split_at(24);
            let encrypted = EncryptedEnvelope {
                nonce: nonce.try_into().unwrap(),
                ciphertext: ciphertext.to_vec(),
            };
            let plain = decrypt(&key, &envelope.aad_bytes().unwrap(), &encrypted).unwrap();
            let payload = serde_json::from_slice(&plain).unwrap();
            (envelope, payload)
        })
        .collect()
}

#[test]
fn delete_fences_the_book_and_enqueues_one_terminal_member_per_contact() {
    let dir = TempDir::new().unwrap();
    let fixture = captured(&dir, two_contacts());
    let owner = fixture.owner_config.device_id.to_string();
    let owner_records: Vec<(u64, PrivatePayload)> = fixture
        .server
        .log
        .iter()
        .map(|envelope| {
            (
                envelope.producer_sequence.0,
                opened(&fixture.vault, envelope),
            )
        })
        .collect();

    let out: Value = serde_json::from_str(
        &fixture
            .observer
            .delete_contact_book(&delete_input(std::slice::from_ref(&owner)))
            .unwrap(),
    )
    .unwrap();

    assert_eq!(
        out,
        json!({"status": "deleted", "book_id": BOOK, "contacts": 2})
    );
    assert!(book_ids(&fixture.observer).is_empty());
    let path = &fixture.observer_config.database_path;
    assert_eq!(count(path, "SELECT COUNT(*) FROM contacts"), 0);
    assert_eq!(
        count(
            path,
            "SELECT COUNT(*) FROM deleted_contact_books WHERE book_id='address-book' AND generation=1"
        ),
        1
    );

    let records = outbox(&fixture.observer, &fixture.vault);
    let fences: Vec<&Envelope> = records
        .iter()
        .filter(|(_, p)| matches!(p, PrivatePayload::ContactBookDeleted { .. }))
        .map(|(envelope, _)| envelope)
        .collect();
    let members: Vec<(&Envelope, &str)> = records
        .iter()
        .filter_map(|(envelope, payload)| match payload {
            PrivatePayload::ContactBookDeletedMember { contact_id, .. } => {
                Some((envelope, contact_id.as_str()))
            }
            _ => None,
        })
        .collect();
    assert_eq!(fences.len(), 1);
    assert_eq!(members.len(), 2);
    assert_eq!(records.len(), 3, "nothing but the fence and members");

    // The fence is the book's retained frontier and has its own identity, so it supersedes no
    // book state; members are terminal and supersede the owner's records of the same contact.
    let fence_compaction = fences[0].compaction.as_ref().unwrap();
    assert!(!fence_compaction.terminal);
    assert!(
        fence_compaction.supersedes.is_empty(),
        "the fence is not superseded by, nor supersedes, the owner's book state"
    );
    for (envelope, contact_id) in members {
        let compaction = envelope.compaction.as_ref().unwrap();
        assert!(compaction.terminal);
        let upsert = owner_records
            .iter()
            .find(|(_, p)| {
                matches!(p, PrivatePayload::ContactUpserted { contact, .. } if contact["id"] == contact_id)
            })
            .unwrap();
        assert!(
            compaction
                .supersedes
                .iter()
                .any(|r| r.producer_device_id.to_string() == owner
                    && r.producer_sequence.0 == upsert.0),
            "member {contact_id} supersedes the owner's upsert"
        );
    }
}

#[test]
fn delete_refusals_each_carry_their_own_reason() {
    let dir = TempDir::new().unwrap();
    let fixture = captured(&dir, two_contacts());
    let owner = fixture.owner_config.device_id.to_string();
    let refusal = |client: &Client, input: String| {
        client
            .delete_contact_book(&input)
            .expect_err("the delete must be refused")
    };

    assert_eq!(
        refusal(
            &fixture.observer,
            json!({"book_id": "missing", "revoked_device_ids": [owner]}).to_string()
        ),
        Error::InvalidRequest("contact book is not on this device")
    );
    assert_eq!(
        refusal(&fixture.observer, delete_input(&[])),
        Error::InvalidRequest("contact book owner is not a removed device")
    );
    assert_eq!(
        refusal(&fixture.observer, "{not json".into()),
        Error::InvalidRequest("contact book deletion input is invalid")
    );
    assert_eq!(
        refusal(&fixture.observer, json!({"book_id": BOOK}).to_string()),
        Error::InvalidRequest("contact book deletion input is invalid")
    );
    assert_eq!(
        refusal(&fixture.owner, delete_input(std::slice::from_ref(&owner))),
        Error::InvalidRequest("contact book belongs to this device")
    );

    // A succeeded owner's book belongs to its successor: refused until that link is gone.
    let successor = DeviceId::new();
    let path = fixture.observer_config.database_path.clone();
    sqlcipher(&path)
        .execute(
            "INSERT INTO device_successions(predecessor,successor,platform,hint_tag,key_epoch) VALUES(?,?,'android','hint',1)",
            params![owner, successor.to_string()],
        )
        .unwrap();
    assert_eq!(
        refusal(
            &fixture.observer,
            delete_input(std::slice::from_ref(&owner))
        ),
        Error::InvalidRequest("contact book owner was succeeded by another device")
    );
    sqlcipher(&path)
        .execute("DELETE FROM device_successions", [])
        .unwrap();

    fixture
        .observer
        .delete_contact_book(&delete_input(std::slice::from_ref(&owner)))
        .unwrap();
    assert_eq!(
        refusal(
            &fixture.observer,
            delete_input(std::slice::from_ref(&owner))
        ),
        Error::InvalidRequest("contact book was already deleted for all devices"),
        "a re-run after success is a clean refusal"
    );
}

#[test]
fn delete_on_a_locked_vault_reports_keys_unavailable() {
    let dir = TempDir::new().unwrap();
    let fixture = captured(&dir, two_contacts());
    let owner = fixture.owner_config.device_id.to_string();
    let observer_config = fixture.observer_config.clone();
    drop(fixture.observer);

    let locked = open(&observer_config);

    assert_eq!(
        locked
            .delete_contact_book(&delete_input(&[owner]))
            .unwrap_err(),
        Error::KeysUnavailable
    );
    assert_eq!(
        count(
            &observer_config.database_path,
            "SELECT COUNT(*) FROM contacts"
        ),
        2,
        "a refused delete changes nothing"
    );
}

#[test]
fn delete_of_a_thousand_contact_book_completes_in_one_call() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let observer_config = config(&dir, "observer", &vault);
    let observer = unlocked(&observer_config, &vault);
    let owner = DeviceId::new();
    // Seeds the stored book directly: publishing 1,000 contacts through capture and sync costs
    // minutes in a debug build and is not what this test measures. The delete path is unchanged.
    let seeded = sqlcipher(&observer_config.database_path);
    seeded
        .execute(
            "INSERT INTO contact_books(id,owner_device_id,generation,state,body,forgotten) VALUES(?,?,1,'active',?,0)",
            params![BOOK, owner.to_string(), book(owner, "1").to_string()],
        )
        .unwrap();
    for index in 0..1_000 {
        let id = format!("c{index:04}");
        let body = json!({"id": id, "book_id": BOOK, "revision": "1", "display_name": "Member"});
        seeded
            .execute(
                "INSERT INTO contacts(id,book_id,revision,body,deleted_at) VALUES(?,?,1,?,NULL)",
                params![id, BOOK, body.to_string()],
            )
            .unwrap();
    }
    drop(seeded);

    let started = Instant::now();
    let out: Value = serde_json::from_str(
        &observer
            .delete_contact_book(&delete_input(&[owner.to_string()]))
            .unwrap(),
    )
    .unwrap();
    eprintln!(
        "delete of 1,000 contacts enqueued 1,001 records and applied locally in {:?}",
        started.elapsed()
    );

    assert_eq!(out["contacts"], 1_000);
    assert_eq!(outbox(&observer, &vault).len(), 1_001);
    assert_eq!(
        count(
            &observer_config.database_path,
            "SELECT COUNT(*) FROM contacts"
        ),
        0
    );
    assert!(book_ids(&observer).is_empty());
}

#[test]
fn edit_request_for_a_deleted_book_creates_no_ledger_row() {
    let dir = TempDir::new().unwrap();
    let (vault, config, observer, owner) = observed_book(&dir);
    let request = |id: &str| {
        json!({
            "schema_version": 1,
            "request_id": id,
            "target_owner": owner.to_string(),
            "book_id": BOOK,
            "kind": "update",
            "contact_id": "ada",
            "base_revision": "1",
            "expires_at": std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs() as i64
                + 86_400,
            "patches": [{"op": "replace", "path": "name.given", "value": "Augusta"}]
        })
    };
    let ledger = |id: &str| {
        count(
            &config.database_path,
            &format!("SELECT COUNT(*) FROM contact_edit_ledger WHERE request_id='{id}'"),
        )
    };
    sync(
        &[sealed(
            &vault,
            DeviceId::new(),
            1,
            PrivatePayload::ContactEditRequest {
                request: request("before"),
            },
        )],
        &observer,
    );
    assert_eq!(ledger("before"), 1, "control: a live request is recorded");

    let report = sync(
        &[sealed(&vault, DeviceId::new(), 2, fence(owner, "1"))],
        &observer,
    );
    assert_eq!(report.quarantined, 0);
    let report = sync(
        &[sealed(
            &vault,
            DeviceId::new(),
            3,
            PrivatePayload::ContactEditRequest {
                request: request("after"),
            },
        )],
        &observer,
    );

    assert_eq!(
        report.quarantined, 0,
        "a request for a deleted book is a no-op"
    );
    assert_eq!(ledger("after"), 0, "no ledger row for a deleted book");
}

#[test]
fn restored_pre_delete_backup_removes_the_book_when_the_fence_replays() {
    let dir = TempDir::new().unwrap();
    let (vault, config, observer, owner) = observed_book(&dir);
    drop(observer);
    // Rollback-journal database: copying the file after the client closed captures all of it.
    let backup = dir.path().join("pre-delete.db");
    std::fs::copy(&config.database_path, &backup).unwrap();
    let restored_config = ClientConfig {
        database_path: backup,
        ..config.clone()
    };
    let restored = unlocked(&restored_config, &vault);
    assert_eq!(book_ids(&restored), vec![BOOK.to_owned()]);

    let report = sync(
        &[sealed(&vault, DeviceId::new(), 1, fence(owner, "1"))],
        &restored,
    );

    assert_eq!(report.quarantined, 0);
    assert!(book_ids(&restored).is_empty());
    assert_eq!(
        count(
            &restored_config.database_path,
            "SELECT COUNT(*) FROM contacts"
        ),
        0
    );
}

#[test]
fn member_before_contact_blocks_old_upsert_but_not_continuation() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let cfg = config(&dir, "member-first", &vault);
    let observer = unlocked(&cfg, &vault);
    let owner = DeviceId::new();
    let deleter = DeviceId::new();
    sync(
        &[sealed(
            &vault,
            deleter,
            1,
            PrivatePayload::ContactBookDeletedMember {
                book_id: BOOK.into(),
                contact_id: "bea".into(),
                owner_device_id: owner.to_string(),
                generation: "1".into(),
            },
        )],
        &observer,
    );
    sync(
        &[sealed(
            &vault,
            owner,
            1,
            PrivatePayload::ContactUpserted {
                book: book(owner, "1"),
                contact: contact("bea", "Bea"),
            },
        )],
        &observer,
    );
    assert_eq!(
        count(
            &cfg.database_path,
            "SELECT COUNT(*) FROM contacts WHERE id='bea'"
        ),
        0
    );
    sync(
        &[sealed(
            &vault,
            owner,
            2,
            PrivatePayload::ContactUpserted {
                book: book(owner, "2"),
                contact: contact("bea", "Bea"),
            },
        )],
        &observer,
    );
    assert_eq!(
        count(
            &cfg.database_path,
            "SELECT COUNT(*) FROM contacts WHERE id='bea'"
        ),
        1
    );
}

#[test]
fn legacy_contacts_are_backfilled_from_their_book_on_reopen() {
    let dir = TempDir::new().unwrap();
    let (vault, config, observer, owner) = observed_book(&dir);
    drop(observer);
    let conn = sqlcipher(&config.database_path);
    conn.execute(
        "UPDATE contacts SET publication_owner=NULL,publication_generation=NULL",
        [],
    )
    .unwrap();
    drop(conn);
    let _observer = unlocked(&config, &vault);
    let conn = sqlcipher(&config.database_path);
    let (stored_owner, generation): (String, i64) = conn
        .query_row(
            "SELECT publication_owner,publication_generation FROM contacts WHERE id='bea'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(stored_owner, owner.to_string());
    assert_eq!(generation, 1);
}

#[test]
fn contact_publication_generation_converges_across_deletion_order() {
    for republished in [None, Some("1"), Some("2")] {
        for deletion_first in [false, true] {
            let dir = TempDir::new().unwrap();
            let (vault, config, observer, owner) = observed_book(&dir);
            let deleter = DeviceId::new();
            let deletion = [
                sealed(&vault, deleter, 1, fence(owner, "1")),
                sealed(
                    &vault,
                    deleter,
                    2,
                    PrivatePayload::ContactBookDeletedMember {
                        book_id: BOOK.into(),
                        contact_id: "bea".into(),
                        owner_device_id: owner.to_string(),
                        generation: "1".into(),
                    },
                ),
            ];
            let mut continuation = vec![sealed(
                &vault,
                owner,
                4,
                PrivatePayload::ContactBookState {
                    book: book(owner, "2"),
                },
            )];
            if let Some(revision) = republished {
                let mut item = contact("bea", "Bea");
                item["revision"] = json!(revision);
                continuation.push(sealed(
                    &vault,
                    owner,
                    5,
                    PrivatePayload::ContactUpserted {
                        book: book(owner, "2"),
                        contact: item,
                    },
                ));
            }
            if deletion_first {
                sync(&deletion, &observer);
                sync(&continuation, &observer);
            } else {
                sync(&continuation, &observer);
                sync(&deletion, &observer);
            }
            assert_eq!(
                count(
                    &config.database_path,
                    "SELECT COUNT(*) FROM contacts WHERE id='bea'"
                ),
                i64::from(republished.is_some()),
                "revision={republished:?}, deletion_first={deletion_first}"
            );
        }
    }
}

#[test]
fn delayed_member_deletes_unrepublished_contact_after_book_continues() {
    let dir = TempDir::new().unwrap();
    let (vault, config, observer, owner) = observed_book(&dir);
    sync(
        &[sealed(
            &vault,
            owner,
            4,
            PrivatePayload::ContactBookState {
                book: book(owner, "2"),
            },
        )],
        &observer,
    );
    sync(
        &[sealed(
            &vault,
            DeviceId::new(),
            1,
            PrivatePayload::ContactBookDeletedMember {
                book_id: BOOK.into(),
                contact_id: "bea".into(),
                owner_device_id: owner.to_string(),
                generation: "1".into(),
            },
        )],
        &observer,
    );
    assert_eq!(
        count(
            &config.database_path,
            "SELECT COUNT(*) FROM contacts WHERE id='bea'"
        ),
        0
    );
}

#[test]
fn delayed_member_never_deletes_a_continued_generation() {
    let dir = TempDir::new().unwrap();
    let (vault, config, observer, owner) = observed_book(&dir);
    sync(
        &[sealed(&vault, DeviceId::new(), 1, fence(owner, "1"))],
        &observer,
    );
    assert!(book_ids(&observer).is_empty());

    // The owner continues the book at generation 2 and republishes Ada.
    let continued = sync(
        &[
            sealed(
                &vault,
                owner,
                4,
                PrivatePayload::ContactBookState {
                    book: book(owner, "2"),
                },
            ),
            sealed(
                &vault,
                owner,
                5,
                PrivatePayload::ContactUpserted {
                    book: book(owner, "2"),
                    contact: contact("ada", "Ada"),
                },
            ),
        ],
        &observer,
    );
    assert_eq!(continued.quarantined, 0);
    assert_eq!(book_ids(&observer), vec![BOOK.to_owned()]);

    // A delayed generation-1 member for Ada arrives after the continuation.
    let report = sync(
        &[sealed(
            &vault,
            DeviceId::new(),
            1,
            PrivatePayload::ContactBookDeletedMember {
                book_id: BOOK.into(),
                contact_id: "ada".into(),
                owner_device_id: owner.to_string(),
                generation: "1".into(),
            },
        )],
        &observer,
    );
    assert_eq!(report.quarantined, 0);
    assert_eq!(
        count(
            &config.database_path,
            "SELECT COUNT(*) FROM contacts WHERE id='ada'"
        ),
        1,
        "the continued generation keeps its contact"
    );
}

#[test]
fn fence_of_another_owner_does_not_suppress_a_later_book() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let observer_config = config(&dir, "observer", &vault);
    let observer = unlocked(&observer_config, &vault);
    let owner = DeviceId::new();
    let other = DeviceId::new();
    let path = &observer_config.database_path;

    // A fence from `other` arrives before any book is stored: it is fenced for `other` only.
    let report = sync(
        &[sealed(&vault, DeviceId::new(), 1, fence(other, "1"))],
        &observer,
    );
    assert_eq!(report.quarantined, 0);
    assert_eq!(
        count(
            path,
            &format!(
                "SELECT COUNT(*) FROM deleted_contact_books WHERE owner_device_id='{other}' AND generation=1"
            )
        ),
        1
    );

    // The owner's book, stored later under the same id, is not suppressed.
    let report = sync(
        &[
            sealed(
                &vault,
                owner,
                1,
                PrivatePayload::ContactBookState {
                    book: book(owner, "1"),
                },
            ),
            sealed(
                &vault,
                owner,
                2,
                PrivatePayload::ContactUpserted {
                    book: book(owner, "1"),
                    contact: contact("ada", "Ada"),
                },
            ),
        ],
        &observer,
    );
    assert_eq!(report.quarantined, 0);
    assert_eq!(book_ids(&observer), vec![BOOK.to_owned()]);
    assert_eq!(count(path, "SELECT COUNT(*) FROM contacts"), 1);
}

#[test]
fn fence_of_another_owner_is_a_noop_once_the_stored_book_is_owned_elsewhere() {
    let dir = TempDir::new().unwrap();
    let (vault, config, observer, _owner) = observed_book(&dir);
    let other = DeviceId::new();
    let path = &config.database_path;

    let report = sync(
        &[sealed(&vault, DeviceId::new(), 1, fence(other, "1"))],
        &observer,
    );

    assert_eq!(report.quarantined, 0);
    assert_eq!(book_ids(&observer), vec![BOOK.to_owned()]);
    assert_eq!(count(path, "SELECT COUNT(*) FROM contacts"), 2);
    assert_eq!(
        count(path, "SELECT COUNT(*) FROM deleted_contact_books"),
        0,
        "no fence is stored for a book another owner holds"
    );

    // A member of that other device never deletes this owner's contacts either.
    let report = sync(
        &[sealed(
            &vault,
            DeviceId::new(),
            2,
            PrivatePayload::ContactBookDeletedMember {
                book_id: BOOK.into(),
                contact_id: "ada".into(),
                owner_device_id: other.to_string(),
                generation: "1".into(),
            },
        )],
        &observer,
    );
    assert_eq!(report.quarantined, 0);
    assert_eq!(count(path, "SELECT COUNT(*) FROM contacts"), 2);
}

#[test]
fn malformed_or_overflowing_generations_are_quarantined() {
    let dir = TempDir::new().unwrap();
    let (vault, config, observer, owner) = observed_book(&dir);
    let member = |generation: &str| PrivatePayload::ContactBookDeletedMember {
        book_id: BOOK.into(),
        contact_id: "ada".into(),
        owner_device_id: owner.to_string(),
        generation: generation.into(),
    };

    let report = sync(
        &[
            sealed(&vault, DeviceId::new(), 1, fence(owner, "abc")),
            sealed(
                &vault,
                DeviceId::new(),
                1,
                fence(owner, "99999999999999999999"),
            ),
            sealed(&vault, DeviceId::new(), 1, member("07")),
        ],
        &observer,
    );

    assert_eq!(report.quarantined, 3);
    assert_eq!(book_ids(&observer), vec![BOOK.to_owned()]);
    let path = &config.database_path;
    assert_eq!(count(path, "SELECT COUNT(*) FROM contacts"), 2);
    assert_eq!(count(path, "SELECT COUNT(*) FROM deleted_contact_books"), 0);
}

#[test]
fn a_continued_book_can_be_deleted_again_under_a_higher_fence() {
    let dir = TempDir::new().unwrap();
    let (vault, config, observer, owner) = observed_book(&dir);
    let revoked = [owner.to_string()];
    observer
        .delete_contact_book(&delete_input(&revoked))
        .unwrap();
    assert!(book_ids(&observer).is_empty());

    // The owner continues the deleted book at generation 2.
    let continued = sync(
        &[sealed(
            &vault,
            owner,
            4,
            PrivatePayload::ContactBookState {
                book: book(owner, "2"),
            },
        )],
        &observer,
    );
    assert_eq!(continued.quarantined, 0);
    assert_eq!(book_ids(&observer), vec![BOOK.to_owned()]);

    let out: Value = serde_json::from_str(
        &observer
            .delete_contact_book(&delete_input(&revoked))
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        out,
        json!({"status": "deleted", "book_id": BOOK, "contacts": 0})
    );
    assert!(book_ids(&observer).is_empty());
    let path = &config.database_path;
    assert_eq!(
        count(
            path,
            &format!(
                "SELECT COUNT(*) FROM deleted_contact_books WHERE owner_device_id='{owner}' AND generation=2"
            )
        ),
        1
    );
    assert_eq!(
        observer
            .delete_contact_book(&delete_input(&revoked))
            .unwrap_err(),
        Error::InvalidRequest("contact book was already deleted for all devices"),
        "the continued deletion is final too"
    );
}

#[test]
fn a_failed_seal_rolls_back_the_whole_delete() {
    let dir = TempDir::new().unwrap();
    let (vault, config, observer, owner) = observed_book(&dir);
    drop(observer);
    // The delete queues a fence and two members; the third queued row fails to seal.
    sqlcipher(&config.database_path)
        .execute_batch(
            "CREATE TRIGGER fail_seal BEFORE UPDATE OF state ON outbox
             WHEN NEW.state='queued' AND (SELECT COUNT(*) FROM outbox WHERE state='queued') >= 2
             BEGIN SELECT RAISE(ABORT, 'seal failure'); END;",
        )
        .unwrap();
    let observer = unlocked(&config, &vault);
    let path = &config.database_path;
    let outbox_before = count(path, "SELECT COUNT(*) FROM outbox");

    assert!(
        observer
            .delete_contact_book(&delete_input(&[owner.to_string()]))
            .is_err()
    );

    assert_eq!(book_ids(&observer), vec![BOOK.to_owned()]);
    assert_eq!(count(path, "SELECT COUNT(*) FROM contacts"), 2);
    assert_eq!(count(path, "SELECT COUNT(*) FROM deleted_contact_books"), 0);
    assert_eq!(count(path, "SELECT COUNT(*) FROM outbox"), outbox_before);
    assert!(observer.pending_outbox().unwrap().is_empty());
}
