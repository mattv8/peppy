mod common;

use common::*;
use peppy_client_core::{
    HistorySearchReason, HistorySearchReplayProof, HistorySearchState, HistorySearchSyncContext,
    HistorySearchSyncFacts, HistorySearchWork,
};
use uuid::Uuid;

const CAPABLE: &str = r#"{"generation":"1","revision":"1","capabilities":{"sms":true,"mms":false},"permission":true,"policy":true,"exhausted":false}"#;

fn replay(started_at_ms: i64, snapshot_generation: i64) -> HistorySearchSyncFacts {
    HistorySearchSyncFacts {
        proof: Some(HistorySearchReplayProof {
            context: HistorySearchSyncContext {
                started_at_ms,
                snapshot_generation,
            },
            high_water_cursor: "42".into(),
            next_after_present: true,
            terminal_next_after: true,
        }),
    }
}

fn start(client: &Client) -> Uuid {
    client
        .start_history_search(Uuid::new_v4())
        .unwrap()
        .attempt_id
        .unwrap()
}

#[test]
fn nil_operation_id_is_rejected() {
    let directory = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&directory, "nil-operation", &vault), &vault);
    assert!(client.start_history_search(Uuid::nil()).is_err());
}

#[test]
fn start_is_idempotent_and_status_is_read_only() {
    let directory = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&directory, "idempotent", &vault), &vault);
    let operation = Uuid::new_v4();
    let first = client.start_history_search(operation).unwrap();
    let again = client.start_history_search(operation).unwrap();
    assert_eq!(first.attempt_id, again.attempt_id);
    // No advertised source at all: the attempt is honestly blocked, never silently catching up.
    assert_eq!(first.state, HistorySearchState::Blocked);
    assert_eq!(first.reason, Some(HistorySearchReason::Unsupported));
    assert_eq!(
        client.history_search_status(first.attempt_id).unwrap(),
        again
    );
    assert_eq!(
        client.history_search_status(first.attempt_id).unwrap(),
        again
    );
}

#[test]
fn unknown_attempts_are_not_found_for_mutations_and_idle_for_status() {
    let directory = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&directory, "unknown", &vault), &vault);
    let unknown = Uuid::new_v4();
    assert!(
        client
            .observe_history_search_sync(unknown, HistorySearchSyncFacts::default())
            .is_err()
    );
    assert!(client.history_search_work(unknown).is_err());
    assert!(client.cancel_history_search(unknown).is_err());
    let status = client.history_search_status(Some(unknown)).unwrap();
    assert_eq!(status.state, HistorySearchState::Idle);
    assert_eq!(status.attempt_id, None);
}

#[test]
fn no_advertised_source_is_blocked_unsupported_with_no_offer() {
    let directory = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&directory, "unsupported", &vault), &vault);
    let attempt = start(&client);
    let status = client
        .observe_history_search_sync(attempt, HistorySearchSyncFacts::default())
        .unwrap();
    assert_eq!(status.state, HistorySearchState::Blocked);
    assert_eq!(status.reason, Some(HistorySearchReason::Unsupported));
    assert!(!status.can_search_further);
    assert_eq!(
        client.history_search_work(attempt).unwrap(),
        HistorySearchWork::Wait
    );
}

#[test]
fn denied_own_source_is_blocked_permission_not_unsupported() {
    let directory = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&directory, "permission", &vault), &vault);
    client
        .publish_message_history_source(r#"{"generation":"1","revision":"1","capabilities":{"sms":true,"mms":false},"permission":false,"policy":true,"exhausted":false}"#)
        .unwrap();
    let attempt = start(&client);
    let status = client.history_search_status(Some(attempt)).unwrap();
    assert_eq!(status.state, HistorySearchState::Blocked);
    assert_eq!(status.reason, Some(HistorySearchReason::Permission));
    assert!(!status.can_search_further);
}

#[test]
fn capable_source_is_not_requested_without_a_proven_replay() {
    let directory = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&directory, "unproven", &vault), &vault);
    client.publish_message_history_source(CAPABLE).unwrap();
    let attempt = start(&client);
    assert_eq!(
        client.history_search_work(attempt).unwrap(),
        HistorySearchWork::WakeSync
    );
    let status = client
        .observe_history_search_sync(attempt, HistorySearchSyncFacts::default())
        .unwrap();
    assert_eq!(status.state, HistorySearchState::CatchingUp);
    assert_eq!(
        client.history_search_work(attempt).unwrap(),
        HistorySearchWork::WakeSync
    );
}

#[test]
fn stale_or_pre_start_replay_proof_is_not_accepted() {
    let directory = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&directory, "stale-proof", &vault), &vault);
    client.publish_message_history_source(CAPABLE).unwrap();
    let attempt = start(&client);
    // Captured before the attempt existed.
    let stale = client
        .observe_history_search_sync(attempt, replay(0, 0))
        .unwrap();
    assert_eq!(stale.state, HistorySearchState::CatchingUp);
    // Captured against another snapshot generation.
    let other_generation = client
        .observe_history_search_sync(attempt, replay(i64::MAX / 2, 99))
        .unwrap();
    assert_eq!(other_generation.state, HistorySearchState::CatchingUp);
    // A proof with the field absent is never terminal evidence.
    let absent = HistorySearchSyncFacts {
        proof: Some(HistorySearchReplayProof {
            next_after_present: false,
            ..replay(i64::MAX / 2, 0).proof.unwrap()
        }),
    };
    assert_eq!(
        client
            .observe_history_search_sync(attempt, absent)
            .unwrap()
            .state,
        HistorySearchState::CatchingUp
    );
    assert_eq!(
        client.history_search_work(attempt).unwrap(),
        HistorySearchWork::WakeSync
    );
}

#[test]
fn sync_context_is_read_only_and_carries_the_current_generation() {
    let directory = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&directory, "context", &vault), &vault);
    let before = client.history_search_status(None).unwrap();
    let context = client.history_search_sync_context().unwrap();
    assert!(context.started_at_ms > 0);
    assert_eq!(context.snapshot_generation, 0);
    assert_eq!(client.history_search_status(None).unwrap(), before);
}

#[test]
fn cancel_is_terminal_and_late_sync_cannot_regress_it() {
    let directory = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&directory, "cancel", &vault), &vault);
    let attempt = start(&client);
    assert_eq!(
        client.cancel_history_search(attempt).unwrap().state,
        HistorySearchState::Cancelled
    );
    assert_eq!(
        client
            .observe_history_search_sync(attempt, replay(i64::MAX / 2, 0))
            .unwrap()
            .state,
        HistorySearchState::Cancelled
    );
    assert_eq!(
        client.cancel_history_search(attempt).unwrap().state,
        HistorySearchState::Cancelled
    );
}

#[test]
fn cancel_never_rewrites_an_already_terminal_attempt() {
    let directory = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&directory, "terminal", &vault), &vault);
    let attempt = start(&client);
    let blocked = client
        .observe_history_search_sync(attempt, HistorySearchSyncFacts::default())
        .unwrap();
    assert_eq!(blocked.state, HistorySearchState::Blocked);
    let after_cancel = client.cancel_history_search(attempt).unwrap();
    assert_eq!(after_cancel.state, HistorySearchState::Blocked);
    assert_eq!(after_cancel.reason, Some(HistorySearchReason::Unsupported));
}

#[test]
fn attempt_state_and_deadlines_survive_restart() {
    let directory = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let configuration = config(&directory, "restart", &vault);
    let attempt = {
        let client = unlocked(&configuration, &vault);
        client.publish_message_history_source(CAPABLE).unwrap();
        start(&client)
    };
    let reopened = unlocked(&configuration, &vault);
    let status = reopened.history_search_status(Some(attempt)).unwrap();
    assert_eq!(status.attempt_id, Some(attempt));
    assert_eq!(status.state, HistorySearchState::CatchingUp);
    assert!(!status.can_search_further);
    assert_eq!(
        reopened.history_search_work(attempt).unwrap(),
        HistorySearchWork::WakeSync
    );
}

#[test]
fn locked_client_reports_needs_unlock_with_a_retry_offer() {
    let directory = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let configuration = config(&directory, "locked", &vault);
    let client = open(&configuration);
    let status = client.start_history_search(Uuid::new_v4()).unwrap();
    assert_eq!(status.state, HistorySearchState::Blocked);
    assert_eq!(status.reason, Some(HistorySearchReason::NeedsUnlock));
    assert!(status.can_search_further);
}
