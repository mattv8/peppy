mod common;

use common::*;
use peppy_client_core::{PermitBlock, PermitDecision, SendCancellationState};

#[test]
fn request_before_original_stays_provisional_then_reconciles_to_the_matching_command() {
    let dir = tempfile::tempdir().unwrap();
    let vault = Vault::new();
    let sender_config = config(&dir, "sender", &vault);
    let gateway_config = config(&dir, "gateway", &vault);
    let sender = unlocked(&sender_config, &vault);
    let gateway = unlocked(&gateway_config, &vault);
    let queued = sender
        .queue_send(
            sms(ConversationId::new(), "reordered cancellation"),
            route(&gateway_config),
        )
        .unwrap();
    let mut server = Server::default();
    server.upload(&sender);
    gateway.publish_send_cancellation_capability(true).unwrap();
    server.upload(&gateway);
    server.sync(&sender);
    sender.request_send_cancellation(queued.message_id).unwrap();
    let cancellation = sender.pending_outbox().unwrap().pop().unwrap();

    gateway.ingest(&cancellation, Cursor(1)).unwrap();
    gateway.apply_pending(100).unwrap();
    assert!(!gateway.process_pending_send_cancellations().unwrap());

    for (cursor, envelope) in server.log.iter().take(2).enumerate() {
        gateway.ingest(envelope, Cursor(cursor as u64 + 2)).unwrap();
    }
    gateway.apply_pending(100).unwrap();
    assert!(gateway.process_pending_send_cancellations().unwrap());
}

#[test]
fn snapshot_cancellation_request_cannot_confirm_a_historical_command_without_attempt_evidence() {
    let dir = tempfile::tempdir().unwrap();
    let vault = Vault::new();
    let sender_config = config(&dir, "sender", &vault);
    let gateway_config = config(&dir, "gateway", &vault);
    let sender = unlocked(&sender_config, &vault);
    let gateway = unlocked(&gateway_config, &vault);
    let conversation = ConversationId::new();
    let queued = sender
        .queue_send(
            sms(conversation, "historical cancellation request"),
            route(&gateway_config),
        )
        .unwrap();
    let mut server = Server::default();
    server.upload(&sender);
    server.sync(&gateway);
    gateway.publish_send_cancellation_capability(true).unwrap();
    server.upload(&gateway);
    server.sync(&sender);
    sender.request_send_cancellation(queued.message_id).unwrap();
    server.upload(&sender);

    let mut restored_config = gateway_config.clone();
    restored_config.database_path = dir.path().join("restored-gateway.db");
    let restored = unlocked(&restored_config, &vault);
    let records: Vec<_> = server
        .log
        .iter()
        .enumerate()
        .map(|(index, envelope)| SnapshotRecord {
            cursor: Cursor(index as u64 + 1),
            envelope: envelope.clone(),
        })
        .collect();
    let session = restored
        .begin_snapshot(
            Cursor(records.len() as u64),
            records.len() as u64,
            SnapshotPurpose::Resync,
        )
        .unwrap();
    restored
        .append_snapshot_page(session.generation, &records)
        .unwrap();
    restored.finish_snapshot(session.generation).unwrap();
    restored.apply_pending(100).unwrap();

    assert_eq!(
        restored
            .message_send_cancellation(queued.message_id)
            .unwrap()
            .state,
        SendCancellationState::Requested
    );
    assert!(!restored.process_pending_send_cancellations().unwrap());
    assert!(restored.pending_outbox().unwrap().is_empty());
    assert!(matches!(
        restored.begin_send_attempt(queued.command_id).unwrap(),
        PermitDecision::Blocked(PermitBlock::Historical | PermitBlock::CancellationRequested)
    ));
}

#[test]
fn snapshot_promotion_keeps_a_live_confirmed_cancellation_from_resurrecting_its_message() {
    let dir = tempfile::tempdir().unwrap();
    let vault = Vault::new();
    let sender_config = config(&dir, "sender", &vault);
    let gateway_config = config(&dir, "gateway", &vault);
    let sender = unlocked(&sender_config, &vault);
    let gateway = unlocked(&gateway_config, &vault);
    let conversation = ConversationId::new();
    let queued = sender
        .queue_send(
            sms(conversation, "snapshot cancellation fence"),
            route(&gateway_config),
        )
        .unwrap();
    let mut server = Server::default();
    server.upload(&sender);
    server.sync(&gateway);
    gateway.publish_send_cancellation_capability(true).unwrap();
    server.upload(&gateway);
    server.sync(&sender);
    let old_cut = server.log.clone();

    sender.request_send_cancellation(queued.message_id).unwrap();
    server.upload(&sender);
    server.sync(&gateway);
    assert!(gateway.process_pending_send_cancellations().unwrap());
    server.upload(&gateway);

    let restored = unlocked(&config(&dir, "restored", &vault), &vault);
    let session = restored
        .begin_snapshot(
            Cursor(old_cut.len() as u64),
            old_cut.len() as u64,
            SnapshotPurpose::Resync,
        )
        .unwrap();
    let old_records: Vec<_> = old_cut
        .iter()
        .enumerate()
        .map(|(index, envelope)| SnapshotRecord {
            cursor: Cursor(index as u64 + 1),
            envelope: envelope.clone(),
        })
        .collect();
    restored
        .append_snapshot_page(session.generation, &old_records)
        .unwrap();
    for (index, envelope) in server.log.iter().enumerate().skip(old_cut.len()) {
        restored.ingest(envelope, Cursor(index as u64 + 1)).unwrap();
    }
    restored.finish_snapshot(session.generation).unwrap();
    restored.apply_pending(100).unwrap();

    assert_eq!(
        restored
            .message_send_cancellation(queued.message_id)
            .unwrap()
            .state,
        SendCancellationState::Confirmed
    );
    assert!(restored.messages(conversation).unwrap().is_empty());
    assert_eq!(
        restored.ingest(&old_cut[0], Cursor(1)).unwrap(),
        IngestResult::Duplicate
    );
    restored.apply_pending(100).unwrap();
    assert!(restored.messages(conversation).unwrap().is_empty());
}
