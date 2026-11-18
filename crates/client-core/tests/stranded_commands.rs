//! L11 core: a gateway retires the commands a snapshot swallowed, and a user can send a message
//! again as a new, independent command. Each device keeps its own device-local record of the copy.
use crate::common::*;
use std::fs;
use tempfile::TempDir;

fn all_records(server: &Server) -> Vec<SnapshotRecord> {
    server
        .log
        .iter()
        .enumerate()
        .map(|(index, envelope)| SnapshotRecord {
            cursor: Cursor(index as u64 + 1),
            envelope: envelope.clone(),
        })
        .collect()
}

fn import(client: &Client, records: &[SnapshotRecord], high_water: u64) {
    let session = client
        .begin_snapshot(
            Cursor(high_water),
            records.len() as u64,
            SnapshotPurpose::Resync,
        )
        .unwrap();
    for page in records.chunks(2) {
        client
            .append_snapshot_page(session.generation, page)
            .unwrap();
    }
    client.finish_snapshot(session.generation).unwrap();
}

/// Imports the whole server log as history and drains it, so every command is a ledger row.
fn import_history(gateway: &Client, server: &Server) {
    import(gateway, &all_records(server), server.log.len() as u64);
    gateway.apply_pending(usize::MAX).unwrap();
}

fn message(client: &Client, message_id: MessageId) -> Message {
    client.message_by_id(message_id).unwrap().unwrap()
}

/// A sender with one queued SMS to `gateway_cfg`; returns the conversation and the original id.
fn queued_sms(
    desktop: &Client,
    gateway_cfg: &ClientConfig,
    body: &str,
) -> (ConversationId, MessageId) {
    let conversation = ConversationId::new();
    let queued = desktop
        .queue_send(sms(conversation, body), route(gateway_cfg))
        .unwrap();
    (conversation, queued.message_id)
}

fn command_ids(gateway: &Client) -> Vec<CommandId> {
    gateway
        .pending_commands()
        .unwrap()
        .into_iter()
        .map(|command| command.command_id)
        .collect()
}

#[test]
fn swallowed_command_is_retired_once_and_peers_see_the_status() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let (desktop_cfg, gateway_cfg) = (
        config(&dir, "desktop", &vault),
        config(&dir, "gateway", &vault),
    );
    let desktop = unlocked(&desktop_cfg, &vault);
    let gateway = unlocked(&gateway_cfg, &vault);
    let conversation = ConversationId::new();
    desktop
        .queue_send(sms(conversation, "swallowed"), route(&gateway_cfg))
        .unwrap();
    let mut server = Server::default();
    server.upload(&desktop);
    import_history(&gateway, &server);
    assert!(gateway.pending_outbox().unwrap().is_empty());

    assert_eq!(gateway.retire_swallowed_commands().unwrap(), 1);
    assert_eq!(gateway.retire_swallowed_commands().unwrap(), 0);
    assert_eq!(
        gateway.pending_outbox().unwrap().len(),
        1,
        "one sealed status event"
    );

    drop(gateway);
    let gateway = unlocked(&gateway_cfg, &vault);
    assert_eq!(
        gateway.retire_swallowed_commands().unwrap(),
        0,
        "a reopened gateway retires nothing new"
    );
    assert_eq!(gateway.pending_outbox().unwrap().len(), 1);

    server.upload(&gateway);
    server.sync(&desktop);
    let shown = only_message(&desktop, conversation);
    assert_eq!(shown.send_state, Some(SendState::FailedBeforeSubmission));
    assert_eq!(shown.send_refusal, None, "the retirement carries no reason");
}

#[test]
fn live_commands_are_never_retired() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let (desktop_cfg, gateway_cfg) = (
        config(&dir, "desktop", &vault),
        config(&dir, "gateway", &vault),
    );
    let desktop = unlocked(&desktop_cfg, &vault);
    let gateway = unlocked(&gateway_cfg, &vault);
    let conversation = ConversationId::new();
    desktop
        .queue_send(sms(conversation, "live"), route(&gateway_cfg))
        .unwrap();
    let mut server = Server::default();
    server.upload(&desktop);
    server.sync(&gateway);

    assert_eq!(gateway.retire_swallowed_commands().unwrap(), 0);
    server.upload(&gateway);
    server.sync(&desktop);
    assert_eq!(
        only_message(&desktop, conversation).send_state,
        Some(SendState::PersistedGateway)
    );
}

#[test]
fn a_command_for_another_gateway_is_retired_only_by_that_gateway() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let (desktop_cfg, first_cfg, second_cfg) = (
        config(&dir, "desktop", &vault),
        config(&dir, "first", &vault),
        config(&dir, "second", &vault),
    );
    let desktop = unlocked(&desktop_cfg, &vault);
    let first = unlocked(&first_cfg, &vault);
    let second = unlocked(&second_cfg, &vault);
    desktop
        .queue_send(sms(ConversationId::new(), "for second"), route(&second_cfg))
        .unwrap();
    let mut server = Server::default();
    server.upload(&desktop);
    import_history(&first, &server);
    import_history(&second, &server);

    assert_eq!(first.retire_swallowed_commands().unwrap(), 0);
    assert_eq!(second.retire_swallowed_commands().unwrap(), 1);
}

#[test]
fn a_gateway_that_already_reported_the_command_does_not_retire_it() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let (desktop_cfg, gateway_cfg) = (
        config(&dir, "desktop", &vault),
        config(&dir, "gateway", &vault),
    );
    let desktop = unlocked(&desktop_cfg, &vault);
    let gateway = unlocked(&gateway_cfg, &vault);
    desktop
        .queue_send(sms(ConversationId::new(), "reported"), route(&gateway_cfg))
        .unwrap();
    let mut server = Server::default();
    server.upload(&desktop);
    server.sync(&gateway);
    server.upload(&gateway);

    // The same device identity restored into an empty database: its own history and its own
    // receipt arrive in the snapshot.
    let restored_cfg = ClientConfig {
        database_path: dir.path().join("restored.db"),
        vault_id: vault.id,
        device_id: gateway_cfg.device_id,
    };
    let restored = unlocked(&restored_cfg, &vault);
    import_history(&restored, &server);
    assert_eq!(restored.retire_swallowed_commands().unwrap(), 0);
}

#[test]
fn retirement_waits_for_a_staging_or_undrained_snapshot_generation() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let (desktop_cfg, gateway_cfg) = (
        config(&dir, "desktop", &vault),
        config(&dir, "gateway", &vault),
    );
    let desktop = unlocked(&desktop_cfg, &vault);
    let gateway = unlocked(&gateway_cfg, &vault);
    let conversation = ConversationId::new();
    desktop
        .queue_send(sms(conversation, "first"), route(&gateway_cfg))
        .unwrap();
    let mut server = Server::default();
    server.upload(&desktop);
    import_history(&gateway, &server);
    desktop
        .queue_send(sms(conversation, "second"), route(&gateway_cfg))
        .unwrap();
    server.upload(&desktop);

    let records = all_records(&server);
    let session = gateway
        .begin_snapshot(Cursor(2), 1, SnapshotPurpose::Resync)
        .unwrap();
    gateway
        .append_snapshot_page(session.generation, &records[1..])
        .unwrap();
    assert_eq!(
        gateway.retire_swallowed_commands().unwrap(),
        0,
        "a staging generation blocks retirement"
    );
    gateway.finish_snapshot(session.generation).unwrap();
    assert_eq!(
        gateway.retire_swallowed_commands().unwrap(),
        0,
        "a published, undrained generation blocks retirement"
    );
    gateway.apply_pending(usize::MAX).unwrap();
    assert_eq!(gateway.retire_swallowed_commands().unwrap(), 2);
}

#[test]
fn a_restore_guarded_gateway_retires_nothing() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let (desktop_cfg, gateway_cfg) = (
        config(&dir, "desktop", &vault),
        config(&dir, "gateway", &vault),
    );
    let desktop = unlocked(&desktop_cfg, &vault);
    let gateway = unlocked(&gateway_cfg, &vault);
    desktop
        .queue_send(sms(ConversationId::new(), "guarded"), route(&gateway_cfg))
        .unwrap();
    let mut server = Server::default();
    server.upload(&desktop);
    import_history(&gateway, &server);
    gateway.mark_restored_gateway().unwrap();

    assert_eq!(gateway.retire_swallowed_commands().unwrap(), 0);
    drop(gateway);
    let gateway = unlocked(&gateway_cfg, &vault);
    assert_eq!(gateway.retire_swallowed_commands().unwrap(), 0);
}

#[test]
fn retirement_is_bounded_per_call_and_retires_every_swallowed_command() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let (desktop_cfg, gateway_cfg) = (
        config(&dir, "desktop", &vault),
        config(&dir, "gateway", &vault),
    );
    let desktop = unlocked(&desktop_cfg, &vault);
    let gateway = unlocked(&gateway_cfg, &vault);
    let conversation = ConversationId::new();
    for number in 0..450 {
        desktop
            .queue_send(
                sms(conversation, &format!("swallowed {number}")),
                route(&gateway_cfg),
            )
            .unwrap();
    }
    let mut server = Server::default();
    server.upload(&desktop);
    import_history(&gateway, &server);

    assert_eq!(gateway.retire_swallowed_commands().unwrap(), 200);
    assert_eq!(gateway.retire_swallowed_commands().unwrap(), 200);
    assert_eq!(gateway.retire_swallowed_commands().unwrap(), 50);
    assert_eq!(gateway.retire_swallowed_commands().unwrap(), 0);

    server.upload(&gateway);
    server.sync(&desktop);
    // `sync` applies at most one bounded batch; the peer drains the rest.
    desktop.apply_pending(usize::MAX).unwrap();
    let messages = desktop.messages(conversation).unwrap();
    assert_eq!(messages.len(), 450);
    assert!(
        messages
            .iter()
            .all(|message| message.send_state == Some(SendState::FailedBeforeSubmission)),
        "every swallowed command shows the same authoritative status"
    );
}

#[test]
fn a_later_published_snapshot_is_retired_from_the_saved_position() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let (desktop_cfg, gateway_cfg) = (
        config(&dir, "desktop", &vault),
        config(&dir, "gateway", &vault),
    );
    let desktop = unlocked(&desktop_cfg, &vault);
    let gateway = unlocked(&gateway_cfg, &vault);
    let conversation = ConversationId::new();
    desktop
        .queue_send(sms(conversation, "first"), route(&gateway_cfg))
        .unwrap();
    let mut server = Server::default();
    server.upload(&desktop);
    import_history(&gateway, &server);
    assert_eq!(gateway.retire_swallowed_commands().unwrap(), 1);
    assert_eq!(gateway.retire_swallowed_commands().unwrap(), 0);

    desktop
        .queue_send(sms(conversation, "second"), route(&gateway_cfg))
        .unwrap();
    server.upload(&desktop);
    let records = all_records(&server);
    let session = gateway
        .begin_snapshot(Cursor(2), 1, SnapshotPurpose::Resync)
        .unwrap();
    gateway
        .append_snapshot_page(session.generation, &records[1..])
        .unwrap();
    gateway.finish_snapshot(session.generation).unwrap();
    gateway.apply_pending(usize::MAX).unwrap();

    assert_eq!(
        gateway.retire_swallowed_commands().unwrap(),
        1,
        "a command a later snapshot brought is found after the earlier pass"
    );
}

#[test]
fn a_retired_swallowed_command_redelivered_by_live_replay_stays_inert() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let (desktop_cfg, gateway_cfg) = (
        config(&dir, "desktop", &vault),
        config(&dir, "gateway", &vault),
    );
    let desktop = unlocked(&desktop_cfg, &vault);
    let gateway = unlocked(&gateway_cfg, &vault);
    let (_, original) = queued_sms(&desktop, &gateway_cfg, "redelivered");
    let mut server = Server::default();
    server.upload(&desktop);
    import_history(&gateway, &server);
    assert_eq!(gateway.retire_swallowed_commands().unwrap(), 1);
    let envelope = server.log[0].clone();
    let command = envelope.command_id.unwrap();
    assert!(message(&desktop, original).send_state.is_some());

    let redelivery = gateway.ingest(&envelope, Cursor(2)).unwrap();

    assert_eq!(redelivery, IngestResult::Duplicate);
    gateway.apply_pending(usize::MAX).unwrap();
    assert!(command_ids(&gateway).is_empty(), "never runnable again");
    assert!(matches!(
        gateway.begin_send_attempt(command).unwrap(),
        PermitDecision::Blocked(PermitBlock::Historical)
    ));
    assert_eq!(gateway.retire_swallowed_commands().unwrap(), 0);
    assert_eq!(
        gateway.pending_outbox().unwrap().len(),
        1,
        "the redelivery adds no second status"
    );
}

#[test]
fn send_again_creates_a_new_command_and_leaves_the_original_untouched() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let gateway_cfg = config(&dir, "gateway", &vault);
    let desktop = unlocked(&config(&dir, "sender", &vault), &vault);
    let (conversation, original_id) = queued_sms(&desktop, &gateway_cfg, "again");
    let original_command = desktop
        .pending_outbox()
        .unwrap()
        .into_iter()
        .find(|envelope| envelope.command_id.is_some())
        .unwrap()
        .command_id
        .unwrap();
    let before = message(&desktop, original_id);

    let copy = desktop
        .send_again(original_id, route(&gateway_cfg))
        .unwrap();

    assert_ne!(copy.message_id, original_id);
    assert_ne!(copy.command_id, original_command);
    let shown = message(&desktop, copy.message_id);
    assert_eq!(shown.payload.body, "again");
    assert_eq!(shown.payload.recipients, vec![ADDRESS.to_string()]);
    assert_eq!(shown.payload.record.conversation_id, conversation);
    assert_eq!(shown.send_state, Some(SendState::QueuedLocal));
    assert_eq!(shown.target_gateway_id, Some(gateway_cfg.device_id));
    assert!(
        desktop
            .pending_outbox()
            .unwrap()
            .iter()
            .any(|envelope| envelope.command_id == Some(copy.command_id)),
        "the new command is sealed in the outbox"
    );
    let after = message(&desktop, original_id);
    assert_eq!(
        (
            after.payload,
            after.send_state,
            after.status_at_ms,
            after.send_refusal,
            after.seen
        ),
        (
            before.payload,
            before.send_state,
            before.status_at_ms,
            before.send_refusal,
            before.seen
        ),
        "the original message, state and status are unchanged"
    );
    assert_eq!(before.sent_again_message_id, None);
    assert_eq!(after.sent_again_message_id, Some(copy.message_id));
}

#[test]
fn a_second_device_sees_the_copy_as_an_ordinary_outgoing_message() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let gateway_cfg = config(&dir, "gateway", &vault);
    let desktop = unlocked(&config(&dir, "sender", &vault), &vault);
    let (_, original_id) = queued_sms(&desktop, &gateway_cfg, "peer");
    let copy = desktop
        .send_again(original_id, route(&gateway_cfg))
        .unwrap();
    let mut server = Server::default();
    server.upload(&desktop);

    let peer = unlocked(&config(&dir, "peer", &vault), &vault);
    server.sync(&peer);
    let peer_copy = message(&peer, copy.message_id);
    assert_eq!(peer_copy.payload.direction, Direction::Outgoing);
    assert_eq!(peer_copy.payload.body, "peer");
    assert_eq!(
        message(&peer, original_id).sent_again_message_id,
        None,
        "the copy link is device-local, so the peer does not see it"
    );
}

#[test]
fn send_again_is_allowed_after_the_gateway_retires_a_swallowed_command() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let gateway_cfg = config(&dir, "gateway", &vault);
    let desktop = unlocked(&config(&dir, "desktop", &vault), &vault);
    let gateway = unlocked(&gateway_cfg, &vault);
    let conversation = ConversationId::new();
    let queued = desktop
        .queue_send(sms(conversation, "not sent"), route(&gateway_cfg))
        .unwrap();
    let mut server = Server::default();
    server.upload(&desktop);
    import_history(&gateway, &server);
    assert_eq!(gateway.retire_swallowed_commands().unwrap(), 1);
    server.upload(&gateway);
    server.sync(&desktop);
    assert_eq!(
        message(&desktop, queued.message_id).send_state,
        Some(SendState::FailedBeforeSubmission)
    );

    let copy = desktop
        .send_again(queued.message_id, route(&gateway_cfg))
        .unwrap();
    assert_eq!(message(&desktop, copy.message_id).payload.body, "not sent");
}

#[test]
fn send_again_refuses_a_missing_or_incoming_original() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let gateway_cfg = config(&dir, "gateway", &vault);
    let desktop = unlocked(&config(&dir, "desktop", &vault), &vault);
    assert_eq!(
        desktop.send_again(MessageId::new(), route(&gateway_cfg)),
        Err(Error::NotFound)
    );
    let incoming_message = desktop
        .capture_incoming(incoming("received", "received-1"))
        .unwrap()
        .message_id;
    assert_eq!(
        desktop.send_again(incoming_message, route(&gateway_cfg)),
        Err(Error::SendAgainNotAllowed)
    );
}

#[test]
fn send_again_refuses_an_sms_shaped_original_that_was_sent_on_an_rcs_link() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let gateway_cfg = config(&dir, "gateway", &vault);
    let desktop = unlocked(&config(&dir, "sender", &vault), &vault);
    let conversation = ConversationId::new();
    let rcs = desktop
        .queue_send(
            sms(conversation, "over rcs"),
            GatewayRoute::RcsLink {
                gateway_device_id: gateway_cfg.device_id,
            },
        )
        .unwrap();
    assert_eq!(
        message(&desktop, rcs.message_id).payload.transport,
        Transport::Sms,
        "the stored shape is SMS, so only the route identifies it"
    );
    assert_eq!(
        desktop.send_again(rcs.message_id, route(&gateway_cfg)),
        Err(Error::SendAgainNotAllowed)
    );
}

#[test]
fn send_again_refuses_a_command_already_submitted_by_its_gateway() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let (desktop_cfg, gateway_cfg) = (
        config(&dir, "desktop", &vault),
        config(&dir, "gateway", &vault),
    );
    let desktop = unlocked(&desktop_cfg, &vault);
    let gateway = unlocked(&gateway_cfg, &vault);
    let queued = desktop
        .queue_send(sms(ConversationId::new(), "submitted"), route(&gateway_cfg))
        .unwrap();
    let mut server = Server::default();
    server.upload(&desktop);
    server.sync(&gateway);
    assert!(matches!(
        gateway.begin_send_attempt(queued.command_id).unwrap(),
        PermitDecision::Permit(_)
    ));
    gateway
        .record_send_result(queued.command_id, SendResult::Submitted)
        .unwrap();
    server.upload(&gateway);
    server.sync(&desktop);

    assert_eq!(
        desktop.send_again(queued.message_id, route(&gateway_cfg)),
        Err(Error::SendAgainNotAllowed)
    );
}

#[test]
fn send_again_twice_is_refused_even_after_reopen() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let gateway_cfg = config(&dir, "gateway", &vault);
    let sender_cfg = config(&dir, "sender", &vault);
    let desktop = unlocked(&sender_cfg, &vault);
    let (_, original_id) = queued_sms(&desktop, &gateway_cfg, "twice");
    let copy = desktop
        .send_again(original_id, route(&gateway_cfg))
        .unwrap();
    assert_eq!(
        desktop.send_again(original_id, route(&gateway_cfg)),
        Err(Error::AlreadySentAgain)
    );
    drop(desktop);
    let desktop = unlocked(&sender_cfg, &vault);
    assert_eq!(
        desktop.send_again(original_id, route(&gateway_cfg)),
        Err(Error::AlreadySentAgain)
    );
    assert_eq!(
        message(&desktop, copy.message_id).payload.body,
        "twice",
        "the first copy is still there"
    );
}

#[test]
fn send_again_of_an_mms_reuses_its_attachment_on_the_sender() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let gateway_cfg = config(&dir, "gateway", &vault);
    let desktop = unlocked(&config(&dir, "sender", &vault), &vault);
    let source = dir.path().join("photo.bin");
    fs::write(&source, b"ciphertext source").unwrap();
    let attachment = desktop
        .prepare_attachment(&source, "image/jpeg", "photo.bin")
        .unwrap();
    let original = desktop
        .queue_mms(
            OutgoingMms {
                conversation_id: ConversationId::new(),
                recipients: vec![ADDRESS.into()],
                body: "photo".into(),
                attachment_ids: vec![attachment.attachment_id],
                subject: None,
            },
            route(&gateway_cfg),
        )
        .unwrap();

    let copy = desktop
        .send_again(original.message_id, route(&gateway_cfg))
        .unwrap();

    let shown = message(&desktop, copy.message_id);
    assert_eq!(shown.payload.body, "photo");
    assert_eq!(shown.payload.record.attachments.len(), 1);
    assert_eq!(
        shown.payload.record.attachments[0].attachment_id,
        attachment.attachment_id
    );
    assert_eq!(shown.send_state, Some(SendState::QueuedLocal));
}

#[test]
fn send_again_of_an_mms_refuses_attachments_this_device_does_not_hold() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let gateway_cfg = config(&dir, "gateway", &vault);
    let sender = unlocked(&config(&dir, "sender", &vault), &vault);
    let source = dir.path().join("held.bin");
    fs::write(&source, b"ciphertext source").unwrap();
    let attachment = sender
        .prepare_attachment(&source, "image/jpeg", "held.bin")
        .unwrap();
    let original = sender
        .queue_mms(
            OutgoingMms {
                conversation_id: ConversationId::new(),
                recipients: vec![ADDRESS.into()],
                body: "held".into(),
                attachment_ids: vec![attachment.attachment_id],
                subject: None,
            },
            route(&gateway_cfg),
        )
        .unwrap();
    sender
        .mark_attachment_uploaded(attachment.attachment_id, &uuid::Uuid::new_v4().to_string())
        .unwrap();
    let mut server = Server::default();
    server.upload(&sender);

    let peer = unlocked(&config(&dir, "peer", &vault), &vault);
    server.sync(&peer);
    let conversation = peer.list_conversations().unwrap()[0].conversation_id;
    let before = peer.messages(conversation).unwrap().len();
    assert_eq!(
        peer.send_again(original.message_id, route(&gateway_cfg)),
        Err(Error::SendAgainAttachmentsUnavailable)
    );
    assert_eq!(
        peer.messages(conversation).unwrap().len(),
        before,
        "nothing is created when an attachment is unavailable"
    );
}

#[test]
fn send_again_after_the_original_attachment_was_released_is_unavailable() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let (sender_cfg, gateway_cfg) = (
        config(&dir, "sender", &vault),
        config(&dir, "gateway", &vault),
    );
    let mut sender = unlocked(&sender_cfg, &vault);
    let gateway = unlocked(&gateway_cfg, &vault);
    let source = dir.path().join("released.bin");
    fs::write(&source, b"ciphertext source").unwrap();
    let attachment = sender
        .prepare_attachment(&source, "image/jpeg", "released.bin")
        .unwrap();
    // The use is registered before the command is queued, as in the retention suite.
    let registration = sender
        .begin_attachment_retention_use(attachment.attachment_id, AttachmentUseKind::PendingSend)
        .unwrap();
    let original = sender
        .queue_mms(
            OutgoingMms {
                conversation_id: ConversationId::new(),
                recipients: vec![ADDRESS.into()],
                body: "released".into(),
                attachment_ids: vec![attachment.attachment_id],
                subject: None,
            },
            route(&gateway_cfg),
        )
        .unwrap();
    sender
        .mark_attachment_uploaded(attachment.attachment_id, &uuid::Uuid::new_v4().to_string())
        .unwrap();
    sender
        .apply_attachment_retention_response(&AttachmentUseResponse {
            use_id: registration.use_id,
            attachment_id: registration.attachment_id,
            device_id: registration.device_id,
            kind: registration.kind,
            state: AttachmentUseState::Registered,
        })
        .unwrap();
    let mut server = Server::default();
    server.upload(&sender);
    server.sync(&gateway);
    gateway
        .install_downloaded_attachment(
            attachment.attachment_id,
            &sender.native_cipher_file(attachment.attachment_id).unwrap(),
        )
        .unwrap();
    assert!(matches!(
        gateway.begin_send_attempt(original.command_id).unwrap(),
        PermitDecision::Permit(_)
    ));
    gateway
        .record_send_result(original.command_id, SendResult::FailedBeforeSubmission)
        .unwrap();
    server.upload(&gateway);
    server.sync(&sender);
    assert_eq!(
        message(&sender, original.message_id).send_state,
        Some(SendState::FailedBeforeSubmission)
    );

    // Terminal evidence queues the release; the sender applies its response.
    let mut release = find_release(&sender);
    if release.is_none() {
        // A reopen runs the bounded inspection again, as the retention suite does.
        drop(sender);
        sender = unlocked(&sender_cfg, &vault);
        release = find_release(&sender);
    }
    let release = release.expect("terminal evidence queues a release");
    sender
        .apply_attachment_retention_response(&AttachmentUseResponse {
            use_id: release.use_id,
            attachment_id: release.attachment_id,
            device_id: release.device_id,
            kind: release.kind,
            state: AttachmentUseState::Released,
        })
        .unwrap();

    let conversation = original_conversation(&sender);
    let before = sender.messages(conversation).unwrap().len();
    assert_eq!(
        sender.send_again(original.message_id, route(&gateway_cfg)),
        Err(Error::SendAgainAttachmentsUnavailable)
    );
    assert_eq!(sender.messages(conversation).unwrap().len(), before);
}

/// The sender's release operation for terminal evidence, found within a few bounded inspections.
fn find_release(client: &Client) -> Option<PendingAttachmentUseOperation> {
    (0..4).find_map(|_| {
        client
            .prepare_attachment_retention_operations(10)
            .unwrap()
            .into_iter()
            .find(|operation| operation.operation == AttachmentUseOperationKind::Release)
    })
}

fn original_conversation(client: &Client) -> ConversationId {
    client.list_conversations().unwrap()[0].conversation_id
}

#[test]
fn two_devices_sending_again_each_run_their_own_command_on_their_own_route() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let (original_cfg, gateway_a_cfg, gateway_b_cfg) = (
        config(&dir, "original-gateway", &vault),
        config(&dir, "gateway-a", &vault),
        config(&dir, "gateway-b", &vault),
    );
    let sender = unlocked(&config(&dir, "sender", &vault), &vault);
    let (_, original) = queued_sms(&sender, &original_cfg, "race");
    let mut server = Server::default();
    server.upload(&sender);
    let device_x = unlocked(&config(&dir, "device-x", &vault), &vault);
    let device_y = unlocked(&config(&dir, "device-y", &vault), &vault);
    server.sync(&device_x);
    server.sync(&device_y);

    let copy_x = device_x
        .send_again(original, route(&gateway_a_cfg))
        .unwrap();
    let copy_y = device_y
        .send_again(original, route(&gateway_b_cfg))
        .unwrap();
    assert_ne!(copy_x.message_id, copy_y.message_id);
    assert_ne!(copy_x.command_id, copy_y.command_id);
    server.upload(&device_x);
    server.upload(&device_y);

    let gateway_a = unlocked(&gateway_a_cfg, &vault);
    let gateway_b = unlocked(&gateway_b_cfg, &vault);
    server.sync(&gateway_a);
    server.sync(&gateway_b);
    assert_eq!(command_ids(&gateway_a), vec![copy_x.command_id]);
    assert_eq!(command_ids(&gateway_b), vec![copy_y.command_id]);
    assert!(matches!(
        gateway_a.begin_send_attempt(copy_x.command_id).unwrap(),
        PermitDecision::Permit(_)
    ));
    assert!(matches!(
        gateway_b.begin_send_attempt(copy_y.command_id).unwrap(),
        PermitDecision::Permit(_)
    ));
    for client in [&device_x, &device_y, &gateway_a, &gateway_b] {
        assert!(
            client.quarantined().unwrap().is_empty(),
            "nothing is quarantined"
        );
    }

    server.upload(&gateway_a);
    server.upload(&gateway_b);
    server.sync(&device_x);
    server.sync(&device_y);
    assert_eq!(
        message(&device_x, copy_x.message_id).send_state,
        Some(SendState::PersistedGateway),
        "X follows its own copy's status from gateway A"
    );
    assert_eq!(
        message(&device_y, copy_y.message_id).send_state,
        Some(SendState::PersistedGateway),
        "Y follows its own copy's status from gateway B"
    );
    assert_eq!(
        message(&device_x, original).sent_again_message_id,
        Some(copy_x.message_id)
    );
    assert_eq!(
        message(&device_y, original).sent_again_message_id,
        Some(copy_y.message_id)
    );
}

#[test]
fn history_applied_after_an_empty_retirement_pass_is_retired_later() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let (desktop_cfg, gateway_cfg) = (
        config(&dir, "desktop", &vault),
        config(&dir, "gateway", &vault),
    );
    let desktop = unlocked(&desktop_cfg, &vault);
    let conversation = ConversationId::new();
    desktop
        .queue_send(sms(conversation, "waiting for keys"), route(&gateway_cfg))
        .unwrap();
    let mut server = Server::default();
    server.upload(&desktop);

    // Locked gateway: the snapshot imports, but its record waits in the journal for keys.
    let gateway = open(&gateway_cfg);
    import(&gateway, &all_records(&server), server.log.len() as u64);
    assert_eq!(
        gateway.apply_pending(usize::MAX).unwrap().waiting_for_keys,
        1
    );
    assert_eq!(
        gateway.retire_swallowed_commands().unwrap(),
        0,
        "the ledger is empty, so this pass finds nothing"
    );
    assert!(!gateway.swallowed_retirement_more_remains().unwrap());

    // The keys arrive and the record applies. No new snapshot is imported, yet the command is retired.
    gateway
        .unlock(&vault.profile, &vault.header, PASSPHRASE)
        .unwrap();
    connect(&gateway);
    gateway.apply_pending(usize::MAX).unwrap();
    assert_eq!(gateway.retire_swallowed_commands().unwrap(), 1);

    server.upload(&gateway);
    server.sync(&desktop);
    assert_eq!(
        only_message(&desktop, conversation).send_state,
        Some(SendState::FailedBeforeSubmission)
    );
}

#[test]
fn send_route_reports_the_route_a_command_was_authored_for() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let gateway_cfg = config(&dir, "gateway", &vault);
    let desktop = unlocked(&config(&dir, "sender", &vault), &vault);
    let conversation = ConversationId::new();
    let carrier = desktop
        .queue_send(sms(conversation, "carrier"), route(&gateway_cfg))
        .unwrap();
    let rcs = desktop
        .queue_send(
            sms(conversation, "rcs"),
            GatewayRoute::RcsLink {
                gateway_device_id: gateway_cfg.device_id,
            },
        )
        .unwrap();
    let incoming_id = desktop
        .capture_incoming(incoming("received", "received-route"))
        .unwrap()
        .message_id;

    assert_eq!(
        message(&desktop, carrier.message_id).send_route,
        Some(SendRouteKind::Carrier)
    );
    assert_eq!(
        message(&desktop, rcs.message_id).send_route,
        Some(SendRouteKind::Rcs)
    );
    assert_eq!(message(&desktop, incoming_id).send_route, None);
}
