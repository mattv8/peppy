mod common;

use common::*;
use peppy_client_core::{ConversationId, Error, LocalSearchRequest, SearchKind};
use std::fs;

#[test]
fn confirmed_cancellation_is_hidden_from_public_projections_but_keeps_attachment_evidence() {
    let directory = tempfile::tempdir().unwrap();
    let vault = Vault::new();
    let sender_config = config(&directory, "sender", &vault);
    let gateway_config = config(&directory, "gateway", &vault);
    let sender = unlocked(&sender_config, &vault);
    let gateway = unlocked(&gateway_config, &vault);
    let source = directory.path().join("retained.png");
    fs::write(&source, b"retained attachment evidence").unwrap();
    let attachment = sender
        .prepare_attachment(&source, "image/png", "retained.png")
        .unwrap();
    sender
        .mark_attachment_uploaded(attachment.attachment_id, &uuid::Uuid::new_v4().to_string())
        .unwrap();
    let conversation = ConversationId::new();
    let queued = sender
        .queue_mms(
            OutgoingMms {
                conversation_id: conversation,
                recipients: vec![ADDRESS.into()],
                body: "projection needle".into(),
                attachment_ids: vec![attachment.attachment_id],
                subject: None,
            },
            route(&gateway_config),
        )
        .unwrap();
    let mut server = Server::default();
    server.upload(&sender);
    server.sync(&gateway);
    gateway.publish_send_cancellation_capability(true).unwrap();
    server.upload(&gateway);
    server.sync(&sender);

    sender.advance_search_index(Some(100)).unwrap();
    let mut initial_media_page = sender.media_page(conversation, None, 10).unwrap();
    let media_cursor = initial_media_page.items.remove(0).cursor;
    assert_eq!(
        sender.search_local(search_request()).unwrap().items.len(),
        1
    );
    assert_eq!(
        sender
            .messages_page(conversation, None, 10)
            .unwrap()
            .messages
            .len(),
        1
    );
    assert_eq!(sender.list_conversations().unwrap().len(), 1);

    sender.request_send_cancellation(queued.message_id).unwrap();
    server.upload(&sender);
    server.sync(&gateway);
    assert!(gateway.process_pending_send_cancellations().unwrap());
    server.upload(&gateway);
    server.sync(&sender);

    assert!(
        sender
            .search_local(search_request())
            .unwrap()
            .items
            .is_empty()
    );
    assert!(sender.messages(conversation).unwrap().is_empty());
    assert_eq!(sender.message_by_id(queued.message_id).unwrap(), None);
    assert!(
        sender
            .messages_page(conversation, None, 10)
            .unwrap()
            .messages
            .is_empty()
    );
    assert!(
        sender
            .messages_since(conversation, queued.message_id)
            .unwrap()
            .is_empty()
    );
    assert!(sender.list_conversations().unwrap().is_empty());
    assert!(
        sender
            .media_page(conversation, None, 10)
            .unwrap()
            .items
            .is_empty()
    );
    assert!(
        sender
            .media_page_newer(conversation, &media_cursor, 10)
            .unwrap()
            .items
            .is_empty()
    );
    assert!(matches!(
        sender.media_page_around_at(
            conversation,
            queued.message_id,
            attachment.attachment_id,
            10
        ),
        Err(Error::NotFound)
    ));
    assert_eq!(
        sender
            .attachment_info(attachment.attachment_id)
            .unwrap()
            .attachment_id,
        attachment.attachment_id
    );
}

fn search_request() -> LocalSearchRequest {
    LocalSearchRequest {
        query: "projection needle".into(),
        kind: Some(SearchKind::Message),
        limit: Some(10),
        cursor: None,
    }
}
