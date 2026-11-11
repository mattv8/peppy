#[path = "common/mod.rs"]
mod common;

use common::*;
use rusqlite::{Connection, OptionalExtension};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

type DetectorRow = (String, i64, i64, i64, i64, i64, i64);
type IdentityRow = (String, String, String, i64);
type OwnershipRow = (
    String,
    String,
    i64,
    Option<String>,
    i64,
    i64,
    Option<String>,
);

fn client() -> (tempfile::TempDir, Client, ConversationId, ClientConfig) {
    let directory = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let config = config(&directory, "rcs-receive", &vault);
    let client = common::unlocked(&config, &vault);
    publish_current_gateway(&client, &config);
    (directory, client, ConversationId::new(), config)
}

fn publish_current_gateway(client: &Client, config: &ClientConfig) {
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    client
        .publish_rcs_capability(
            &RcsLinkStatusEvent {
                gateway_device_id: config.device_id,
                state: RcsCapabilityState::Linked,
                availability: true,
                health: true,
                phone_responsive: true,
                observed_at_ms: now_ms,
                capability_revision: 1,
                feature_bits: 1,
                linked_since_ms: Some(now_ms),
            },
            &[],
            now_ms,
        )
        .unwrap();
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

/// The stored local recovery record for a conversation, parsed from SQLCipher metadata.
fn recovery_json(config: &ClientConfig, conversation_id: ConversationId) -> serde_json::Value {
    let json: String = sqlcipher(&config.database_path)
        .query_row(
            "SELECT v FROM metadata WHERE k=?",
            [format!("rcs_recovery:{conversation_id}")],
            |row| row.get(0),
        )
        .unwrap();
    serde_json::from_str(&json).unwrap()
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ReceiveRows {
    detectors: Vec<DetectorRow>,
    identities: Vec<IdentityRow>,
    ownership: Vec<OwnershipRow>,
    message_count: i64,
    message_ids: Vec<String>,
}

fn receive_rows(connection: &Connection) -> ReceiveRows {
    let detectors = connection
        .prepare("SELECT conversation_id,provider_row_id,provider_thread_id,provider_type,observed_at_ms,observed_generation,resolved FROM rcs_provider_detectors ORDER BY conversation_id,provider_row_id")
        .unwrap()
        .query_map([], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
                row.get(6)?,
            ))
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    let identities = connection
        .prepare("SELECT google_message_id,google_conversation_id,conversation_id,ownership_generation FROM rcs_message_identities ORDER BY google_message_id,google_conversation_id")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    let ownership = connection
        .prepare("SELECT conversation_id,owner,ownership_generation,history_token,detector_floor,observed_detector_high_water,alert_code FROM rcs_lane_ownership ORDER BY conversation_id")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?, row.get(6)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    let message_ids: Vec<String> = connection
        .prepare("SELECT id FROM messages ORDER BY id")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    ReceiveRows {
        detectors,
        identities,
        ownership,
        message_count: message_ids.len() as i64,
        message_ids,
    }
}

#[test]
fn link_ownership_is_generation_fenced_and_identities_are_replayed() {
    let (_directory, client, conversation_id, _config) = client();
    let ownership = client
        .begin_rcs_link_ownership(conversation_id, 41)
        .unwrap();
    assert_eq!(ownership.owner, RcsReceiveOwner::Link);
    assert_eq!(ownership.ownership_generation, 1);
    assert!(matches!(
        client.record_rcs_link_identity(
            conversation_id,
            ownership.ownership_generation,
            "google-message",
            "google-conversation"
        ),
        Ok(RcsIdentityOutcome::Recorded)
    ));
    assert!(matches!(
        client.record_rcs_link_identity(
            conversation_id,
            ownership.ownership_generation,
            "google-message",
            "google-conversation"
        ),
        Ok(RcsIdentityOutcome::Duplicate)
    ));
    assert!(matches!(
        client.record_rcs_link_identity(
            conversation_id,
            ownership.ownership_generation + 1,
            "stale-message",
            "google-conversation"
        ),
        Ok(RcsIdentityOutcome::Rejected(RcsReceiveCode::OwnershipStale))
    ));
}

#[test]
fn unlink_freezes_link_and_marks_ambiguous_detectors_for_alert() {
    let (_directory, client, conversation_id, _config) = client();
    let link = client
        .begin_rcs_link_ownership(conversation_id, 10)
        .unwrap();
    client
        .record_rcs_provider_detector(
            conversation_id,
            link.ownership_generation,
            RcsProviderDetector {
                provider_row_id: 10,
                provider_thread_id: 2,
                provider_type: 132,
                observed_at_ms: 100,
                observed_generation: link.ownership_generation,
            },
        )
        .unwrap();
    assert_eq!(
        client
            .record_rcs_provider_detector(
                conversation_id,
                link.ownership_generation,
                RcsProviderDetector {
                    provider_row_id: 11,
                    provider_thread_id: 2,
                    provider_type: 132,
                    observed_at_ms: 101,
                    observed_generation: link.ownership_generation,
                }
            )
            .unwrap(),
        RcsIdentityOutcome::Recorded
    );

    let cutover = client.begin_rcs_unlink_cutover(conversation_id).unwrap();
    assert_eq!(cutover.owner, RcsReceiveOwner::Provider);
    assert_eq!(cutover.freeze_detector_floor, 11);
    assert!(cutover.ambiguous_detector_alert);
    assert!(matches!(
        client.record_rcs_link_identity(
            conversation_id,
            link.ownership_generation,
            "late-message",
            "google-conversation"
        ),
        Ok(RcsIdentityOutcome::Rejected(RcsReceiveCode::OwnershipStale))
    ));
}

#[test]
fn duplicate_link_identity_is_idempotent_and_rejected_writes_change_nothing() {
    let (_directory, client, conversation_id, config) = client();
    let owner = client.begin_rcs_link_ownership(conversation_id, 0).unwrap();
    assert_eq!(
        client
            .record_rcs_link_identity(
                conversation_id,
                owner.ownership_generation,
                "message",
                "conversation",
            )
            .unwrap(),
        RcsIdentityOutcome::Recorded
    );
    assert_eq!(
        client
            .record_rcs_link_identity(
                conversation_id,
                owner.ownership_generation,
                "message",
                "conversation",
            )
            .unwrap(),
        RcsIdentityOutcome::Duplicate
    );
    let before = recovery_json(&config, conversation_id);
    drop(client);
    let reopened = open(&config);
    assert_eq!(
        reopened
            .record_rcs_link_identity(
                conversation_id,
                owner.ownership_generation + 1,
                "message",
                "conversation",
            )
            .unwrap(),
        RcsIdentityOutcome::Rejected(RcsReceiveCode::OwnershipStale)
    );
    assert_eq!(recovery_json(&config, conversation_id), before);
}

#[test]
fn identity_is_scoped_to_the_google_conversation_and_stale_writes_are_refused() {
    let (_directory, client, first, _config) = client();
    let second = ConversationId::new();
    let first_owner = client.begin_rcs_link_ownership(first, 7).unwrap();
    let second_owner = client.begin_rcs_link_ownership(second, 9).unwrap();
    assert_eq!(
        client
            .record_rcs_link_identity(
                first,
                first_owner.ownership_generation,
                "message",
                "chat-one"
            )
            .unwrap(),
        RcsIdentityOutcome::Recorded
    );
    assert_eq!(
        client
            .record_rcs_link_identity(
                second,
                second_owner.ownership_generation,
                "message",
                "chat-two"
            )
            .unwrap(),
        RcsIdentityOutcome::Recorded
    );
    assert_eq!(
        client
            .record_rcs_link_identity(
                first,
                first_owner.ownership_generation + 1,
                "late",
                "chat-one"
            )
            .unwrap(),
        RcsIdentityOutcome::Rejected(RcsReceiveCode::OwnershipStale)
    );
}

#[test]
fn proof_handover_cutover_and_relink_preserve_generations() {
    let (_directory, client, conversation_id, _config) = client();
    let link = client
        .begin_rcs_link_ownership(conversation_id, 21)
        .unwrap();
    assert_eq!(link.detector_floor, 21);
    let no_alert = client.begin_rcs_unlink_cutover(conversation_id).unwrap();
    assert!(!no_alert.ambiguous_detector_alert);
    assert_eq!(no_alert.freeze_detector_floor, 21);
    let relink = client
        .begin_rcs_link_ownership(conversation_id, 30)
        .unwrap();
    assert_eq!(relink.owner, RcsReceiveOwner::Link);
    assert_eq!(relink.ownership_generation, link.ownership_generation + 2);
    client
        .record_rcs_provider_detector(
            conversation_id,
            relink.ownership_generation,
            RcsProviderDetector {
                provider_row_id: 31,
                provider_thread_id: 4,
                provider_type: 132,
                observed_at_ms: 1,
                observed_generation: relink.ownership_generation.saturating_sub(1),
            },
        )
        .unwrap();
    let alert = client.begin_rcs_unlink_cutover(conversation_id).unwrap();
    assert!(alert.ambiguous_detector_alert);
    assert_eq!(alert.freeze_detector_floor, 31);
}

#[test]
fn detector_records_never_create_messages_or_link_identities() {
    let (_directory, client, conversation_id, config) = client();
    let link = client
        .begin_rcs_link_ownership(conversation_id, 10)
        .unwrap();
    let connection = sqlcipher(&config.database_path);
    let before = receive_rows(&connection);

    assert_eq!(
        client
            .record_rcs_provider_detector(
                conversation_id,
                link.ownership_generation + 1,
                RcsProviderDetector {
                    provider_row_id: 11,
                    provider_thread_id: 2,
                    provider_type: 132,
                    observed_at_ms: 100,
                    observed_generation: link.ownership_generation.saturating_sub(1),
                }
            )
            .unwrap(),
        RcsIdentityOutcome::Rejected(RcsReceiveCode::OwnershipStale)
    );
    assert_eq!(receive_rows(&connection), before);

    assert_eq!(
        client
            .record_rcs_provider_detector(
                conversation_id,
                link.ownership_generation,
                RcsProviderDetector {
                    provider_row_id: 11,
                    provider_thread_id: 2,
                    provider_type: 132,
                    observed_at_ms: 100,
                    observed_generation: link.ownership_generation.saturating_sub(1),
                }
            )
            .unwrap(),
        RcsIdentityOutcome::Recorded
    );
    let mut expected = before.clone();
    expected
        .detectors
        .push((conversation_id.to_string(), 11, 2, 132, 100, 0, 0));
    expected.ownership[0].5 = 11;
    assert_eq!(receive_rows(&connection), expected);

    client
        .record_rcs_provider_detector(
            conversation_id,
            link.ownership_generation,
            RcsProviderDetector {
                provider_row_id: 12,
                provider_thread_id: 3,
                provider_type: 133,
                observed_at_ms: 101,
                observed_generation: link.ownership_generation,
            },
        )
        .unwrap();
    expected.detectors.push((
        conversation_id.to_string(),
        12,
        3,
        133,
        101,
        link.ownership_generation as i64,
        0,
    ));
    expected.ownership[0].5 = 12;
    assert_eq!(
        receive_rows(&connection),
        expected,
        "detector writes must not alter identities, ownership, or messages"
    );
}

#[test]
fn detector_pruning_keeps_unresolved_ambiguous_window_rows() {
    let (_directory, client, conversation_id, config) = client();
    let link = client.begin_rcs_link_ownership(conversation_id, 0).unwrap();
    for provider_row_id in 0..1_024 {
        client
            .record_rcs_provider_detector(
                conversation_id,
                link.ownership_generation,
                RcsProviderDetector {
                    provider_row_id,
                    provider_thread_id: 2,
                    provider_type: 132,
                    observed_at_ms: provider_row_id,
                    observed_generation: link.ownership_generation,
                },
            )
            .unwrap();
    }
    let connection = sqlcipher(&config.database_path);
    connection
        .execute(
            "UPDATE rcs_provider_detectors SET resolved=1 WHERE conversation_id=? AND provider_row_id=1",
            [conversation_id.to_string()],
        )
        .unwrap();

    client
        .record_rcs_provider_detector(
            conversation_id,
            link.ownership_generation,
            RcsProviderDetector {
                provider_row_id: 1_024,
                provider_thread_id: 2,
                provider_type: 132,
                observed_at_ms: 1_024,
                observed_generation: link.ownership_generation,
            },
        )
        .unwrap();

    let unresolved_oldest: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM rcs_provider_detectors WHERE conversation_id=? AND provider_row_id=0 AND resolved=0",
            [conversation_id.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    let resolved_oldest: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM rcs_provider_detectors WHERE conversation_id=? AND provider_row_id=1",
            [conversation_id.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(unresolved_oldest, 1);
    assert_eq!(resolved_oldest, 0);
}

#[test]
fn detector_overflow_sets_alert_without_dropping_unresolved_rows() {
    let (_directory, client, conversation_id, config) = client();
    let link = client.begin_rcs_link_ownership(conversation_id, 0).unwrap();
    for provider_row_id in 0..1_024 {
        client
            .record_rcs_provider_detector(
                conversation_id,
                link.ownership_generation,
                RcsProviderDetector {
                    provider_row_id,
                    provider_thread_id: 2,
                    provider_type: 132,
                    observed_at_ms: provider_row_id,
                    observed_generation: link.ownership_generation,
                },
            )
            .unwrap();
    }

    client
        .record_rcs_provider_detector(
            conversation_id,
            link.ownership_generation,
            RcsProviderDetector {
                provider_row_id: 1_024,
                provider_thread_id: 2,
                provider_type: 132,
                observed_at_ms: 1_024,
                observed_generation: link.ownership_generation,
            },
        )
        .unwrap();

    let connection = sqlcipher(&config.database_path);
    let (count, alert_code): (i64, Option<String>) = connection
        .query_row(
            "SELECT (SELECT COUNT(*) FROM rcs_provider_detectors WHERE conversation_id=?),alert_code FROM rcs_lane_ownership WHERE conversation_id=?",
            [conversation_id.to_string(), conversation_id.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(count, 1_024);
    assert_eq!(alert_code.as_deref(), Some("detector_overflow"));
    let new_row_count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM rcs_provider_detectors WHERE conversation_id=? AND provider_row_id=1024",
            [conversation_id.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(new_row_count, 0);
}

#[test]
fn overflow_cutover_keeps_high_water_and_stronger_alert() {
    let (_directory, client, conversation_id, _config) = client();
    let link = client.begin_rcs_link_ownership(conversation_id, 0).unwrap();
    for provider_row_id in 0..=1_024 {
        client
            .record_rcs_provider_detector(
                conversation_id,
                link.ownership_generation,
                RcsProviderDetector {
                    provider_row_id,
                    provider_thread_id: 2,
                    provider_type: 132,
                    observed_at_ms: provider_row_id,
                    observed_generation: link.ownership_generation,
                },
            )
            .unwrap();
    }
    let cutover = client.begin_rcs_unlink_cutover(conversation_id).unwrap();
    assert!(cutover.freeze_detector_floor >= 1_024);
    assert_eq!(
        client
            .rcs_receive_state(conversation_id)
            .unwrap()
            .unwrap()
            .alert,
        RcsReceiveAlert::DetectorOverflow
    );
}

#[test]
fn cutover_never_moves_the_detector_floor_backwards() {
    let (_directory, client, conversation_id, _config) = client();
    let first = client
        .begin_rcs_link_ownership(conversation_id, 100)
        .unwrap();
    client
        .record_rcs_provider_detector(
            conversation_id,
            first.ownership_generation,
            RcsProviderDetector {
                provider_row_id: 50,
                provider_thread_id: 2,
                provider_type: 132,
                observed_at_ms: 50,
                observed_generation: first.ownership_generation,
            },
        )
        .unwrap();
    client.begin_rcs_unlink_cutover(conversation_id).unwrap();
    let relink = client
        .begin_rcs_link_ownership(conversation_id, 200)
        .unwrap();
    assert_eq!(
        client
            .begin_rcs_unlink_cutover(conversation_id)
            .unwrap()
            .freeze_detector_floor,
        200
    );
    assert!(relink.ownership_generation > first.ownership_generation);
}

#[test]
fn recovery_progress_and_completion_are_generation_fenced() {
    let (_directory, client, conversation_id, config) = client();
    let ownership = client.begin_rcs_link_ownership(conversation_id, 0).unwrap();
    let generation = ownership.ownership_generation;
    let proof = recovery_json(&config, conversation_id)["watermark_ms"]
        .as_i64()
        .unwrap();
    assert_eq!(
        client
            .record_rcs_recovery_progress(conversation_id, generation, 50, "page-one")
            .unwrap(),
        RcsRecoveryOutcome::Recorded
    );
    let progressed = recovery_json(&config, conversation_id);
    assert_eq!(
        progressed,
        serde_json::json!({
            "v": 1,
            "watermark_ms": proof,
            "resume_hint": {"target_ms": 50, "cursor": "page-one"},
            "incomplete": true
        })
    );
    assert_eq!(
        client
            .record_rcs_recovery_progress(conversation_id, generation + 1, 60, "stale")
            .unwrap(),
        RcsRecoveryOutcome::Rejected(RcsReceiveCode::OwnershipStale)
    );
    assert_eq!(
        client
            .complete_rcs_recovery(conversation_id, generation + 1, 60)
            .unwrap(),
        RcsRecoveryOutcome::Rejected(RcsReceiveCode::OwnershipStale)
    );
    assert_eq!(
        client
            .clear_rcs_recovery_hint(conversation_id, generation + 1)
            .unwrap(),
        RcsRecoveryOutcome::Rejected(RcsReceiveCode::OwnershipStale)
    );
    assert_eq!(recovery_json(&config, conversation_id), progressed);
    assert_eq!(
        client
            .record_rcs_recovery_progress(conversation_id, generation, 50, "not canonical")
            .unwrap(),
        RcsRecoveryOutcome::Rejected(RcsReceiveCode::CursorInvalid)
    );
    assert_eq!(recovery_json(&config, conversation_id), progressed);
}

#[test]
fn complete_takes_the_maximum_watermark_and_clears_the_gap() {
    let (_directory, client, conversation_id, config) = client();
    let generation = client
        .begin_rcs_link_ownership(conversation_id, 0)
        .unwrap()
        .ownership_generation;
    let proof = recovery_json(&config, conversation_id)["watermark_ms"]
        .as_i64()
        .unwrap();
    client
        .record_rcs_recovery_progress(conversation_id, generation, proof + 500, "cursor")
        .unwrap();
    assert_eq!(
        client
            .complete_rcs_recovery(conversation_id, generation, proof + 200)
            .unwrap(),
        RcsRecoveryOutcome::Recorded
    );
    assert_eq!(
        recovery_json(&config, conversation_id),
        serde_json::json!({
            "v": 1,
            "watermark_ms": proof + 200,
            "resume_hint": null,
            "incomplete": false
        })
    );
    client
        .complete_rcs_recovery(conversation_id, generation, proof + 100)
        .unwrap();
    assert_eq!(
        recovery_json(&config, conversation_id)["watermark_ms"],
        proof + 200
    );
}

#[test]
fn cleared_hint_keeps_the_gap_open_for_a_later_retry() {
    let (_directory, client, conversation_id, config) = client();
    let generation = client
        .begin_rcs_link_ownership(conversation_id, 0)
        .unwrap()
        .ownership_generation;
    client
        .record_rcs_recovery_progress(conversation_id, generation, 50, "cursor")
        .unwrap();
    assert_eq!(
        client
            .clear_rcs_recovery_hint(conversation_id, generation)
            .unwrap(),
        RcsRecoveryOutcome::Recorded
    );
    let state = recovery_json(&config, conversation_id);
    assert!(state["resume_hint"].is_null(), "{state}");
    assert_eq!(state["incomplete"], true, "{state}");
}

#[test]
fn recovery_writes_never_enqueue_replication_or_touch_conversation_metadata() {
    let (_directory, client, conversation_id, config) = client();
    let generation = client
        .begin_rcs_link_ownership(conversation_id, 0)
        .unwrap()
        .ownership_generation;
    let connection = sqlcipher(&config.database_path);
    let outbox_before: i64 = connection
        .query_row("SELECT COUNT(*) FROM outbox", [], |row| row.get(0))
        .unwrap();
    let metadata_before = client.rcs_conversation_metadata(conversation_id).unwrap();
    client
        .record_rcs_recovery_progress(conversation_id, generation, 50, "cursor")
        .unwrap();
    client
        .complete_rcs_recovery(conversation_id, generation, 50)
        .unwrap();
    client
        .clear_rcs_recovery_hint(conversation_id, generation)
        .unwrap();
    let outbox_after: i64 = connection
        .query_row("SELECT COUNT(*) FROM outbox", [], |row| row.get(0))
        .unwrap();
    assert_eq!(outbox_after, outbox_before);
    assert_eq!(
        client.rcs_conversation_metadata(conversation_id).unwrap(),
        metadata_before
    );
}

#[test]
fn receive_state_survives_reopen() {
    let (directory, client, conversation_id, config) = client();
    let ownership = client.begin_rcs_link_ownership(conversation_id, 7).unwrap();
    client
        .record_rcs_provider_detector(
            conversation_id,
            ownership.ownership_generation,
            RcsProviderDetector {
                provider_row_id: 8,
                provider_thread_id: 2,
                provider_type: 132,
                observed_at_ms: 8,
                observed_generation: ownership.ownership_generation,
            },
        )
        .unwrap();
    drop(client);
    let reopened = open(&config);
    let state = reopened
        .rcs_receive_state(conversation_id)
        .unwrap()
        .unwrap();
    assert_eq!(state.owner, RcsReceiveOwner::Link);
    assert_eq!(state.ownership_generation, ownership.ownership_generation);
    assert_eq!(
        (
            state.detector_floor,
            state.observed_detector_high_water,
            state.alert
        ),
        (7, 8, RcsReceiveAlert::None)
    );
    drop(directory);
}

#[test]
fn terminal_unlink_candidates_require_completion_and_are_stably_bounded() {
    let (_directory, client, first, _config) = client();
    client.begin_rcs_link_ownership(first, 1).unwrap();
    for _ in 0..100 {
        client
            .begin_rcs_link_ownership(ConversationId::new(), 1)
            .unwrap();
    }
    assert!(client.rcs_owner_unlink_cutover_candidates().is_err());
    client
        .record_rcs_link_lifecycle(
            RcsCapabilityState::Unlinked,
            false,
            false,
            false,
            1_000,
            1_000,
        )
        .unwrap();
    assert!(client.rcs_owner_unlink_cutover_candidates().is_err());
    client
        .record_rcs_explicit_unlink_completed(1_000, 1_000)
        .unwrap();
    let candidates = client.rcs_owner_unlink_cutover_candidates().unwrap();
    assert_eq!(candidates.len(), 100);
    assert!(
        candidates
            .windows(2)
            .all(|rows| rows[0].conversation_id < rows[1].conversation_id)
    );
    assert!(candidates.iter().all(|row| row.lifecycle_generation == 1));
    client
        .record_rcs_link_lifecycle(RcsCapabilityState::Linked, true, true, true, 2_000, 2_000)
        .unwrap();
    assert!(client.rcs_owner_unlink_cutover_candidates().is_err());
}

#[test]
fn link_owned_context_enumeration_recovers_every_mapped_lane() {
    let (_directory, client, first, config) = client();
    let connection = sqlcipher(&config.database_path);
    authorize_local_link_gateway(&client, &connection);
    let mut expected = vec![first];
    let mut participants = std::collections::BTreeSet::new();
    client.begin_rcs_link_ownership(first, 1).unwrap();
    assert!(participants.insert("+14155551000".to_owned()));
    insert_route_proof(&connection, first, "google-first", "+14155551000");
    connection
        .execute(
            "INSERT INTO metadata(k,v) VALUES(?,?)",
            rusqlite::params![format!("rcs_link_owner_google:{first}"), "google-first"],
        )
        .unwrap();
    for index in 0..100 {
        let conversation_id = ConversationId::new();
        let participant = format!("+1415555{index:04}");
        expected.push(conversation_id);
        assert!(participants.insert(participant.clone()));
        client.begin_rcs_link_ownership(conversation_id, 1).unwrap();
        insert_route_proof(
            &connection,
            conversation_id,
            &format!("google-{index}"),
            &participant,
        );
        connection
            .execute(
                "INSERT INTO metadata(k,v) VALUES(?,?)",
                rusqlite::params![
                    format!("rcs_link_owner_google:{conversation_id}"),
                    format!("google-{index}"),
                ],
            )
            .unwrap();
    }
    assert_eq!(participants.len(), 101);
    let contexts = client.rcs_link_owned_conversations().unwrap();
    let recovered = contexts
        .iter()
        .map(|context| context.conversation_id.to_string())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(contexts.len(), 101);
    assert_eq!(
        recovered,
        expected
            .into_iter()
            .map(|conversation_id| conversation_id.to_string())
            .collect::<std::collections::BTreeSet<_>>()
    );
    assert!(
        contexts
            .iter()
            .all(|context| context.owner == RcsReceiveOwner::Link)
    );
}

fn authorize_local_link_gateway(client: &Client, connection: &Connection) {
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    client
        .record_rcs_link_lifecycle(RcsCapabilityState::Linked, true, true, true, now_ms, now_ms)
        .unwrap();
    connection
        .execute(
            "INSERT INTO metadata(k,v) VALUES('rcs_mapping_context_v1',?) ON CONFLICT(k) DO UPDATE SET v=excluded.v",
            [serde_json::json!({
                "link_generation": 1,
                "session_generation": 1,
                "context_revision": 1,
                "link_state": 0,
            })
            .to_string()],
        )
        .unwrap();
}

fn insert_route_proof(
    connection: &Connection,
    conversation_id: ConversationId,
    google_id: &str,
    participant: &str,
) {
    connection
        .execute(
            "INSERT INTO rcs_conversation_mappings(peppy_conversation_id,google_conversation_id,class,proof_version,proof_hash,mapping_revision,verified_at_ms) VALUES(?, ?, 'one_to_one', 2, ?, 1, 1000)",
            rusqlite::params![conversation_id.to_string(), google_id, vec![0_u8; 32]],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO rcs_mapping_proofs(peppy_conversation_id,mapping_revision,canonical_participant,identity_region,own_identity) VALUES(?,1,?,'US','+14155550100')",
            rusqlite::params![conversation_id.to_string(), participant],
        )
        .unwrap();
    // The proof stands for one whose thread passed the one-to-one guard.
    connection
        .execute(
            "INSERT INTO metadata(k,v) VALUES(?,'1')",
            [format!("rcs_receive_guarded:{conversation_id}")],
        )
        .unwrap();
}

#[test]
fn provider_row_routing_uses_exact_proofs_without_creating_domain_rows() {
    let (_directory, client, link_conversation, config) = client();
    client
        .begin_rcs_link_ownership(link_conversation, 1)
        .unwrap();
    let connection = sqlcipher(&config.database_path);
    insert_route_proof(
        &connection,
        link_conversation,
        "google-link",
        "+14155552671",
    );
    let before_messages: i64 = connection
        .query_row("SELECT COUNT(*) FROM messages", [], |row| row.get(0))
        .unwrap();
    let before_domain_rows: i64 = connection
        .query_row(
            "SELECT (SELECT COUNT(*) FROM addresses) + (SELECT COUNT(*) FROM sms_acquisitions) + (SELECT COUNT(*) FROM mms_acquisitions)",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let route = client
        .route_rcs_provider_row(RcsProviderRow {
            provider_row_id: 9,
            provider_thread_id: 3,
            provider_type: 1,
            observed_at_ms: 1_000,
            addresses: vec!["+1 (415) 555-2671".into()],
            restricted: true,
            text_only: true,
        })
        .unwrap();
    assert_eq!(route, RcsProviderRowRoute::Provider);
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM messages", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        before_messages
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT (SELECT COUNT(*) FROM addresses) + (SELECT COUNT(*) FROM sms_acquisitions) + (SELECT COUNT(*) FROM mms_acquisitions)",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        before_domain_rows
    );
    client.begin_rcs_unlink_cutover(link_conversation).unwrap();
    assert_eq!(
        client
            .route_rcs_provider_row(RcsProviderRow {
                provider_row_id: 10,
                provider_thread_id: 4,
                provider_type: 1,
                observed_at_ms: 1_001,
                addresses: vec!["+14155552671".into()],
                restricted: true,
                text_only: true,
            })
            .unwrap(),
        RcsProviderRowRoute::Provider
    );
    assert_eq!(
        client
            .route_rcs_provider_row(RcsProviderRow {
                provider_row_id: 12,
                provider_thread_id: 6,
                provider_type: 1,
                observed_at_ms: 1_001,
                addresses: vec!["+14155559999".into()],
                restricted: true,
                text_only: true,
            })
            .unwrap(),
        RcsProviderRowRoute::Provider
    );
    let second = ConversationId::new();
    insert_route_proof(&connection, second, "google-ambiguous", "+14155552671");
    assert_eq!(
        client
            .route_rcs_provider_row(RcsProviderRow {
                provider_row_id: 11,
                provider_thread_id: 5,
                provider_type: 1,
                observed_at_ms: 1_002,
                addresses: vec!["+14155552671".into()],
                restricted: true,
                text_only: true,
            })
            .unwrap(),
        RcsProviderRowRoute::Alert
    );
}

#[test]
fn provider_row_routing_refuses_a_foreign_current_gateway() {
    let (_directory, client, _conversation, config) = client();
    let connection = sqlcipher(&config.database_path);
    connection
        .execute(
            "UPDATE rcs_link_status_projection SET gateway_device_id=?",
            [DeviceId::new().to_string()],
        )
        .unwrap();
    assert!(
        client
            .route_rcs_provider_row(RcsProviderRow {
                provider_row_id: 1,
                provider_thread_id: 1,
                provider_type: 1,
                observed_at_ms: 1_000,
                addresses: vec!["+14155552671".into()],
                restricted: true,
                text_only: true,
            })
            .is_err()
    );
}

#[test]
fn unrestricted_provider_row_returns_provider_without_reading_mapping_state() {
    let (_directory, client, _conversation, config) = client();
    let connection = sqlcipher(&config.database_path);
    connection
        .execute(
            "INSERT INTO rcs_conversation_mappings(peppy_conversation_id,google_conversation_id,class,proof_version,proof_hash,mapping_revision,verified_at_ms) VALUES('not-a-conversation','google','one_to_one',2,?,1,1000)",
            [vec![0_u8; 32]],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO rcs_mapping_proofs(peppy_conversation_id,mapping_revision,canonical_participant,identity_region,own_identity) VALUES('not-a-conversation',1,'+14155552671','US','+14155550100')",
            [],
        )
        .unwrap();

    assert_eq!(
        client
            .route_rcs_provider_row(RcsProviderRow {
                provider_row_id: 1,
                provider_thread_id: 1,
                provider_type: 1,
                observed_at_ms: 1_000,
                addresses: vec!["+14155552671".into()],
                restricted: false,
                text_only: true,
            })
            .unwrap(),
        RcsProviderRowRoute::Provider
    );
}

#[test]
fn restricted_text_row_before_proof_time_returns_provider() {
    let (_directory, client, _unused, config) = client();
    let captured = client
        .capture_incoming(IncomingSms {
            conversation_id: None,
            sender_address: "peer-local".into(),
            body: "existing".into(),
            provider_message_id: Some("provider-existing".into()),
            imported: true,
        })
        .unwrap();
    client
        .begin_rcs_link_ownership(captured.conversation_id, 1)
        .unwrap();
    let connection = sqlcipher(&config.database_path);
    insert_route_proof(
        &connection,
        captured.conversation_id,
        "google-raw",
        "raw:peer-local",
    );
    assert_eq!(
        client
            .route_rcs_provider_row(RcsProviderRow {
                provider_row_id: 101,
                provider_thread_id: 17,
                provider_type: 1,
                observed_at_ms: 1_000,
                addresses: vec!["peer-local".into()],
                restricted: true,
                text_only: true,
            })
            .unwrap(),
        RcsProviderRowRoute::Provider
    );
}

#[test]
fn restricted_text_row_without_mapping_returns_provider() {
    let (_directory, client, _unused, _config) = client();
    let captured = client
        .capture_incoming(IncomingSms {
            conversation_id: None,
            sender_address: "unmapped-peer".into(),
            body: "existing".into(),
            provider_message_id: Some("provider-unmapped".into()),
            imported: true,
        })
        .unwrap();
    client
        .begin_rcs_link_ownership(captured.conversation_id, 1)
        .unwrap();
    assert_eq!(
        client
            .route_rcs_provider_row(RcsProviderRow {
                provider_row_id: 102,
                provider_thread_id: 18,
                provider_type: 1,
                observed_at_ms: 1_000,
                addresses: vec!["unmapped-peer".into()],
                restricted: true,
                text_only: true,
            })
            .unwrap(),
        RcsProviderRowRoute::Provider
    );
}

#[test]
fn outage_provider_routing_preserves_detector_floor_for_local_link_lane() {
    let (_directory, client, _unused, config) = client();
    let captured = client
        .capture_incoming(IncomingSms {
            conversation_id: None,
            sender_address: "outage-peer".into(),
            body: "existing".into(),
            provider_message_id: Some("provider-outage-existing".into()),
            imported: true,
        })
        .unwrap();
    let owner = client
        .begin_rcs_link_ownership(captured.conversation_id, 0)
        .unwrap();
    let states = [
        (RcsCapabilityState::SignedOut, true, true),
        (RcsCapabilityState::NeedsRelink, true, true),
        (RcsCapabilityState::Unlinked, true, true),
        (RcsCapabilityState::Unavailable, false, false),
        (RcsCapabilityState::Linked, false, true),
    ];
    for (index, (state, availability, health)) in states.into_iter().enumerate() {
        let observed_at_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        client
            .publish_rcs_capability(
                &RcsLinkStatusEvent {
                    gateway_device_id: config.device_id,
                    state,
                    availability,
                    health,
                    phone_responsive: health,
                    observed_at_ms,
                    capability_revision: u64::try_from(index + 2).unwrap(),
                    feature_bits: 0,
                    linked_since_ms: None,
                },
                &[],
                observed_at_ms,
            )
            .unwrap();
        assert_eq!(
            client
                .route_rcs_provider_row(RcsProviderRow {
                    provider_row_id: i64::try_from(index + 1).unwrap(),
                    provider_thread_id: 20,
                    provider_type: 1,
                    observed_at_ms,
                    addresses: vec!["outage-peer".into()],
                    restricted: true,
                    text_only: true,
                })
                .unwrap(),
            RcsProviderRowRoute::Provider
        );
    }
    assert_eq!(owner.owner, RcsReceiveOwner::Link);
    let cutover = client
        .begin_rcs_unlink_cutover(captured.conversation_id)
        .unwrap();
    assert_eq!(cutover.freeze_detector_floor, 0);
    assert!(!cutover.ambiguous_detector_alert);
}

#[test]
fn group_provider_row_never_routes_into_a_single_participant_link_lane() {
    let (_directory, client, _unused, config) = client();
    let captured = client
        .capture_incoming(IncomingSms {
            conversation_id: None,
            sender_address: "group-peer-a".into(),
            body: "existing".into(),
            provider_message_id: Some("provider-group-existing".into()),
            imported: true,
        })
        .unwrap();
    client
        .begin_rcs_link_ownership(captured.conversation_id, 0)
        .unwrap();
    let connection = sqlcipher(&config.database_path);
    let before = receive_rows(&connection);
    assert_eq!(
        client
            .route_rcs_provider_row(RcsProviderRow {
                provider_row_id: 201,
                provider_thread_id: 21,
                provider_type: 2,
                observed_at_ms: 1_000,
                addresses: vec!["group-peer-a".into(), "group-peer-b".into()],
                restricted: true,
                text_only: true,
            })
            .unwrap(),
        RcsProviderRowRoute::Alert
    );
    assert_eq!(receive_rows(&connection), before);
}

const CLAIM_GOOGLE_ID: &str = "google-conversation";
const CLAIM_PARTICIPANT: &str = "raw-peer";

fn claim_observation(participant: &str, revision: u64) -> RcsMappingObservation {
    RcsMappingObservation {
        google_conversation_id: CLAIM_GOOGLE_ID.into(),
        google_candidate_count: 1,
        participants: vec![RcsParticipantObservation {
            identity: participant.into(),
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
                link_generation: 4,
                session_generation: 7,
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
        identity_region: "ZZ".into(),
        observed_at_ms: 1_000,
        link_generation: 4,
        session_generation: 7,
        context_revision: 1,
        canonicalizer_revision: 1,
        mapping_revision: revision,
        link_state: RcsLinkState::Linked,
    }
}

fn claim_request(conversation_id: ConversationId) -> RcsLinkOwnershipClaim {
    RcsLinkOwnershipClaim {
        conversation_id,
        google_conversation_id: CLAIM_GOOGLE_ID.into(),
        mapping_revision: 1,
        provider_floor: 3,
        expected_lifecycle_generation: 1,
        expected_link_generation: 4,
        expected_session_generation: 7,
        expected_context_revision: 1,
    }
}

fn claim_client() -> (tempfile::TempDir, Client, ConversationId, ClientConfig) {
    let directory = tempfile::tempdir().unwrap();
    let vault = Vault::new();
    let config = config(&directory, "rcs-claim", &vault);
    let client = open(&config);
    let conversation = client
        .capture_incoming(IncomingSms {
            conversation_id: None,
            sender_address: CLAIM_PARTICIPANT.into(),
            body: String::new(),
            provider_message_id: Some("claim-provider".into()),
            imported: true,
        })
        .unwrap()
        .conversation_id;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    client
        .record_rcs_link_lifecycle(RcsCapabilityState::Linked, true, true, true, now, now)
        .unwrap();
    (directory, client, conversation, config)
}

fn verify_claim_mapping(client: &Client, conversation_id: ConversationId, revision: u64) {
    client
        .capture_incoming(IncomingSms {
            conversation_id: Some(conversation_id),
            sender_address: CLAIM_PARTICIPANT.into(),
            body: String::new(),
            provider_message_id: Some(format!("claim-provider-{revision}")),
            imported: true,
        })
        .unwrap();
    let outcome = client
        .verify_rcs_mapping(&claim_observation(CLAIM_PARTICIPANT, revision), 1_000)
        .unwrap();
    assert!(
        matches!(outcome, RcsMappingOutcome::Mapped(_)),
        "{outcome:?}"
    );
}

#[test]
fn claim_refuses_missing_stale_mismatched_unlinked_and_stale_generations() {
    let (_directory, client, conversation, _config) = claim_client();
    assert_eq!(
        client
            .claim_rcs_link_ownership(claim_request(conversation))
            .unwrap(),
        RcsLinkOwnershipClaimOutcome::Refused
    );
    verify_claim_mapping(&client, conversation, 1);
    for refused in [
        {
            let mut v = claim_request(conversation);
            v.mapping_revision = 2;
            v
        },
        {
            let mut v = claim_request(conversation);
            v.google_conversation_id = "other".into();
            v
        },
        {
            let mut v = claim_request(conversation);
            v.expected_lifecycle_generation += 1;
            v
        },
        {
            let mut v = claim_request(conversation);
            v.expected_link_generation += 1;
            v
        },
        {
            let mut v = claim_request(conversation);
            v.expected_session_generation += 1;
            v
        },
        {
            let mut v = claim_request(conversation);
            v.expected_context_revision += 1;
            v
        },
    ] {
        assert_eq!(
            client.claim_rcs_link_ownership(refused).unwrap(),
            RcsLinkOwnershipClaimOutcome::Refused
        );
    }
}

#[test]
fn claim_owns_publishes_and_reclaims_without_resetting_watermark() {
    let (_directory, client, conversation, config) = claim_client();
    verify_claim_mapping(&client, conversation, 1);
    let claimed = match client
        .claim_rcs_link_ownership(claim_request(conversation))
        .unwrap()
    {
        RcsLinkOwnershipClaimOutcome::Claimed(value) => value,
        RcsLinkOwnershipClaimOutcome::Refused => panic!("claim refused"),
    };
    assert_eq!(claimed.owner, RcsReceiveOwner::Link);
    assert_eq!(
        recovery_json(&config, conversation),
        serde_json::json!({
            "v": 1,
            "watermark_ms": claimed.proof_time_ms,
            "resume_hint": null,
            "incomplete": false
        })
    );
    assert_eq!(
        client.rcs_link_owned_conversations().unwrap()[0].conversation_id,
        conversation
    );
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    client
        .publish_rcs_eligibility_from_lifecycle(&[conversation], now)
        .unwrap();
    assert!(
        client
            .rcs_eligibility_projection(conversation, config.device_id, now)
            .unwrap()
            .is_some()
    );
    verify_claim_mapping(&client, conversation, 2);
    let mut re_claim = claim_request(conversation);
    re_claim.mapping_revision = 2;
    let re_claimed = match client.claim_rcs_link_ownership(re_claim).unwrap() {
        RcsLinkOwnershipClaimOutcome::Claimed(value) => value,
        RcsLinkOwnershipClaimOutcome::Refused => panic!("reclaim refused"),
    };
    assert_eq!(re_claimed.proof_time_ms, claimed.proof_time_ms);
    assert_eq!(
        recovery_json(&config, conversation)["watermark_ms"],
        claimed.proof_time_ms
    );
}

#[test]
fn claim_initial_floor_and_router_guard_band() {
    let (_directory, client, conversation, _config) = claim_client();
    verify_claim_mapping(&client, conversation, 1);
    let before = now_ms();
    let ownership = match client
        .claim_rcs_link_ownership(claim_request(conversation))
        .unwrap()
    {
        RcsLinkOwnershipClaimOutcome::Claimed(value) => value,
        RcsLinkOwnershipClaimOutcome::Refused => panic!("claim refused"),
    };
    let after = now_ms();
    assert!(
        (before - 15 * 60 * 1_000..=after - 15 * 60 * 1_000).contains(&ownership.proof_time_ms)
    );
    for (offset, restricted, text_only, expected, reason) in [
        (
            121_000,
            true,
            true,
            RcsProviderRowRoute::DetectorRecorded,
            None,
        ),
        (
            60_000,
            true,
            true,
            RcsProviderRowRoute::Provider,
            Some(RcsProviderRouteReason::GuardBand),
        ),
        (
            121_000,
            true,
            false,
            RcsProviderRowRoute::DetectorRecorded,
            None,
        ),
        (
            60_000,
            true,
            false,
            RcsProviderRowRoute::Provider,
            Some(RcsProviderRouteReason::GuardBand),
        ),
        (
            121_000,
            false,
            false,
            RcsProviderRowRoute::Provider,
            Some(RcsProviderRouteReason::NotRestrictedText),
        ),
        (
            121_000,
            false,
            true,
            RcsProviderRowRoute::Provider,
            Some(RcsProviderRouteReason::NotRestrictedText),
        ),
    ] {
        let routed = client
            .route_rcs_provider_row_result(RcsProviderRow {
                provider_row_id: offset,
                provider_thread_id: 1,
                provider_type: 1,
                observed_at_ms: ownership.proof_time_ms + offset,
                addresses: vec![CLAIM_PARTICIPANT.into()],
                restricted,
                text_only,
            })
            .unwrap();
        assert_eq!(routed.route, expected);
        assert_eq!(routed.reason, reason);
    }
    let pre_proof = client
        .route_rcs_provider_row_result(RcsProviderRow {
            provider_row_id: 122_000,
            provider_thread_id: 1,
            provider_type: 1,
            observed_at_ms: ownership.proof_time_ms,
            addresses: vec![CLAIM_PARTICIPANT.into()],
            restricted: true,
            text_only: true,
        })
        .unwrap();
    assert_eq!(pre_proof.route, RcsProviderRowRoute::Provider);
    assert_eq!(pre_proof.reason, Some(RcsProviderRouteReason::PreProof));
}

const CLAIM_GOOGLE_ID_TWO: &str = "google-conversation-two";
const THIRD_PARTY: &str = "raw-third-party";

fn capture_from(client: &Client, conversation_id: ConversationId, sender: &str, row: &str) {
    client
        .capture_incoming(IncomingSms {
            conversation_id: Some(conversation_id),
            sender_address: sender.into(),
            body: String::new(),
            provider_message_id: Some(row.into()),
            imported: true,
        })
        .unwrap();
}

/// A linked gateway holding the participant's thread, plus what another device needs to queue a
/// text through it.
struct SendProofGateway {
    directory: tempfile::TempDir,
    vault: Vault,
    config: ClientConfig,
    client: Client,
    conversation: ConversationId,
    server: Server,
}

fn send_proof_gateway() -> SendProofGateway {
    let directory = tempfile::tempdir().unwrap();
    let vault = Vault::new();
    let config = config(&directory, "rcs-send-proof", &vault);
    let client = common::unlocked(&config, &vault);
    publish_current_gateway(&client, &config);
    let conversation = ConversationId::new();
    capture_from(&client, conversation, CLAIM_PARTICIPANT, "participant-row");
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    client
        .record_rcs_link_lifecycle(RcsCapabilityState::Linked, true, true, true, now, now)
        .unwrap();
    SendProofGateway {
        directory,
        vault,
        config,
        client,
        conversation,
        server: Server::default(),
    }
}

impl SendProofGateway {
    /// Proves the chat for a text another device queued to the participant in this thread.
    fn prove_by_send(&mut self, sender_name: &str, revision: u64) {
        let sender = common::unlocked(
            &config(&self.directory, sender_name, &self.vault),
            &self.vault,
        );
        let queued = sender
            .queue_send(
                OutgoingSms {
                    conversation_id: self.conversation,
                    recipients: vec![CLAIM_PARTICIPANT.into()],
                    body: "hello".into(),
                },
                GatewayRoute::RcsLink {
                    gateway_device_id: self.config.device_id,
                },
            )
            .unwrap();
        self.server.upload(&sender);
        self.server.sync(&self.client);
        let outcome = self
            .client
            .verify_rcs_send_mapping(
                &claim_observation(CLAIM_PARTICIPANT, revision),
                queued.command_id,
                1_000,
            )
            .unwrap();
        assert!(
            matches!(outcome, RcsMappingOutcome::Mapped(_)),
            "{outcome:?}"
        );
    }

    fn claim(&self, revision: u64) -> RcsLinkOwnershipClaimOutcome {
        let lifecycle = self
            .client
            .rcs_link_lifecycle_generation()
            .unwrap()
            .unwrap();
        self.client
            .claim_rcs_link_ownership(RcsLinkOwnershipClaim {
                mapping_revision: revision,
                expected_lifecycle_generation: lifecycle,
                ..claim_request(self.conversation)
            })
            .unwrap()
    }

    fn owned_lanes(&self) -> usize {
        self.client.rcs_link_owned_conversations().unwrap().len()
    }
}

#[test]
fn a_send_proof_on_the_participants_own_thread_opens_the_receive_lane() {
    let mut gateway = send_proof_gateway();
    gateway.prove_by_send("sender-a", 1);

    assert!(matches!(
        gateway.claim(1),
        RcsLinkOwnershipClaimOutcome::Claimed(_)
    ));
    assert_eq!(gateway.owned_lanes(), 1);
}

/// A send needs only its own recipient, so it may prove a chat on a thread that also carries
/// someone else. That proof must not let the link receive into the thread.
#[test]
fn a_send_proof_on_a_thread_with_another_party_cannot_claim_the_receive_lane() {
    let mut gateway = send_proof_gateway();
    capture_from(
        &gateway.client,
        gateway.conversation,
        THIRD_PARTY,
        "third-party-row",
    );
    gateway.prove_by_send("sender-a", 1);

    assert_eq!(gateway.claim(1), RcsLinkOwnershipClaimOutcome::Refused);
    assert_eq!(gateway.owned_lanes(), 0);
}

/// Google Messages mirrors chat messages into the phone's message store, and the provider capture
/// stored such rows before the link owned the lane. Those earlier rows must not make the capture
/// take a new mirrored row as well: the link already delivers that message.
#[test]
fn a_mirrored_chat_row_is_left_to_the_link_despite_earlier_captured_rows_naming_the_participant() {
    let mut gateway = send_proof_gateway();
    gateway.prove_by_send("sender-a", 1);
    assert!(matches!(
        gateway.claim(1),
        RcsLinkOwnershipClaimOutcome::Claimed(_)
    ));
    let earlier = gateway
        .client
        .begin_mms_acquisition(MmsAcquisitionInput {
            source: MmsSource {
                source_generation: "phone-install-a".into(),
                subscription_id: "android-subscription-7".into(),
                provider_message_id: "mirrored-earlier".into(),
                provider_thread_id: Some("thread-mirror".into()),
            },
            direction: Direction::Incoming,
            sender_address: Some(CLAIM_PARTICIPANT.into()),
            recipients: vec!["+12025550100".into()],
            subject: None,
            body: "mirrored chat message".into(),
            imported: true,
            observed_at_ms: 1,
            transaction_id: None,
        })
        .unwrap();
    gateway
        .client
        .complete_mms_acquisition(&earlier.acquisition_id)
        .unwrap();
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;

    let route = gateway
        .client
        .route_rcs_provider_row(RcsProviderRow {
            provider_row_id: 77,
            provider_thread_id: 5,
            provider_type: 1,
            observed_at_ms: now_ms,
            addresses: vec![CLAIM_PARTICIPANT.into(), "+12025550100".into()],
            restricted: true,
            text_only: true,
        })
        .unwrap();

    assert_eq!(route, RcsProviderRowRoute::DetectorRecorded);
}

/// A receive context fetched while the thread was the participant's alone must not be usable
/// after the thread gained someone else and only a send has proven the chat since.
#[test]
fn a_receive_context_fetched_earlier_cannot_ingest_once_the_thread_lost_its_guard() {
    let mut gateway = send_proof_gateway();
    gateway.prove_by_send("sender-a", 1);
    let RcsLinkOwnershipClaimOutcome::Claimed(ownership) = gateway.claim(1) else {
        panic!("the participant's own thread opens a lane");
    };
    capture_from(
        &gateway.client,
        gateway.conversation,
        THIRD_PARTY,
        "third-party-row",
    );
    gateway.prove_by_send("sender-b", 2);

    let ingested = gateway
        .client
        .ingest_rcs_projected_record(
            gateway.conversation,
            ownership.ownership_generation,
            historical_record("after-guard-loss", 2_000),
        )
        .unwrap();
    let progressed = gateway
        .client
        .complete_rcs_recovery(gateway.conversation, ownership.ownership_generation, 2_000)
        .unwrap();

    assert_eq!(
        ingested,
        RcsReceiveOutcome::Rejected(RcsReceiveCode::ConversationUnproven)
    );
    assert_eq!(
        progressed,
        RcsRecoveryOutcome::Rejected(RcsReceiveCode::ConversationUnproven)
    );
}

#[test]
fn a_send_proof_does_not_reopen_a_lane_whose_thread_gained_another_party() {
    let mut gateway = send_proof_gateway();
    gateway.prove_by_send("sender-a", 1);
    assert!(matches!(
        gateway.claim(1),
        RcsLinkOwnershipClaimOutcome::Claimed(_)
    ));
    capture_from(
        &gateway.client,
        gateway.conversation,
        THIRD_PARTY,
        "third-party-row",
    );

    gateway.prove_by_send("sender-b", 2);

    assert_eq!(gateway.owned_lanes(), 0);
    assert_eq!(
        gateway.client.rcs_receive_context(CLAIM_GOOGLE_ID).unwrap(),
        None
    );
}

fn claimed(outcome: RcsLinkOwnershipClaimOutcome) -> RcsOwnership {
    match outcome {
        RcsLinkOwnershipClaimOutcome::Claimed(value) => value,
        RcsLinkOwnershipClaimOutcome::Refused => panic!("claim refused"),
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

fn historical_record(google_message_id: &str, sent_at_ms: i64) -> RcsProjectedRecord {
    RcsProjectedRecord {
        google_message_id: google_message_id.into(),
        google_conversation_id: CLAIM_GOOGLE_ID.into(),
        direction: Direction::Incoming,
        body: "history".into(),
        sent_at_ms: Some(sent_at_ms),
        tmp_id: None,
        rcs_reported: false,
        e2ee_reported: None,
        attachments: Vec::new(),
    }
}

/// Unlocked variant of `claim_client` with a current gateway, so ingest and context writes run.
fn unlocked_claim_client() -> (tempfile::TempDir, Client, ConversationId, ClientConfig) {
    let directory = tempfile::tempdir().unwrap();
    let vault = Vault::new();
    let config = config(&directory, "rcs-claim-unlocked", &vault);
    let client = unlocked(&config, &vault);
    let conversation = client
        .capture_incoming(IncomingSms {
            conversation_id: None,
            sender_address: CLAIM_PARTICIPANT.into(),
            body: String::new(),
            provider_message_id: Some("claim-provider".into()),
            imported: true,
        })
        .unwrap()
        .conversation_id;
    let now = now_ms();
    client
        .record_rcs_link_lifecycle(RcsCapabilityState::Linked, true, true, true, now, now)
        .unwrap();
    client
        .publish_rcs_capability(
            &RcsLinkStatusEvent {
                gateway_device_id: config.device_id,
                state: RcsCapabilityState::Linked,
                availability: true,
                health: true,
                phone_responsive: true,
                observed_at_ms: now,
                capability_revision: 2,
                feature_bits: 1,
                linked_since_ms: Some(now),
            },
            &[],
            now,
        )
        .unwrap();
    (directory, client, conversation, config)
}

#[test]
fn claim_changed_google_id_begins_a_fenced_generation() {
    let (_directory, client, conversation, config) = unlocked_claim_client();
    verify_claim_mapping(&client, conversation, 1);
    let first = claimed(
        client
            .claim_rcs_link_ownership(claim_request(conversation))
            .unwrap(),
    );

    client
        .invalidate_rcs_context_for_google_event(
            conversation,
            CLAIM_GOOGLE_ID,
            RcsContextExpectation {
                ownership_generation: first.ownership_generation,
                link_generation: 4,
                session_generation: 7,
                context_revision: 1,
            },
        )
        .unwrap();
    client
        .capture_incoming(IncomingSms {
            conversation_id: Some(conversation),
            sender_address: CLAIM_PARTICIPANT.into(),
            body: String::new(),
            provider_message_id: Some("claim-provider-two".into()),
            imported: true,
        })
        .unwrap();
    let mut observation = claim_observation(CLAIM_PARTICIPANT, 2);
    observation.google_conversation_id = CLAIM_GOOGLE_ID_TWO.into();
    observation.context_revision = 2;
    observation.native_evidence.lifetime.context_revision = 2;
    observation.carrier_binding.route_context_revision = 2;
    let outcome = client.verify_rcs_mapping(&observation, 1_000).unwrap();
    assert!(
        matches!(outcome, RcsMappingOutcome::Mapped(_)),
        "{outcome:?}"
    );
    assert_eq!(
        client.rcs_receive_context(CLAIM_GOOGLE_ID_TWO).unwrap(),
        None
    );
    assert!(
        client
            .rcs_link_owned_conversations()
            .unwrap()
            .iter()
            .all(|context| context.conversation_id != conversation)
    );
    let mismatched_owner = client
        .route_rcs_provider_row_result(RcsProviderRow {
            provider_row_id: 700,
            provider_thread_id: 7,
            provider_type: 1,
            observed_at_ms: now_ms(),
            addresses: vec![CLAIM_PARTICIPANT.into()],
            restricted: true,
            text_only: true,
        })
        .unwrap();
    assert_eq!(mismatched_owner.route, RcsProviderRowRoute::Provider);
    assert_eq!(
        mismatched_owner.reason,
        Some(RcsProviderRouteReason::NotLinkOwned)
    );

    client
        .record_rcs_recovery_progress(conversation, first.ownership_generation, 1, "old-cursor")
        .unwrap();

    let before = now_ms();
    let mut request = claim_request(conversation);
    request.google_conversation_id = CLAIM_GOOGLE_ID_TWO.into();
    request.mapping_revision = 2;
    request.expected_context_revision = 2;
    let second = claimed(client.claim_rcs_link_ownership(request).unwrap());
    assert!(second.ownership_generation > first.ownership_generation);
    assert!(second.proof_time_ms >= before - 15 * 60 * 1_000);
    assert_eq!(
        recovery_json(&config, conversation),
        serde_json::json!({
            "v": 1,
            "watermark_ms": second.proof_time_ms,
            "resume_hint": null,
            "incomplete": false
        })
    );

    assert_eq!(
        client
            .ingest_rcs_projected_observation(
                conversation,
                first.ownership_generation,
                historical_record("stale-old", now_ms()),
                RcsObservationProvenance::Live
            )
            .unwrap(),
        RcsReceiveOutcome::Rejected(RcsReceiveCode::OwnershipStale)
    );
    assert_eq!(
        client
            .record_rcs_link_identity(
                conversation,
                first.ownership_generation,
                "stale-old",
                CLAIM_GOOGLE_ID
            )
            .unwrap(),
        RcsIdentityOutcome::Rejected(RcsReceiveCode::OwnershipStale)
    );
    assert_eq!(
        client
            .record_rcs_recovery_progress(conversation, first.ownership_generation, 2, "stale")
            .unwrap(),
        RcsRecoveryOutcome::Rejected(RcsReceiveCode::OwnershipStale)
    );
}

#[test]
fn claim_initial_floor_accepts_recent_history_and_rejects_older_provider_floor() {
    let (_directory, client, conversation, _config) = unlocked_claim_client();
    verify_claim_mapping(&client, conversation, 1);
    let ownership = claimed(
        client
            .claim_rcs_link_ownership(claim_request(conversation))
            .unwrap(),
    );
    let recent = now_ms() - 5 * 60 * 1_000;

    assert_eq!(
        client
            .ingest_rcs_projected_observation(
                conversation,
                ownership.ownership_generation,
                historical_record("recent", recent),
                RcsObservationProvenance::Historical,
            )
            .unwrap(),
        RcsReceiveOutcome::Inserted
    );
    assert_eq!(
        client
            .ingest_rcs_projected_observation(
                conversation,
                ownership.ownership_generation,
                historical_record("older-than-window", ownership.proof_time_ms - 1),
                RcsObservationProvenance::Historical,
            )
            .unwrap(),
        RcsReceiveOutcome::Rejected(RcsReceiveCode::ProviderFloor)
    );
}

#[test]
fn cutover_alert_read_reports_only_ambiguous_provider_windows() {
    let (_directory, client, conversation, _config) = client();
    assert_eq!(
        client.rcs_cutover_alerts().unwrap(),
        RcsCutoverAlerts {
            count: 0,
            conversation_ids: vec![]
        }
    );
    let clean = ConversationId::new();
    client.begin_rcs_link_ownership(clean, 10).unwrap();
    assert!(
        !client
            .begin_rcs_unlink_cutover(clean)
            .unwrap()
            .ambiguous_detector_alert
    );
    assert_eq!(client.rcs_cutover_alerts().unwrap().count, 0);

    let link = client.begin_rcs_link_ownership(conversation, 10).unwrap();
    client
        .record_rcs_provider_detector(
            conversation,
            link.ownership_generation,
            RcsProviderDetector {
                provider_row_id: 11,
                provider_thread_id: 2,
                provider_type: 132,
                observed_at_ms: 101,
                observed_generation: link.ownership_generation,
            },
        )
        .unwrap();
    assert!(
        client
            .begin_rcs_unlink_cutover(conversation)
            .unwrap()
            .ambiguous_detector_alert
    );
    assert_eq!(
        client.rcs_cutover_alerts().unwrap(),
        RcsCutoverAlerts {
            count: 1,
            conversation_ids: vec![conversation]
        }
    );
}

#[test]
fn cutover_to_provider_deletes_link_owner_and_recovery_state() {
    let (_directory, client, conversation, config) = unlocked_claim_client();
    verify_claim_mapping(&client, conversation, 1);
    claimed(
        client
            .claim_rcs_link_ownership(claim_request(conversation))
            .unwrap(),
    );
    let connection = sqlcipher(&config.database_path);
    let key = format!("rcs_link_owner_google:{conversation}");
    let recovery_key = format!("rcs_recovery:{conversation}");
    let stored = |connection: &Connection, key: &str| -> Option<String> {
        connection
            .query_row("SELECT v FROM metadata WHERE k=?", [key], |row| row.get(0))
            .optional()
            .unwrap()
    };
    assert_eq!(stored(&connection, &key).as_deref(), Some(CLAIM_GOOGLE_ID));
    assert!(stored(&connection, &recovery_key).is_some());
    client.begin_rcs_unlink_cutover(conversation).unwrap();
    assert_eq!(stored(&connection, &key), None);
    assert_eq!(stored(&connection, &recovery_key), None);
}

#[test]
fn missing_or_corrupt_recovery_state_falls_back_to_the_proof_floor() {
    let (_directory, client, conversation, config) = unlocked_claim_client();
    verify_claim_mapping(&client, conversation, 1);
    let owned = claimed(
        client
            .claim_rcs_link_ownership(claim_request(conversation))
            .unwrap(),
    );
    let connection = sqlcipher(&config.database_path);
    let recovery_key = format!("rcs_recovery:{conversation}");
    let floor = RcsRecoveryState {
        watermark_ms: owned.proof_time_ms,
        resume_hint: None,
        incomplete: true,
    };
    for stored in [None, Some("not json")] {
        connection
            .execute("DELETE FROM metadata WHERE k=?", [&recovery_key])
            .unwrap();
        if let Some(value) = stored {
            connection
                .execute(
                    "INSERT INTO metadata(k,v) VALUES(?,?)",
                    [recovery_key.as_str(), value],
                )
                .unwrap();
        }
        let enumerated = client.rcs_link_owned_conversations().unwrap();
        assert_eq!(owned_recovery(&enumerated, conversation), &floor);
        assert_eq!(
            client
                .rcs_receive_context(CLAIM_GOOGLE_ID)
                .unwrap()
                .unwrap()
                .recovery,
            floor
        );
    }
    assert_eq!(
        client
            .complete_rcs_recovery(
                conversation,
                owned.ownership_generation,
                owned.proof_time_ms + 10
            )
            .unwrap(),
        RcsRecoveryOutcome::Recorded
    );
    assert_eq!(
        recovery_json(&config, conversation)["watermark_ms"],
        owned.proof_time_ms + 10
    );
}

fn owned_recovery(
    contexts: &[RcsReceiveContext],
    conversation: ConversationId,
) -> &RcsRecoveryState {
    &contexts
        .iter()
        .find(|context| context.conversation_id == conversation)
        .unwrap()
        .recovery
}

#[test]
fn link_owned_context_exposes_watermark_and_resume_hint() {
    let (_directory, client, conversation, _config) = unlocked_claim_client();
    verify_claim_mapping(&client, conversation, 1);
    let owned = claimed(
        client
            .claim_rcs_link_ownership(claim_request(conversation))
            .unwrap(),
    );
    client
        .record_rcs_recovery_progress(
            conversation,
            owned.ownership_generation,
            owned.proof_time_ms + 500,
            "page-cursor",
        )
        .unwrap();
    let expected = RcsRecoveryState {
        watermark_ms: owned.proof_time_ms,
        resume_hint: Some(RcsResumeHint {
            target_ms: owned.proof_time_ms + 500,
            cursor: "page-cursor".into(),
        }),
        incomplete: true,
    };
    let enumerated = client
        .rcs_link_owned_conversations()
        .unwrap()
        .into_iter()
        .find(|context| context.conversation_id == conversation)
        .unwrap();
    assert_eq!(enumerated.recovery, expected);
    assert_eq!(
        client
            .rcs_receive_context(CLAIM_GOOGLE_ID)
            .unwrap()
            .unwrap()
            .recovery,
        expected
    );
}
