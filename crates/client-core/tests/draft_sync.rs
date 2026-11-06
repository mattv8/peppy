use crate::common::*;

#[test]
fn authored_save_creates_a_stable_pending_push() {
    let dir = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let client_config = config(&dir, "client", &vault);
    let client = unlocked(&client_config, &vault);
    let draft = client.recipientless_compose_draft().unwrap();
    let saved = client
        .save_compose_draft(
            draft.draft_id,
            draft.revision,
            ComposeDraftUpdate {
                text: "draft".into(),
                recipients: vec!["+14155552671".into()],
                attachment_ids: vec![],
                route: None,
            },
        )
        .unwrap();
    let first = client.pending_draft_pushes().unwrap();
    let second = client.pending_draft_pushes().unwrap();
    assert_eq!(first, second);
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].draft_id, saved.draft_id);
    assert!(first[0].authored);
}

fn save_draft(client: &Client, text: &str, recipients: Vec<String>) -> ComposeDraft {
    let draft = client.recipientless_compose_draft().unwrap();
    client
        .save_compose_draft(
            draft.draft_id,
            draft.revision,
            ComposeDraftUpdate {
                text: text.into(),
                recipients,
                attachment_ids: vec![],
                route: None,
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
fn undecryptable_head_keeps_the_page_cursor() {
    let dir = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let sender = unlocked(&config(&dir, "sender", &vault), &vault);
    let receiver = unlocked(&config(&dir, "receiver", &vault), &vault);
    let saved = save_draft(&sender, "remote", vec![ADDRESS.into()]);
    let push = sender.pending_draft_pushes().unwrap().remove(0);
    let mut change = remote_head(&push, uuid::Uuid::new_v4());
    let RemoteDraftChange::Head { sealed_record, .. } = &mut change else {
        unreachable!();
    };
    sealed_record[4] ^= 1;

    assert!(receiver.apply_draft_changes(vec![change], "1").is_err());
    assert_eq!(receiver.draft_sync_cursor().unwrap(), "0");
    assert!(receiver.compose_draft(saved.draft_id).unwrap().is_none());
}

#[test]
fn accepted_echo_preserves_a_later_pending_save() {
    let dir = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let sender = unlocked(&config(&dir, "sender", &vault), &vault);
    let receiver = unlocked(&config(&dir, "receiver", &vault), &vault);
    let saved = save_draft(&sender, "r1", vec![ADDRESS.into()]);
    let r1 = sender.pending_draft_pushes().unwrap().remove(0);
    let branch_id = uuid::Uuid::new_v4();
    receiver
        .apply_draft_changes(vec![remote_head(&r1, branch_id)], "1")
        .unwrap();
    let adopted = receiver.compose_draft(saved.draft_id).unwrap().unwrap();
    receiver
        .save_compose_draft(
            adopted.draft_id,
            adopted.revision,
            ComposeDraftUpdate {
                text: "r2".into(),
                recipients: adopted.recipients,
                attachment_ids: vec![],
                route: None,
            },
        )
        .unwrap();
    let pending = receiver.pending_draft_pushes().unwrap().remove(0);

    receiver
        .apply_draft_changes(vec![remote_head(&r1, branch_id)], "2")
        .unwrap();

    assert_eq!(receiver.pending_draft_pushes().unwrap(), vec![pending]);
    assert_eq!(
        receiver
            .compose_draft(saved.draft_id)
            .unwrap()
            .unwrap()
            .text,
        "r2"
    );
}

#[test]
fn clean_remote_head_adopts_into_matching_conversation() {
    let dir = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let sender = unlocked(&config(&dir, "sender", &vault), &vault);
    let receiver = unlocked(&config(&dir, "receiver", &vault), &vault);
    let saved = save_draft(&sender, "remote", vec![ADDRESS.into()]);
    let push = sender.pending_draft_pushes().unwrap().remove(0);

    receiver
        .apply_draft_changes(vec![remote_head(&push, uuid::Uuid::new_v4())], "1")
        .unwrap();

    let adopted = receiver.compose_draft(saved.draft_id).unwrap().unwrap();
    assert_eq!(adopted.text, "remote");
    assert_eq!(adopted.recipients, vec![ADDRESS]);
}

#[test]
fn dirty_local_head_is_stored_as_a_conflict() {
    let dir = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "client", &vault), &vault);
    let saved = save_draft(&client, "local", vec![ADDRESS.into()]);
    let push = client.pending_draft_pushes().unwrap().remove(0);
    let branch_id = uuid::Uuid::new_v4();

    client
        .apply_draft_changes(vec![remote_head(&push, branch_id)], "1")
        .unwrap();

    let status = client.draft_sync_status(saved.draft_id).unwrap();
    assert_eq!(status.conflicts.len(), 1);
    assert_eq!(status.conflicts[0].branch_id, branch_id);
    assert_eq!(
        client.compose_draft(saved.draft_id).unwrap().unwrap().text,
        "local"
    );
}

#[test]
fn remote_identity_does_not_overwrite_a_nonblank_conversation_draft() {
    let dir = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let sender = unlocked(&config(&dir, "sender", &vault), &vault);
    let receiver = unlocked(&config(&dir, "receiver", &vault), &vault);
    let local = save_draft(&receiver, "local", vec![ADDRESS.into()]);
    let remote = save_draft(&sender, "remote", vec![ADDRESS.into()]);
    let push = sender.pending_draft_pushes().unwrap().remove(0);
    let branch_id = uuid::Uuid::new_v4();

    receiver
        .apply_draft_changes(vec![remote_head(&push, branch_id)], "1")
        .unwrap();

    assert_eq!(
        receiver
            .compose_draft(local.draft_id)
            .unwrap()
            .unwrap()
            .text,
        "local"
    );
    assert!(receiver.compose_draft(remote.draft_id).unwrap().is_none());
    assert_eq!(
        receiver
            .draft_sync_status(local.draft_id)
            .unwrap()
            .conflicts[0]
            .branch_id,
        branch_id
    );
    receiver
        .apply_draft_changes(
            vec![RemoteDraftChange::BranchClosed {
                draft_id: remote.draft_id,
                branch_id,
            }],
            "2",
        )
        .unwrap();
    assert!(
        receiver
            .draft_sync_status(local.draft_id)
            .unwrap()
            .conflicts
            .is_empty()
    );
}

#[test]
fn resolving_conflict_keeps_local_and_queues_other_branches() {
    let dir = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "client", &vault), &vault);
    let saved = save_draft(&client, "local", vec![ADDRESS.into()]);
    let push = client.pending_draft_pushes().unwrap().remove(0);
    let branch_id = uuid::Uuid::new_v4();
    client
        .apply_draft_changes(vec![remote_head(&push, branch_id)], "1")
        .unwrap();

    let resolved = client.resolve_draft_conflict(saved.draft_id, None).unwrap();
    let pending = client.pending_draft_pushes().unwrap().remove(0);

    assert_eq!(resolved.text, "local");
    assert_eq!(pending.resolves_branch_ids, vec![branch_id]);
}

#[test]
fn resolving_conflict_with_remote_version_applies_it_and_closed_branch_removes_it() {
    let dir = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "client", &vault), &vault);
    let saved = save_draft(&client, "remote", vec![ADDRESS.into()]);
    let push = client.pending_draft_pushes().unwrap().remove(0);
    let branch_id = uuid::Uuid::new_v4();
    client
        .apply_draft_changes(vec![remote_head(&push, branch_id)], "1")
        .unwrap();

    let resolved = client
        .resolve_draft_conflict(saved.draft_id, Some(branch_id))
        .unwrap();
    client
        .apply_draft_changes(
            vec![RemoteDraftChange::BranchClosed {
                draft_id: saved.draft_id,
                branch_id,
            }],
            "2",
        )
        .unwrap();

    assert_eq!(resolved.text, "remote");
    assert!(
        client
            .draft_sync_status(saved.draft_id)
            .unwrap()
            .conflicts
            .is_empty()
    );
}

#[test]
fn discard_of_synced_draft_is_queued_then_cleared() {
    let dir = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "client", &vault), &vault);
    let draft = client.recipientless_compose_draft().unwrap();
    let saved = client
        .save_compose_draft(
            draft.draft_id,
            draft.revision,
            ComposeDraftUpdate::default(),
        )
        .unwrap();
    let push = client.pending_draft_pushes().unwrap().remove(0);
    client
        .record_draft_push(
            saved.draft_id,
            push.revision_id,
            DraftPushOutcome::Accepted {
                branch_id: uuid::Uuid::new_v4(),
                expires_at_ms: 1_000,
                conflict: false,
            },
        )
        .unwrap();

    client
        .discard_compose_draft(saved.draft_id, saved.revision)
        .unwrap();
    assert_eq!(
        client.pending_draft_discards().unwrap(),
        vec![saved.draft_id]
    );
    client.record_draft_discard(saved.draft_id).unwrap();
    assert!(client.pending_draft_discards().unwrap().is_empty());
}

#[test]
fn attachment_failure_states_map_upload_download_and_unavailable() {
    let dir = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let uploader = unlocked(&config(&dir, "uploader", &vault), &vault);
    let receiver = unlocked(&config(&dir, "receiver", &vault), &vault);
    let source = dir.path().join("draft-image.png");
    std::fs::write(&source, b"draft image").unwrap();
    let attachment = uploader
        .prepare_attachment(&source, "image/png", "draft-image.png")
        .unwrap();
    let draft = uploader.recipientless_compose_draft().unwrap();
    let saved = uploader
        .save_compose_draft(
            draft.draft_id,
            draft.revision,
            ComposeDraftUpdate {
                text: "image".into(),
                recipients: vec![ADDRESS.into()],
                attachment_ids: vec![attachment.attachment_id],
                route: None,
            },
        )
        .unwrap();
    uploader
        .record_draft_attachment_transfer_failure(
            attachment.attachment_id,
            DraftTransferDirection::Upload,
        )
        .unwrap();
    assert_eq!(
        uploader.draft_attachment_states(saved.draft_id).unwrap()[0].1,
        DraftAttachmentState::UploadFailed
    );

    uploader
        .mark_attachment_uploaded(attachment.attachment_id, &uuid::Uuid::new_v4().to_string())
        .unwrap();
    let push = uploader.pending_draft_pushes().unwrap().remove(0);
    receiver
        .apply_draft_changes(vec![remote_head(&push, uuid::Uuid::new_v4())], "1")
        .unwrap();
    receiver
        .record_draft_attachment_transfer_failure(
            attachment.attachment_id,
            DraftTransferDirection::Download,
        )
        .unwrap();
    assert_eq!(
        receiver.draft_attachment_states(saved.draft_id).unwrap()[0].1,
        DraftAttachmentState::DownloadFailed
    );

    let cipher = uploader
        .native_cipher_file(attachment.attachment_id)
        .unwrap();
    std::fs::remove_file(cipher).unwrap();
    assert_eq!(
        uploader.draft_attachment_states(saved.draft_id).unwrap()[0].1,
        DraftAttachmentState::Unavailable
    );
}

#[test]
fn downloaded_synced_draft_queues_one_mms_after_verified_install() {
    let dir = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let sender_config = config(&dir, "sender", &vault);
    let receiver_config = config(&dir, "receiver", &vault);
    let sender = unlocked(&sender_config, &vault);
    let receiver = unlocked(&receiver_config, &vault);
    let source = dir.path().join("shared-image.png");
    std::fs::write(&source, b"shared draft image").unwrap();
    let attachment = sender
        .prepare_attachment(&source, "image/png", "shared-image.png")
        .unwrap();
    let cipher =
        std::fs::read(sender.native_cipher_file(attachment.attachment_id).unwrap()).unwrap();
    sender
        .mark_attachment_uploaded(attachment.attachment_id, &uuid::Uuid::new_v4().to_string())
        .unwrap();
    let saved = sender
        .save_new_compose_draft(ComposeDraftUpdate {
            text: "caption".into(),
            recipients: vec![ADDRESS.into()],
            attachment_ids: vec![attachment.attachment_id],
            route: Some(route(&sender_config)),
        })
        .unwrap();
    let push = sender.pending_draft_pushes().unwrap().remove(0);
    receiver
        .apply_draft_changes(vec![remote_head(&push, uuid::Uuid::new_v4())], "1")
        .unwrap();
    let received = receiver.compose_draft(saved.draft_id).unwrap().unwrap();
    assert_eq!(received.route, Some(route(&sender_config)));
    assert!(
        receiver
            .begin_draft_send(saved.draft_id, received.revision)
            .is_err()
    );
    assert!(receiver.pending_outbox().unwrap().is_empty());

    let download = dir.path().join("shared-image.cipher");
    std::fs::write(&download, cipher).unwrap();
    receiver
        .install_downloaded_attachment(attachment.attachment_id, &download)
        .unwrap();
    assert_eq!(
        receiver
            .attachment_info(attachment.attachment_id)
            .unwrap()
            .state,
        AttachmentState::Available
    );
    assert_eq!(
        receiver.draft_attachment_states(saved.draft_id).unwrap()[0].1,
        DraftAttachmentState::Uploaded
    );

    let routed = receiver
        .save_compose_draft(
            saved.draft_id,
            received.revision,
            ComposeDraftUpdate {
                text: received.text,
                recipients: received.recipients,
                attachment_ids: received.attachment_ids,
                route: Some(route(&receiver_config)),
            },
        )
        .unwrap();
    let route_push = receiver.pending_draft_pushes().unwrap().remove(0);
    accept_push(&receiver, &route_push, uuid::Uuid::new_v4());
    let send = receiver
        .begin_draft_send(saved.draft_id, routed.revision)
        .unwrap()
        .remote
        .unwrap();
    receiver
        .send_compose_draft_accepted(saved.draft_id, routed.revision, send.command_id)
        .unwrap();
    let queued = receiver.pending_outbox().unwrap();
    assert_eq!(queued.len(), 1);
    let message = only_message(&receiver, received.conversation_id);
    assert_eq!(message.payload.body, "caption");
    assert_eq!(message.payload.transport, Transport::Mms);
    assert_eq!(
        message.payload.record.attachments,
        vec![AttachmentReference {
            attachment_id: attachment.attachment_id,
            pending: false,
        }]
    );
    receiver
        .send_compose_draft_accepted(saved.draft_id, routed.revision, send.command_id)
        .unwrap();
    assert_eq!(receiver.pending_outbox().unwrap(), queued);
}

#[test]
fn resolving_same_draft_conflicts_uses_the_selected_branch_as_the_next_base() {
    for keep_remote in [false, true] {
        let dir = tempfile::TempDir::new().unwrap();
        let vault = Vault::new();
        let client = unlocked(&config(&dir, "client", &vault), &vault);
        let saved = save_draft(&client, "A", vec![ADDRESS.into()]);
        let push_a = client.pending_draft_pushes().unwrap().remove(0);
        let branch_a = uuid::Uuid::new_v4();
        accept_push(&client, &push_a, branch_a);
        let edited = client
            .save_compose_draft(
                saved.draft_id,
                saved.revision,
                ComposeDraftUpdate {
                    text: "B".into(),
                    recipients: vec![ADDRESS.into()],
                    attachment_ids: vec![],
                    route: None,
                },
            )
            .unwrap();
        let push_b = client.pending_draft_pushes().unwrap().remove(0);
        let branch_b = uuid::Uuid::new_v4();
        client
            .apply_draft_changes(vec![remote_head(&push_a, branch_a)], "1")
            .unwrap();
        client
            .apply_draft_changes(vec![remote_head(&push_b, branch_b)], "2")
            .unwrap();

        let resolved = client
            .resolve_draft_conflict(saved.draft_id, keep_remote.then_some(branch_b))
            .unwrap();
        let pending = client.pending_draft_pushes().unwrap().remove(0);
        assert_eq!(resolved.text, "B");
        assert_eq!(
            pending.base_revision_id,
            Some(if keep_remote {
                push_b.revision_id
            } else {
                push_a.revision_id
            })
        );
        assert!(pending.resolves_branch_ids.contains(&if keep_remote {
            branch_a
        } else {
            branch_b
        }));
        assert!(!pending.resolves_branch_ids.contains(&if keep_remote {
            branch_b
        } else {
            branch_a
        }));
        assert_eq!(resolved.attachment_ids, edited.attachment_ids);
        assert_eq!(resolved.route, edited.route);
    }
}

#[test]
fn attachment_inventory_excludes_remote_downloads_and_unavailable_placeholders() {
    let dir = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let sender = unlocked(&config(&dir, "sender", &vault), &vault);
    let receiver = unlocked(&config(&dir, "receiver", &vault), &vault);
    let source = dir.path().join("inventory.png");
    std::fs::write(&source, b"inventory").unwrap();
    let attachment = sender
        .prepare_attachment(&source, "image/png", "inventory.png")
        .unwrap();
    sender
        .mark_attachment_uploaded(attachment.attachment_id, &uuid::Uuid::new_v4().to_string())
        .unwrap();
    assert!(
        sender
            .local_attachment_ids(None, 100)
            .unwrap()
            .contains(&attachment.attachment_id)
    );
    let saved = sender
        .save_new_compose_draft(ComposeDraftUpdate {
            text: "remote image".into(),
            recipients: vec![ADDRESS.into()],
            attachment_ids: vec![attachment.attachment_id],
            route: None,
        })
        .unwrap();
    let push = sender.pending_draft_pushes().unwrap().remove(0);
    receiver
        .apply_draft_changes(vec![remote_head(&push, uuid::Uuid::new_v4())], "1")
        .unwrap();
    assert!(
        !receiver
            .local_attachment_ids(None, 100)
            .unwrap()
            .contains(&attachment.attachment_id)
    );

    let local = sender.recipientless_compose_draft().unwrap();
    let local = sender
        .save_compose_draft(
            local.draft_id,
            local.revision,
            ComposeDraftUpdate {
                text: "offline".into(),
                recipients: vec![ADDRESS.into()],
                attachment_ids: vec![attachment.attachment_id],
                route: None,
            },
        )
        .unwrap();
    std::fs::remove_file(sender.native_cipher_file(attachment.attachment_id).unwrap()).unwrap();
    expire(&sender, local.draft_id, "1");
    let placeholder = sender
        .compose_drafts()
        .unwrap()
        .into_iter()
        .find(|draft| draft.draft_id != saved.draft_id)
        .unwrap()
        .attachment_ids[0];
    assert!(
        !sender
            .local_attachment_ids(None, 100)
            .unwrap()
            .contains(&placeholder)
    );
}

#[test]
fn accepted_r1_after_r2_save_keeps_r2_pending_on_r1_base() {
    let dir = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "client", &vault), &vault);
    let saved = save_draft(&client, "r1", vec![ADDRESS.into()]);
    let r1 = client.pending_draft_pushes().unwrap().remove(0);
    let r2 = client
        .save_compose_draft(
            saved.draft_id,
            saved.revision,
            ComposeDraftUpdate {
                text: "r2".into(),
                recipients: vec![ADDRESS.into()],
                attachment_ids: vec![],
                route: None,
            },
        )
        .unwrap();
    let branch_id = uuid::Uuid::new_v4();
    accept_push(&client, &r1, branch_id);
    let pending = client.pending_draft_pushes().unwrap().remove(0);
    assert_eq!(pending.base_revision_id, Some(r1.revision_id));
    assert_eq!(client.compose_draft(saved.draft_id).unwrap().unwrap(), r2);
    client
        .apply_draft_changes(vec![remote_head(&r1, branch_id)], "1")
        .unwrap();
    assert_eq!(client.pending_draft_pushes().unwrap(), vec![pending]);
    assert!(
        client
            .draft_sync_status(saved.draft_id)
            .unwrap()
            .conflicts
            .is_empty()
    );
}

#[test]
fn selecting_a_conflict_branch_closes_the_original_tracked_branch() {
    let dir = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "client", &vault), &vault);
    let saved = save_draft(&client, "r1", vec![ADDRESS.into()]);
    let push = client.pending_draft_pushes().unwrap().remove(0);
    let tracked = uuid::Uuid::new_v4();
    accept_push(&client, &push, tracked);
    let chosen = uuid::Uuid::new_v4();
    client
        .apply_draft_changes(vec![remote_head(&push, chosen)], "1")
        .unwrap();

    client
        .resolve_draft_conflict(saved.draft_id, Some(chosen))
        .unwrap();
    let outgoing = client.pending_draft_pushes().unwrap().remove(0);
    assert_eq!(outgoing.base_revision_id, Some(push.revision_id));
    assert_eq!(outgoing.resolves_branch_ids, vec![tracked]);
}

#[test]
fn local_send_retires_the_old_remote_identity_before_a_delayed_acceptance() {
    let dir = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let client_config = config(&dir, "client", &vault);
    let client = unlocked(&client_config, &vault);
    let source = dir.path().join("sent-offline.png");
    std::fs::write(&source, b"sent offline").unwrap();
    let attachment = client
        .prepare_attachment(&source, "image/png", "sent-offline.png")
        .unwrap();
    let saved = client
        .save_new_compose_draft(ComposeDraftUpdate {
            text: "sent offline".into(),
            recipients: vec![ADDRESS.into()],
            attachment_ids: vec![attachment.attachment_id],
            route: Some(route(&client_config)),
        })
        .unwrap();
    client
        .mark_attachment_uploaded(attachment.attachment_id, &uuid::Uuid::new_v4().to_string())
        .unwrap();
    let old_push = client
        .pending_draft_pushes()
        .unwrap()
        .into_iter()
        .find(|push| push.draft_id == saved.draft_id)
        .unwrap();

    client.set_draft_sync_supported(false).unwrap();
    client
        .send_compose_draft(saved.draft_id, saved.revision)
        .unwrap();
    client.set_draft_sync_supported(true).unwrap();
    assert!(client.pending_draft_pushes().unwrap().is_empty());
    let cleared = client.compose_draft(saved.draft_id).unwrap().unwrap();
    assert_eq!(cleared.draft_id, saved.draft_id);
    assert_eq!(cleared.revision, saved.revision + 1);

    accept_push(&client, &old_push, uuid::Uuid::new_v4());
    assert_eq!(
        client.pending_draft_discards().unwrap(),
        vec![saved.draft_id]
    );
    assert!(client.pending_draft_pushes().unwrap().is_empty());

    let next = client
        .save_compose_draft(
            cleared.draft_id,
            cleared.revision,
            ComposeDraftUpdate {
                text: "next".into(),
                recipients: cleared.recipients,
                attachment_ids: vec![],
                route: cleared.route,
            },
        )
        .unwrap();
    assert_ne!(next.draft_id, saved.draft_id);
    assert!(matches!(
        client.send_compose_draft(saved.draft_id, cleared.revision),
        Err(Error::StaleDraft { .. })
    ));
    assert_eq!(
        client.pending_draft_pushes().unwrap()[0].draft_id,
        next.draft_id
    );
}

fn expire(client: &Client, draft_id: DraftId, cursor: &str) {
    client
        .apply_draft_changes(
            vec![RemoteDraftChange::Terminal {
                draft_id,
                reason: DraftTerminalReasonCore::Expired,
                command_id: None,
            }],
            cursor,
        )
        .unwrap();
}

#[test]
fn expired_clean_draft_is_removed() {
    let dir = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "client", &vault), &vault);
    let saved = save_draft(&client, "clean", vec![ADDRESS.into()]);
    let push = client.pending_draft_pushes().unwrap().remove(0);
    client
        .record_draft_push(
            saved.draft_id,
            push.revision_id,
            DraftPushOutcome::Accepted {
                branch_id: uuid::Uuid::new_v4(),
                expires_at_ms: 1_000,
                conflict: false,
            },
        )
        .unwrap();

    expire(&client, saved.draft_id, "1");

    assert!(client.compose_draft(saved.draft_id).unwrap().is_none());
}

#[test]
fn expired_dirty_draft_reprepares_images_under_fresh_ids() {
    let dir = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "client", &vault), &vault);
    let source = dir.path().join("draft-image.png");
    std::fs::write(&source, b"draft image bytes").unwrap();
    let old_attachment = client
        .prepare_attachment(&source, "image/png", "draft-image.png")
        .unwrap();
    let draft = client.recipientless_compose_draft().unwrap();
    let saved = client
        .save_compose_draft(
            draft.draft_id,
            draft.revision,
            ComposeDraftUpdate {
                text: "offline edit".into(),
                recipients: vec![ADDRESS.into()],
                attachment_ids: vec![old_attachment.attachment_id],
                route: None,
            },
        )
        .unwrap();
    let old_ciphertext = std::fs::read(
        client
            .native_cipher_file(old_attachment.attachment_id)
            .unwrap(),
    )
    .unwrap();
    let old_plaintext = std::fs::read(
        client
            .open_native_plaintext(old_attachment.attachment_id)
            .unwrap()
            .path(),
    )
    .unwrap();

    expire(&client, saved.draft_id, "1");

    assert!(client.compose_draft(saved.draft_id).unwrap().is_none());
    let restored = client.compose_drafts().unwrap().into_iter().next().unwrap();
    let new_attachment = restored.attachment_ids[0];
    assert_ne!(restored.draft_id, saved.draft_id);
    assert_ne!(new_attachment, old_attachment.attachment_id);
    assert_eq!(restored.text, "offline edit");
    assert_eq!(restored.recipients, vec![ADDRESS]);
    assert_ne!(
        std::fs::read(client.native_cipher_file(new_attachment).unwrap()).unwrap(),
        old_ciphertext
    );
    assert_eq!(
        std::fs::read(client.open_native_plaintext(new_attachment).unwrap().path()).unwrap(),
        old_plaintext
    );
    assert_eq!(
        client.pending_draft_uploads().unwrap()[0].draft_id,
        restored.draft_id
    );
    assert_eq!(
        client.pending_draft_uploads().unwrap()[0]
            .object
            .attachment_id,
        new_attachment
    );
}

#[test]
fn expired_draft_with_missing_ciphertext_keeps_unavailable_placeholder() {
    let dir = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "client", &vault), &vault);
    let source = dir.path().join("missing-image.png");
    std::fs::write(&source, b"missing image").unwrap();
    let attachment = client
        .prepare_attachment(&source, "image/png", "missing-image.png")
        .unwrap();
    let draft = client.recipientless_compose_draft().unwrap();
    let saved = client
        .save_compose_draft(
            draft.draft_id,
            draft.revision,
            ComposeDraftUpdate {
                text: "offline edit".into(),
                recipients: vec![ADDRESS.into()],
                attachment_ids: vec![attachment.attachment_id],
                route: None,
            },
        )
        .unwrap();
    std::fs::remove_file(client.native_cipher_file(attachment.attachment_id).unwrap()).unwrap();

    expire(&client, saved.draft_id, "1");

    let restored = client.compose_drafts().unwrap().into_iter().next().unwrap();
    let placeholder = restored.attachment_ids[0];
    assert_ne!(placeholder, attachment.attachment_id);
    assert_eq!(
        client.attachment_info(placeholder).unwrap().display_name,
        "missing-image.png"
    );
    assert_eq!(
        client.draft_attachment_states(restored.draft_id).unwrap()[0].1,
        DraftAttachmentState::Unavailable
    );
    assert!(client.pending_draft_uploads().unwrap().is_empty());
}

#[test]
fn repeated_expired_terminal_and_push_termination_restore_once() {
    let dir = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "client", &vault), &vault);
    let saved = save_draft(&client, "offline", vec![ADDRESS.into()]);

    expire(&client, saved.draft_id, "1");
    let restored = client.compose_drafts().unwrap().into_iter().next().unwrap();
    expire(&client, saved.draft_id, "2");
    client
        .record_draft_push(
            saved.draft_id,
            uuid::Uuid::new_v4(),
            DraftPushOutcome::Terminated(DraftTerminalReasonCore::Expired),
        )
        .unwrap();

    assert_eq!(client.compose_drafts().unwrap(), vec![restored]);
}

#[test]
fn bootstrap_remains_required_until_successful_finish_across_reopen() {
    let dir = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let client_config = config(&dir, "client", &vault);
    let client = unlocked(&client_config, &vault);
    assert!(client.draft_bootstrap_required().unwrap());
    client.apply_draft_changes(vec![], "1").unwrap();
    assert!(client.draft_bootstrap_required().unwrap());
    drop(client);

    let reopened = unlocked(&client_config, &vault);
    assert!(reopened.draft_bootstrap_required().unwrap());
    reopened.finish_draft_bootstrap(&[]).unwrap();
    assert!(!reopened.draft_bootstrap_required().unwrap());
    reopened.set_draft_sync_supported(false).unwrap();
    assert!(!reopened.draft_bootstrap_required().unwrap());
}

#[test]
fn abort_draft_send_intent_requires_the_matching_unqueued_command() {
    let dir = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "client", &vault), &vault);
    let saved = save_draft(&client, "ready", vec![ADDRESS.into()]);
    let push = client.pending_draft_pushes().unwrap().remove(0);
    client
        .record_draft_push(
            saved.draft_id,
            push.revision_id,
            DraftPushOutcome::Accepted {
                branch_id: uuid::Uuid::new_v4(),
                expires_at_ms: 1_000,
                conflict: false,
            },
        )
        .unwrap();
    let command = client
        .begin_draft_send(saved.draft_id, saved.revision)
        .unwrap()
        .remote
        .unwrap()
        .command_id;

    client
        .abort_draft_send_intent(saved.draft_id, CommandId::new())
        .unwrap();
    assert_eq!(
        client
            .begin_draft_send(saved.draft_id, saved.revision)
            .unwrap()
            .remote
            .unwrap()
            .command_id,
        command
    );
    client
        .abort_draft_send_intent(saved.draft_id, command)
        .unwrap();
    assert_ne!(
        client
            .begin_draft_send(saved.draft_id, saved.revision)
            .unwrap()
            .remote
            .unwrap()
            .command_id,
        command
    );
}

#[test]
fn distinct_live_branch_is_retained_after_clean_tracked_branch() {
    let dir = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "client", &vault), &vault);
    let saved = save_draft(&client, "r1", vec![ADDRESS.into()]);
    let push = client.pending_draft_pushes().unwrap().remove(0);
    let tracked_branch = uuid::Uuid::new_v4();
    accept_push(&client, &push, tracked_branch);

    let conflicting_branch = uuid::Uuid::new_v4();
    client
        .apply_draft_changes(vec![remote_head(&push, conflicting_branch)], "1")
        .unwrap();

    let status = client.draft_sync_status(saved.draft_id).unwrap();
    assert_eq!(status.conflicts.len(), 1);
    assert_eq!(status.conflicts[0].branch_id, conflicting_branch);
    assert_eq!(
        client.compose_draft(saved.draft_id).unwrap().unwrap().text,
        "r1"
    );
}

#[test]
fn duplicate_remote_descriptor_is_reused_but_a_mismatched_id_rejects_the_page() {
    let dir = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let sender = unlocked(&config(&dir, "sender", &vault), &vault);
    let receiver = unlocked(&config(&dir, "receiver", &vault), &vault);
    let source = dir.path().join("image.png");
    std::fs::write(&source, b"image").unwrap();
    let attachment = sender
        .prepare_attachment(&source, "image/png", "image.png")
        .unwrap();
    sender
        .mark_attachment_uploaded(attachment.attachment_id, &uuid::Uuid::new_v4().to_string())
        .unwrap();
    let saved = sender
        .save_new_compose_draft(ComposeDraftUpdate {
            text: "image".into(),
            recipients: vec![ADDRESS.into()],
            attachment_ids: vec![attachment.attachment_id],
            route: None,
        })
        .unwrap();
    let push = sender.pending_draft_pushes().unwrap().remove(0);
    let branch_id = uuid::Uuid::new_v4();
    receiver
        .apply_draft_changes(vec![remote_head(&push, branch_id)], "1")
        .unwrap();
    receiver
        .apply_draft_changes(vec![remote_head(&push, branch_id)], "2")
        .unwrap();

    let mut mismatched = remote_head(&push, uuid::Uuid::new_v4());
    let RemoteDraftChange::Head { attachment_ids, .. } = &mut mismatched else {
        unreachable!()
    };
    attachment_ids[0] = AttachmentId::new();
    assert!(receiver.apply_draft_changes(vec![mismatched], "3").is_err());
    assert_eq!(receiver.draft_sync_cursor().unwrap(), "2");
    assert_eq!(
        receiver
            .compose_draft(saved.draft_id)
            .unwrap()
            .unwrap()
            .attachment_ids,
        vec![attachment.attachment_id]
    );
}

#[test]
fn save_new_and_recipient_move_keep_draft_sync_state() {
    let dir = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "client", &vault), &vault);
    let saved = client
        .save_new_compose_draft(ComposeDraftUpdate {
            text: "new".into(),
            recipients: vec![ADDRESS.into()],
            attachment_ids: vec![],
            route: None,
        })
        .unwrap();
    let push = client.pending_draft_pushes().unwrap().remove(0);
    accept_push(&client, &push, uuid::Uuid::new_v4());
    client
        .save_new_compose_draft(ComposeDraftUpdate {
            text: "destination".into(),
            recipients: vec!["+15555550101".into()],
            attachment_ids: vec![],
            route: None,
        })
        .unwrap();
    let moved = client
        .save_compose_draft(
            saved.draft_id,
            saved.revision,
            ComposeDraftUpdate {
                text: "moved".into(),
                recipients: vec!["+15555550101".into()],
                attachment_ids: vec![],
                route: None,
            },
        )
        .unwrap();

    assert_ne!(moved.draft_id, saved.draft_id);
    assert_eq!(
        client.pending_draft_discards().unwrap(),
        vec![saved.draft_id]
    );
    assert_eq!(
        client.pending_draft_pushes().unwrap()[0].draft_id,
        moved.draft_id
    );
}

#[test]
fn matching_sent_terminal_recovers_one_snapshot_command_after_empty_apply() {
    let dir = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let client_config = config(&dir, "client", &vault);
    let client = unlocked(&client_config, &vault);
    let saved = client
        .save_new_compose_draft(ComposeDraftUpdate {
            text: "send snapshot".into(),
            recipients: vec![ADDRESS.into()],
            attachment_ids: vec![],
            route: Some(route(&client_config)),
        })
        .unwrap();
    let push = client.pending_draft_pushes().unwrap().remove(0);
    accept_push(&client, &push, uuid::Uuid::new_v4());
    let remote = client
        .begin_draft_send(saved.draft_id, saved.revision)
        .unwrap()
        .remote
        .unwrap();
    client
        .apply_draft_changes(
            vec![RemoteDraftChange::Terminal {
                draft_id: saved.draft_id,
                reason: DraftTerminalReasonCore::Sent,
                command_id: Some(remote.command_id),
            }],
            "1",
        )
        .unwrap();
    let first = client.pending_outbox().unwrap();
    client.apply_draft_changes(vec![], "2").unwrap();
    assert_eq!(client.pending_outbox().unwrap(), first);
    assert_eq!(first.len(), 1);
}

#[test]
fn send_intent_keeps_r1_snapshot_after_r2_edit_and_rejects_stale_cas() {
    let dir = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let client_config = config(&dir, "client", &vault);
    let client = unlocked(&client_config, &vault);
    let saved = client
        .save_new_compose_draft(ComposeDraftUpdate {
            text: "r1".into(),
            recipients: vec![ADDRESS.into()],
            attachment_ids: vec![],
            route: Some(route(&client_config)),
        })
        .unwrap();
    let push = client.pending_draft_pushes().unwrap().remove(0);
    accept_push(&client, &push, uuid::Uuid::new_v4());
    assert!(matches!(
        client.begin_draft_send(saved.draft_id, saved.revision + 1),
        Err(Error::StaleDraft { .. })
    ));
    let remote = client
        .begin_draft_send(saved.draft_id, saved.revision)
        .unwrap()
        .remote
        .unwrap();
    let edited = client
        .save_compose_draft(
            saved.draft_id,
            saved.revision,
            ComposeDraftUpdate {
                text: "r2".into(),
                recipients: vec![ADDRESS.into()],
                attachment_ids: vec![],
                route: Some(route(&client_config)),
            },
        )
        .unwrap();
    assert_eq!(
        client
            .begin_draft_send(saved.draft_id, edited.revision)
            .unwrap()
            .remote
            .unwrap(),
        remote
    );
    client
        .send_compose_draft_accepted(saved.draft_id, saved.revision, remote.command_id)
        .unwrap();
    assert_eq!(
        only_message(&client, saved.conversation_id).payload.body,
        "r1"
    );
    assert_eq!(
        client.compose_draft(saved.draft_id).unwrap().unwrap().text,
        "r2"
    );
}

#[test]
fn nonmatching_sent_terminal_keeps_dirty_draft_without_queueing() {
    let dir = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "client", &vault), &vault);
    let saved = save_draft(&client, "r1", vec![ADDRESS.into()]);
    let push = client.pending_draft_pushes().unwrap().remove(0);
    accept_push(&client, &push, uuid::Uuid::new_v4());
    client
        .save_compose_draft(
            saved.draft_id,
            saved.revision,
            ComposeDraftUpdate {
                text: "r2".into(),
                recipients: vec![ADDRESS.into()],
                attachment_ids: vec![],
                route: None,
            },
        )
        .unwrap();
    client
        .apply_draft_changes(
            vec![RemoteDraftChange::Terminal {
                draft_id: saved.draft_id,
                reason: DraftTerminalReasonCore::Sent,
                command_id: Some(CommandId::new()),
            }],
            "1",
        )
        .unwrap();
    let kept = client.compose_drafts().unwrap();
    assert_eq!(kept.len(), 1);
    assert_ne!(kept[0].draft_id, saved.draft_id);
    assert_eq!(kept[0].conversation_id, saved.conversation_id);
    assert_eq!(kept[0].text, "r2");
    assert!(client.pending_outbox().unwrap().is_empty());
}

#[test]
fn unsupported_sync_uses_legacy_only_for_never_synced_drafts() {
    let dir = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "client", &vault), &vault);
    client.set_draft_sync_supported(false).unwrap();
    let local = save_draft(&client, "local", vec![ADDRESS.into()]);
    assert!(
        client
            .begin_draft_send(local.draft_id, local.revision)
            .unwrap()
            .remote
            .is_none()
    );
    client.set_draft_sync_supported(true).unwrap();
    let synced = save_draft(&client, "synced", vec!["+15555550101".into()]);
    let push = client
        .pending_draft_pushes()
        .unwrap()
        .into_iter()
        .find(|push| push.draft_id == synced.draft_id)
        .unwrap();
    accept_push(&client, &push, uuid::Uuid::new_v4());
    client.set_draft_sync_supported(false).unwrap();
    assert!(
        client
            .begin_draft_send(synced.draft_id, synced.revision)
            .unwrap()
            .remote
            .is_some()
    );
}

#[test]
fn intent_reference_prevents_attachment_cleanup_until_abort_and_discard() {
    let dir = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let client_config = config(&dir, "client", &vault);
    let client = unlocked(&client_config, &vault);
    let source = dir.path().join("held.png");
    std::fs::write(&source, b"held").unwrap();
    let attachment = client
        .prepare_attachment(&source, "image/png", "held.png")
        .unwrap();
    client
        .mark_attachment_uploaded(attachment.attachment_id, &uuid::Uuid::new_v4().to_string())
        .unwrap();
    let saved = client
        .save_new_compose_draft(ComposeDraftUpdate {
            text: "held".into(),
            recipients: vec![],
            attachment_ids: vec![attachment.attachment_id],
            route: Some(route(&client_config)),
        })
        .unwrap();
    let push = client.pending_draft_pushes().unwrap().remove(0);
    accept_push(&client, &push, uuid::Uuid::new_v4());
    let command = client
        .begin_draft_send(saved.draft_id, saved.revision)
        .unwrap()
        .remote
        .unwrap()
        .command_id;
    assert!(
        !client
            .discard_unreferenced_attachment(attachment.attachment_id)
            .unwrap()
    );
    client
        .abort_draft_send_intent(saved.draft_id, command)
        .unwrap();
    let emptied = client
        .save_compose_draft(
            saved.draft_id,
            saved.revision,
            ComposeDraftUpdate::default(),
        )
        .unwrap();
    client
        .discard_compose_draft(saved.draft_id, emptied.revision)
        .unwrap();
    assert!(
        client
            .discard_unreferenced_attachment(attachment.attachment_id)
            .unwrap()
    );
}
