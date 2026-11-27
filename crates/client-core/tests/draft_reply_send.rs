use crate::common::*;

/// A saved text-only draft to `ADDRESS` over the carrier route, plus a message already in its
/// conversation that a reply can target.
fn reply_fixture(client: &Client, config: &ClientConfig) -> (ComposeDraft, MessageId) {
    let draft = client
        .save_new_compose_draft(ComposeDraftUpdate {
            group_name: None,
            text: "reply".into(),
            recipients: vec![ADDRESS.into()],
            attachment_ids: Vec::new(),
            route: Some(route(config)),
        })
        .unwrap();
    let target = client
        .queue_send(sms(draft.conversation_id, "original text"), route(config))
        .unwrap()
        .message_id;
    (draft, target)
}

fn reply_options(target: MessageId) -> ComposeSendOptions {
    ComposeSendOptions {
        reply_to: Some(target),
        ..ComposeSendOptions::default()
    }
}

fn message_with_id(client: &Client, conversation: ConversationId, id: MessageId) -> Message {
    client
        .messages(conversation)
        .unwrap()
        .into_iter()
        .find(|message| message.payload.record.message_id == id)
        .unwrap()
}

fn accept_push(client: &Client) {
    let push = client.pending_draft_pushes().unwrap().remove(0);
    client
        .record_draft_push(
            push.draft_id,
            push.revision_id,
            DraftPushOutcome::Accepted {
                branch_id: uuid::Uuid::new_v4(),
                expires_at_ms: 1_000,
                conflict: false,
            },
        )
        .unwrap();
}

#[test]
fn reply_send_queues_the_reply_target_and_keeps_the_body_unquoted() {
    let dir = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let config = config(&dir, "client", &vault);
    let client = unlocked(&config, &vault);
    let (draft, target) = reply_fixture(&client, &config);

    let queued = client
        .send_compose_draft_with_options(draft.draft_id, draft.revision, reply_options(target))
        .unwrap();

    let sent = message_with_id(&client, draft.conversation_id, queued.message_id);
    assert_eq!(sent.payload.reply_to, Some(target));
    assert_eq!(
        sent.payload.body, "reply",
        "the quote is added by the carrier, not stored"
    );
}

#[test]
fn default_options_send_has_no_reply_target() {
    let dir = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let config = config(&dir, "client", &vault);
    let client = unlocked(&config, &vault);
    let (draft, _) = reply_fixture(&client, &config);

    let queued = client
        .send_compose_draft_with_options(
            draft.draft_id,
            draft.revision,
            ComposeSendOptions::default(),
        )
        .unwrap();

    let sent = message_with_id(&client, draft.conversation_id, queued.message_id);
    assert_eq!(sent.payload.reply_to, None);
}

#[test]
fn reply_target_in_another_conversation_is_refused_and_queues_nothing() {
    let dir = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let config = config(&dir, "client", &vault);
    let client = unlocked(&config, &vault);
    let (draft, _) = reply_fixture(&client, &config);
    let foreign = client
        .queue_send(sms(ConversationId::new(), "elsewhere"), route(&config))
        .unwrap()
        .message_id;
    let outbox_before = client.pending_outbox().unwrap().len();

    assert_invalid_request(client.send_compose_draft_with_options(
        draft.draft_id,
        draft.revision,
        reply_options(foreign),
    ));

    assert_eq!(client.pending_outbox().unwrap().len(), outbox_before);
    assert_eq!(client.messages(draft.conversation_id).unwrap().len(), 1);
}

#[test]
fn missing_reply_target_is_refused() {
    let dir = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let config = config(&dir, "client", &vault);
    let client = unlocked(&config, &vault);
    let (draft, _) = reply_fixture(&client, &config);

    assert_invalid_request(client.send_compose_draft_with_options(
        draft.draft_id,
        draft.revision,
        reply_options(MessageId::new()),
    ));
}

#[test]
fn split_reply_attaches_to_the_first_queued_part_only() {
    let dir = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let config = config(&dir, "client", &vault);
    let client = unlocked(&config, &vault);
    let attachments: Vec<AttachmentId> = (0..2)
        .map(|index| {
            let source = dir.path().join(format!("image-{index}.png"));
            std::fs::write(&source, format!("image-{index}")).unwrap();
            let attachment = client
                .prepare_attachment(&source, "image/png", &format!("image-{index}.png"))
                .unwrap();
            client
                .mark_attachment_uploaded(
                    attachment.attachment_id,
                    &uuid::Uuid::new_v4().to_string(),
                )
                .unwrap();
            attachment.attachment_id
        })
        .collect();
    let draft = client
        .save_new_compose_draft(ComposeDraftUpdate {
            group_name: None,
            text: "caption".into(),
            recipients: vec![ADDRESS.into()],
            attachment_ids: attachments.clone(),
            route: Some(route(&config)),
        })
        .unwrap();
    let target = client
        .queue_send(sms(draft.conversation_id, "original text"), route(&config))
        .unwrap()
        .message_id;
    let parts = DraftSendParts {
        caption: true,
        image_groups: vec![attachments[..1].to_vec(), attachments[1..].to_vec()],
    };

    client
        .send_compose_draft_with_options(
            draft.draft_id,
            draft.revision,
            ComposeSendOptions {
                parts: Some(parts),
                reply_to: Some(target),
                ..ComposeSendOptions::default()
            },
        )
        .unwrap();

    let split: Vec<Message> = client
        .messages(draft.conversation_id)
        .unwrap()
        .into_iter()
        .filter(|message| message.payload.record.message_id != target)
        .collect();
    assert_eq!(split.len(), 3, "caption plus two image parts");
    let first = split
        .iter()
        .find(|message| message.payload.body == "caption")
        .unwrap();
    assert_eq!(first.payload.reply_to, Some(target));
    assert!(
        split
            .iter()
            .filter(|message| message.payload.record.message_id != first.payload.record.message_id)
            .all(|message| message.payload.reply_to.is_none())
    );
}

#[test]
fn reply_to_survives_a_remote_accepted_intent() {
    let dir = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let config = config(&dir, "client", &vault);
    let client = unlocked(&config, &vault);
    let (draft, target) = reply_fixture(&client, &config);
    accept_push(&client);
    let remote = client
        .begin_draft_send_for_group_with_reply(
            draft.draft_id,
            draft.revision,
            None,
            None,
            Some(target),
        )
        .unwrap()
        .remote
        .unwrap();

    client
        .send_compose_draft_accepted_with_parts(
            draft.draft_id,
            draft.revision,
            remote.command_id,
            None,
        )
        .unwrap();

    let replies: Vec<Message> = client
        .messages(draft.conversation_id)
        .unwrap()
        .into_iter()
        .filter(|message| message.payload.reply_to.is_some())
        .collect();
    assert_eq!(replies.len(), 1);
    assert_eq!(replies[0].payload.reply_to, Some(target));
    assert_eq!(replies[0].payload.body, "reply");
}

#[test]
fn remote_intent_refuses_a_foreign_reply_target() {
    let dir = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let config = config(&dir, "client", &vault);
    let client = unlocked(&config, &vault);
    let (draft, _) = reply_fixture(&client, &config);
    accept_push(&client);
    let foreign = client
        .queue_send(sms(ConversationId::new(), "elsewhere"), route(&config))
        .unwrap()
        .message_id;

    assert_invalid_request(client.begin_draft_send_for_group_with_reply(
        draft.draft_id,
        draft.revision,
        None,
        None,
        Some(foreign),
    ));
}

#[test]
fn send_again_keeps_the_reply_target() {
    let dir = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let config = config(&dir, "client", &vault);
    let client = unlocked(&config, &vault);
    let (draft, target) = reply_fixture(&client, &config);
    let original = client
        .send_compose_draft_with_options(draft.draft_id, draft.revision, reply_options(target))
        .unwrap()
        .message_id;

    let copy = client
        .send_again(original, route(&config))
        .unwrap()
        .message_id;

    let copied = message_with_id(&client, draft.conversation_id, copy);
    assert_eq!(copied.payload.reply_to, Some(target));
    assert_eq!(copied.payload.body, "reply");
}
