//! Atomic initial compose-draft creation and reuse.

use crate::common::*;
use std::fs;
use tempfile::TempDir;

const PEER: &str = "+14155552671";

fn update(
    text: &str,
    recipients: &[&str],
    attachment_ids: Vec<AttachmentId>,
) -> ComposeDraftUpdate {
    ComposeDraftUpdate {
        text: text.into(),
        recipients: recipients
            .iter()
            .map(|recipient| (*recipient).into())
            .collect(),
        attachment_ids,
        route: None,
    }
}

fn attachment(client: &Client, dir: &TempDir) -> AttachmentId {
    let path = dir.path().join("draft.png");
    fs::write(&path, b"draft attachment").unwrap();
    client
        .prepare_attachment(&path, "image/png", "draft.png")
        .unwrap()
        .attachment_id
}

#[test]
fn blank_open_reuses_a_revised_recipientless_draft() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "client", &vault), &vault);

    let created = client
        .save_new_compose_draft(update("kept", &[], vec![]))
        .unwrap();
    let opened = client
        .save_new_compose_draft(ComposeDraftUpdate::default())
        .unwrap();

    assert_eq!(opened, created);
}

#[test]
fn recipientless_attachment_draft_is_not_bypassed_or_overwritten() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "client", &vault), &vault);
    let media = attachment(&client, &dir);

    let existing = client
        .save_new_compose_draft(update("", &[], vec![media]))
        .unwrap();
    assert_eq!(
        client
            .save_new_compose_draft(ComposeDraftUpdate::default())
            .unwrap(),
        existing
    );
    assert!(matches!(
        client.save_new_compose_draft(update("new", &[], vec![])),
        Err(Error::DraftMoved { draft_id, conversation_id, current_revision })
            if draft_id == existing.draft_id
                && conversation_id == existing.conversation_id
                && current_revision == existing.revision
    ));
    assert_eq!(
        client.compose_draft(existing.draft_id).unwrap().unwrap(),
        existing
    );
}

#[test]
fn invalid_initial_input_leaves_no_draft_row() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "client", &vault), &vault);

    for invalid in [
        update(&"x".repeat(64 * 1024 + 1), &[], vec![]),
        update("", &vec![PEER; 21], vec![]),
        update("", &[""], vec![]),
    ] {
        assert!(matches!(
            client.save_new_compose_draft(invalid),
            Err(Error::InvalidRequest(_))
        ));
        assert!(client.compose_drafts().unwrap().is_empty());
    }

    assert!(matches!(
        client.save_new_compose_draft(ComposeDraftUpdate {
            route: Some(GatewayRoute {
                gateway_device_id: DeviceId::new(),
                subscription_id: "x".repeat(513),
            }),
            ..Default::default()
        }),
        Err(Error::InvalidRequest("subscription id"))
    ));
    assert!(client.compose_drafts().unwrap().is_empty());
}

#[test]
fn preset_recipients_resolve_history_without_a_recipientless_row() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "client", &vault), &vault);
    let history = client
        .capture_incoming(incoming("history", "history-1"))
        .unwrap();

    let draft = client
        .save_new_compose_draft(update("initial", &[ADDRESS], vec![]))
        .unwrap();

    assert_eq!(draft.conversation_id, history.conversation_id);
    assert_eq!(draft.text, "initial");
    assert_eq!(client.compose_drafts().unwrap(), vec![draft.clone()]);
    assert!(client.compose_draft_is_new(draft.draft_id).unwrap());
}

#[test]
fn blank_preset_reply_adopts_new_compose_intent_before_group_fork() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "client", &vault), &vault);
    let history = client
        .capture_incoming(incoming("history", "history-1"))
        .unwrap();
    let reply = client
        .create_compose_draft(Some(history.conversation_id))
        .unwrap();
    let reply = client
        .save_compose_draft(
            reply.draft_id,
            reply.revision,
            update("retained", &[ADDRESS], vec![]),
        )
        .unwrap();

    let opened = client
        .save_new_compose_draft(update("", &[ADDRESS], vec![]))
        .unwrap();

    assert_eq!(opened.draft_id, reply.draft_id);
    assert_eq!(opened.conversation_id, history.conversation_id);
    assert_eq!(opened.text, "retained");
    assert_eq!(opened.recipients, vec![ADDRESS]);
    assert_eq!(opened.revision, reply.revision + 1);
    assert!(client.compose_draft_is_new(opened.draft_id).unwrap());

    let group = client
        .save_compose_draft(
            opened.draft_id,
            opened.revision,
            update("group", &[ADDRESS, PEER], vec![]),
        )
        .unwrap();
    assert_ne!(group.conversation_id, history.conversation_id);
    assert_eq!(client.messages(history.conversation_id).unwrap().len(), 1);
}

#[test]
fn preset_existing_content_returns_its_recovery_identity() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "client", &vault), &vault);
    let existing = client
        .save_new_compose_draft(update("stored", &[PEER], vec![]))
        .unwrap();

    assert_eq!(
        client
            .save_new_compose_draft(update("", &[PEER], vec![]))
            .unwrap(),
        existing
    );
    assert!(matches!(
        client.save_new_compose_draft(update("submitted", &[PEER], vec![])),
        Err(Error::DraftMoved { draft_id, conversation_id, current_revision })
            if draft_id == existing.draft_id
                && conversation_id == existing.conversation_id
                && current_revision == existing.revision
    ));
    assert_eq!(
        client.compose_draft(existing.draft_id).unwrap().unwrap(),
        existing
    );
}
