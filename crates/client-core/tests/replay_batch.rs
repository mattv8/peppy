//! Atomic raw replay-page receipt and durable progress tests.
mod common;
use common::*;
use tempfile::TempDir;

fn raw(cursor: u64, envelope: &Envelope) -> RawReplayRecord {
    RawReplayRecord {
        cursor: Cursor(cursor),
        envelope_json: serde_json::to_vec(envelope).unwrap(),
    }
}

fn client(dir: &TempDir, vault: &Vault, name: &str) -> Client {
    open(&config(dir, name, vault))
}

#[test]
fn batch_gap_then_fill_advances_the_receive_frontier() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let target = client(&dir, &vault, "target");

    let report = target
        .ingest_raw_batch(&[
            RawReplayRecord {
                cursor: Cursor(2),
                envelope_json: b"two".to_vec(),
            },
            RawReplayRecord {
                cursor: Cursor(3),
                envelope_json: b"three".to_vec(),
            },
        ])
        .unwrap();
    assert_eq!((report.quarantined, report.receive_cursor), (2, Cursor(0)));

    let report = target
        .ingest_raw_batch(&[RawReplayRecord {
            cursor: Cursor(1),
            envelope_json: b"one".to_vec(),
        }])
        .unwrap();
    assert_eq!(report.receive_cursor, Cursor(3));
}

#[test]
fn batch_conflict_rolls_back_its_prefix() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let target = client(&dir, &vault, "target");
    target.ingest_raw(b"old", Cursor(2)).unwrap();

    assert_eq!(
        target.ingest_raw_batch(&[
            RawReplayRecord {
                cursor: Cursor(1),
                envelope_json: b"new".to_vec(),
            },
            RawReplayRecord {
                cursor: Cursor(2),
                envelope_json: b"different".to_vec(),
            },
        ]),
        Err(Error::Conflict)
    );
    assert_eq!(target.receive_cursor().unwrap(), Cursor(0));
    assert_eq!(target.quarantined().unwrap().len(), 1);
}

#[test]
fn batch_duplicates_and_malformed_rows_are_counted_without_double_effects() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let source = unlocked(&config(&dir, "source", &vault), &vault);
    source.capture_incoming(incoming("once", "once")).unwrap();
    let envelope = source.pending_outbox().unwrap().remove(0);
    let foreign_vault = Vault::new();
    let foreign = unlocked(&config(&dir, "foreign", &foreign_vault), &foreign_vault);
    foreign
        .capture_incoming(incoming("foreign", "foreign"))
        .unwrap();
    let foreign_envelope = foreign.pending_outbox().unwrap().remove(0);
    let target = client(&dir, &vault, "target");

    let first = target
        .ingest_raw_batch(&[
            raw(1, &envelope),
            RawReplayRecord {
                cursor: Cursor(2),
                envelope_json: b"not-json".to_vec(),
            },
            raw(3, &foreign_envelope),
        ])
        .unwrap();
    assert_eq!((first.journaled, first.quarantined), (1, 2));
    let duplicate = target.ingest_raw_batch(&[raw(1, &envelope)]).unwrap();
    assert_eq!(
        (duplicate.duplicate, duplicate.receive_cursor),
        (1, Cursor(3))
    );
    assert_eq!(
        target.apply_pending(10).unwrap().applied,
        0,
        "locked target does not apply"
    );
}

#[test]
fn batch_rejects_bounds_and_order_without_mutation() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let target = client(&dir, &vault, "target");
    let invalid = [
        vec![],
        vec![RawReplayRecord {
            cursor: Cursor(0),
            envelope_json: vec![],
        }],
        vec![
            RawReplayRecord {
                cursor: Cursor(2),
                envelope_json: vec![],
            },
            RawReplayRecord {
                cursor: Cursor(1),
                envelope_json: vec![],
            },
        ],
        vec![
            RawReplayRecord {
                cursor: Cursor(1),
                envelope_json: vec![],
            },
            RawReplayRecord {
                cursor: Cursor(1),
                envelope_json: vec![],
            },
        ],
    ];
    for records in invalid {
        assert!(target.ingest_raw_batch(&records).is_err());
    }
    let too_many: Vec<_> = (1..=MAX_REPLAY_PAGE as u64 + 1)
        .map(|cursor| RawReplayRecord {
            cursor: Cursor(cursor),
            envelope_json: vec![],
        })
        .collect();
    assert!(target.ingest_raw_batch(&too_many).is_err());
    assert!(
        target
            .ingest_raw_batch(&[RawReplayRecord {
                cursor: Cursor(1),
                envelope_json: vec![0; MAX_SNAPSHOT_PAGE_BYTES + 1],
            }])
            .is_err()
    );
    assert_eq!(target.receive_cursor().unwrap(), Cursor(0));
}

#[test]
fn batch_preserves_snapshot_collision_semantics() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let source = unlocked(&config(&dir, "source", &vault), &vault);
    source
        .capture_incoming(incoming("snapshot", "snapshot"))
        .unwrap();
    let envelope = source.pending_outbox().unwrap().remove(0);
    let target = client(&dir, &vault, "target");
    let snapshot = target
        .begin_snapshot(Cursor(1), 1, SnapshotPurpose::Resync)
        .unwrap();
    target
        .append_snapshot_raw_page(
            snapshot.generation,
            &[RawSnapshotRecord {
                cursor: Cursor(1),
                envelope_json: serde_json::to_vec(&envelope).unwrap(),
            }],
        )
        .unwrap();

    target.finish_snapshot(snapshot.generation).unwrap();
    assert_eq!(
        target
            .ingest_raw_batch(&[raw(1, &envelope)])
            .unwrap()
            .duplicate,
        1
    );
    assert_eq!(
        target.ingest_raw_batch(&[RawReplayRecord {
            cursor: Cursor(1),
            envelope_json: b"conflict".to_vec(),
        }]),
        Err(Error::Conflict)
    );
}

#[test]
fn progress_stops_at_pending_locked_rows_and_counts_waiting_keys() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let source = unlocked(&config(&dir, "source", &vault), &vault);
    source
        .capture_incoming(incoming("locked", "locked"))
        .unwrap();
    let envelope = source.pending_outbox().unwrap().remove(0);
    let target_config = config(&dir, "target", &vault);
    let target = open(&target_config);
    target
        .ingest_raw_batch(&[
            raw(1, &envelope),
            RawReplayRecord {
                cursor: Cursor(2),
                envelope_json: b"settled quarantine".to_vec(),
            },
        ])
        .unwrap();
    assert_eq!(
        target.replay_progress().unwrap(),
        ReplayProgress {
            received_through: Cursor(2),
            processed_through: Cursor(0),
            waiting_for_keys: 1,
        }
    );
    target
        .unlock(&vault.profile, &vault.header, PASSPHRASE)
        .unwrap();
    assert_eq!(target.apply_pending(10).unwrap().applied, 1);
    assert_eq!(
        target.replay_progress().unwrap().processed_through,
        Cursor(2)
    );
}

#[test]
fn progress_treats_an_empty_snapshot_as_processed_through_its_high_water() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let target = client(&dir, &vault, "target");
    let snapshot = target
        .begin_snapshot(Cursor(500), 0, SnapshotPurpose::Resync)
        .unwrap();
    target.finish_snapshot(snapshot.generation).unwrap();
    assert_eq!(
        target.replay_progress().unwrap(),
        ReplayProgress {
            received_through: Cursor(500),
            processed_through: Cursor(500),
            waiting_for_keys: 0,
        }
    );
}

#[test]
fn progress_skips_compacted_snapshot_holes_only_after_the_snapshot_drains() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let target = client(&dir, &vault, "target");
    let snapshot = target
        .begin_snapshot(Cursor(5), 2, SnapshotPurpose::Resync)
        .unwrap();
    target
        .append_snapshot_raw_page(
            snapshot.generation,
            &[
                RawSnapshotRecord {
                    cursor: Cursor(2),
                    envelope_json: b"compacted-2".to_vec(),
                },
                RawSnapshotRecord {
                    cursor: Cursor(4),
                    envelope_json: b"compacted-4".to_vec(),
                },
            ],
        )
        .unwrap();
    target.finish_snapshot(snapshot.generation).unwrap();
    assert_eq!(
        target.replay_progress().unwrap().processed_through,
        Cursor(0)
    );
    target.apply_pending(10).unwrap();
    assert_eq!(
        target.replay_progress().unwrap().processed_through,
        Cursor(5)
    );
}

#[test]
fn progress_stops_at_a_pending_retained_snapshot_record_then_advances() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let source = unlocked(&config(&dir, "source", &vault), &vault);
    source
        .capture_incoming(incoming("retained", "retained"))
        .unwrap();
    let envelope = source.pending_outbox().unwrap().remove(0);
    let target_config = config(&dir, "target", &vault);
    let target = open(&target_config);
    let snapshot = target
        .begin_snapshot(Cursor(5), 1, SnapshotPurpose::Resync)
        .unwrap();
    target
        .append_snapshot_raw_page(
            snapshot.generation,
            &[RawSnapshotRecord {
                cursor: Cursor(2),
                envelope_json: serde_json::to_vec(&envelope).unwrap(),
            }],
        )
        .unwrap();
    target.finish_snapshot(snapshot.generation).unwrap();
    target.apply_pending(10).unwrap();
    assert_eq!(
        target.replay_progress().unwrap().processed_through,
        Cursor(1)
    );
    target
        .unlock(&vault.profile, &vault.header, PASSPHRASE)
        .unwrap();
    target.apply_pending(10).unwrap();
    assert_eq!(
        target.replay_progress().unwrap().processed_through,
        Cursor(5)
    );
}

#[test]
fn progress_waits_for_a_post_snapshot_tail_gap() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let target = client(&dir, &vault, "target");
    let snapshot = target
        .begin_snapshot(Cursor(5), 0, SnapshotPurpose::Resync)
        .unwrap();
    target.finish_snapshot(snapshot.generation).unwrap();
    target
        .ingest_raw_batch(&[RawReplayRecord {
            cursor: Cursor(7),
            envelope_json: b"tail-7".to_vec(),
        }])
        .unwrap();
    assert_eq!(
        target.replay_progress().unwrap().processed_through,
        Cursor(5)
    );
    target
        .ingest_raw_batch(&[RawReplayRecord {
            cursor: Cursor(6),
            envelope_json: b"tail-6".to_vec(),
        }])
        .unwrap();
    assert_eq!(
        target.replay_progress().unwrap().processed_through,
        Cursor(7)
    );
}

#[test]
fn repeated_progress_reads_use_the_persisted_frontier_after_large_replay() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let target = client(&dir, &vault, "target");
    for page in 0..5 {
        let records: Vec<_> = (1..=MAX_REPLAY_PAGE)
            .map(|offset| RawReplayRecord {
                cursor: Cursor((page * MAX_REPLAY_PAGE + offset) as u64),
                envelope_json: b"quarantined".to_vec(),
            })
            .collect();
        target.ingest_raw_batch(&records).unwrap();
    }
    let expected = ReplayProgress {
        received_through: Cursor(1_000),
        processed_through: Cursor(1_000),
        waiting_for_keys: 0,
    };
    for _ in 0..20 {
        assert_eq!(target.replay_progress().unwrap(), expected);
    }
}
