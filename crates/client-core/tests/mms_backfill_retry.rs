//! Compatibility coverage for lifecycle continuation exposed by the existing backfill DTO.

use crate::common::*;
use tempfile::TempDir;

fn input(id: &str) -> MmsAcquisitionInput {
    MmsAcquisitionInput {
        source: MmsSource {
            source_generation: "phone-install".into(),
            subscription_id: "sim-1".into(),
            provider_message_id: id.into(),
            provider_thread_id: None,
        },
        direction: Direction::Incoming,
        sender_address: Some(ADDRESS.into()),
        recipients: Vec::new(),
        subject: None,
        body: "pending".into(),
        imported: false,
        observed_at_ms: 0,
        transaction_id: None,
    }
}

#[test]
fn recovered_remote_capacity_retries_pending_once_not_on_every_upload() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "gateway", &vault), &vault);
    client
        .plan_mms_backfill_scans("phone-install", "sim-1", "7")
        .unwrap();
    let acquired = client.begin_mms_acquisition(input("7")).unwrap();
    let conditions = MmsBackfillRecheckConditions::default();
    client
        .report_mms_backfill_remote_capacity("phone-install", "sim-1", false)
        .unwrap();
    client
        .record_mms_backfill_recheck_outcome(
            &acquired.acquisition_id,
            1000,
            MmsBackfillRecheckOutcome::UnchangedPartUnavailable,
            conditions,
        )
        .unwrap();
    assert!(
        client
            .mms_backfill_due_rechecks("phone-install", "sim-1", 1001, conditions)
            .unwrap()
            .requests
            .is_empty()
    );
    client
        .report_mms_backfill_remote_capacity("phone-install", "sim-1", true)
        .unwrap();
    assert_eq!(
        client
            .mms_backfill_due_rechecks("phone-install", "sim-1", 1002, conditions)
            .unwrap()
            .requests
            .len(),
        1
    );
    client
        .record_mms_backfill_recheck_outcome(
            &acquired.acquisition_id,
            1003,
            MmsBackfillRecheckOutcome::UnchangedPartUnavailable,
            conditions,
        )
        .unwrap();
    client
        .report_mms_backfill_remote_capacity("phone-install", "sim-1", true)
        .unwrap();
    assert!(
        client
            .mms_backfill_due_rechecks("phone-install", "sim-1", 1004, conditions)
            .unwrap()
            .requests
            .is_empty()
    );
}

#[test]
fn compatibility_recheck_reports_continuation_only_when_it_returns_work() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "gateway", &vault), &vault);
    client
        .plan_mms_backfill_scans("phone-install", "sim-1", "1")
        .unwrap();

    assert!(
        client
            .mms_backfill_recheck_requests("phone-install", "sim-1")
            .unwrap()
            .requests
            .is_empty()
    );
    assert!(
        !client
            .mms_backfill_recheck_requests("phone-install", "sim-1")
            .unwrap()
            .continuation_due
    );

    client.begin_mms_acquisition(input("1")).unwrap();
    let pass = client
        .mms_backfill_recheck_requests("phone-install", "sim-1")
        .unwrap();
    assert_eq!(pass.requests.len(), 1);
    assert!(pass.continuation_due);
}

#[test]
fn due_recheck_exposes_identity_and_backoff_survives_restart() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let cfg = config(&dir, "gateway", &vault);
    let client = unlocked(&cfg, &vault);
    let acquisition = client.begin_mms_acquisition(input("7")).unwrap();
    let conditions = MmsBackfillRecheckConditions::default();

    client
        .record_mms_backfill_recheck_outcome(
            &acquisition.acquisition_id,
            1_000,
            MmsBackfillRecheckOutcome::UnchangedPartUnavailable,
            conditions,
        )
        .unwrap();
    assert!(
        client
            .mms_backfill_due_rechecks("phone-install", "sim-1", 1_001, conditions)
            .unwrap()
            .requests
            .is_empty()
    );
    drop(client);

    let reopened = unlocked(&cfg, &vault);
    let due = reopened
        .mms_backfill_due_rechecks("phone-install", "sim-1", 901_000, conditions)
        .unwrap();
    assert_eq!(due.requests.len(), 1);
    assert_eq!(due.requests[0].acquisition_id, acquisition.acquisition_id);
    assert_eq!(due.requests[0].state, MmsAcquisitionState::Pending);
    reopened
        .record_mms_backfill_recheck_outcome(
            &acquisition.acquisition_id,
            901_000,
            MmsBackfillRecheckOutcome::Progress,
            conditions,
        )
        .unwrap();
    assert_eq!(
        reopened
            .mms_backfill_due_rechecks("phone-install", "sim-1", 901_000, conditions)
            .unwrap()
            .requests
            .len(),
        1,
        "progress clears the pending retry delay"
    );
}

#[test]
fn explicit_refresh_and_provider_hint_reset_only_pending_due_work() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "gateway", &vault), &vault);
    let first = client.begin_mms_acquisition(input("1")).unwrap();
    let second = client.begin_mms_acquisition(input("2")).unwrap();
    let conditions = MmsBackfillRecheckConditions::default();
    for acquisition in [&first, &second] {
        client
            .record_mms_backfill_recheck_outcome(
                &acquisition.acquisition_id,
                1_000,
                MmsBackfillRecheckOutcome::TransientFailure,
                conditions,
            )
            .unwrap();
    }

    assert!(
        client
            .mms_backfill_due_rechecks("phone-install", "sim-1", 1_001, conditions)
            .unwrap()
            .requests
            .is_empty()
    );
    let hinted = client
        .mms_backfill_due_rechecks(
            "phone-install",
            "sim-1",
            1_001,
            MmsBackfillRecheckConditions {
                provider_changed: true,
                ..conditions
            },
        )
        .unwrap();
    assert_eq!(hinted.requests.len(), 2);
    for request in &hinted.requests {
        client
            .record_mms_backfill_recheck_outcome(
                &request.acquisition_id,
                1_001,
                MmsBackfillRecheckOutcome::TransientFailure,
                conditions,
            )
            .unwrap();
    }
    assert!(
        client
            .mms_backfill_due_rechecks("phone-install", "sim-1", 1_002, conditions)
            .unwrap()
            .requests
            .is_empty()
    );
    assert_eq!(
        client
            .mms_backfill_due_rechecks(
                "phone-install",
                "sim-1",
                1_002,
                MmsBackfillRecheckConditions {
                    explicit_refresh: true,
                    ..conditions
                },
            )
            .unwrap()
            .requests
            .len(),
        2
    );
}

#[test]
fn charging_completed_sweep_is_newest_first_and_rests_after_feedback() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "gateway", &vault), &vault);
    let two = client.begin_mms_acquisition(input("2")).unwrap();
    let ten = client.begin_mms_acquisition(input("10")).unwrap();
    client
        .complete_mms_acquisition(&two.acquisition_id)
        .unwrap();
    client
        .complete_mms_acquisition(&ten.acquisition_id)
        .unwrap();
    let charging = MmsBackfillRecheckConditions {
        charging: true,
        ..Default::default()
    };

    assert!(
        client
            .mms_backfill_due_rechecks(
                "phone-install",
                "sim-1",
                MMS_COMPLETED_RECHECK_REST_MS,
                MmsBackfillRecheckConditions::default(),
            )
            .unwrap()
            .requests
            .is_empty()
    );
    let sweep = client
        .mms_backfill_due_rechecks(
            "phone-install",
            "sim-1",
            MMS_COMPLETED_RECHECK_REST_MS,
            charging,
        )
        .unwrap();
    assert!(sweep.continuation_due);
    assert_eq!(
        sweep
            .requests
            .iter()
            .map(|request| request.provider_message_id.as_str())
            .collect::<Vec<_>>(),
        ["10", "2"]
    );
    for request in &sweep.requests {
        client
            .record_mms_backfill_recheck_outcome(
                &request.acquisition_id,
                MMS_COMPLETED_RECHECK_REST_MS,
                MmsBackfillRecheckOutcome::Progress,
                charging,
            )
            .unwrap();
    }
    assert!(
        !client
            .mms_backfill_due_rechecks(
                "phone-install",
                "sim-1",
                2 * MMS_COMPLETED_RECHECK_REST_MS - 1,
                charging,
            )
            .unwrap()
            .continuation_due
    );
}

#[test]
fn due_pending_work_precedes_charging_completed_maintenance() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "gateway", &vault), &vault);
    let pending = client.begin_mms_acquisition(input("99")).unwrap();
    let complete = client.begin_mms_acquisition(input("100")).unwrap();
    client
        .complete_mms_acquisition(&complete.acquisition_id)
        .unwrap();

    let requests = client
        .mms_backfill_due_rechecks(
            "phone-install",
            "sim-1",
            MMS_COMPLETED_RECHECK_REST_MS,
            MmsBackfillRecheckConditions {
                charging: true,
                ..Default::default()
            },
        )
        .unwrap()
        .requests;
    assert_eq!(requests[0].acquisition_id, pending.acquisition_id);
    assert_eq!(requests[0].state, MmsAcquisitionState::Pending);
    assert_eq!(requests[1].state, MmsAcquisitionState::Complete);
}

#[test]
fn retry_backoff_caps_at_24_hours_and_clock_rollback_does_not_stall_due_work() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "gateway", &vault), &vault);
    let acquisition = client.begin_mms_acquisition(input("1")).unwrap();
    let conditions = MmsBackfillRecheckConditions::default();
    for _ in 0..8 {
        client
            .record_mms_backfill_recheck_outcome(
                &acquisition.acquisition_id,
                1_000_000,
                MmsBackfillRecheckOutcome::TransientFailure,
                conditions,
            )
            .unwrap();
    }

    assert!(
        client
            .mms_backfill_due_rechecks(
                "phone-install",
                "sim-1",
                1_000_000 + MMS_COMPLETED_RECHECK_REST_MS,
                conditions,
            )
            .unwrap()
            .requests
            .iter()
            .any(|request| request.acquisition_id == acquisition.acquisition_id)
    );
    client
        .record_mms_backfill_recheck_outcome(
            &acquisition.acquisition_id,
            2_000_000,
            MmsBackfillRecheckOutcome::TransientFailure,
            conditions,
        )
        .unwrap();
    assert!(
        client
            .mms_backfill_due_rechecks("phone-install", "sim-1", 1_999_999, conditions)
            .unwrap()
            .requests
            .iter()
            .any(|request| request.acquisition_id == acquisition.acquisition_id)
    );
}

#[test]
fn manual_refresh_runs_one_completed_batch_without_charging() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "gateway", &vault), &vault);
    let acquisition = client.begin_mms_acquisition(input("10")).unwrap();
    client
        .complete_mms_acquisition(&acquisition.acquisition_id)
        .unwrap();

    let requests = client
        .mms_backfill_due_rechecks(
            "phone-install",
            "sim-1",
            MMS_COMPLETED_RECHECK_REST_MS,
            MmsBackfillRecheckConditions {
                explicit_refresh: true,
                ..Default::default()
            },
        )
        .unwrap()
        .requests;
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].state, MmsAcquisitionState::Complete);
}

fn pending_ids(requests: &[MmsBackfillRecheckRequest]) -> Vec<&str> {
    requests
        .iter()
        .map(|request| request.provider_message_id.as_str())
        .collect()
}

#[test]
fn provider_hint_surfaces_newest_failed_arrival_past_permanently_due_rows() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "gateway", &vault), &vault);
    // Older carrier-notification rows are never ready and stay due on every ordinary pass.
    for id in 1..=40 {
        client
            .begin_mms_acquisition(input(&id.to_string()))
            .unwrap();
    }
    let recent = client.begin_mms_acquisition(input("1000")).unwrap();
    let conditions = MmsBackfillRecheckConditions::default();
    client
        .record_mms_backfill_recheck_outcome(
            &recent.acquisition_id,
            1_000,
            MmsBackfillRecheckOutcome::TransientFailure,
            conditions,
        )
        .unwrap();
    assert!(
        !client
            .mms_backfill_due_rechecks("phone-install", "sim-1", 1_001, conditions)
            .unwrap()
            .requests
            .iter()
            .any(|request| request.provider_message_id == "1000"),
        "the failed arrival waits for its backoff without a hint"
    );

    let hinted = client
        .mms_backfill_due_rechecks(
            "phone-install",
            "sim-1",
            1_001,
            MmsBackfillRecheckConditions {
                provider_changed: true,
                ..conditions
            },
        )
        .unwrap();
    assert_eq!(hinted.requests.len(), MAX_MMS_BACKFILL_RECHECK);
    assert_eq!(hinted.requests[0].provider_message_id, "1000");
}

#[test]
fn attempted_awaiting_rows_do_not_starve_an_unattempted_older_row() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "gateway", &vault), &vault);
    let conditions = MmsBackfillRecheckConditions::default();
    // The attempted rows are older by local order and would fill the budget under local-order fairness.
    for id in 10..50 {
        let acquisition = client
            .begin_mms_acquisition(input(&id.to_string()))
            .unwrap();
        client
            .record_mms_backfill_recheck_outcome(
                &acquisition.acquisition_id,
                1_000,
                MmsBackfillRecheckOutcome::AwaitingProvider,
                conditions,
            )
            .unwrap();
    }
    client.begin_mms_acquisition(input("5")).unwrap();

    let due = client
        .mms_backfill_due_rechecks("phone-install", "sim-1", 1_001, conditions)
        .unwrap();
    assert_eq!(due.requests[0].provider_message_id, "5");
}

#[test]
fn awaiting_provider_rests_fixed_fifteen_minutes_after_three_observations() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "gateway", &vault), &vault);
    let acquisition = client.begin_mms_acquisition(input("1")).unwrap();
    let conditions = MmsBackfillRecheckConditions::default();
    for _ in 0..3 {
        client
            .record_mms_backfill_recheck_outcome(
                &acquisition.acquisition_id,
                1_000,
                MmsBackfillRecheckOutcome::AwaitingProvider,
                conditions,
            )
            .unwrap();
    }

    assert!(
        client
            .mms_backfill_due_rechecks("phone-install", "sim-1", 1_001, conditions)
            .unwrap()
            .requests
            .is_empty(),
        "the third observation rests"
    );
    assert_eq!(
        client
            .mms_backfill_due_rechecks("phone-install", "sim-1", 901_000, conditions)
            .unwrap()
            .requests
            .len(),
        1,
        "the rest is a fixed fifteen minutes, not the 24-hour unavailable cap"
    );

    let hinted = client
        .mms_backfill_due_rechecks(
            "phone-install",
            "sim-1",
            1_001,
            MmsBackfillRecheckConditions {
                provider_changed: true,
                ..conditions
            },
        )
        .unwrap();
    assert_eq!(pending_ids(&hinted.requests), ["1"]);
}

#[test]
fn first_two_awaiting_observations_carry_no_extra_delay() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "gateway", &vault), &vault);
    let acquisition = client.begin_mms_acquisition(input("1")).unwrap();
    let conditions = MmsBackfillRecheckConditions::default();
    for _ in 0..2 {
        client
            .record_mms_backfill_recheck_outcome(
                &acquisition.acquisition_id,
                1_000,
                MmsBackfillRecheckOutcome::AwaitingProvider,
                conditions,
            )
            .unwrap();
    }

    assert_eq!(
        pending_ids(
            &client
                .mms_backfill_due_rechecks("phone-install", "sim-1", 1_001, conditions)
                .unwrap()
                .requests
        ),
        ["1"]
    );
}

fn completed_ids(requests: &[MmsBackfillRecheckRequest]) -> Vec<&str> {
    requests
        .iter()
        .map(|request| request.provider_message_id.as_str())
        .collect()
}

fn charging() -> MmsBackfillRecheckConditions {
    MmsBackfillRecheckConditions {
        charging: true,
        ..Default::default()
    }
}

#[test]
fn completed_batch_keeps_unattempted_claims_across_reopen_without_skipping() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let cfg = config(&dir, "gateway", &vault);
    let client = unlocked(&cfg, &vault);
    for id in 1..=33 {
        let acquisition = client
            .begin_mms_acquisition(input(&id.to_string()))
            .unwrap();
        client
            .complete_mms_acquisition(&acquisition.acquisition_id)
            .unwrap();
    }
    let first = client
        .mms_backfill_due_rechecks(
            "phone-install",
            "sim-1",
            MMS_COMPLETED_RECHECK_REST_MS,
            charging(),
        )
        .unwrap();
    assert_eq!(first.requests.len(), MAX_MMS_BACKFILL_RECHECK);
    assert!(
        !completed_ids(&first.requests).contains(&"1"),
        "the oldest provider ID waits for the next batch"
    );
    client
        .record_mms_backfill_recheck_outcome(
            &first.requests[0].acquisition_id,
            MMS_COMPLETED_RECHECK_REST_MS,
            MmsBackfillRecheckOutcome::Progress,
            charging(),
        )
        .unwrap();
    drop(client);

    let reopened = unlocked(&cfg, &vault);
    let retry = reopened
        .mms_backfill_due_rechecks(
            "phone-install",
            "sim-1",
            MMS_COMPLETED_RECHECK_REST_MS,
            charging(),
        )
        .unwrap();
    assert_eq!(
        completed_ids(&retry.requests),
        completed_ids(&first.requests[1..]),
        "remaining claims return in the same newest-first order with no skip"
    );
    for request in &retry.requests {
        reopened
            .record_mms_backfill_recheck_outcome(
                &request.acquisition_id,
                MMS_COMPLETED_RECHECK_REST_MS,
                MmsBackfillRecheckOutcome::Progress,
                charging(),
            )
            .unwrap();
    }
    let last = reopened
        .mms_backfill_due_rechecks(
            "phone-install",
            "sim-1",
            MMS_COMPLETED_RECHECK_REST_MS,
            charging(),
        )
        .unwrap();
    assert_eq!(completed_ids(&last.requests), ["1"]);
}

#[test]
fn completed_rest_starts_after_the_whole_sweep_not_per_row() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "gateway", &vault), &vault);
    for id in ["1", "2"] {
        let acquisition = client.begin_mms_acquisition(input(id)).unwrap();
        client
            .complete_mms_acquisition(&acquisition.acquisition_id)
            .unwrap();
    }
    let round = client
        .mms_backfill_due_rechecks("phone-install", "sim-1", 1_000, charging())
        .unwrap();
    assert_eq!(completed_ids(&round.requests), ["2", "1"]);
    for request in &round.requests {
        client
            .record_mms_backfill_recheck_outcome(
                &request.acquisition_id,
                1_000,
                MmsBackfillRecheckOutcome::Progress,
                charging(),
            )
            .unwrap();
    }
    let finished = client
        .mms_backfill_due_rechecks("phone-install", "sim-1", 1_000, charging())
        .unwrap();
    assert!(finished.requests.is_empty());
    assert!(!finished.continuation_due);
    assert!(
        client
            .mms_backfill_due_rechecks(
                "phone-install",
                "sim-1",
                1_000 + MMS_COMPLETED_RECHECK_REST_MS - 1,
                charging(),
            )
            .unwrap()
            .requests
            .is_empty(),
        "the 24-hour rest begins after the sweep, not after each row"
    );
    assert_eq!(
        completed_ids(
            &client
                .mms_backfill_due_rechecks(
                    "phone-install",
                    "sim-1",
                    1_000 + MMS_COMPLETED_RECHECK_REST_MS,
                    charging(),
                )
                .unwrap()
                .requests
        ),
        ["2", "1"]
    );
}

#[test]
fn clock_rollback_after_completion_starts_a_fresh_bounded_round() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "gateway", &vault), &vault);
    let acquisition = client.begin_mms_acquisition(input("1")).unwrap();
    client
        .complete_mms_acquisition(&acquisition.acquisition_id)
        .unwrap();
    let now = 5 * MMS_COMPLETED_RECHECK_REST_MS;
    let batch = client
        .mms_backfill_due_rechecks("phone-install", "sim-1", now, charging())
        .unwrap();
    client
        .record_mms_backfill_recheck_outcome(
            &batch.requests[0].acquisition_id,
            now,
            MmsBackfillRecheckOutcome::Progress,
            charging(),
        )
        .unwrap();
    assert!(
        client
            .mms_backfill_due_rechecks("phone-install", "sim-1", now, charging())
            .unwrap()
            .requests
            .is_empty()
    );

    let rolled_back = client
        .mms_backfill_due_rechecks("phone-install", "sim-1", now - 1_000, charging())
        .unwrap();
    assert_eq!(completed_ids(&rolled_back.requests), ["1"]);
}

#[test]
fn explicit_refresh_restarts_one_resting_round_and_not_an_active_cursor() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "gateway", &vault), &vault);
    for id in ["1", "2"] {
        let acquisition = client.begin_mms_acquisition(input(id)).unwrap();
        client
            .complete_mms_acquisition(&acquisition.acquisition_id)
            .unwrap();
    }
    let explicit = MmsBackfillRecheckConditions {
        explicit_refresh: true,
        ..Default::default()
    };
    let round = client
        .mms_backfill_due_rechecks("phone-install", "sim-1", 1_000, charging())
        .unwrap();
    for request in &round.requests {
        client
            .record_mms_backfill_recheck_outcome(
                &request.acquisition_id,
                1_000,
                MmsBackfillRecheckOutcome::Progress,
                charging(),
            )
            .unwrap();
    }
    assert!(
        client
            .mms_backfill_due_rechecks("phone-install", "sim-1", 1_000, charging())
            .unwrap()
            .requests
            .is_empty()
    );

    let refreshed = client
        .mms_backfill_due_rechecks("phone-install", "sim-1", 1_001, explicit)
        .unwrap();
    assert_eq!(completed_ids(&refreshed.requests), ["2", "1"]);
    let repeated = client
        .mms_backfill_due_rechecks("phone-install", "sim-1", 1_001, explicit)
        .unwrap();
    assert_eq!(
        completed_ids(&repeated.requests),
        ["2", "1"],
        "a repeated manual call returns the same unresolved claims, not a fresh restart"
    );
    assert!(
        client
            .mms_backfill_due_rechecks("phone-install", "sim-1", 1_002, charging())
            .unwrap()
            .requests
            .iter()
            .all(|request| request.state == MmsAcquisitionState::Complete)
    );
}
