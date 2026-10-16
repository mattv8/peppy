//! Provider SMS capture keeps provider identity separate from message content.
mod common;

use common::*;
use tempfile::TempDir;

fn provider_input(
    id: &str,
    body: &str,
    sent_at_ms: Option<i64>,
    broadcast_id: Option<&str>,
) -> SmsAcquisitionInput {
    SmsAcquisitionInput {
        source: SmsSource {
            source_generation: "install-1".into(),
            subscription_id: "sim-1".into(),
            provider_message_id: id.into(),
            provider_thread_id: Some("thread-1".into()),
            broadcast_id: broadcast_id.map(str::to_owned),
        },
        direction: Direction::Incoming,
        sender_address: Some(ADDRESS.into()),
        recipients: vec![],
        body: body.into(),
        imported: true,
        sent_at_ms,
        date_sent_ms: sent_at_ms,
        creator: None,
        source_row_fingerprint: format!("{id}:{body}:{sent_at_ms:?}:{broadcast_id:?}"),
    }
}

#[test]
fn broadcast_and_provider_capture_share_the_one_to_one_alias() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "client", &vault), &vault);

    // The receiver captures the broadcast under its deterministic identity...
    let mut sms = incoming("hello", "broadcast-9");
    sms.provider_message_id = Some("broadcast-9".into());
    let captured = client
        .capture_incoming_broadcast(sms, Some(10), Some(12))
        .unwrap();
    // ...and the provider row deriving the same identity reuses it exactly once.
    let provider = client
        .begin_sms_acquisition(provider_input(
            "provider-9",
            "hello",
            Some(12),
            Some("broadcast-9"),
        ))
        .unwrap();
    let second = client
        .complete_sms_acquisition(&provider.acquisition_id)
        .unwrap();
    assert!(second.duplicate);
    assert_eq!(second.message_id, captured.message_id);
    assert_eq!(client.messages(captured.conversation_id).unwrap().len(), 1);

    // A distinct provider row with the same identity (an identical text in the same second,
    // which the broadcast path could only see once) is not collapsed into the claimed message.
    let repeated = client
        .begin_sms_acquisition(provider_input(
            "provider-10",
            "hello",
            Some(12),
            Some("broadcast-9"),
        ))
        .unwrap();
    let repeated = client
        .complete_sms_acquisition(&repeated.acquisition_id)
        .unwrap();
    assert!(!repeated.duplicate);
    assert_eq!(client.messages(captured.conversation_id).unwrap().len(), 2);
}

#[test]
fn identical_provider_rows_with_distinct_provider_ids_are_preserved() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "client", &vault), &vault);
    let first = client
        .begin_sms_acquisition(provider_input("1", "same", Some(10), None))
        .unwrap();
    let second = client
        .begin_sms_acquisition(provider_input("2", "same", Some(10), None))
        .unwrap();
    let first = client
        .complete_sms_acquisition(&first.acquisition_id)
        .unwrap();
    let second = client
        .complete_sms_acquisition(&second.acquisition_id)
        .unwrap();

    assert_ne!(first.message_id, second.message_id);
    assert_eq!(client.messages(first.conversation_id).unwrap().len(), 2);
}

#[test]
fn timestamped_history_is_paginated_by_timestamp_not_arrival_order() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "client", &vault), &vault);
    let live = client
        .begin_sms_acquisition(provider_input("live", "live", Some(200), None))
        .unwrap();
    let history = client
        .begin_sms_acquisition(provider_input("history", "history", Some(100), None))
        .unwrap();
    let live = client
        .complete_sms_acquisition(&live.acquisition_id)
        .unwrap();
    client
        .complete_sms_acquisition(&history.acquisition_id)
        .unwrap();

    let page = client.messages_page(live.conversation_id, None, 1).unwrap();
    assert_eq!(page.messages[0].payload.body, "live");
    assert!(page.has_more);
    let older = client
        .messages_page(
            live.conversation_id,
            Some(page.messages[0].payload.record.message_id),
            1,
        )
        .unwrap();
    assert_eq!(older.messages[0].payload.body, "history");
}

/// Users see provider `date` (local receipt, the same clock as sends); the SMSC `date_sent`
/// is reconciliation evidence and can be skewed by the carrier.
#[test]
fn incoming_capture_displays_the_provider_receipt_date() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "client", &vault), &vault);
    let mut input = provider_input("smsc-date", "dated", Some(10), None);
    input.date_sent_ms = Some(20);
    let acquisition = client.begin_sms_acquisition(input).unwrap();
    let captured = client
        .complete_sms_acquisition(&acquisition.acquisition_id)
        .unwrap();

    assert_eq!(
        client.messages(captured.conversation_id).unwrap()[0]
            .payload
            .sent_at_ms,
        Some(10)
    );
}

#[test]
fn provider_id_reuse_with_distinct_evidence_creates_distinct_acquisitions() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "gateway", &vault), &vault);
    let first = client
        .begin_sms_acquisition(provider_input("42", "old", Some(100), None))
        .unwrap();
    let second = client
        .begin_sms_acquisition(provider_input("42", "new", Some(200), None))
        .unwrap();
    assert_ne!(first.acquisition_id, second.acquisition_id);
}

#[test]
fn own_creator_provider_row_reconciles_the_attempted_command_without_a_command_id() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let desktop_config = config(&dir, "desktop", &vault);
    let gateway_config = config(&dir, "gateway", &vault);
    let desktop = unlocked(&desktop_config, &vault);
    let gateway = unlocked(&gateway_config, &vault);
    let conversation = ConversationId::new();
    let queued = desktop
        .queue_send(
            sms(conversation, "provider-confirmed"),
            route(&gateway_config),
        )
        .unwrap();
    let mut server = Server::default();
    server.upload(&desktop);
    server.sync(&gateway);
    let PermitDecision::Permit(command) = gateway.begin_send_attempt(queued.command_id).unwrap()
    else {
        panic!("attempt must be available to the gateway");
    };

    let mut input = provider_input("provider-outgoing", "provider-confirmed", None, None);
    input.direction = Direction::Outgoing;
    input.sender_address = None;
    input.recipients = vec![ADDRESS.into()];
    input.imported = false;
    input.creator = Some("dev.peppy.mobile".into());
    let acquisition = gateway.begin_sms_acquisition(input).unwrap();

    assert_eq!(acquisition.state, SmsAcquisitionState::Complete);
    assert_eq!(
        acquisition.conversation_id,
        command.message.record.conversation_id
    );
    let captured = gateway
        .complete_sms_acquisition(&acquisition.acquisition_id)
        .unwrap();
    assert!(captured.duplicate);
    assert_eq!(captured.message_id, command.message.record.message_id);
    assert_eq!(
        gateway
            .messages(acquisition.conversation_id)
            .unwrap()
            .into_iter()
            .find(|message| message.payload.record.message_id == captured.message_id)
            .unwrap()
            .payload
            .record
            .source_sequence,
        command.message.record.source_sequence
    );
}

#[test]
fn full_pending_queue_backpressures_new_rows_without_pausing_history() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = open(&config(&dir, "client", &vault));
    client
        .plan_sms_scans("install-1", "provider", "5000")
        .unwrap();
    let outgoing = |id: String| {
        let mut input = provider_input(&id, "queued", None, None);
        input.direction = Direction::Outgoing;
        input.sender_address = None;
        input.recipients = vec![ADDRESS.into()];
        input
    };
    let mut first = None;
    for index in 0..MAX_PENDING_SMS_ACQUISITIONS {
        let acquisition = client
            .begin_sms_acquisition(outgoing(index.to_string()))
            .unwrap();
        first.get_or_insert(acquisition.acquisition_id);
    }
    assert_eq!(
        client.begin_sms_acquisition(outgoing("overflow".into())),
        Err(Error::SmsCapacity)
    );
    assert!(
        !client
            .sms_history_status("install-1", "provider")
            .unwrap()
            .paused
    );
    // Draining one pending row makes room again: the queue is bounded, not a lifetime cap.
    client.complete_sms_acquisition(&first.unwrap()).unwrap();
    client
        .begin_sms_acquisition(outgoing("overflow".into()))
        .unwrap();
}

#[test]
fn plans_freeze_history_and_require_explicit_exhaustion() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "client", &vault), &vault);
    let plans = client
        .plan_sms_scans("install-1", "provider", "50")
        .unwrap();
    assert_eq!(plans.len(), 2);
    assert_eq!(plans[1].history_upper_bound.as_deref(), Some("50"));
    client
        .ack_sms_scan_exhausted("install-1", "provider", true, "50")
        .unwrap();
    let plans = client
        .plan_sms_scans("install-1", "provider", "99")
        .unwrap();
    assert_eq!(plans[1].history_upper_bound.as_deref(), Some("50"));
    assert!(plans[1].history_complete);
}

#[test]
fn reconciliation_sweep_discovers_reused_or_decreased_ids_without_resetting_history() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let config = config(&dir, "client", &vault);
    let client = unlocked(&config, &vault);
    let original = client
        .begin_sms_acquisition(provider_input("42", "original", Some(100), None))
        .unwrap();
    let captured = client
        .complete_sms_acquisition(&original.acquisition_id)
        .unwrap();
    client
        .plan_sms_scans("install-1", "provider", "42")
        .unwrap();
    client
        .ack_sms_scan_exhausted("install-1", "provider", true, "42")
        .unwrap();

    // The first sweep is deferred one interval: initial history already observed every row.
    let start = 1_800_000_000_000;
    assert_eq!(
        client
            .plan_sms_reconciliation_at("install-1", "provider", "42", None, start)
            .unwrap(),
        None
    );
    let first_sweep = client
        .plan_sms_reconciliation_at(
            "install-1",
            "provider",
            "42",
            None,
            start + SMS_RECONCILIATION_INTERVAL_MS,
        )
        .unwrap()
        .unwrap();
    assert_eq!(first_sweep.cursor, None);
    assert_eq!(first_sweep.upper_bound, "42");
    assert_eq!(first_sweep.history_upper_bound.as_deref(), Some("42"));
    let replacement = client
        .begin_sms_acquisition(provider_input("42", "replacement", Some(200), None))
        .unwrap();
    let replacement_capture = client
        .complete_sms_acquisition(&replacement.acquisition_id)
        .unwrap();
    assert_ne!(captured.message_id, replacement_capture.message_id);
    client
        .complete_sms_reconciliation("install-1", "provider", "42", Some("42"), true)
        .unwrap();
    // A completed sweep does not restart on every pass.
    assert_eq!(
        client
            .plan_sms_reconciliation("install-1", "provider", "42", None)
            .unwrap(),
        None
    );

    drop(client);
    let reopened = unlocked(&config, &vault);
    let later = reopened
        .plan_sms_reconciliation_at("install-1", "provider", "10", None, i64::MAX / 2)
        .unwrap()
        .unwrap();
    assert_eq!(later.cursor, None);
    assert_eq!(later.upper_bound, "10");
    assert!(
        reopened
            .plan_sms_scans("install-1", "provider", "10")
            .unwrap()[1]
            .history_complete
    );
    assert_eq!(
        reopened.messages(captured.conversation_id).unwrap().len(),
        2
    );
}
