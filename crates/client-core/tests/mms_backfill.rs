//! Durable MMS provider-history lifecycle, wired by the core module export.
mod common;

use common::*;
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
        body: "history".into(),
        imported: true,
        observed_at_ms: 0,
        transaction_id: None,
    }
}

fn history_plan(plans: &[MmsBackfillScanPlan]) -> &MmsBackfillScanPlan {
    plans
        .iter()
        .find(|plan| plan.phase == MmsBackfillPhase::History)
        .unwrap()
}

fn live_plan(plans: &[MmsBackfillScanPlan]) -> &MmsBackfillScanPlan {
    plans
        .iter()
        .find(|plan| plan.phase == MmsBackfillPhase::Live)
        .unwrap()
}

#[test]
fn absent_migration_is_not_zero_and_restart_keeps_frozen_baseline() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let cfg = config(&dir, "gateway", &vault);
    let client = unlocked(&cfg, &vault);
    client
        .migrate_mms_backfill_state("phone-install", "sim-1", MmsLegacyBackfillState::default())
        .unwrap();
    assert!(
        !client
            .mms_backfill_status("phone-install", "sim-1")
            .unwrap()
            .baseline_initialized
    );
    let plans = client
        .plan_mms_backfill_scans("phone-install", "sim-1", "19")
        .unwrap();
    assert_eq!(
        history_plan(&plans).history_upper_bound.as_deref(),
        Some("19")
    );
    drop(client);
    let reopened = unlocked(&cfg, &vault);
    assert_eq!(
        history_plan(
            &reopened
                .plan_mms_backfill_scans("phone-install", "sim-1", "25")
                .unwrap()
        )
        .history_upper_bound
        .as_deref(),
        Some("19")
    );
}

#[test]
fn explicit_zero_migration_and_reenable_preserve_progress() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let cfg = config(&dir, "gateway", &vault);
    let client = unlocked(&cfg, &vault);
    client
        .migrate_mms_backfill_state(
            "phone-install",
            "sim-1",
            MmsLegacyBackfillState {
                history_baseline: Some("0".into()),
                import_history: Some(true),
                recheck_cursor: None,
            },
        )
        .unwrap();
    client
        .advance_mms_backfill_page("phone-install", "sim-1", MmsBackfillPhase::History, "4")
        .unwrap();
    let plans = client
        .plan_mms_backfill_scans("phone-install", "sim-1", "99")
        .unwrap();
    let plan = history_plan(&plans);
    assert_eq!(plan.history_upper_bound.as_deref(), Some("0"));
    assert_eq!(plan.cursor.as_deref(), Some("4"));
}

#[test]
fn legacy_false_never_skips_history_without_rust_owned_exhaustion() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "gateway", &vault), &vault);
    client
        .migrate_mms_backfill_state(
            "phone-install",
            "sim-1",
            MmsLegacyBackfillState {
                history_baseline: Some("100".into()),
                import_history: Some(false),
                recheck_cursor: None,
            },
        )
        .unwrap();
    let plans = client
        .plan_mms_backfill_scans("phone-install", "sim-1", "101")
        .unwrap();
    let history = history_plan(&plans);
    assert!(!history.history_complete);
    assert_eq!(
        history.cursor, None,
        "no legacy imported checkpoint means scan from the start"
    );
    assert_eq!(history.history_upper_bound.as_deref(), Some("100"));
}

#[test]
fn legacy_false_with_no_baseline_starts_a_fresh_history_scan() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "gateway", &vault), &vault);
    client
        .migrate_mms_backfill_state(
            "phone-install",
            "sim-1",
            MmsLegacyBackfillState {
                history_baseline: None,
                import_history: Some(false),
                recheck_cursor: None,
            },
        )
        .unwrap();
    let plans = client
        .plan_mms_backfill_scans("phone-install", "sim-1", "100")
        .unwrap();
    assert!(!history_plan(&plans).history_complete);
    assert_eq!(
        history_plan(&plans).history_upper_bound.as_deref(),
        Some("100")
    );
}

#[test]
fn legacy_checkpoint_is_retained_until_safe_exhaustion_confirms_completion() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "gateway", &vault), &vault);
    client
        .migrate_mms_backfill_state(
            "phone-install",
            "sim-1",
            MmsLegacyBackfillState {
                history_baseline: Some("100".into()),
                import_history: Some(false),
                recheck_cursor: None,
            },
        )
        .unwrap();
    client
        .advance_mms_backfill_page("phone-install", "sim-1", MmsBackfillPhase::History, "100")
        .unwrap();
    let plans = client
        .plan_mms_backfill_scans("phone-install", "sim-1", "101")
        .unwrap();
    assert_eq!(history_plan(&plans).cursor.as_deref(), Some("100"));
    assert!(!history_plan(&plans).history_complete);
    assert!(
        client
            .finish_mms_backfill_phase(
                "phone-install",
                "sim-1",
                MmsBackfillPhase::History,
                "100",
                true
            )
            .unwrap()
    );
}

#[test]
fn legacy_uuid_recheck_cursor_resolves_or_safely_restarts() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "gateway", &vault), &vault);
    let acquisition = client.begin_mms_acquisition(input("7")).unwrap();
    client
        .migrate_mms_backfill_state(
            "phone-install",
            "sim-1",
            MmsLegacyBackfillState {
                history_baseline: Some("100".into()),
                import_history: Some(false),
                recheck_cursor: Some(acquisition.acquisition_id),
            },
        )
        .unwrap();
    assert_eq!(
        client
            .mms_backfill_status("phone-install", "sim-1")
            .unwrap()
            .recheck_cursor,
        Some("1".into())
    );

    let second_dir = TempDir::new().unwrap();
    let second_vault = Vault::new();
    let second = unlocked(
        &config(&second_dir, "gateway", &second_vault),
        &second_vault,
    );
    second
        .migrate_mms_backfill_state(
            "phone-install",
            "sim-1",
            MmsLegacyBackfillState {
                history_baseline: Some("100".into()),
                import_history: Some(false),
                recheck_cursor: Some("3d6f0a55-f2e5-4b04-8bb8-b6dc9ccfe544".into()),
            },
        )
        .unwrap();
    assert_eq!(
        second
            .mms_backfill_status("phone-install", "sim-1")
            .unwrap()
            .recheck_cursor,
        None
    );
}

#[test]
fn capacity_pauses_resume_only_when_the_matching_capacity_changes() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "gateway", &vault), &vault);
    client
        .plan_mms_backfill_scans("phone-install", "sim-1", "5")
        .unwrap();
    client
        .report_mms_backfill_vault_quota_exceeded("phone-install", "sim-1")
        .unwrap();
    assert_eq!(
        client
            .mms_backfill_status("phone-install", "sim-1")
            .unwrap()
            .pause,
        Some(MmsBackfillPause::RemoteStorage)
    );
    client
        .report_mms_backfill_remote_capacity("phone-install", "sim-1", true)
        .unwrap();
    assert_eq!(
        client
            .mms_backfill_status("phone-install", "sim-1")
            .unwrap()
            .pause,
        None
    );
    client
        .pause_mms_backfill_for_local_capacity("phone-install", "sim-1")
        .unwrap();
    client
        .report_mms_backfill_remote_capacity("phone-install", "sim-1", true)
        .unwrap();
    assert_eq!(
        client
            .mms_backfill_status("phone-install", "sim-1")
            .unwrap()
            .pause,
        Some(MmsBackfillPause::LocalCapacity)
    );
}

#[test]
fn history_never_completes_while_pending_and_rechecks_rotate() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "gateway", &vault), &vault);
    let plans = client
        .plan_mms_backfill_scans("phone-install", "sim-1", "8")
        .unwrap();
    let bound = history_plan(&plans).history_upper_bound.clone().unwrap();
    let first = client.begin_mms_acquisition(input("1")).unwrap();
    client.begin_mms_acquisition(input("2")).unwrap();
    assert!(
        !client
            .finish_mms_backfill_phase(
                "phone-install",
                "sim-1",
                MmsBackfillPhase::History,
                &bound,
                true
            )
            .unwrap()
    );
    let pass = client
        .mms_backfill_recheck_requests("phone-install", "sim-1")
        .unwrap();
    assert_eq!(pass.requests.len(), 2);
    assert!(pass.cursor.is_some());
    let next = client
        .mms_backfill_recheck_requests("phone-install", "sim-1")
        .unwrap();
    assert_eq!(
        next.requests.len(),
        2,
        "a bounded new round retries rather than spinning rows"
    );
    client
        .set_mms_acquisition_state(
            &first.acquisition_id,
            MmsAcquisitionState::Unavailable,
            Some("provider missing"),
        )
        .unwrap();
    let second = client
        .mms_acquisitions(10)
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
    client
        .set_mms_acquisition_state(
            &second.acquisition_id,
            MmsAcquisitionState::Unavailable,
            Some("provider missing"),
        )
        .unwrap();
    assert!(
        client
            .finish_mms_backfill_phase(
                "phone-install",
                "sim-1",
                MmsBackfillPhase::History,
                &bound,
                true
            )
            .unwrap()
    );
}

#[test]
fn provider_wide_scope_observes_actual_subscription_acquisitions() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "gateway", &vault), &vault);
    client
        .plan_mms_backfill_scans("phone-install", "android-mms-provider", "8")
        .unwrap();
    let mut pending = input("1");
    pending.source.subscription_id = "android-subscription:1".into();
    client.begin_mms_acquisition(pending).unwrap();
    let status = client
        .mms_backfill_status("phone-install", "android-mms-provider")
        .unwrap();
    assert_eq!(status.pending_count, 1);
    assert_eq!(
        client
            .mms_backfill_recheck_requests("phone-install", "android-mms-provider")
            .unwrap()
            .requests
            .len(),
        1
    );
}

#[test]
fn completed_provider_id_date_change_remains_the_existing_source() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let cfg = config(&dir, "gateway", &vault);
    let client = unlocked(&cfg, &vault);
    client
        .plan_mms_backfill_scans("phone-install", "sim-1", "9")
        .unwrap();
    let acquisition = client.begin_mms_acquisition(input("9")).unwrap();
    client
        .complete_mms_acquisition(&acquisition.acquisition_id)
        .unwrap();

    let mut reused = input("9");
    reused.observed_at_ms = 42;
    assert_eq!(
        client.begin_mms_acquisition(reused).unwrap().acquisition_id,
        acquisition.acquisition_id
    );
    assert_eq!(
        client
            .mms_backfill_status("phone-install", "sim-1")
            .unwrap()
            .pending_count,
        0
    );
    assert!(
        client
            .finish_mms_backfill_phase(
                "phone-install",
                "sim-1",
                MmsBackfillPhase::History,
                "9",
                true
            )
            .unwrap()
    );
    drop(client);
    assert_eq!(
        unlocked(&cfg, &vault)
            .mms_backfill_status("phone-install", "sim-1")
            .unwrap()
            .pending_count,
        0
    );
}

#[test]
fn live_checkpoint_rewinds_on_provider_regression_without_moving_history_baseline() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "gateway", &vault), &vault);
    let plans = client
        .plan_mms_backfill_scans("phone-install", "android-mms-provider", "100")
        .unwrap();
    assert_eq!(
        history_plan(&plans).history_upper_bound.as_deref(),
        Some("100")
    );
    client
        .advance_mms_backfill_page(
            "phone-install",
            "android-mms-provider",
            MmsBackfillPhase::Live,
            "101",
        )
        .unwrap();
    let plans = client
        .plan_mms_backfill_scans("phone-install", "android-mms-provider", "3")
        .unwrap();
    assert_eq!(live_plan(&plans).cursor.as_deref(), Some("3"));
    assert_eq!(
        history_plan(&plans).history_upper_bound.as_deref(),
        Some("100")
    );
}

#[test]
fn completed_rows_recheck_and_enriched_repeats_remain_known_sources() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "gateway", &vault), &vault);
    client
        .plan_mms_backfill_scans("phone-install", "sim-1", "10")
        .unwrap();
    let acquisition = client.begin_mms_acquisition(input("10")).unwrap();
    client
        .complete_mms_acquisition(&acquisition.acquisition_id)
        .unwrap();
    let mut enriched = input("10");
    enriched.imported = false;
    enriched.body = "decoded text part".into();
    assert!(client.begin_mms_acquisition(enriched).unwrap().state == MmsAcquisitionState::Complete);
    assert_eq!(
        client
            .mms_backfill_recheck_requests("phone-install", "sim-1")
            .unwrap()
            .requests[0]
            .provider_message_id,
        "10"
    );
}

#[test]
fn paused_rechecks_include_existing_backlog_and_fairly_mix_completed_rows() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "gateway", &vault), &vault);
    client
        .plan_mms_backfill_scans("phone-install", "sim-1", "40")
        .unwrap();
    let complete = client.begin_mms_acquisition(input("1")).unwrap();
    client
        .complete_mms_acquisition(&complete.acquisition_id)
        .unwrap();
    for id in 2..35 {
        client
            .begin_mms_acquisition(input(&id.to_string()))
            .unwrap();
    }
    client
        .pause_mms_backfill_for_local_capacity("phone-install", "sim-1")
        .unwrap();
    let pass = client
        .mms_backfill_recheck_requests("phone-install", "sim-1")
        .unwrap();
    assert!(pass.requests.len() <= MAX_MMS_BACKFILL_RECHECK);
    assert!(
        pass.requests
            .iter()
            .any(|request| request.provider_message_id == "1")
    );
}

#[test]
fn phase_pauses_keep_live_and_recheck_available_for_remote_capacity() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "gateway", &vault), &vault);
    client
        .plan_mms_backfill_scans("phone-install", "sim-1", "40")
        .unwrap();
    client
        .report_mms_backfill_vault_quota_exceeded("phone-install", "sim-1")
        .unwrap();
    let plans = client
        .plan_mms_backfill_scans("phone-install", "sim-1", "40")
        .unwrap();
    assert!(!live_plan(&plans).paused);
    assert!(history_plan(&plans).paused);
    let recheck = plans
        .iter()
        .find(|plan| plan.phase == MmsBackfillPhase::Recheck)
        .unwrap();
    assert!(!recheck.paused);
    assert_eq!(recheck.pause, Some(MmsBackfillPause::RemoteStorage));
}

#[test]
fn manual_recheck_reopens_only_bounded_terminal_work() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "gateway", &vault), &vault);
    let first = client.begin_mms_acquisition(input("1")).unwrap();
    let second = client.begin_mms_acquisition(input("2")).unwrap();
    client
        .set_mms_acquisition_state(
            &first.acquisition_id,
            MmsAcquisitionState::Unavailable,
            Some("provider missing"),
        )
        .unwrap();
    client
        .set_mms_acquisition_state(
            &second.acquisition_id,
            MmsAcquisitionState::Blocked,
            Some("unsupported"),
        )
        .unwrap();
    assert_eq!(
        client
            .request_mms_recheck("phone-install", "sim-1", 1)
            .unwrap(),
        1
    );
    let acquisitions = client.mms_acquisitions(10).unwrap();
    assert_eq!(
        acquisitions
            .iter()
            .filter(|acquisition| acquisition.state == MmsAcquisitionState::Pending)
            .count(),
        1
    );
}

#[test]
fn blocked_rows_are_terminal_for_capacity_status_and_history_completion() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "blocked-terminal", &vault), &vault);
    let plans = client
        .plan_mms_backfill_scans("phone-install", "sim-1", "8")
        .unwrap();
    let blocked = client.begin_mms_acquisition(input("1")).unwrap();
    client
        .set_mms_acquisition_state(
            &blocked.acquisition_id,
            MmsAcquisitionState::Blocked,
            Some("terminal provider limitation"),
        )
        .unwrap();

    assert_eq!(
        client
            .mms_backfill_status("phone-install", "sim-1")
            .unwrap()
            .pending_count,
        0
    );
    assert!(
        client
            .finish_mms_backfill_phase(
                "phone-install",
                "sim-1",
                MmsBackfillPhase::History,
                history_plan(&plans).history_upper_bound.as_deref().unwrap(),
                true,
            )
            .unwrap()
    );
}

#[test]
fn remote_pause_is_restored_after_local_acquisition_capacity_clears() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "capacity-precedence", &vault), &vault);
    client
        .plan_mms_backfill_scans("phone-install", "sim-1", "1001")
        .unwrap();
    client
        .report_mms_backfill_vault_quota_exceeded("phone-install", "sim-1")
        .unwrap();
    let mut first = None;
    for number in 0..MAX_MMS_ACQUISITIONS {
        let acquisition = client
            .begin_mms_acquisition(input(&number.to_string()))
            .unwrap();
        first.get_or_insert(acquisition.acquisition_id);
    }

    let plans = client
        .plan_mms_backfill_scans("phone-install", "sim-1", "1001")
        .unwrap();
    assert_eq!(
        live_plan(&plans).pause,
        Some(MmsBackfillPause::LocalCapacity)
    );
    assert!(live_plan(&plans).paused);
    assert!(
        !plans
            .iter()
            .find(|plan| plan.phase == MmsBackfillPhase::Recheck)
            .unwrap()
            .paused
    );
    client
        .report_mms_backfill_remote_capacity("phone-install", "sim-1", true)
        .unwrap();
    assert_eq!(
        client
            .mms_backfill_status("phone-install", "sim-1")
            .unwrap()
            .pause,
        Some(MmsBackfillPause::LocalCapacity)
    );
    client
        .report_mms_backfill_vault_quota_exceeded("phone-install", "sim-1")
        .unwrap();

    client
        .set_mms_acquisition_state(
            first.as_deref().unwrap(),
            MmsAcquisitionState::Unavailable,
            Some("retired"),
        )
        .unwrap();
    let plans = client
        .plan_mms_backfill_scans("phone-install", "sim-1", "1001")
        .unwrap();
    assert_eq!(
        live_plan(&plans).pause,
        Some(MmsBackfillPause::RemoteStorage)
    );
    assert!(!live_plan(&plans).paused);
    assert!(history_plan(&plans).paused);
}

#[test]
fn blocked_rows_do_not_disagree_with_pending_only_admission_capacity() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "pending-admission", &vault), &vault);
    client
        .plan_mms_backfill_scans("phone-install", "sim-1", "1001")
        .unwrap();
    let blocked = client.begin_mms_acquisition(input("blocked")).unwrap();
    client
        .set_mms_acquisition_state(
            &blocked.acquisition_id,
            MmsAcquisitionState::Blocked,
            Some("terminal"),
        )
        .unwrap();
    for number in 0..(MAX_MMS_ACQUISITIONS - 1) {
        client
            .begin_mms_acquisition(input(&format!("pending-{number}")))
            .unwrap();
    }

    assert!(
        !live_plan(
            &client
                .plan_mms_backfill_scans("phone-install", "sim-1", "1001")
                .unwrap()
        )
        .paused
    );
    client
        .begin_mms_acquisition(input("pending-final"))
        .unwrap();
    assert!(
        live_plan(
            &client
                .plan_mms_backfill_scans("phone-install", "sim-1", "1001")
                .unwrap()
        )
        .paused
    );
    assert_eq!(
        client.begin_mms_acquisition(input("pending-overflow")),
        Err(Error::MmsAcquisitionLimit)
    );
}
