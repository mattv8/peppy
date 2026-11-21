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
    json!({"book_id": BOOK, "revoked_device_ids": revoked, "sync_current": true}).to_string()
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
            json!({"book_id": "missing", "revoked_device_ids": [owner], "sync_current": true})
                .to_string()
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

const HINT: &[u8] = b"reinstall-stable-android-id";

fn capture_owned(client: &Client, owner: DeviceId) {
    client
        .capture_contact_book(
            &json!({
                "schema_version": 1,
                "book": book(owner, "1"),
                "contacts": [contact("ada", "Ada"), contact("bea", "Bea")]
            })
            .to_string(),
        )
        .unwrap();
}

/// A tombstone for Ada at `revision` that embeds `book` as the owner published it.
fn ada_tombstone(book: Value, revision: &str) -> PrivatePayload {
    let mut restored = contact("ada", "Ada");
    restored["revision"] = json!(revision);
    PrivatePayload::ContactRemoved {
        book_id: BOOK.into(),
        contact_id: "ada".into(),
        tombstone: json!({
            "contact_id": "ada",
            "revision": revision,
            "deleted_at": 5,
            "restored_contact": restored,
            "book": book,
        }),
    }
}

fn ada_member(owner: DeviceId, generation: &str) -> PrivatePayload {
    PrivatePayload::ContactBookDeletedMember {
        book_id: BOOK.into(),
        contact_id: "ada".into(),
        owner_device_id: owner.to_string(),
        generation: generation.into(),
    }
}

#[test]
fn deletion_after_succession_removes_the_inherited_book_and_its_contacts() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let a_cfg = config(&dir, "android-before-reinstall", &vault);
    let b_cfg = config(&dir, "android-after-reinstall", &vault);
    let c_cfg = config(&dir, "observer", &vault);
    let a = unlocked(&a_cfg, &vault);
    let b = unlocked(&b_cfg, &vault);
    let c = unlocked(&c_cfg, &vault);
    capture_owned(&a, a_cfg.device_id);
    a.register_device_hint("android", HINT).unwrap();
    let a_events = a.pending_outbox().unwrap();
    sync(&a_events, &b);
    b.observe_server_high_water(Cursor(a_events.len() as u64))
        .unwrap();
    b.register_device_hint("android", HINT).unwrap();
    let b_events = b.pending_outbox().unwrap();
    sync(&a_events, &c);
    sync(&b_events, &c);

    // B inherits A's book unchanged, so the book and its contacts are B's to delete.
    let path = &c_cfg.database_path;
    assert_eq!(
        count(
            path,
            &format!(
                "SELECT COUNT(*) FROM contacts WHERE publication_owner='{}'",
                b_cfg.device_id
            )
        ),
        2,
        "inherited contacts publish under the successor"
    );

    let deleted = c
        .delete_contact_book(&delete_input(&[b_cfg.device_id.to_string()]))
        .unwrap();
    assert!(deleted.contains("\"deleted\""), "{deleted}");
    assert!(book_ids(&c).is_empty());
    assert_eq!(count(path, "SELECT COUNT(*) FROM contacts"), 0);
    assert_eq!(count(path, "SELECT COUNT(*) FROM contact_books"), 0);
}

#[test]
fn delayed_predecessor_records_cannot_resurrect_a_book_deleted_under_its_successor() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let a_cfg = config(&dir, "android-before-reinstall", &vault);
    let b_cfg = config(&dir, "android-after-reinstall", &vault);
    let c_cfg = config(&dir, "observer", &vault);
    let a = unlocked(&a_cfg, &vault);
    let b = unlocked(&b_cfg, &vault);
    let c = unlocked(&c_cfg, &vault);
    capture_owned(&a, a_cfg.device_id);
    a.register_device_hint("android", HINT).unwrap();
    let a_events = a.pending_outbox().unwrap();
    sync(&a_events, &b);
    b.observe_server_high_water(Cursor(a_events.len() as u64))
        .unwrap();
    b.register_device_hint("android", HINT).unwrap();
    let b_events = b.pending_outbox().unwrap();
    sync(&a_events, &c);
    sync(&b_events, &c);
    let deleted = c
        .delete_contact_book(&delete_input(&[b_cfg.device_id.to_string()]))
        .unwrap();
    assert!(deleted.contains("\"deleted\""), "{deleted}");

    // A's book and contact arrive late, named by A but judged as B's, which the fence covers.
    let path = &c_cfg.database_path;
    let a_id = a_cfg.device_id;
    sync(
        &[
            sealed(
                &vault,
                a_id,
                900,
                PrivatePayload::ContactBookState {
                    book: book(a_id, "1"),
                },
            ),
            sealed(
                &vault,
                a_id,
                901,
                PrivatePayload::ContactUpserted {
                    book: book(a_id, "1"),
                    contact: contact("ada", "Ada"),
                },
            ),
            sealed(&vault, a_id, 902, ada_tombstone(book(a_id, "1"), "2")),
        ],
        &c,
    );
    assert!(book_ids(&c).is_empty());
    assert_eq!(count(path, "SELECT COUNT(*) FROM contacts"), 0);
    assert_eq!(count(path, "SELECT COUNT(*) FROM contact_books"), 0);
}

#[test]
fn succession_first_arrival_of_a_predecessor_book_is_inherited_without_deletion() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let a_cfg = config(&dir, "android-before-reinstall", &vault);
    let b_cfg = config(&dir, "android-after-reinstall", &vault);
    let a = unlocked(&a_cfg, &vault);
    let b = unlocked(&b_cfg, &vault);
    capture_owned(&a, a_cfg.device_id);
    a.register_device_hint("android", HINT).unwrap();
    let (hints, books): (Vec<Envelope>, Vec<Envelope>) = a
        .pending_outbox()
        .unwrap()
        .into_iter()
        .partition(|envelope| {
            matches!(
                opened(&vault, envelope),
                PrivatePayload::DeviceHintRegistered { .. }
            )
        });
    // The successor learns the succession before the predecessor's book reaches it.
    sync(&hints, &b);
    b.observe_server_high_water(Cursor(hints.len() as u64))
        .unwrap();
    b.register_device_hint("android", HINT).unwrap();
    assert!(book_ids(&b).is_empty());

    let state: Vec<Envelope> = books
        .into_iter()
        .filter(|envelope| {
            matches!(
                opened(&vault, envelope),
                PrivatePayload::ContactBookState { .. }
            )
        })
        .collect();
    assert_eq!(sync(&state, &b).quarantined, 0);
    assert_eq!(book_ids(&b), vec![BOOK.to_owned()]);
    let listed: Value = serde_json::from_str(&b.list_contact_books_json().unwrap()).unwrap();
    assert_eq!(
        listed["books"][0]["owner_device_id"],
        json!(b_cfg.device_id.to_string())
    );
    assert!(matches!(
        b.contact_book_selection().unwrap(),
        ContactBookSelection::Inherited { ref book_id, .. } if book_id == BOOK
    ));
}

/// Whether a predecessor's deletion reaches the observer before or after the succession.
#[derive(Clone, Copy, Debug)]
enum Arrival {
    BeforeSuccession,
    AfterSuccession,
}

/// The predecessor's generation-1 deletion: the book fence, or Ada's member record.
#[derive(Clone, Copy, Debug)]
enum Deletion {
    Fence,
    Member,
}

/// Stored book and contact ids at `path`.
fn stored_ids(path: &Path, sql: &str) -> Vec<String> {
    let connection = sqlcipher(path);
    let mut statement = connection.prepare(sql).unwrap();
    statement
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

/// Stored book ids and contact ids of one device.
type StoredIds = (Vec<String>, Vec<String>);

/// Ingests the predecessor's book, the succession, the deletion and then the successor's
/// republication, in order. Returns the observer's stored ids once the records are ingested live,
/// and the same for a fresh device that ingests the whole list as one batch.
fn predecessor_deletion_then_republication(
    deletion: Deletion,
    arrival: Arrival,
) -> (StoredIds, StoredIds) {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let a_cfg = config(&dir, "android-before-reinstall", &vault);
    let b_cfg = config(&dir, "android-after-reinstall", &vault);
    let c_cfg = config(&dir, "observer", &vault);
    let d_cfg = config(&dir, "fresh", &vault);
    let a = unlocked(&a_cfg, &vault);
    let b = unlocked(&b_cfg, &vault);
    let c = unlocked(&c_cfg, &vault);
    let (a_id, b_id) = (a_cfg.device_id, b_cfg.device_id);
    capture_owned(&a, a_id);
    a.register_device_hint("android", HINT).unwrap();
    let a_events = a.pending_outbox().unwrap();
    sync(&a_events, &b);
    b.observe_server_high_water(Cursor(a_events.len() as u64))
        .unwrap();
    b.register_device_hint("android", HINT).unwrap();
    let b_events = b.pending_outbox().unwrap();

    let record = match deletion {
        Deletion::Fence => fence(a_id, "1"),
        Deletion::Member => ada_member(a_id, "1"),
    };
    let deletion_event = sealed(&vault, a_id, 900, record);
    let republication = vec![
        sealed(
            &vault,
            b_id,
            1001,
            PrivatePayload::ContactBookState {
                book: book(b_id, "1"),
            },
        ),
        sealed(
            &vault,
            b_id,
            1002,
            PrivatePayload::ContactUpserted {
                book: book(b_id, "1"),
                contact: contact("ada", "Ada"),
            },
        ),
        sealed(
            &vault,
            b_id,
            1003,
            PrivatePayload::ContactUpserted {
                book: book(b_id, "1"),
                contact: contact("bea", "Bea"),
            },
        ),
    ];
    let (before, after) = match arrival {
        Arrival::BeforeSuccession => (vec![deletion_event.clone()], Vec::new()),
        Arrival::AfterSuccession => (Vec::new(), vec![deletion_event]),
    };
    let chunks = [a_events, before, b_events, after, republication];

    for chunk in &chunks {
        sync(chunk, &c);
    }
    let live = (
        stored_ids(
            &c_cfg.database_path,
            "SELECT id FROM contact_books ORDER BY id",
        ),
        stored_ids(&c_cfg.database_path, "SELECT id FROM contacts ORDER BY id"),
    );

    let d = unlocked(&d_cfg, &vault);
    sync(&chunks.concat(), &d);
    let fresh = (
        stored_ids(
            &d_cfg.database_path,
            "SELECT id FROM contact_books ORDER BY id",
        ),
        stored_ids(&d_cfg.database_path, "SELECT id FROM contacts ORDER BY id"),
    );
    (live, fresh)
}

#[test]
fn predecessor_deletions_never_suppress_the_successors_republication_in_either_order() {
    let expected = (
        vec![BOOK.to_owned()],
        vec!["ada".to_owned(), "bea".to_owned()],
    );
    for deletion in [Deletion::Fence, Deletion::Member] {
        for arrival in [Arrival::BeforeSuccession, Arrival::AfterSuccession] {
            let (live, fresh) = predecessor_deletion_then_republication(deletion, arrival);
            assert_eq!(live, expected, "{deletion:?} {arrival:?}: live arrival");
            assert_eq!(fresh, expected, "{deletion:?} {arrival:?}: fresh import");
        }
    }
}

#[test]
fn republication_above_the_book_generation_establishes_known_provenance() {
    let dir = TempDir::new().unwrap();
    let (vault, config, observer, owner) = observed_book(&dir);
    sqlcipher(&config.database_path)
        .execute("UPDATE contacts SET publication_generation=NULL", [])
        .unwrap();
    sync(
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
                    book: book(owner, "3"),
                    contact: contact("ada", "Ada"),
                },
            ),
        ],
        &observer,
    );
    let path = &config.database_path;
    assert_eq!(
        count(
            path,
            "SELECT COUNT(*) FROM contacts WHERE id='ada' AND publication_generation=3"
        ),
        1,
        "a generation-3 republication is known provenance"
    );
    sync(
        &[sealed(&vault, DeviceId::new(), 1, fence(owner, "2"))],
        &observer,
    );
    assert_eq!(
        count(path, "SELECT COUNT(*) FROM contacts WHERE id='ada'"),
        1
    );
    sync(
        &[sealed(&vault, DeviceId::new(), 2, fence(owner, "3"))],
        &observer,
    );
    assert_eq!(
        count(path, "SELECT COUNT(*) FROM contacts WHERE id='ada'"),
        0
    );
}

#[test]
fn delayed_member_keeps_unknown_provenance_under_a_continued_book() {
    let dir = TempDir::new().unwrap();
    let (vault, config, observer, owner) = observed_book(&dir);
    sqlcipher(&config.database_path)
        .execute("UPDATE contacts SET publication_generation=NULL", [])
        .unwrap();
    sync(
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
                    book: book(owner, "1"),
                    contact: contact("ada", "Ada"),
                },
            ),
            sealed(&vault, DeviceId::new(), 1, ada_member(owner, "1")),
        ],
        &observer,
    );
    let path = &config.database_path;
    assert_eq!(
        count(
            path,
            "SELECT COUNT(*) FROM contacts WHERE id='ada' AND publication_generation IS NULL"
        ),
        1,
        "a delayed generation-1 replay keeps unknown provenance under a generation-2 book"
    );
    sync(
        &[sealed(&vault, DeviceId::new(), 2, ada_member(owner, "2"))],
        &observer,
    );
    assert_eq!(
        count(path, "SELECT COUNT(*) FROM contacts WHERE id='ada'"),
        0
    );
}

#[test]
fn live_generation_two_tombstone_survives_a_delayed_generation_one_member_and_fence() {
    let dir = TempDir::new().unwrap();
    let (vault, config, observer, owner) = observed_book(&dir);
    let deleter = DeviceId::new();
    let report = sync(
        &[sealed(
            &vault,
            owner,
            4,
            ada_tombstone(book(owner, "2"), "2"),
        )],
        &observer,
    );
    assert_eq!(report.quarantined, 0);
    let path = &config.database_path;
    let tombstones = "SELECT COUNT(*) FROM contacts WHERE id='ada' AND deleted_at IS NOT NULL";
    assert_eq!(count(path, tombstones), 1);

    sync(
        &[
            sealed(&vault, deleter, 1, ada_member(owner, "1")),
            sealed(&vault, deleter, 2, fence(owner, "1")),
        ],
        &observer,
    );
    assert_eq!(
        count(path, tombstones),
        1,
        "the generation-2 tombstone stays"
    );
    assert_eq!(book_ids(&observer), vec![BOOK.to_owned()]);
}

#[test]
fn old_tombstone_after_a_retained_member_recreates_nothing() {
    let dir = TempDir::new().unwrap();
    let (vault, config, observer, owner) = observed_book(&dir);
    sync(
        &[sealed(&vault, DeviceId::new(), 1, ada_member(owner, "1"))],
        &observer,
    );
    let path = &config.database_path;
    assert_eq!(
        count(path, "SELECT COUNT(*) FROM contacts WHERE id='ada'"),
        0
    );

    let report = sync(
        &[sealed(
            &vault,
            owner,
            4,
            ada_tombstone(book(owner, "1"), "2"),
        )],
        &observer,
    );
    assert_eq!(report.quarantined, 0);
    assert_eq!(
        count(path, "SELECT COUNT(*) FROM contacts WHERE id='ada'"),
        0,
        "a covered tombstone is an applied no-op"
    );
}

/// Pre-provenance rows keep an unknown generation: a deletion judges them by the book's current
/// generation, so a generation-1 fence removes them only while the book is still generation 1.
#[test]
fn legacy_contacts_with_unknown_generation_follow_the_book_generation() {
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
    let observer = unlocked(&config, &vault);
    let path = &config.database_path;
    assert_eq!(
        count(
            path,
            &format!(
                "SELECT COUNT(*) FROM contacts WHERE publication_generation IS NULL AND publication_owner='{owner}'"
            )
        ),
        2,
        "the owner is backfilled, the generation stays unknown"
    );

    // The book continues at generation 2 without republishing: its legacy contacts are not
    // covered by a generation-1 fence, but a generation-2 fence removes them.
    let deleter = DeviceId::new();
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
    sync(&[sealed(&vault, deleter, 1, fence(owner, "1"))], &observer);
    assert_eq!(count(path, "SELECT COUNT(*) FROM contacts"), 2);
    sync(&[sealed(&vault, deleter, 2, fence(owner, "2"))], &observer);
    assert_eq!(count(path, "SELECT COUNT(*) FROM contacts"), 0);
    assert_eq!(count(path, "SELECT COUNT(*) FROM contact_books"), 0);
}

#[test]
fn delayed_republication_preserves_unknown_generation_under_continued_book() {
    let dir = TempDir::new().unwrap();
    let (vault, config, observer, owner) = observed_book(&dir);
    sqlcipher(&config.database_path)
        .execute("UPDATE contacts SET publication_generation=NULL", [])
        .unwrap();
    sync(
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
                    book: book(owner, "1"),
                    contact: contact("ada", "Ada"),
                },
            ),
        ],
        &observer,
    );
    sync(
        &[sealed(&vault, DeviceId::new(), 1, fence(owner, "1"))],
        &observer,
    );
    assert_eq!(
        count(
            &config.database_path,
            "SELECT COUNT(*) FROM contacts WHERE id='ada'"
        ),
        1
    );
}

#[test]
fn legacy_contacts_beneath_generation_one_are_removed_by_a_generation_one_fence() {
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

    sync(
        &[sealed(&vault, DeviceId::new(), 1, fence(owner, "1"))],
        &_observer,
    );
    assert_eq!(
        count(&config.database_path, "SELECT COUNT(*) FROM contacts"),
        0
    );
    assert_eq!(
        count(&config.database_path, "SELECT COUNT(*) FROM contact_books"),
        0
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
    let (stored_owner, generation): (String, Option<i64>) = conn
        .query_row(
            "SELECT publication_owner,publication_generation FROM contacts WHERE id='bea'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(stored_owner, owner.to_string());
    assert_eq!(generation, None, "a legacy generation stays unknown");
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

const NOT_CURRENT: &str = "contact book deletion needs an up-to-date sync";

fn delete_json(owner: &str, sync_current: Option<bool>) -> String {
    let mut input = json!({"book_id": BOOK, "revoked_device_ids": [owner]});
    if let Some(current) = sync_current {
        input["sync_current"] = json!(current);
    }
    input.to_string()
}

/// Appends the records of the owner's log to the observer as a snapshot above its live cursor,
/// without draining it. The snapshot stays published (received, not drained).
fn publish_full_history(fixture: &Captured, generation: u64, corrupt: Option<usize>) {
    let above = fixture.server.log.len();
    let mut records: Vec<SnapshotRecord> = fixture
        .server
        .log
        .iter()
        .enumerate()
        .map(|(index, envelope)| SnapshotRecord {
            cursor: Cursor((above + index + 1) as u64),
            envelope: envelope.clone(),
        })
        .collect();
    if let Some(position) = corrupt {
        records[position].envelope.ciphertext[0] ^= 0x55;
    }
    let session = fixture
        .observer
        .begin_snapshot_with_compaction(
            Cursor((above + records.len()) as u64),
            records.len() as u64,
            SnapshotPurpose::Resync,
            Some(generation),
        )
        .unwrap();
    fixture
        .observer
        .append_snapshot_page(session.generation, &records)
        .unwrap();
    fixture
        .observer
        .finish_snapshot(session.generation)
        .unwrap();
}

#[test]
fn delete_needs_sync_current_and_a_missing_flag_counts_as_false() {
    let dir = TempDir::new().unwrap();
    let fixture = captured(&dir, two_contacts());
    let owner = fixture.owner_config.device_id.to_string();
    let path = fixture.observer_config.database_path.clone();

    for input in [delete_json(&owner, None), delete_json(&owner, Some(false))] {
        assert_eq!(
            fixture.observer.delete_contact_book(&input).unwrap_err(),
            Error::InvalidRequest(NOT_CURRENT)
        );
    }
    assert_eq!(
        count(&path, "SELECT COUNT(*) FROM contacts"),
        2,
        "a refused delete changes nothing"
    );

    fixture
        .observer
        .delete_contact_book(&delete_json(&owner, Some(true)))
        .unwrap();
    assert!(book_ids(&fixture.observer).is_empty());
}

#[test]
fn refusal_order_is_input_then_lock_then_sync_then_book() {
    let dir = TempDir::new().unwrap();
    let fixture = captured(&dir, two_contacts());
    let owner = fixture.owner_config.device_id.to_string();
    let config = fixture.observer_config.clone();

    // Invalid input wins over an out-of-date sync.
    assert_eq!(
        fixture
            .observer
            .delete_contact_book(&json!({"book_id": BOOK}).to_string())
            .unwrap_err(),
        Error::InvalidRequest("contact book deletion input is invalid")
    );
    // A locked vault reports keys before the sync gate speaks.
    drop(fixture.observer);
    let locked = open(&config);
    assert_eq!(
        locked
            .delete_contact_book(&delete_json(&owner, Some(false)))
            .unwrap_err(),
        Error::KeysUnavailable
    );
    drop(locked);
    // Not current beats the book checks: an owner that was not removed is still a sync refusal.
    let unlocked_again = unlocked(&config, &fixture.vault);
    assert_eq!(
        unlocked_again
            .delete_contact_book(&delete_json("someone-else", Some(false)))
            .unwrap_err(),
        Error::InvalidRequest(NOT_CURRENT)
    );
    assert_eq!(
        unlocked_again
            .delete_contact_book(&delete_json("someone-else", Some(true)))
            .unwrap_err(),
        Error::InvalidRequest("contact book owner is not a removed device")
    );
}

#[test]
fn pending_inbound_work_refuses_until_apply_pending_clears_it() {
    let dir = TempDir::new().unwrap();
    let fixture = captured(&dir, two_contacts());
    let owner = fixture.owner_config.device_id.to_string();
    let path = fixture.observer_config.database_path.clone();
    let next = fixture.observer.receive_cursor().unwrap().0 + 1;
    fixture
        .observer
        .ingest(
            &seal_plain(
                &fixture.vault,
                DeviceId::new(),
                900,
                br#"{"kind":"future_unknown_event"}"#,
            ),
            Cursor(next),
        )
        .unwrap();
    assert_eq!(
        fixture
            .observer
            .delete_contact_book(&delete_json(&owner, Some(true)))
            .unwrap_err(),
        Error::InvalidRequest(NOT_CURRENT),
        "an applicable pending row means local sync is not current"
    );

    fixture.observer.apply_pending(100).unwrap();
    fixture
        .observer
        .delete_contact_book(&delete_json(&owner, Some(true)))
        .unwrap();
    assert_eq!(count(&path, "SELECT COUNT(*) FROM contacts"), 0);
}

#[test]
fn pending_rows_of_a_locked_epoch_are_not_a_sync_refusal() {
    let dir = TempDir::new().unwrap();
    let fixture = captured(&dir, two_contacts());
    let owner = fixture.owner_config.device_id.to_string();
    let path = fixture.observer_config.database_path.clone();
    let next = fixture.observer.receive_cursor().unwrap().0 + 1;
    fixture
        .observer
        .ingest(
            &seal_plain(
                &fixture.vault,
                DeviceId::new(),
                901,
                br#"{"kind":"future_unknown_event"}"#,
            ),
            Cursor(next),
        )
        .unwrap();
    // The row waits for an epoch this device has not unlocked, so apply_pending cannot apply it.
    sqlcipher(&path)
        .execute("UPDATE journal SET key_epoch=7 WHERE status='pending'", [])
        .unwrap();

    // The sync gate does not count it, so it is not a sync refusal. The member publication then
    // hits the existing compaction rule: a contact record may not start while an unreadable
    // record of a locked epoch exists (`KeysUnavailable`). That rule is unchanged here.
    assert_eq!(
        fixture
            .observer
            .delete_contact_book(&delete_json(&owner, Some(true)))
            .unwrap_err(),
        Error::KeysUnavailable
    );
    assert_eq!(
        count(&path, "SELECT COUNT(*) FROM contacts"),
        2,
        "nothing changed"
    );
}

#[test]
fn an_undrained_published_snapshot_refuses_the_delete() {
    let dir = TempDir::new().unwrap();
    let fixture = captured(&dir, two_contacts());
    let owner = fixture.owner_config.device_id.to_string();
    publish_full_history(&fixture, 9, None);

    assert_eq!(
        fixture
            .observer
            .delete_contact_book(&delete_json(&owner, Some(true)))
            .unwrap_err(),
        Error::InvalidRequest(NOT_CURRENT)
    );

    crate::snapshot_projection::settle(&fixture.observer);
    fixture
        .observer
        .delete_contact_book(&delete_json(&owner, Some(true)))
        .unwrap();
}

#[test]
fn a_draining_or_staging_projection_refuses_the_delete() {
    let dir = TempDir::new().unwrap();
    let fixture = captured(&dir, two_contacts());
    let owner = fixture.owner_config.device_id.to_string();
    publish_full_history(&fixture, 9, None);

    let mut refused_mid_projection = false;
    for _ in 0..200 {
        let state = fixture
            .observer
            .snapshot_projection_status()
            .unwrap()
            .map(|status| status.state);
        if matches!(
            state,
            Some(SnapshotProjectionState::Draining | SnapshotProjectionState::Staging)
        ) {
            assert_eq!(
                fixture
                    .observer
                    .delete_contact_book(&delete_json(&owner, Some(true)))
                    .unwrap_err(),
                Error::InvalidRequest(NOT_CURRENT)
            );
            refused_mid_projection = true;
            break;
        }
        fixture.observer.apply_pending(1).unwrap();
    }
    assert!(
        refused_mid_projection,
        "the projection passed through draining or staging"
    );
    crate::snapshot_projection::settle(&fixture.observer);
}

#[test]
fn a_failed_projection_refuses_the_delete() {
    let dir = TempDir::new().unwrap();
    let fixture = captured(&dir, two_contacts());
    let owner = fixture.owner_config.device_id.to_string();
    let upsert = fixture
        .server
        .log
        .iter()
        .position(|envelope| {
            matches!(
                opened(&fixture.vault, envelope),
                PrivatePayload::ContactUpserted { .. }
            )
        })
        .unwrap();
    publish_full_history(&fixture, 9, Some(upsert));

    for _ in 0..200 {
        if matches!(
            fixture
                .observer
                .snapshot_projection_status()
                .unwrap()
                .map(|status| status.state),
            Some(SnapshotProjectionState::Failed)
        ) {
            break;
        }
        fixture.observer.apply_pending(1).unwrap();
    }
    assert_eq!(
        fixture
            .observer
            .snapshot_projection_status()
            .unwrap()
            .map(|status| status.state),
        Some(SnapshotProjectionState::Failed)
    );
    assert_eq!(
        fixture
            .observer
            .delete_contact_book(&delete_json(&owner, Some(true)))
            .unwrap_err(),
        Error::InvalidRequest(NOT_CURRENT)
    );
}

#[test]
fn a_latched_contact_repair_refuses_the_delete() {
    let dir = TempDir::new().unwrap();
    let fixture = captured(&dir, two_contacts());
    let owner = fixture.owner_config.device_id.to_string();
    fixture.observer.request_contact_repair().unwrap();

    assert_eq!(
        fixture
            .observer
            .delete_contact_book(&delete_json(&owner, Some(true)))
            .unwrap_err(),
        Error::InvalidRequest(NOT_CURRENT)
    );
    assert_eq!(
        count(
            &fixture.observer_config.database_path,
            "SELECT COUNT(*) FROM contacts"
        ),
        2
    );
}
