#[path = "common/mod.rs"]
mod common;

use common::*;
use rusqlite::Connection;
use std::path::Path;

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
    (directory, open(&config), ConversationId::new(), config)
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
        .record_rcs_provider_detector(RcsProviderDetector {
            conversation_id,
            provider_row_id: 10,
            provider_thread_id: 2,
            provider_type: 132,
            observed_at_ms: 100,
            observed_generation: link.ownership_generation,
        })
        .unwrap();
    client
        .record_rcs_provider_detector(RcsProviderDetector {
            conversation_id,
            provider_row_id: 11,
            provider_thread_id: 2,
            provider_type: 132,
            observed_at_ms: 101,
            observed_generation: link.ownership_generation,
        })
        .unwrap();

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
        .record_rcs_provider_detector(RcsProviderDetector {
            conversation_id,
            provider_row_id: 31,
            provider_thread_id: 4,
            provider_type: 132,
            observed_at_ms: 1,
            observed_generation: relink.ownership_generation.saturating_sub(1),
        })
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

    client
        .record_rcs_provider_detector(RcsProviderDetector {
            conversation_id,
            provider_row_id: 11,
            provider_thread_id: 2,
            provider_type: 132,
            observed_at_ms: 100,
            observed_generation: link.ownership_generation.saturating_sub(1),
        })
        .unwrap();
    let mut expected = before.clone();
    expected
        .detectors
        .push((conversation_id.to_string(), 11, 2, 132, 100, 0, 0));
    expected.ownership[0].5 = 11;
    assert_eq!(receive_rows(&connection), expected);

    client
        .record_rcs_provider_detector(RcsProviderDetector {
            conversation_id,
            provider_row_id: 12,
            provider_thread_id: 3,
            provider_type: 133,
            observed_at_ms: 101,
            observed_generation: link.ownership_generation,
        })
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
            .record_rcs_provider_detector(RcsProviderDetector {
                conversation_id,
                provider_row_id,
                provider_thread_id: 2,
                provider_type: 132,
                observed_at_ms: provider_row_id,
                observed_generation: link.ownership_generation,
            })
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
        .record_rcs_provider_detector(RcsProviderDetector {
            conversation_id,
            provider_row_id: 1_024,
            provider_thread_id: 2,
            provider_type: 132,
            observed_at_ms: 1_024,
            observed_generation: link.ownership_generation,
        })
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
            .record_rcs_provider_detector(RcsProviderDetector {
                conversation_id,
                provider_row_id,
                provider_thread_id: 2,
                provider_type: 132,
                observed_at_ms: provider_row_id,
                observed_generation: link.ownership_generation,
            })
            .unwrap();
    }

    client
        .record_rcs_provider_detector(RcsProviderDetector {
            conversation_id,
            provider_row_id: 1_024,
            provider_thread_id: 2,
            provider_type: 132,
            observed_at_ms: 1_024,
            observed_generation: link.ownership_generation,
        })
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
            .record_rcs_provider_detector(RcsProviderDetector {
                conversation_id,
                provider_row_id,
                provider_thread_id: 2,
                provider_type: 132,
                observed_at_ms: provider_row_id,
                observed_generation: link.ownership_generation,
            })
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
        .record_rcs_provider_detector(RcsProviderDetector {
            conversation_id,
            provider_row_id: 50,
            provider_thread_id: 2,
            provider_type: 132,
            observed_at_ms: 50,
            observed_generation: first.ownership_generation,
        })
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
        .record_rcs_provider_detector(RcsProviderDetector {
            conversation_id,
            provider_row_id: 8,
            provider_thread_id: 2,
            provider_type: 132,
            observed_at_ms: 8,
            observed_generation: ownership.ownership_generation,
        })
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
