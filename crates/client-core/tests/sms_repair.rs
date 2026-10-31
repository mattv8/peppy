//! Regression coverage for the final SMS repair wave, using provider-shaped evidence: Android SMS
//! `date` is milliseconds, outgoing `date_sent` is 0, and incoming `date_sent` is the SMSC time.

use crate::common::*;
use std::time::{SystemTime, UNIX_EPOCH};
use tempfile::TempDir;

const GENERATION: &str = "install-1";
const SIM: &str = "sim-1";
const PEPPY: &str = "dev.peppy.mobile";
const MESSAGES_APP: &str = "com.google.android.apps.messaging";

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

fn row(
    id: &str,
    direction: Direction,
    body: &str,
    date: Option<i64>,
    date_sent: Option<i64>,
) -> SmsAcquisitionInput {
    SmsAcquisitionInput {
        source: SmsSource {
            source_generation: GENERATION.into(),
            subscription_id: SIM.into(),
            provider_message_id: id.into(),
            provider_thread_id: Some("7".into()),
            broadcast_id: None,
        },
        direction,
        sender_address: (direction == Direction::Incoming).then(|| ADDRESS.into()),
        recipients: if direction == Direction::Outgoing {
            vec![ADDRESS.into()]
        } else {
            vec![]
        },
        body: body.into(),
        imported: false,
        sent_at_ms: date,
        date_sent_ms: date_sent,
        creator: None,
        source_row_fingerprint: format!("fp:{id}:{date:?}"),
    }
}

fn capture(client: &Client, input: SmsAcquisitionInput) -> Captured {
    let acquisition = client.begin_sms_acquisition(input).unwrap();
    client
        .complete_sms_acquisition(&acquisition.acquisition_id)
        .unwrap()
}

/// C1: the gateway records its attempt time; the platform writes the Peppy-created provider row
/// with `date` near that time and `date_sent=0`. No command id is available from the provider.
#[test]
fn c1_peppy_sent_provider_row_with_zero_date_sent_reconciles_to_the_attempted_command() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let desktop_config = config(&dir, "desktop", &vault);
    let gateway_config = config(&dir, "gateway", &vault);
    let desktop = unlocked(&desktop_config, &vault);
    let gateway = unlocked(&gateway_config, &vault);
    let queued = desktop
        .queue_send(
            sms(ConversationId::new(), "on my way"),
            route(&gateway_config),
        )
        .unwrap();
    let mut server = Server::default();
    server.upload(&desktop);
    server.sync(&gateway);
    let PermitDecision::Permit(command) = gateway.begin_send_attempt(queued.command_id).unwrap()
    else {
        panic!("attempt must be permitted");
    };
    let platform_date = now_ms() + 350;
    let mut provider = row(
        "2",
        Direction::Outgoing,
        "on my way",
        Some(platform_date),
        Some(0),
    );
    provider.creator = Some(PEPPY.into());

    let acquisition = gateway.begin_sms_acquisition(provider).unwrap();
    let captured = gateway
        .complete_sms_acquisition(&acquisition.acquisition_id)
        .unwrap();

    assert!(captured.duplicate);
    assert_eq!(captured.message_id, command.message.record.message_id);
    assert_eq!(
        gateway
            .messages(command.message.record.conversation_id)
            .unwrap()
            .len(),
        1
    );

    // The same text later sent from the OEM messaging app is a separate, real message.
    let mut native = row(
        "3",
        Direction::Outgoing,
        "on my way",
        Some(platform_date + 60_000),
        Some(0),
    );
    native.creator = Some(MESSAGES_APP.into());
    let native = capture(&gateway, native);
    assert!(!native.duplicate);
    assert_ne!(native.message_id, command.message.record.message_id);
}

/// C3: provider zero timestamps are unknown, never 1970.
#[test]
fn c3_zero_date_sent_falls_back_to_the_positive_provider_date() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "gateway", &vault), &vault);
    let date = 1_790_875_448_413;
    let captured = capture(
        &client,
        row("4", Direction::Outgoing, "dated", Some(date), Some(0)),
    );
    let message = only_message(&client, captured.conversation_id);
    assert_eq!(message.payload.sent_at_ms, Some(date));

    let unknown = capture(
        &client,
        row("5", Direction::Outgoing, "undated", Some(0), Some(0)),
    );
    let message = client
        .messages(unknown.conversation_id)
        .unwrap()
        .into_iter()
        .find(|message| message.payload.record.message_id == unknown.message_id)
        .unwrap();
    assert_eq!(message.payload.sent_at_ms, None);
}

/// C4: undated live captures sort by local insertion time, consistently across latest, list,
/// pages and full history.
#[test]
fn c4_undated_live_and_dated_history_share_one_deterministic_order() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "gateway", &vault), &vault);
    // An undated capture (e.g. a legacy broadcast) arrives first...
    let undated = client
        .capture_incoming(incoming("undated live", "legacy-broadcast"))
        .unwrap();
    // ...then a dated provider row that is genuinely newer, and dated history that is older.
    let mut newer = row(
        "10",
        Direction::Incoming,
        "newer live",
        Some(now_ms() + 60_000),
        Some(now_ms() + 59_000),
    );
    newer.source.provider_thread_id = None;
    let newer = capture(&client, newer);
    let mut older = row(
        "11",
        Direction::Incoming,
        "old history",
        Some(1_600_000_000_000),
        Some(1_600_000_000_000),
    );
    older.imported = true;
    older.source.provider_thread_id = None;
    let older = capture(&client, older);
    assert_eq!(undated.conversation_id, newer.conversation_id);
    assert_eq!(undated.conversation_id, older.conversation_id);

    let all: Vec<_> = client
        .messages(undated.conversation_id)
        .unwrap()
        .into_iter()
        .map(|message| message.payload.body)
        .collect();
    assert_eq!(all, ["old history", "undated live", "newer live"]);
    let latest = client
        .latest_message(undated.conversation_id)
        .unwrap()
        .unwrap();
    assert_eq!(latest.payload.body, "newer live");

    // Another conversation whose only message is undated but inserted now must sort above a
    // conversation whose newest dated message is old.
    let mut stale = row(
        "12",
        Direction::Incoming,
        "stale",
        Some(1_500_000_000_000),
        Some(1_500_000_000_000),
    );
    stale.sender_address = Some("+15555550199".into());
    stale.source.provider_thread_id = None;
    stale.imported = true;
    let stale = capture(&client, stale);
    let mut fresh = incoming("fresh undated", "legacy-broadcast-2");
    fresh.sender_address = "+15555550177".into();
    let fresh = client.capture_incoming(fresh).unwrap();
    let order: Vec<_> = client
        .list_conversations()
        .unwrap()
        .into_iter()
        .map(|c| c.conversation_id)
        .collect();
    let fresh_at = order
        .iter()
        .position(|id| *id == fresh.conversation_id)
        .unwrap();
    let stale_at = order
        .iter()
        .position(|id| *id == stale.conversation_id)
        .unwrap();
    assert!(
        fresh_at < stale_at,
        "undated live conversation must sort above old history"
    );

    // Pages walk the same comparator without omission or duplication.
    let mut seen = Vec::new();
    let mut before = None;
    loop {
        let page = client
            .messages_page(undated.conversation_id, before, 1)
            .unwrap();
        for message in page.messages.iter().rev() {
            seen.push(message.payload.body.clone());
        }
        before = page.messages.first().map(|m| m.payload.record.message_id);
        if !page.has_more {
            break;
        }
    }
    seen.reverse();
    assert_eq!(seen, all);
    let _ = older;
}

/// C5: the first plan seeds the live checkpoint at the frozen baseline.
#[test]
fn c5_first_plan_seeds_live_checkpoint_at_history_baseline() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "gateway", &vault), &vault);
    let plans = client
        .plan_sms_scans(GENERATION, "android-sms-provider", "100")
        .unwrap();
    let live = plans.iter().find(|plan| !plan.imported).unwrap();
    let history = plans.iter().find(|plan| plan.imported).unwrap();
    assert_eq!(live.checkpoint.as_deref(), Some("100"));
    assert_eq!(history.checkpoint, None);
    assert_eq!(history.history_upper_bound.as_deref(), Some("100"));
    // Later baselines never move either value.
    let plans = client
        .plan_sms_scans(GENERATION, "android-sms-provider", "250")
        .unwrap();
    assert_eq!(
        plans
            .iter()
            .find(|plan| !plan.imported)
            .unwrap()
            .checkpoint
            .as_deref(),
        Some("100")
    );
}

/// C6: completed bookkeeping is not a lifetime cap.
#[test]
fn c6_more_than_a_thousand_completed_history_rows_continue() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = open(&config(&dir, "gateway", &vault));
    for index in 0..1_005 {
        let mut input = row(
            &index.to_string(),
            Direction::Outgoing,
            "history",
            Some(1_600_000_000_000 + index),
            Some(0),
        );
        input.imported = true;
        capture(&client, input);
    }
}

/// C7: a malformed provider row is durably recorded as unavailable so the scan can advance.
#[test]
fn c7_malformed_provider_rows_are_recorded_unavailable_instead_of_failing() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "gateway", &vault), &vault);
    let mut missing = row("20", Direction::Incoming, "no sender", Some(1), Some(1));
    missing.sender_address = None;
    let missing = client.begin_sms_acquisition(missing).unwrap();
    assert_eq!(missing.state, SmsAcquisitionState::Unavailable);
    let mut long = row("21", Direction::Outgoing, "bad address", Some(1), Some(0));
    long.recipients = vec!["9".repeat(4_000)];
    let long = client.begin_sms_acquisition(long).unwrap();
    assert_eq!(long.state, SmsAcquisitionState::Unavailable);
    assert!(
        client
            .complete_sms_acquisition(&long.acquisition_id)
            .is_err()
    );
    assert!(client.list_conversations().unwrap().is_empty());

    // An over-long draft later edited in place (same immutable evidence) becomes publishable and
    // is acquired afresh into the counterpart's conversation.
    let existing = capture(
        &client,
        row("22", Direction::Outgoing, "earlier", Some(1), Some(0)),
    );
    let mut oversized = row("23", Direction::Outgoing, "x", Some(2), Some(0));
    oversized.body = "x".repeat(70_000);
    let oversized_acquisition = client.begin_sms_acquisition(oversized.clone()).unwrap();
    assert_eq!(
        oversized_acquisition.state,
        SmsAcquisitionState::Unavailable
    );
    oversized.body = "edited".into();
    let edited = client.begin_sms_acquisition(oversized).unwrap();
    assert_eq!(edited.state, SmsAcquisitionState::Pending);
    assert_eq!(edited.conversation_id, existing.conversation_id);
    let edited = client
        .complete_sms_acquisition(&edited.acquisition_id)
        .unwrap();
    assert!(!edited.duplicate);
    assert_eq!(client.messages(existing.conversation_id).unwrap().len(), 2);
}

/// I2: history completion is the imported scope only.
#[test]
fn i2_history_status_completes_after_imported_exhaustion() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "gateway", &vault), &vault);
    client
        .plan_sms_scans(GENERATION, "android-sms-provider", "0")
        .unwrap();
    client
        .ack_sms_scan_exhausted(GENERATION, "android-sms-provider", true, "0")
        .unwrap();
    let status = client
        .sms_history_status(GENERATION, "android-sms-provider")
        .unwrap();
    assert!(status.history_complete);
    assert!(!status.scanning);
}

/// I3: mutable fields refresh in place; changed immutable evidence supersedes the stale row.
#[test]
fn i3_pending_acquisitions_refresh_and_are_superseded() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "gateway", &vault), &vault);
    let draft = row(
        "30",
        Direction::Outgoing,
        "draft v1",
        Some(1_700_000_000_000),
        Some(0),
    );
    let first = client.begin_sms_acquisition(draft.clone()).unwrap();
    let mut edited = draft.clone();
    edited.body = "draft v2".into();
    let refreshed = client.begin_sms_acquisition(edited).unwrap();
    assert_eq!(refreshed.acquisition_id, first.acquisition_id);
    assert_eq!(refreshed.input.body, "draft v2");

    let mut resent = draft;
    resent.sent_at_ms = Some(1_700_000_100_000);
    resent.source_row_fingerprint = "fp:30:resent".into();
    let resent = client.begin_sms_acquisition(resent).unwrap();
    let open: Vec<_> = client
        .sms_acquisitions_after(None, 100)
        .unwrap()
        .into_iter()
        .map(|item| item.acquisition_id)
        .collect();
    assert_eq!(open, vec![resent.acquisition_id]);
}

fn snapshot_import(target: &Client, server: &Server) {
    let records: Vec<SnapshotRecord> = server
        .log
        .iter()
        .enumerate()
        .map(|(index, envelope)| SnapshotRecord {
            cursor: Cursor(index as u64 + 1),
            envelope: envelope.clone(),
        })
        .collect();
    let session = target
        .begin_snapshot(
            Cursor(records.len() as u64),
            records.len() as u64,
            SnapshotPurpose::Resync,
        )
        .unwrap();
    for page in records.chunks(2) {
        target
            .append_snapshot_page(session.generation, page)
            .unwrap();
    }
    target.finish_snapshot(session.generation).unwrap();
    target.apply_pending(100).unwrap();
}

fn bodies(client: &Client, conversation: ConversationId) -> Vec<String> {
    client
        .messages(conversation)
        .unwrap()
        .into_iter()
        .map(|message| message.payload.body)
        .collect()
}

/// C-A: an undated queued send keeps its origin position on every client: the sender, the
/// gateway, a device that syncs later, and a device restored from a snapshot later still.
#[test]
fn ca_origin_ordering_is_identical_across_synced_and_snapshot_restored_clients() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let desktop = unlocked(&config(&dir, "desktop-a", &vault), &vault);
    let gateway_config = config(&dir, "gateway", &vault);
    let gateway = unlocked(&gateway_config, &vault);
    let mut server = Server::default();
    let conversation = ConversationId::new();
    desktop
        .queue_send(sms(conversation, "leaving now"), route(&gateway_config))
        .unwrap();
    server.upload(&desktop);
    server.sync(&gateway);
    std::thread::sleep(std::time::Duration::from_millis(40));
    // The dated reply arrives at the phone after the send was queued.
    let reply_at = now_ms();
    let mut reply = row(
        "40",
        Direction::Incoming,
        "see you",
        Some(reply_at),
        Some(reply_at - 1_000),
    );
    reply.source.provider_thread_id = None;
    let reply = capture(&gateway, reply);
    assert_eq!(reply.conversation_id, conversation);
    server.upload(&gateway);
    server.sync(&desktop);
    std::thread::sleep(std::time::Duration::from_millis(40));
    // Devices that only see the history later must not place the send after the reply.
    let late = unlocked(&config(&dir, "desktop-b", &vault), &vault);
    server.sync(&late);
    let restored = unlocked(&config(&dir, "restored", &vault), &vault);
    snapshot_import(&restored, &server);

    let expected = ["leaving now", "see you"];
    for client in [&desktop, &gateway, &late, &restored] {
        assert_eq!(bodies(client, conversation), expected);
        assert_eq!(
            client
                .latest_message(conversation)
                .unwrap()
                .unwrap()
                .payload
                .body,
            "see you"
        );
        // The ordering time is never presented as an actual send time.
        let send = client
            .messages(conversation)
            .unwrap()
            .into_iter()
            .find(|message| message.payload.body == "leaving now")
            .unwrap();
        assert_eq!(send.payload.sent_at_ms, None);
    }
}

/// C-B: two identical legacy Peppy sends (attempts recorded before attempt times existed) make
/// their provider rows ambiguous. A due sweep re-observing them must keep them terminal.
#[test]
fn cb_ambiguous_legacy_attempts_stay_terminal_across_due_sweeps() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let desktop = unlocked(&config(&dir, "desktop", &vault), &vault);
    let gateway_config = config(&dir, "gateway", &vault);
    let gateway = unlocked(&gateway_config, &vault);
    let mut server = Server::default();
    let conversation = ConversationId::new();
    let first = desktop
        .queue_send(sms(conversation, "ok"), route(&gateway_config))
        .unwrap();
    let second = desktop
        .queue_send(sms(conversation, "ok"), route(&gateway_config))
        .unwrap();
    server.upload(&desktop);
    server.sync(&gateway);
    for command in [first.command_id, second.command_id] {
        assert!(matches!(
            gateway.begin_send_attempt(command).unwrap(),
            PermitDecision::Permit(_)
        ));
    }
    drop(gateway);
    // Pre-migration attempts carry no attempt time.
    let conn = rusqlite::Connection::open(&gateway_config.database_path).unwrap();
    conn.execute_batch(
        "PRAGMA key = \"x'0707070707070707070707070707070707070707070707070707070707070707'\"; UPDATE attempts SET attempted_at_ms=NULL;",
    )
    .unwrap();
    drop(conn);
    let gateway = unlocked(&gateway_config, &vault);
    gateway
        .plan_sms_scans(GENERATION, "android-sms-provider", "0")
        .unwrap();
    let rows: Vec<_> = ["1", "2"]
        .into_iter()
        .map(|id| {
            let mut input = row(id, Direction::Outgoing, "ok", Some(now_ms()), Some(0));
            input.creator = Some(PEPPY.into());
            input
        })
        .collect();
    for input in &rows {
        let acquisition = gateway.begin_sms_acquisition(input.clone()).unwrap();
        assert_eq!(acquisition.state, SmsAcquisitionState::Unavailable);
        assert_eq!(acquisition.reason.as_deref(), Some(SMS_REASON_AMBIGUOUS));
    }
    // The first plan defers; once the interval has passed a sweep is due.
    assert!(
        gateway
            .plan_sms_reconciliation_at(GENERATION, "android-sms-provider", "2", None, now_ms())
            .unwrap()
            .is_none()
    );
    let due = gateway
        .plan_sms_reconciliation_at(
            GENERATION,
            "android-sms-provider",
            "2",
            None,
            now_ms() + SMS_RECONCILIATION_INTERVAL_MS,
        )
        .unwrap();
    assert!(due.is_some());
    for _ in 0..2 {
        for input in &rows {
            let again = gateway.begin_sms_acquisition(input.clone()).unwrap();
            assert_eq!(again.state, SmsAcquisitionState::Unavailable);
            assert_eq!(again.reason.as_deref(), Some(SMS_REASON_AMBIGUOUS));
        }
    }
    assert!(gateway.sms_acquisitions_after(None, 10).unwrap().is_empty());
    let status = gateway
        .sms_history_status(GENERATION, "android-sms-provider")
        .unwrap();
    assert_eq!((status.pending_count, status.ambiguous_count), (0, 2));
    // Neither send was merged arbitrarily, and no provider message was invented.
    assert_eq!(gateway.messages(conversation).unwrap().len(), 2);
}

fn completed_sweep(client: &Client, high_water: &str) {
    let deferred = now_ms();
    assert_eq!(
        client
            .plan_sms_reconciliation_at(
                GENERATION,
                "android-sms-provider",
                high_water,
                None,
                deferred
            )
            .unwrap(),
        None
    );
    let plan = client
        .plan_sms_reconciliation_at(
            GENERATION,
            "android-sms-provider",
            high_water,
            None,
            deferred + SMS_RECONCILIATION_INTERVAL_MS,
        )
        .unwrap()
        .unwrap();
    client
        .complete_sms_reconciliation(
            GENERATION,
            "android-sms-provider",
            &plan.upper_bound,
            None,
            true,
        )
        .unwrap();
}

/// I-A: reused provider IDs start a bounded sweep promptly despite the restart throttle, and
/// unchanged provider state never restarts one.
#[test]
fn ia_reused_ids_trigger_a_prompt_sweep_without_a_restart_storm() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "gateway", &vault), &vault);
    let scope = "android-sms-provider";
    client.plan_sms_scans(GENERATION, scope, "0").unwrap();
    // Live rows 1..=5 captured and checkpointed.
    for id in 1..=5 {
        capture(
            &client,
            row(
                &id.to_string(),
                Direction::Outgoing,
                "x",
                Some(1_700_000_000_000 + id),
                Some(0),
            ),
        );
        client
            .set_sms_scan_checkpoint(GENERATION, scope, false, &id.to_string())
            .unwrap();
    }
    completed_sweep(&client, "5");
    // Unchanged state: neither a missing anchor nor the known anchor restarts a sweep.
    for _ in 0..3 {
        assert_eq!(
            client
                .plan_sms_reconciliation(GENERATION, scope, "5", None)
                .unwrap(),
            None
        );
        assert_eq!(
            client
                .plan_sms_reconciliation(GENERATION, scope, "5", Some("fp:5:Some(1700000000005)"))
                .unwrap(),
            None
        );
    }

    // Same high-water, replaced anchor row (row 5 deleted, new send reused ID 5).
    let replaced = client
        .plan_sms_reconciliation(GENERATION, scope, "5", Some("fp:5:replacement"))
        .unwrap();
    assert_eq!(
        replaced.as_ref().map(|plan| plan.upper_bound.as_str()),
        Some("5")
    );
    // The sweep's own re-observation acquires the new row; after it completes no storm follows.
    let mut reobserved = row(
        "5",
        Direction::Outgoing,
        "reused",
        Some(1_700_000_100_000),
        Some(0),
    );
    reobserved.source_row_fingerprint = "fp:5:replacement".into();
    capture(&client, reobserved);
    client
        .complete_sms_reconciliation(GENERATION, scope, "5", None, true)
        .unwrap();
    for _ in 0..3 {
        assert_eq!(
            client
                .plan_sms_reconciliation(GENERATION, scope, "5", Some("fp:5:replacement"))
                .unwrap(),
            None
        );
    }

    // High-water regression (rows deleted, IDs restart lower): prompt sweep, live resumes at 3.
    let regressed = client
        .plan_sms_reconciliation(GENERATION, scope, "3", None)
        .unwrap()
        .unwrap();
    assert_eq!(regressed.upper_bound, "3");
    let live = client.plan_sms_scans(GENERATION, scope, "3").unwrap();
    assert_eq!(
        live.iter()
            .find(|plan| !plan.imported)
            .unwrap()
            .checkpoint
            .as_deref(),
        Some("3")
    );
    client
        .complete_sms_reconciliation(GENERATION, scope, "3", None, true)
        .unwrap();
    assert_eq!(
        client
            .plan_sms_reconciliation(GENERATION, scope, "3", None)
            .unwrap(),
        None
    );
}
