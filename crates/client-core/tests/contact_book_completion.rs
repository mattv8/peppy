//! Receiver completion of a vault-wide book deletion. A device that applies a book fence queues one
//! terminal member for each covered contact it still holds, unless a terminal member for that
//! contact is already known. The owner device never queues one, and nothing is queued while locked.
use crate::common::*;
use peppy_crypto::{
    EncryptedEnvelope, KeyPurpose, decrypt, derive_purpose_key, derive_root_key, encrypt,
};
use rusqlite::{Connection, params};
use serde_json::{Value, json};
use std::path::Path;
use tempfile::TempDir;

const BOOK: &str = "address-book";

fn book(owner: DeviceId, generation: &str) -> Value {
    json!({
        "id": BOOK,
        "owner_device_id": owner.to_string(),
        "generation": generation,
        "state": "active",
        "region": "US"
    })
}

fn contact_body(id: &str) -> Value {
    json!({
        "id": id,
        "book_id": BOOK,
        "revision": "1",
        "display_name": id,
        "name": {"given": id},
        "phones": [],
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

/// Journals one page and reports its end as the server head, then applies it. A page that ends at
/// the head is the common host case, so completion is never held back by these helpers.
fn ingest_and_apply(envelopes: &[Envelope], client: &Client) -> ApplyReport {
    let start = client.receive_cursor().unwrap().0;
    for (index, envelope) in envelopes.iter().enumerate() {
        client
            .ingest(envelope, Cursor(start + index as u64 + 1))
            .unwrap_or_else(|error| panic!("failed to ingest envelope {index}: {error:?}"));
    }
    client
        .observe_replay_head(Cursor(start + envelopes.len() as u64))
        .unwrap();
    client.apply_pending(100).unwrap()
}

fn seal(vault: &Vault, producer: DeviceId, sequence: u64, payload: &PrivatePayload) -> Envelope {
    let plain = serde_json::to_vec(payload).unwrap();
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
    let encrypted = encrypt(&key, &envelope.aad_bytes().unwrap(), &plain).unwrap();
    envelope.ciphertext = [encrypted.nonce.as_slice(), &encrypted.ciphertext].concat();
    envelope
}

/// The owner's book state, then one upsert per `(contact, book generation)` pair, all from `owner`.
fn owners_book(vault: &Vault, owner: DeviceId, contacts: &[(&str, &str)]) -> Vec<Envelope> {
    let mut events = vec![seal(
        vault,
        owner,
        1,
        &PrivatePayload::ContactBookState {
            book: book(owner, "1"),
        },
    )];
    for (index, (id, generation)) in contacts.iter().enumerate() {
        events.push(seal(
            vault,
            owner,
            index as u64 + 2,
            &PrivatePayload::ContactUpserted {
                book: book(owner, generation),
                contact: contact_body(id),
            },
        ));
    }
    events
}

fn fence(vault: &Vault, producer: DeviceId, sequence: u64, owner: DeviceId) -> Envelope {
    seal(
        vault,
        producer,
        sequence,
        &PrivatePayload::ContactBookDeleted {
            book_id: BOOK.into(),
            owner_device_id: owner.to_string(),
            generation: "1".into(),
            deleted_at_ms: 1_000,
        },
    )
}

fn member(
    vault: &Vault,
    producer: DeviceId,
    sequence: u64,
    owner: DeviceId,
    contact: &str,
) -> Envelope {
    seal(
        vault,
        producer,
        sequence,
        &PrivatePayload::ContactBookDeletedMember {
            book_id: BOOK.into(),
            contact_id: contact.into(),
            owner_device_id: owner.to_string(),
            generation: "1".into(),
        },
    )
}

/// The decrypted payload of every queued (unacknowledged) outbox envelope.
fn queued_payloads(client: &Client, vault: &Vault) -> Vec<(Envelope, PrivatePayload)> {
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

/// Contact ids of the queued member records, sorted.
fn queued_members(client: &Client, vault: &Vault) -> Vec<String> {
    let mut ids: Vec<String> = queued_payloads(client, vault)
        .into_iter()
        .filter_map(|(_, payload)| match payload {
            PrivatePayload::ContactBookDeletedMember { contact_id, .. } => Some(contact_id),
            _ => None,
        })
        .collect();
    ids.sort();
    ids
}

fn queued_fences(client: &Client, vault: &Vault) -> usize {
    queued_payloads(client, vault)
        .into_iter()
        .filter(|(_, payload)| matches!(payload, PrivatePayload::ContactBookDeleted { .. }))
        .count()
}

/// Counts the owed member records still waiting on this device.
fn owed(path: &Path) -> i64 {
    count(
        path,
        "SELECT COUNT(*) FROM contact_book_deletion_completion_contacts",
    )
}

/// The receiver's client, unlocked and compaction-ready, plus its database path.
fn receiver(dir: &TempDir, vault: &Vault, name: &str) -> (ClientConfig, Client) {
    let config = config(dir, name, vault);
    let client = unlocked(&config, vault);
    (config, client)
}

/// Owner `owner` publishes `c1` and `c2` at book generation 1. A deleter then fences generation 1.
/// The deleter holds only `c1`, so it sends one member for `c1`, which covers `c1` alone.
fn deleter_covering_c1_only(vault: &Vault, owner: DeviceId) -> Vec<Envelope> {
    let deleter = DeviceId::new();
    vec![
        fence(vault, deleter, 1, owner),
        member(vault, deleter, 2, owner, "c1"),
    ]
}

#[test]
fn deleter_that_covered_only_c1_makes_the_receiver_queue_one_member_for_c2() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let owner = DeviceId::new();
    let (config, client) = receiver(&dir, &vault, "receiver");
    ingest_and_apply(
        &owners_book(&vault, owner, &[("c1", "1"), ("c2", "1")]),
        &client,
    );

    let report = ingest_and_apply(&deleter_covering_c1_only(&vault, owner), &client);

    assert_eq!(report.quarantined, 0);
    assert_eq!(queued_members(&client, &vault), vec!["c2"]);
    assert_eq!(owed(&config.database_path), 0, "the owed work is settled");
}

#[test]
fn the_queued_member_carries_the_contacts_compaction_identity() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let owner = DeviceId::new();
    let (config, client) = receiver(&dir, &vault, "receiver");
    ingest_and_apply(
        &owners_book(&vault, owner, &[("c1", "1"), ("c2", "1")]),
        &client,
    );
    ingest_and_apply(&deleter_covering_c1_only(&vault, owner), &client);

    let (envelope, _) = queued_payloads(&client, &vault)
        .into_iter()
        .find(|(_, payload)| {
            matches!(payload, PrivatePayload::ContactBookDeletedMember { contact_id, .. } if contact_id == "c2")
        })
        .expect("the c2 member is queued");
    let compaction = envelope
        .compaction
        .as_ref()
        .expect("members are compactable");
    assert!(compaction.terminal);
    assert_eq!(envelope.producer_device_id, config.device_id);
}

#[test]
fn members_received_before_the_fence_in_one_batch_still_leave_only_c2() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let owner = DeviceId::new();
    let (_config, client) = receiver(&dir, &vault, "receiver");
    ingest_and_apply(
        &owners_book(&vault, owner, &[("c1", "1"), ("c2", "1")]),
        &client,
    );
    let deleter = DeviceId::new();

    ingest_and_apply(
        &[
            member(&vault, deleter, 1, owner, "c1"),
            fence(&vault, deleter, 2, owner),
        ],
        &client,
    );

    assert_eq!(queued_members(&client, &vault), vec!["c2"]);
}

#[test]
fn a_second_receiver_that_already_has_the_first_receivers_member_emits_nothing() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let owner = DeviceId::new();
    let book_events = owners_book(&vault, owner, &[("c1", "1"), ("c2", "1")]);
    let deleter_events = deleter_covering_c1_only(&vault, owner);
    let (_first_config, first) = receiver(&dir, &vault, "first");
    ingest_and_apply(&book_events, &first);
    ingest_and_apply(&deleter_events, &first);
    let first_c2 = queued_payloads(&first, &vault)
        .into_iter()
        .find(|(_, payload)| {
            matches!(payload, PrivatePayload::ContactBookDeletedMember { contact_id, .. } if contact_id == "c2")
        })
        .expect("the first receiver queued c2")
        .0;

    let (_second_config, second) = receiver(&dir, &vault, "second");
    ingest_and_apply(&book_events, &second);
    let mut received = deleter_events;
    received.push(first_c2);
    ingest_and_apply(&received, &second);

    assert!(
        queued_members(&second, &vault).is_empty(),
        "c2 already has a terminal member from the first receiver"
    );
}

#[test]
fn owner_device_never_queues_members_for_its_own_book() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let (config, owner) = {
        let config = config(&dir, "owner", &vault);
        let owner = unlocked(&config, &vault);
        (config, owner)
    };
    let own = config.device_id;
    ingest_and_apply(
        &owners_book(&vault, own, &[("c1", "1"), ("c2", "1")]),
        &owner,
    );

    let report = ingest_and_apply(&[fence(&vault, DeviceId::new(), 1, own)], &owner);

    assert_eq!(
        report.quarantined, 1,
        "a fence naming this device is refused"
    );
    assert!(queued_members(&owner, &vault).is_empty());
    assert_eq!(owed(&config.database_path), 0);
}

#[test]
fn a_restart_between_the_fence_and_the_drain_keeps_the_owed_members() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let owner = DeviceId::new();
    let (config, client) = receiver(&dir, &vault, "receiver");
    ingest_and_apply(
        &owners_book(&vault, owner, &[("c1", "1"), ("c2", "1")]),
        &client,
    );
    // Compaction is not ready, so the drain cannot enqueue and the work stays owed.
    client.set_server_compaction_supported(false).unwrap();
    ingest_and_apply(&[fence(&vault, DeviceId::new(), 1, owner)], &client);
    assert!(queued_members(&client, &vault).is_empty());
    assert_eq!(owed(&config.database_path), 2);
    // Owed work the drain cannot enqueue yet is not reported as pending, so hosts do not spin.
    assert!(!client.sync_work_status().unwrap().pending_apply);
    drop(client);

    let reopened = unlocked(&config, &vault);
    reopened.apply_pending(100).unwrap();

    assert_eq!(queued_members(&reopened, &vault), vec!["c1", "c2"]);
    assert_eq!(owed(&config.database_path), 0);
}

#[test]
fn a_locked_receiver_keeps_its_owed_members_until_it_unlocks() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let owner = DeviceId::new();
    let (config, client) = receiver(&dir, &vault, "receiver");
    ingest_and_apply(
        &owners_book(&vault, owner, &[("c1", "1"), ("c2", "1")]),
        &client,
    );
    client.set_server_compaction_supported(false).unwrap();
    ingest_and_apply(&[fence(&vault, DeviceId::new(), 1, owner)], &client);
    drop(client);

    let locked = open(&config);
    locked.apply_pending(100).unwrap();
    assert_eq!(
        owed(&config.database_path),
        2,
        "locked: nothing is emitted or dropped"
    );
    drop(locked);

    let unlocked_client = unlocked(&config, &vault);
    unlocked_client.apply_pending(100).unwrap();
    assert_eq!(queued_members(&unlocked_client, &vault), vec!["c1", "c2"]);
    assert_eq!(owed(&config.database_path), 0);
}

#[test]
fn a_book_larger_than_one_pass_drains_across_passes() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let owner = DeviceId::new();
    let (config, client) = receiver(&dir, &vault, "receiver");
    // Seeds the stored book directly: capture and sync of 300 contacts costs time this test does
    // not measure. Each held contact is covered (no publication owner or generation recorded).
    {
        let seeded = sqlcipher(&config.database_path);
        seeded
            .execute(
                "INSERT INTO contact_books(id,owner_device_id,generation,state,body,forgotten) VALUES(?,?,1,'active',?,0)",
                params![BOOK, owner.to_string(), book(owner, "1").to_string()],
            )
            .unwrap();
        for index in 0..300 {
            let id = format!("c{index:04}");
            let body = contact_body(&id);
            seeded
                .execute(
                    "INSERT INTO contacts(id,book_id,revision,body,deleted_at) VALUES(?,?,1,?,NULL)",
                    params![id, BOOK, body.to_string()],
                )
                .unwrap();
        }
    }

    ingest_and_apply(&[fence(&vault, DeviceId::new(), 1, owner)], &client);
    assert_eq!(
        queued_members(&client, &vault).len(),
        256,
        "one pass is bounded"
    );
    assert_eq!(owed(&config.database_path), 44);

    client.apply_pending(100).unwrap();
    assert_eq!(queued_members(&client, &vault).len(), 300);
    assert_eq!(owed(&config.database_path), 0);
}

/// The decrypted payload of one sealed Event envelope.
fn payload_of(vault: &Vault, envelope: &Envelope) -> PrivatePayload {
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

#[test]
fn a_snapshot_promotion_queues_members_for_contacts_the_fence_covers() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let owner_config = config(&dir, "owner", &vault);
    let (config, client) = receiver(&dir, &vault, "receiver");
    let mut server = Server {
        log: owners_book(&vault, owner_config.device_id, &[("c1", "1"), ("c2", "1")]),
    };
    // The receiver holds c1 and c2 live before the deletion exists on the server.
    server.sync(&client);
    assert_eq!(
        count(&config.database_path, "SELECT COUNT(*) FROM contacts"),
        2
    );

    // A real deleter that holds both contacts fences the book and publishes a member for each.
    // Its member for c2 is then dropped from the log, so the deleter covered only c1.
    let deleter_config = crate::common::config(&dir, "deleter", &vault);
    let deleter = unlocked(&deleter_config, &vault);
    server.sync(&deleter);
    call_delete(&deleter, owner_config.device_id);
    server.upload(&deleter);
    server.log.retain(|envelope| {
        !matches!(
            payload_of(&vault, envelope),
            PrivatePayload::ContactBookDeletedMember { ref contact_id, .. } if contact_id == "c2"
        )
    });
    assert_eq!(
        server.log.len(),
        3 + 2,
        "owner history, the fence and the c1 member"
    );

    // A receiver that was offline imports the whole history. The deletion is then staged and takes
    // effect at promotion, not as a live record, and promotion captures the contacts it covers.
    let records: Vec<SnapshotRecord> = server
        .log
        .iter()
        .enumerate()
        .map(|(index, envelope)| SnapshotRecord {
            cursor: Cursor(index as u64 + 1),
            envelope: envelope.clone(),
        })
        .collect();
    let session = client
        .begin_snapshot_with_compaction(
            Cursor(records.len() as u64),
            records.len() as u64,
            SnapshotPurpose::Resync,
            Some(9),
        )
        .unwrap();
    client
        .append_snapshot_page(session.generation, &records)
        .unwrap();
    client.finish_snapshot(session.generation).unwrap();
    client
        .observe_replay_head(Cursor(records.len() as u64))
        .unwrap();
    for _ in 0..200 {
        client.apply_pending(1).unwrap();
        if matches!(
            client
                .snapshot_projection_status()
                .unwrap()
                .map(|s| s.state),
            Some(SnapshotProjectionState::Promoted)
        ) {
            break;
        }
    }

    // Members wait until every received record is applied, so the tail after the promoted stage
    // (the deleter's own member) is applied before the completion pass is expected to emit.
    client.apply_pending(100).unwrap();
    assert_eq!(queued_members(&client, &vault), vec!["c2"]);
    assert_eq!(
        count(&config.database_path, "SELECT COUNT(*) FROM contacts"),
        0
    );
}

fn call_delete(client: &Client, owner: DeviceId) {
    let input = json!({
        "book_id": BOOK,
        "revoked_device_ids": [owner.to_string()],
        "sync_current": true
    });
    client.delete_contact_book(&input.to_string()).unwrap();
}

#[test]
fn a_member_already_applied_before_the_fence_settles_the_contact() {
    // Duplicate of the in-batch reversal, with the member in an earlier batch.
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let owner = DeviceId::new();
    let (_config, client) = receiver(&dir, &vault, "receiver");
    ingest_and_apply(
        &owners_book(&vault, owner, &[("c1", "1"), ("c2", "1")]),
        &client,
    );
    let deleter = DeviceId::new();
    ingest_and_apply(&[member(&vault, deleter, 1, owner, "c1")], &client);
    assert!(queued_members(&client, &vault).is_empty());

    ingest_and_apply(&[fence(&vault, deleter, 2, owner)], &client);

    assert_eq!(queued_members(&client, &vault), vec!["c2"]);
    assert_eq!(queued_fences(&client, &vault), 0);
}

/// The owner republishes `c1` at book generation 2, after the generation-1 fence of its book.
fn republication(vault: &Vault, owner: DeviceId, first_sequence: u64) -> Vec<Envelope> {
    vec![
        seal(
            vault,
            owner,
            first_sequence,
            &PrivatePayload::ContactBookState {
                book: book(owner, "2"),
            },
        ),
        seal(
            vault,
            owner,
            first_sequence + 1,
            &PrivatePayload::ContactUpserted {
                book: book(owner, "2"),
                contact: contact_body("c1"),
            },
        ),
    ]
}

/// The generation of the stored `c1` publication, if the contact is live.
fn published_generation(path: &Path) -> i64 {
    count(
        path,
        "SELECT COALESCE(publication_generation,-1) FROM contacts WHERE id='c1'",
    )
}

#[test]
fn a_republication_in_the_same_batch_as_the_fence_withdraws_the_owed_member() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let owner = DeviceId::new();
    let (config, client) = receiver(&dir, &vault, "receiver");
    ingest_and_apply(&owners_book(&vault, owner, &[("c1", "1")]), &client);

    let mut batch = vec![fence(&vault, DeviceId::new(), 1, owner)];
    batch.extend(republication(&vault, owner, 3));
    ingest_and_apply(&batch, &client);

    assert!(
        queued_members(&client, &vault).is_empty(),
        "c1 continued at generation 2, so a generation-1 member would supersede it"
    );
    assert_eq!(owed(&config.database_path), 0);
    assert_eq!(published_generation(&config.database_path), 2);
}

#[test]
fn a_republication_received_after_the_fence_withdraws_the_owed_member() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let owner = DeviceId::new();
    let (config, client) = receiver(&dir, &vault, "receiver");
    ingest_and_apply(&owners_book(&vault, owner, &[("c1", "1")]), &client);
    // The fence is applied with no reported head, so its completion stays deferred.
    ingest_only(&[fence(&vault, DeviceId::new(), 1, owner)], &client);
    client.apply_pending(100).unwrap();
    assert_eq!(owed(&config.database_path), 1);
    assert!(queued_members(&client, &vault).is_empty());
    drop(client);

    // A restart before the drain, then the republication in a later batch.
    let reopened = unlocked(&config, &vault);
    ingest_and_apply(&republication(&vault, owner, 3), &reopened);

    assert!(queued_members(&reopened, &vault).is_empty());
    assert_eq!(owed(&config.database_path), 0);
    assert_eq!(published_generation(&config.database_path), 2);
}

/// Journals envelopes without applying them, as a host does for one replay page.
fn ingest_only(envelopes: &[Envelope], client: &Client) {
    let start = client.receive_cursor().unwrap().0;
    for (index, envelope) in envelopes.iter().enumerate() {
        client
            .ingest(envelope, Cursor(start + index as u64 + 1))
            .unwrap();
    }
}

#[test]
fn deleter_members_on_a_later_page_than_the_fence_are_not_duplicated() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let owner = DeviceId::new();
    let (config, client) = receiver(&dir, &vault, "receiver");
    ingest_and_apply(
        &owners_book(&vault, owner, &[("c1", "1"), ("c2", "1")]),
        &client,
    );
    let deleter = DeviceId::new();
    // The server head (6) is past both pages: the fence is page one, the members are page two.
    ingest_only(&[fence(&vault, deleter, 1, owner)], &client);
    client.observe_replay_head(Cursor(6)).unwrap();
    client.apply_pending(100).unwrap();
    assert!(
        queued_members(&client, &vault).is_empty(),
        "receipt has not reached the head, so the deleter's members are not yet known"
    );
    assert_eq!(owed(&config.database_path), 2);

    ingest_only(
        &[
            member(&vault, deleter, 2, owner, "c1"),
            member(&vault, deleter, 3, owner, "c2"),
        ],
        &client,
    );
    client.observe_replay_head(Cursor(6)).unwrap();
    client.apply_pending(100).unwrap();

    assert!(queued_members(&client, &vault).is_empty());
    assert_eq!(owed(&config.database_path), 0);
}

#[test]
fn a_member_missing_from_the_deleter_is_queued_once_after_catch_up() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let owner = DeviceId::new();
    let (config, client) = receiver(&dir, &vault, "receiver");
    ingest_and_apply(
        &owners_book(&vault, owner, &[("c1", "1"), ("c2", "1")]),
        &client,
    );
    let deleter = DeviceId::new();
    ingest_only(&[fence(&vault, deleter, 1, owner)], &client);
    client.observe_replay_head(Cursor(5)).unwrap();
    client.apply_pending(100).unwrap();
    assert!(queued_members(&client, &vault).is_empty());

    ingest_only(&[member(&vault, deleter, 2, owner, "c1")], &client);
    client.observe_replay_head(Cursor(5)).unwrap();
    client.apply_pending(100).unwrap();
    client.apply_pending(100).unwrap();

    assert_eq!(queued_members(&client, &vault), vec!["c2"]);
    assert_eq!(owed(&config.database_path), 0);
}

#[test]
fn two_receivers_of_a_split_deletion_each_queue_only_the_missing_member() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let owner = DeviceId::new();
    let book_events = owners_book(&vault, owner, &[("c1", "1"), ("c2", "1")]);
    let deleter = DeviceId::new();
    let fence_page = [fence(&vault, deleter, 1, owner)];
    let member_page = [member(&vault, deleter, 2, owner, "c1")];

    for name in ["first", "second"] {
        let (_config, client) = receiver(&dir, &vault, name);
        ingest_and_apply(&book_events, &client);
        ingest_only(&fence_page, &client);
        client.observe_replay_head(Cursor(5)).unwrap();
        client.apply_pending(100).unwrap();
        ingest_only(&member_page, &client);
        client.observe_replay_head(Cursor(5)).unwrap();
        client.apply_pending(100).unwrap();
        assert_eq!(
            queued_members(&client, &vault),
            vec!["c2"],
            "{name}: c2 is the only uncovered contact, and c1 is never re-emitted"
        );
    }
}

/// A fresh receiver imports `log` as one snapshot, promotes it, and reports the queued members.
fn fresh_import_queues(vault: &Vault, log: &[Envelope]) -> Vec<String> {
    let dir = TempDir::new().unwrap();
    let (_config, client) = receiver(&dir, vault, "fresh");
    let records: Vec<SnapshotRecord> = log
        .iter()
        .enumerate()
        .map(|(index, envelope)| SnapshotRecord {
            cursor: Cursor(index as u64 + 1),
            envelope: envelope.clone(),
        })
        .collect();
    let session = client
        .begin_snapshot_with_compaction(
            Cursor(records.len() as u64),
            records.len() as u64,
            SnapshotPurpose::Resync,
            Some(9),
        )
        .unwrap();
    client
        .append_snapshot_page(session.generation, &records)
        .unwrap();
    client.finish_snapshot(session.generation).unwrap();
    client
        .observe_replay_head(Cursor(records.len() as u64))
        .unwrap();
    for _ in 0..200 {
        client.apply_pending(1).unwrap();
        if matches!(
            client
                .snapshot_projection_status()
                .unwrap()
                .map(|s| s.state),
            Some(SnapshotProjectionState::Promoted)
        ) {
            break;
        }
    }
    for _ in 0..20 {
        client.apply_pending(100).unwrap();
    }
    queued_members(&client, vault)
}

/// The owner's history and a real deleter's fence and members. Real clients seal them, so each
/// record carries the compaction metadata a snapshot retains.
struct RealDeletion {
    vault: Vault,
    history: Vec<Envelope>,
    deletion: Vec<Envelope>,
}

fn real_deletion(dir: &TempDir) -> RealDeletion {
    let vault = Vault::new();
    let owner_config = config(dir, "owner", &vault);
    let owner = unlocked(&owner_config, &vault);
    owner
        .capture_contact_book(
            &json!({
                "schema_version": 1,
                "book": book(owner_config.device_id, "1"),
                "contacts": [contact_body("c1"), contact_body("c2")]
            })
            .to_string(),
        )
        .unwrap();
    let history = owner.pending_outbox().unwrap();
    let deleter = unlocked(&config(dir, "deleter", &vault), &vault);
    ingest_and_apply(&history, &deleter);
    call_delete(&deleter, owner_config.device_id);
    RealDeletion {
        deletion: deleter.pending_outbox().unwrap(),
        history,
        vault,
    }
}

/// The one envelope of a kind, selected by its decrypted payload.
fn pick(
    vault: &Vault,
    envelopes: &[Envelope],
    wanted: impl Fn(&PrivatePayload) -> bool,
) -> Envelope {
    envelopes
        .iter()
        .find(|envelope| wanted(&payload_of(vault, envelope)))
        .expect("the record is present")
        .clone()
}

#[test]
fn a_fresh_import_owes_an_uncovered_staged_contact_whatever_the_record_order() {
    let dir = TempDir::new().unwrap();
    let real = real_deletion(&dir);
    let vault = &real.vault;
    let of = |envelopes: &[Envelope], id: &'static str| {
        pick(vault, envelopes, move |payload| match payload {
            PrivatePayload::ContactUpserted { contact, .. } => contact["id"] == id,
            PrivatePayload::ContactBookDeletedMember { contact_id, .. } => {
                contact_id.as_str() == id
            }
            _ => false,
        })
    };
    let book_state = pick(vault, &real.history, |payload| {
        matches!(payload, PrivatePayload::ContactBookState { .. })
    });
    let fence_record = pick(vault, &real.deletion, |payload| {
        matches!(payload, PrivatePayload::ContactBookDeleted { .. })
    });
    let (c1, c2) = (of(&real.history, "c1"), of(&real.history, "c2"));
    let (member_c1, member_c2) = (of(&real.deletion, "c1"), of(&real.deletion, "c2"));

    // c1 is retained as a publication before the fence, or after it. The deleter sends c2's
    // member only, so c1 is the uncovered contact.
    let publication_first = [
        book_state.clone(),
        c1.clone(),
        c2.clone(),
        fence_record.clone(),
        member_c2.clone(),
    ];
    let fence_first = [
        book_state.clone(),
        fence_record.clone(),
        c1.clone(),
        c2.clone(),
        member_c2.clone(),
    ];
    assert_eq!(fresh_import_queues(vault, &publication_first), vec!["c1"]);
    assert_eq!(fresh_import_queues(vault, &fence_first), vec!["c1"]);

    // Controls: a complete set of members leaves nothing owed, in either order.
    let complete_first = [
        publication_first.as_slice(),
        std::slice::from_ref(&member_c1),
    ]
    .concat();
    let complete_fence_first = [fence_first.as_slice(), std::slice::from_ref(&member_c1)].concat();
    assert!(fresh_import_queues(vault, &complete_first).is_empty());
    assert!(fresh_import_queues(vault, &complete_fence_first).is_empty());
}
