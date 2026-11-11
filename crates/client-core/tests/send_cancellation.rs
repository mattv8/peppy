mod common;

use common::*;
use peppy_client_core::{ConversationId, SendCancellationState};

#[test]
fn queued_send_is_fenced_and_confirmed_by_its_gateway() {
    let dir = tempfile::tempdir().unwrap();
    let vault = Vault::new();
    let sender_config = config(&dir, "sender", &vault);
    let gateway_config = config(&dir, "gateway", &vault);
    let sender = unlocked(&sender_config, &vault);
    let gateway = unlocked(&gateway_config, &vault);
    let conversation = ConversationId::new();
    let queued = sender
        .queue_send(sms(conversation, "cancel me"), route(&gateway_config))
        .unwrap();
    let mut server = Server::default();
    server.upload(&sender);
    server.sync(&gateway);
    gateway.publish_send_cancellation_capability(true).unwrap();
    server.upload(&gateway);
    server.sync(&sender);

    assert!(
        sender
            .message_send_cancellation(queued.message_id)
            .unwrap()
            .can_request
    );
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
    server.upload(&gateway);
    server.sync(&sender);
    assert_eq!(
        sender
            .message_send_cancellation(queued.message_id)
            .unwrap()
            .state,
        SendCancellationState::Confirmed
    );
    assert!(sender.messages(conversation).unwrap().is_empty());
}
