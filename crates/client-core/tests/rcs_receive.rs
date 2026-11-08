#[path = "common/mod.rs"]
mod common;

use common::*;
use rusqlite::Connection;
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
        .begin_rcs_link_ownership(conversation_id, "opaque-page-token", 41)
        .unwrap();
    assert_eq!(ownership.owner, RcsReceiveOwner::Link);
    assert_eq!(ownership.ownership_generation, 1);
    assert!(matches!(
        client.record_rcs_link_identity(
            conversation_id,
            ownership.ownership_generation,
            "google-message",
            "google-conversation",
            Some("opaque-page-token"),
        ),
        Ok(RcsIdentityOutcome::Recorded)
    ));
    assert!(matches!(
        client.record_rcs_link_identity(
            conversation_id,
            ownership.ownership_generation,
            "google-message",
            "google-conversation",
            None,
        ),
        Ok(RcsIdentityOutcome::Duplicate)
    ));
    assert!(matches!(
        client.record_rcs_link_identity(
            conversation_id,
            ownership.ownership_generation + 1,
            "stale-message",
            "google-conversation",
            None,
        ),
        Ok(RcsIdentityOutcome::Rejected(RcsReceiveCode::OwnershipStale))
    ));
}

#[test]
fn unlink_freezes_link_and_marks_ambiguous_detectors_for_alert() {
    let (_directory, client, conversation_id, _config) = client();
    let link = client
        .begin_rcs_link_ownership(conversation_id, "opaque-page-token", 10)
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
            "google-conversation",
            None,
        ),
        Ok(RcsIdentityOutcome::Rejected(RcsReceiveCode::OwnershipStale))
    ));
}

#[test]
fn history_tokens_are_canonical_opaque_values() {
    let (_directory, client, conversation_id, _config) = client();
    assert!(matches!(
        client.begin_rcs_link_ownership(conversation_id, "not canonical", 1),
        Err(Error::InvalidRequest(_))
    ));
    assert!(matches!(
        client.begin_rcs_link_ownership(conversation_id, "", 1),
        Err(Error::InvalidRequest(_))
    ));
    assert!(matches!(
        client.begin_rcs_link_ownership(conversation_id, &"a".repeat(4_097), 1),
        Err(Error::InvalidRequest(_))
    ));
}

#[test]
fn duplicate_link_identity_advances_a_valid_checkpoint_and_rejected_writes_do_not() {
    let (_directory, client, conversation_id, config) = client();
    let owner = client
        .begin_rcs_link_ownership(conversation_id, "initial", 0)
        .unwrap();
    assert_eq!(
        client
            .record_rcs_link_identity(
                conversation_id,
                owner.ownership_generation,
                "message",
                "conversation",
                Some("first"),
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
                Some("second"),
            )
            .unwrap(),
        RcsIdentityOutcome::Duplicate
    );
    drop(client);
    let reopened = open(&config);
    assert_eq!(
        reopened
            .rcs_receive_state(conversation_id)
            .unwrap()
            .unwrap()
            .history_token
            .as_deref(),
        Some("second")
    );
    assert_eq!(
        reopened
            .record_rcs_link_identity(
                conversation_id,
                owner.ownership_generation + 1,
                "message",
                "conversation",
                Some("rejected"),
            )
            .unwrap(),
        RcsIdentityOutcome::Rejected(RcsReceiveCode::OwnershipStale)
    );
    assert_eq!(
        reopened
            .rcs_receive_state(conversation_id)
            .unwrap()
            .unwrap()
            .history_token
            .as_deref(),
        Some("second")
    );
}

#[test]
fn identity_is_scoped_to_the_google_conversation_and_stale_writes_are_refused() {
    let (_directory, client, first, _config) = client();
    let second = ConversationId::new();
    let first_owner = client
        .begin_rcs_link_ownership(first, "token-one", 7)
        .unwrap();
    let second_owner = client
        .begin_rcs_link_ownership(second, "token-two", 9)
        .unwrap();
    assert_eq!(
        client
            .record_rcs_link_identity(
                first,
                first_owner.ownership_generation,
                "message",
                "chat-one",
                None
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
                "chat-two",
                None
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
                "chat-one",
                None
            )
            .unwrap(),
        RcsIdentityOutcome::Rejected(RcsReceiveCode::OwnershipStale)
    );
}

#[test]
fn proof_handover_cutover_and_relink_preserve_generations() {
    let (_directory, client, conversation_id, _config) = client();
    let link = client
        .begin_rcs_link_ownership(conversation_id, "token-one", 21)
        .unwrap();
    assert_eq!(link.detector_floor, 21);
    let no_alert = client.begin_rcs_unlink_cutover(conversation_id).unwrap();
    assert!(!no_alert.ambiguous_detector_alert);
    assert_eq!(no_alert.freeze_detector_floor, 21);
    let relink = client
        .begin_rcs_link_ownership(conversation_id, "token-two", 30)
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
        .begin_rcs_link_ownership(conversation_id, "token", 10)
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
    let link = client
        .begin_rcs_link_ownership(conversation_id, "token", 0)
        .unwrap();
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
    let link = client
        .begin_rcs_link_ownership(conversation_id, "token", 0)
        .unwrap();
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
    let link = client
        .begin_rcs_link_ownership(conversation_id, "token", 0)
        .unwrap();
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
        .begin_rcs_link_ownership(conversation_id, "first", 100)
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
        .begin_rcs_link_ownership(conversation_id, "second", 200)
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
fn checkpoint_advances_duplicate_and_empty_pages_but_rejects_stale_writers() {
    let (_directory, client, conversation_id, _config) = client();
    let ownership = client
        .begin_rcs_link_ownership(conversation_id, "first", 0)
        .unwrap();
    client
        .record_rcs_link_identity(
            conversation_id,
            ownership.ownership_generation,
            "message",
            "chat",
            None,
        )
        .unwrap();
    assert_eq!(
        client
            .checkpoint_rcs_history(conversation_id, ownership.ownership_generation, "duplicate")
            .unwrap(),
        RcsIdentityOutcome::Recorded
    );
    assert_eq!(
        client
            .checkpoint_rcs_history(conversation_id, ownership.ownership_generation, "empty")
            .unwrap(),
        RcsIdentityOutcome::Recorded
    );
    assert_eq!(
        client
            .checkpoint_rcs_history(conversation_id, ownership.ownership_generation + 1, "stale")
            .unwrap(),
        RcsIdentityOutcome::Rejected(RcsReceiveCode::OwnershipStale)
    );
    assert_eq!(
        client
            .rcs_receive_state(conversation_id)
            .unwrap()
            .unwrap()
            .history_token
            .as_deref(),
        Some("empty")
    );
}

#[test]
fn receive_state_survives_reopen() {
    let (directory, client, conversation_id, config) = client();
    let ownership = client
        .begin_rcs_link_ownership(conversation_id, "token", 7)
        .unwrap();
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
    assert_eq!(state.history_token.as_deref(), Some("token"));
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
    client.begin_rcs_link_ownership(first, "first", 1).unwrap();
    for index in 0..100 {
        client
            .begin_rcs_link_ownership(ConversationId::new(), &format!("token{index}"), 1)
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
}

#[test]
fn provider_row_routing_uses_exact_proofs_without_creating_domain_rows() {
    let (_directory, client, link_conversation, config) = client();
    client
        .begin_rcs_link_ownership(link_conversation, "cursor", 1)
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
        })
        .unwrap();
    assert_eq!(route, RcsProviderRowRoute::DetectorRecorded);
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
            })
            .is_err()
    );
}

#[test]
fn raw_identity_link_lane_routes_to_detector() {
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
        .begin_rcs_link_ownership(captured.conversation_id, "raw-link", 1)
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
            })
            .unwrap(),
        RcsProviderRowRoute::DetectorRecorded
    );
}

#[test]
fn exact_raw_identity_link_lane_without_mapping_never_routes_provider() {
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
        .begin_rcs_link_ownership(captured.conversation_id, "unmapped-link", 1)
        .unwrap();
    assert_ne!(
        client
            .route_rcs_provider_row(RcsProviderRow {
                provider_row_id: 102,
                provider_thread_id: 18,
                provider_type: 1,
                observed_at_ms: 1_000,
                addresses: vec!["unmapped-peer".into()],
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
        .begin_rcs_link_ownership(captured.conversation_id, "outage-link", 0)
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
                })
                .unwrap(),
            RcsProviderRowRoute::DetectorRecorded
        );
    }
    assert_eq!(owner.owner, RcsReceiveOwner::Link);
    let cutover = client
        .begin_rcs_unlink_cutover(captured.conversation_id)
        .unwrap();
    assert_eq!(cutover.freeze_detector_floor, 5);
    assert!(cutover.ambiguous_detector_alert);
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
        .begin_rcs_link_ownership(captured.conversation_id, "group-link", 0)
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
            })
            .unwrap(),
        RcsProviderRowRoute::Alert
    );
    assert_eq!(receive_rows(&connection), before);
}
