//! Happy-path roundtrip between two real clients that share one vault. A desktop "Search further
//! back" attempt asks the phone, the phone captures imported SMS through the public acquisition
//! and checkpoint APIs, and the completed result plus the captured message let the desktop's
//! local search find it. Only encrypted envelopes move between the clients.

use crate::common::*;
use peppy_client_core::EnvelopeId;
use serde_json::{Value, json};
use uuid::Uuid;

const PROVIDER: &str = "18d0a04f-98e9-4cc2-8faa-e8893e6d00aa";
const SUBSCRIPTION: &str = "sim-1";
const PROVIDER_ROW: &str = "100";
const KEYWORD: &str = "lanternfish";
const BODY: &str = "Dinner plans: lanternfish tacos at seven";

fn value(output: Result<String, Error>) -> Value {
    serde_json::from_str(&output.unwrap()).unwrap()
}

/// Ingests server log entries `[receive_cursor, through)` in order, then applies them. A terminal
/// replay passes the whole log length; a shorter `through` models a page that ends early.
fn deliver(server: &Server, client: &Client, through: usize) {
    let from = usize::try_from(client.receive_cursor().unwrap().0).unwrap();
    for (index, envelope) in server.log.iter().enumerate().take(through).skip(from) {
        let outcome = client.ingest(envelope, Cursor(index as u64 + 1)).unwrap();
        assert!(matches!(
            outcome,
            IngestResult::Journaled | IngestResult::Duplicate
        ));
    }
    assert_eq!(client.receive_cursor().unwrap().0, through as u64);
    let report = client.apply_pending(100).unwrap();
    assert_eq!(report.quarantined, 0);
}

/// Replay proof for a page fetched from a server whose high water is `high_water`. The field is
/// present and explicitly terminal, the only shape that counts as replay evidence.
fn terminal_replay(context: HistorySearchSyncContext, high_water: usize) -> HistorySearchSyncFacts {
    HistorySearchSyncFacts {
        proof: Some(HistorySearchReplayProof {
            context,
            high_water_cursor: high_water.to_string(),
            next_after_present: true,
            terminal_next_after: true,
        }),
    }
}

/// Uploads the phone's pending envelopes with the held message last. The message is identified by
/// its envelope ID captured right after acquisition, so the rest (the completed result) goes first.
fn upload_result_before_message(server: &mut Server, phone: &Client, message: EnvelopeId) {
    let pending = phone.pending_outbox().unwrap();
    let (held, result): (Vec<Envelope>, Vec<Envelope>) = pending
        .into_iter()
        .partition(|envelope| envelope.envelope_id == message);
    assert_eq!(held.len(), 1, "exactly the captured message is held");
    assert_eq!(result.len(), 1, "exactly the completed result follows");
    for envelope in result.into_iter().chain(held) {
        let id = envelope.envelope_id;
        server.log.push(envelope);
        phone.ack_outbox(id).unwrap();
    }
}

fn history_row() -> SmsAcquisitionInput {
    SmsAcquisitionInput {
        source: SmsSource {
            source_generation: PROVIDER.into(),
            subscription_id: SUBSCRIPTION.into(),
            provider_message_id: PROVIDER_ROW.into(),
            provider_thread_id: Some("thread-1".into()),
            broadcast_id: None,
        },
        direction: Direction::Incoming,
        sender_address: Some(ADDRESS.into()),
        recipients: vec![],
        body: BODY.into(),
        imported: true,
        sent_at_ms: Some(1_700_000_000_000),
        date_sent_ms: Some(1_700_000_000_000),
        creator: None,
        source_row_fingerprint: format!("{PROVIDER_ROW}:{BODY}"),
    }
}

fn keyword_query() -> LocalSearchRequest {
    LocalSearchRequest {
        query: KEYWORD.into(),
        kind: Some(SearchKind::Message),
        cursor: None,
        limit: None,
    }
}

#[test]
fn search_further_back_roundtrips_through_phone_capture_and_local_search() {
    let directory = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let phone_config = config(&directory, "phone", &vault);
    let desktop_config = config(&directory, "desktop", &vault);
    let phone = unlocked(&phone_config, &vault);
    let desktop = unlocked(&desktop_config, &vault);
    let phone_device = phone_config.device_id.to_string();
    let mut server = Server::default();

    // The phone advertises SMS-only capture for its provider generation; the desktop receives it.
    let published = value(phone.publish_message_history_capture_source(
        &json!({"providerGeneration": PROVIDER, "sms": true, "mms": false, "permission": true, "policy": true, "forceRepublish": false}).to_string(),
    ));
    assert_eq!(published["published"], true);
    assert_eq!(server.upload(&phone), 1);
    deliver(&server, &desktop, server.log.len());

    // Desktop starts the attempt; the first observation is still catching up, not blocked.
    let started = desktop.start_history_search(Uuid::new_v4()).unwrap();
    let attempt = started.attempt_id.unwrap();
    assert_eq!(started.state, HistorySearchState::CatchingUp);
    assert_eq!(
        desktop.history_search_work(attempt).unwrap(),
        HistorySearchWork::WakeSync
    );

    // A post-start terminal replay yields the proof that admits exactly one phone request.
    let context = desktop.history_search_sync_context().unwrap();
    deliver(&server, &desktop, server.log.len());
    let observed = desktop
        .observe_history_search_sync(attempt, terminal_replay(context, server.log.len()))
        .unwrap();
    assert_eq!(observed.state, HistorySearchState::WaitingForPhone);
    let HistorySearchWork::RequestPhone {
        request_id,
        target_device_id,
    } = desktop.history_search_work(attempt).unwrap()
    else {
        panic!("expected a phone request");
    };
    assert_eq!(target_device_id, phone_device);

    // The request is already encrypted in the desktop outbox; the phone admits it from the log.
    assert_eq!(server.upload(&desktop), 1);
    deliver(&server, &phone, server.log.len());
    let pending = value(phone.pending_message_history_capture_requests(PROVIDER));
    assert_eq!(pending["needsSms"], true);
    assert_eq!(pending["needsMms"], false);
    assert_eq!(pending["requests"].as_array().unwrap().len(), 1);
    assert_eq!(
        pending["requests"][0]["requestId"].as_str(),
        Some(request_id.as_str())
    );
    let begun = value(phone.begin_message_history_capture(PROVIDER, "sms"));
    assert_eq!(begun["budget"], 100);
    let token = begun["token"].as_str().unwrap().to_owned();

    // Imported SMS history is captured through the real acquisition path. The scan bound is
    // frozen first, then the checkpoint advances and exhaustion is acknowledged durably.
    phone.open_history_import_gate("contacts_settled").unwrap();
    phone
        .plan_sms_scans(PROVIDER, SUBSCRIPTION, PROVIDER_ROW)
        .unwrap();
    let acquisition = phone.begin_sms_acquisition(history_row()).unwrap();
    let captured = phone
        .complete_sms_acquisition(&acquisition.acquisition_id)
        .unwrap();
    assert!(!captured.duplicate);
    let held = phone.pending_outbox().unwrap();
    assert_eq!(held.len(), 1, "only the captured message is unacknowledged");
    let message_envelope = held[0].envelope_id;
    phone
        .set_sms_scan_checkpoint(PROVIDER, SUBSCRIPTION, true, PROVIDER_ROW)
        .unwrap();
    phone
        .ack_sms_scan_exhausted(PROVIDER, SUBSCRIPTION, true, PROVIDER_ROW)
        .unwrap();
    assert!(
        phone
            .sms_history_status(PROVIDER, SUBSCRIPTION)
            .unwrap()
            .history_complete
    );

    // Finishing the page publishes the completed result, whose receipt names the captured message.
    let finished = value(phone.finish_message_history_capture(
        &json!({"providerGeneration": PROVIDER, "token": token, "observations": 1, "exhausted": true, "reason": null}).to_string(),
    ));
    assert_eq!(finished["state"], "finished");
    upload_result_before_message(&mut server, &phone, message_envelope);
    assert_eq!(server.log.len(), 4);

    // Result before message: the request is not completed while its receipt is still missing.
    deliver(&server, &desktop, 3);
    let indexing = desktop
        .observe_history_search_sync(attempt, HistorySearchSyncFacts::default())
        .unwrap();
    assert_eq!(indexing.state, HistorySearchState::Indexing);
    assert!(!indexing.can_search_further);
    assert_eq!(
        desktop.history_search_work(attempt).unwrap(),
        HistorySearchWork::WakeSync,
        "a pending receipt never re-requests the phone"
    );
    assert!(
        desktop
            .search_local(keyword_query())
            .unwrap()
            .items
            .is_empty()
    );

    // The held message arrives: the receipt verifies against the indexed message and the attempt
    // completes under a fresh terminal replay proof.
    let context = desktop.history_search_sync_context().unwrap();
    deliver(&server, &desktop, server.log.len());
    let completed = desktop
        .observe_history_search_sync(attempt, terminal_replay(context, server.log.len()))
        .unwrap();
    assert_eq!(completed.state, HistorySearchState::Completed);
    assert_eq!(completed.reason, None);
    assert_eq!(
        desktop.history_search_work(attempt).unwrap(),
        HistorySearchWork::Wait
    );

    let page = desktop.search_local(keyword_query()).unwrap();
    assert_eq!(page.items.len(), 1);
    let LocalSearchHit::Message {
        conversation_id,
        message_id,
        ..
    } = &page.items[0]
    else {
        panic!("expected a message hit");
    };
    assert_eq!(message_id, &captured.message_id.to_string());
    assert_eq!(conversation_id, &captured.conversation_id.to_string());

    // Both clients have consumed the same contiguous log.
    deliver(&server, &phone, server.log.len());
    assert_eq!(desktop.receive_cursor().unwrap().0, server.log.len() as u64);
}
