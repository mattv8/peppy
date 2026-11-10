use crate::common::*;

fn saved_split_draft(
    client: &Client,
    config: &ClientConfig,
    dir: &tempfile::TempDir,
    text: &str,
    count: usize,
) -> (ComposeDraft, Vec<AttachmentId>) {
    let attachment_ids: Vec<AttachmentId> = (0..count)
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
            text: text.into(),
            recipients: vec![ADDRESS.into()],
            attachment_ids: attachment_ids.clone(),
            route: Some(route(config)),
        })
        .unwrap();
    (draft, attachment_ids)
}

fn split_parts(ids: &[AttachmentId], caption: bool) -> DraftSendParts {
    DraftSendParts {
        caption,
        image_groups: vec![ids[..1].to_vec(), ids[1..].to_vec()],
    }
}

fn accept_draft_push(client: &Client, draft: &ComposeDraft) {
    let push = client.pending_draft_pushes().unwrap().remove(0);
    assert_eq!(push.draft_id, draft.draft_id);
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
fn split_send_queues_caption_then_image_only_mms_parts_in_order() {
    let dir = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let config = config(&dir, "client", &vault);
    let client = unlocked(&config, &vault);
    let (draft, ids) = saved_split_draft(&client, &config, &dir, "caption", 3);

    client
        .send_compose_draft_with_parts(draft.draft_id, draft.revision, split_parts(&ids, true))
        .unwrap();

    let messages = client.messages(draft.conversation_id).unwrap();
    assert_eq!(messages.len(), 3);
    assert_eq!(messages[0].payload.body, "caption");
    assert!(messages[0].payload.record.attachments.is_empty());
    for (message, expected) in messages[1..].iter().zip([&ids[..1], &ids[1..]]) {
        assert_eq!(message.payload.transport, Transport::Mms);
        assert!(message.payload.body.is_empty());
        assert_eq!(
            message
                .payload
                .record
                .attachments
                .iter()
                .map(|item| item.attachment_id)
                .collect::<Vec<_>>(),
            expected,
        );
    }
    assert!(
        messages
            .iter()
            .all(|message| message.payload.transport == Transport::Mms)
    );
}

#[test]
fn empty_split_caption_omits_the_text_only_part() {
    let dir = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let config = config(&dir, "client", &vault);
    let client = unlocked(&config, &vault);
    let (draft, ids) = saved_split_draft(&client, &config, &dir, "   ", 3);

    client
        .send_compose_draft_with_parts(draft.draft_id, draft.revision, split_parts(&ids, false))
        .unwrap();

    let messages = client.messages(draft.conversation_id).unwrap();
    assert_eq!(messages.len(), 2);
    assert!(
        messages
            .iter()
            .all(|message| message.payload.body.is_empty())
    );
}

#[test]
fn invalid_split_plan_rolls_back_without_clearing_the_draft() {
    let dir = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let config = config(&dir, "client", &vault);
    let client = unlocked(&config, &vault);
    let (draft, ids) = saved_split_draft(&client, &config, &dir, "caption", 2);
    let invalid = DraftSendParts {
        caption: true,
        image_groups: vec![vec![ids[0]], vec![ids[0]]],
    };

    assert!(
        client
            .send_compose_draft_with_parts(draft.draft_id, draft.revision, invalid)
            .is_err()
    );
    assert!(client.messages(draft.conversation_id).unwrap().is_empty());
    assert_eq!(
        client.compose_draft(draft.draft_id).unwrap().unwrap(),
        draft
    );
}

#[test]
fn accepted_command_id_belongs_to_the_first_split_part() {
    let dir = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let config = config(&dir, "client", &vault);
    let client = unlocked(&config, &vault);
    let (draft, ids) = saved_split_draft(&client, &config, &dir, "caption", 2);
    accept_draft_push(&client, &draft);
    let accepted_id = client
        .begin_draft_send(
            draft.draft_id,
            draft.revision,
            Some(split_parts(&ids, true)),
        )
        .unwrap()
        .remote
        .unwrap()
        .command_id;

    client
        .send_compose_draft_accepted_with_parts(draft.draft_id, draft.revision, accepted_id, None)
        .unwrap();

    let outbox = client.pending_outbox().unwrap();
    assert_eq!(outbox.len(), 3);
    assert_eq!(outbox[0].command_id, Some(accepted_id));
    assert!(
        outbox[1..]
            .iter()
            .all(|envelope| envelope.command_id != Some(accepted_id))
    );
}

#[test]
fn persisted_send_intent_reports_split_part_count_after_queueing() {
    let dir = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let config = config(&dir, "client", &vault);
    let client = unlocked(&config, &vault);
    let (draft, ids) = saved_split_draft(&client, &config, &dir, "caption", 2);
    accept_draft_push(&client, &draft);
    let command_id = client
        .begin_draft_send(
            draft.draft_id,
            draft.revision,
            Some(split_parts(&ids, true)),
        )
        .unwrap()
        .remote
        .unwrap()
        .command_id;
    client
        .send_compose_draft_accepted(draft.draft_id, draft.revision, command_id)
        .unwrap();

    assert_eq!(
        client
            .draft_send_intent_parts_count(draft.draft_id, command_id)
            .unwrap(),
        Some(3)
    );
    assert_eq!(
        client
            .draft_send_intent_parts_count(draft.draft_id, CommandId::new())
            .unwrap(),
        None
    );
}

#[test]
fn sent_terminal_recovers_the_persisted_split_plan_once() {
    let dir = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let config = config(&dir, "client", &vault);
    let client = unlocked(&config, &vault);
    let (draft, ids) = saved_split_draft(&client, &config, &dir, "caption", 3);
    accept_draft_push(&client, &draft);
    let remote = client
        .begin_draft_send(
            draft.draft_id,
            draft.revision,
            Some(split_parts(&ids, true)),
        )
        .unwrap()
        .remote
        .unwrap();

    client
        .apply_draft_changes(
            vec![RemoteDraftChange::Terminal {
                draft_id: draft.draft_id,
                reason: DraftTerminalReasonCore::Sent,
                command_id: Some(remote.command_id),
            }],
            "1",
        )
        .unwrap();

    let first = client.pending_outbox().unwrap();
    assert_eq!(first.len(), 3);
    assert_eq!(first[0].command_id, Some(remote.command_id));
    client.apply_draft_changes(vec![], "2").unwrap();
    assert_eq!(client.pending_outbox().unwrap(), first);
}
