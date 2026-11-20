use super::*;
use serde_json::json;

#[test]
fn overflowing_compaction_reference_does_not_recover_or_latch() {
    assert_negative_only_recovery(&["reference_overflow"]);
}

#[test]
fn malformed_or_unauthenticated_candidates_do_not_recover_or_latch() {
    assert_negative_only_recovery(&[
        "null",
        "string",
        "missing_message",
        "missing_status",
        "unknown_status",
        "reported_type",
        "explicit",
        "tampered",
        "profile",
        "vault",
        "compaction",
        "producer_overflow",
    ]);
}

fn assert_negative_only_recovery(cases: &[&str]) {
    use peppy_crypto::{create_vault_check_header, derive_root_key};

    let dir = tempfile::TempDir::new().unwrap();
    let vault_id = VaultId::new();
    let profile = KeyProfile::new(vault_id.0, 1).unwrap();
    let root = derive_root_key("correct horse battery staple", &profile).unwrap();
    let header = create_vault_check_header(&root, profile.clone()).unwrap();
    let key = derive_purpose_key(&root, &profile, KeyPurpose::Event).unwrap();
    let config = ClientConfig {
        database_path: dir.path().join("negative-only.db"),
        vault_id,
        device_id: DeviceId::new(),
    };
    let client = Client::open(config.clone(), DatabaseKey::new(&[12; 32]).unwrap()).unwrap();
    client
        .unlock(&profile, &header, "correct horse battery staple")
        .unwrap();
    {
        let store = client.lock().unwrap();
        store.conn.execute(
            "INSERT INTO projection_generations(generation,high_water,state,progress,reason) VALUES(99,100,'failed',0,'invalid_payload')",
            [],
        ).unwrap();
        set_meta(&store.conn, "decoder_revision:1", "13").unwrap();
        for (index, case) in cases.iter().enumerate() {
            let cursor = index as i64 + 1;
            let mut envelope = Envelope {
                protocol_version: PROTOCOL_VERSION,
                envelope_id: EnvelopeId::new(),
                command_id: None,
                vault_id: if *case == "vault" {
                    VaultId::new()
                } else {
                    vault_id
                },
                producer_device_id: DeviceId::new(),
                producer_sequence: SourceSequence(if *case == "producer_overflow" {
                    i64::MAX as u64 + 1
                } else {
                    2
                }),
                key_epoch: 1,
                crypto_suite: profile.crypto_suite,
                profile_fingerprint: if *case == "profile" {
                    "0".repeat(64)
                } else {
                    profile.fingerprint().unwrap()
                },
                purpose: EnvelopePurpose::Event,
                route: None,
                compaction: Some(CompactionMetadata {
                    key: vec![7; 32],
                    terminal: false,
                    supersedes: if *case == "reference_overflow" {
                        vec![CompactionReference {
                            producer_device_id: DeviceId::new(),
                            producer_sequence: SourceSequence(i64::MAX as u64 + 1),
                        }]
                    } else {
                        vec![]
                    },
                    checkpoint: false,
                }),
                origin_tag: None,
                ciphertext: Vec::new(),
            };
            let mut body = json!({
                "kind": "rcs_message_status",
                "message_id": MessageId::new(),
                "status": "read",
                "compaction": envelope.compaction,
            });
            match *case {
                "null" => body["rcs_reported"] = json!(null),
                "string" => body["rcs_reported"] = json!("true"),
                "missing_message" => {
                    body.as_object_mut().unwrap().remove("message_id");
                }
                "missing_status" => {
                    body.as_object_mut().unwrap().remove("status");
                }
                "unknown_status" => body["status"] = json!("unknown"),
                "reported_type" => body["kind"] = json!("rcs_message_reported"),
                "explicit" => body["rcs_reported"] = json!(false),
                "compaction" => {
                    body["compaction"]["key"] =
                        json!("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=")
                }
                _ => {}
            }
            envelope.ciphertext = seal_frame(
                &encrypt(
                    &key,
                    &envelope.aad_bytes().unwrap(),
                    &serde_json::to_vec(&body).unwrap(),
                )
                .unwrap(),
            );
            if *case == "tampered" {
                envelope.ciphertext[30] ^= 1;
            }
            store.conn.execute(
                "INSERT INTO journal(cursor,envelope_id,key_epoch,wire,status,reason,historical) VALUES(?,?,1,?,'quarantined','invalid_payload',1)",
                params![cursor, envelope.envelope_id.to_string(), serde_json::to_vec(&envelope).unwrap()],
            ).unwrap();
        }
    }
    drop(client);
    let reopened = Client::open(config, DatabaseKey::new(&[12; 32]).unwrap()).unwrap();
    reopened
        .unlock(&profile, &header, "correct horse battery staple")
        .unwrap();
    let store = reopened.lock().unwrap();
    for (index, case) in cases.iter().enumerate() {
        let cursor = index as i64 + 1;
        let (status, reason): (String, Option<String>) = store
            .conn
            .query_row(
                "SELECT status,reason FROM journal WHERE cursor=?",
                [cursor],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(status, "quarantined", "{case}");
        if *case == "reference_overflow" {
            assert_eq!(reason.as_deref(), Some("invalid_payload"));
        }
        assert!(
            get_meta(&store.conn, &format!("snapshot_import_origin:{cursor}"))
                .unwrap()
                .is_none(),
            "{case}"
        );
    }
    assert!(
        get_meta(&store.conn, "contact_repair_required")
            .unwrap()
            .is_none()
    );
    assert_eq!(
        get_meta(&store.conn, "decoder_revision:1")
            .unwrap()
            .as_deref(),
        Some("14")
    );
}

#[test]
fn later_epoch_unlock_recovers_its_own_authenticated_legacy_status() {
    use peppy_crypto::{create_vault_check_header, derive_root_key};

    let dir = tempfile::TempDir::new().unwrap();
    let vault_id = VaultId::new();
    let first = KeyProfile::new(vault_id.0, 1).unwrap();
    let second = KeyProfile::new(vault_id.0, 2).unwrap();
    let root1 = derive_root_key("correct horse battery staple", &first).unwrap();
    let root2 = derive_root_key("correct horse battery staple", &second).unwrap();
    let header1 = create_vault_check_header(&root1, first.clone()).unwrap();
    let header2 = create_vault_check_header(&root2, second.clone()).unwrap();
    let event_key = derive_purpose_key(&root2, &second, KeyPurpose::Event).unwrap();
    let config = ClientConfig {
        database_path: dir.path().join("later-epoch.db"),
        vault_id,
        device_id: DeviceId::new(),
    };
    let mut envelope = Envelope {
        protocol_version: PROTOCOL_VERSION,
        envelope_id: EnvelopeId::new(),
        command_id: None,
        vault_id,
        producer_device_id: DeviceId::new(),
        producer_sequence: SourceSequence(1),
        key_epoch: 2,
        crypto_suite: second.crypto_suite,
        profile_fingerprint: second.fingerprint().unwrap(),
        purpose: EnvelopePurpose::Event,
        route: None,
        compaction: Some(CompactionMetadata {
            key: vec![4; 32],
            terminal: false,
            supersedes: vec![],
            checkpoint: false,
        }),
        origin_tag: None,
        ciphertext: Vec::new(),
    };
    let raw = serde_json::to_vec(&json!({
        "kind": "rcs_message_status",
        "message_id": MessageId::new(),
        "status": "read",
        "compaction": envelope.compaction,
    }))
    .unwrap();
    envelope.ciphertext =
        seal_frame(&encrypt(&event_key, &envelope.aad_bytes().unwrap(), &raw).unwrap());
    let client = Client::open(config.clone(), DatabaseKey::new(&[13; 32]).unwrap()).unwrap();
    client
        .unlock(&first, &header1, "correct horse battery staple")
        .unwrap();
    client
        .unlock(&second, &header2, "correct horse battery staple")
        .unwrap();
    {
        let store = client.lock().unwrap();
        store.conn.execute(
            "INSERT INTO journal(cursor,envelope_id,key_epoch,wire,status,reason,historical) VALUES(1,?,2,?,'quarantined','invalid_payload',1)",
            params![envelope.envelope_id.to_string(), serde_json::to_vec(&envelope).unwrap()],
        ).unwrap();
        store.conn.execute(
            "INSERT INTO projection_generations(generation,high_water,state,progress,reason) VALUES(99,1,'failed',0,'invalid_payload')",
            [],
        ).unwrap();
        set_meta(&store.conn, "decoder_revision:2", "13").unwrap();
    }
    drop(client);
    let reopened = Client::open(config.clone(), DatabaseKey::new(&[13; 32]).unwrap()).unwrap();
    reopened
        .unlock(&first, &header1, "correct horse battery staple")
        .unwrap();
    {
        let store = reopened.lock().unwrap();
        assert_eq!(
            get_meta(&store.conn, "decoder_revision:1")
                .unwrap()
                .as_deref(),
            Some("14")
        );
        assert_eq!(
            get_meta(&store.conn, "decoder_revision:2")
                .unwrap()
                .as_deref(),
            Some("13")
        );
        assert!(
            get_meta(&store.conn, "contact_repair_required")
                .unwrap()
                .is_none()
        );
        assert!(
            get_meta(&store.conn, "snapshot_import_origin:1")
                .unwrap()
                .is_none()
        );
        let status: String = store
            .conn
            .query_row("SELECT status FROM journal WHERE cursor=1", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(status, "quarantined");
    }
    reopened
        .unlock(&second, &header2, "correct horse battery staple")
        .unwrap();
    {
        let store = reopened.lock().unwrap();
        assert_eq!(
            get_meta(&store.conn, "decoder_revision:2")
                .unwrap()
                .as_deref(),
            Some("14")
        );
        assert_eq!(
            get_meta(&store.conn, "contact_repair_required")
                .unwrap()
                .as_deref(),
            Some("upgrade")
        );
        assert!(
            get_meta(&store.conn, "snapshot_import_origin:1")
                .unwrap()
                .is_some()
        );
        let state: (String, Option<String>, bool) = store
            .conn
            .query_row(
                "SELECT status,reason,historical FROM journal WHERE cursor=1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(
            state,
            ("pending".into(), Some("snapshot_rcs_isolated".into()), true)
        );
    }
    drop(reopened);
    let reopened = Client::open(config, DatabaseKey::new(&[13; 32]).unwrap()).unwrap();
    reopened
        .unlock(&second, &header2, "correct horse battery staple")
        .unwrap();
    let store = reopened.lock().unwrap();
    assert_eq!(
        get_meta(&store.conn, "decoder_revision:2")
            .unwrap()
            .as_deref(),
        Some("14")
    );
    assert_eq!(
        get_meta(&store.conn, "contact_repair_required")
            .unwrap()
            .as_deref(),
        Some("upgrade")
    );
    assert!(
        get_meta(&store.conn, "snapshot_import_origin:1")
            .unwrap()
            .is_some()
    );
}

#[test]
fn historical_status_defaults_to_reported_for_each_status() {
    let message_id = MessageId::new();
    for (wire_status, expected) in [
        ("sending", RcsMessageStatus::Sending),
        ("sent", RcsMessageStatus::Sent),
        ("failed", RcsMessageStatus::Failed),
        ("delivered", RcsMessageStatus::Delivered),
        ("read", RcsMessageStatus::Read),
    ] {
        let raw = json!({
            "kind": "rcs_message_status",
            "message_id": message_id,
            "status": wire_status,
        });
        assert_eq!(
            serde_json::from_value::<PrivatePayload>(raw).unwrap(),
            PrivatePayload::RcsMessageStatus {
                message_id,
                status: expected,
                rcs_reported: true,
            }
        );
    }
}

#[test]
fn status_boolean_and_required_fields_remain_strict() {
    let message_id = MessageId::new();
    for reported in [false, true] {
        let raw = json!({
            "kind": "rcs_message_status",
            "message_id": message_id,
            "status": "read",
            "rcs_reported": reported,
        });
        assert_eq!(
            serde_json::from_value::<PrivatePayload>(raw).unwrap(),
            PrivatePayload::RcsMessageStatus {
                message_id,
                status: RcsMessageStatus::Read,
                rcs_reported: reported,
            }
        );
    }
    for raw in [
        json!({"kind": "rcs_message_status", "message_id": message_id, "status": "read", "rcs_reported": null}),
        json!({"kind": "rcs_message_status", "message_id": message_id, "status": "read", "rcs_reported": "true"}),
        json!({"kind": "rcs_message_status", "status": "read"}),
        json!({"kind": "rcs_message_status", "message_id": message_id}),
        json!({"kind": "rcs_message_status", "message_id": message_id, "status": null}),
        json!({"kind": "rcs_message_status", "message_id": message_id, "status": "unknown"}),
        json!({"kind": "rcs_message_reported", "message_id": message_id}),
    ] {
        assert!(
            serde_json::from_value::<PrivatePayload>(raw.clone()).is_err(),
            "{raw}"
        );
    }
}

#[test]
fn covered_newer_or_other_failed_projection_never_requeues_ordinary() {
    let dir = tempfile::TempDir::new().unwrap();
    let client = Client::open(
        ClientConfig {
            database_path: dir.path().join("covered.db"),
            vault_id: VaultId::new(),
            device_id: DeviceId::new(),
        },
        DatabaseKey::new(&[8; 32]).unwrap(),
    )
    .unwrap();
    let store = client.lock().unwrap();
    store
        .conn
        .execute(
            "INSERT INTO journal(cursor,wire,status,reason) VALUES(1,X'01','quarantined','invalid_payload')",
            [],
        )
        .unwrap();
    for (state, reason, progress) in [
        ("promoted", None, 0),
        ("staging", None, 0),
        ("failed", Some("authentication_failed"), 0),
        ("failed", Some("invalid_payload"), 1),
    ] {
        store
            .conn
            .execute("DELETE FROM projection_generations", [])
            .unwrap();
        store.conn.execute(
            "INSERT INTO projection_generations(generation,high_water,progress,state,reason) VALUES(1,1,?,?,?)",
            params![progress, state, reason],
        ).unwrap();
        snapshot_projection::recover_legacy_rcs_status(&store.conn, 1).unwrap();
        let status: String = store
            .conn
            .query_row("SELECT status FROM journal WHERE cursor=1", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(status, "quarantined", "{state}/{reason:?}/{progress}");
        assert!(
            get_meta(&store.conn, "contact_repair_required")
                .unwrap()
                .is_none()
        );
    }
}

#[test]
fn failed_fence_write_rolls_back_all_recovery_mutations() {
    let dir = tempfile::TempDir::new().unwrap();
    let client = Client::open(
        ClientConfig {
            database_path: dir.path().join("atomic.db"),
            vault_id: VaultId::new(),
            device_id: DeviceId::new(),
        },
        DatabaseKey::new(&[8; 32]).unwrap(),
    )
    .unwrap();
    let store = client.lock().unwrap();
    store.conn.execute_batch(
        "INSERT INTO journal(cursor,wire,status,reason) VALUES(1,X'01','quarantined','invalid_payload');
         INSERT INTO projection_generations(generation,high_water,progress,state,reason) VALUES(1,1,0,'failed','invalid_payload');
         CREATE TRIGGER reject_repair BEFORE INSERT ON metadata WHEN NEW.k='contact_repair_required' BEGIN SELECT RAISE(ABORT,'test interruption'); END;"
    ).unwrap();
    assert!(snapshot_projection::recover_legacy_rcs_status(&store.conn, 1).is_err());
    let row: (String, Option<String>) = store
        .conn
        .query_row(
            "SELECT status,reason FROM journal WHERE cursor=1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(row, ("quarantined".into(), Some("invalid_payload".into())));
    assert!(
        get_meta(&store.conn, "snapshot_import_origin:1")
            .unwrap()
            .is_none()
    );
    store
        .conn
        .execute_batch("DROP TRIGGER reject_repair")
        .unwrap();
    snapshot_projection::recover_legacy_rcs_status(&store.conn, 1).unwrap();
    assert!(
        get_meta(&store.conn, "snapshot_import_origin:1")
            .unwrap()
            .is_some()
    );
    assert!(
        get_meta(&store.conn, "contact_repair_required")
            .unwrap()
            .is_some()
    );
}

#[test]
fn failed_projection_recovers_gc_removed_origin_without_exposing_status() {
    recover_after_gc(true);
}

#[test]
fn repaired_snapshot_omitting_gc_removed_origin_keeps_it_fenced() {
    recover_after_gc(false);
}

fn recover_after_gc(retain: bool) {
    use peppy_crypto::{create_vault_check_header, derive_root_key};

    let dir = tempfile::TempDir::new().unwrap();
    let vault_id = VaultId::new();
    let profile = KeyProfile::new(vault_id.0, 1).unwrap();
    let root = derive_root_key("correct horse battery staple", &profile).unwrap();
    let header = create_vault_check_header(&root, profile.clone()).unwrap();
    let key = derive_purpose_key(&root, &profile, KeyPurpose::Event).unwrap();
    let config = ClientConfig {
        database_path: dir.path().join("recovery.db"),
        vault_id,
        device_id: DeviceId::new(),
    };
    let message_id = MessageId::new();
    let mut envelope = Envelope {
        protocol_version: PROTOCOL_VERSION,
        envelope_id: EnvelopeId::new(),
        command_id: None,
        vault_id,
        producer_device_id: DeviceId::new(),
        producer_sequence: SourceSequence(2),
        key_epoch: 1,
        crypto_suite: profile.crypto_suite,
        profile_fingerprint: profile.fingerprint().unwrap(),
        purpose: EnvelopePurpose::Event,
        route: None,
        compaction: Some(CompactionMetadata {
            key: vec![7; 32],
            terminal: false,
            supersedes: vec![],
            checkpoint: false,
        }),
        origin_tag: None,
        ciphertext: Vec::new(),
    };
    let raw = serde_json::to_vec(&json!({
        "kind": "rcs_message_status",
        "message_id": message_id,
        "status": "read",
        "compaction": envelope.compaction
    }))
    .unwrap();
    envelope.ciphertext = seal_frame(&encrypt(&key, &envelope.aad_bytes().unwrap(), &raw).unwrap());
    let replay = Client::open(
        ClientConfig {
            database_path: dir.path().join("replay.db"),
            vault_id,
            device_id: DeviceId::new(),
        },
        DatabaseKey::new(&[6; 32]).unwrap(),
    )
    .unwrap();
    replay
        .unlock(&profile, &header, "correct horse battery staple")
        .unwrap();
    let mut earlier = envelope.clone();
    earlier.envelope_id = EnvelopeId::new();
    earlier.producer_sequence = SourceSequence(1);
    let earlier_raw = serde_json::to_vec(&json!({
        "kind": "rcs_message_status",
        "message_id": message_id,
        "status": "delivered",
        "rcs_reported": false,
        "compaction": earlier.compaction,
    }))
    .unwrap();
    earlier.ciphertext =
        seal_frame(&encrypt(&key, &earlier.aad_bytes().unwrap(), &earlier_raw).unwrap());
    replay.ingest(&earlier, Cursor(1)).unwrap();
    replay.apply_pending(10).unwrap();
    assert_eq!(
        replay.rcs_message_status(message_id).unwrap(),
        Some(RcsMessageStatusProjection {
            status: RcsMessageStatus::Delivered,
            rcs_reported: false,
        })
    );
    replay.ingest(&envelope, Cursor(2)).unwrap();
    replay.apply_pending(10).unwrap();
    assert_eq!(
        replay.rcs_message_status(message_id).unwrap(),
        Some(RcsMessageStatusProjection {
            status: RcsMessageStatus::Read,
            rcs_reported: true,
        })
    );

    let snapshot = Client::open(
        ClientConfig {
            database_path: dir.path().join("snapshot.db"),
            vault_id,
            device_id: DeviceId::new(),
        },
        DatabaseKey::new(&[7; 32]).unwrap(),
    )
    .unwrap();
    snapshot
        .unlock(&profile, &header, "correct horse battery staple")
        .unwrap();
    let session = snapshot
        .begin_snapshot_with_compaction(Cursor(1), 1, SnapshotPurpose::Resync, Some(1))
        .unwrap();
    snapshot
        .append_snapshot_page(
            session.generation,
            &[SnapshotRecord {
                cursor: Cursor(1),
                envelope: envelope.clone(),
            }],
        )
        .unwrap();
    snapshot.finish_snapshot(session.generation).unwrap();
    for _ in 0..5 {
        snapshot.apply_pending(10).unwrap();
    }
    assert_eq!(
        snapshot.rcs_message_status(message_id).unwrap(),
        Some(RcsMessageStatusProjection {
            status: RcsMessageStatus::Read,
            rcs_reported: true,
        })
    );

    let client = Client::open(config.clone(), DatabaseKey::new(&[5; 32]).unwrap()).unwrap();
    client
        .unlock(&profile, &header, "correct horse battery staple")
        .unwrap();
    client.ingest(&envelope, Cursor(1)).unwrap();
    {
        let store = client.lock().unwrap();
        for (cursor, case) in [
            (2, "null"),
            (3, "explicit"),
            (4, "bad_status"),
            (5, "tampered"),
            (6, "metadata_mismatch"),
            (7, "wrong_vault"),
            (8, "overflow"),
            (9, "missing_epoch"),
            (10, "ordinary"),
        ] {
            let mut candidate = envelope.clone();
            candidate.envelope_id = EnvelopeId::new();
            candidate.producer_sequence = SourceSequence(if case == "overflow" {
                i64::MAX as u64 + 1
            } else {
                cursor as u64
            });
            if case == "wrong_vault" {
                candidate.vault_id = VaultId::new();
            }
            if case == "missing_epoch" {
                candidate.key_epoch = 2;
            }
            let mut body = json!({
                "kind": "rcs_message_status",
                "message_id": message_id,
                "status": "read",
                "compaction": candidate.compaction,
            });
            if case == "ordinary" {
                body["message_id"] = json!(MessageId::new());
            }
            match case {
                "null" => body["rcs_reported"] = json!(null),
                "explicit" => body["rcs_reported"] = json!(false),
                "bad_status" => body["status"] = json!("unknown"),
                "metadata_mismatch" => {
                    body["compaction"]["key"] =
                        json!("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=")
                }
                _ => {}
            }
            candidate.ciphertext = seal_frame(
                &encrypt(
                    &key,
                    &candidate.aad_bytes().unwrap(),
                    &serde_json::to_vec(&body).unwrap(),
                )
                .unwrap(),
            );
            if case == "tampered" {
                candidate.ciphertext[30] ^= 1;
            }
            store.conn.execute(
                "INSERT INTO journal(cursor,envelope_id,key_epoch,wire,status,reason,historical) VALUES(?,?,?,?,'quarantined','invalid_payload',1)",
                params![cursor, candidate.envelope_id.to_string(), candidate.key_epoch, serde_json::to_vec(&candidate).unwrap()],
            ).unwrap();
        }
        store.conn.execute_batch(
            "UPDATE journal SET status='quarantined',reason='invalid_payload',historical=1 WHERE cursor=1;
             INSERT INTO projection_generations(generation,high_water,state,progress,reason) VALUES(99,1,'failed',0,'invalid_payload');
             INSERT INTO projection_retained(generation,cursor) VALUES(99,1);
             INSERT INTO metadata(k,v) VALUES('snapshot_import_origin:1','99');
             INSERT INTO metadata(k,v) VALUES('snapshot_import_origin:11','99');
             UPDATE metadata SET v='13' WHERE k='decoder_revision:1';"
        ).unwrap();
        store.conn.execute(
            "INSERT INTO journal(cursor,wire,status,reason,historical) VALUES(11,?,'pending','snapshot_rcs_isolated',1)",
            [serde_json::to_vec(&envelope).unwrap()],
        ).unwrap();
        snapshot_projection::collect_garbage(&store.conn).unwrap();
        if !retain {
            set_meta(&store.conn, "contact_repair_required", "requested").unwrap();
        }
        assert!(
            get_meta(&store.conn, "snapshot_import_origin:1")
                .unwrap()
                .is_none()
        );
        assert_eq!(
            store
                .conn
                .query_row(
                    "SELECT COUNT(*) FROM projection_retained WHERE generation=99",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            0
        );
    }
    drop(client);
    let reopened = Client::open(config.clone(), DatabaseKey::new(&[5; 32]).unwrap()).unwrap();
    reopened
        .unlock(&profile, &header, "correct horse battery staple")
        .unwrap();
    let store = reopened.lock().unwrap();
    let row: (String, Option<String>, bool) = store
        .conn
        .query_row(
            "SELECT status,reason,historical FROM journal WHERE cursor=1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        row,
        ("pending".into(), Some("snapshot_rcs_isolated".into()), true)
    );
    let unsafe_pending: i64 = store
        .conn
        .query_row(
            "SELECT COUNT(*) FROM journal WHERE cursor BETWEEN 2 AND 9 AND status='pending'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(unsafe_pending, 0);
    let ordinary: (String, bool) = store
        .conn
        .query_row(
            "SELECT status,historical FROM journal WHERE cursor=10",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(ordinary, ("pending".into(), true));
    assert_eq!(
        get_meta(&store.conn, "snapshot_import_origin:11")
            .unwrap()
            .as_deref(),
        Some("99")
    );
    assert!(
        get_meta(&store.conn, "snapshot_import_origin:1")
            .unwrap()
            .is_some()
    );
    assert_eq!(
        get_meta(&store.conn, "contact_repair_required")
            .unwrap()
            .as_deref(),
        Some(if retain { "upgrade" } else { "requested" })
    );
    drop(store);
    assert_eq!(reopened.rcs_message_status(message_id).unwrap(), None);
    let session = reopened
        .begin_snapshot_with_compaction(
            Cursor(1),
            u64::from(retain),
            SnapshotPurpose::Resync,
            Some(1),
        )
        .unwrap();
    if retain {
        reopened
            .append_snapshot_page(
                session.generation,
                &[SnapshotRecord {
                    cursor: Cursor(1),
                    envelope,
                }],
            )
            .unwrap();
    }
    reopened.finish_snapshot(session.generation).unwrap();
    for _ in 0..5 {
        reopened.apply_pending(10).unwrap();
    }
    if retain {
        assert_eq!(
            reopened.rcs_message_status(message_id).unwrap(),
            Some(RcsMessageStatusProjection {
                status: RcsMessageStatus::Read,
                rcs_reported: true,
            })
        );
    } else {
        assert_eq!(reopened.rcs_message_status(message_id).unwrap(), None);
        let store = reopened.lock().unwrap();
        let reason: Option<String> = store
            .conn
            .query_row("SELECT reason FROM journal WHERE cursor=1", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(reason.as_deref(), Some("snapshot_rcs_isolated"));
        assert!(
            get_meta(&store.conn, "snapshot_import_origin:1")
                .unwrap()
                .is_some()
        );
    }
    assert!(!reopened.contact_repair_required().unwrap());
    drop(reopened);
    let reopened = Client::open(config, DatabaseKey::new(&[5; 32]).unwrap()).unwrap();
    reopened
        .unlock(&profile, &header, "correct horse battery staple")
        .unwrap();
    assert!(!reopened.contact_repair_required().unwrap());
}
