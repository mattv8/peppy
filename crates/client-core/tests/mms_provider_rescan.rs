use crate::common::*;
use tempfile::TempDir;

const GENERATION: &str = "phone-install";
const SCOPE: &str = "sim-1";

fn provider_rescan_plan(plans: &[MmsBackfillScanPlan]) -> Option<&MmsBackfillScanPlan> {
    plans
        .iter()
        .find(|plan| plan.phase == MmsBackfillPhase::ProviderRescan)
}

fn client() -> (TempDir, Vault, ClientConfig, Client) {
    let directory = TempDir::new().unwrap();
    let vault = Vault::new();
    let config = config(&directory, "provider-rescan", &vault);
    let client = unlocked(&config, &vault);
    (directory, vault, config, client)
}

fn imported_input(id: &str) -> MmsAcquisitionInput {
    MmsAcquisitionInput {
        source: MmsSource {
            source_generation: GENERATION.into(),
            subscription_id: SCOPE.into(),
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

fn reserve_capacity_after_plan(config: &ClientConfig) {
    let connection = rusqlite::Connection::open(&config.database_path).unwrap();
    connection
        .execute_batch(
            "PRAGMA key = \"x'0707070707070707070707070707070707070707070707070707070707070707'\";",
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO messages(local_order,id,conversation_id,source_device_id,source_sequence,payload,seen) VALUES(90000,'reserve-message','reserve-conversation','reserve-device',1,'{}',0)",
            [],
        )
        .unwrap();
}

#[test]
fn provider_rescan_is_explicit_and_has_an_independent_frozen_descending_cursor() {
    let (_directory, _vault, _config, client) = client();
    assert_eq!(
        client.request_mms_provider_rescan(GENERATION, SCOPE, "90"),
        Err(Error::InvalidRequest("MMS backfill scope"))
    );
    client
        .plan_mms_backfill_scans(GENERATION, SCOPE, "100")
        .unwrap();
    assert!(
        provider_rescan_plan(
            &client
                .plan_mms_backfill_scans(GENERATION, SCOPE, "101")
                .unwrap()
        )
        .is_none()
    );

    assert!(
        client
            .request_mms_provider_rescan(GENERATION, SCOPE, "90")
            .unwrap()
    );
    let plans = client
        .plan_mms_backfill_scans(GENERATION, SCOPE, "101")
        .unwrap();
    let rescan = provider_rescan_plan(&plans).unwrap();
    assert_eq!(rescan.history_upper_bound.as_deref(), Some("90"));
    assert_eq!(rescan.cursor, None);
    let token = rescan.provider_rescan_token.clone().unwrap();
    assert!(rescan.descending);
    assert_eq!(rescan.budget, MAX_MMS_BACKFILL_RECHECK);

    client
        .commit_mms_provider_rescan_page(GENERATION, SCOPE, &token, None, Some("80"), false)
        .unwrap();
    // A concurrent page planned before the lower cursor was committed must not move it back up.
    client
        .commit_mms_provider_rescan_page(GENERATION, SCOPE, &token, None, Some("85"), false)
        .unwrap();
    let plans = client
        .plan_mms_backfill_scans(GENERATION, SCOPE, "101")
        .unwrap();
    assert_eq!(
        provider_rescan_plan(&plans).unwrap().cursor.as_deref(),
        Some("80")
    );
    assert_eq!(
        plans
            .iter()
            .find(|plan| plan.phase == MmsBackfillPhase::Live)
            .unwrap()
            .cursor,
        Some("100".into())
    );
}

#[test]
fn repeated_provider_rescan_requests_coalesce_and_reopen_resume() {
    let (_directory, vault, config, client) = client();
    client
        .plan_mms_backfill_scans(GENERATION, SCOPE, "100")
        .unwrap();
    assert!(
        client
            .request_mms_provider_rescan(GENERATION, SCOPE, "90")
            .unwrap()
    );
    let token = provider_rescan_plan(
        &client
            .plan_mms_backfill_scans(GENERATION, SCOPE, "101")
            .unwrap(),
    )
    .unwrap()
    .provider_rescan_token
    .clone()
    .unwrap();
    client
        .commit_mms_provider_rescan_page(GENERATION, SCOPE, &token, None, Some("80"), false)
        .unwrap();
    assert!(
        !client
            .request_mms_provider_rescan(GENERATION, SCOPE, "99")
            .unwrap()
    );
    drop(client);

    let reopened = unlocked(&config, &vault);
    let plans = reopened
        .plan_mms_backfill_scans(GENERATION, SCOPE, "101")
        .unwrap();
    let rescan = provider_rescan_plan(&plans).unwrap();
    assert_eq!(rescan.history_upper_bound.as_deref(), Some("90"));
    assert_eq!(rescan.cursor.as_deref(), Some("80"));
    assert_eq!(
        rescan.provider_rescan_token.as_deref(),
        Some(token.as_str())
    );
}

#[test]
fn stale_or_unexhausted_rescan_finish_keeps_the_active_sweep() {
    let (_directory, _vault, _config, client) = client();
    client
        .plan_mms_backfill_scans(GENERATION, SCOPE, "100")
        .unwrap();
    client
        .request_mms_provider_rescan(GENERATION, SCOPE, "90")
        .unwrap();

    let token = provider_rescan_plan(
        &client
            .plan_mms_backfill_scans(GENERATION, SCOPE, "101")
            .unwrap(),
    )
    .unwrap()
    .provider_rescan_token
    .clone()
    .unwrap();
    assert!(
        !client
            .commit_mms_provider_rescan_page(GENERATION, SCOPE, &token, Some("1"), None, true)
            .unwrap()
    );
    assert!(
        provider_rescan_plan(
            &client
                .plan_mms_backfill_scans(GENERATION, SCOPE, "101")
                .unwrap()
        )
        .is_some()
    );
    assert!(
        client
            .commit_mms_provider_rescan_page(GENERATION, SCOPE, &token, None, None, true)
            .unwrap()
    );
    assert!(
        provider_rescan_plan(
            &client
                .plan_mms_backfill_scans(GENERATION, SCOPE, "101")
                .unwrap()
        )
        .is_none()
    );
}

#[test]
fn provider_rescan_is_gated_by_history_capacity_even_after_ordinary_history_completes() {
    let (_directory, _vault, _config, client) = client();
    client
        .plan_mms_backfill_scans(GENERATION, SCOPE, "100")
        .unwrap();
    assert!(
        client
            .finish_mms_backfill_phase(GENERATION, SCOPE, MmsBackfillPhase::History, "100", true)
            .unwrap()
    );
    client
        .report_mms_backfill_vault_quota_exceeded(GENERATION, SCOPE)
        .unwrap();
    client
        .request_mms_provider_rescan(GENERATION, SCOPE, "90")
        .unwrap();
    let plans = client
        .plan_mms_backfill_scans(GENERATION, SCOPE, "101")
        .unwrap();
    let rescan = provider_rescan_plan(&plans).unwrap();
    assert!(rescan.paused);
    assert_eq!(rescan.pause, Some(MmsBackfillPause::RemoteStorage));
}

#[test]
fn ordinary_completed_history_does_not_open_a_rescan_gate_without_a_rescan() {
    let (directory, vault, config, client) = client();
    client
        .migrate_mms_backfill_state(
            GENERATION,
            SCOPE,
            MmsLegacyBackfillState {
                history_baseline: Some("100".into()),
                ..Default::default()
            },
        )
        .unwrap();
    assert!(
        client
            .finish_mms_backfill_phase(GENERATION, SCOPE, MmsBackfillPhase::History, "100", true)
            .unwrap()
    );
    assert!(
        provider_rescan_plan(
            &client
                .plan_mms_backfill_scans(GENERATION, SCOPE, "101")
                .unwrap()
        )
        .is_none()
    );
    drop(client);

    let connection = rusqlite::Connection::open(&config.database_path).unwrap();
    connection
        .execute_batch(
            "PRAGMA key = \"x'0707070707070707070707070707070707070707070707070707070707070707'\";",
        )
        .unwrap();
    let gate_exists: bool = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM history_import_gate)",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(!gate_exists);
    drop(connection);
    drop(vault);
    drop(directory);
}

#[test]
fn stale_token_and_cursor_cannot_commit_a_restarted_rescan() {
    let (_directory, _vault, _config, client) = client();
    client
        .plan_mms_backfill_scans(GENERATION, SCOPE, "100")
        .unwrap();
    client
        .request_mms_provider_rescan(GENERATION, SCOPE, "90")
        .unwrap();
    let first = provider_rescan_plan(
        &client
            .plan_mms_backfill_scans(GENERATION, SCOPE, "101")
            .unwrap(),
    )
    .unwrap()
    .provider_rescan_token
    .clone()
    .unwrap();
    assert!(
        client
            .commit_mms_provider_rescan_page(GENERATION, SCOPE, &first, None, None, true)
            .unwrap()
    );
    assert!(
        client
            .request_mms_provider_rescan(GENERATION, SCOPE, "90")
            .unwrap()
    );
    let second = provider_rescan_plan(
        &client
            .plan_mms_backfill_scans(GENERATION, SCOPE, "101")
            .unwrap(),
    )
    .unwrap()
    .provider_rescan_token
    .clone()
    .unwrap();
    assert_ne!(first, second);
    assert!(
        !client
            .commit_mms_provider_rescan_page(GENERATION, SCOPE, &first, None, Some("80"), false)
            .unwrap()
    );
    assert!(
        client
            .commit_mms_provider_rescan_page(GENERATION, SCOPE, &second, None, Some("80"), false)
            .unwrap()
    );
    assert!(
        !client
            .commit_mms_provider_rescan_page(GENERATION, SCOPE, &second, None, Some("70"), false)
            .unwrap()
    );
    assert!(
        client
            .commit_mms_provider_rescan_page(
                GENERATION,
                SCOPE,
                &second,
                Some("80"),
                Some("70"),
                false
            )
            .unwrap()
    );
}

#[test]
fn reserve_after_planning_rejects_new_imported_rows_but_allows_live_rows() {
    let (_directory, vault, config, client) = client();
    client
        .plan_mms_backfill_scans(GENERATION, SCOPE, "100")
        .unwrap();
    client
        .record_recovery_snapshot_capacity(Cursor(0), 0)
        .unwrap();
    drop(client);
    reserve_capacity_after_plan(&config);
    let client = unlocked(&config, &vault);

    assert_eq!(
        client.begin_mms_acquisition(imported_input("imported")),
        Err(Error::MmsAcquisitionLimit)
    );
    let mut live = imported_input("live");
    live.imported = false;
    assert_eq!(
        client.begin_mms_acquisition(live).unwrap().state,
        MmsAcquisitionState::Pending
    );
}

#[test]
fn completed_imported_replay_remains_single_acquisition_at_reserve_capacity() {
    let (_directory, vault, config, client) = client();
    let input = imported_input("replay");
    let acquired = client.begin_mms_acquisition(input.clone()).unwrap();
    client
        .complete_mms_acquisition(&acquired.acquisition_id)
        .unwrap();
    client
        .record_recovery_snapshot_capacity(Cursor(0), 0)
        .unwrap();
    drop(client);
    reserve_capacity_after_plan(&config);
    let client = unlocked(&config, &vault);

    let replay = client.begin_mms_acquisition(input).unwrap();
    assert_eq!(replay.acquisition_id, acquired.acquisition_id);
    assert_eq!(replay.state, MmsAcquisitionState::Complete);
}
