//! Compaction snapshots are authoritative for compactable state: a receiver that missed a
//! tombstone the server has since purged (offline past the 120-day horizon) loses the removed
//! contact/notification, while owner state, unsent local writes and non-compactable history are
//! untouched, and nothing visible is cleared before the whole snapshot is authenticated.
use crate::common::*;
use peppy_crypto::{
    EncryptedEnvelope, KeyPurpose, decrypt, derive_purpose_key, derive_root_key, encrypt,
};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashSet};
use tempfile::TempDir;

pub(crate) const BOOK: &str = "book1";

pub(crate) fn call(result: Result<String, Error>) -> Value {
    serde_json::from_str(&result.unwrap()).unwrap()
}
pub(crate) fn book(owner: &ClientConfig, id: &str) -> Value {
    json!({
        "id": id,
        "owner_device_id": owner.device_id.to_string(),
        "generation": "1",
        "state": "active",
        "capabilities": {"read": true, "write": true, "photo": true},
        "accounts": [{"id": "native:acct", "name": "Google", "writable": true}],
        "default_account_id": "native:acct",
        "policy": {"remote_edits": "auto", "large_delete_requires_approval": true}
    })
}
pub(crate) fn contact(book_id: &str, id: &str, given: &str) -> Value {
    json!({
        "id": id,
        "book_id": book_id,
        "display_name": format!("{given} Lovelace"),
        "name": {"given": given, "family": "Lovelace"},
        "phones": [{"id": "p1", "label": "mobile", "value": "+12025550100"}],
        "emails": [],
        "addresses": [],
        "provenance": {"source_id": format!("native:src-{id}"), "account_id": "native:acct", "read_only": false}
    })
}
pub(crate) fn capture(client: &Client, book: &Value, contacts: Vec<Value>) {
    call(client.capture_contact_book(
        &json!({"schema_version": 1, "book": book, "contacts": contacts}).to_string(),
    ));
}
fn contact_upsert_envelope(
    client: &Client,
    vault: &Vault,
    contact_id: &str,
    display_name: &str,
) -> Envelope {
    let root = derive_root_key(PASSPHRASE, &vault.profile).unwrap();
    let key = derive_purpose_key(&root, &vault.profile, KeyPurpose::Event).unwrap();
    client
        .pending_outbox()
        .unwrap()
        .into_iter()
        .find(|envelope| {
            if envelope.purpose != EnvelopePurpose::Event || envelope.ciphertext.len() < 24 {
                return false;
            }
            let (nonce, ciphertext) = envelope.ciphertext.split_at(24);
            let encrypted = EncryptedEnvelope {
                nonce: nonce.try_into().unwrap(),
                ciphertext: ciphertext.to_vec(),
            };
            let Ok(plain) = decrypt(&key, &envelope.aad_bytes().unwrap(), &encrypted) else {
                return false;
            };
            matches!(
                serde_json::from_slice::<PrivatePayload>(&plain),
                Ok(PrivatePayload::ContactUpserted { contact, .. })
                    if contact["id"] == contact_id && contact["display_name"] == display_name
            )
        })
        .expect("authenticated contact upsert must remain pending for upload")
}
/// Visible (non-tombstoned) contact ids of `book_id`; empty when the book is unknown.
pub(crate) fn visible(client: &Client, book_id: &str) -> Vec<String> {
    let Ok(raw) = client.contact_book_view(
        &json!({"schema_version": 1, "book_id": book_id, "limit": 200}).to_string(),
    ) else {
        return Vec::new();
    };
    let mut ids: Vec<String> = serde_json::from_str::<Value>(&raw).unwrap()["contacts"]
        .as_array()
        .map(|list| {
            list.iter()
                .map(|c| c["id"].as_str().unwrap().to_owned())
                .collect()
        })
        .unwrap_or_default();
    ids.sort();
    ids
}
/// The owner removes `id` with an authoritative full scan observing only `keep`.
pub(crate) fn tombstone(owner: &Client, book_id: &str, scan: &str, keep: &[&str]) {
    call(owner.begin_contact_scan(
        &json!({"schema_version": 1, "scan_id": scan, "book_id": book_id, "generation": "1", "access": "full", "authoritative": true}).to_string(),
    ));
    for id in keep {
        owner
            .observe_contact_scan(
                &json!({"schema_version": 1, "scan_id": scan, "contact": contact(book_id, id, "Kept")}).to_string(),
            )
            .unwrap();
    }
    let done = call(owner.finish_contact_scan(
        &json!({"schema_version": 1, "scan_id": scan, "complete": true}).to_string(),
    ));
    assert_eq!(done["deletions_applied"], true, "{done}");
}

/// Server compaction model: a record named in any retained record's `supersedes` is dropped,
/// and (past the purge horizon) terminal tombstones are dropped too. Cursors are preserved.
pub(crate) fn compact(log: &[Envelope], purge_terminal: bool) -> Vec<SnapshotRecord> {
    let superseded: HashSet<(DeviceId, u64)> = log
        .iter()
        .filter_map(|e| e.compaction.as_ref())
        .flat_map(|c| c.supersedes.iter())
        .map(|r| (r.producer_device_id, r.producer_sequence.0))
        .collect();
    log.iter()
        .enumerate()
        .filter(|(_, e)| !superseded.contains(&(e.producer_device_id, e.producer_sequence.0)))
        .filter(|(_, e)| !(purge_terminal && e.compaction.as_ref().is_some_and(|c| c.terminal)))
        .map(|(i, e)| SnapshotRecord {
            cursor: Cursor(i as u64 + 1),
            envelope: e.clone(),
        })
        .collect()
}
fn import(client: &Client, records: &[SnapshotRecord], high_water: usize, generation: Option<u64>) {
    let session = client
        .begin_snapshot_with_compaction(
            Cursor(high_water as u64),
            records.len() as u64,
            SnapshotPurpose::Resync,
            generation,
        )
        .unwrap();
    for page in records.chunks(2) {
        client
            .append_snapshot_page(session.generation, page)
            .unwrap();
    }
    client.finish_snapshot(session.generation).unwrap();
}
/// Calls `apply_pending` like native hosts do until no step reports progress.
pub(crate) fn settle(client: &Client) -> ApplyReport {
    let mut total = ApplyReport::default();
    loop {
        let step = client.apply_pending(1000).unwrap();
        total.applied += step.applied;
        total.quarantined += step.quarantined;
        total.drained += step.drained;
        total.superseded += step.superseded;
        if step.applied + step.drained + step.quarantined + step.superseded == 0
            && step.snapshot_remaining == 0
        {
            return total;
        }
    }
}
pub(crate) fn state(client: &Client) -> Option<(SnapshotProjectionState, Option<String>)> {
    client
        .snapshot_projection_status()
        .unwrap()
        .map(|s| (s.state, s.reason))
}
fn wait_for_failed_snapshot(client: &Client, record_count: usize) {
    let max_steps = record_count.saturating_mul(2).saturating_add(2);
    for _ in 0..max_steps {
        if matches!(state(client), Some((SnapshotProjectionState::Failed, _))) {
            return;
        }
        client.apply_pending(1).unwrap();
    }
    panic!(
        "snapshot did not fail after {max_steps} one-row drain/stage steps; final phase={:?}",
        state(client)
    );
}

struct Pair {
    _dir: TempDir,
    vault: Vault,
    owner_cfg: ClientConfig,
    peer_cfg: ClientConfig,
    owner: Client,
    peer: Client,
    server: Server,
}
fn pair() -> Pair {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let owner_cfg = config(&dir, "owner", &vault);
    let peer_cfg = config(&dir, "peer", &vault);
    let owner = unlocked(&owner_cfg, &vault);
    let peer = unlocked(&peer_cfg, &vault);
    for client in [&owner, &peer] {
        client.set_server_compaction_supported(true).unwrap();
    }
    Pair {
        _dir: dir,
        vault,
        owner_cfg,
        peer_cfg,
        owner,
        peer,
        server: Server::default(),
    }
}

#[test]
fn offline_receiver_loses_purged_contact_only_after_complete_compaction_snapshot() {
    let mut p = pair();
    let b = book(&p.owner_cfg, BOOK);
    capture(
        &p.owner,
        &b,
        vec![contact(BOOK, "c1", "Ada"), contact(BOOK, "c2", "Bea")],
    );
    // A separate receiver whose process we can crash and reopen.
    let offline_cfg = config(&p._dir, "offline", &p.vault);
    let offline = unlocked(&offline_cfg, &p.vault);
    p.server.upload(&p.owner);
    p.server.sync(&offline);
    assert_eq!(visible(&offline, BOOK), ["c1", "c2"]);

    // The receiver goes offline; the owner removes c1 and the server later purges the
    // tombstone together with the upsert it superseded.
    tombstone(&p.owner, BOOK, "scan1", &["c2"]);
    p.server.upload(&p.owner);
    let high_water = p.server.log.len();
    let retained = compact(&p.server.log, true);
    assert!(retained.len() < high_water, "compaction must drop records");
    assert!(retained.iter().all(|r| r.envelope.compaction.is_some()));

    // A fresh database needs no repair; a reported one latches until promotion.
    assert!(!offline.contact_repair_required().unwrap());
    offline.request_contact_repair().unwrap();
    assert!(offline.contact_repair_required().unwrap());

    // Partial progress never clears anything.
    import(&offline, &retained, high_water, Some(7));
    let first = offline.apply_pending(1).unwrap();
    assert!(first.drained > 0 || first.snapshot_remaining > 0);
    assert_eq!(
        visible(&offline, BOOK),
        ["c1", "c2"],
        "partial import must not clear"
    );
    assert!(matches!(
        state(&offline),
        Some((
            SnapshotProjectionState::Draining | SnapshotProjectionState::Staging,
            None
        ))
    ));

    // Crash and reopen mid-import: the durable stage resumes.
    drop(offline);
    let offline = unlocked(&offline_cfg, &p.vault);
    assert_eq!(visible(&offline, BOOK), ["c1", "c2"]);
    settle(&offline);
    assert_eq!(
        state(&offline),
        Some((SnapshotProjectionState::Promoted, None))
    );
    assert_eq!(
        visible(&offline, BOOK),
        ["c2"],
        "purged contact must disappear"
    );
    assert!(
        !offline.contact_repair_required().unwrap(),
        "promotion clears the repair latch"
    );
    assert_eq!(offline.quarantined().unwrap(), vec![]);

    // The owner importing the same snapshot keeps its own state and can still rescan.
    import(&p.owner, &retained, high_water, Some(7));
    settle(&p.owner);
    assert_eq!(
        state(&p.owner),
        Some((SnapshotProjectionState::Promoted, None))
    );
    assert_eq!(visible(&p.owner, BOOK), ["c2"]);
    capture(
        &p.owner,
        &b,
        vec![contact(BOOK, "c2", "Kept"), contact(BOOK, "c3", "Cy")],
    );
    assert_eq!(visible(&p.owner, BOOK), ["c2", "c3"]);

    // Live sync continues on top of the promoted snapshot.
    p.server.upload(&p.owner);
    p.server.sync(&offline);
    assert_eq!(visible(&offline, BOOK), ["c2", "c3"]);
}

#[test]
fn retained_empty_archive_does_not_block_contact_projection() {
    let mut p = pair();
    let b = book(&p.owner_cfg, BOOK);
    capture(&p.owner, &b, vec![contact(BOOK, "c1", "Ada")]);
    p.server.upload(&p.owner);
    let mut envelope = p
        .server
        .log
        .iter()
        .find(|e| e.compaction.is_some())
        .unwrap()
        .clone();
    envelope.envelope_id = EnvelopeId::new();
    envelope.producer_sequence = SourceSequence(
        p.server
            .log
            .iter()
            .map(|e| e.producer_sequence.0)
            .max()
            .unwrap()
            + 1,
    );
    let metadata = envelope.compaction.as_mut().unwrap();
    metadata.key = vec![123; 32];
    metadata.supersedes.clear();
    metadata.terminal = false;
    let conversation = ConversationId::new();
    let raw = json!({
        "kind": "conversation_folder",
        "conversation_id": conversation.to_string(),
        "folder": "archived",
        "logical_revision": 1,
        "writer_device_id": envelope.producer_device_id.to_string(),
        "compaction": envelope.compaction,
    });
    let root = derive_root_key(PASSPHRASE, &p.vault.profile).unwrap();
    let key = derive_purpose_key(&root, &p.vault.profile, KeyPurpose::Event).unwrap();
    let sealed = encrypt(
        &key,
        &envelope.aad_bytes().unwrap(),
        &serde_json::to_vec(&raw).unwrap(),
    )
    .unwrap();
    envelope.ciphertext = sealed.nonce.to_vec();
    envelope.ciphertext.extend_from_slice(&sealed.ciphertext);
    p.server.log.push(envelope);
    p.peer
        .capture_incoming(IncomingSms {
            conversation_id: Some(conversation),
            sender_address: ADDRESS.into(),
            body: "message".into(),
            provider_message_id: Some("snapshot-folder-message".into()),
            imported: false,
        })
        .unwrap();
    let retained = compact(&p.server.log, false);
    import(&p.peer, &retained, p.server.log.len(), Some(1));
    settle(&p.peer);
    assert_eq!(
        state(&p.peer),
        Some((SnapshotProjectionState::Promoted, None))
    );
    assert_eq!(visible(&p.peer, BOOK), ["c1"]);
    let conn = rusqlite::Connection::open(&p.peer_cfg.database_path).unwrap();
    let key = DB_KEY
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    conn.execute_batch(&format!(
        "PRAGMA key = \"x'{key}'\"; PRAGMA cipher_compatibility = 4;"
    ))
    .unwrap();
    let folder: (String, Option<i64>) = conn
        .query_row(
            "SELECT folder,archived_through_ms FROM conversation_folders WHERE conversation_id=?",
            [conversation.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(folder, ("archived".into(), None));
    let conversations = p.peer.list_conversations().unwrap();
    assert_eq!(
        conversations
            .iter()
            .find(|item| item.conversation_id == conversation)
            .unwrap()
            .folder,
        ConversationFolder::Inbox
    );
}

#[test]
fn legacy_snapshot_merges_and_keeps_removed_contact() {
    let mut p = pair();
    capture(
        &p.owner,
        &book(&p.owner_cfg, BOOK),
        vec![contact(BOOK, "c1", "Ada"), contact(BOOK, "c2", "Bea")],
    );
    p.server.upload(&p.owner);
    p.server.sync(&p.peer);
    tombstone(&p.owner, BOOK, "scan1", &["c2"]);
    p.server.upload(&p.owner);
    let high_water = p.server.log.len();
    import(&p.peer, &compact(&p.server.log, true), high_water, None);
    settle(&p.peer);
    assert_eq!(state(&p.peer), None);
    assert_eq!(
        visible(&p.peer, BOOK),
        ["c1", "c2"],
        "legacy merge cannot remove"
    );
}

#[test]
fn pending_superseded_upsert_cannot_resurrect_and_journaled_records_reconstruct() {
    let mut p = pair();
    capture(
        &p.owner,
        &book(&p.owner_cfg, BOOK),
        vec![contact(BOOK, "c1", "Ada"), contact(BOOK, "c2", "Bea")],
    );
    p.server.upload(&p.owner);
    // A locked receiver journals the upserts but cannot apply them.
    let locked_cfg = config(&p._dir, "locked", &p.vault);
    let locked = open(&locked_cfg);
    p.server.sync(&locked);
    assert!(visible(&locked, BOOK).is_empty());

    tombstone(&p.owner, BOOK, "scan1", &["c2"]);
    p.server.upload(&p.owner);
    let high_water = p.server.log.len();
    import(&locked, &compact(&p.server.log, true), high_water, Some(3));
    // Locked: the drain completes, staging waits for keys without reporting busy work.
    let report = locked.apply_pending(1000).unwrap();
    assert_eq!((report.applied, report.snapshot_remaining), (0, 0));
    assert_eq!(
        state(&locked),
        Some((SnapshotProjectionState::Staging, None))
    );
    drop(locked);

    let unlocked_client = unlocked(&locked_cfg, &p.vault);
    let total = settle(&unlocked_client);
    assert_eq!(
        state(&unlocked_client),
        Some((SnapshotProjectionState::Promoted, None))
    );
    assert!(
        total.superseded > 0,
        "the pending c1 upsert must be suppressed"
    );
    assert_eq!(visible(&unlocked_client, BOOK), ["c2"]);
    // Re-applying never revives it either.
    settle_one_by_one(&unlocked_client);
    assert_eq!(visible(&unlocked_client, BOOK), ["c2"]);
}

fn assert_omitted_newer_contact_survives(limit: usize) {
    let mut p = pair();
    let b = book(&p.owner_cfg, BOOK);
    capture(&p.owner, &b, vec![contact(BOOK, "c1", "Ada")]);
    p.server.upload(&p.owner);
    let retained = full(&p.server.log);

    // The receiver journals both authenticated revisions while locked. A snapshot that only
    // retains the older revision must not let high-water suppression discard the newer one.
    let locked_cfg = config(&p._dir, "locked-newer", &p.vault);
    let locked = open(&locked_cfg);
    p.server.sync(&locked);
    capture(&p.owner, &b, vec![contact(BOOK, "c1", "Bea")]);
    p.server.upload(&p.owner);
    p.server.sync(&locked);
    let high_water = p.server.log.len();
    import(&locked, &retained, high_water, Some(38));
    drop(locked);

    let unlocked_client = unlocked(&locked_cfg, &p.vault);
    loop {
        let step = unlocked_client.apply_pending(limit).unwrap();
        if step.applied + step.drained + step.quarantined + step.superseded == 0
            && step.snapshot_remaining == 0
        {
            break;
        }
    }
    let view = call(unlocked_client.contact_book_view(
        &json!({"schema_version": 1, "book_id": BOOK, "limit": 200}).to_string(),
    ));
    assert_eq!(view["contacts"][0]["name"]["given"], "Bea");
}

#[test]
fn authenticated_newer_pending_contact_survives_snapshot_omission() {
    for limit in [1, 2, 1_000] {
        assert_omitted_newer_contact_survives(limit);
    }
}

#[test]
fn large_retained_projection_defers_a_newer_pending_contact_until_promotion() {
    let mut p = pair();
    let b = book(&p.owner_cfg, BOOK);
    let initial: Vec<_> = (0..1_001)
        .map(|index| contact(BOOK, &format!("c{index:04}"), "Ada"))
        .collect();
    capture(&p.owner, &b, initial.clone());
    p.server.upload(&p.owner);
    let retained = full(&p.server.log);
    p.server.sync(&p.peer);
    settle(&p.peer);

    // The predecessor is already live rather than pending, while the retained snapshot needs
    // more than one stage batch. This is the batching gap that must defer the update.
    let peer_cfg = p.peer_cfg.clone();
    drop(p.peer);
    let locked = open(&peer_cfg);
    let mut updated = initial;
    updated[1_000] = contact(BOOK, "c1000", "Bea");
    capture(&p.owner, &b, updated);
    p.server.upload(&p.owner);
    p.server.sync(&locked);
    import(&locked, &retained, p.server.log.len(), Some(39));
    drop(locked);

    let peer = unlocked(&peer_cfg, &p.vault);
    loop {
        let step = peer.apply_pending(2).unwrap();
        if step.applied + step.drained + step.quarantined + step.superseded == 0
            && step.snapshot_remaining == 0
        {
            break;
        }
    }
    let mut offset = 0;
    let updated_contact = loop {
        let page = call(
            peer.contact_book_view(
                &json!({"schema_version": 1, "book_id": BOOK, "offset": offset, "limit": 200})
                    .to_string(),
            ),
        );
        let contacts = page["contacts"].as_array().unwrap();
        if let Some(contact) = contacts.iter().find(|contact| contact["id"] == "c1000") {
            break contact.clone();
        }
        assert!(
            !contacts.is_empty(),
            "c1000 was absent from the paginated view"
        );
        offset += contacts.len();
    };
    assert_eq!(updated_contact["name"]["given"], "Bea");
}

#[test]
fn pending_contacts_after_projection_gc_compare_against_durable_state() {
    let mut p = pair();
    let b = book(&p.owner_cfg, BOOK);
    let initial = vec![
        contact(BOOK, "c1", "Ada"),
        contact(BOOK, "c2", "Bea"),
        contact(BOOK, "c3", "Cy"),
    ];
    capture(&p.owner, &b, initial.clone());
    p.server.upload(&p.owner);
    let retained = full(&p.server.log);
    let locked_cfg = config(&p._dir, "locked-gc", &p.vault);
    let locked = open(&locked_cfg);
    p.server.sync(&locked);
    capture(
        &p.owner,
        &b,
        vec![
            contact(BOOK, "c1", "Anna"),
            contact(BOOK, "c2", "Bryn"),
            contact(BOOK, "c3", "Cyd"),
        ],
    );
    p.server.upload(&p.owner);
    p.server.sync(&locked);
    import(&locked, &retained, p.server.log.len(), Some(40));
    drop(locked);

    let peer = unlocked(&locked_cfg, &p.vault);
    // One record per call forces stage GC before the second pending update can be decided.
    settle_one_by_one(&peer);
    let view = call(peer.contact_book_view(
        &json!({"schema_version": 1, "book_id": BOOK, "limit": 200}).to_string(),
    ));
    let names: Vec<_> = view["contacts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|contact| contact["name"]["given"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["Anna", "Bryn", "Cyd"]);
}

#[test]
fn later_epoch_unlock_applies_omitted_newer_contact_after_projection_gc() {
    let mut p = pair();
    let b = book(&p.owner_cfg, BOOK);
    capture(&p.owner, &b, vec![contact(BOOK, "c1", "Ada")]);
    p.server.upload(&p.owner);
    let retained = full(&p.server.log);
    let locked_cfg = config(&p._dir, "locked-epoch", &p.vault);
    let locked = open(&locked_cfg);
    p.server.sync(&locked);

    let next_epoch = Vault::epoch(p.vault.id, 2);
    p.owner
        .unlock(&next_epoch.profile, &next_epoch.header, PASSPHRASE)
        .unwrap();
    p.owner.activate_epoch(2).unwrap();
    capture(&p.owner, &b, vec![contact(BOOK, "c1", "Bea")]);
    p.server.upload(&p.owner);
    p.server.sync(&locked);
    import(&locked, &retained, p.server.log.len(), Some(41));
    drop(locked);

    let peer = unlocked(&locked_cfg, &p.vault);
    settle_one_by_one(&peer);
    assert_eq!(visible(&peer, BOOK), ["c1"]);
    peer.unlock(&next_epoch.profile, &next_epoch.header, PASSPHRASE)
        .unwrap();
    settle_one_by_one(&peer);
    let view = call(peer.contact_book_view(
        &json!({"schema_version": 1, "book_id": BOOK, "limit": 200}).to_string(),
    ));
    assert_eq!(view["contacts"][0]["name"]["given"], "Bea");
}

#[test]
fn unauthenticated_retained_record_fails_projection_and_keeps_view() {
    let mut p = pair();
    let b = book(&p.owner_cfg, BOOK);
    capture(
        &p.owner,
        &b,
        vec![contact(BOOK, "c1", "Ada"), contact(BOOK, "c2", "Bea")],
    );
    p.server.upload(&p.owner);
    p.server.sync(&p.peer);
    tombstone(&p.owner, BOOK, "scan1", &["c2"]);
    capture(
        &p.owner,
        &b,
        vec![contact(BOOK, "c2", "Kept"), contact(BOOK, "c3", "Cy")],
    );
    p.server.upload(&p.owner);
    let high_water = p.server.log.len();
    let mut retained = compact(&p.server.log, true);
    // Corrupt one retained record the peer has not journaled yet.
    let last = retained.last_mut().unwrap();
    let byte = last.envelope.ciphertext.last_mut().unwrap();
    *byte ^= 0x55;

    p.peer.request_contact_repair().unwrap();
    import(&p.peer, &retained, high_water, Some(9));
    settle(&p.peer);
    assert!(
        p.peer.contact_repair_required().unwrap(),
        "a failed projection keeps the repair latch"
    );
    assert_eq!(
        state(&p.peer),
        Some((
            SnapshotProjectionState::Failed,
            Some("authentication_failed".into())
        ))
    );
    // Previous visible state is kept; the snapshot never became authoritative.
    assert!(visible(&p.peer, BOOK).contains(&"c1".to_owned()));

    // A later valid snapshot still promotes.
    let retained = compact(&p.server.log, true);
    let fresh_cfg = config(&p._dir, "fresh", &p.vault);
    let fresh = unlocked(&fresh_cfg, &p.vault);
    import(&fresh, &retained, high_water, Some(10));
    settle(&fresh);
    assert_eq!(
        state(&fresh),
        Some((SnapshotProjectionState::Promoted, None))
    );
    assert_eq!(visible(&fresh, BOOK), ["c2", "c3"]);
}

#[test]
fn zero_record_compaction_snapshot_clears_remote_state_only() {
    let mut p = pair();
    capture(
        &p.owner,
        &book(&p.owner_cfg, BOOK),
        vec![contact(BOOK, "c1", "Ada")],
    );
    capture(
        &p.peer,
        &book(&p.peer_cfg, "own"),
        vec![contact("own", "m1", "Me")],
    );
    p.server.upload(&p.owner);
    p.server.upload(&p.peer);
    p.server.sync(&p.peer);
    assert_eq!(visible(&p.peer, BOOK), ["c1"]);
    let high_water = p.server.log.len();
    import(&p.peer, &[], high_water, Some(4));
    settle(&p.peer);
    assert_eq!(
        state(&p.peer),
        Some((SnapshotProjectionState::Promoted, None))
    );
    assert!(visible(&p.peer, BOOK).is_empty());
    assert_eq!(visible(&p.peer, "own"), ["m1"], "owned book is local truth");
}

#[test]
fn historical_snapshot_edit_request_never_yields_a_permit() {
    let mut p = pair();
    capture(
        &p.owner,
        &book(&p.owner_cfg, BOOK),
        vec![contact(BOOK, "c1", "Ada")],
    );
    p.server.upload(&p.owner);
    p.server.sync(&p.peer);
    let request = json!({
        "schema_version": 1,
        "request_id": "r1",
        "target_owner": p.owner_cfg.device_id.to_string(),
        "book_id": BOOK,
        "kind": "update",
        "contact_id": "c1",
        "base_revision": "1",
        "expires_at": std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
            + 86_400,
        "patches": [{"op": "replace", "path": "name.given", "value": "Augusta"}]
    });
    call(p.peer.request_contact_edit(&request.to_string()));
    p.server.upload(&p.peer);
    // The owner was offline and learns the request only from the compaction snapshot.
    let high_water = p.server.log.len();
    import(&p.owner, &compact(&p.server.log, true), high_water, Some(5));
    settle(&p.owner);
    assert_eq!(
        state(&p.owner),
        Some((SnapshotProjectionState::Promoted, None))
    );
    let permit = p
        .owner
        .next_contact_apply_permit(&json!({"schema_version": 1, "request_id": "r1"}).to_string());
    if let Ok(permit) = permit {
        assert_ne!(
            serde_json::from_str::<Value>(&permit).unwrap()["status"],
            "permit",
            "historical request must not authorize an OS write"
        );
    }
    assert_eq!(visible(&p.owner, BOOK), ["c1"]);
}

fn notification(key: &str, package: &str, text: &str) -> NotificationCapture {
    NotificationCapture {
        notification_key: key.into(),
        instance: format!("{key}-1"),
        package_name: package.into(),
        app_name: "App".into(),
        app_icon_png_base64: None,
        title: "Title".into(),
        text: text.into(),
        category: None,
        posted_at: 42,
        dismissible: true,
    }
}

const ICON_32PX: &str = "iVBORw0KGgoAAAANSUhEUgAAACAAAAAg";
fn shown(client: &Client) -> Vec<String> {
    let mut keys: Vec<String> = client
        .notification_snapshot()
        .unwrap()
        .notifications
        .into_iter()
        .map(|n| n.target.notification_key)
        .collect();
    keys.sort();
    keys
}
fn muted(client: &Client, package: &str) -> Option<bool> {
    client
        .notification_snapshot()
        .unwrap()
        .app_filters
        .into_iter()
        .find(|f| f.package_name == package)
        .map(|f| f.muted)
}

#[test]
fn purged_notification_disappears_and_filters_keep_lww_and_unsent_writes() {
    let mut p = pair();
    let (phone, desktop) = (&p.owner, &p.peer);
    let phone_id = p.owner_cfg.device_id.to_string();
    for (key, text) in [("n1", "first"), ("n2", "second")] {
        phone
            .capture_notification(notification(key, "com.example.mail", text))
            .unwrap();
    }
    phone
        .capture_notification(notification("n3", "com.example.news", "headline"))
        .unwrap();
    p.server.upload(phone);
    p.server.sync(desktop);
    assert_eq!(shown(desktop), ["n1", "n2", "n3"]);

    // Desktop offline: phone removes n1 and mutes news; the desktop mutes chat locally but
    // has not uploaded that write yet.
    phone.remove_notification("n1", "n1-1").unwrap();
    phone
        .set_app_muted(&phone_id, "com.example.news", "News", true)
        .unwrap();
    p.server.upload(phone);
    desktop
        .set_app_muted(&phone_id, "com.example.chat", "Chat", true)
        .unwrap();

    let high_water = p.server.log.len();
    let retained = compact(&p.server.log, true);
    import(desktop, &retained, high_water, Some(11));
    settle(desktop);
    assert_eq!(
        state(desktop),
        Some((SnapshotProjectionState::Promoted, None))
    );
    assert_eq!(shown(desktop), ["n2"], "purged n1 gone, muted news hidden");
    assert_eq!(muted(desktop, "com.example.news"), Some(true));
    assert_eq!(
        muted(desktop, "com.example.chat"),
        Some(true),
        "unsent local filter write is preserved"
    );
    // The phone's own notifications are local truth and untouched by its own import.
    import(phone, &retained, high_water, Some(11));
    settle(phone);
    assert_eq!(shown(phone), ["n2"]);

    // The unsent filter still reaches the server and the phone afterwards.
    p.server.upload(desktop);
    p.server.sync(phone);
    assert_eq!(muted(phone, "com.example.chat"), Some(true));
}

#[test]
fn snapshot_promotion_retains_icons_and_newer_absent_icons_overwrite_them() {
    let mut p = pair();
    let mut posted = notification("icon", "com.example.mail", "with icon");
    posted.app_icon_png_base64 = Some(ICON_32PX.into());
    p.owner.capture_notification(posted).unwrap();
    p.server.upload(&p.owner);

    let high_water = p.server.log.len();
    import(
        &p.peer,
        &p.server
            .log
            .iter()
            .enumerate()
            .map(|(index, envelope)| SnapshotRecord {
                cursor: Cursor(index as u64 + 1),
                envelope: envelope.clone(),
            })
            .collect::<Vec<_>>(),
        high_water,
        Some(20),
    );
    settle(&p.peer);
    assert_eq!(
        p.peer.notification_snapshot().unwrap().notifications[0].app_icon_png_base64,
        Some(ICON_32PX.into())
    );

    p.owner
        .capture_notification(notification("icon", "com.example.mail", "without icon"))
        .unwrap();
    p.server.upload(&p.owner);
    let high_water = p.server.log.len();
    import(
        &p.peer,
        &p.server
            .log
            .iter()
            .enumerate()
            .map(|(index, envelope)| SnapshotRecord {
                cursor: Cursor(index as u64 + 1),
                envelope: envelope.clone(),
            })
            .collect::<Vec<_>>(),
        high_water,
        Some(21),
    );
    settle(&p.peer);
    let updated = p
        .peer
        .notification_snapshot()
        .unwrap()
        .notifications
        .remove(0);
    assert_eq!(updated.app_icon_png_base64, None);
    assert_eq!(updated.text, "without icon");
}

fn books(client: &Client) -> Value {
    serde_json::from_str(&client.list_contact_books_json().unwrap()).unwrap()
}

#[test]
fn snapshot_out_of_order_upsert_cannot_rewind_retired_book() {
    let p = pair();
    let b = book(&p.owner_cfg, BOOK);
    capture(&p.owner, &b, vec![contact(BOOK, "c1", "Ada")]);
    let mut retired = b.clone();
    retired["state"] = json!("retired");
    capture(&p.owner, &retired, vec![]);
    // The photo-held upsert lands after the retirement.
    let mut pending = p.owner.pending_outbox().unwrap();
    pending.sort_by_key(|e| std::cmp::Reverse(e.producer_sequence.0));
    let records = pending
        .iter()
        .enumerate()
        .map(|(i, e)| SnapshotRecord {
            cursor: Cursor(i as u64 + 1),
            envelope: e.clone(),
        })
        .collect::<Vec<_>>();
    import(&p.peer, &records, records.len(), Some(3));
    settle(&p.peer);
    assert_eq!(
        state(&p.peer),
        Some((SnapshotProjectionState::Promoted, None))
    );
    assert_eq!(books(&p.peer)["books"][0]["state"], "retired");
    assert_eq!(visible(&p.peer, BOOK), ["c1"]);
}

#[test]
fn snapshot_tombstone_before_book_state_keeps_restore_data() {
    let p = pair();
    let b = book(&p.owner_cfg, BOOK);
    capture(
        &p.owner,
        &b,
        vec![contact(BOOK, "c1", "Ada"), contact(BOOK, "c2", "Bea")],
    );
    for e in p.owner.pending_outbox().unwrap() {
        p.owner.ack_outbox(e.envelope_id).unwrap();
    }
    tombstone(&p.owner, BOOK, "scan1", &["c2"]);
    // Compacted: c2's upsert and the book-bearing records are gone except the removal and
    // the latest book state, which follows it.
    let records = p
        .owner
        .pending_outbox()
        .unwrap()
        .iter()
        .enumerate()
        .map(|(i, e)| SnapshotRecord {
            cursor: Cursor(i as u64 + 1),
            envelope: e.clone(),
        })
        .collect::<Vec<_>>();
    import(&p.peer, &records, records.len(), Some(3));
    settle(&p.peer);
    assert_eq!(
        state(&p.peer),
        Some((SnapshotProjectionState::Promoted, None))
    );
    let restorable = call(
        p.peer
            .list_restorable_contacts_json(&json!({"book_id": BOOK}).to_string()),
    );
    assert_eq!(restorable["contacts"][0]["id"], "c1");
}

#[test]
fn retained_tombstone_waits_for_authentication_without_blocking_recovery_or_live_replay() {
    let mut p = pair();
    let b = book(&p.owner_cfg, BOOK);
    capture(
        &p.owner,
        &b,
        vec![contact(BOOK, "c1", "Ada"), contact(BOOK, "c2", "Bea")],
    );
    p.server.upload(&p.owner);
    p.server.sync(&p.peer);
    assert_eq!(visible(&p.peer, BOOK), ["c1", "c2"]);
    let original_book = call(p.peer.contact_book_view(
        &json!({"schema_version": 1, "book_id": BOOK, "limit": 200}).to_string(),
    ));

    tombstone(&p.owner, BOOK, "remove-c2", &["c1"]);
    capture(&p.owner, &b, vec![contact(BOOK, "c1", "Intermediate")]);
    let corrupt_envelope =
        contact_upsert_envelope(&p.owner, &p.vault, "c1", "Intermediate Lovelace");
    capture(&p.owner, &b, vec![contact(BOOK, "c1", "Final")]);
    let replacement_envelope = contact_upsert_envelope(&p.owner, &p.vault, "c1", "Final Lovelace");
    assert_eq!(
        corrupt_envelope.producer_device_id,
        replacement_envelope.producer_device_id
    );
    assert!(
        replacement_envelope
            .compaction
            .as_ref()
            .unwrap()
            .supersedes
            .iter()
            .any(|superseded| {
                superseded.producer_device_id == corrupt_envelope.producer_device_id
                    && superseded.producer_sequence == corrupt_envelope.producer_sequence
            }),
        "the later authenticated c1 upsert must explicitly supersede the corrupt candidate"
    );
    p.server.upload(&p.owner);
    let high_water = p.server.log.len();
    let mut corrupt_snapshot = full(&p.server.log);
    let corrupt_cursor = corrupt_snapshot
        .iter()
        .position(|record| record.envelope.envelope_id == corrupt_envelope.envelope_id)
        .expect("authenticated intermediate c1 upsert must be present in full snapshot");
    corrupt_snapshot[corrupt_cursor].envelope.ciphertext[0] ^= 0x55;

    let session = p
        .peer
        .begin_snapshot_with_compaction(
            Cursor(high_water as u64),
            corrupt_snapshot.len() as u64,
            SnapshotPurpose::Resync,
            Some(38),
        )
        .unwrap();
    for record in &corrupt_snapshot {
        p.peer
            .append_snapshot_page(session.generation, std::slice::from_ref(record))
            .unwrap();
    }
    p.peer.finish_snapshot(session.generation).unwrap();
    wait_for_failed_snapshot(&p.peer, corrupt_snapshot.len());
    assert!(matches!(
        state(&p.peer),
        Some((SnapshotProjectionState::Failed, Some(_)))
    ));
    for _ in 0..3 {
        p.peer.apply_pending(1).unwrap();
        let view = call(p.peer.contact_book_view(
            &json!({"schema_version": 1, "book_id": BOOK, "limit": 200}).to_string(),
        ));
        assert_eq!(view, original_book);
    }

    let peer_cfg = p.peer_cfg.clone();
    drop(p.peer);
    let peer = unlocked(&peer_cfg, &p.vault);
    assert_eq!(
        call(peer.contact_book_view(
            &json!({"schema_version": 1, "book_id": BOOK, "limit": 200}).to_string(),
        )),
        original_book
    );
    for _ in 0..3 {
        peer.apply_pending(1).unwrap();
        assert_eq!(
            call(peer.contact_book_view(
                &json!({"schema_version": 1, "book_id": BOOK, "limit": 200}).to_string(),
            )),
            original_book
        );
    }

    let replacement = compact(&p.server.log, false);
    assert!(
        replacement
            .iter()
            .all(|record| record.cursor.0 != (corrupt_cursor + 1) as u64)
    );
    assert!(
        replacement
            .iter()
            .any(|record| { record.envelope.envelope_id == replacement_envelope.envelope_id })
    );
    import(&peer, &replacement, high_water, Some(39));
    settle_one_by_one(&peer);
    assert_eq!(
        state(&peer),
        Some((SnapshotProjectionState::Promoted, None))
    );
    assert_eq!(visible(&peer, BOOK), ["c1"]);
    assert_eq!(
        peer.replay_progress().unwrap().processed_through,
        Cursor(high_water as u64)
    );
    let promoted_book = call(peer.contact_book_view(
        &json!({"schema_version": 1, "book_id": BOOK, "limit": 200}).to_string(),
    ));
    drop(peer);
    let peer = unlocked(&peer_cfg, &p.vault);
    assert_eq!(
        state(&peer),
        Some((SnapshotProjectionState::Promoted, None))
    );
    assert_eq!(
        call(peer.contact_book_view(
            &json!({"schema_version": 1, "book_id": BOOK, "limit": 200}).to_string(),
        )),
        promoted_book
    );

    capture(
        &p.owner,
        &b,
        vec![contact(BOOK, "c1", "Ada"), contact(BOOK, "c3", "Cy")],
    );
    p.server.upload(&p.owner);
    p.server.sync(&peer);
    assert_eq!(visible(&peer, BOOK), ["c1", "c3"]);
}

/// Every log record as an uncompacted snapshot (the server has not compacted them yet).
fn full(log: &[Envelope]) -> Vec<SnapshotRecord> {
    log.iter()
        .enumerate()
        .map(|(index, envelope)| SnapshotRecord {
            cursor: Cursor(index as u64 + 1),
            envelope: envelope.clone(),
        })
        .collect()
}

/// A phone without server compaction support posts a mail notification (n1) and mutes news
/// (purging its unsent n2 post): every record it uploads is header-free. Returns the phone id.
fn legacy_posts_and_mute(p: &mut Pair) -> String {
    let phone_id = p.owner_cfg.device_id.to_string();
    p.owner.set_server_compaction_supported(false).unwrap();
    p.owner
        .capture_notification(notification("n1", "com.example.mail", "first"))
        .unwrap();
    p.owner
        .capture_notification(notification("n2", "com.example.news", "headline"))
        .unwrap();
    p.owner
        .set_app_muted(&phone_id, "com.example.news", "News", true)
        .unwrap();
    p.server.upload(&p.owner);
    assert!(p.server.log.iter().all(|e| e.compaction.is_none()));
    phone_id
}

#[test]
fn applied_legacy_notification_and_mute_survive_authoritative_snapshot() {
    let mut p = pair();
    legacy_posts_and_mute(&mut p);
    p.server.sync(&p.peer);
    assert_eq!(
        shown(&p.peer),
        ["n1"],
        "the mute purged the unsent news post"
    );
    assert_eq!(muted(&p.peer, "com.example.news"), Some(true));

    let high_water = p.server.log.len();
    import(&p.peer, &compact(&p.server.log, true), high_water, Some(30));
    settle(&p.peer);
    assert_eq!(
        state(&p.peer),
        Some((SnapshotProjectionState::Promoted, None))
    );
    assert_eq!(shown(&p.peer), ["n1"], "legacy post is not absent");
    assert_eq!(muted(&p.peer, "com.example.news"), Some(true));
    assert_eq!(p.peer.quarantined().unwrap(), vec![]);
}

#[test]
fn applied_legacy_tone_survives_authoritative_snapshot() {
    let mut p = pair();
    let seed = "+15555550109";
    p.owner.set_server_compaction_supported(false).unwrap();
    p.owner.set_conversation_tone(seed, Some(47)).unwrap();
    p.server.upload(&p.owner);
    assert!(p.server.log.iter().all(|e| e.compaction.is_none()));
    p.server.sync(&p.peer);
    assert_eq!(
        p.peer.conversation_tones().unwrap(),
        BTreeMap::from([(seed.to_owned(), 47)])
    );

    let high_water = p.server.log.len();
    import(&p.peer, &compact(&p.server.log, true), high_water, Some(34));
    settle(&p.peer);
    assert_eq!(
        state(&p.peer),
        Some((SnapshotProjectionState::Promoted, None))
    );
    assert_eq!(
        p.peer.conversation_tones().unwrap(),
        BTreeMap::from([(seed.to_owned(), 47)]),
        "the applied legacy tone is not absent"
    );
}

#[test]
fn newer_compactable_removal_and_filter_win_over_preserved_legacy_state() {
    let mut p = pair();
    let phone_id = legacy_posts_and_mute(&mut p);
    p.server.sync(&p.peer);
    let legacy = p.server.log.len();

    // The phone gains compaction support; the peer is offline for its newer writes.
    connect(&p.owner);
    p.owner.remove_notification("n1", "n1-1").unwrap();
    p.owner
        .set_app_muted(&phone_id, "com.example.news", "News", false)
        .unwrap();
    p.server.upload(&p.owner);
    assert!(
        p.server.log[legacy..]
            .iter()
            .all(|e| e.compaction.is_some())
    );

    let high_water = p.server.log.len();
    import(&p.peer, &full(&p.server.log), high_water, Some(31));
    settle(&p.peer);
    assert_eq!(
        state(&p.peer),
        Some((SnapshotProjectionState::Promoted, None))
    );
    assert!(shown(&p.peer).is_empty(), "the staged removal wins");
    assert_eq!(muted(&p.peer, "com.example.news"), Some(false));
}

#[test]
fn newer_compactable_tone_wins_over_preserved_legacy_tone() {
    let mut p = pair();
    let seed = "+15555550110";
    p.owner.set_server_compaction_supported(false).unwrap();
    p.owner.set_conversation_tone(seed, Some(48)).unwrap();
    p.server.upload(&p.owner);
    p.server.sync(&p.peer);

    connect(&p.owner);
    p.owner.set_conversation_tone(seed, Some(49)).unwrap();
    p.server.upload(&p.owner);
    assert!(
        p.server.log[1..]
            .iter()
            .all(|envelope| envelope.compaction.is_some())
    );

    let high_water = p.server.log.len();
    import(&p.peer, &full(&p.server.log), high_water, Some(35));
    settle(&p.peer);
    assert_eq!(
        p.peer.conversation_tones().unwrap(),
        BTreeMap::from([(seed.to_owned(), 49)])
    );
}

#[test]
fn fresh_device_applies_pending_legacy_records_after_promotion() {
    let mut p = pair();
    legacy_posts_and_mute(&mut p);
    let fresh_cfg = config(&p._dir, "fresh", &p.vault);
    let fresh = unlocked(&fresh_cfg, &p.vault);
    let high_water = p.server.log.len();
    import(&fresh, &compact(&p.server.log, true), high_water, Some(32));
    let total = settle(&fresh);
    assert_eq!(
        state(&fresh),
        Some((SnapshotProjectionState::Promoted, None))
    );
    assert!(total.applied > 0, "legacy records apply live");
    assert_eq!(shown(&fresh), ["n1"]);
    assert_eq!(muted(&fresh, "com.example.news"), Some(true));
}

#[test]
fn unreadable_legacy_record_does_not_fail_projection() {
    let mut p = pair();
    legacy_posts_and_mute(&mut p);
    p.server.sync(&p.peer);
    // An unrelated header-free record of an epoch this device never unlocked.
    let mut unreadable = p.server.log[0].clone();
    unreadable.envelope_id = EnvelopeId::new();
    unreadable.key_epoch = 99;
    p.server.log.push(unreadable);

    let high_water = p.server.log.len();
    import(&p.peer, &full(&p.server.log), high_water, Some(33));
    settle(&p.peer);
    assert_eq!(
        state(&p.peer),
        Some((SnapshotProjectionState::Promoted, None))
    );
    assert_eq!(shown(&p.peer), ["n1"]);
    assert_eq!(muted(&p.peer, "com.example.news"), Some(true));
}

#[test]
fn only_the_applied_legacy_wire_keeps_its_state() {
    let mut p = pair();
    let phone_id = p.owner_cfg.device_id.to_string();
    p.owner.set_server_compaction_supported(false).unwrap();
    p.owner
        .capture_notification(notification("n1", "com.example.mail", "first"))
        .unwrap();
    p.server.upload(&p.owner);
    assert_eq!(p.server.log.len(), 1);
    p.owner
        .set_app_muted(&phone_id, "com.example.news", "News", true)
        .unwrap();
    p.server.upload(&p.owner);
    p.server.sync(&p.peer);
    assert_eq!(shown(&p.peer), ["n1"]);

    // Altered bytes at an already journaled cursor are rejected outright.
    let mut tampered = p.server.log[0].clone();
    *tampered.ciphertext.last_mut().unwrap() ^= 0x55;
    let high_water = p.server.log.len() + 1;
    let session = p
        .peer
        .begin_snapshot_with_compaction(
            Cursor(high_water as u64),
            1,
            SnapshotPurpose::Resync,
            Some(34),
        )
        .unwrap();
    assert!(
        p.peer
            .append_snapshot_page(
                session.generation,
                &[SnapshotRecord {
                    cursor: Cursor(1),
                    envelope: tampered.clone(),
                }],
            )
            .is_err()
    );

    // The same altered record at a new cursor is never applied, so the post it claims to
    // reproduce has no provenance and is absent; the applied mute keeps its own.
    let mut records = full(&p.server.log);
    records[0] = SnapshotRecord {
        cursor: Cursor(high_water as u64),
        envelope: tampered,
    };
    records.sort_by_key(|r| r.cursor.0);
    import(&p.peer, &records, high_water, Some(35));
    settle(&p.peer);
    assert_eq!(
        state(&p.peer),
        Some((SnapshotProjectionState::Promoted, None))
    );
    assert!(
        shown(&p.peer).is_empty(),
        "tampered wire grants no exemption"
    );
    assert_eq!(muted(&p.peer, "com.example.news"), Some(true));
}

/// Like `settle`, but one record per step, so draining, staging and live apply interleave.
fn settle_one_by_one(client: &Client) {
    loop {
        let step = client.apply_pending(1).unwrap();
        if step.applied + step.drained + step.quarantined + step.superseded == 0
            && step.snapshot_remaining == 0
        {
            return;
        }
    }
}

#[test]
fn fresh_device_keeps_legacy_state_applied_while_snapshot_drains() {
    let mut p = pair();
    legacy_posts_and_mute(&mut p);
    let fresh_cfg = config(&p._dir, "fresh", &p.vault);
    let fresh = unlocked(&fresh_cfg, &p.vault);
    let high_water = p.server.log.len();
    assert!(high_water > 1, "the drain must take several steps");
    import(&fresh, &compact(&p.server.log, true), high_water, Some(36));
    settle_one_by_one(&fresh);
    assert_eq!(
        state(&fresh),
        Some((SnapshotProjectionState::Promoted, None))
    );
    assert_eq!(shown(&fresh), ["n1"]);
    assert_eq!(muted(&fresh, "com.example.news"), Some(true));
}

#[test]
fn legacy_state_applied_after_unlock_during_staging_survives_promotion() {
    let mut p = pair();
    legacy_posts_and_mute(&mut p);
    // Locked: the header-free records are journaled but stay pending.
    let locked_cfg = config(&p._dir, "locked", &p.vault);
    let locked = open(&locked_cfg);
    p.server.sync(&locked);
    // Compactable records keep the projection staging for several steps after unlock.
    connect(&p.owner);
    for key in ["c1", "c2", "c3"] {
        p.owner
            .capture_notification(notification(key, "com.example.chat", key))
            .unwrap();
    }
    p.server.upload(&p.owner);
    let high_water = p.server.log.len();
    let records = compact(&p.server.log, true);
    assert!(
        records
            .iter()
            .filter(|r| r.envelope.compaction.is_some())
            .count()
            > 1
    );
    import(&locked, &records, high_water, Some(37));
    settle(&locked);
    assert_eq!(
        state(&locked),
        Some((SnapshotProjectionState::Staging, None))
    );
    drop(locked);

    let unlocked_client = unlocked(&locked_cfg, &p.vault);
    settle_one_by_one(&unlocked_client);
    assert_eq!(
        state(&unlocked_client),
        Some((SnapshotProjectionState::Promoted, None))
    );
    assert_eq!(shown(&unlocked_client), ["c1", "c2", "c3", "n1"]);
    assert_eq!(muted(&unlocked_client, "com.example.news"), Some(true));
}

/// Whether `client` still lists the book itself, not only its contacts.
fn listed(client: &Client, book_id: &str) -> bool {
    let raw: Value = serde_json::from_str(&client.list_contact_books_json().unwrap()).unwrap();
    raw["books"]
        .as_array()
        .unwrap()
        .iter()
        .any(|book| book["id"] == book_id)
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

fn reseal_snapshot_payload(vault: &Vault, envelope: &mut Envelope, payload: &PrivatePayload) {
    let root = derive_root_key(PASSPHRASE, &vault.profile).unwrap();
    let key = derive_purpose_key(&root, &vault.profile, KeyPurpose::Event).unwrap();
    let mut plain = serde_json::to_value(payload).unwrap();
    plain["compaction"] = serde_json::to_value(&envelope.compaction).unwrap();
    let encrypted = encrypt(
        &key,
        &envelope.aad_bytes().unwrap(),
        &serde_json::to_vec(&plain).unwrap(),
    )
    .unwrap();
    envelope.ciphertext = [encrypted.nonce.as_slice(), &encrypted.ciphertext].concat();
}

/// The owner publishes two contacts; the peer syncs them; a third device then deletes the book
/// for the vault after the owner was removed, and publishes the fence and member records.
fn deleted_by_another_device(p: &mut Pair) {
    capture(
        &p.owner,
        &book(&p.owner_cfg, BOOK),
        vec![contact(BOOK, "c1", "Ada"), contact(BOOK, "c2", "Bea")],
    );
    p.server.upload(&p.owner);
    p.server.sync(&p.peer);
    let deleter = unlocked(&config(&p._dir, "deleter", &p.vault), &p.vault);
    p.server.sync(&deleter);
    call(
        deleter.delete_contact_book(
            &json!({"book_id": BOOK, "revoked_device_ids": [p.owner_cfg.device_id.to_string()]})
                .to_string(),
        ),
    );
    p.server.upload(&deleter);
}

#[test]
fn failed_snapshot_deletion_never_changes_visible_book() {
    let mut p = pair();
    deleted_by_another_device(&mut p);
    let original = call(p.peer.contact_book_view(
        &json!({"schema_version": 1, "book_id": BOOK, "limit": 200}).to_string(),
    ));
    let mut records = full(&p.server.log);
    let corrupt = records
        .iter()
        .position(|record| {
            matches!(
                opened(&p.vault, &record.envelope),
                PrivatePayload::ContactBookDeletedMember { .. }
            )
        })
        .unwrap();
    records[corrupt].envelope.ciphertext[0] ^= 0x55;
    import(&p.peer, &records, p.server.log.len(), Some(71));
    wait_for_failed_snapshot(&p.peer, records.len());
    for _ in 0..(records.len() + 3) {
        p.peer.apply_pending(1).unwrap();
        assert!(listed(&p.peer, BOOK));
        assert_eq!(
            call(p.peer.contact_book_view(
                &json!({"schema_version": 1, "book_id": BOOK, "limit": 200}).to_string(),
            )),
            original
        );
    }
}

#[test]
fn deleted_book_snapshot_stays_visible_until_promotion_then_stays_removed() {
    let mut p = pair();
    deleted_by_another_device(&mut p);
    // The peer was offline for the deletion and imports the whole history, one row per step.
    let high_water = p.server.log.len();
    import(&p.peer, &full(&p.server.log), high_water, Some(7));
    p.peer.apply_pending(1).unwrap();
    assert_eq!(
        visible(&p.peer, BOOK),
        ["c1", "c2"],
        "staging must not change live rows"
    );
    assert!(listed(&p.peer, BOOK));

    settle(&p.peer);
    assert_eq!(
        state(&p.peer),
        Some((SnapshotProjectionState::Promoted, None))
    );
    assert!(visible(&p.peer, BOOK).is_empty());
    assert!(!listed(&p.peer, BOOK));

    // A later compacted snapshot cannot bring it back either.
    import(&p.peer, &compact(&p.server.log, true), high_water, Some(8));
    settle(&p.peer);
    assert!(visible(&p.peer, BOOK).is_empty());
    assert!(!listed(&p.peer, BOOK));
}

#[test]
fn deletion_records_ahead_of_owner_records_in_a_snapshot_still_remove_the_book() {
    let mut p = pair();
    deleted_by_another_device(&mut p);
    // A receiver that never saw the log imports a snapshot re-sequenced so the fence and members
    // stage before the owner's records. Cursors are fresh, so nothing is journaled yet.
    let late = unlocked(&config(&p._dir, "late", &p.vault), &p.vault);
    let (deletions, rest): (Vec<Envelope>, Vec<Envelope>) =
        p.server.log.iter().cloned().partition(|envelope| {
            matches!(
                opened(&p.vault, envelope),
                PrivatePayload::ContactBookDeleted { .. }
                    | PrivatePayload::ContactBookDeletedMember { .. }
            )
        });
    assert_eq!(deletions.len(), 3, "one fence and two members");
    let records: Vec<SnapshotRecord> = deletions
        .into_iter()
        .chain(rest)
        .enumerate()
        .map(|(index, envelope)| SnapshotRecord {
            cursor: Cursor(index as u64 + 1),
            envelope,
        })
        .collect();

    import(&late, &records, records.len(), Some(7));
    settle(&late);

    assert_eq!(
        state(&late),
        Some((SnapshotProjectionState::Promoted, None))
    );
    assert!(visible(&late, BOOK).is_empty());
    assert!(!listed(&late, BOOK));
}

#[test]
fn legacy_snapshot_after_a_deletion_cannot_resurrect_the_book() {
    let mut p = pair();
    deleted_by_another_device(&mut p);
    p.server.sync(&p.peer);
    assert!(!listed(&p.peer, BOOK), "live apply removed the book");

    let high_water = p.server.log.len();
    import(&p.peer, &full(&p.server.log), high_water, None);
    settle(&p.peer);

    assert!(!listed(&p.peer, BOOK));
    assert!(visible(&p.peer, BOOK).is_empty());
}

#[test]
fn different_owners_fences_survive_compaction_and_reconstruction() {
    let mut p = pair();
    capture(
        &p.owner,
        &book(&p.owner_cfg, BOOK),
        vec![contact(BOOK, "c1", "Ada")],
    );
    p.server.upload(&p.owner);
    p.server.sync(&p.peer);
    call(
        p.peer.delete_contact_book(
            &json!({
                "book_id": BOOK, "revoked_device_ids": [p.owner_cfg.device_id.to_string()]
            })
            .to_string(),
        ),
    );
    p.server.upload(&p.peer);
    p.server.sync(&p.peer);
    let owner_a_fence = p.server.log.iter().find(|envelope| {
        matches!(opened(&p.vault, envelope), PrivatePayload::ContactBookDeleted { ref owner_device_id, .. } if owner_device_id == &p.owner_cfg.device_id.to_string())
    }).unwrap().clone();

    let owner_b_cfg = config(&p._dir, "owner-b", &p.vault);
    let owner_b = unlocked(&owner_b_cfg, &p.vault);
    capture(
        &owner_b,
        &book(&owner_b_cfg, BOOK),
        vec![contact(BOOK, "c2", "Bea")],
    );
    p.server.upload(&owner_b);
    p.server.sync(&p.peer);
    assert_eq!(visible(&p.peer, BOOK), ["c2"]);
    call(
        p.peer.delete_contact_book(
            &json!({
                "book_id": BOOK, "revoked_device_ids": [owner_b_cfg.device_id.to_string()]
            })
            .to_string(),
        ),
    );
    p.server.upload(&p.peer);
    let owner_b_fence = p.server.log.iter().find(|envelope| {
        matches!(opened(&p.vault, envelope), PrivatePayload::ContactBookDeleted { ref owner_device_id, .. } if owner_device_id == &owner_b_cfg.device_id.to_string())
    }).unwrap();
    assert!(
        !owner_b_fence
            .compaction
            .as_ref()
            .unwrap()
            .supersedes
            .iter()
            .any(
                |item| item.producer_device_id == owner_a_fence.producer_device_id
                    && item.producer_sequence == owner_a_fence.producer_sequence
            )
    );
    let records = compact(&p.server.log, false);
    assert!(
        records
            .iter()
            .any(|record| record.envelope.envelope_id == owner_a_fence.envelope_id)
    );
    assert!(
        records
            .iter()
            .any(|record| record.envelope.envelope_id == owner_b_fence.envelope_id)
    );
    let fresh_cfg = config(&p._dir, "reconstruction", &p.vault);
    let fresh = unlocked(&fresh_cfg, &p.vault);
    import(&fresh, &records, p.server.log.len(), Some(92));
    settle(&fresh);
    assert_eq!(
        state(&fresh),
        Some((SnapshotProjectionState::Promoted, None))
    );
    assert!(!listed(&fresh, BOOK));
    let connection = crate::contact_book_deletion::sqlcipher(&fresh_cfg.database_path);
    let fences: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM deleted_contact_books WHERE book_id=?",
            [BOOK],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(fences, 2);
}

#[test]
fn foreign_owner_fence_does_not_remove_snapshot_book() {
    let mut p = pair();
    capture(
        &p.owner,
        &book(&p.owner_cfg, BOOK),
        vec![contact(BOOK, "c1", "Ada")],
    );
    p.server.upload(&p.owner);
    p.server.sync(&p.peer);
    let mut foreign = p.server.log[0].clone();
    foreign.envelope_id = EnvelopeId::new();
    foreign.producer_device_id = DeviceId::new();
    foreign.producer_sequence = SourceSequence(1);
    let payload = PrivatePayload::ContactBookDeleted {
        book_id: BOOK.into(),
        owner_device_id: DeviceId::new().to_string(),
        generation: "1".into(),
        deleted_at_ms: 1,
    };
    reseal_snapshot_payload(&p.vault, &mut foreign, &payload);
    p.server.log.push(foreign);
    let records = full(&p.server.log);
    import(&p.peer, &records, records.len(), Some(93));
    settle(&p.peer);
    assert_eq!(
        state(&p.peer),
        Some((SnapshotProjectionState::Promoted, None))
    );
    assert_eq!(visible(&p.peer, BOOK), ["c1"]);
}

#[test]
fn malformed_deletion_generation_in_snapshot_is_quarantined() {
    for invalid in ["abc", "07", "99999999999999999999"] {
        let mut p = pair();
        capture(
            &p.owner,
            &book(&p.owner_cfg, BOOK),
            vec![contact(BOOK, "c1", "Ada")],
        );
        p.server.upload(&p.owner);
        p.server.sync(&p.peer);
        let mut bad = p.server.log[0].clone();
        bad.envelope_id = EnvelopeId::new();
        bad.producer_device_id = DeviceId::new();
        bad.producer_sequence = SourceSequence(1);
        reseal_snapshot_payload(
            &p.vault,
            &mut bad,
            &PrivatePayload::ContactBookDeleted {
                book_id: BOOK.into(),
                owner_device_id: p.owner_cfg.device_id.to_string(),
                generation: invalid.into(),
                deleted_at_ms: 1,
            },
        );
        p.server.log.push(bad);
        let records = full(&p.server.log);
        import(&p.peer, &records, records.len(), Some(91));
        wait_for_failed_snapshot(&p.peer, records.len());
        assert_eq!(
            state(&p.peer),
            Some((
                SnapshotProjectionState::Failed,
                Some("invalid_payload".into())
            ))
        );
        assert_eq!(visible(&p.peer, BOOK), ["c1"]);
    }
}

#[test]
fn snapshot_contact_publication_generation_converges_across_deletion_order() {
    for revision in [None, Some("1"), Some("2")] {
        for deletion_first in [false, true] {
            let mut p = pair();
            deleted_by_another_device(&mut p);
            let mut book2 = book(&p.owner_cfg, BOOK);
            book2["generation"] = json!("2");
            let original = p.server.log.iter().find(|envelope| {
                matches!(opened(&p.vault, envelope), PrivatePayload::ContactUpserted { ref contact, .. } if contact["id"] == "c2")
            }).unwrap().clone();
            let original_payload = opened(&p.vault, &original);
            let PrivatePayload::ContactUpserted { mut contact, .. } = original_payload else {
                unreachable!()
            };
            let original_revision = contact["revision"]
                .as_str()
                .unwrap()
                .parse::<u64>()
                .unwrap();
            let mut continuation = Vec::new();
            let make = |payload: PrivatePayload, sequence: u64| {
                let mut envelope = original.clone();
                envelope.envelope_id = EnvelopeId::new();
                envelope.producer_sequence = SourceSequence(sequence);
                reseal_snapshot_payload(&p.vault, &mut envelope, &payload);
                envelope
            };
            continuation.push(make(
                PrivatePayload::ContactBookState {
                    book: book2.clone(),
                },
                100,
            ));
            if let Some(revision) = revision {
                contact["revision"] =
                    json!((original_revision + u64::from(revision == "2")).to_string());
                continuation.push(make(
                    PrivatePayload::ContactUpserted {
                        book: book2,
                        contact,
                    },
                    101,
                ));
            }
            let (mut deletions, originals): (Vec<_>, Vec<_>) =
                p.server.log.iter().cloned().partition(|envelope| {
                    matches!(
                        opened(&p.vault, envelope),
                        PrivatePayload::ContactBookDeleted { .. }
                            | PrivatePayload::ContactBookDeletedMember { .. }
                    )
                });
            if deletion_first && revision == Some("1") {
                // Exercise member-first staging without relying on a book fence.
                deletions.retain(|envelope| {
                    matches!(
                        opened(&p.vault, envelope),
                        PrivatePayload::ContactBookDeletedMember { .. }
                    )
                });
            }
            let mut records = originals;
            if deletion_first {
                records.extend(deletions);
                records.extend(continuation);
            } else {
                records.extend(continuation);
                records.extend(deletions);
            }
            let records: Vec<_> = records
                .into_iter()
                .enumerate()
                .map(|(index, envelope)| SnapshotRecord {
                    cursor: Cursor(index as u64 + 1),
                    envelope,
                })
                .collect();
            let receiver = unlocked(&config(&p._dir, "matrix", &p.vault), &p.vault);
            import(&receiver, &records, records.len(), Some(88));
            settle_one_by_one(&receiver);
            assert_eq!(
                state(&receiver),
                Some((SnapshotProjectionState::Promoted, None))
            );
            assert_eq!(
                visible(&receiver, BOOK).contains(&"c2".to_owned()),
                revision.is_some(),
                "revision={revision:?}, deletion_first={deletion_first}"
            );
        }
    }
}

#[test]
fn snapshot_generation_two_tombstone_survives_generation_one_records_in_either_order() {
    for member_first in [false, true] {
        let mut p = pair();
        capture(
            &p.owner,
            &book(&p.owner_cfg, BOOK),
            vec![contact(BOOK, "c1", "Ada")],
        );
        p.server.upload(&p.owner);
        let original = p.server.log[0].clone();
        let make = |producer: DeviceId, sequence: u64, payload: PrivatePayload| {
            let mut envelope = original.clone();
            envelope.envelope_id = EnvelopeId::new();
            envelope.producer_device_id = producer;
            envelope.producer_sequence = SourceSequence(sequence);
            reseal_snapshot_payload(&p.vault, &mut envelope, &payload);
            envelope
        };
        // Owner removes Ada at generation 2; the generation-1 member and fence arrive late.
        let mut restored = contact(BOOK, "c1", "Ada");
        restored["revision"] = json!("2");
        let mut continued = book(&p.owner_cfg, BOOK);
        continued["generation"] = json!("2");
        let tombstone = make(
            p.owner_cfg.device_id,
            100,
            PrivatePayload::ContactRemoved {
                book_id: BOOK.into(),
                contact_id: "c1".into(),
                tombstone: json!({
                    "contact_id": "c1",
                    "revision": "2",
                    "deleted_at": 5,
                    "restored_contact": restored,
                    "book": continued,
                }),
            },
        );
        let owner = p.owner_cfg.device_id.to_string();
        let member = make(
            DeviceId::new(),
            1,
            PrivatePayload::ContactBookDeletedMember {
                book_id: BOOK.into(),
                contact_id: "c1".into(),
                owner_device_id: owner.clone(),
                generation: "1".into(),
            },
        );
        let fence = make(
            DeviceId::new(),
            2,
            PrivatePayload::ContactBookDeleted {
                book_id: BOOK.into(),
                owner_device_id: owner,
                generation: "1".into(),
                deleted_at_ms: 1,
            },
        );
        let mut log = p.server.log.clone();
        if member_first {
            log.extend([member, fence, tombstone]);
        } else {
            log.extend([tombstone, member, fence]);
        }
        let name = if member_first {
            "member-first"
        } else {
            "tombstone-first"
        };
        let receiver_config = config(&p._dir, name, &p.vault);
        let receiver = unlocked(&receiver_config, &p.vault);
        import(&receiver, &full(&log), log.len(), Some(95));
        settle(&receiver);

        assert_eq!(
            state(&receiver),
            Some((SnapshotProjectionState::Promoted, None))
        );
        let row: (i64, i64) = crate::contact_book_deletion::sqlcipher(
            &receiver_config.database_path,
        )
        .query_row(
            "SELECT deleted_at IS NOT NULL, publication_generation FROM contacts WHERE id='c1'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
        assert_eq!(row, (1, 2), "{name}: the generation-2 tombstone survives");
    }
}

#[test]
fn snapshot_promotion_moves_a_book_named_by_a_succeeded_owner_to_its_successor() {
    let mut p = pair();
    capture(
        &p.owner,
        &book(&p.owner_cfg, BOOK),
        vec![contact(BOOK, "c1", "Ada")],
    );
    p.server.upload(&p.owner);
    let receiver_config = config(&p._dir, "succeeded", &p.vault);
    let receiver = unlocked(&receiver_config, &p.vault);
    let path = &receiver_config.database_path;
    let predecessor = p.owner_cfg.device_id;
    let successor = DeviceId::new();
    // This device already knows the owner was succeeded, so the staged book names a predecessor.
    crate::contact_book_deletion::sqlcipher(path)
        .execute_batch(&format!(
            "INSERT INTO device_successions(predecessor,successor,platform,hint_tag,key_epoch) VALUES('{predecessor}','{successor}','android','hint',1)"
        ))
        .unwrap();

    import(
        &receiver,
        &full(&p.server.log),
        p.server.log.len(),
        Some(96),
    );
    settle(&receiver);

    assert_eq!(
        state(&receiver),
        Some((SnapshotProjectionState::Promoted, None))
    );
    let connection = crate::contact_book_deletion::sqlcipher(path);
    let book_owner: String = connection
        .query_row("SELECT owner_device_id FROM contact_books", [], |row| {
            row.get(0)
        })
        .unwrap();
    let contact_owner: String = connection
        .query_row(
            "SELECT publication_owner FROM contacts WHERE id='c1'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(book_owner, successor.to_string());
    assert_eq!(contact_owner, successor.to_string());
}

#[test]
fn a_member_before_or_after_a_continued_upsert_stages_the_same_contact() {
    let mut p = pair();
    deleted_by_another_device(&mut p);
    // The owner continues the deleted book at generation 2 and republishes Ada with a changed
    // name, so the continuation carries a generation-2 upsert of her.
    let mut continued = book(&p.owner_cfg, BOOK);
    continued["generation"] = json!("2");
    capture(&p.owner, &continued, vec![contact(BOOK, "c1", "Augusta")]);
    p.server.upload(&p.owner);

    let log = p.server.log.clone();
    let (deletions, rest): (Vec<Envelope>, Vec<Envelope>) =
        log.iter().cloned().partition(|envelope| {
            matches!(
                opened(&p.vault, envelope),
                PrivatePayload::ContactBookDeleted { .. }
                    | PrivatePayload::ContactBookDeletedMember { .. }
            )
        });
    // Log order stages the generation-1 members before the continuation; the reversed order
    // stages them after it. Ada, republished at generation 2, survives either way.
    let members_after: Vec<Envelope> = rest.into_iter().chain(deletions).collect();
    for (name, records) in [("before", &log), ("after", &members_after)] {
        let receiver = unlocked(&config(&p._dir, name, &p.vault), &p.vault);
        import(&receiver, &full(records), records.len(), Some(7));
        settle(&receiver);
        assert_eq!(
            state(&receiver),
            Some((SnapshotProjectionState::Promoted, None))
        );
        assert!(
            visible(&receiver, BOOK).iter().any(|id| id == "c1"),
            "{name}: the continued contact is kept"
        );
    }
}

/// The owner's authoritative full scan observes exactly `contacts`: unobserved rows are deleted
/// and observed rows take their supplied bodies.
fn replace_contacts(owner: &Client, book_id: &str, scan: &str, contacts: Vec<Value>) {
    call(owner.begin_contact_scan(
        &json!({"schema_version": 1, "scan_id": scan, "book_id": book_id, "generation": "1", "access": "full", "authoritative": true}).to_string(),
    ));
    for contact in contacts {
        owner
            .observe_contact_scan(
                &json!({"schema_version": 1, "scan_id": scan, "contact": contact}).to_string(),
            )
            .unwrap();
    }
    let done = call(owner.finish_contact_scan(
        &json!({"schema_version": 1, "scan_id": scan, "complete": true}).to_string(),
    ));
    assert_eq!(done["deletions_applied"], true, "{done}");
}

#[test]
fn snapshot_promotion_between_cursor_pages_keeps_keyset_order() {
    let mut p = pair();
    let b = book(&p.owner_cfg, BOOK);
    capture(
        &p.owner,
        &b,
        ["a:Ada", "b:Bea", "c:Cy", "d:Dee", "e:Eve"]
            .iter()
            .map(|entry| {
                let (id, given) = entry.split_once(':').unwrap();
                contact(BOOK, id, given)
            })
            .collect(),
    );
    p.server.upload(&p.owner);
    p.server.sync(&p.peer);
    settle(&p.peer);

    let sort = json!({"key": "given", "direction": "asc"});
    let page = |after: Option<Value>| -> Value {
        let mut input = json!({"schema_version": 1, "book_id": BOOK, "sort": sort, "limit": 2});
        if let Some(cursor) = after {
            input["after"] = cursor;
        }
        call(p.peer.contact_book_view(&input.to_string()))
    };
    let ids_of = |output: &Value| -> Vec<String> {
        output["contacts"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row["id"].as_str().unwrap().to_owned())
            .collect()
    };

    // The first page ends at "b" (Bea), the cursor boundary.
    let first = page(None);
    assert_eq!(ids_of(&first), ["a", "b"]);
    assert_eq!(first["total"], 5);
    let cursor = first["next_cursor"].clone();
    assert!(cursor.is_string());

    // The owner deletes the boundary row "b", moves "d" from after the boundary to before it,
    // and inserts "f" after it. The changes are published as one authoritative scan and then
    // promoted from a full snapshot while the peer is mid-pagination.
    replace_contacts(
        &p.owner,
        BOOK,
        "scan-boundary",
        ["a:Ada", "c:Cy", "d:Aaron", "e:Eve", "f:Fay"]
            .iter()
            .map(|entry| {
                let (id, given) = entry.split_once(':').unwrap();
                contact(BOOK, id, given)
            })
            .collect(),
    );
    p.server.upload(&p.owner);
    let log = full(&p.server.log);
    import(&p.peer, &log, p.server.log.len(), Some(7));
    settle(&p.peer);
    assert_eq!(
        state(&p.peer),
        Some((SnapshotProjectionState::Promoted, None))
    );

    // Rows sorting after the cursor key ("Bea") in the promoted state.
    let promoted = call(p.peer.contact_book_view(
        &json!({"schema_version": 1, "book_id": BOOK, "sort": sort, "limit": 200}).to_string(),
    ));
    assert_eq!(promoted["total"], 5);
    let expected: Vec<String> = promoted["contacts"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|row| row["name"]["given"].as_str().unwrap() > "Bea")
        .map(|row| row["id"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(expected, ["c", "e", "f"]);

    // Continue with the pre-promotion cursor and walk the rest of the book.
    let mut returned = ids_of(&first);
    let mut after = cursor;
    loop {
        let output = page(Some(after.clone()));
        assert_eq!(output["total"], 5);
        let ids = ids_of(&output);
        for id in &ids {
            assert!(!returned.contains(id), "{id} was returned twice");
        }
        returned.extend(ids);
        if output["next_cursor"].is_null() {
            break;
        }
        after = output["next_cursor"].clone();
    }
    assert_eq!(returned[2..], expected[..]);
}
