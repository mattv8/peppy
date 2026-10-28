//! Durable replicated conversation-pin regression coverage.
mod common;
use common::*;
use tempfile::TempDir;

fn incoming_from(body: &str, provider_id: &str, sender_address: &str) -> IncomingSms {
    let mut message = incoming(body, provider_id);
    message.sender_address = sender_address.into();
    message
}

#[test]
fn pinning_an_established_conversation_is_visible_and_survives_reopen() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let cfg = config(&dir, "pin-writer", &vault);
    let client = unlocked(&cfg, &vault);
    let conversation = client
        .capture_incoming(incoming("hello", "pin-1"))
        .unwrap()
        .conversation_id;
    client.set_conversation_pinned(conversation, true).unwrap();
    assert!(
        client
            .list_conversations()
            .unwrap()
            .into_iter()
            .find(|row| row.conversation_id == conversation)
            .unwrap()
            .pinned
    );
    drop(client);
    let reopened = unlocked(&cfg, &vault);
    assert!(
        reopened
            .list_conversations()
            .unwrap()
            .into_iter()
            .find(|row| row.conversation_id == conversation)
            .unwrap()
            .pinned
    );
}

#[test]
fn pins_and_false_tombstones_converge_between_peers_after_offline_writes() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let writer = unlocked(&config(&dir, "pin-writer", &vault), &vault);
    let peer = unlocked(&config(&dir, "pin-peer", &vault), &vault);
    let conversation = writer
        .capture_incoming(incoming("hello", "pin-sync"))
        .unwrap()
        .conversation_id;
    let mut server = Server::default();
    server.upload(&writer);
    server.sync(&peer);

    writer.set_conversation_pinned(conversation, true).unwrap();
    server.upload(&writer);
    server.sync(&peer);
    assert!(
        peer.list_conversations()
            .unwrap()
            .into_iter()
            .find(|row| row.conversation_id == conversation)
            .unwrap()
            .pinned
    );

    peer.set_conversation_pinned(conversation, false).unwrap();
    server.upload(&peer);
    server.sync(&writer);
    assert!(
        !writer
            .list_conversations()
            .unwrap()
            .into_iter()
            .find(|row| row.conversation_id == conversation)
            .unwrap()
            .pinned
    );
    assert!(
        !peer
            .list_conversations()
            .unwrap()
            .into_iter()
            .find(|row| row.conversation_id == conversation)
            .unwrap()
            .pinned
    );
}

#[test]
fn pinning_rejects_locked_and_message_less_conversations() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let cfg = config(&dir, "pin-locked", &vault);
    let locked = open(&cfg);
    assert!(matches!(
        locked.set_conversation_pinned(ConversationId::new(), true),
        Err(Error::KeysUnavailable)
    ));
    let unlocked = unlocked(&cfg, &vault);
    assert!(matches!(
        unlocked.set_conversation_pinned(ConversationId::new(), true),
        Err(Error::InvalidRequest(_))
    ));
}

#[test]
fn remote_pin_metadata_is_retained_before_its_message_arrives() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let writer = unlocked(&config(&dir, "pin-order-writer", &vault), &vault);
    let peer = unlocked(&config(&dir, "pin-order-peer", &vault), &vault);
    let conversation = writer
        .capture_incoming(incoming("hello", "pin-order"))
        .unwrap()
        .conversation_id;
    writer.set_conversation_pinned(conversation, true).unwrap();
    let mut events = writer.pending_outbox().unwrap();
    assert_eq!(events.len(), 2);
    events.reverse();
    peer.ingest(&events[0], Cursor(1)).unwrap();
    peer.apply_pending(10).unwrap();
    assert!(peer.list_conversations().unwrap().is_empty());
    peer.ingest(&events[1], Cursor(2)).unwrap();
    peer.apply_pending(10).unwrap();
    assert!(peer.list_conversations().unwrap()[0].pinned);
}

#[test]
fn equal_revision_conflicts_use_writer_identity_and_duplicate_delivery_is_idempotent() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let left_cfg = config(&dir, "pin-tie-left", &vault);
    let right_cfg = config(&dir, "pin-tie-right", &vault);
    let left = unlocked(&left_cfg, &vault);
    let right = unlocked(&right_cfg, &vault);
    let conversation = left
        .capture_incoming(incoming("hello", "pin-tie"))
        .unwrap()
        .conversation_id;
    let mut server = Server::default();
    server.upload(&left);
    server.sync(&left);
    server.sync(&right);

    left.set_conversation_pinned(conversation, true).unwrap();
    right.set_conversation_pinned(conversation, false).unwrap();
    let left_event = left.pending_outbox().unwrap().pop().unwrap();
    let right_event = right.pending_outbox().unwrap().pop().unwrap();
    let expected =
        left_event.producer_device_id.to_string() > right_event.producer_device_id.to_string();
    for client in [&left, &right] {
        client.ingest(&left_event, Cursor(2)).unwrap();
        client.ingest(&right_event, Cursor(3)).unwrap();
        assert!(matches!(
            client.ingest(&right_event, Cursor(3)).unwrap(),
            IngestResult::Duplicate
        ));
        client.apply_pending(10).unwrap();
        assert_eq!(client.list_conversations().unwrap()[0].pinned, expected);
    }
}

fn import_empty_compaction_snapshot(client: &Client, high_water: u64, generation: u64) {
    let session = client
        .begin_snapshot_with_compaction(
            Cursor(high_water),
            0,
            SnapshotPurpose::Resync,
            Some(generation),
        )
        .unwrap();
    client.finish_snapshot(session.generation).unwrap();
    loop {
        let report = client.apply_pending(100).unwrap();
        if report.applied + report.drained + report.quarantined + report.superseded == 0
            && report.snapshot_remaining == 0
        {
            break;
        }
    }
}

#[test]
fn empty_authoritative_snapshot_keeps_unsent_local_pin_write() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "pin-snapshot-local", &vault), &vault);
    let conversation = client
        .capture_incoming(incoming("hello", "pin-snapshot-local"))
        .unwrap()
        .conversation_id;
    client.set_conversation_pinned(conversation, true).unwrap();
    import_empty_compaction_snapshot(&client, 0, 1);
    assert!(client.list_conversations().unwrap()[0].pinned);
}

#[test]
fn empty_authoritative_snapshot_removes_a_synced_pin_tombstone_state() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let writer = unlocked(&config(&dir, "pin-snapshot-writer", &vault), &vault);
    let peer = unlocked(&config(&dir, "pin-snapshot-peer", &vault), &vault);
    let conversation = writer
        .capture_incoming(incoming("hello", "pin-snapshot-remote"))
        .unwrap()
        .conversation_id;
    writer.set_conversation_pinned(conversation, true).unwrap();
    let mut server = Server::default();
    server.upload(&writer);
    server.sync(&peer);
    import_empty_compaction_snapshot(&peer, 2, 2);
    let store = peer.list_conversations().unwrap();
    assert_eq!(store.len(), 1);
    assert!(!store[0].pinned);
}

#[test]
fn legacy_server_without_compaction_accepts_header_free_pin_and_unpin() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let writer = unlocked(&config(&dir, "pin-legacy-writer", &vault), &vault);
    let peer = unlocked(&config(&dir, "pin-legacy-peer", &vault), &vault);
    for client in [&writer, &peer] {
        client.set_server_compaction_state(false, false).unwrap();
    }
    let conversation = writer
        .capture_incoming(incoming("hello", "pin-legacy"))
        .unwrap()
        .conversation_id;
    let mut server = Server::default();
    server.upload(&writer);
    server.sync(&peer);

    writer.set_conversation_pinned(conversation, true).unwrap();
    let pin = writer.pending_outbox().unwrap().pop().unwrap();
    assert!(pin.compaction.is_none());
    writer.ack_outbox(pin.envelope_id).unwrap();
    server.log.push(pin);
    server.sync(&peer);
    assert!(peer.list_conversations().unwrap()[0].pinned);

    writer.set_conversation_pinned(conversation, false).unwrap();
    let unpin = writer.pending_outbox().unwrap().pop().unwrap();
    assert!(unpin.compaction.is_none());
    writer.ack_outbox(unpin.envelope_id).unwrap();
    server.log.push(unpin);
    server.sync(&peer);
    assert!(!peer.list_conversations().unwrap()[0].pinned);
}

#[test]
fn authoritative_snapshot_keeps_retained_legacy_pin_and_applies_newer_compactable_unpin() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let writer = unlocked(
        &config(&dir, "pin-legacy-projection-writer", &vault),
        &vault,
    );
    let peer = unlocked(&config(&dir, "pin-legacy-projection-peer", &vault), &vault);
    for client in [&writer, &peer] {
        client.set_server_compaction_state(false, false).unwrap();
    }
    let retained_pin = writer
        .capture_incoming(incoming_from(
            "retained",
            "pin-legacy-projection-retained",
            "+15555550101",
        ))
        .unwrap()
        .conversation_id;
    let superseded_pin = writer
        .capture_incoming(incoming_from(
            "superseded",
            "pin-legacy-projection-superseded",
            "+15555550102",
        ))
        .unwrap()
        .conversation_id;
    let mut server = Server::default();
    server.upload(&writer);
    server.sync(&peer);

    writer.set_conversation_pinned(retained_pin, true).unwrap();
    writer
        .set_conversation_pinned(superseded_pin, true)
        .unwrap();
    server.upload(&writer);
    assert!(
        server
            .log
            .iter()
            .all(|envelope| envelope.compaction.is_none()),
        "legacy records must be header-free"
    );
    server.sync(&peer);

    writer.set_server_compaction_state(true, true).unwrap();
    writer
        .set_conversation_pinned(superseded_pin, false)
        .unwrap();
    server.upload(&writer);
    assert!(
        server.log.last().unwrap().compaction.is_some(),
        "the newer unpin must be compactable"
    );

    let records: Vec<SnapshotRecord> = server
        .log
        .iter()
        .enumerate()
        .map(|(index, envelope)| SnapshotRecord {
            cursor: Cursor(index as u64 + 1),
            envelope: envelope.clone(),
        })
        .collect();
    let session = peer
        .begin_snapshot_with_compaction(
            Cursor(records.len() as u64),
            records.len() as u64,
            SnapshotPurpose::Resync,
            Some(3),
        )
        .unwrap();
    peer.append_snapshot_page(session.generation, &records)
        .unwrap();
    peer.finish_snapshot(session.generation).unwrap();
    loop {
        let report = peer.apply_pending(100).unwrap();
        if report.applied + report.drained + report.quarantined + report.superseded == 0
            && report.snapshot_remaining == 0
        {
            break;
        }
    }

    let rows = peer.list_conversations().unwrap();
    assert!(
        rows.iter()
            .find(|row| row.conversation_id == retained_pin)
            .unwrap()
            .pinned,
        "an applied header-free pin retained by the snapshot must survive promotion"
    );
    assert!(
        !rows
            .iter()
            .find(|row| row.conversation_id == superseded_pin)
            .unwrap()
            .pinned,
        "a newer staged compactable unpin must beat retained legacy provenance"
    );
}

#[test]
fn nonempty_compaction_snapshot_restores_lww_pins_and_keeps_unsent_local_write() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let writer = unlocked(&config(&dir, "pin-projection-writer", &vault), &vault);
    let peer = unlocked(&config(&dir, "pin-projection-peer", &vault), &vault);
    let retained_true = writer
        .capture_incoming(incoming_from("true", "pin-projection-true", "+15555550101"))
        .unwrap()
        .conversation_id;
    let retained_false = writer
        .capture_incoming(incoming_from(
            "false",
            "pin-projection-false",
            "+15555550102",
        ))
        .unwrap()
        .conversation_id;
    let local_pending = writer
        .capture_incoming(incoming_from(
            "local",
            "pin-projection-local",
            "+15555550103",
        ))
        .unwrap()
        .conversation_id;
    let local_newer = writer
        .capture_incoming(incoming_from(
            "newer",
            "pin-projection-newer",
            "+15555550104",
        ))
        .unwrap()
        .conversation_id;
    writer.set_server_compaction_state(true, true).unwrap();
    writer.set_conversation_pinned(retained_true, true).unwrap();
    writer
        .set_conversation_pinned(retained_false, true)
        .unwrap();
    writer
        .set_conversation_pinned(retained_false, false)
        .unwrap();
    writer.set_conversation_pinned(local_newer, true).unwrap();
    let mut server = Server::default();
    server.upload(&writer);
    server.sync(&peer);

    peer.set_conversation_pinned(local_pending, true).unwrap();
    peer.set_conversation_pinned(local_newer, false).unwrap();
    let records: Vec<SnapshotRecord> = server
        .log
        .iter()
        .enumerate()
        .map(|(index, envelope)| SnapshotRecord {
            cursor: Cursor(index as u64 + 1),
            envelope: envelope.clone(),
        })
        .collect();
    assert_eq!(
        records
            .iter()
            .filter(|record| record.envelope.compaction.is_some())
            .count(),
        4,
        "the nonempty authoritative snapshot must retain all pin winners"
    );
    let session = peer
        .begin_snapshot_with_compaction(
            Cursor(server.log.len() as u64),
            records.len() as u64,
            SnapshotPurpose::Resync,
            Some(3),
        )
        .unwrap();
    peer.append_snapshot_page(session.generation, &records)
        .unwrap();
    peer.finish_snapshot(session.generation).unwrap();
    loop {
        let report = peer.apply_pending(100).unwrap();
        if report.applied + report.drained + report.quarantined + report.superseded == 0
            && report.snapshot_remaining == 0
        {
            break;
        }
    }

    let rows = peer.list_conversations().unwrap();
    assert!(
        rows.iter()
            .find(|row| row.conversation_id == retained_true)
            .unwrap()
            .pinned
    );
    assert!(
        !rows
            .iter()
            .find(|row| row.conversation_id == local_newer)
            .unwrap()
            .pinned,
        "a lower-revision staged pin must not override the newer local tombstone"
    );
    assert!(
        !rows
            .iter()
            .find(|row| row.conversation_id == retained_false)
            .unwrap()
            .pinned
    );
    assert!(
        rows.iter()
            .find(|row| row.conversation_id == local_pending)
            .unwrap()
            .pinned
    );

    // The retained snapshot includes the earlier true state before its false tombstone; its
    // staging reducer must keep the false LWW winner rather than resurrecting that stale pin.
    assert!(
        !peer
            .list_conversations()
            .unwrap()
            .iter()
            .find(|row| row.conversation_id == retained_false)
            .unwrap()
            .pinned
    );
}
