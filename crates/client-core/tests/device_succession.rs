//! Re-pair regression coverage for contact-book device succession.

use crate::common::*;
use peppy_crypto::{KeyPurpose, derive_purpose_key, derive_root_key, encrypt};
use serde_json::{Value, json};
use tempfile::TempDir;

const HINT: &[u8] = b"reinstall-stable-android-id";

fn book(owner: &ClientConfig, id: &str) -> Value {
    json!({
        "id": id,
        "owner_device_id": owner.device_id.to_string(),
        "generation": "1",
        "state": "active",
        "region": "US"
    })
}

fn contact(book_id: &str, id: &str, name: &str) -> Value {
    let phone = if id == "ada" {
        "+12025550100"
    } else {
        "+12025550101"
    };
    json!({
        "id": id,
        "book_id": book_id,
        "display_name": name,
        "name": {"given": name},
        "phones": [{"id": "phone", "value": phone}],
        "emails": [],
        "addresses": []
    })
}

fn capture(client: &Client, owner: &ClientConfig, id: &str) {
    client
        .capture_contact_book(
            &json!({
                "schema_version": 1,
                "book": book(owner, id),
                "contacts": [contact(id, "ada", "Ada"), contact(id, "bea", "Bea")]
            })
            .to_string(),
        )
        .unwrap();
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

fn queued(client: &Client) -> Vec<Envelope> {
    client.pending_outbox().unwrap()
}

fn tag(vault: &Vault) -> String {
    let root = derive_root_key(PASSPHRASE, &vault.profile).unwrap();
    let key = derive_purpose_key(&root, &vault.profile, KeyPurpose::Compaction).unwrap();
    peppy_crypto::compaction_hmac(&key, b"device-hint-v1", &[b"android", HINT])
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn sealed(vault: &Vault, producer: DeviceId, sequence: u64, payload: PrivatePayload) -> Envelope {
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
    let plain = serde_json::to_vec(&payload).unwrap();
    let encrypted = encrypt(&key, &envelope.aad_bytes().unwrap(), &plain).unwrap();
    envelope.ciphertext = [encrypted.nonce.as_slice(), &encrypted.ciphertext].concat();
    envelope
}

fn hint(vault: &Vault, device: DeviceId, sequence: u64) -> Envelope {
    sealed(
        vault,
        device,
        sequence,
        PrivatePayload::DeviceHintRegistered {
            device_id: device,
            platform: "android".into(),
            hint_tag: tag(vault),
            key_epoch: 1,
        },
    )
}

#[test]
fn re_pair_inherits_book_and_does_not_duplicate_contacts_for_observer() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let a_cfg = config(&dir, "android-before-reinstall", &vault);
    let b_cfg = config(&dir, "android-after-reinstall", &vault);
    let c_cfg = config(&dir, "observer", &vault);
    let a = unlocked(&a_cfg, &vault);
    let b = unlocked(&b_cfg, &vault);
    let c = unlocked(&c_cfg, &vault);

    capture(&a, &a_cfg, "address-book");
    a.register_device_hint("android", HINT).unwrap();
    let a_events = queued(&a);
    sync(&a_events, &b);
    b.observe_server_high_water(Cursor(a_events.len() as u64))
        .unwrap();

    b.register_device_hint("android", HINT).unwrap();
    let b_events = queued(&b);
    assert!(
        b_events
            .iter()
            .any(|envelope| envelope.producer_device_id == b_cfg.device_id)
    );
    assert!(b_events.iter().all(|envelope| {
        !serde_json::to_string(envelope)
            .unwrap()
            .contains("reinstall-stable")
    }));
    let books: Value = serde_json::from_str(&b.list_contact_books_json().unwrap()).unwrap();
    assert_eq!(books["books"].as_array().unwrap().len(), 1);
    assert_eq!(
        books["books"][0]["owner_device_id"],
        b_cfg.device_id.to_string()
    );

    sync(&a_events, &c);
    sync(&b_events, &c);
    let search: Value = serde_json::from_str(
        &c.search_contact_recipients_json(
            &json!({"schema_version": 1, "query": "Ada"}).to_string(),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(search["results"].as_array().unwrap().len(), 1);
    let request = json!({"schema_version": 1, "addresses": ["+12025550100"]}).to_string();
    let _ = c.resolve_contact_addresses_json(&request).unwrap();
    let resolved: Value =
        serde_json::from_str(&c.resolve_contact_addresses_json(&request).unwrap()).unwrap();
    assert_eq!(resolved["matches"].as_array().unwrap().len(), 1);
}

#[test]
fn succession_rejects_wrong_producer_tag_epoch_second_successor_self_and_cycle() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let observer_cfg = config(&dir, "observer", &vault);
    let observer = unlocked(&observer_cfg, &vault);
    let predecessor = DeviceId::new();
    let successor = DeviceId::new();
    let other = DeviceId::new();
    let attacker = DeviceId::new();
    let hint_events = [
        hint(&vault, predecessor, 1),
        hint(&vault, successor, 1),
        hint(&vault, other, 1),
    ];
    sync(&hint_events, &observer);

    let bad_events = vec![
        sealed(
            &vault,
            attacker,
            1,
            PrivatePayload::DeviceSucceeded {
                predecessor,
                successor,
                platform: "android".into(),
                hint_tag: tag(&vault),
                key_epoch: 1,
            },
        ),
        sealed(
            &vault,
            successor,
            2,
            PrivatePayload::DeviceSucceeded {
                predecessor,
                successor,
                platform: "android".into(),
                hint_tag: "wrong".into(),
                key_epoch: 1,
            },
        ),
        sealed(
            &vault,
            successor,
            3,
            PrivatePayload::DeviceSucceeded {
                predecessor,
                successor,
                platform: "android".into(),
                hint_tag: tag(&vault),
                key_epoch: 2,
            },
        ),
        sealed(
            &vault,
            predecessor,
            2,
            PrivatePayload::DeviceSucceeded {
                predecessor,
                successor: predecessor,
                platform: "android".into(),
                hint_tag: tag(&vault),
                key_epoch: 1,
            },
        ),
    ];
    sync(&bad_events, &observer);
    assert!(
        serde_json::from_str::<Value>(&observer.list_contact_books_json().unwrap()).unwrap()
            ["books"]
            .as_array()
            .unwrap()
            .is_empty()
    );

    sync(
        &[sealed(
            &vault,
            successor,
            4,
            PrivatePayload::DeviceSucceeded {
                predecessor,
                successor,
                platform: "android".into(),
                hint_tag: tag(&vault),
                key_epoch: 1,
            },
        )],
        &observer,
    );
    sync(
        &[sealed(
            &vault,
            other,
            2,
            PrivatePayload::DeviceSucceeded {
                predecessor,
                successor: other,
                platform: "android".into(),
                hint_tag: tag(&vault),
                key_epoch: 1,
            },
        )],
        &observer,
    );
    sync(
        &[sealed(
            &vault,
            predecessor,
            3,
            PrivatePayload::DeviceSucceeded {
                predecessor: successor,
                successor: predecessor,
                platform: "android".into(),
                hint_tag: tag(&vault),
                key_epoch: 1,
            },
        )],
        &observer,
    );
    assert_eq!(
        observer.quarantine_count().unwrap(),
        0,
        "invalid controls are consumed, never wedge replay"
    );
}

#[test]
fn pending_succession_is_accepted_when_predecessor_hint_arrives_late() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "observer", &vault), &vault);
    let predecessor = DeviceId::new();
    let successor = DeviceId::new();
    sync(&[hint(&vault, successor, 1)], &client);
    sync(
        &[sealed(
            &vault,
            successor,
            2,
            PrivatePayload::DeviceSucceeded {
                predecessor,
                successor,
                platform: "android".into(),
                hint_tag: tag(&vault),
                key_epoch: 1,
            },
        )],
        &client,
    );
    sync(&[hint(&vault, predecessor, 1)], &client);
    assert_eq!(client.quarantine_count().unwrap(), 0);
}

#[test]
fn selection_waits_then_mints_or_inherits_after_replay() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let cfg = config(&dir, "android", &vault);
    let client = unlocked(&cfg, &vault);
    assert_eq!(
        client.contact_book_selection().unwrap(),
        ContactBookSelection::Wait
    );
    client.register_device_hint("android", HINT).unwrap();
    client.observe_server_high_water(Cursor(0)).unwrap();
    assert_eq!(
        client.contact_book_selection().unwrap(),
        ContactBookSelection::Mint
    );
}

#[test]
fn successor_matches_a_predecessor_hint_after_epoch_rotation() {
    let dir = TempDir::new().unwrap();
    let epoch1 = Vault::new();
    let epoch2 = Vault::epoch(epoch1.id, 2);
    let predecessor_cfg = config(&dir, "android-before-reinstall", &epoch1);
    let successor_cfg = config(&dir, "android-after-reinstall", &epoch1);
    let predecessor = unlocked(&predecessor_cfg, &epoch1);
    let successor = unlocked(&successor_cfg, &epoch1);
    let observer_cfg = config(&dir, "observer", &epoch1);
    let observer = unlocked(&observer_cfg, &epoch1);

    capture(&predecessor, &predecessor_cfg, "address-book");
    predecessor.register_device_hint("android", HINT).unwrap();
    let predecessor_events = queued(&predecessor);
    sync(&predecessor_events, &successor);
    successor
        .observe_server_high_water(Cursor(predecessor_events.len() as u64))
        .unwrap();

    successor
        .unlock(&epoch2.profile, &epoch2.header, PASSPHRASE)
        .unwrap();
    successor.activate_epoch(2).unwrap();
    successor.register_device_hint("android", HINT).unwrap();
    let successor_events = queued(&successor);

    let books: Value = serde_json::from_str(&successor.list_contact_books_json().unwrap()).unwrap();
    assert_eq!(books["books"].as_array().unwrap().len(), 1);
    assert_eq!(
        books["books"][0]["owner_device_id"],
        successor_cfg.device_id.to_string()
    );

    observer
        .unlock(&epoch2.profile, &epoch2.header, PASSPHRASE)
        .unwrap();
    sync(&predecessor_events, &observer);
    sync(&successor_events, &observer);
    let observer_books: Value =
        serde_json::from_str(&observer.list_contact_books_json().unwrap()).unwrap();
    assert_eq!(
        observer_books["books"][0]["owner_device_id"],
        successor_cfg.device_id.to_string(),
        "the observer accepts the hint matched under the predecessor epoch"
    );
}

#[test]
fn late_succession_retires_the_minted_book_from_contact_queries() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let predecessor_cfg = config(&dir, "android-before-reinstall", &vault);
    let successor_cfg = config(&dir, "android-after-reinstall", &vault);
    let predecessor = unlocked(&predecessor_cfg, &vault);
    let successor = unlocked(&successor_cfg, &vault);

    capture(&predecessor, &predecessor_cfg, "inherited-book");
    predecessor.register_device_hint("android", HINT).unwrap();
    let predecessor_events = queued(&predecessor);

    capture(&successor, &successor_cfg, "minted-book");
    sync(&predecessor_events, &successor);
    successor
        .observe_server_high_water(Cursor(predecessor_events.len() as u64))
        .unwrap();
    successor.register_device_hint("android", HINT).unwrap();

    assert_eq!(
        successor.contact_book_selection().unwrap(),
        ContactBookSelection::Inherited {
            book_id: "inherited-book".into(),
            retire_book_ids: vec!["minted-book".into()],
        }
    );
    successor
        .forget_contact_book(&json!({"book_id": "minted-book"}).to_string())
        .unwrap();

    let search: Value = serde_json::from_str(
        &successor
            .search_contact_recipients_json(
                &json!({"schema_version": 1, "query": "Ada"}).to_string(),
            )
            .unwrap(),
    )
    .unwrap();
    assert_eq!(search["results"].as_array().unwrap().len(), 1);
    let resolved: Value = serde_json::from_str(
        &successor
            .resolve_contact_addresses_json(
                &json!({"schema_version": 1, "addresses": ["+12025550100"]}).to_string(),
            )
            .unwrap(),
    )
    .unwrap();
    assert_eq!(resolved["matches"].as_array().unwrap().len(), 1);
}

#[test]
fn inherited_selection_retries_retirement_after_a_held_large_delete() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let predecessor_cfg = config(&dir, "android-before-reinstall", &vault);
    let successor_cfg = config(&dir, "android-after-reinstall", &vault);
    let predecessor = unlocked(&predecessor_cfg, &vault);
    let successor = unlocked(&successor_cfg, &vault);

    capture(&predecessor, &predecessor_cfg, "inherited-book");
    predecessor.register_device_hint("android", HINT).unwrap();
    let predecessor_events = queued(&predecessor);
    let contacts = (0..20)
        .map(|index| contact("minted-book", &format!("contact-{index}"), "Minted"))
        .collect::<Vec<_>>();
    successor
        .capture_contact_book(
            &json!({"schema_version": 1, "book": book(&successor_cfg, "minted-book"), "contacts": contacts})
                .to_string(),
        )
        .unwrap();
    sync(&predecessor_events, &successor);
    successor
        .observe_server_high_water(Cursor(predecessor_events.len() as u64))
        .unwrap();
    successor.register_device_hint("android", HINT).unwrap();

    successor
        .begin_contact_scan(r#"{"schema_version":1,"scan_id":"takeover","book_id":"inherited-book","generation":"1","access":"full","authoritative":true}"#)
        .unwrap();
    successor
        .finish_contact_scan(r#"{"schema_version":1,"scan_id":"takeover","complete":true}"#)
        .unwrap();
    successor
        .begin_contact_scan(r#"{"schema_version":1,"scan_id":"retire","book_id":"minted-book","generation":"1","access":"full","authoritative":true}"#)
        .unwrap();
    let held: Value = serde_json::from_str(
        &successor
            .finish_contact_scan(r#"{"schema_version":1,"scan_id":"retire","complete":true}"#)
            .unwrap(),
    )
    .unwrap();
    assert_eq!(held["held"], 20);
    assert_eq!(
        successor.contact_book_selection().unwrap(),
        ContactBookSelection::Inherited {
            book_id: "inherited-book".into(),
            retire_book_ids: vec!["minted-book".into()],
        }
    );
    successor
        .contact_approval_json(
            &json!({"schema_version": 1, "scan_id": "retire", "approve": true}).to_string(),
        )
        .unwrap();
    successor
        .forget_contact_book(&json!({"book_id": "minted-book"}).to_string())
        .unwrap();
    assert_eq!(
        successor.contact_book_selection().unwrap(),
        ContactBookSelection::Owned {
            book_id: "inherited-book".into(),
        }
    );
}

#[test]
fn succession_requeues_successor_contact_events_quarantined_before_takeover() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let predecessor_cfg = config(&dir, "android-before-reinstall", &vault);
    let successor_cfg = config(&dir, "android-after-reinstall", &vault);
    let observer = unlocked(&config(&dir, "observer", &vault), &vault);
    let predecessor = unlocked(&predecessor_cfg, &vault);
    let successor = unlocked(&successor_cfg, &vault);

    capture(&predecessor, &predecessor_cfg, "address-book");
    predecessor.register_device_hint("android", HINT).unwrap();
    let predecessor_events = queued(&predecessor);
    capture(&successor, &successor_cfg, "address-book");
    let successor_contact_events = queued(&successor);

    sync(&predecessor_events, &observer);
    let rejected = sync(&successor_contact_events, &observer);
    assert_eq!(rejected.quarantined, 3);
    assert_eq!(observer.quarantine_count().unwrap(), 3);

    let successor_controls = [
        hint(&vault, successor_cfg.device_id, 4),
        sealed(
            &vault,
            successor_cfg.device_id,
            5,
            PrivatePayload::DeviceSucceeded {
                predecessor: predecessor_cfg.device_id,
                successor: successor_cfg.device_id,
                platform: "android".into(),
                hint_tag: tag(&vault),
                key_epoch: 1,
            },
        ),
    ];
    sync(&successor_controls, &observer);
    let books: Value = serde_json::from_str(&observer.list_contact_books_json().unwrap()).unwrap();
    assert_eq!(
        books["books"][0]["owner_device_id"],
        successor_cfg.device_id.to_string(),
        "the succession is accepted before requeueing"
    );
    observer.apply_pending(100).unwrap();

    assert_eq!(observer.quarantine_count().unwrap(), 0);
    assert_eq!(books["books"].as_array().unwrap().len(), 1);
    assert_eq!(
        books["books"][0]["owner_device_id"],
        successor_cfg.device_id.to_string()
    );
}

#[test]
fn predecessor_contact_events_after_succession_are_rejected() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let predecessor_cfg = config(&dir, "android-before-reinstall", &vault);
    let successor_cfg = config(&dir, "android-after-reinstall", &vault);
    let observer = unlocked(&config(&dir, "observer", &vault), &vault);
    let predecessor = unlocked(&predecessor_cfg, &vault);
    let successor = unlocked(&successor_cfg, &vault);

    capture(&predecessor, &predecessor_cfg, "address-book");
    predecessor.register_device_hint("android", HINT).unwrap();
    let predecessor_events = queued(&predecessor);
    sync(&predecessor_events, &successor);
    successor
        .observe_server_high_water(Cursor(predecessor_events.len() as u64))
        .unwrap();
    successor.register_device_hint("android", HINT).unwrap();
    let successor_events = queued(&successor);
    sync(&predecessor_events, &observer);
    sync(&successor_events, &observer);

    let late_events = [
        sealed(
            &vault,
            predecessor_cfg.device_id,
            100,
            PrivatePayload::ContactBookState {
                book: book(&predecessor_cfg, "address-book"),
            },
        ),
        sealed(
            &vault,
            predecessor_cfg.device_id,
            101,
            PrivatePayload::ContactUpserted {
                book: book(&predecessor_cfg, "address-book"),
                contact: contact("address-book", "ada", "Ada"),
            },
        ),
    ];
    let rejected = sync(&late_events, &observer);
    assert_eq!(rejected.quarantined, 2);
    assert_eq!(observer.quarantine_count().unwrap(), 2);
}

#[test]
fn selection_mints_after_replay_when_no_device_hint_is_available() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "android", &vault), &vault);

    client.register_no_device_hint().unwrap();
    client.observe_server_high_water(Cursor(0)).unwrap();

    assert_eq!(
        client.contact_book_selection().unwrap(),
        ContactBookSelection::Mint
    );
    assert!(client.pending_outbox().unwrap().is_empty());
}
