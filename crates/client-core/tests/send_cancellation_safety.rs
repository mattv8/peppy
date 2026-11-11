mod common;

use common::*;
use peppy_client_core::{
    ConversationId, PermitBlock, PermitDecision, SendCancellationState, SendResult,
};

fn publish_cancellation_support(sender: &Client, gateway: &Client, server: &mut Server) {
    gateway.publish_send_cancellation_capability(true).unwrap();
    server.upload(gateway);
    server.sync(sender);
}

fn queue_received_send(
    sender: &Client,
    gateway_config: &ClientConfig,
    server: &mut Server,
) -> (ConversationId, QueuedSend) {
    let conversation = ConversationId::new();
    let queued = sender
        .queue_send(sms(conversation, "cancel safety"), route(gateway_config))
        .unwrap();
    server.upload(sender);
    (conversation, queued)
}

#[test]
fn received_queued_command_becomes_cancellable_after_capability_exchange_without_recompose() {
    let dir = tempfile::tempdir().unwrap();
    let vault = Vault::new();
    let sender_config = config(&dir, "sender", &vault);
    let gateway_config = config(&dir, "gateway", &vault);
    let sender = unlocked(&sender_config, &vault);
    let gateway = unlocked(&gateway_config, &vault);
    let mut server = Server::default();
    let (conversation, queued) = queue_received_send(&sender, &gateway_config, &mut server);

    assert_eq!(
        sender
            .message_send_cancellation(queued.message_id)
            .unwrap()
            .state,
        SendCancellationState::None
    );
    assert!(
        !sender
            .message_send_cancellation(queued.message_id)
            .unwrap()
            .can_request
    );

    publish_cancellation_support(&sender, &gateway, &mut server);

    let view = sender.message_send_cancellation(queued.message_id).unwrap();
    assert_eq!(view.state, SendCancellationState::None);
    assert!(view.can_request);
    assert_eq!(
        only_message(&sender, conversation)
            .payload
            .record
            .message_id,
        queued.message_id
    );
}

#[test]
fn cancellation_before_admission_fences_the_original_across_duplicate_replay() {
    let dir = tempfile::tempdir().unwrap();
    let vault = Vault::new();
    let sender_config = config(&dir, "sender", &vault);
    let gateway_config = config(&dir, "gateway", &vault);
    let sender = unlocked(&sender_config, &vault);
    let gateway = unlocked(&gateway_config, &vault);
    let mut server = Server::default();
    let (conversation, queued) = queue_received_send(&sender, &gateway_config, &mut server);
    server.sync(&gateway);
    publish_cancellation_support(&sender, &gateway, &mut server);

    assert_eq!(
        sender
            .request_send_cancellation(queued.message_id)
            .unwrap()
            .state,
        SendCancellationState::Requested
    );
    server.upload(&sender);
    server.sync(&gateway);
    assert!(matches!(
        gateway.begin_send_attempt(queued.command_id).unwrap(),
        PermitDecision::Blocked(PermitBlock::CancellationRequested)
    ));
    assert!(gateway.process_pending_send_cancellations().unwrap());
    assert!(!gateway.process_pending_send_cancellations().unwrap());
    assert!(matches!(
        gateway.begin_send_attempt(queued.command_id).unwrap(),
        PermitDecision::Blocked(_)
    ));

    server.upload(&gateway);
    server.sync(&sender);
    server.sync(&sender);
    assert_eq!(
        sender
            .message_send_cancellation(queued.message_id)
            .unwrap()
            .state,
        SendCancellationState::Confirmed
    );
    assert!(sender.messages(conversation).unwrap().is_empty());
    assert!(matches!(
        gateway.begin_send_attempt(queued.command_id).unwrap(),
        PermitDecision::Blocked(_)
    ));
}

#[test]
fn cancellation_after_recorded_permit_is_too_late_and_does_not_issue_another_permit() {
    let dir = tempfile::tempdir().unwrap();
    let vault = Vault::new();
    let sender_config = config(&dir, "sender", &vault);
    let gateway_config = config(&dir, "gateway", &vault);
    let sender = unlocked(&sender_config, &vault);
    let gateway = unlocked(&gateway_config, &vault);
    let mut server = Server::default();
    let (_, queued) = queue_received_send(&sender, &gateway_config, &mut server);
    server.sync(&gateway);
    publish_cancellation_support(&sender, &gateway, &mut server);

    assert!(matches!(
        gateway.begin_send_attempt(queued.command_id).unwrap(),
        PermitDecision::Permit(_)
    ));
    assert_eq!(
        sender
            .request_send_cancellation(queued.message_id)
            .unwrap()
            .state,
        SendCancellationState::Requested
    );
    server.upload(&sender);
    server.sync(&gateway);
    assert!(gateway.process_pending_send_cancellations().unwrap());
    assert!(matches!(
        gateway.begin_send_attempt(queued.command_id).unwrap(),
        PermitDecision::AlreadyAttempted(_)
    ));

    server.upload(&gateway);
    server.sync(&sender);
    assert_eq!(
        sender
            .message_send_cancellation(queued.message_id)
            .unwrap()
            .state,
        SendCancellationState::TooLate
    );
}

#[test]
fn sent_and_delivered_sends_cannot_request_cancellation_or_enqueue_a_request() {
    let dir = tempfile::tempdir().unwrap();
    let vault = Vault::new();
    let sender_config = config(&dir, "sender", &vault);
    let gateway_config = config(&dir, "gateway", &vault);
    let sender = unlocked(&sender_config, &vault);
    let gateway = unlocked(&gateway_config, &vault);
    let mut server = Server::default();
    let (conversation, queued) = queue_received_send(&sender, &gateway_config, &mut server);
    server.sync(&gateway);
    publish_cancellation_support(&sender, &gateway, &mut server);
    assert!(matches!(
        gateway.begin_send_attempt(queued.command_id).unwrap(),
        PermitDecision::Permit(_)
    ));
    gateway
        .record_send_result(queued.command_id, SendResult::Sent)
        .unwrap();
    server.upload(&gateway);
    server.sync(&sender);
    assert_sent_cancellation_is_ineligible(&sender, queued.message_id, conversation);

    gateway
        .record_send_result(queued.command_id, SendResult::Delivered)
        .unwrap();
    server.upload(&gateway);
    server.sync(&sender);

    assert_sent_cancellation_is_ineligible(&sender, queued.message_id, conversation);
}

fn assert_sent_cancellation_is_ineligible(
    sender: &Client,
    message_id: peppy_client_core::MessageId,
    conversation: ConversationId,
) {
    let before = sender.pending_outbox_count().unwrap();
    let view = sender.message_send_cancellation(message_id).unwrap();
    assert_eq!(view.state, SendCancellationState::None);
    assert!(!view.can_request);
    assert_eq!(
        sender.request_send_cancellation(message_id).unwrap().state,
        SendCancellationState::TooLate
    );
    assert_eq!(sender.pending_outbox_count().unwrap(), before);
    assert_eq!(
        only_message(sender, conversation).payload.record.message_id,
        message_id
    );
}

#[test]
fn missing_or_unsupported_capability_cannot_confirm_or_hide_a_send() {
    let dir = tempfile::tempdir().unwrap();
    let vault = Vault::new();
    let sender_config = config(&dir, "sender", &vault);
    let gateway_config = config(&dir, "gateway", &vault);
    let sender = unlocked(&sender_config, &vault);
    let gateway = unlocked(&gateway_config, &vault);
    let mut server = Server::default();
    let (conversation, queued) = queue_received_send(&sender, &gateway_config, &mut server);

    let missing = sender.request_send_cancellation(queued.message_id).unwrap();
    assert_eq!(missing.state, SendCancellationState::None);
    assert!(missing.requires_gateway_update);
    assert_eq!(
        only_message(&sender, conversation)
            .payload
            .record
            .message_id,
        queued.message_id
    );

    gateway.publish_send_cancellation_capability(false).unwrap();
    server.upload(&gateway);
    server.sync(&sender);
    let unsupported = sender.request_send_cancellation(queued.message_id).unwrap();
    assert_eq!(unsupported.state, SendCancellationState::None);
    assert!(unsupported.requires_gateway_update);
    assert_eq!(
        only_message(&sender, conversation)
            .payload
            .record
            .message_id,
        queued.message_id
    );
}

#[test]
fn confirmed_cancellation_survives_reopen_and_keeps_original_hidden() {
    let dir = tempfile::tempdir().unwrap();
    let vault = Vault::new();
    let sender_config = config(&dir, "sender", &vault);
    let gateway_config = config(&dir, "gateway", &vault);
    let sender = unlocked(&sender_config, &vault);
    let gateway = unlocked(&gateway_config, &vault);
    let mut server = Server::default();
    let (conversation, queued) = queue_received_send(&sender, &gateway_config, &mut server);
    server.sync(&gateway);
    publish_cancellation_support(&sender, &gateway, &mut server);
    sender.request_send_cancellation(queued.message_id).unwrap();
    server.upload(&sender);
    server.sync(&gateway);
    gateway.process_pending_send_cancellations().unwrap();
    server.upload(&gateway);
    server.sync(&sender);
    drop(sender);

    let reopened = unlocked(&sender_config, &vault);
    assert_eq!(
        reopened
            .message_send_cancellation(queued.message_id)
            .unwrap()
            .state,
        SendCancellationState::Confirmed
    );
    assert!(reopened.messages(conversation).unwrap().is_empty());
}
