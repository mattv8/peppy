//! Durable replicated conversation-tone regression coverage.
mod common;
use common::*;
use std::collections::BTreeMap;
use tempfile::TempDir;

fn tones(client: &Client) -> BTreeMap<String, u8> {
    client.conversation_tones().unwrap()
}

fn settle_snapshot(client: &Client) {
    loop {
        let report = client.apply_pending(100).unwrap();
        if report.applied + report.drained + report.quarantined + report.superseded == 0
            && report.snapshot_remaining == 0
        {
            return;
        }
    }
}

fn import_snapshot(client: &Client, records: &[SnapshotRecord], high_water: u64, generation: u64) {
    let session = client
        .begin_snapshot_with_compaction(
            Cursor(high_water),
            records.len() as u64,
            SnapshotPurpose::Resync,
            Some(generation),
        )
        .unwrap();
    if !records.is_empty() {
        client
            .append_snapshot_page(session.generation, records)
            .unwrap();
    }
    client.finish_snapshot(session.generation).unwrap();
    settle_snapshot(client);
}

#[test]
fn concurrent_equal_revision_tones_use_writer_identity_and_duplicate_delivery_is_idempotent() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let left = unlocked(&config(&dir, "tone-tie-left", &vault), &vault);
    let right = unlocked(&config(&dir, "tone-tie-right", &vault), &vault);
    let seed = "+15555550101";

    left.set_conversation_tone(seed, Some(12)).unwrap();
    right.set_conversation_tone(seed, Some(78)).unwrap();
    let left_event = left.pending_outbox().unwrap().pop().unwrap();
    let right_event = right.pending_outbox().unwrap().pop().unwrap();
    let expected =
        if left_event.producer_device_id.to_string() > right_event.producer_device_id.to_string() {
            12
        } else {
            78
        };

    for client in [&left, &right] {
        client.ingest(&left_event, Cursor(1)).unwrap();
        client.ingest(&right_event, Cursor(2)).unwrap();
        assert!(matches!(
            client.ingest(&right_event, Cursor(2)).unwrap(),
            IngestResult::Duplicate
        ));
        client.apply_pending(10).unwrap();
        assert_eq!(tones(client), BTreeMap::from([(seed.to_owned(), expected)]));
    }
}

#[test]
fn set_and_automatic_reset_converge_between_peers() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let writer = unlocked(&config(&dir, "tone-reset-writer", &vault), &vault);
    let peer = unlocked(&config(&dir, "tone-reset-peer", &vault), &vault);
    let seed = "+15555550102";
    let mut server = Server::default();

    writer.set_conversation_tone(seed, Some(17)).unwrap();
    server.upload(&writer);
    server.sync(&peer);
    assert_eq!(tones(&peer), BTreeMap::from([(seed.to_owned(), 17)]));

    peer.set_conversation_tone(seed, None).unwrap();
    server.upload(&peer);
    server.sync(&writer);
    assert!(tones(&writer).is_empty());
    assert!(tones(&peer).is_empty());
}

#[test]
fn empty_authoritative_snapshot_removes_synced_tones_but_keeps_unsent_local_writes() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let writer = unlocked(&config(&dir, "tone-empty-writer", &vault), &vault);
    let peer = unlocked(&config(&dir, "tone-empty-peer", &vault), &vault);
    let synced_seed = "+15555550103";
    let local_seed = "+15555550104";
    let mut server = Server::default();

    writer.set_conversation_tone(synced_seed, Some(21)).unwrap();
    server.upload(&writer);
    server.sync(&peer);
    peer.set_conversation_tone(local_seed, Some(22)).unwrap();

    import_snapshot(&peer, &[], server.log.len() as u64, 1);
    assert_eq!(tones(&peer), BTreeMap::from([(local_seed.to_owned(), 22)]));
}

#[test]
fn nonempty_authoritative_snapshot_restores_winners_and_preserves_newer_unsent_write() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let writer = unlocked(&config(&dir, "tone-snapshot-writer", &vault), &vault);
    let peer = unlocked(&config(&dir, "tone-snapshot-peer", &vault), &vault);
    let retained_seed = "+15555550105";
    let reset_seed = "+15555550106";
    let local_seed = "+15555550107";
    let mut server = Server::default();

    writer
        .set_conversation_tone(retained_seed, Some(31))
        .unwrap();
    writer.set_conversation_tone(reset_seed, Some(32)).unwrap();
    writer.set_conversation_tone(reset_seed, None).unwrap();
    server.upload(&writer);
    server.sync(&peer);
    peer.set_conversation_tone(local_seed, Some(33)).unwrap();

    let records: Vec<SnapshotRecord> = server
        .log
        .iter()
        .enumerate()
        .map(|(index, envelope)| SnapshotRecord {
            cursor: Cursor(index as u64 + 1),
            envelope: envelope.clone(),
        })
        .collect();
    import_snapshot(&peer, &records, server.log.len() as u64, 2);

    assert_eq!(
        tones(&peer),
        BTreeMap::from([(local_seed.to_owned(), 33), (retained_seed.to_owned(), 31)])
    );
}

#[test]
fn locked_client_rejects_tone_mutations() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let config = config(&dir, "tone-locked", &vault);
    let locked = open(&config);

    assert!(matches!(
        locked.set_conversation_tone("+15555550108", Some(40)),
        Err(Error::KeysUnavailable)
    ));
}
