//! Bounded, durable-ID conversation history paging.
mod common;

use common::*;
use tempfile::TempDir;

fn capture_messages(client: &Client, count: usize) -> (ConversationId, Vec<MessageId>) {
    capture_messages_from(client, 0, count)
}

fn capture_messages_from(
    client: &Client,
    start: usize,
    count: usize,
) -> (ConversationId, Vec<MessageId>) {
    let mut conversation_id = None;
    let mut message_ids = Vec::with_capacity(count);
    for index in start..start + count {
        let captured = client
            .capture_incoming(incoming(
                &format!("message-{index}"),
                &format!("history-{index}"),
            ))
            .unwrap();
        conversation_id = Some(captured.conversation_id);
        message_ids.push(captured.message_id);
    }
    (
        conversation_id.unwrap_or_else(ConversationId::new),
        message_ids,
    )
}

#[test]
fn messages_page_returns_empty_history() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "client", &vault), &vault);

    let page = client
        .messages_page(ConversationId::new(), None, 50)
        .unwrap();

    assert!(page.messages.is_empty());
    assert!(!page.has_more);
}

#[test]
fn latest_message_matches_full_history_and_is_empty_without_messages() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "client", &vault), &vault);
    let empty_conversation = ConversationId::new();

    assert_eq!(client.latest_message(empty_conversation).unwrap(), None);
    let (conversation_id, _) = capture_messages(&client, 3);
    assert_eq!(
        client.latest_message(conversation_id).unwrap(),
        client.messages(conversation_id).unwrap().last().cloned()
    );
}

#[test]
fn messages_page_rejects_zero_limit() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "client", &vault), &vault);

    assert!(matches!(
        client.messages_page(ConversationId::new(), None, 0),
        Err(Error::InvalidRequest("history page size"))
    ));
}

#[test]
fn messages_page_returns_an_exact_fifty_message_history_without_continuation() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "client", &vault), &vault);
    let (conversation_id, _) = capture_messages(&client, 50);

    let page = client.messages_page(conversation_id, None, 50).unwrap();

    assert_eq!(page.messages.len(), 50);
    assert!(!page.has_more);
    assert_eq!(page.messages[0].payload.body, "message-0");
    assert_eq!(page.messages[49].payload.body, "message-49");
}

#[test]
fn messages_page_returns_fifty_newest_messages_in_chronological_order() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "client", &vault), &vault);
    let (conversation_id, _) = capture_messages(&client, 51);

    let page = client.messages_page(conversation_id, None, 50).unwrap();

    assert_eq!(page.messages.len(), 50);
    assert!(page.has_more);
    assert_eq!(page.messages[0].payload.body, "message-1");
    assert_eq!(page.messages[49].payload.body, "message-50");
}

#[test]
fn messages_page_uses_an_exclusive_older_anchor() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "client", &vault), &vault);
    let (conversation_id, message_ids) = capture_messages(&client, 51);

    let page = client
        .messages_page(conversation_id, Some(message_ids[1]), 50)
        .unwrap();

    assert_eq!(page.messages.len(), 1);
    assert!(!page.has_more);
    assert_eq!(page.messages[0].payload.body, "message-0");
}

#[test]
fn messages_page_caps_requested_limit_at_five_hundred() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "client", &vault), &vault);
    let (conversation_id, _) = capture_messages(&client, 501);

    let page = client.messages_page(conversation_id, None, 501).unwrap();

    assert_eq!(page.messages.len(), 500);
    assert!(page.has_more);
    assert_eq!(page.messages[0].payload.body, "message-1");
    assert_eq!(page.messages[499].payload.body, "message-500");
}

#[test]
fn history_anchors_must_belong_to_the_requested_conversation() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "client", &vault), &vault);
    let (_, message_ids) = capture_messages(&client, 1);

    assert!(matches!(
        client.messages_page(ConversationId::new(), Some(message_ids[0]), 50),
        Err(Error::InvalidRequest("history anchor"))
    ));
    assert!(matches!(
        client.messages_since(ConversationId::new(), message_ids[0]),
        Err(Error::InvalidRequest("history anchor"))
    ));
}

#[test]
fn missing_history_anchors_are_rejected() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "client", &vault), &vault);
    let conversation_id = ConversationId::new();
    let missing = MessageId::new();

    assert!(matches!(
        client.messages_page(conversation_id, Some(missing), 50),
        Err(Error::InvalidRequest("history anchor"))
    ));
    assert!(matches!(
        client.messages_since(conversation_id, missing),
        Err(Error::InvalidRequest("history anchor"))
    ));
}

#[test]
fn messages_page_walks_backward_to_exhaustion_without_duplicates() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "client", &vault), &vault);
    let (conversation_id, _) = capture_messages(&client, 51);
    let mut before = None;
    let mut pages = Vec::new();

    loop {
        let page = client.messages_page(conversation_id, before, 25).unwrap();
        before = page
            .messages
            .first()
            .map(|message| message.payload.record.message_id);
        pages.splice(0..0, page.messages);
        if !page.has_more {
            break;
        }
    }

    assert_eq!(pages, client.messages(conversation_id).unwrap());
}

#[test]
fn messages_since_includes_anchor_and_new_arrivals() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "client", &vault), &vault);
    let (conversation_id, message_ids) = capture_messages(&client, 51);
    capture_messages_from(&client, 51, 2);

    let messages = client
        .messages_since(conversation_id, message_ids[0])
        .unwrap();

    assert_eq!(messages.len(), 53);
    assert_eq!(messages[0].payload.body, "message-0");
    assert_eq!(messages[52].payload.body, "message-52");
    assert_eq!(messages, client.messages(conversation_id).unwrap());
}
