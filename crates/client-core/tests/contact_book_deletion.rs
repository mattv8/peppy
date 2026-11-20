//! Vault-wide contact book deletion: the replicated fence, per-contact records, local cleanup,
//! late-record suppression, and the tag on consumed unknown event kinds.

use crate::common::*;
use peppy_crypto::{KeyPurpose, derive_purpose_key, derive_root_key, encrypt};
use rusqlite::{Connection, params};
use serde_json::{Value, json};
use std::path::Path;
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

fn sqlcipher(path: &Path) -> Connection {
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
