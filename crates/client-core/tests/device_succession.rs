//! Re-pair regression coverage for contact-book device succession.

use crate::common::*;
use peppy_crypto::{
    EncryptedEnvelope, KeyPurpose, decrypt, derive_purpose_key, derive_root_key, encrypt,
};
use rusqlite::{Connection, params};
use serde_json::{Value, json};
use std::path::Path;
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

fn books(client: &Client) -> Value {
    serde_json::from_str(&client.list_contact_books_json().unwrap()).unwrap()
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
    tag_for(vault, HINT)
}

fn tag_for(vault: &Vault, hint: &[u8]) -> String {
    let root = derive_root_key(PASSPHRASE, &vault.profile).unwrap();
    let key = derive_purpose_key(&root, &vault.profile, KeyPurpose::Compaction).unwrap();
    peppy_crypto::compaction_hmac(&key, b"device-hint-v1", &[b"android", hint])
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
fn reverse_applied_succession_resolves_the_terminal_successor() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let a_cfg = config(&dir, "android-a", &vault);
    let c_cfg = config(&dir, "android-c", &vault);
    let observer = unlocked(&c_cfg, &vault);
    let a = unlocked(&a_cfg, &vault);
    let b = DeviceId::new();
    let c = c_cfg.device_id;

    capture(&a, &a_cfg, "address-book");
    a.register_device_hint("android", HINT).unwrap();
    let a_events = queued(&a);
    sync(&a_events, &observer);
    observer.register_device_hint("android", HINT).unwrap();
    let a_id = a_cfg.device_id;
    sync(
        &[
            hint(&vault, b, 1),
            hint(&vault, c, 1),
            sealed(
                &vault,
                c,
                2,
                PrivatePayload::DeviceSucceeded {
                    predecessor: b,
                    successor: c,
                    platform: "android".into(),
                    hint_tag: tag(&vault),
                    key_epoch: 1,
                },
            ),
            sealed(
                &vault,
                b,
                2,
                PrivatePayload::DeviceSucceeded {
                    predecessor: a_id,
                    successor: b,
                    platform: "android".into(),
                    hint_tag: tag(&vault),
                    key_epoch: 1,
                },
            ),
        ],
        &observer,
    );

    let books: Value = serde_json::from_str(&observer.list_contact_books_json().unwrap()).unwrap();
    assert_eq!(books["books"][0]["owner_device_id"], c.to_string());
    observer
        .observe_server_high_water(observer.receive_cursor().unwrap())
        .unwrap();
    assert_eq!(
        observer.contact_book_selection().unwrap(),
        ContactBookSelection::Inherited {
            book_id: "address-book".into(),
            retire_book_ids: vec![],
        }
    );
    drop(observer);
    let reopened = open(&c_cfg);
    assert_eq!(
        reopened.contact_book_selection().unwrap(),
        ContactBookSelection::Inherited {
            book_id: "address-book".into(),
            retire_book_ids: vec![],
        }
    );
}

#[test]
fn retired_fence_survives_both_succession_delivery_orders() {
    for reverse in [false, true] {
        let dir = TempDir::new().unwrap();
        let vault = Vault::new();
        let a_cfg = config(&dir, "retired-chain-a", &vault);
        let observer_cfg = config(&dir, "retired-chain-observer", &vault);
        let a = unlocked(&a_cfg, &vault);
        let observer = unlocked(&observer_cfg, &vault);
        let b = DeviceId::new();
        let c = DeviceId::new();
        capture(&a, &a_cfg, "address-book");
        a.register_device_hint("android", HINT).unwrap();
        let mut retired = book(&a_cfg, "address-book");
        retired["state"] = json!("retired");
        crate::snapshot_projection::capture(&a, &retired, vec![]);
        assert_eq!(sync(&queued(&a), &observer).quarantined, 0);
        observer
            .delete_contact_book(
                &json!({"book_id":"address-book","revoked_device_ids":[],"sync_current":true})
                    .to_string(),
            )
            .unwrap();
        assert_eq!(
            sync(&[hint(&vault, b, 1), hint(&vault, c, 1)], &observer).quarantined,
            0
        );
        let edge = |producer, predecessor, successor| {
            sealed(
                &vault,
                producer,
                2,
                PrivatePayload::DeviceSucceeded {
                    predecessor,
                    successor,
                    platform: "android".into(),
                    hint_tag: tag(&vault),
                    key_epoch: 1,
                },
            )
        };
        let ab = edge(b, a_cfg.device_id, b);
        let bc = edge(c, b, c);
        let edges = if reverse { vec![bc, ab] } else { vec![ab, bc] };
        assert_eq!(sync(&edges, &observer).quarantined, 0);
        let db = sqlcipher(&observer_cfg.database_path);
        let next: String = db
            .query_row(
                "SELECT successor FROM device_successions WHERE predecessor=?",
                [a_cfg.device_id.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(next, b.to_string(), "A→B, reverse={reverse}");
        let terminal: String = db
            .query_row(
                "SELECT successor FROM device_successions WHERE predecessor=?",
                [&next],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(terminal, c.to_string(), "B→C, reverse={reverse}");
        drop(db);
        retired["state_revision"] = json!("100");
        let mut ada = contact("address-book", "ada", "Ada");
        ada["revision"] = json!("1");
        let late = [
            sealed(
                &vault,
                a_cfg.device_id,
                1000,
                PrivatePayload::ContactBookState {
                    book: retired.clone(),
                },
            ),
            sealed(
                &vault,
                a_cfg.device_id,
                1001,
                PrivatePayload::ContactUpserted {
                    book: retired,
                    contact: ada,
                },
            ),
        ];
        assert_eq!(sync(&late, &observer).quarantined, 0);
        assert!(
            books(&observer)["books"].as_array().unwrap().is_empty(),
            "reverse={reverse}"
        );
        let count: i64 = sqlcipher(&observer_cfg.database_path)
            .query_row("SELECT COUNT(*) FROM contacts", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 0, "reverse={reverse}");
    }
}

#[test]
fn forward_succession_chain_selects_the_terminal_book_after_reopen() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let a_cfg = config(&dir, "android-a", &vault);
    let b_cfg = config(&dir, "android-b", &vault);
    let c_cfg = config(&dir, "android-c", &vault);
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
    sync(&a_events, &c);
    sync(&b_events, &c);
    c.observe_server_high_water(c.receive_cursor().unwrap())
        .unwrap();
    c.register_device_hint("android", HINT).unwrap();

    assert_eq!(
        c.contact_book_selection().unwrap(),
        ContactBookSelection::Inherited {
            book_id: "address-book".into(),
            retire_book_ids: vec![],
        }
    );
    drop(c);
    let reopened = open(&c_cfg);
    assert_eq!(
        reopened.contact_book_selection().unwrap(),
        ContactBookSelection::Inherited {
            book_id: "address-book".into(),
            retire_book_ids: vec![],
        }
    );
}

#[test]
fn reopen_reconciles_a_legacy_stranded_book_to_the_authenticated_terminal_successor() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let a_cfg = config(&dir, "android-a", &vault);
    let c_cfg = config(&dir, "android-c", &vault);
    let a = unlocked(&a_cfg, &vault);
    let c = unlocked(&c_cfg, &vault);
    let b = DeviceId::new();

    capture(&a, &a_cfg, "address-book");
    sync(&queued(&a), &c);
    c.register_device_hint("android", HINT).unwrap();
    c.observe_server_high_water(c.receive_cursor().unwrap())
        .unwrap();
    drop(c);

    let connection = sqlcipher(&c_cfg.database_path);
    let body: String = connection
        .query_row(
            "SELECT body FROM contact_books WHERE id='address-book'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let mut body: Value = serde_json::from_str(&body).unwrap();
    body["owner_device_id"] = json!(b.to_string());
    connection
        .execute(
            "INSERT INTO device_successions(predecessor,successor,platform,hint_tag,key_epoch) VALUES(?,?,?,?,?),(?,?,?,?,?)",
            params![
                a_cfg.device_id.to_string(), b.to_string(), "android", tag(&vault), 1,
                b.to_string(), c_cfg.device_id.to_string(), "android", tag(&vault), 1,
            ],
        )
        .unwrap();
    connection
        .execute(
            "UPDATE contact_books SET owner_device_id=?,body=? WHERE id='address-book'",
            params![b.to_string(), body.to_string()],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO inherited_contact_books(book_id,predecessor) VALUES('address-book',?)",
            [a_cfg.device_id.to_string()],
        )
        .unwrap();
    drop(connection);

    let reopened = open(&c_cfg);
    assert_eq!(
        reopened.contact_book_selection().unwrap(),
        ContactBookSelection::Inherited {
            book_id: "address-book".into(),
            retire_book_ids: vec![],
        }
    );
    drop(reopened);
    let connection = sqlcipher(&c_cfg.database_path);
    let repaired: String = connection
        .query_row(
            "SELECT body FROM contact_books WHERE id='address-book'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let repaired: Value = serde_json::from_str(&repaired).unwrap();
    assert_eq!(repaired["owner_device_id"], c_cfg.device_id.to_string());
}

#[test]
fn delayed_succession_waits_for_the_exact_old_hint_epoch() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let a_cfg = config(&dir, "android-a", &vault);
    let successor_cfg = config(&dir, "android-successor", &vault);
    let successor_client = unlocked(&successor_cfg, &vault);
    let successor = successor_cfg.device_id;
    let old_hint = b"old-reinstall-hint";
    let new_hint = b"new-reinstall-hint";
    let old_tag = tag_for(&vault, old_hint);

    let source = unlocked(&a_cfg, &vault);
    capture(&source, &a_cfg, "address-book");
    let contact_events = queued(&source);
    sync(&contact_events, &successor_client);
    successor_client
        .register_device_hint("android", old_hint)
        .unwrap();
    sync(
        &[
            sealed(
                &vault,
                a_cfg.device_id,
                99,
                PrivatePayload::DeviceHintRegistered {
                    device_id: a_cfg.device_id,
                    platform: "android".into(),
                    hint_tag: tag_for(&vault, new_hint),
                    key_epoch: 2,
                },
            ),
            sealed(
                &vault,
                successor,
                1,
                PrivatePayload::DeviceHintRegistered {
                    device_id: successor,
                    platform: "android".into(),
                    hint_tag: tag_for(&vault, new_hint),
                    key_epoch: 2,
                },
            ),
            sealed(
                &vault,
                successor,
                2,
                PrivatePayload::DeviceSucceeded {
                    predecessor: a_cfg.device_id,
                    successor,
                    platform: "android".into(),
                    hint_tag: old_tag.clone(),
                    key_epoch: 1,
                },
            ),
        ],
        &successor_client,
    );
    assert_eq!(
        books(&successor_client)["books"][0]["owner_device_id"],
        a_cfg.device_id.to_string()
    );

    sync(
        &[sealed(
            &vault,
            a_cfg.device_id,
            100,
            PrivatePayload::DeviceHintRegistered {
                device_id: a_cfg.device_id,
                platform: "android".into(),
                hint_tag: old_tag,
                key_epoch: 1,
            },
        )],
        &successor_client,
    );
    successor_client
        .observe_server_high_water(successor_client.receive_cursor().unwrap())
        .unwrap();
    assert_eq!(
        successor_client.contact_book_selection().unwrap(),
        ContactBookSelection::Inherited {
            book_id: "address-book".into(),
            retire_book_ids: vec![],
        }
    );
    drop(successor_client);
    let reopened = open(&successor_cfg);
    assert_eq!(
        reopened.contact_book_selection().unwrap(),
        ContactBookSelection::Inherited {
            book_id: "address-book".into(),
            retire_book_ids: vec![],
        }
    );
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

/// Decrypted payloads of `client`'s pending outbox, in order.
fn published(vault: &Vault, client: &Client) -> Vec<PrivatePayload> {
    queued(client)
        .iter()
        .map(|envelope| opened(vault, envelope))
        .collect()
}

#[test]
fn successor_capture_of_a_predecessor_named_book_publishes_under_the_successor() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let a_cfg = config(&dir, "android-before-reinstall", &vault);
    let b_cfg = config(&dir, "android-after-reinstall", &vault);
    let a = unlocked(&a_cfg, &vault);
    let b = unlocked(&b_cfg, &vault);
    a.register_device_hint("android", HINT).unwrap();
    let a_events = queued(&a);
    sync(&a_events, &b);
    b.observe_server_high_water(Cursor(a_events.len() as u64))
        .unwrap();
    b.register_device_hint("android", HINT).unwrap();
    b.pending_outbox().unwrap();

    // The successor captures the book still named by its predecessor.
    capture(&b, &a_cfg, "address-book");

    let successor = b_cfg.device_id.to_string();
    let listed = books(&b);
    assert_eq!(listed["books"][0]["owner_device_id"], json!(successor));
    let connection = sqlcipher(&b_cfg.database_path);
    let body: String = connection
        .query_row(
            "SELECT body FROM contact_books WHERE id='address-book'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let body: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(body["owner_device_id"], json!(successor));
    let owners: Vec<String> = connection
        .prepare("SELECT publication_owner FROM contacts WHERE book_id='address-book'")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(owners, vec![successor.clone(), successor.clone()]);
    let marked: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM inherited_contact_books WHERE book_id='address-book' AND predecessor=?",
            [a_cfg.device_id.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        marked, 1,
        "the captured book carries the inheritance marker"
    );

    let published_books: Vec<Value> = published(&vault, &b)
        .into_iter()
        .filter_map(|payload| match payload {
            PrivatePayload::ContactBookState { book }
            | PrivatePayload::ContactUpserted { book, .. } => Some(book),
            _ => None,
        })
        .collect();
    assert!(
        !published_books.is_empty(),
        "the capture publishes the book"
    );
    for book in &published_books {
        assert_eq!(book["owner_device_id"], json!(successor), "{book}");
    }
    assert!(matches!(
        b.contact_book_selection().unwrap(),
        ContactBookSelection::Inherited { ref book_id, .. } if book_id == "address-book"
    ));
}

#[test]
fn successor_deletion_removes_a_legacy_stranded_book_before_any_reconciliation() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let a_cfg = config(&dir, "android-a", &vault);
    let c_cfg = config(&dir, "android-c", &vault);
    let a = unlocked(&a_cfg, &vault);
    let c = unlocked(&c_cfg, &vault);
    let b = DeviceId::new();

    capture(&a, &a_cfg, "address-book");
    sync(&queued(&a), &c);
    drop(c);
    // Legacy state: the authenticated succession A→B is recorded, but the book is still stored
    // under its predecessor, as older code left it.
    sqlcipher(&c_cfg.database_path)
        .execute(
            "INSERT INTO device_successions(predecessor,successor,platform,hint_tag,key_epoch) VALUES(?,?,?,?,?)",
            params![a_cfg.device_id.to_string(), b.to_string(), "android", tag(&vault), 1],
        )
        .unwrap();

    // Reopening repairs the stored owner; the successor's deletion then removes the book.
    let reopened = unlocked(&c_cfg, &vault);
    let fence = sealed(
        &vault,
        b,
        1,
        PrivatePayload::ContactBookDeleted {
            book_id: "address-book".into(),
            owner_device_id: b.to_string(),
            generation: "1".into(),
            deleted_at_ms: 1_000,
        },
    );
    let applied = sync(&[fence], &reopened);
    assert_eq!(applied.quarantined, 0);
    assert!(books(&reopened)["books"].as_array().unwrap().is_empty());
    let connection = sqlcipher(&c_cfg.database_path);
    let contacts: i64 = connection
        .query_row("SELECT COUNT(*) FROM contacts", [], |row| row.get(0))
        .unwrap();
    assert_eq!(contacts, 0);
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

/// When the successor learned of its predecessor relative to minting its own book.
#[derive(Clone, Copy)]
enum MintOrder {
    /// Minted with no hint, then learned of the predecessor and registered the shared hint.
    LateHint,
    /// Registered the shared hint before minting; succession is accepted when the predecessor's
    /// hint arrives, before its book does.
    EarlyHint,
    /// Decided to mint before succession, but captured only after it was accepted.
    MintAfterAcceptance,
}

/// A successor B that inherited predecessor A's book X and still owns its own book Y, with a
/// desktop C in the same vault. Nothing has run X's takeover scan yet.
struct Superseded {
    dir: TempDir,
    vault: Vault,
    b_cfg: ClientConfig,
    c_cfg: ClientConfig,
    b: Client,
    c: Client,
    server: Server,
}

fn superseded_mint(order: MintOrder, minted: usize, book_fields: &Value) -> Superseded {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let a_cfg = config(&dir, "superseded-predecessor", &vault);
    let b_cfg = config(&dir, "superseded-successor", &vault);
    let c_cfg = config(&dir, "superseded-desktop", &vault);
    let a = unlocked(&a_cfg, &vault);
    let b = unlocked(&b_cfg, &vault);
    let c = unlocked(&c_cfg, &vault);
    capture(&a, &a_cfg, "X");
    a.register_device_hint("android", HINT).unwrap();
    // B has completed an empty initial replay before predecessor history is learned.
    b.observe_server_high_water(Cursor(0)).unwrap();
    if matches!(order, MintOrder::EarlyHint) {
        b.register_device_hint("android", HINT).unwrap();
    } else {
        b.register_no_device_hint().unwrap();
    }
    assert_eq!(
        b.contact_book_selection().unwrap(),
        ContactBookSelection::Mint
    );
    if !matches!(order, MintOrder::MintAfterAcceptance) {
        mint(&b, &b_cfg, minted, book_fields);
    }
    let mut server = Server::default();
    server.upload(&a);
    server.upload(&b);
    // With an already-registered hint, the first late X upsert transfers X to B. Later A-authored
    // X records then fail stored-owner validation; the takeover scan repairs them, as on the phone.
    let quarantined = server.sync(&b).quarantined;
    assert_eq!(
        quarantined,
        if matches!(order, MintOrder::EarlyHint) {
            2
        } else {
            0
        }
    );
    b.register_device_hint("android", HINT).unwrap();
    if matches!(order, MintOrder::MintAfterAcceptance) {
        assert_eq!(
            b.contact_book_selection().unwrap(),
            ContactBookSelection::Inherited {
                book_id: "X".into(),
                retire_book_ids: vec![]
            }
        );
        mint(&b, &b_cfg, minted, book_fields);
        server.upload(&b);
    }
    assert_eq!(
        b.contact_book_selection().unwrap(),
        ContactBookSelection::Inherited {
            book_id: "X".into(),
            retire_book_ids: vec!["Y".into()]
        }
    );
    Superseded {
        dir,
        vault,
        b_cfg,
        c_cfg,
        b,
        c,
        server,
    }
}

/// B mints its own book Y with `minted` contacts and accepts scan requests for it.
fn mint(b: &Client, b_cfg: &ClientConfig, minted: usize, book_fields: &Value) {
    let mut y = book(b_cfg, "Y");
    for (key, value) in book_fields.as_object().unwrap() {
        y[key] = value.clone();
    }
    let contacts: Vec<Value> = (0..minted)
        .map(|i| contact("Y", &format!("mint-{i}"), &format!("Mint {i}")))
        .collect();
    b.capture_contact_book(
        &json!({"schema_version": 1, "book": y, "contacts": contacts}).to_string(),
    )
    .unwrap();
    b.set_contact_scan_request_capability(r#"{"book_id":"Y","enabled":true}"#)
        .unwrap();
}

/// B's complete authoritative takeover scan of X.
fn take_over(b: &Client) {
    let generation = listed(b, "X")["generation"].clone();
    b.begin_contact_scan(
        &json!({"schema_version": 1, "scan_id": "takeover-X", "book_id": "X",
            "generation": generation, "access": "full", "authoritative": true})
        .to_string(),
    )
    .unwrap();
    for (id, name) in [("ada", "Ada"), ("bea", "Bea")] {
        b.observe_contact_scan(
            &json!({"schema_version": 1, "scan_id": "takeover-X", "contact": contact("X", id, name)})
                .to_string(),
        )
        .unwrap();
    }
    let finished: Value = serde_json::from_str(
        &b.finish_contact_scan(
            &json!({"schema_version": 1, "scan_id": "takeover-X", "complete": true}).to_string(),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(finished["held"], 0);
}

fn retire(client: &Client) -> Value {
    let out: Value =
        serde_json::from_str(&client.retire_superseded_contact_books().unwrap()).unwrap();
    out["retired"].clone()
}

fn listed(client: &Client, id: &str) -> Value {
    books(client)["books"]
        .as_array()
        .unwrap()
        .iter()
        .find(|book| book["id"] == id)
        .cloned()
        .unwrap()
}

#[test]
fn superseded_mint_retires_after_takeover_when_the_hint_arrives_late() {
    retires_after_takeover(MintOrder::LateHint);
}

#[test]
fn superseded_mint_retires_after_takeover_when_the_hint_came_first() {
    retires_after_takeover(MintOrder::EarlyHint);
}

#[test]
fn superseded_mint_retires_after_takeover_when_captured_after_acceptance() {
    retires_after_takeover(MintOrder::MintAfterAcceptance);
}

fn retires_after_takeover(order: MintOrder) {
    let mut t = superseded_mint(order, 11, &json!({}));
    assert_eq!(
        retire(&t.b),
        json!([]),
        "nothing retires before the takeover scan"
    );
    take_over(&t.b);
    t.server.upload(&t.b);
    assert_eq!(t.server.sync(&t.c).quarantined, 0);
    t.c.request_contact_sync(r#"{"book_id":"Y"}"#).unwrap();
    t.server.upload(&t.c);
    assert_eq!(t.server.sync(&t.b).quarantined, 0);

    assert_eq!(
        retire(&t.b),
        json!([{"book_id": "Y", "replaced_by": "X", "contacts_removed": 11}])
    );
    let held: i64 = sqlcipher(&t.b_cfg.database_path)
        .query_row(
            "SELECT COUNT(*) FROM contact_scan_holds WHERE book_id='Y' AND status='awaiting_approval'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(held, 0, "retirement asks for no approval");
    let y = listed(&t.b, "Y");
    assert_eq!(y["state"], "retired");
    assert_eq!(y["superseded_by"], "X");
    assert_eq!(y["contact_count"], 0);
    assert_eq!(
        t.b.contact_book_selection().unwrap(),
        ContactBookSelection::Owned {
            book_id: "X".into()
        }
    );
    assert_eq!(retire(&t.b), json!([]), "retirement runs once");

    t.server.upload(&t.b);
    assert_eq!(t.server.sync(&t.c).quarantined, 0);
    let y = listed(&t.c, "Y");
    assert_eq!(y["state"], "retired");
    assert_eq!(y["superseded_by"], "X");
    assert_eq!(y["contact_count"], 0);
    assert_eq!(listed(&t.c, "X")["state"], "active");
    let request: Value = serde_json::from_str(
        &t.c.contact_sync_request_status(r#"{"book_id":"Y"}"#)
            .unwrap(),
    )
    .unwrap();
    assert_eq!(request["state"], "blocked");
    assert_eq!(request["reason"], "source_retired");
}

#[test]
fn retiring_the_inherited_book_keeps_the_superseded_mint() {
    let t = superseded_mint(MintOrder::LateHint, 11, &json!({}));
    take_over(&t.b);
    // Stop and retire publishes the phone's current book retired.
    let mut retired = book(&t.b_cfg, "X");
    retired["state"] = json!("retired");
    t.b.capture_contact_book(
        &json!({"schema_version": 1, "book": retired, "contacts": []}).to_string(),
    )
    .unwrap();

    assert_eq!(retire(&t.b), json!([]));
    let y = listed(&t.b, "Y");
    assert_eq!(y["state"], "active");
    assert_eq!(y["contact_count"], 11);
    assert_eq!(
        t.b.contact_book_selection().unwrap(),
        ContactBookSelection::Owned {
            book_id: "Y".into()
        }
    );
}

#[test]
fn retirement_closes_the_superseded_books_open_requests() {
    let mut t = superseded_mint(
        MintOrder::LateHint,
        11,
        &json!({"capabilities": {"read": true, "write": true}, "policy": {"remote_edits": "auto"}}),
    );
    t.server.upload(&t.b);
    assert_eq!(t.server.sync(&t.c).quarantined, 0);
    let expires_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
        + 86_400;
    for (id, contact_id) in [
        ("r-new", "mint-0"),
        ("r-open", "mint-1"),
        ("r-done", "mint-2"),
    ] {
        t.c.request_contact_edit(
            &json!({"schema_version": 1, "request_id": id,
                "target_owner": t.b_cfg.device_id.to_string(), "book_id": "Y", "kind": "update",
                "contact_id": contact_id, "base_revision": "1", "expires_at": expires_at,
                "patches": [{"op": "replace", "path": "name.given", "value": "Renamed"}]})
            .to_string(),
        )
        .unwrap();
    }
    t.server.upload(&t.c);
    assert_eq!(t.server.sync(&t.b).quarantined, 0);
    let permit = |id: &str| -> Value {
        serde_json::from_str(
            &t.b.next_contact_apply_permit(
                &json!({"schema_version": 1, "request_id": id}).to_string(),
            )
            .unwrap(),
        )
        .unwrap()
    };
    assert_eq!(permit("r-open")["status"], "permit");
    assert_eq!(permit("r-done")["status"], "permit");
    t.b.reconcile_contact_apply(
        &json!({"schema_version": 1, "request_id": "r-done", "outcome": "failed"}).to_string(),
    )
    .unwrap();
    take_over(&t.b);

    assert_eq!(retire(&t.b)[0]["contacts_removed"], 11);
    let ledger = |id: &str| -> (String, Option<String>) {
        sqlcipher(&t.b_cfg.database_path)
            .query_row(
                "SELECT status,json_extract(observed,'$.reason') FROM contact_edit_ledger WHERE request_id=?",
                [id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap()
    };
    assert_eq!(
        ledger("r-new"),
        ("rejected".into(), Some("book_replaced".into()))
    );
    assert_eq!(
        ledger("r-open"),
        ("failed".into(), Some("book_replaced".into())),
        "an issued write that never reconciled is resolved, never retried"
    );
    let (done_status, done_reason) = ledger("r-done");
    assert_eq!(done_status, "failed");
    assert_ne!(done_reason.as_deref(), Some("book_replaced"));
    assert_eq!(permit("r-new")["status"], "rejected");
    assert_eq!(permit("r-open")["status"], "failed");

    t.server.upload(&t.b);
    assert_eq!(t.server.sync(&t.c).quarantined, 0);
    let requests: Value = serde_json::from_str(
        &t.c.list_contact_requests_json(
            &json!({"schema_version": 1, "requester": t.c_cfg.device_id.to_string()}).to_string(),
        )
        .unwrap(),
    )
    .unwrap();
    let state = |id: &str| {
        requests["requests"]
            .as_array()
            .unwrap()
            .iter()
            .find(|request| request["request_id"] == id)
            .unwrap()["state"]
            .clone()
    };
    assert_eq!(state("r-new"), "rejected");
    assert_eq!(state("r-open"), "failed");
    assert_eq!(state("r-done"), "failed");
}

#[test]
fn retiring_a_thousand_contact_superseded_book() {
    let t = superseded_mint(MintOrder::LateHint, 1_000, &json!({}));
    take_over(&t.b);
    let started = std::time::Instant::now();
    let retired = retire(&t.b);
    eprintln!(
        "retired a 1,000-contact superseded book in {:?}",
        started.elapsed()
    );
    assert_eq!(retired[0]["contacts_removed"], 1_000);
    assert!(t.b.pending_outbox().unwrap().len() > 1_000);
}

/// Retires Y on B after the takeover and delivers everything to C.
fn replaced_on_both(t: &mut Superseded) {
    take_over(&t.b);
    assert_eq!(retire(&t.b)[0]["book_id"], "Y");
    t.server.upload(&t.b);
    assert_eq!(t.server.sync(&t.c).quarantined, 0);
    assert_eq!(listed(&t.c, "Y")["superseded_by"], "X");
}

fn active_y(b_cfg: &ClientConfig, generation: &str) -> Value {
    let mut y = book(b_cfg, "Y");
    y["generation"] = json!(generation);
    y
}

/// A Y contact at a revision above any B published, as a delayed or forged record would carry.
fn late_contact(id: &str) -> Value {
    let mut late = contact("Y", id, "Late");
    late["revision"] = json!("9");
    late
}

fn in_a_day() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
        + 86_400
}

fn ledger_status(b_cfg: &ClientConfig, request_id: &str) -> (String, Option<String>) {
    sqlcipher(&b_cfg.database_path)
        .query_row(
            "SELECT status,json_extract(observed,'$.reason') FROM contact_edit_ledger WHERE request_id=?",
            [request_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap()
}

#[test]
fn a_replaced_book_refuses_owner_capture_and_scans() {
    let mut t = superseded_mint(MintOrder::LateHint, 11, &json!({}));
    let open_scan = |mut input: Value| {
        input["schema_version"] = json!(1);
        input["scan_id"] = json!("open-Y");
        input.to_string()
    };
    t.b.begin_contact_scan(&open_scan(json!({"book_id": "Y", "generation": "1",
        "access": "full", "authoritative": true})))
        .unwrap();
    replaced_on_both(&mut t);
    let replaced = Error::InvalidRequest("contact book was replaced");
    assert_eq!(
        t.b.observe_contact_scan(&open_scan(
            json!({"contact": contact("Y", "mint-0", "Mint 0")})
        ))
        .unwrap_err(),
        replaced,
        "a scan under way when the book was replaced observes nothing"
    );
    let finished: Value = serde_json::from_str(
        &t.b.finish_contact_scan(&open_scan(json!({"complete": true})))
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        (finished["deleted"].clone(), finished["held"].clone()),
        (json!(0), json!(0))
    );
    for generation in ["1", "2"] {
        assert_eq!(
            t.b.capture_contact_book(
                &json!({"schema_version": 1, "book": active_y(&t.b_cfg, generation),
                    "contacts": [contact("Y", "mint-0", "Mint 0")]})
                .to_string(),
            )
            .unwrap_err(),
            replaced
        );
    }
    assert_eq!(
        t.b.begin_contact_scan(
            &json!({"schema_version": 1, "scan_id": "late-Y", "book_id": "Y", "generation": "1",
                "access": "full", "authoritative": true})
            .to_string(),
        )
        .unwrap_err(),
        replaced
    );
    let y = listed(&t.b, "Y");
    assert_eq!(y["state"], "retired");
    assert_eq!(y["superseded_by"], "X");
    assert_eq!(y["contact_count"], 0);
    // Only core marks a book replaced.
    let mut x = book(&t.b_cfg, "X");
    x["superseded_by"] = json!("Y");
    t.b.capture_contact_book(&json!({"schema_version": 1, "book": x, "contacts": []}).to_string())
        .unwrap();
    assert!(listed(&t.b, "X").get("superseded_by").is_none());
}

#[test]
fn late_requests_for_a_replaced_book_never_get_a_permit() {
    let mut t = superseded_mint(
        MintOrder::LateHint,
        11,
        &json!({"capabilities": {"read": true, "write": true}, "policy": {"remote_edits": "auto"}}),
    );
    replaced_on_both(&mut t);
    let target = t.b_cfg.device_id.to_string();
    let update = json!({"schema_version": 1, "request_id": "r-late", "target_owner": target,
        "book_id": "Y", "kind": "update", "contact_id": "mint-0", "base_revision": "1",
        "expires_at": in_a_day(),
        "patches": [{"op": "replace", "path": "name.given", "value": "Renamed"}]});
    let restore = json!({"schema_version": 1, "request_id": "r-late-restore",
        "target_owner": target, "book_id": "Y", "kind": "create", "expires_at": in_a_day(),
        "display_name": "Mint 1", "name": {"given": "Mint 1"}, "phones": [], "emails": [],
        "addresses": [], "provenance": {"restored_from": "mint-1"}});
    let late: Vec<Envelope> = [update, restore]
        .into_iter()
        .zip(9_000..)
        .map(|(request, sequence)| {
            sealed(
                &t.vault,
                t.c_cfg.device_id,
                sequence,
                PrivatePayload::ContactEditRequest { request },
            )
        })
        .collect();
    assert_eq!(sync(&late, &t.b).quarantined, 0);

    let permit: Value = serde_json::from_str(
        &t.b.next_contact_apply_permit(r#"{"schema_version":1,"request_id":"r-late"}"#)
            .unwrap(),
    )
    .unwrap();
    assert_eq!(permit["status"], "rejected");
    assert_eq!(permit["reason"], "book_replaced");
    // The next retirement pass closes what the host never asked a permit for.
    assert_eq!(retire(&t.b), json!([]));
    assert_eq!(
        ledger_status(&t.b_cfg, "r-late-restore"),
        ("rejected".into(), Some("book_replaced".into()))
    );
    let claims: i64 = sqlcipher(&t.b_cfg.database_path)
        .query_row(
            "SELECT COUNT(*) FROM contact_restore_claims WHERE book_id='Y'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(claims, 0, "no restore was ever claimed");
}

#[test]
fn retirement_releases_an_unresolved_restore_claim() {
    let mut t = superseded_mint(
        MintOrder::LateHint,
        11,
        &json!({"capabilities": {"read": true, "write": true}, "policy": {"remote_edits": "auto"}}),
    );
    // Before the takeover, B's own scan of Y removes mint-0 and mint-1.
    t.b.begin_contact_scan(
        &json!({"schema_version": 1, "scan_id": "scan-Y", "book_id": "Y", "generation": "1",
            "access": "full", "authoritative": true})
        .to_string(),
    )
    .unwrap();
    for i in 2..11 {
        t.b.observe_contact_scan(
            &json!({"schema_version": 1, "scan_id": "scan-Y",
                "contact": contact("Y", &format!("mint-{i}"), &format!("Mint {i}"))})
            .to_string(),
        )
        .unwrap();
    }
    t.b.finish_contact_scan(r#"{"schema_version":1,"scan_id":"scan-Y","complete":true}"#)
        .unwrap();
    t.server.upload(&t.b);
    assert_eq!(t.server.sync(&t.c).quarantined, 0);
    let restore: Value = serde_json::from_str(
        &t.c.restore_contact_json(r#"{"book_id":"Y","contact_id":"mint-0"}"#)
            .unwrap(),
    )
    .unwrap();
    let request_id = restore["request_id"].as_str().unwrap().to_owned();
    t.server.upload(&t.c);
    assert_eq!(t.server.sync(&t.b).quarantined, 0);
    let permit: Value = serde_json::from_str(
        &t.b.next_contact_apply_permit(
            &json!({"schema_version": 1, "request_id": request_id}).to_string(),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(permit["status"], "permit");
    take_over(&t.b);

    assert_eq!(retire(&t.b)[0]["contacts_removed"], 9);
    let claim: String = sqlcipher(&t.b_cfg.database_path)
        .query_row(
            "SELECT status FROM contact_restore_claims WHERE book_id='Y' AND tombstone_id='mint-0'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(claim, "failed");
    assert_eq!(
        ledger_status(&t.b_cfg, &request_id),
        ("failed".into(), Some("book_replaced".into()))
    );
    assert!(
        t.b.reconcile_contact_apply(
            &json!({"schema_version": 1, "request_id": request_id, "outcome": "applied",
                "observed": contact("Y", "mint-0", "Mint 0")})
            .to_string(),
        )
        .is_err(),
        "a late reconcile never repopulates the replaced book"
    );
    assert_eq!(listed(&t.b, "Y")["contact_count"], 0);
}

#[test]
fn peers_ignore_late_records_for_a_replaced_book() {
    let mut t = superseded_mint(MintOrder::LateHint, 11, &json!({}));
    replaced_on_both(&mut t);
    let late = [
        PrivatePayload::ContactUpserted {
            book: active_y(&t.b_cfg, "1"),
            contact: late_contact("mint-0"),
        },
        PrivatePayload::ContactUpserted {
            book: active_y(&t.b_cfg, "2"),
            contact: late_contact("late-contact"),
        },
        PrivatePayload::ContactBookState {
            book: active_y(&t.b_cfg, "3"),
        },
    ]
    .into_iter()
    .zip(9_000..)
    .map(|(payload, sequence)| sealed(&t.vault, t.b_cfg.device_id, sequence, payload))
    .collect::<Vec<_>>();
    assert_eq!(sync(&late, &t.c).quarantined, 0);
    let y = listed(&t.c, "Y");
    assert_eq!(y["state"], "retired");
    assert_eq!(y["superseded_by"], "X");
    assert_eq!(y["contact_count"], 0);
}

/// A copy of `template`, a real record with compaction metadata, that carries `payload`.
fn resealed(
    vault: &Vault,
    template: &Envelope,
    sequence: u64,
    payload: &PrivatePayload,
) -> Envelope {
    let root = derive_root_key(PASSPHRASE, &vault.profile).unwrap();
    let key = derive_purpose_key(&root, &vault.profile, KeyPurpose::Event).unwrap();
    let mut envelope = template.clone();
    envelope.envelope_id = EnvelopeId::new();
    envelope.producer_sequence = SourceSequence(sequence);
    let mut plain = serde_json::to_value(payload).unwrap();
    plain["compaction"] = serde_json::to_value(&envelope.compaction).unwrap();
    let encrypted = encrypt(
        &key,
        &envelope.aad_bytes().unwrap(),
        &serde_json::to_vec(&plain).unwrap(),
    )
    .unwrap();
    envelope.ciphertext = [encrypted.nonce.as_slice(), &encrypted.ciphertext].concat();
    envelope
}

/// A fresh device named `name` that imported `log` as one compacted resync snapshot.
fn fresh_import(t: &Superseded, name: &str, log: &[Envelope]) -> Client {
    let fresh = unlocked(&config(&t.dir, name, &t.vault), &t.vault);
    let records: Vec<SnapshotRecord> = log
        .iter()
        .zip(1..)
        .map(|(envelope, cursor)| SnapshotRecord {
            cursor: Cursor(cursor),
            envelope: envelope.clone(),
        })
        .collect();
    let session = fresh
        .begin_snapshot_with_compaction(
            Cursor(records.len() as u64),
            records.len() as u64,
            SnapshotPurpose::Resync,
            Some(9),
        )
        .unwrap();
    fresh
        .append_snapshot_page(session.generation, &records)
        .unwrap();
    fresh.finish_snapshot(session.generation).unwrap();
    loop {
        let step = fresh.apply_pending(1_000).unwrap();
        if step.applied + step.drained + step.quarantined + step.superseded == 0
            && step.snapshot_remaining == 0
        {
            break;
        }
    }
    assert_eq!(
        fresh
            .snapshot_projection_status()
            .unwrap()
            .map(|status| status.state),
        Some(SnapshotProjectionState::Promoted)
    );
    fresh
}

#[test]
fn a_fresh_import_never_repopulates_a_replaced_book() {
    let mut t = superseded_mint(MintOrder::LateHint, 11, &json!({}));
    replaced_on_both(&mut t);
    let template = t
        .server
        .log
        .iter()
        .find(|envelope| {
            envelope.producer_device_id == t.b_cfg.device_id
                && matches!(
                    opened(&t.vault, envelope),
                    PrivatePayload::ContactUpserted { ref book, .. } if book["id"] == "Y"
                )
        })
        .unwrap();
    let stray = |generation: &str| {
        resealed(
            &t.vault,
            template,
            9_000,
            &PrivatePayload::ContactUpserted {
                book: active_y(&t.b_cfg, generation),
                contact: late_contact("late-contact"),
            },
        )
    };
    let log = &t.server.log;
    let cases: [(&str, Vec<Envelope>); 3] = [
        // A delayed record staged before the replaced state.
        (
            "stray-first",
            [stray("1")].iter().chain(log).cloned().collect(),
        ),
        (
            "stray-last",
            log.iter().chain([&stray("1")]).cloned().collect(),
        ),
        // A later capture at a new generation, as an app without the owner fence would publish.
        (
            "recapture-last",
            log.iter().chain([&stray("2")]).cloned().collect(),
        ),
    ];
    for (name, log) in cases {
        let fresh = fresh_import(&t, name, &log);
        let y = listed(&fresh, "Y");
        assert_eq!(y["state"], "retired", "{name}");
        assert_eq!(y["superseded_by"], "X", "{name}");
        assert_eq!(y["contact_count"], 0, "{name}");
        assert_eq!(listed(&fresh, "X")["state"], "active", "{name}");
    }
}
