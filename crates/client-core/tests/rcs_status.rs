#[path = "common/mod.rs"]
mod common;

use common::*;
use peppy_crypto::{KeyPurpose, derive_purpose_key, derive_root_key, encrypt};
use rusqlite::{Connection, OptionalExtension, params};
use std::time::{SystemTime, UNIX_EPOCH};

fn publish_current_gateway(client: &Client, config: &ClientConfig) {
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    let revision = client
        .rcs_link_status(config.device_id, now_ms)
        .unwrap()
        .map_or(1, |status| status.status.capability_revision + 1);
    client
        .publish_rcs_capability(
            &RcsLinkStatusEvent {
                gateway_device_id: config.device_id,
                state: RcsCapabilityState::Linked,
                availability: true,
                health: true,
                phone_responsive: true,
                observed_at_ms: now_ms,
                capability_revision: revision,
                feature_bits: 1,
                linked_since_ms: Some(now_ms),
            },
            &[],
            now_ms,
        )
        .unwrap();
}

fn unlocked(config: &ClientConfig, vault: &Vault) -> Client {
    let client = common::unlocked(config, vault);
    publish_current_gateway(&client, config);
    client
}

fn status_rank(config: &ClientConfig, message_id: MessageId) -> Option<i64> {
    let connection = Connection::open(&config.database_path).unwrap();
    connection
        .execute_batch(
            "PRAGMA key = \"x'0707070707070707070707070707070707070707070707070707070707070707'\";",
        )
        .unwrap();
    connection
        .query_row(
            "SELECT status_rank FROM rcs_message_status WHERE message_id=?",
            [message_id.to_string()],
            |row| row.get(0),
        )
        .optional()
        .unwrap()
}

fn outbox_count(config: &ClientConfig) -> i64 {
    let connection = Connection::open(&config.database_path).unwrap();
    connection
        .execute_batch(
            "PRAGMA key = \"x'0707070707070707070707070707070707070707070707070707070707070707'\";",
        )
        .unwrap();
    connection
        .query_row("SELECT COUNT(*) FROM outbox", [], |row| row.get(0))
        .unwrap()
}

fn database(config: &ClientConfig) -> Connection {
    let connection = Connection::open(&config.database_path).unwrap();
    connection
        .execute_batch(
            "PRAGMA key = \"x'0707070707070707070707070707070707070707070707070707070707070707'\";",
        )
        .unwrap();
    connection
}

fn journal_lifecycle(
    config: &ClientConfig,
    cursor: Cursor,
) -> (Vec<u8>, String, Option<String>, bool) {
    let connection = database(config);
    let (wire, status, reason) = connection
        .query_row(
            "SELECT wire,status,reason FROM journal WHERE cursor=?",
            [i64::try_from(cursor.0).unwrap()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    let origin: bool = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM metadata WHERE k=?)",
            [format!("snapshot_import_origin:{}", cursor.0)],
            |row| row.get(0),
        )
        .unwrap();
    (wire, status, reason, origin)
}

fn status(status: RcsProjectedStatus) -> RcsStatusApplication {
    RcsStatusApplication {
        google_message_id: "provider-message".into(),
        google_conversation_id: "provider-conversation".into(),
        tmp_id: None,
        status,
    }
}

fn mapping_observation() -> RcsMappingObservation {
    RcsMappingObservation {
        google_conversation_id: "conversation-local".into(),
        google_candidate_count: 1,
        participants: vec![RcsParticipantObservation {
            identity: "peer-local".into(),
            google_participant_id: "google-peer".into(),
            identifier_type: 1,
            is_visible: true,
            is_me: false,
        }],
        conversation_type: RcsConversationType::Rcs,
        status: "eligible".into(),
        send_mode: RcsSendMode::Automatic,
        read_only: false,
        default_outgoing: true,
        selected_sim_id: "android-subscription-7".into(),
        selected_sim_own_identity: "+12025550100".into(),
        carrier_binding: NativeCarrierRouteBinding {
            carrier_route_id: "android-subscription-7".into(),
            android_subscription_id: 7,
            is_current_default_sms: true,
            own_number_raw: "+12025550100".into(),
            own_number_source: NativeOwnNumberSource::SubscriptionManagerPhoneNumber,
            route_observed_at_ms: 1_000,
            route_context_revision: 1,
            same_handset_evidence_id: "verified-handset".into(),
        },
        outgoing: NativeRcsOutgoing {
            outgoing_id: "google-outgoing".into(),
            sim_participant_id: "google-outgoing".into(),
            own_identity_raw: "+12025550100".into(),
            sim_payload: NativeRcsSimPayload {
                two: 2,
                sim_number: 7,
            },
            rcs_chats_enabled: true,
        },
        native_evidence: NativeRcsEvidence {
            lifetime: NativeRcsLifetime {
                runtime_id: "runtime".into(),
                run_generation: 1,
                link_generation: 1,
                session_generation: 1,
                context_revision: 1,
            },
            lookup_id: "lookup".into(),
            lookup_complete: true,
            google_candidate_count: 1,
            settings_revision: 1,
            settings_provenance: NativeSettingsProvenance::CurrentEvent,
            settings_observed_at_ms: 1_000,
            settings_stream_epoch: 1,
            settings_currentness: NativeSettingsCurrentness::ContinuousVerifiedFeed,
            current_settings_sims: vec![NativeRcsOutgoing {
                outgoing_id: "google-outgoing".into(),
                sim_participant_id: "google-outgoing".into(),
                own_identity_raw: "+12025550100".into(),
                sim_payload: NativeRcsSimPayload {
                    two: 2,
                    sim_number: 7,
                },
                rcs_chats_enabled: true,
            }],
            settings_validated_at_ms: 1_000,
            conversation_observed_at_ms: 1_000,
        },
        identity_region: "US".into(),
        observed_at_ms: 1_000,
        context_revision: 1,
        link_generation: 1,
        session_generation: 1,
        canonicalizer_revision: 1,
        mapping_revision: 1,
        link_state: RcsLinkState::Linked,
    }
}

fn record(body: &str) -> RcsProjectedRecord {
    RcsProjectedRecord {
        google_message_id: "provider-message".into(),
        google_conversation_id: "provider-conversation".into(),
        direction: Direction::Incoming,
        body: body.into(),
        sent_at_ms: Some(1),
        tmp_id: None,
        rcs_reported: true,
        e2ee_reported: Some(true),
    }
}

#[test]
fn rcs_conversation_metadata_converges_out_of_order_without_provider_identifiers() {
    let directory = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let first_config = config(&directory, "first", &vault);
    let second_config = config(&directory, "second", &vault);
    let first = unlocked(&first_config, &vault);
    let second = unlocked(&second_config, &vault);
    let conversation = ConversationId::new();
    let first_owner = first.begin_rcs_link_ownership(conversation, 0).unwrap();

    assert!(first
        .publish_rcs_conversation_metadata(
            conversation,
            first_owner.ownership_generation,
            None,
        )
        .unwrap());
    assert!(
        first
            .publish_rcs_conversation_metadata(
                conversation,
                first_owner.ownership_generation,
                Some(true),
            )
            .unwrap()
    );

    let mut envelopes = first.pending_outbox().unwrap();
    assert_eq!(envelopes.len(), 3);
    envelopes.reverse();
    for (index, envelope) in envelopes.iter().enumerate() {
        assert!(!serde_json::to_string(envelope).unwrap().contains("google_"));
        assert!(matches!(
            second
                .ingest(envelope, Cursor(u64::try_from(index + 1).unwrap()))
                .unwrap(),
            IngestResult::Journaled
        ));
    }
    second.apply_pending(100).unwrap();
    let metadata = second
        .rcs_conversation_metadata(conversation)
        .unwrap()
        .unwrap();
    assert_eq!(metadata.transport, RcsConversationTransport::Rcs);
    assert_eq!(metadata.rcs_encryption, Some(true));
    assert_eq!(metadata.logical_revision, 2);
    assert!(
        !serde_json::to_string(&metadata)
            .unwrap()
            .contains("provider-")
    );
}

#[test]
fn stale_metadata_generation_is_rejected_without_writing() {
    let directory = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let config = config(&directory, "client", &vault);
    let client = unlocked(&config, &vault);
    let conversation = ConversationId::new();
    let owner = client.begin_rcs_link_ownership(conversation, 0).unwrap();

    assert_eq!(
        client
            .publish_rcs_conversation_metadata(
                conversation,
                owner.ownership_generation + 1,
                Some(true),
            )
            .unwrap_err(),
        Error::InvalidRequest("stale RCS conversation metadata writer")
    );
    assert!(
        !client
            .rcs_conversation_metadata(conversation)
            .unwrap()
            .unwrap()
            .rcs_history_gap
    );
}

#[test]
fn rcs_conversation_metadata_omits_encryption_until_reported() {
    let directory = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let config = config(&directory, "client", &vault);
    let client = unlocked(&config, &vault);
    let conversation = ConversationId::new();
    let owner = client.begin_rcs_link_ownership(conversation, 0).unwrap();

    client
        .publish_rcs_conversation_metadata(conversation, owner.ownership_generation, None)
        .unwrap();
    assert_eq!(
        client
            .rcs_conversation_metadata(conversation)
            .unwrap()
            .unwrap()
            .rcs_encryption,
        None
    );
}

#[test]
fn reported_rcs_fact_replicates_without_a_delivery_status() {
    let directory = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let first_config = config(&directory, "first", &vault);
    let second_config = config(&directory, "second", &vault);
    let first = unlocked(&first_config, &vault);
    let second = unlocked(&second_config, &vault);
    let conversation = ConversationId::new();
    let owner = first.begin_rcs_link_ownership(conversation, 0).unwrap();
    first
        .ingest_rcs_projected_record(conversation, owner.ownership_generation, record("body"))
        .unwrap();
    let message_id = only_message(&first, conversation).payload.record.message_id;

    let mut server = Server::default();
    server.upload(&first);
    server.sync(&second);
    assert_eq!(second.rcs_message_reported(message_id).unwrap(), Some(true));
    assert_eq!(second.rcs_message_status(message_id).unwrap(), None);
}

#[test]
fn rcs_read_accessors_expose_google_reported_status_and_metadata_facts() {
    let directory = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let config = config(&directory, "client", &vault);
    let client = unlocked(&config, &vault);
    let conversation = ConversationId::new();
    let owner = client.begin_rcs_link_ownership(conversation, 0).unwrap();
    client
        .ingest_rcs_projected_record(conversation, owner.ownership_generation, record("body"))
        .unwrap();
    let message_id = only_message(&client, conversation)
        .payload
        .record
        .message_id;
    assert_eq!(client.rcs_message_status(message_id).unwrap(), None);
    client
        .apply_rcs_projected_status(
            conversation,
            owner.ownership_generation,
            status(RcsProjectedStatus::Delivered),
        )
        .unwrap();
    assert_eq!(
        client.rcs_message_status(message_id).unwrap(),
        Some(RcsMessageStatusProjection {
            status: RcsMessageStatus::Delivered,
            rcs_reported: true,
        })
    );
    client
        .publish_rcs_conversation_metadata(conversation, owner.ownership_generation, Some(true))
        .unwrap();
    let metadata = client
        .rcs_conversation_metadata(conversation)
        .unwrap()
        .unwrap();
    assert_eq!(metadata.rcs_encryption, Some(true));
    assert!(!metadata.rcs_history_gap);
}

#[test]
fn rcs_metadata_replicates_a_history_gap_through_cutover_and_relink() {
    let directory = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let first_config = config(&directory, "first", &vault);
    let second_config = config(&directory, "second", &vault);
    let first = unlocked(&first_config, &vault);
    let second = unlocked(&second_config, &vault);
    let conversation = ConversationId::new();
    let owner = first.begin_rcs_link_ownership(conversation, 0).unwrap();
    first
        .publish_rcs_conversation_metadata(conversation, owner.ownership_generation, None)
        .unwrap();
    first
        .record_rcs_provider_detector(
            conversation,
            owner.ownership_generation,
            RcsProviderDetector {
                provider_row_id: 1,
                provider_thread_id: 1,
                provider_type: 1,
                observed_at_ms: 1,
                observed_generation: owner.ownership_generation,
            },
        )
        .unwrap();
    first.begin_rcs_unlink_cutover(conversation).unwrap();

    let mut server = Server::default();
    server.upload(&first);
    server.sync(&second);
    assert!(
        second
            .rcs_conversation_metadata(conversation)
            .unwrap()
            .unwrap()
            .rcs_history_gap
    );

    let _relink = first.begin_rcs_link_ownership(conversation, 1).unwrap();
    assert!(
        !first
            .rcs_conversation_metadata(conversation)
            .unwrap()
            .unwrap()
            .rcs_history_gap
    );
    server.upload(&first);
    server.sync(&second);
    assert!(
        !second
            .rcs_conversation_metadata(conversation)
            .unwrap()
            .unwrap()
            .rcs_history_gap
    );
}

#[test]
fn compacted_snapshot_restores_rcs_status_and_metadata_without_provider_identifiers() {
    let directory = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let gateway = unlocked(&config(&directory, "gateway", &vault), &vault);
    let restored = unlocked(&config(&directory, "restored", &vault), &vault);
    let conversation = ConversationId::new();
    let owner = gateway.begin_rcs_link_ownership(conversation, 0).unwrap();
    gateway
        .ingest_rcs_projected_record(conversation, owner.ownership_generation, record("body"))
        .unwrap();
    gateway
        .apply_rcs_projected_status(
            conversation,
            owner.ownership_generation,
            status(RcsProjectedStatus::Read),
        )
        .unwrap();
    gateway
        .publish_rcs_conversation_metadata(conversation, owner.ownership_generation, Some(true))
        .unwrap();
    let mut server = Server::default();
    server.upload(&gateway);
    let records: Vec<SnapshotRecord> = server
        .log
        .iter()
        .enumerate()
        .filter(|(_, envelope)| envelope.compaction.is_some())
        .map(|(index, envelope)| SnapshotRecord {
            cursor: Cursor(u64::try_from(index + 1).unwrap()),
            envelope: envelope.clone(),
        })
        .collect();
    assert!(records.iter().all(|record| {
        !serde_json::to_string(&record.envelope)
            .unwrap()
            .contains("provider-")
    }));
    let session = restored
        .begin_snapshot_with_compaction(
            Cursor(u64::try_from(server.log.len()).unwrap()),
            u64::try_from(records.len()).unwrap(),
            SnapshotPurpose::Resync,
            Some(3),
        )
        .unwrap();
    restored
        .append_snapshot_page(session.generation, &records)
        .unwrap();
    restored.finish_snapshot(session.generation).unwrap();
    while restored.apply_pending(100).unwrap().snapshot_remaining > 0 {}
    let message_id = only_message(&gateway, conversation)
        .payload
        .record
        .message_id;
    assert_eq!(
        status_rank(&config(&directory, "restored", &vault), message_id),
        Some(5)
    );
    assert_eq!(
        restored.rcs_conversation_metadata(conversation).unwrap(),
        gateway.rcs_conversation_metadata(conversation).unwrap()
    );
}

#[test]
fn compacted_snapshot_restores_fact_only_reported_values() {
    for reported in [false, true] {
        let directory = tempfile::TempDir::new().unwrap();
        let vault = Vault::new();
        let gateway = unlocked(&config(&directory, "gateway", &vault), &vault);
        let restored = unlocked(&config(&directory, "restored", &vault), &vault);
        let conversation = ConversationId::new();
        let owner = gateway.begin_rcs_link_ownership(conversation, 0).unwrap();
        let mut projected = record("body");
        projected.rcs_reported = reported;
        projected.e2ee_reported = None;
        gateway
            .ingest_rcs_projected_record(conversation, owner.ownership_generation, projected)
            .unwrap();
        let mut server = Server::default();
        server.upload(&gateway);
        let records: Vec<SnapshotRecord> = server
            .log
            .iter()
            .enumerate()
            .filter(|(_, envelope)| envelope.compaction.is_some())
            .map(|(index, envelope)| SnapshotRecord {
                cursor: Cursor(u64::try_from(index + 1).unwrap()),
                envelope: envelope.clone(),
            })
            .collect();
        let session = restored
            .begin_snapshot_with_compaction(
                Cursor(u64::try_from(server.log.len()).unwrap()),
                u64::try_from(records.len()).unwrap(),
                SnapshotPurpose::Resync,
                Some(3),
            )
            .unwrap();
        restored
            .append_snapshot_page(session.generation, &records)
            .unwrap();
        restored.finish_snapshot(session.generation).unwrap();
        while restored.apply_pending(100).unwrap().snapshot_remaining > 0 {}
        let message_id = only_message(&gateway, conversation)
            .payload
            .record
            .message_id;
        assert_eq!(restored.rcs_message_status(message_id).unwrap(), None);
        assert_eq!(
            restored.rcs_message_reported(message_id).unwrap(),
            Some(reported)
        );
    }
}

#[test]
fn failed_snapshot_never_exposes_staged_rcs_status() {
    let directory = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let gateway = unlocked(&config(&directory, "gateway", &vault), &vault);
    let restored = unlocked(&config(&directory, "restored", &vault), &vault);
    let conversation = ConversationId::new();
    let owner = gateway.begin_rcs_link_ownership(conversation, 0).unwrap();
    gateway
        .ingest_rcs_projected_record(conversation, owner.ownership_generation, record("body"))
        .unwrap();
    gateway
        .apply_rcs_projected_status(
            conversation,
            owner.ownership_generation,
            status(RcsProjectedStatus::Read),
        )
        .unwrap();
    let message_id = only_message(&gateway, conversation)
        .payload
        .record
        .message_id;
    let mut server = Server::default();
    server.upload(&gateway);
    let valid = server.log.last().unwrap().clone();
    let mut invalid = valid.clone();
    invalid.ciphertext[0] ^= 1;
    let records = [
        SnapshotRecord {
            cursor: Cursor(1),
            envelope: valid,
        },
        SnapshotRecord {
            cursor: Cursor(2),
            envelope: invalid,
        },
    ];
    let session = restored
        .begin_snapshot_with_compaction(Cursor(2), 2, SnapshotPurpose::Resync, Some(7))
        .unwrap();
    restored
        .append_snapshot_page(session.generation, &records)
        .unwrap();
    restored.finish_snapshot(session.generation).unwrap();

    restored.apply_pending(1).unwrap();
    assert_eq!(restored.rcs_message_status(message_id).unwrap(), None);

    for _ in 0..4 {
        restored.apply_pending(1).unwrap();
    }
    assert_eq!(restored.rcs_message_status(message_id).unwrap(), None);
    assert_eq!(
        restored
            .snapshot_projection_status()
            .unwrap()
            .unwrap()
            .state,
        SnapshotProjectionState::Failed
    );
}

#[test]
fn failed_snapshots_keep_snapshot_only_rcs_events_isolated_after_reopen_and_gc() {
    for header_free in [false, true] {
        for kind in ["status", "reported", "metadata"] {
            let directory = tempfile::TempDir::new().unwrap();
            let vault = Vault::new();
            let gateway_config = config(&directory, "gateway", &vault);
            let restored_config = config(&directory, "restored", &vault);
            let gateway = unlocked(&gateway_config, &vault);
            if header_free {
                gateway.set_server_compaction_state(false, false).unwrap();
            }
            let restored = unlocked(&restored_config, &vault);
            let conversation = ConversationId::new();
            let owner = gateway.begin_rcs_link_ownership(conversation, 0).unwrap();
            let message_id = match kind {
                "status" => {
                    gateway
                        .ingest_rcs_projected_record(
                            conversation,
                            owner.ownership_generation,
                            record("status"),
                        )
                        .unwrap();
                    let message_id = only_message(&gateway, conversation)
                        .payload
                        .record
                        .message_id;
                    gateway
                        .apply_rcs_projected_status(
                            conversation,
                            owner.ownership_generation,
                            status(RcsProjectedStatus::Read),
                        )
                        .unwrap();
                    Some(message_id)
                }
                "reported" => {
                    gateway
                        .ingest_rcs_projected_record(
                            conversation,
                            owner.ownership_generation,
                            record("reported"),
                        )
                        .unwrap();
                    Some(
                        only_message(&gateway, conversation)
                            .payload
                            .record
                            .message_id,
                    )
                }
                "metadata" => {
                    gateway
                        .publish_rcs_conversation_metadata(
                            conversation,
                            owner.ownership_generation,
                            Some(true),
                        )
                        .unwrap();
                    None
                }
                _ => unreachable!(),
            };
            let mut server = Server::default();
            server.upload(&gateway);
            let valid = server.log.last().unwrap().clone();
            let valid_for_failure = if header_free {
                gateway.set_server_compaction_state(true, true).unwrap();
                gateway
                    .publish_rcs_conversation_metadata(
                        conversation,
                        owner.ownership_generation,
                        Some(true),
                    )
                    .unwrap();
                server.upload(&gateway);
                server.log.last().unwrap().clone()
            } else {
                valid.clone()
            };
            let mut invalid = valid_for_failure.clone();
            invalid.ciphertext[0] ^= 1;
            let session = restored
                .begin_snapshot_with_compaction(Cursor(3), 3, SnapshotPurpose::Resync, Some(91))
                .unwrap();
            restored
                .append_snapshot_page(
                    session.generation,
                    &[
                        SnapshotRecord {
                            cursor: Cursor(1),
                            envelope: valid.clone(),
                        },
                        SnapshotRecord {
                            cursor: Cursor(2),
                            envelope: valid_for_failure,
                        },
                        SnapshotRecord {
                            cursor: Cursor(3),
                            envelope: invalid,
                        },
                    ],
                )
                .unwrap();
            restored.finish_snapshot(session.generation).unwrap();
            restored.apply_pending(1).unwrap();
            let (wire, status, reason, origin) = journal_lifecycle(&restored_config, Cursor(1));
            assert_eq!(
                wire,
                serde_json::to_vec(&valid).unwrap(),
                "{kind}/{header_free}"
            );
            assert_eq!(status, "pending", "{kind}/{header_free}");
            assert_eq!(reason.as_deref(), Some("snapshot_rcs_isolated"));
            assert!(origin, "{kind}/{header_free}");
            drop(restored);
            let restored = unlocked(&restored_config, &vault);
            for _ in 0..8 {
                restored.apply_pending(1).unwrap();
            }
            assert_eq!(
                restored
                    .snapshot_projection_status()
                    .unwrap()
                    .unwrap()
                    .state,
                SnapshotProjectionState::Failed,
                "{kind}/{header_free}"
            );
            match (kind, message_id) {
                ("status", Some(message_id)) => {
                    assert_eq!(restored.rcs_message_status(message_id).unwrap(), None)
                }
                ("reported", Some(message_id)) => {
                    assert_eq!(restored.rcs_message_reported(message_id).unwrap(), None)
                }
                ("metadata", None) => {
                    assert_eq!(
                        restored.rcs_conversation_metadata(conversation).unwrap(),
                        None
                    )
                }
                _ => unreachable!(),
            }
            let (_, status, reason, origin) = journal_lifecycle(&restored_config, Cursor(1));
            assert_eq!(status, "pending", "{kind}/{header_free}");
            assert_eq!(reason.as_deref(), Some("snapshot_rcs_isolated"));
            assert!(origin, "{kind}/{header_free}");
        }
    }
}

#[test]
fn ordinary_identical_replays_release_only_the_matching_snapshot_origin() {
    let directory = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let gateway = unlocked(&config(&directory, "gateway", &vault), &vault);
    let restored_config = config(&directory, "restored", &vault);
    let restored = unlocked(&restored_config, &vault);
    let conversation = ConversationId::new();
    let owner = gateway.begin_rcs_link_ownership(conversation, 0).unwrap();
    gateway
        .ingest_rcs_projected_record(conversation, owner.ownership_generation, record("body"))
        .unwrap();
    let message_id = only_message(&gateway, conversation)
        .payload
        .record
        .message_id;
    gateway
        .apply_rcs_projected_status(
            conversation,
            owner.ownership_generation,
            status(RcsProjectedStatus::Read),
        )
        .unwrap();
    let mut server = Server::default();
    server.upload(&gateway);
    let valid = server.log.last().unwrap().clone();
    let mut invalid = valid.clone();
    invalid.ciphertext[0] ^= 1;
    let session = restored
        .begin_snapshot_with_compaction(Cursor(2), 2, SnapshotPurpose::Resync, Some(92))
        .unwrap();
    restored
        .append_snapshot_page(
            session.generation,
            &[
                SnapshotRecord {
                    cursor: Cursor(1),
                    envelope: valid.clone(),
                },
                SnapshotRecord {
                    cursor: Cursor(2),
                    envelope: invalid,
                },
            ],
        )
        .unwrap();
    restored.finish_snapshot(session.generation).unwrap();
    for _ in 0..4 {
        restored.apply_pending(1).unwrap();
    }
    let (wire, status, reason, origin) = journal_lifecycle(&restored_config, Cursor(1));
    assert_eq!(wire, serde_json::to_vec(&valid).unwrap());
    assert_eq!(status, "pending");
    assert_eq!(reason.as_deref(), Some("snapshot_rcs_isolated"));
    assert!(origin);

    let mut conflicting = valid.clone();
    conflicting.ciphertext[0] ^= 2;
    assert_eq!(
        restored.ingest(&conflicting, Cursor(3)).unwrap(),
        IngestResult::Quarantined(QuarantineReason::EnvelopeConflict)
    );
    assert!(journal_lifecycle(&restored_config, Cursor(1)).3);

    assert_eq!(
        restored.ingest(&valid, Cursor(4)).unwrap(),
        IngestResult::Duplicate
    );
    let (_, status, reason, origin) = journal_lifecycle(&restored_config, Cursor(1));
    assert_eq!(status, "pending");
    assert_eq!(reason, None);
    assert!(!origin);
    restored.apply_pending(10).unwrap();
    assert_eq!(
        restored.rcs_message_status(message_id).unwrap(),
        Some(RcsMessageStatusProjection {
            status: RcsMessageStatus::Read,
            rcs_reported: true,
        })
    );
}

fn snapshot_status_envelope(
    gateway: &Client,
    conversation: ConversationId,
    body: &str,
) -> (Envelope, MessageId) {
    let owner = gateway.begin_rcs_link_ownership(conversation, 0).unwrap();
    let mut projected = record(body);
    projected.google_message_id = body.into();
    gateway
        .ingest_rcs_projected_record(conversation, owner.ownership_generation, projected)
        .unwrap();
    let message_id = only_message(gateway, conversation)
        .payload
        .record
        .message_id;
    let mut application = status(RcsProjectedStatus::Read);
    application.google_message_id = body.into();
    gateway
        .apply_rcs_projected_status(conversation, owner.ownership_generation, application)
        .unwrap();
    let mut server = Server::default();
    server.upload(gateway);
    (server.log.last().unwrap().clone(), message_id)
}

fn import_snapshot(client: &Client, high_water: u64, generation: u64, records: &[SnapshotRecord]) {
    let session = client
        .begin_snapshot_with_compaction(
            Cursor(high_water),
            records.len() as u64,
            SnapshotPurpose::Resync,
            Some(generation),
        )
        .unwrap();
    client
        .append_snapshot_page(session.generation, records)
        .unwrap();
    client.finish_snapshot(session.generation).unwrap();
}

fn metadata_marker(config: &ClientConfig, key: &str) -> Option<String> {
    database(config)
        .query_row("SELECT v FROM metadata WHERE k=?", [key], |row| row.get(0))
        .optional()
        .unwrap()
}

fn sealed_metadata_event(
    vault: &Vault,
    producer: DeviceId,
    sequence: u64,
    metadata: RcsConversationMetadata,
) -> Envelope {
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
    let plain = serde_json::to_vec(&PrivatePayload::RcsConversationMetadata(metadata)).unwrap();
    let encrypted = encrypt(&key, &envelope.aad_bytes().unwrap(), &plain).unwrap();
    envelope.ciphertext = [encrypted.nonce.as_slice(), &encrypted.ciphertext].concat();
    envelope
}

#[test]
fn semantically_invalid_snapshot_metadata_quarantines_instead_of_becoming_isolated() {
    for invalid_writer in [false, true] {
        let directory = tempfile::TempDir::new().unwrap();
        let vault = Vault::new();
        let restored_config = config(&directory, "restored", &vault);
        let restored = unlocked(&restored_config, &vault);
        let producer = DeviceId::new();
        let metadata = RcsConversationMetadata {
            conversation_id: ConversationId::new(),
            transport: RcsConversationTransport::Rcs,
            rcs_encryption: Some(true),
            rcs_history_gap: false,
            logical_revision: if invalid_writer { 1 } else { 0 },
            writer_device_id: if invalid_writer {
                DeviceId::new().to_string()
            } else {
                producer.to_string()
            },
        };
        let invalid = sealed_metadata_event(&vault, producer, 1, metadata);
        let mut authentication_failure = invalid.clone();
        authentication_failure.ciphertext[0] ^= 1;
        import_snapshot(
            &restored,
            2,
            109,
            &[
                SnapshotRecord {
                    cursor: Cursor(1),
                    envelope: invalid,
                },
                SnapshotRecord {
                    cursor: Cursor(2),
                    envelope: authentication_failure,
                },
            ],
        );
        for _ in 0..8 {
            restored.apply_pending(1).unwrap();
        }
        let (_, status, reason, origin) = journal_lifecycle(&restored_config, Cursor(1));
        assert_eq!(status, "quarantined", "invalid_writer={invalid_writer}");
        assert_eq!(reason.as_deref(), Some("invalid_payload"));
        assert!(!origin, "invalid metadata must not remain snapshot-held");
    }
}

#[test]
fn prejournalled_ordinary_rcs_row_retained_by_failed_snapshot_remains_eligible() {
    let directory = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let gateway = unlocked(&config(&directory, "gateway", &vault), &vault);
    let restored_config = config(&directory, "restored", &vault);
    let restored = unlocked(&restored_config, &vault);
    let conversation = ConversationId::new();
    let (valid, message_id) = snapshot_status_envelope(&gateway, conversation, "ordinary-first");
    assert_eq!(
        restored.ingest(&valid, Cursor(1)).unwrap(),
        IngestResult::Journaled
    );
    let mut invalid = valid.clone();
    invalid.ciphertext[0] ^= 1;
    import_snapshot(
        &restored,
        2,
        101,
        &[
            SnapshotRecord {
                cursor: Cursor(1),
                envelope: valid,
            },
            SnapshotRecord {
                cursor: Cursor(2),
                envelope: invalid,
            },
        ],
    );
    for _ in 0..6 {
        restored.apply_pending(1).unwrap();
    }
    let (_, status, reason, origin) = journal_lifecycle(&restored_config, Cursor(1));
    assert_eq!((status.as_str(), reason, origin), ("applied", None, false));
    assert_eq!(
        restored
            .rcs_message_status(message_id)
            .unwrap()
            .unwrap()
            .status,
        RcsMessageStatus::Read
    );
    assert_eq!(
        database(&restored_config)
            .query_row(
                "SELECT COUNT(*) FROM projection_retained WHERE cursor=1",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
}

#[test]
fn same_cursor_identical_ordinary_replay_releases_only_identical_isolated_row() {
    let directory = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let gateway = unlocked(&config(&directory, "gateway", &vault), &vault);
    let restored_config = config(&directory, "restored", &vault);
    let restored = unlocked(&restored_config, &vault);
    let conversation = ConversationId::new();
    let (valid, message_id) = snapshot_status_envelope(&gateway, conversation, "same-cursor");
    let mut invalid = valid.clone();
    invalid.ciphertext[0] ^= 1;
    import_snapshot(
        &restored,
        2,
        102,
        &[
            SnapshotRecord {
                cursor: Cursor(1),
                envelope: valid.clone(),
            },
            SnapshotRecord {
                cursor: Cursor(2),
                envelope: invalid,
            },
        ],
    );
    for _ in 0..5 {
        restored.apply_pending(1).unwrap();
    }
    let before = journal_lifecycle(&restored_config, Cursor(1));
    assert_eq!(before.2.as_deref(), Some("snapshot_rcs_isolated"));
    let mut conflict = valid.clone();
    conflict.ciphertext[0] ^= 2;
    assert_eq!(restored.ingest(&conflict, Cursor(1)), Err(Error::Conflict));
    assert!(journal_lifecycle(&restored_config, Cursor(1)).3);
    assert_eq!(
        restored.ingest(&valid, Cursor(1)).unwrap(),
        IngestResult::Duplicate
    );
    let after = journal_lifecycle(&restored_config, Cursor(1));
    assert_eq!(after.0, before.0);
    assert_eq!(
        (after.1.as_str(), after.2, after.3),
        ("pending", None, false)
    );
    restored.apply_pending(10).unwrap();
    assert_eq!(
        restored
            .rcs_message_status(message_id)
            .unwrap()
            .unwrap()
            .status,
        RcsMessageStatus::Read
    );
}

#[test]
fn successful_same_cursor_reimport_releases_compactable_and_header_free_rows_after_promotion() {
    for header_free in [false, true] {
        let directory = tempfile::TempDir::new().unwrap();
        let vault = Vault::new();
        let gateway = unlocked(&config(&directory, "gateway", &vault), &vault);
        if header_free {
            gateway.set_server_compaction_state(false, false).unwrap();
        }
        let second_gateway = unlocked(&config(&directory, "second-gateway", &vault), &vault);
        let restored_config = config(&directory, "restored", &vault);
        let restored = unlocked(&restored_config, &vault);
        let conversation = ConversationId::new();
        let (valid, message_id) = snapshot_status_envelope(&gateway, conversation, "reimport");
        let (second, _) =
            snapshot_status_envelope(&second_gateway, ConversationId::new(), "promotion-barrier");
        let mut invalid = if header_free {
            gateway.set_server_compaction_state(true, true).unwrap();
            let failure_conversation = ConversationId::new();
            let owner = gateway
                .begin_rcs_link_ownership(failure_conversation, 0)
                .unwrap();
            gateway
                .publish_rcs_conversation_metadata(
                    failure_conversation,
                    owner.ownership_generation,
                    Some(true),
                )
                .unwrap();
            let mut server = Server::default();
            server.upload(&gateway);
            server.log.last().unwrap().clone()
        } else {
            valid.clone()
        };
        invalid.ciphertext[0] ^= 1;
        import_snapshot(
            &restored,
            2,
            103,
            &[
                SnapshotRecord {
                    cursor: Cursor(1),
                    envelope: valid.clone(),
                },
                SnapshotRecord {
                    cursor: Cursor(2),
                    envelope: invalid,
                },
            ],
        );
        for _ in 0..5 {
            restored.apply_pending(1).unwrap();
        }
        assert!(journal_lifecycle(&restored_config, Cursor(1)).3);
        import_snapshot(
            &restored,
            3,
            104,
            &[
                SnapshotRecord {
                    cursor: Cursor(1),
                    envelope: valid.clone(),
                },
                SnapshotRecord {
                    cursor: Cursor(3),
                    envelope: second,
                },
            ],
        );
        restored.apply_pending(1).unwrap();
        assert_eq!(
            journal_lifecycle(&restored_config, Cursor(1)).2.as_deref(),
            Some("snapshot_rcs_isolated"),
            "must not release before promotion/{header_free}"
        );
        for _ in 0..6 {
            restored.apply_pending(1).unwrap();
        }
        let (_, status, reason, origin) = journal_lifecycle(&restored_config, Cursor(1));
        assert_eq!(
            (status.as_str(), reason, origin),
            ("applied", None, false),
            "{header_free}"
        );
        assert_eq!(
            restored
                .rcs_message_status(message_id)
                .unwrap()
                .unwrap()
                .status,
            RcsMessageStatus::Read
        );
    }
}

#[test]
fn newer_snapshot_omitting_old_isolated_cursor_does_not_release_or_starve_ordinary_rcs() {
    let directory = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let first_gateway = unlocked(&config(&directory, "first-gateway", &vault), &vault);
    let second_gateway = unlocked(&config(&directory, "second-gateway", &vault), &vault);
    let restored_config = config(&directory, "restored", &vault);
    let restored = unlocked(&restored_config, &vault);
    let old_conversation = ConversationId::new();
    let new_conversation = ConversationId::new();
    let (old, _) = snapshot_status_envelope(&first_gateway, old_conversation, "old-held");
    let (new, new_message) =
        snapshot_status_envelope(&second_gateway, new_conversation, "new-retained");
    let mut invalid = old.clone();
    invalid.ciphertext[0] ^= 1;
    import_snapshot(
        &restored,
        2,
        105,
        &[
            SnapshotRecord {
                cursor: Cursor(1),
                envelope: old.clone(),
            },
            SnapshotRecord {
                cursor: Cursor(2),
                envelope: invalid,
            },
        ],
    );
    for _ in 0..5 {
        restored.apply_pending(1).unwrap();
    }
    let old_wire = journal_lifecycle(&restored_config, Cursor(1)).0;
    import_snapshot(
        &restored,
        3,
        106,
        &[SnapshotRecord {
            cursor: Cursor(3),
            envelope: new.clone(),
        }],
    );
    for _ in 0..6 {
        restored.apply_pending(1).unwrap();
    }
    let old_after = journal_lifecycle(&restored_config, Cursor(1));
    assert_eq!(old_after.0, old_wire);
    assert_eq!(
        (old_after.1.as_str(), old_after.2.as_deref(), old_after.3),
        ("pending", Some("snapshot_rcs_isolated"), true)
    );
    assert_eq!(
        restored
            .rcs_message_status(new_message)
            .unwrap()
            .unwrap()
            .status,
        RcsMessageStatus::Read
    );
    let ordinary_conversation = ConversationId::new();
    let (ordinary, ordinary_message) =
        snapshot_status_envelope(&second_gateway, ordinary_conversation, "ordinary-later");
    assert_eq!(
        restored.ingest(&ordinary, Cursor(4)).unwrap(),
        IngestResult::Journaled
    );
    restored.apply_pending(1).unwrap();
    assert_eq!(
        restored
            .rcs_message_status(ordinary_message)
            .unwrap()
            .unwrap()
            .status,
        RcsMessageStatus::Read
    );
    assert!(journal_lifecycle(&restored_config, Cursor(1)).3);
    assert!(!restored.sync_work_status().unwrap().pending_apply);
    let processed = metadata_marker(&restored_config, "processed_cursor")
        .unwrap()
        .parse::<u64>()
        .unwrap();
    assert!(
        processed < 1,
        "held cursor must remain a processed-frontier barrier"
    );
    drop(restored);
    let restored = common::unlocked(&restored_config, &vault);
    assert!(!restored.sync_work_status().unwrap().pending_apply);
    let (_, held_status, held_reason, held_origin) = journal_lifecycle(&restored_config, Cursor(1));
    assert_eq!(
        (held_status.as_str(), held_reason.as_deref(), held_origin),
        ("pending", Some("snapshot_rcs_isolated"), true)
    );
}

#[test]
fn published_undrained_equal_replay_observation_survives_reopen_and_releases_on_drain() {
    let directory = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let gateway = unlocked(&config(&directory, "gateway", &vault), &vault);
    let restored_config = config(&directory, "restored", &vault);
    let restored = unlocked(&restored_config, &vault);
    let conversation = ConversationId::new();
    let (valid, message_id) = snapshot_status_envelope(&gateway, conversation, "undrained");
    import_snapshot(
        &restored,
        1,
        107,
        &[SnapshotRecord {
            cursor: Cursor(1),
            envelope: valid.clone(),
        }],
    );
    let outbox_before = outbox_count(&restored_config);
    assert_eq!(
        restored.ingest(&valid, Cursor(1)).unwrap(),
        IngestResult::Duplicate
    );
    assert!(metadata_marker(&restored_config, "snapshot_import_observed:1").is_some());
    assert_eq!(outbox_count(&restored_config), outbox_before);
    drop(restored);
    let restored = common::unlocked(&restored_config, &vault);
    assert!(metadata_marker(&restored_config, "snapshot_import_observed:1").is_some());
    restored.apply_pending(1).unwrap();
    assert!(!journal_lifecycle(&restored_config, Cursor(1)).3);
    assert_eq!(journal_lifecycle(&restored_config, Cursor(1)).2, None);
    assert!(metadata_marker(&restored_config, "snapshot_import_observed:1").is_none());
    for _ in 0..4 {
        restored.apply_pending(1).unwrap();
    }
    assert_eq!(
        restored
            .rcs_message_status(message_id)
            .unwrap()
            .unwrap()
            .status,
        RcsMessageStatus::Read
    );
    assert_eq!(outbox_count(&restored_config), outbox_before);

    let command_directory = tempfile::TempDir::new().unwrap();
    let command_vault = Vault::new();
    let sender_config = config(&command_directory, "sender", &command_vault);
    let gateway_config = config(&command_directory, "gateway", &command_vault);
    let sender = unlocked(&sender_config, &command_vault);
    let queued = sender
        .queue_send(
            sms(ConversationId::new(), "published-undrained-command"),
            route(&gateway_config),
        )
        .unwrap();
    let command_envelope = sender
        .pending_outbox()
        .unwrap()
        .into_iter()
        .find(|envelope| envelope.command_id == Some(queued.command_id))
        .unwrap();
    import_snapshot(
        &sender,
        1,
        108,
        &[SnapshotRecord {
            cursor: Cursor(1),
            envelope: command_envelope.clone(),
        }],
    );
    let command_outbox_before = sender.pending_outbox().unwrap().len();
    assert_eq!(
        sender.ingest(&command_envelope, Cursor(1)).unwrap(),
        IngestResult::Duplicate
    );
    assert_eq!(
        sender.pending_outbox().unwrap().len(),
        command_outbox_before
    );
    assert!(
        sender
            .pending_outbox()
            .unwrap()
            .iter()
            .any(|envelope| { envelope.command_id == Some(queued.command_id) })
    );
    assert!(metadata_marker(&sender_config, "snapshot_import_observed:1").is_some());
    sender.apply_pending(1).unwrap();
    assert!(
        !sender
            .pending_outbox()
            .unwrap()
            .iter()
            .any(|envelope| { envelope.command_id == Some(queued.command_id) })
    );
    assert_eq!(
        sender.pending_outbox().unwrap().len(),
        command_outbox_before - 1
    );
    let (_, command_status, command_reason, command_origin) =
        journal_lifecycle(&sender_config, Cursor(1));
    assert_eq!(
        (command_status.as_str(), command_reason, command_origin),
        ("duplicate", None, false)
    );

    let second_command = sender
        .queue_send(
            sms(ConversationId::new(), "resolved-origin-command"),
            route(&gateway_config),
        )
        .unwrap();
    let second_envelope = sender
        .pending_outbox()
        .unwrap()
        .into_iter()
        .find(|envelope| envelope.command_id == Some(second_command.command_id))
        .unwrap();
    import_snapshot(
        &sender,
        2,
        110,
        &[SnapshotRecord {
            cursor: Cursor(2),
            envelope: second_envelope,
        }],
    );
    assert!(
        sender
            .pending_outbox()
            .unwrap()
            .iter()
            .any(|envelope| { envelope.command_id == Some(second_command.command_id) })
    );
    for _ in 0..3 {
        sender.apply_pending(1).unwrap();
    }
    assert!(
        !sender
            .pending_outbox()
            .unwrap()
            .iter()
            .any(|envelope| { envelope.command_id == Some(second_command.command_id) })
    );
    let (_, resolved_status, resolved_reason, resolved_origin) =
        journal_lifecycle(&sender_config, Cursor(2));
    assert_eq!(
        (resolved_status.as_str(), resolved_reason, resolved_origin),
        ("duplicate", None, false),
        "resolved header-free command origin must be collected"
    );
}

#[test]
fn authoritative_snapshot_keeps_unuploaded_local_rcs_evidence_and_metadata() {
    let directory = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let gateway = unlocked(&config(&directory, "gateway", &vault), &vault);
    let restored_config = config(&directory, "restored", &vault);
    let restored = unlocked(&restored_config, &vault);
    let conversation = ConversationId::new();
    let local_owner = restored.begin_rcs_link_ownership(conversation, 0).unwrap();
    restored
        .ingest_rcs_projected_record(
            conversation,
            local_owner.ownership_generation,
            record("local"),
        )
        .unwrap();
    restored
        .apply_rcs_projected_status(
            conversation,
            local_owner.ownership_generation,
            status(RcsProjectedStatus::Read),
        )
        .unwrap();
    let local_message = only_message(&restored, conversation)
        .payload
        .record
        .message_id;
    restored
        .publish_rcs_conversation_metadata(
            conversation,
            local_owner.ownership_generation,
            Some(false),
        )
        .unwrap();
    let local_metadata = restored.rcs_conversation_metadata(conversation).unwrap();
    assert_eq!(local_metadata.as_ref().unwrap().logical_revision, 3);
    assert_eq!(local_metadata.as_ref().unwrap().rcs_encryption, Some(false));
    let outbox_before_snapshot = outbox_count(&restored_config);

    let remote_owner = gateway.begin_rcs_link_ownership(conversation, 0).unwrap();
    let mut remote = record("remote");
    remote.google_message_id = "remote-message".into();
    gateway
        .ingest_rcs_projected_record(conversation, remote_owner.ownership_generation, remote)
        .unwrap();
    let mut server = Server::default();
    server.upload(&gateway);
    let records: Vec<SnapshotRecord> = server
        .log
        .iter()
        .enumerate()
        .filter(|(_, envelope)| envelope.compaction.is_some())
        .map(|(index, envelope)| SnapshotRecord {
            cursor: Cursor(u64::try_from(index + 1).unwrap()),
            envelope: envelope.clone(),
        })
        .collect();
    let session = restored
        .begin_snapshot_with_compaction(
            Cursor(u64::try_from(server.log.len()).unwrap()),
            u64::try_from(records.len()).unwrap(),
            SnapshotPurpose::Resync,
            Some(8),
        )
        .unwrap();
    restored
        .append_snapshot_page(session.generation, &records)
        .unwrap();
    restored.finish_snapshot(session.generation).unwrap();
    while restored.apply_pending(100).unwrap().snapshot_remaining > 0 {}

    assert_eq!(
        restored.rcs_message_status(local_message).unwrap(),
        Some(RcsMessageStatusProjection {
            status: RcsMessageStatus::Read,
            rcs_reported: true,
        })
    );
    assert_eq!(
        restored.rcs_conversation_metadata(conversation).unwrap(),
        local_metadata
    );
    assert_eq!(outbox_count(&restored_config), outbox_before_snapshot);

    server.upload(&restored);
    server.sync(&restored);
    assert_eq!(
        restored.rcs_message_status(local_message).unwrap(),
        Some(RcsMessageStatusProjection {
            status: RcsMessageStatus::Read,
            rcs_reported: true,
        })
    );
    assert_eq!(
        restored.rcs_conversation_metadata(conversation).unwrap(),
        local_metadata
    );
    assert_eq!(outbox_count(&restored_config), outbox_before_snapshot);
    drop(restored);
    let restored = common::unlocked(&restored_config, &vault);
    assert_eq!(
        restored.rcs_message_status(local_message).unwrap(),
        Some(RcsMessageStatusProjection {
            status: RcsMessageStatus::Read,
            rcs_reported: true,
        })
    );
    assert_eq!(
        restored.rcs_conversation_metadata(conversation).unwrap(),
        local_metadata
    );
    assert_eq!(outbox_count(&restored_config), outbox_before_snapshot);
}

#[test]
fn duplicate_report_keeps_compacted_true_fact_and_does_not_republish_unchanged_metadata() {
    let directory = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let gateway_config = config(&directory, "gateway", &vault);
    let gateway = unlocked(&gateway_config, &vault);
    let restored = unlocked(&config(&directory, "restored", &vault), &vault);
    let conversation = ConversationId::new();
    let owner = gateway.begin_rcs_link_ownership(conversation, 0).unwrap();
    let mut projected = record("body");
    projected.rcs_reported = false;
    projected.e2ee_reported = Some(true);
    gateway
        .ingest_rcs_projected_record(conversation, owner.ownership_generation, projected.clone())
        .unwrap();
    projected.rcs_reported = true;
    assert_eq!(
        gateway
            .ingest_rcs_projected_record(
                conversation,
                owner.ownership_generation,
                projected.clone()
            )
            .unwrap(),
        RcsReceiveOutcome::Duplicate
    );
    let after_true = outbox_count(&gateway_config);
    assert_eq!(
        gateway
            .ingest_rcs_projected_record(
                conversation,
                owner.ownership_generation,
                projected.clone()
            )
            .unwrap(),
        RcsReceiveOutcome::Duplicate
    );
    assert_eq!(outbox_count(&gateway_config), after_true);
    projected.rcs_reported = false;
    assert_eq!(
        gateway
            .ingest_rcs_projected_record(conversation, owner.ownership_generation, projected)
            .unwrap(),
        RcsReceiveOutcome::Duplicate
    );
    assert_eq!(outbox_count(&gateway_config), after_true);

    let mut server = Server::default();
    server.upload(&gateway);
    let superseded = |envelope: &Envelope| {
        server
            .log
            .iter()
            .filter_map(|candidate| candidate.compaction.as_ref())
            .flat_map(|metadata| metadata.supersedes.iter())
            .any(|reference| {
                reference.producer_device_id == envelope.producer_device_id
                    && reference.producer_sequence == envelope.producer_sequence
            })
    };
    let compactable = server
        .log
        .iter()
        .filter(|envelope| envelope.compaction.is_some())
        .count();
    let records: Vec<SnapshotRecord> = server
        .log
        .iter()
        .enumerate()
        .filter(|(_, envelope)| envelope.compaction.is_some() && !superseded(envelope))
        .map(|(index, envelope)| SnapshotRecord {
            cursor: Cursor(u64::try_from(index + 1).unwrap()),
            envelope: envelope.clone(),
        })
        .collect();
    assert!(
        records.len() < compactable,
        "snapshot must drop superseded facts"
    );
    let session = restored
        .begin_snapshot_with_compaction(
            Cursor(u64::try_from(server.log.len()).unwrap()),
            u64::try_from(records.len()).unwrap(),
            SnapshotPurpose::Resync,
            Some(3),
        )
        .unwrap();
    restored
        .append_snapshot_page(session.generation, &records)
        .unwrap();
    restored.finish_snapshot(session.generation).unwrap();
    while restored.apply_pending(100).unwrap().snapshot_remaining > 0 {}
    let message_id = only_message(&gateway, conversation)
        .payload
        .record
        .message_id;
    assert_eq!(
        restored.rcs_message_reported(message_id).unwrap(),
        Some(true)
    );
}

#[test]
fn snapshot_promotion_merges_rcs_evidence_without_pruning_absent_local_rows() {
    let directory = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let client_config = config(&directory, "client", &vault);
    let client = unlocked(&client_config, &vault);
    let local_message = MessageId::new();
    let fact_before_message = MessageId::new();
    let staged_message = MessageId::new();
    let conversation = ConversationId::new();
    let writer = client_config.device_id.to_string();
    let connection = Connection::open(&client_config.database_path).unwrap();
    connection
        .execute_batch(
            "PRAGMA key = \"x'0707070707070707070707070707070707070707070707070707070707070707'\";",
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO projection_generations(generation,high_water,state) VALUES(1,?,'staging')",
            [i64::MAX],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO rcs_message_status(message_id,status_rank,rcs_reported) VALUES(?,?,?),(?,?,?)",
            params![
                local_message.to_string(),
                5,
                true,
                fact_before_message.to_string(),
                0,
                false,
            ],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO projection_rcs_message_status(generation,message_id,status_rank,rcs_reported) VALUES(?,?,?,?),(?,?,?,?)",
            params![
                1,
                local_message.to_string(),
                2,
                false,
                1,
                staged_message.to_string(),
                2,
                true,
            ],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO rcs_conversation_metadata(conversation_id,transport,rcs_encryption,rcs_history_gap,logical_revision,writer_device_id,producer_device_id,source_sequence) VALUES(?,'rcs',?,?,?,?,?,?)",
            params![conversation.to_string(), true, true, 2, writer, client_config.device_id.to_string(), 2],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO projection_rcs_conversation_metadata(generation,conversation_id,rcs_encryption,rcs_history_gap,logical_revision,writer_device_id,producer_device_id,source_sequence) VALUES(?,?,?,?,?,?,?,?)",
            params![1, conversation.to_string(), false, false, 1, client_config.device_id.to_string(), client_config.device_id.to_string(), 1],
        )
        .unwrap();
    drop(connection);

    client.apply_pending(100).unwrap();

    assert_eq!(
        client.rcs_message_status(local_message).unwrap(),
        Some(RcsMessageStatusProjection {
            status: RcsMessageStatus::Read,
            rcs_reported: true,
        })
    );
    assert_eq!(
        client.rcs_message_reported(fact_before_message).unwrap(),
        Some(false)
    );
    assert_eq!(
        client.rcs_message_status(staged_message).unwrap(),
        Some(RcsMessageStatusProjection {
            status: RcsMessageStatus::Sent,
            rcs_reported: true,
        })
    );
    let metadata = client
        .rcs_conversation_metadata(conversation)
        .unwrap()
        .unwrap();
    assert_eq!(metadata.rcs_encryption, Some(true));
    assert!(metadata.rcs_history_gap);
    assert_eq!(metadata.logical_revision, 2);
}

#[test]
fn deterministic_rcs_message_ids_converge_without_exposing_provider_identity() {
    let directory = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let first_config = config(&directory, "first", &vault);
    let second_config = config(&directory, "second", &vault);
    let first = unlocked(&first_config, &vault);
    let second = unlocked(&second_config, &vault);
    let conversation = ConversationId::new();
    let first_owner = first.begin_rcs_link_ownership(conversation, 0).unwrap();
    let second_owner = second.begin_rcs_link_ownership(conversation, 0).unwrap();

    assert_eq!(
        first
            .ingest_rcs_projected_record(
                conversation,
                first_owner.ownership_generation,
                record("one")
            )
            .unwrap(),
        RcsReceiveOutcome::Inserted
    );
    assert_eq!(
        second
            .ingest_rcs_projected_record(
                conversation,
                second_owner.ownership_generation,
                record("one")
            )
            .unwrap(),
        RcsReceiveOutcome::Inserted
    );
    let first_id = only_message(&first, conversation).payload.record.message_id;
    let second_id = only_message(&second, conversation)
        .payload
        .record
        .message_id;
    assert_eq!(first_id, second_id);
    assert_ne!(first_id.to_string(), "provider-message");
    assert_ne!(first_id.to_string(), "provider-conversation");

    let other_vault = Vault::new();
    let other_config = config(&directory, "other", &other_vault);
    let other = unlocked(&other_config, &other_vault);
    let other_owner = other.begin_rcs_link_ownership(conversation, 0).unwrap();
    other
        .ingest_rcs_projected_record(
            conversation,
            other_owner.ownership_generation,
            record("one"),
        )
        .unwrap();
    assert_ne!(
        first_id,
        only_message(&other, conversation).payload.record.message_id
    );
}

#[test]
fn replicated_rcs_messages_are_idempotent() {
    let directory = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let first_config = config(&directory, "first", &vault);
    let second_config = config(&directory, "second", &vault);
    let first = unlocked(&first_config, &vault);
    let second = unlocked(&second_config, &vault);
    let conversation = ConversationId::new();
    let first_owner = first.begin_rcs_link_ownership(conversation, 0).unwrap();
    let second_owner = second.begin_rcs_link_ownership(conversation, 0).unwrap();
    first
        .ingest_rcs_projected_record(
            conversation,
            first_owner.ownership_generation,
            record("one"),
        )
        .unwrap();
    second
        .ingest_rcs_projected_record(
            conversation,
            second_owner.ownership_generation,
            record("one"),
        )
        .unwrap();

    let mut server = Server::default();
    server.upload(&first);
    server.upload(&second);
    server.sync(&first);
    server.sync(&second);
    assert_eq!(first.messages(conversation).unwrap().len(), 1);
    assert_eq!(second.messages(conversation).unwrap().len(), 1);
}

#[test]
fn incoming_rcs_replay_stale_writer_and_page_floor_are_rejected_without_rows() {
    let directory = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let config = config(&directory, "client", &vault);
    let client = unlocked(&config, &vault);
    let conversation = ConversationId::new();
    let owner = client.begin_rcs_link_ownership(conversation, 10).unwrap();

    assert_eq!(
        client
            .ingest_rcs_projected_record(conversation, owner.ownership_generation, record("one"))
            .unwrap(),
        RcsReceiveOutcome::Inserted
    );
    assert_eq!(
        client
            .ingest_rcs_projected_record(conversation, owner.ownership_generation, record("one"))
            .unwrap(),
        RcsReceiveOutcome::Duplicate
    );
    assert_eq!(client.messages(conversation).unwrap().len(), 1);
    assert_eq!(
        client
            .ingest_rcs_projected_record(
                conversation,
                owner.ownership_generation + 1,
                record("two")
            )
            .unwrap(),
        RcsReceiveOutcome::Rejected(RcsReceiveCode::OwnershipStale)
    );
    let mut page = record("page");
    page.google_message_id = "page-message".into();
    page.sent_at_ms = Some(10);
    assert_eq!(
        client
            .ingest_rcs_projected_observation(
                conversation,
                owner.ownership_generation,
                page,
                RcsObservationProvenance::Historical,
            )
            .unwrap(),
        RcsReceiveOutcome::Rejected(RcsReceiveCode::ProviderFloor)
    );
    assert_eq!(client.messages(conversation).unwrap().len(), 1);
}

#[test]
fn page_records_are_imported_while_live_records_remain_unseen() {
    let directory = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let config = config(&directory, "client", &vault);
    let client = unlocked(&config, &vault);
    let conversation = ConversationId::new();
    let owner = client.begin_rcs_link_ownership(conversation, 0).unwrap();
    client
        .ingest_rcs_projected_record(conversation, owner.ownership_generation, record("live"))
        .unwrap();
    let mut page = record("page");
    page.google_message_id = "page-message".into();
    page.sent_at_ms = Some(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64
            + 1,
    );
    client
        .ingest_rcs_projected_observation(
            conversation,
            owner.ownership_generation,
            page,
            RcsObservationProvenance::Historical,
        )
        .unwrap();

    let messages = client.messages(conversation).unwrap();
    assert_eq!(messages.len(), 2);
    assert!(
        messages
            .iter()
            .any(|message| message.payload.body == "live" && !message.seen)
    );
    assert!(
        messages
            .iter()
            .any(|message| message.payload.body == "page" && message.seen)
    );
}

#[test]
fn rcs_status_lattice_is_monotonic() {
    let directory = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let config = config(&directory, "client", &vault);
    let client = unlocked(&config, &vault);
    let conversation = ConversationId::new();
    let owner = client.begin_rcs_link_ownership(conversation, 0).unwrap();
    client
        .ingest_rcs_projected_record(conversation, owner.ownership_generation, record("live"))
        .unwrap();
    for projected_status in [
        RcsProjectedStatus::Sending,
        RcsProjectedStatus::Sent,
        RcsProjectedStatus::Failed,
        RcsProjectedStatus::Delivered,
        RcsProjectedStatus::Read,
    ] {
        assert_eq!(
            client
                .apply_rcs_projected_status(
                    conversation,
                    owner.ownership_generation,
                    status(projected_status),
                )
                .unwrap(),
            RcsReceiveOutcome::StatusApplied
        );
    }
    assert_eq!(
        client
            .apply_rcs_projected_status(
                conversation,
                owner.ownership_generation,
                status(RcsProjectedStatus::Failed),
            )
            .unwrap(),
        RcsReceiveOutcome::StatusUnchanged
    );
}

#[test]
fn statuses_converge_and_rejections_leave_no_status_row() {
    let directory = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let first_config = config(&directory, "first", &vault);
    let second_config = config(&directory, "second", &vault);
    let first = unlocked(&first_config, &vault);
    let second = unlocked(&second_config, &vault);
    let conversation = ConversationId::new();
    let first_owner = first.begin_rcs_link_ownership(conversation, 0).unwrap();
    let second_owner = second.begin_rcs_link_ownership(conversation, 0).unwrap();
    for (client, generation) in [
        (&first, first_owner.ownership_generation),
        (&second, second_owner.ownership_generation),
    ] {
        client
            .ingest_rcs_projected_record(conversation, generation, record("body"))
            .unwrap();
    }
    let message_id = only_message(&first, conversation).payload.record.message_id;
    first
        .apply_rcs_projected_status(
            conversation,
            first_owner.ownership_generation,
            status(RcsProjectedStatus::Failed),
        )
        .unwrap();
    first
        .apply_rcs_projected_status(
            conversation,
            first_owner.ownership_generation,
            status(RcsProjectedStatus::Read),
        )
        .unwrap();
    second
        .apply_rcs_projected_status(
            conversation,
            second_owner.ownership_generation,
            status(RcsProjectedStatus::Delivered),
        )
        .unwrap();
    second
        .apply_rcs_projected_status(
            conversation,
            second_owner.ownership_generation,
            status(RcsProjectedStatus::Sent),
        )
        .unwrap();
    let mut server = Server::default();
    server.upload(&first);
    server.upload(&second);
    server.sync(&first);
    server.sync(&second);
    assert_eq!(status_rank(&first_config, message_id), Some(5));
    assert_eq!(status_rank(&second_config, message_id), Some(5));
    assert_eq!(
        first
            .apply_rcs_projected_status(
                conversation,
                first_owner.ownership_generation + 1,
                status(RcsProjectedStatus::Read)
            )
            .unwrap(),
        RcsReceiveOutcome::Rejected(RcsReceiveCode::OwnershipStale)
    );
    assert_eq!(
        first
            .apply_rcs_projected_status(
                conversation,
                first_owner.ownership_generation,
                RcsStatusApplication {
                    google_message_id: "unknown".into(),
                    ..status(RcsProjectedStatus::Read)
                }
            )
            .unwrap(),
        RcsReceiveOutcome::Rejected(RcsReceiveCode::GatewayUnavailable)
    );
    assert_eq!(status_rank(&first_config, message_id), Some(5));
}

#[test]
fn unmatched_foreign_outgoing_is_inserted_once_without_a_command() {
    let directory = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let config = config(&directory, "client", &vault);
    let client = unlocked(&config, &vault);
    let conversation = ConversationId::new();
    let owner = client.begin_rcs_link_ownership(conversation, 0).unwrap();
    let mut foreign = record("foreign");
    foreign.direction = Direction::Outgoing;
    assert_eq!(
        client
            .ingest_rcs_projected_record(conversation, owner.ownership_generation, foreign.clone())
            .unwrap(),
        RcsReceiveOutcome::UnmatchedOutgoing
    );
    assert_eq!(
        client
            .ingest_rcs_projected_record(conversation, owner.ownership_generation, foreign)
            .unwrap(),
        RcsReceiveOutcome::Duplicate
    );
    let message = only_message(&client, conversation);
    assert_eq!(message.payload.direction, Direction::Outgoing);
    assert_eq!(message.send_state, None);
    let message_id = message.payload.record.message_id;
    assert_eq!(
        client
            .apply_rcs_projected_status(
                conversation,
                owner.ownership_generation,
                status(RcsProjectedStatus::Sent),
            )
            .unwrap(),
        RcsReceiveOutcome::StatusApplied
    );
    assert_eq!(status_rank(&config, message_id), Some(2));
    for envelope in client.pending_outbox().unwrap() {
        let serialized = serde_json::to_string(&envelope).unwrap();
        assert!(!serialized.contains("provider-message"));
        assert!(!serialized.contains("provider-conversation"));
    }
}

#[test]
fn outgoing_tmp_echo_binds_existing_message_and_replays_after_reopen() {
    let directory = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let config = config(&directory, "client", &vault);
    let client = unlocked(&config, &vault);
    let conversation = ConversationId::new();
    let owner = client.begin_rcs_link_ownership(conversation, 0).unwrap();
    let mut existing = record("existing");
    existing.google_message_id = "existing-message".into();
    existing.google_conversation_id = "existing-conversation".into();
    client
        .ingest_rcs_projected_record(conversation, owner.ownership_generation, existing)
        .unwrap();
    let message_id = only_message(&client, conversation)
        .payload
        .record
        .message_id;
    let command_id = CommandId::new();
    let connection = Connection::open(&config.database_path).unwrap();
    connection
        .execute_batch(
            "PRAGMA key = \"x'0707070707070707070707070707070707070707070707070707070707070707'\";",
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO commands(command_id,message_id,gateway_device_id) VALUES(?,?,?)",
            [
                command_id.to_string(),
                message_id.to_string(),
                DeviceId::new().to_string(),
            ],
        )
        .unwrap();
    connection.execute(
        "INSERT INTO rcs_send_ledger(command_id,tmp_id,mapping_revision,proof_digest,google_conversation_id,attempted_at_ms) VALUES(?,?,1,zeroblob(32),?,1)",
        [command_id.to_string(), "tmp".into(), "provider-conversation".into()],
    ).unwrap();
    drop(connection);
    let mut echo = record("echo");
    echo.direction = Direction::Outgoing;
    echo.tmp_id = Some("tmp".into());
    assert_eq!(
        client
            .ingest_rcs_projected_record(conversation, owner.ownership_generation, echo.clone())
            .unwrap(),
        RcsReceiveOutcome::OutgoingMatched
    );
    assert_eq!(client.messages(conversation).unwrap().len(), 1);
    assert_eq!(
        client
            .ingest_rcs_projected_record(conversation, owner.ownership_generation, echo)
            .unwrap(),
        RcsReceiveOutcome::Duplicate
    );
    drop(client);
    let reopened = unlocked(&config, &vault);
    let mut server = Server::default();
    server.upload(&reopened);
    assert_eq!(reopened.messages(conversation).unwrap().len(), 1);
}

#[test]
fn matched_echo_status_replicates_without_provider_identifiers() {
    let directory = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let sender_config = config(&directory, "sender", &vault);
    let gateway_config = config(&directory, "gateway", &vault);
    let sender = common::unlocked(&sender_config, &vault);
    let gateway = unlocked(&gateway_config, &vault);
    let captured = gateway
        .capture_incoming(IncomingSms {
            conversation_id: None,
            sender_address: "peer-local".into(),
            body: String::new(),
            provider_message_id: Some("provider-local".into()),
            imported: true,
        })
        .unwrap();
    let observation = mapping_observation();
    assert!(matches!(
        gateway.verify_rcs_mapping(&observation, 1_000).unwrap(),
        RcsMappingOutcome::Mapped(_)
    ));
    let queued = sender
        .queue_send(
            OutgoingSms {
                conversation_id: captured.conversation_id,
                recipients: vec!["peer-local".into()],
                body: "outgoing body".into(),
            },
            GatewayRoute::RcsLink {
                gateway_device_id: gateway_config.device_id,
            },
        )
        .unwrap();
    let mut server = Server::default();
    server.upload(&sender);
    server.sync(&gateway);
    let permit = gateway
        .begin_rcs_send_attempt(queued.command_id, &observation, 1_000)
        .unwrap();
    let owner = gateway
        .begin_rcs_link_ownership(captured.conversation_id, 0)
        .unwrap();
    let mut echo = record("outgoing body");
    echo.direction = Direction::Outgoing;
    echo.google_conversation_id = observation.google_conversation_id.clone();
    echo.tmp_id = Some(permit.tmp_id().into());
    assert_eq!(
        gateway
            .ingest_rcs_projected_record(captured.conversation_id, owner.ownership_generation, echo)
            .unwrap(),
        RcsReceiveOutcome::OutgoingMatched
    );
    let mut projected_status = status(RcsProjectedStatus::Read);
    projected_status.google_conversation_id = observation.google_conversation_id;
    projected_status.tmp_id = Some(permit.tmp_id().into());
    assert_eq!(
        gateway
            .apply_rcs_projected_status(
                captured.conversation_id,
                owner.ownership_generation,
                projected_status
            )
            .unwrap(),
        RcsReceiveOutcome::StatusApplied
    );
    server.upload(&gateway);
    server.sync(&sender);
    let replicated = sender
        .messages(captured.conversation_id)
        .unwrap()
        .into_iter()
        .find(|message| message.payload.record.message_id == queued.message_id)
        .unwrap();
    assert_eq!(
        sender.rcs_message_status(queued.message_id).unwrap(),
        Some(RcsMessageStatusProjection {
            status: RcsMessageStatus::Read,
            rcs_reported: true,
        })
    );
    let decrypted = serde_json::to_string(&replicated.payload).unwrap();
    assert!(!decrypted.contains("provider-message"));
    assert!(!decrypted.contains("conversation-local"));
}

#[test]
fn conflicting_identity_rejects_without_partial_message() {
    let directory = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let config = config(&directory, "client", &vault);
    let client = unlocked(&config, &vault);
    let conversation = ConversationId::new();
    let owner = client.begin_rcs_link_ownership(conversation, 0).unwrap();
    let connection = Connection::open(&config.database_path).unwrap();
    connection
        .execute_batch(
            "PRAGMA key = \"x'0707070707070707070707070707070707070707070707070707070707070707'\";",
        )
        .unwrap();
    connection.execute("INSERT INTO rcs_message_identities(google_message_id,google_conversation_id,conversation_id,ownership_generation,message_id) VALUES('provider-message','provider-conversation',?,1,NULL)", [ConversationId::new().to_string()]).unwrap();
    let before: (i64, i64) = connection
        .query_row(
            "SELECT (SELECT COUNT(*) FROM messages),(SELECT COUNT(*) FROM rcs_message_identities)",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(
        client
            .ingest_rcs_projected_record(conversation, owner.ownership_generation, record("body"))
            .unwrap(),
        RcsReceiveOutcome::Rejected(RcsReceiveCode::IdentityConflict)
    );
    let after: (i64, i64) = connection
        .query_row(
            "SELECT (SELECT COUNT(*) FROM messages),(SELECT COUNT(*) FROM rcs_message_identities)",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(after, before);
}
