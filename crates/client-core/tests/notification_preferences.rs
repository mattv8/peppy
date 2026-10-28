//! Vault-wide notification preference replication.
mod common;
use common::*;
use tempfile::TempDir;

fn hidden() -> NotificationPreferences {
    NotificationPreferences {
        preview: NotificationPreview::Hidden,
        ..NotificationPreferences::default()
    }
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
fn locked_default_and_clocked_adoption_are_refused() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let cfg = config(&dir, "locked", &vault);
    let locked = open(&cfg);
    assert_eq!(locked.notification_preferences().unwrap(), None);
    assert!(matches!(
        locked.set_notification_preferences(hidden()),
        Err(Error::KeysUnavailable)
    ));
    assert!(
        !locked
            .adopt_legacy_notification_preferences(hidden())
            .unwrap()
    );
    let client = unlocked(&config(&dir, "defaults", &vault), &vault);
    assert!(
        !client
            .adopt_legacy_notification_preferences(NotificationPreferences::default())
            .unwrap()
    );
}

#[test]
fn explicit_replication_tie_breaking_and_adoption_promotion() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let left = unlocked(&config(&dir, "left", &vault), &vault);
    let right = unlocked(&config(&dir, "right", &vault), &vault);
    let left_value = hidden();
    let right_value = NotificationPreferences {
        message_banners: false,
        ..Default::default()
    };
    left.set_notification_preferences(left_value.clone())
        .unwrap();
    right
        .set_notification_preferences(right_value.clone())
        .unwrap();
    let left_event = left.pending_outbox().unwrap().pop().unwrap();
    let right_event = right.pending_outbox().unwrap().pop().unwrap();
    let expected =
        if left_event.producer_device_id.to_string() > right_event.producer_device_id.to_string() {
            left_value
        } else {
            right_value
        };
    for client in [&left, &right] {
        client.ingest(&left_event, Cursor(1)).unwrap();
        client.ingest(&right_event, Cursor(2)).unwrap();
        client.apply_pending(10).unwrap();
        assert_eq!(
            client.notification_preferences().unwrap(),
            Some(expected.clone())
        );
    }
    let adopter = unlocked(&config(&dir, "adopter", &vault), &vault);
    assert!(
        adopter
            .adopt_legacy_notification_preferences(hidden())
            .unwrap()
    );
    let before = adopter.pending_outbox().unwrap().len();
    adopter.set_notification_preferences(hidden()).unwrap();
    assert_eq!(adopter.pending_outbox().unwrap().len(), before + 1);
}

#[test]
fn adoption_refuses_a_pending_preference_already_in_the_compaction_frontier() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let writer = unlocked(&config(&dir, "frontier-writer", &vault), &vault);
    let target = unlocked(&config(&dir, "frontier-target", &vault), &vault);
    writer.set_notification_preferences(hidden()).unwrap();
    let event = writer.pending_outbox().unwrap().pop().unwrap();
    target.ingest(&event, Cursor(1)).unwrap();
    target.set_server_compaction_state(true, true).unwrap();
    assert!(
        !target
            .adopt_legacy_notification_preferences(NotificationPreferences {
                message_banners: false,
                ..Default::default()
            })
            .unwrap()
    );
}

#[test]
fn snapshot_without_preference_record_never_deletes_existing_value() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let producer_config = config(&dir, "noncompact-producer", &vault);
    let producer = open(&producer_config);
    producer
        .unlock(&vault.profile, &vault.header, PASSPHRASE)
        .unwrap();
    let peer = unlocked(&config(&dir, "noncompact-peer", &vault), &vault);
    producer.set_notification_preferences(hidden()).unwrap();
    let event = producer.pending_outbox().unwrap().pop().unwrap();
    assert!(event.compaction.is_none());
    producer.ingest(&event, Cursor(1)).unwrap();
    producer.apply_pending(10).unwrap();
    peer.ingest(&event, Cursor(1)).unwrap();
    peer.apply_pending(10).unwrap();
    import_snapshot(&producer, &[], 1, 1);
    import_snapshot(&peer, &[], 1, 1);
    assert_eq!(producer.notification_preferences().unwrap(), Some(hidden()));
    assert_eq!(peer.notification_preferences().unwrap(), Some(hidden()));
}

#[test]
fn compacted_snapshot_stages_and_merges_the_vault_winner() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let writer = unlocked(&config(&dir, "compact-writer", &vault), &vault);
    let fresh = unlocked(&config(&dir, "compact-fresh", &vault), &vault);
    writer.set_notification_preferences(hidden()).unwrap();
    let event = writer.pending_outbox().unwrap().pop().unwrap();
    assert!(event.compaction.is_some());
    import_snapshot(
        &fresh,
        &[SnapshotRecord {
            cursor: Cursor(1),
            envelope: event,
        }],
        1,
        1,
    );
    assert_eq!(fresh.notification_preferences().unwrap(), Some(hidden()));
}
