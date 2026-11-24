//! Receiver completion of a vault-wide book deletion. A covered contact owes one terminal member
//! while it has an open publication: a non-terminal record of its compaction identity still on the
//! frontier. A member that names that publication settles it; a member that does not, even one
//! known before or after the fence, leaves it owed. This holds for publications the fence captured,
//! for covered records that arrive after it, and for snapshot promotions. The owner device never
//! queues one, and nothing is queued while locked.
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
    seal_with_compaction(vault, producer, sequence, payload, None)
}

/// Seals `payload` with `compaction`, which is authenticated inside the plaintext next to the payload.
fn seal_with_compaction(
    vault: &Vault,
    producer: DeviceId,
    sequence: u64,
    payload: &PrivatePayload,
    compaction: Option<CompactionMetadata>,
) -> Envelope {
    let mut value = serde_json::to_value(payload).unwrap();
    if let Some(compaction) = &compaction {
        value["compaction"] = serde_json::to_value(compaction).unwrap();
    }
    let plain = serde_json::to_vec(&value).unwrap();
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
        compaction,
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

/// A member that names no publication. It is a terminal record, as every deleter's member is, but
/// it leaves each open publication of its contact on the frontier.
fn member(
    vault: &Vault,
    producer: DeviceId,
    sequence: u64,
    owner: DeviceId,
    contact: &str,
) -> Envelope {
    member_superseding(vault, producer, sequence, owner, contact, &[])
}

/// A member that names `superseded` (producer and sequence) in its compaction metadata, as a deleter
/// that held those publications sends it. Compaction metadata is outside the sealed AAD.
fn member_superseding(
    vault: &Vault,
    producer: DeviceId,
    sequence: u64,
    owner: DeviceId,
    contact: &str,
    superseded: &[(DeviceId, u64)],
) -> Envelope {
    let compaction = CompactionMetadata {
        key: vec![7; 32],
        terminal: true,
        supersedes: superseded
            .iter()
            .map(|(producer, sequence)| CompactionReference {
                producer_device_id: *producer,
                producer_sequence: SourceSequence(*sequence),
            })
            .collect(),
        checkpoint: false,
    };
    seal_with_compaction(
        vault,
        producer,
        sequence,
        &PrivatePayload::ContactBookDeletedMember {
            book_id: BOOK.into(),
            contact_id: contact.into(),
            owner_device_id: owner.to_string(),
            generation: "1".into(),
        },
        Some(compaction),
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

/// Owner `owner` publishes `c1` and `c2` at book generation 1 (`c1` is sequence 2, `c2` sequence 3,
/// see `owners_book`). A deleter then fences generation 1. The deleter holds only `c1`, so it sends
/// one member for `c1`, which supersedes that publication and covers `c1` alone.
fn deleter_covering_c1_only(vault: &Vault, owner: DeviceId) -> Vec<Envelope> {
    let deleter = DeviceId::new();
    vec![
        fence(vault, deleter, 1, owner),
        member_superseding(vault, deleter, 2, owner, "c1", &[(owner, 2)]),
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
            member_superseding(&vault, deleter, 1, owner, "c1", &[(owner, 2)]),
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
    // not measure. Each held contact is covered (no publication owner or generation recorded) and
    // its one open publication is on the frontier, as a learned record is.
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
            seeded
                .execute(
                    "INSERT INTO compaction_frontier(producer_device_id,source_sequence,identity,terminal) VALUES(?,?,?,0)",
                    params![owner.to_string(), index as i64 + 2, format!("contact:{BOOK}:{id}")],
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
    ingest_and_apply(
        &[member_superseding(
            &vault,
            deleter,
            1,
            owner,
            "c1",
            &[(owner, 2)],
        )],
        &client,
    );
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
            member_superseding(&vault, deleter, 2, owner, "c1", &[(owner, 2)]),
            member_superseding(&vault, deleter, 3, owner, "c2", &[(owner, 3)]),
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

    ingest_only(
        &[member_superseding(
            &vault,
            deleter,
            2,
            owner,
            "c1",
            &[(owner, 2)],
        )],
        &client,
    );
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
    let member_page = [member_superseding(
        &vault,
        deleter,
        2,
        owner,
        "c1",
        &[(owner, 2)],
    )];

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
    owner: DeviceId,
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
        owner: owner_config.device_id,
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

/// The record of `contact` in `envelopes`: its publication, or the deleter's member for it.
fn contact_record(vault: &Vault, envelopes: &[Envelope], contact: &'static str) -> Envelope {
    pick(vault, envelopes, move |payload| match payload {
        PrivatePayload::ContactUpserted { contact: body, .. } => body["id"] == contact,
        PrivatePayload::ContactBookDeletedMember { contact_id, .. } => {
            contact_id.as_str() == contact
        }
        _ => false,
    })
}

#[test]
fn a_fresh_import_owes_an_uncovered_staged_contact_whatever_the_record_order() {
    let dir = TempDir::new().unwrap();
    let real = real_deletion(&dir);
    let vault = &real.vault;
    let of = |envelopes: &[Envelope], id: &'static str| contact_record(vault, envelopes, id);
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

#[test]
fn a_fresh_import_owes_a_staged_contact_whose_member_does_not_supersede_it_whatever_the_record_order()
 {
    let dir = TempDir::new().unwrap();
    let real = real_deletion(&dir);
    let vault = &real.vault;
    let of = |envelopes: &[Envelope], id: &'static str| contact_record(vault, envelopes, id);
    let book_state = pick(vault, &real.history, |payload| {
        matches!(payload, PrivatePayload::ContactBookState { .. })
    });
    let fence_record = pick(vault, &real.deletion, |payload| {
        matches!(payload, PrivatePayload::ContactBookDeleted { .. })
    });
    let (c1, c2) = (of(&real.history, "c1"), of(&real.history, "c2"));
    let member_c2 = of(&real.deletion, "c2");
    // The deleter's member for c1 names no publication, so c1's staged publication stays open.
    let plain_member_c1 = member(vault, DeviceId::new(), 1, real.owner, "c1");

    let publication_first = [
        book_state.clone(),
        c1.clone(),
        c2.clone(),
        fence_record.clone(),
        member_c2.clone(),
        plain_member_c1.clone(),
    ];
    let fence_first = [
        book_state.clone(),
        fence_record.clone(),
        c1.clone(),
        c2.clone(),
        member_c2.clone(),
        plain_member_c1.clone(),
    ];
    let member_first = [
        book_state.clone(),
        plain_member_c1.clone(),
        fence_record.clone(),
        c1.clone(),
        c2.clone(),
        member_c2.clone(),
    ];
    // The member is staged before the publication it does not supersede, and the fence comes last.
    let member_before_publication = [book_state, plain_member_c1, c1, fence_record, c2, member_c2];
    for (order, log) in [
        ("publication first", publication_first),
        ("fence first", fence_first),
        ("member first", member_first),
        ("member before publication", member_before_publication),
    ] {
        assert_eq!(
            fresh_import_queues(vault, &log),
            vec!["c1"],
            "{order}: c1 owes one member, and c2 is covered by the deleter's member"
        );
    }
}

/// A live upsert of `contact` from `owner` at book generation `generation`.
fn live_upsert(
    vault: &Vault,
    owner: DeviceId,
    sequence: u64,
    contact: &str,
    generation: &str,
) -> Envelope {
    seal(
        vault,
        owner,
        sequence,
        &PrivatePayload::ContactUpserted {
            book: book(owner, generation),
            contact: contact_body(contact),
        },
    )
}

/// A live removal of `contact` from `owner`'s book at generation 1, with the book embedded.
fn live_removal(vault: &Vault, owner: DeviceId, sequence: u64, contact: &str) -> Envelope {
    let mut restored = contact_body(contact);
    restored["revision"] = json!("2");
    seal(
        vault,
        owner,
        sequence,
        &PrivatePayload::ContactRemoved {
            book_id: BOOK.into(),
            contact_id: contact.into(),
            tombstone: json!({
                "contact_id": contact,
                "revision": "2",
                "deleted_at": 5,
                "restored_contact": restored,
                "book": book(owner, "1"),
            }),
        },
    )
}

/// Rows of `contact` materialised in the live table.
fn contact_rows(path: &Path, contact: &str) -> i64 {
    count(
        path,
        &format!("SELECT COUNT(*) FROM contacts WHERE book_id='{BOOK}' AND id='{contact}'"),
    )
}

#[test]
fn a_live_upsert_after_the_fence_owes_a_member_for_the_contact_the_deleter_missed() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let owner = DeviceId::new();
    let (config, client) = receiver(&dir, &vault, "receiver");
    ingest_and_apply(&owners_book(&vault, owner, &[("c1", "1")]), &client);
    let deleter = DeviceId::new();
    ingest_and_apply(
        &[
            fence(&vault, deleter, 1, owner),
            member_superseding(&vault, deleter, 2, owner, "c1", &[(owner, 2)]),
        ],
        &client,
    );
    assert!(queued_members(&client, &vault).is_empty());

    // c2 was never held before the fence; it arrives live, covered, and the deleter never sent it.
    let report = ingest_and_apply(&[live_upsert(&vault, owner, 3, "c2", "1")], &client);

    assert_eq!(report.quarantined, 0);
    assert_eq!(queued_members(&client, &vault), vec!["c2"]);
    assert_eq!(owed(&config.database_path), 0);
    assert_eq!(
        contact_rows(&config.database_path, "c2"),
        0,
        "a covered live contact is not materialised"
    );
}

#[test]
fn a_covered_live_upsert_waits_for_a_head_that_covers_its_receipt() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let owner = DeviceId::new();
    let (config, client) = receiver(&dir, &vault, "receiver");
    ingest_and_apply(&owners_book(&vault, owner, &[("c1", "1")]), &client);
    let deleter = DeviceId::new();
    ingest_and_apply(
        &[
            fence(&vault, deleter, 1, owner),
            member_superseding(&vault, deleter, 2, owner, "c1", &[(owner, 2)]),
        ],
        &client,
    );

    // Split page: the covered upsert is received without a head that covers its receipt.
    ingest_only(&[live_upsert(&vault, owner, 3, "c2", "1")], &client);
    client.apply_pending(100).unwrap();
    assert!(queued_members(&client, &vault).is_empty());
    assert_eq!(owed(&config.database_path), 1);
    assert!(client.completion_needs_replay_head().unwrap());
    assert!(
        !client.sync_work_status().unwrap().pending_apply,
        "owed work that waits for a head is not pending apply"
    );

    let receipt = client.receive_cursor().unwrap();
    client.observe_replay_head(receipt).unwrap();
    client.apply_pending(100).unwrap();

    assert_eq!(queued_members(&client, &vault), vec!["c2"]);
    assert_eq!(owed(&config.database_path), 0);
    assert!(!client.completion_needs_replay_head().unwrap());
}

#[test]
fn a_live_tombstone_after_the_fence_owes_a_member_for_the_contact_the_deleter_missed() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let owner = DeviceId::new();
    let (config, client) = receiver(&dir, &vault, "receiver");
    ingest_and_apply(&owners_book(&vault, owner, &[("c1", "1")]), &client);
    let deleter = DeviceId::new();
    ingest_and_apply(
        &[
            fence(&vault, deleter, 1, owner),
            member_superseding(&vault, deleter, 2, owner, "c1", &[(owner, 2)]),
        ],
        &client,
    );

    ingest_and_apply(&[live_removal(&vault, owner, 3, "c2")], &client);

    assert_eq!(queued_members(&client, &vault), vec!["c2"]);
    assert_eq!(owed(&config.database_path), 0);
    assert_eq!(contact_rows(&config.database_path, "c2"), 0);
}

#[test]
fn a_known_member_with_no_later_publication_queues_nothing() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let owner = DeviceId::new();
    let (config, client) = receiver(&dir, &vault, "receiver");
    ingest_and_apply(&owners_book(&vault, owner, &[("c1", "1")]), &client);
    let deleter = DeviceId::new();
    ingest_and_apply(
        &[
            fence(&vault, deleter, 1, owner),
            member_superseding(&vault, deleter, 2, owner, "c1", &[(owner, 2)]),
            member(&vault, deleter, 3, owner, "c2"),
        ],
        &client,
    );

    assert!(queued_members(&client, &vault).is_empty());
    assert_eq!(owed(&config.database_path), 0);
}

#[test]
fn a_fence_captured_contact_whose_member_does_not_supersede_it_queues_exactly_one_member() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let owner = DeviceId::new();
    let (config, client) = receiver(&dir, &vault, "receiver");
    ingest_and_apply(&owners_book(&vault, owner, &[("c1", "1")]), &client);
    let deleter = DeviceId::new();

    // The fence captures c1's open publication (sequence 2). The deleter's member names nothing, so
    // the publication stays on the frontier and c1 still owes its own member.
    let report = ingest_and_apply(
        &[
            fence(&vault, deleter, 1, owner),
            member(&vault, deleter, 2, owner, "c1"),
        ],
        &client,
    );

    assert_eq!(report.quarantined, 0);
    assert_eq!(queued_members(&client, &vault), vec!["c1"]);
    assert_eq!(owed(&config.database_path), 0);
    let (member, _) = queued_payloads(&client, &vault)
        .into_iter()
        .find(|(_, payload)| matches!(payload, PrivatePayload::ContactBookDeletedMember { .. }))
        .unwrap();
    assert!(
        member
            .compaction
            .unwrap()
            .supersedes
            .iter()
            .any(|reference| {
                reference.producer_device_id == owner
                    && reference.producer_sequence == SourceSequence(2)
            }),
        "the new member supersedes the captured publication"
    );
}

#[test]
fn a_covered_upsert_after_a_known_member_queues_one_more_member() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let owner = DeviceId::new();
    let (config, client) = receiver(&dir, &vault, "receiver");
    ingest_and_apply(&owners_book(&vault, owner, &[("c1", "1")]), &client);
    let deleter = DeviceId::new();
    ingest_and_apply(
        &[
            fence(&vault, deleter, 1, owner),
            member_superseding(&vault, deleter, 2, owner, "c1", &[(owner, 2)]),
            member(&vault, deleter, 3, owner, "c2"),
        ],
        &client,
    );

    // The known member for c2 does not name this later publication, so it is still uncovered.
    ingest_and_apply(&[live_upsert(&vault, owner, 4, "c2", "1")], &client);

    assert_eq!(queued_members(&client, &vault), vec!["c2"]);
    assert_eq!(owed(&config.database_path), 0);
    let (member, _) = queued_payloads(&client, &vault)
        .into_iter()
        .find(|(_, payload)| matches!(payload, PrivatePayload::ContactBookDeletedMember { .. }))
        .unwrap();
    let supersedes = member.compaction.unwrap().supersedes;
    assert!(
        supersedes.iter().any(|reference| {
            reference.producer_device_id == owner
                && reference.producer_sequence == SourceSequence(4)
        }),
        "the new member supersedes the late publication"
    );
}

#[test]
fn a_member_arriving_after_a_covered_live_upsert_that_it_does_not_supersede_queues_one_member() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let owner = DeviceId::new();
    let (config, client) = receiver(&dir, &vault, "receiver");
    ingest_and_apply(&owners_book(&vault, owner, &[("c1", "1")]), &client);
    let deleter = DeviceId::new();
    ingest_and_apply(
        &[
            fence(&vault, deleter, 1, owner),
            member_superseding(&vault, deleter, 2, owner, "c1", &[(owner, 2)]),
        ],
        &client,
    );
    // Received before any head covers it, so the owed member is not emitted yet.
    ingest_only(&[live_upsert(&vault, owner, 3, "c2", "1")], &client);
    client.apply_pending(100).unwrap();
    assert_eq!(owed(&config.database_path), 1);

    // The deleter's member for c2 does not name the upsert, so the upsert stays on the frontier.
    ingest_and_apply(&[member(&vault, deleter, 4, owner, "c2")], &client);

    assert_eq!(queued_members(&client, &vault), vec!["c2"]);
    assert_eq!(owed(&config.database_path), 0);
}

#[test]
fn a_member_superseding_a_covered_live_upsert_settles_it_without_a_second_member() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let owner = DeviceId::new();
    let (config, client) = receiver(&dir, &vault, "receiver");
    ingest_and_apply(&owners_book(&vault, owner, &[("c1", "1")]), &client);
    let deleter = DeviceId::new();
    ingest_and_apply(
        &[
            fence(&vault, deleter, 1, owner),
            member_superseding(&vault, deleter, 2, owner, "c1", &[(owner, 2)]),
        ],
        &client,
    );
    ingest_only(&[live_upsert(&vault, owner, 3, "c2", "1")], &client);
    client.apply_pending(100).unwrap();
    assert_eq!(owed(&config.database_path), 1);

    // The deleter's member for c2 names the upsert as superseded, so nothing is left uncovered.
    let report = ingest_and_apply(
        &[member_superseding(
            &vault,
            deleter,
            4,
            owner,
            "c2",
            &[(owner, 3)],
        )],
        &client,
    );

    assert_eq!(report.quarantined, 0);
    assert!(queued_members(&client, &vault).is_empty());
    assert_eq!(owed(&config.database_path), 0);
}

#[test]
fn a_higher_generation_upsert_after_the_fence_is_applied_not_owed() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let owner = DeviceId::new();
    let (config, client) = receiver(&dir, &vault, "receiver");
    ingest_and_apply(&owners_book(&vault, owner, &[("c1", "1")]), &client);
    let deleter = DeviceId::new();
    ingest_and_apply(
        &[
            fence(&vault, deleter, 1, owner),
            member_superseding(&vault, deleter, 2, owner, "c1", &[(owner, 2)]),
        ],
        &client,
    );

    ingest_and_apply(&[live_upsert(&vault, owner, 3, "c2", "2")], &client);

    assert!(queued_members(&client, &vault).is_empty());
    assert_eq!(owed(&config.database_path), 0);
    assert_eq!(
        contact_rows(&config.database_path, "c2"),
        1,
        "a continuation past the fence is a live contact"
    );
}

#[test]
fn a_fence_received_over_a_socket_without_a_head_waits_for_one_replay() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let owner = DeviceId::new();
    let (_config, client) = receiver(&dir, &vault, "receiver");
    // The last HTTP replay proved the head before the fence arrived.
    ingest_and_apply(
        &owners_book(&vault, owner, &[("c1", "1"), ("c2", "1")]),
        &client,
    );

    // Socket delivery: records are ingested and applied with no replay head reported.
    ingest_only(&deleter_covering_c1_only(&vault, owner), &client);
    client.apply_pending(100).unwrap();
    assert!(queued_members(&client, &vault).is_empty());
    assert!(client.completion_needs_replay_head().unwrap());
    // The head reported before the fence is below the gate the fence's capture set.
    let need = client.completion_replay_need().unwrap().unwrap();
    assert!(need.replay_head.is_some_and(|head| head < need.gate));
    assert!(!client.sync_work_status().unwrap().pending_apply);

    // The one replay a live host requests reports the head; nothing else is needed.
    let head = client.receive_cursor().unwrap();
    client.observe_replay_head(head).unwrap();
    client.apply_pending(100).unwrap();

    assert_eq!(queued_members(&client, &vault), vec!["c2"]);
    assert!(!client.completion_needs_replay_head().unwrap());
    assert!(!client.sync_work_status().unwrap().pending_apply);
}
