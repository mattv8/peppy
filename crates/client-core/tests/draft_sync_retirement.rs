use crate::common::*;

fn save_draft(client: &Client, text: &str, route: Option<GatewayRoute>) -> ComposeDraft {
    let draft = client.recipientless_compose_draft().unwrap();
    client
        .save_compose_draft(
            draft.draft_id,
            draft.revision,
            ComposeDraftUpdate {
                group_name: None,
                text: text.into(),
                recipients: vec![ADDRESS.into()],
                attachment_ids: vec![],
                route,
            },
        )
        .unwrap()
}

fn remote_head(push: &DraftPush, branch_id: uuid::Uuid) -> RemoteDraftChange {
    RemoteDraftChange::Head {
        draft_id: push.draft_id,
        branch_id,
        revision_id: push.revision_id,
        parent_revision_id: push.base_revision_id,
        sealed_record: push.sealed_record.clone(),
        attachment_ids: push.attachment_ids.clone(),
        expires_at_ms: 1_000,
    }
}

fn accept_push(client: &Client, push: &DraftPush, branch_id: uuid::Uuid) {
    client
        .record_draft_push(
            push.draft_id,
            push.revision_id,
            DraftPushOutcome::Accepted {
                branch_id,
                expires_at_ms: 1_000,
                conflict: false,
            },
        )
        .unwrap();
}

#[test]
fn retired_send_survives_discard_ack_late_head_and_terminal_without_a_push() {
    let dir = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let config = config(&dir, "client", &vault);
    let client = unlocked(&config, &vault);
    let saved = save_draft(&client, "sent", Some(route(&config)));
    let push = client.pending_draft_pushes().unwrap().remove(0);

    client
        .send_compose_draft(saved.draft_id, saved.revision)
        .unwrap();
    client.record_draft_discard(saved.draft_id).unwrap();
    client
        .apply_draft_changes(vec![remote_head(&push, uuid::Uuid::new_v4())], "1")
        .unwrap();
    client
        .apply_draft_changes(
            vec![RemoteDraftChange::Terminal {
                draft_id: saved.draft_id,
                reason: DraftTerminalReasonCore::Discarded,
                command_id: None,
            }],
            "2",
        )
        .unwrap();
    accept_push(&client, &push, uuid::Uuid::new_v4());

    let cleared = client.compose_draft(saved.draft_id).unwrap().unwrap();
    assert!(cleared.text.is_empty());
    assert!(cleared.attachment_ids.is_empty());
    assert_eq!(cleared.revision, saved.revision + 1);
    assert!(client.pending_draft_pushes().unwrap().is_empty());
    assert!(client.pending_draft_discards().unwrap().is_empty());
    let replacement = client
        .save_compose_draft(
            saved.draft_id,
            cleared.revision,
            ComposeDraftUpdate {
                group_name: None,
                text: "next".into(),
                recipients: cleared.recipients,
                attachment_ids: vec![],
                route: cleared.route,
            },
        )
        .unwrap();
    assert_ne!(replacement.draft_id, saved.draft_id);
}

#[test]
fn route_only_save_keeps_retired_identity_until_an_authored_save_rotates_it() {
    let dir = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let config = config(&dir, "client", &vault);
    let client = unlocked(&config, &vault);
    let saved = save_draft(&client, "sent", Some(route(&config)));

    client
        .send_compose_draft(saved.draft_id, saved.revision)
        .unwrap();
    let cleared = client.compose_draft(saved.draft_id).unwrap().unwrap();
    let route_only = client
        .save_compose_draft(
            cleared.draft_id,
            cleared.revision,
            ComposeDraftUpdate {
                group_name: None,
                text: cleared.text.clone(),
                recipients: cleared.recipients.clone(),
                attachment_ids: cleared.attachment_ids.clone(),
                route: Some(GatewayRoute::Carrier {
                    gateway_device_id: config.device_id,
                    subscription_id: "sim-2".into(),
                }),
            },
        )
        .unwrap();
    assert_eq!(route_only.draft_id, saved.draft_id);
    assert!(client.pending_draft_pushes().unwrap().is_empty());

    let edited = client
        .save_compose_draft(
            route_only.draft_id,
            route_only.revision,
            ComposeDraftUpdate {
                group_name: None,
                text: "next".into(),
                recipients: route_only.recipients,
                attachment_ids: vec![],
                route: route_only.route,
            },
        )
        .unwrap();
    assert_ne!(edited.draft_id, saved.draft_id);
    assert_eq!(
        client.pending_draft_pushes().unwrap()[0].draft_id,
        edited.draft_id
    );
}

#[test]
fn save_new_keeps_a_retired_cleared_draft_until_an_authored_follow_up() {
    let dir = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let config = config(&dir, "client", &vault);
    let client = unlocked(&config, &vault);
    let sent = client
        .save_new_compose_draft(ComposeDraftUpdate {
            group_name: None,
            text: "sent".into(),
            recipients: vec![ADDRESS.into()],
            attachment_ids: vec![],
            route: Some(route(&config)),
        })
        .unwrap();
    client
        .send_compose_draft(sent.draft_id, sent.revision)
        .unwrap();

    let reopened = client
        .save_new_compose_draft(ComposeDraftUpdate {
            group_name: None,
            text: String::new(),
            recipients: vec![ADDRESS.into()],
            attachment_ids: vec![],
            route: Some(route(&config)),
        })
        .unwrap();
    assert_eq!(reopened.draft_id, sent.draft_id);
    assert!(client.pending_draft_pushes().unwrap().is_empty());

    let replacement = client
        .save_new_compose_draft(ComposeDraftUpdate {
            group_name: None,
            text: "next".into(),
            recipients: vec![ADDRESS.into()],
            attachment_ids: vec![],
            route: Some(route(&config)),
        })
        .unwrap();
    assert_ne!(replacement.draft_id, sent.draft_id);
    assert_eq!(
        client.pending_draft_pushes().unwrap()[0].draft_id,
        replacement.draft_id
    );
}

#[test]
fn save_new_rotates_a_retired_cleared_draft_for_new_content() {
    let dir = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let config = config(&dir, "client", &vault);
    let client = unlocked(&config, &vault);
    let sent = client
        .save_new_compose_draft(ComposeDraftUpdate {
            group_name: None,
            text: "sent".into(),
            recipients: vec![ADDRESS.into()],
            attachment_ids: vec![],
            route: Some(route(&config)),
        })
        .unwrap();
    client
        .send_compose_draft(sent.draft_id, sent.revision)
        .unwrap();

    let replacement = client
        .save_new_compose_draft(ComposeDraftUpdate {
            group_name: None,
            text: "next".into(),
            recipients: vec![ADDRESS.into()],
            attachment_ids: vec![],
            route: Some(route(&config)),
        })
        .unwrap();
    assert_ne!(replacement.draft_id, sent.draft_id);
    assert_eq!(
        client.pending_draft_pushes().unwrap()[0].draft_id,
        replacement.draft_id
    );
}

#[test]
fn moving_into_a_retired_draft_rotates_the_destination_and_updates_redirects() {
    let dir = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let config = config(&dir, "client", &vault);
    let client = unlocked(&config, &vault);
    let destination_address = "+15555550101";
    let destination = client
        .save_new_compose_draft(ComposeDraftUpdate {
            group_name: None,
            text: "destination".into(),
            recipients: vec![destination_address.into()],
            attachment_ids: vec![],
            route: Some(route(&config)),
        })
        .unwrap();
    let late_head = client.pending_draft_pushes().unwrap().remove(0);
    client
        .send_compose_draft(destination.draft_id, destination.revision)
        .unwrap();
    let attachment_path = dir.path().join("incoming.png");
    std::fs::write(&attachment_path, b"image").unwrap();
    let attachment = client
        .prepare_attachment(&attachment_path, "image/png", "incoming.png")
        .unwrap();
    let source = client
        .save_new_compose_draft(ComposeDraftUpdate {
            group_name: None,
            text: "source".into(),
            recipients: vec![ADDRESS.into()],
            attachment_ids: vec![],
            route: None,
        })
        .unwrap();

    let moved = client
        .save_compose_draft(
            source.draft_id,
            source.revision,
            ComposeDraftUpdate {
                group_name: None,
                text: "incoming".into(),
                recipients: vec![destination_address.into()],
                attachment_ids: vec![attachment.attachment_id],
                route: None,
            },
        )
        .unwrap();
    assert_ne!(moved.draft_id, destination.draft_id);
    assert_eq!(moved.text, "incoming");
    assert_eq!(moved.attachment_ids, vec![attachment.attachment_id]);
    assert_eq!(moved.route, Some(route(&config)));
    client
        .apply_draft_changes(vec![remote_head(&late_head, uuid::Uuid::new_v4())], "1")
        .unwrap();
    assert_eq!(
        client.compose_draft(moved.draft_id).unwrap().unwrap(),
        moved
    );
    assert!(matches!(
        client.save_compose_draft(
            destination.draft_id,
            destination.revision,
            ComposeDraftUpdate {
                group_name: None,
                text: "stale".into(),
                recipients: vec![destination_address.into()],
                attachment_ids: vec![],
                route: Some(route(&config)),
            },
        ),
        Err(Error::DraftMoved { .. } | Error::NotFound | Error::StaleDraft { .. })
    ));
    assert!(client.pending_draft_pushes().unwrap().is_empty());
}

#[test]
fn accepted_remote_send_retires_locally_without_queuing_a_remote_discard() {
    let dir = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let config = config(&dir, "client", &vault);
    let client = unlocked(&config, &vault);
    let saved = save_draft(&client, "sent", Some(route(&config)));
    let push = client.pending_draft_pushes().unwrap().remove(0);
    accept_push(&client, &push, uuid::Uuid::new_v4());
    let gate = client
        .begin_draft_send(saved.draft_id, saved.revision, None)
        .unwrap();
    let remote = gate.remote.unwrap();

    client
        .send_compose_draft_accepted(saved.draft_id, saved.revision, remote.command_id)
        .unwrap();

    assert!(client.pending_draft_discards().unwrap().is_empty());
    let cleared = client.compose_draft(saved.draft_id).unwrap().unwrap();
    assert!(cleared.text.is_empty());
    let replacement = client
        .save_compose_draft(
            saved.draft_id,
            cleared.revision,
            ComposeDraftUpdate {
                group_name: None,
                text: "next".into(),
                recipients: cleared.recipients,
                attachment_ids: vec![],
                route: cleared.route,
            },
        )
        .unwrap();
    assert_ne!(replacement.draft_id, saved.draft_id);
}

#[test]
fn resolving_a_foreign_identity_keeps_the_local_base_without_closing_any_branch() {
    let dir = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let local = unlocked(&config(&dir, "local", &vault), &vault);
    let remote = unlocked(&config(&dir, "remote", &vault), &vault);
    let local_draft = save_draft(&local, "local", None);
    let local_push = local.pending_draft_pushes().unwrap().remove(0);
    let local_branch = uuid::Uuid::new_v4();
    accept_push(&local, &local_push, local_branch);
    let remote_draft = save_draft(&remote, "remote", None);
    let remote_push = remote.pending_draft_pushes().unwrap().remove(0);
    let remote_branch = uuid::Uuid::new_v4();

    local
        .apply_draft_changes(vec![remote_head(&remote_push, remote_branch)], "1")
        .unwrap();
    let resolved = local
        .resolve_draft_conflict(local_draft.draft_id, Some(remote_branch))
        .unwrap();
    let outgoing = local.pending_draft_pushes().unwrap().remove(0);

    assert_eq!(resolved.text, "remote");
    assert_eq!(outgoing.draft_id, local_draft.draft_id);
    assert_eq!(outgoing.base_revision_id, Some(local_push.revision_id));
    assert!(!outgoing.resolves_branch_ids.contains(&local_branch));
    assert!(!outgoing.resolves_branch_ids.contains(&remote_branch));
    assert!(outgoing.resolves_branch_ids.is_empty());
    assert_ne!(remote_draft.draft_id, local_draft.draft_id);
}
