mod common;

use common::*;
use peppy_client_core::{
    AttachmentRetentionState, AttachmentUseKind, AttachmentUseOperationKind, AttachmentUseResponse,
    AttachmentUseState, Error, RemoteAttachmentState, SendResult,
};
use std::fs;

fn prepared(client: &Client, dir: &tempfile::TempDir, name: &str) -> AttachmentInfo {
    let source = dir.path().join(name);
    fs::write(&source, b"ciphertext source").unwrap();
    client
        .prepare_attachment(&source, "text/plain", name)
        .unwrap()
}

fn response(
    operation: &PendingAttachmentUseOperation,
    state: AttachmentUseState,
) -> AttachmentUseResponse {
    AttachmentUseResponse {
        use_id: operation.use_id,
        attachment_id: operation.attachment_id,
        device_id: operation.device_id,
        kind: operation.kind,
        state,
    }
}

#[test]
fn canonical_response_identity_is_validated_and_released_replay_is_monotonic() {
    let dir = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "canonical", &vault), &vault);
    let attachment = prepared(&client, &dir, "attachment.txt");
    let registration = client
        .begin_attachment_retention_use(attachment.attachment_id, AttachmentUseKind::PendingSend)
        .unwrap();

    let mut wrong = response(&registration, AttachmentUseState::Registered);
    wrong.device_id = DeviceId::new();
    assert_eq!(
        client.apply_attachment_retention_response(&wrong),
        Err(Error::Conflict)
    );
    assert_eq!(
        client.pending_attachment_retention_operations().unwrap(),
        vec![registration.clone()]
    );

    client
        .apply_attachment_retention_response(&response(&registration, AttachmentUseState::Released))
        .unwrap();
    assert!(
        client
            .pending_attachment_retention_operations()
            .unwrap()
            .is_empty()
    );
    client
        .apply_attachment_retention_response(&response(
            &registration,
            AttachmentUseState::Registered,
        ))
        .unwrap();
    assert!(
        client
            .pending_attachment_retention_operations()
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        client.request_attachment_retention_release(registration.use_id),
        Err(Error::Conflict)
    );
}

#[test]
fn pending_send_release_requires_durable_terminal_command_evidence_across_reopen() {
    let dir = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let sender_config = config(&dir, "sender", &vault);
    let gateway_config = config(&dir, "gateway", &vault);
    let sender = unlocked(&sender_config, &vault);
    let gateway = unlocked(&gateway_config, &vault);
    let attachment = prepared(&sender, &dir, "send.txt");
    let registration = sender
        .begin_attachment_retention_use(attachment.attachment_id, AttachmentUseKind::PendingSend)
        .unwrap();
    let queued = sender
        .queue_mms(
            OutgoingMms {
                conversation_id: ConversationId::new(),
                recipients: vec![ADDRESS.into()],
                body: "retained".into(),
                attachment_ids: vec![attachment.attachment_id],
                subject: None,
            },
            route(&gateway_config),
        )
        .unwrap();
    sender
        .mark_attachment_uploaded(attachment.attachment_id, &uuid::Uuid::new_v4().to_string())
        .unwrap();
    sender
        .apply_attachment_retention_response(&response(
            &registration,
            AttachmentUseState::Registered,
        ))
        .unwrap();
    assert_eq!(
        sender.request_attachment_retention_release(registration.use_id),
        Err(Error::Conflict)
    );

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
        gateway.begin_send_attempt(queued.command_id).unwrap(),
        PermitDecision::Permit(_)
    ));
    gateway
        .record_send_result(queued.command_id, SendResult::Submitted)
        .unwrap();
    server.upload(&gateway);
    server.sync(&sender);
    assert_eq!(
        sender.request_attachment_retention_release(registration.use_id),
        Err(Error::Conflict)
    );
    assert!(
        sender
            .prepare_attachment_retention_operations(10)
            .unwrap()
            .iter()
            .all(|operation| operation.operation != AttachmentUseOperationKind::Release),
        "Submitted/OutcomeUnknown is not terminal release evidence"
    );
    let nonterminal = sender
        .begin_attachment_retention_use(
            attachment.attachment_id,
            AttachmentUseKind::RestoreCritical,
        )
        .unwrap();
    sender
        .apply_attachment_retention_response(&response(
            &nonterminal,
            AttachmentUseState::Registered,
        ))
        .unwrap();
    gateway
        .record_send_result(queued.command_id, SendResult::Delivered)
        .unwrap();
    server.upload(&gateway);
    server.sync(&sender);
    assert!(
        sender
            .prepare_attachment_retention_operations(1)
            .unwrap()
            .is_empty(),
        "bounded inspection may consume one nonterminal hint without producing network work"
    );
    assert!(sender.attachment_retention_preparation_pending().unwrap());
    drop(sender);

    let reopened = unlocked(&sender_config, &vault);
    assert!(
        reopened.attachment_retention_preparation_pending().unwrap(),
        "later due candidate work must survive reopen after an empty bounded batch"
    );
    let releases = reopened.prepare_attachment_retention_operations(1).unwrap();
    assert_eq!(releases.len(), 1);
    assert_eq!(releases[0].operation, AttachmentUseOperationKind::Release);
    assert_eq!(releases[0].use_id, registration.use_id);
}

#[test]
fn tombstone_suppresses_real_transfer_queues_and_late_completion_but_preserves_local_ciphertext() {
    let dir = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let configuration = config(&dir, "removed", &vault);
    let client = unlocked(&configuration, &vault);
    let attachment = prepared(&client, &dir, "removed.txt");
    let path = client.native_cipher_file(attachment.attachment_id).unwrap();
    let registration = client
        .begin_attachment_retention_use(attachment.attachment_id, AttachmentUseKind::MessageHistory)
        .unwrap();
    let queued = client
        .queue_mms(
            OutgoingMms {
                conversation_id: ConversationId::new(),
                recipients: vec![ADDRESS.into()],
                body: "removed".into(),
                attachment_ids: vec![attachment.attachment_id],
                subject: None,
            },
            route(&configuration),
        )
        .unwrap();
    assert!(
        client
            .pending_uploads()
            .unwrap()
            .iter()
            .any(|item| item.attachment_id == attachment.attachment_id)
    );

    client
        .report_attachment_removed_by_storage_policy(attachment.attachment_id)
        .unwrap();
    assert!(
        client
            .pending_uploads()
            .unwrap()
            .iter()
            .all(|item| item.attachment_id != attachment.attachment_id)
    );
    assert!(
        client
            .pending_outbox()
            .unwrap()
            .iter()
            .all(|item| item.envelope_id != queued.envelope_id)
    );
    assert_eq!(
        client
            .mark_attachment_uploaded(attachment.attachment_id, &uuid::Uuid::new_v4().to_string()),
        Err(Error::AttachmentRemovedByStoragePolicy)
    );
    assert_eq!(
        client.request_attachment_download(attachment.attachment_id),
        Err(Error::AttachmentRemovedByStoragePolicy)
    );
    assert!(path.is_file());
    drop(client);

    let reopened = unlocked(&configuration, &vault);
    assert!(
        reopened
            .pending_uploads()
            .unwrap()
            .iter()
            .all(|item| item.attachment_id != attachment.attachment_id)
    );
    assert_eq!(
        reopened
            .attachment_retention_state(attachment.attachment_id)
            .unwrap(),
        AttachmentRetentionState {
            remote: RemoteAttachmentState::RemovedByStoragePolicy,
            local_available: true
        }
    );
    assert!(
        reopened
            .pending_attachment_retention_operations()
            .unwrap()
            .is_empty()
    );
    reopened
        .configure_attachment_retention_protocol(true)
        .unwrap();
    reopened.mark_attachment_retention_reconciled().unwrap();
    assert!(reopened.attachment_retention_ready().unwrap());
    assert_eq!(
        reopened.request_attachment_retention_release(registration.use_id),
        Err(Error::Conflict)
    );
}

#[test]
fn active_registered_use_is_ready_after_explicit_reconciliation_but_reopen_invalidates_handshake() {
    let dir = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let configuration = config(&dir, "ready", &vault);
    let client = unlocked(&configuration, &vault);
    let attachment = prepared(&client, &dir, "ready.txt");
    let registration = client
        .begin_attachment_retention_use(attachment.attachment_id, AttachmentUseKind::PendingSend)
        .unwrap();
    client
        .apply_attachment_retention_response(&response(
            &registration,
            AttachmentUseState::Registered,
        ))
        .unwrap();
    client
        .configure_attachment_retention_protocol(true)
        .unwrap();
    client.mark_attachment_retention_reconciled().unwrap();
    assert!(client.attachment_retention_ready().unwrap());
    client
        .begin_snapshot(Cursor(0), 0, SnapshotPurpose::Restore)
        .unwrap();
    assert!(!client.attachment_retention_ready().unwrap());
    client.mark_attachment_retention_reconciled().unwrap();
    drop(client);
    let reopened = unlocked(&configuration, &vault);
    assert!(!reopened.attachment_retention_ready().unwrap());
}

#[test]
fn restore_critical_release_requires_bound_completed_generation() {
    let dir = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "restore-release", &vault), &vault);
    let attachment = prepared(&client, &dir, "restore.txt");
    let first = client
        .begin_attachment_retention_use(
            attachment.attachment_id,
            AttachmentUseKind::RestoreCritical,
        )
        .unwrap();
    client
        .apply_attachment_retention_response(&response(&first, AttachmentUseState::Registered))
        .unwrap();
    assert!(
        client
            .begin_snapshot(Cursor(0), 1, SnapshotPurpose::Restore)
            .is_err()
    );
    assert_eq!(
        client.request_attachment_retention_release(first.use_id),
        Err(Error::Conflict)
    );
    let incomplete = client
        .begin_snapshot(Cursor(1), 1, SnapshotPurpose::Restore)
        .unwrap();
    assert_eq!(
        client.request_attachment_retention_release(first.use_id),
        Err(Error::Conflict)
    );
    assert!(client.finish_snapshot(incomplete.generation).is_err());
    assert_eq!(
        client.request_attachment_retention_release(first.use_id),
        Err(Error::Conflict)
    );

    let second = client
        .begin_attachment_retention_use(
            attachment.attachment_id,
            AttachmentUseKind::RestoreCritical,
        )
        .unwrap();
    client
        .apply_attachment_retention_response(&response(&second, AttachmentUseState::Registered))
        .unwrap();
    let completed = client
        .begin_snapshot(Cursor(0), 0, SnapshotPurpose::Restore)
        .unwrap();
    client.finish_snapshot(completed.generation).unwrap();
    assert_eq!(
        client.request_attachment_retention_release(first.use_id),
        Err(Error::Conflict)
    );
    let release = client
        .request_attachment_retention_release(second.use_id)
        .unwrap();
    assert_eq!(release.use_id, second.use_id);

    let third = client
        .begin_attachment_retention_use(
            attachment.attachment_id,
            AttachmentUseKind::RestoreCritical,
        )
        .unwrap();
    client
        .apply_attachment_retention_response(&response(&third, AttachmentUseState::Registered))
        .unwrap();
    assert_eq!(
        client.request_attachment_retention_release(third.use_id),
        Err(Error::Conflict)
    );
}

#[test]
fn all_required_pending_send_uses_gate_effect_and_message_history_cannot_replace_them() {
    let dir = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let configuration = config(&dir, "multi-use", &vault);
    let client = unlocked(&configuration, &vault);
    let attachment = prepared(&client, &dir, "multi.txt");
    let pending_send = client
        .begin_attachment_retention_use(attachment.attachment_id, AttachmentUseKind::PendingSend)
        .unwrap();
    let history = client
        .begin_attachment_retention_use(attachment.attachment_id, AttachmentUseKind::MessageHistory)
        .unwrap();
    client
        .apply_attachment_retention_response(&response(&history, AttachmentUseState::Registered))
        .unwrap();
    let queued = client
        .queue_mms(
            OutgoingMms {
                conversation_id: ConversationId::new(),
                recipients: vec![ADDRESS.into()],
                body: "multi".into(),
                attachment_ids: vec![attachment.attachment_id],
                subject: None,
            },
            route(&configuration),
        )
        .unwrap();
    client
        .mark_attachment_uploaded(attachment.attachment_id, &uuid::Uuid::new_v4().to_string())
        .unwrap();
    assert!(
        client
            .pending_outbox()
            .unwrap()
            .iter()
            .all(|item| item.envelope_id != queued.envelope_id)
    );
    client
        .apply_attachment_retention_response(&response(
            &pending_send,
            AttachmentUseState::Registered,
        ))
        .unwrap();
    assert!(
        client
            .pending_outbox()
            .unwrap()
            .iter()
            .any(|item| item.envelope_id == queued.envelope_id)
    );
}

#[test]
fn released_replay_removes_already_queued_effect_from_publication_and_count() {
    let dir = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let configuration = config(&dir, "released-queued", &vault);
    let client = unlocked(&configuration, &vault);
    let attachment = prepared(&client, &dir, "released.txt");
    let registration = client
        .begin_attachment_retention_use(attachment.attachment_id, AttachmentUseKind::PendingSend)
        .unwrap();
    let queued = client
        .queue_mms(
            OutgoingMms {
                conversation_id: ConversationId::new(),
                recipients: vec![ADDRESS.into()],
                body: "released".into(),
                attachment_ids: vec![attachment.attachment_id],
                subject: None,
            },
            route(&configuration),
        )
        .unwrap();
    client
        .mark_attachment_uploaded(attachment.attachment_id, &uuid::Uuid::new_v4().to_string())
        .unwrap();
    client
        .apply_attachment_retention_response(&response(
            &registration,
            AttachmentUseState::Registered,
        ))
        .unwrap();
    assert!(
        client
            .pending_outbox()
            .unwrap()
            .iter()
            .any(|item| item.envelope_id == queued.envelope_id)
    );
    client
        .apply_attachment_retention_response(&response(&registration, AttachmentUseState::Released))
        .unwrap();
    drop(client);
    let client = unlocked(&configuration, &vault);
    assert!(
        client
            .pending_outbox()
            .unwrap()
            .iter()
            .all(|item| item.envelope_id != queued.envelope_id)
    );
    assert_eq!(client.pending_outbox_count().unwrap(), 0);
}

#[test]
fn received_gateway_command_requires_its_own_registered_protection_before_permit() {
    let dir = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let sender_cfg = config(&dir, "gateway-sender", &vault);
    let gateway_cfg = config(&dir, "gateway-receiver", &vault);
    let sender = unlocked(&sender_cfg, &vault);
    let gateway = unlocked(&gateway_cfg, &vault);
    gateway
        .configure_attachment_retention_protocol(true)
        .unwrap();
    let attachment = prepared(&sender, &dir, "gateway.txt");
    let sender_use = sender
        .begin_attachment_retention_use(attachment.attachment_id, AttachmentUseKind::PendingSend)
        .unwrap();
    let queued = sender
        .queue_mms(
            OutgoingMms {
                conversation_id: ConversationId::new(),
                recipients: vec![ADDRESS.into()],
                body: "gateway".into(),
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
        .apply_attachment_retention_response(&response(&sender_use, AttachmentUseState::Registered))
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
    let gateway_use = gateway
        .pending_attachment_retention_operations()
        .unwrap()
        .into_iter()
        .find(|operation| operation.kind == AttachmentUseKind::PendingSend)
        .expect("received command must atomically create gateway protection");
    gateway
        .configure_attachment_retention_protocol(true)
        .unwrap();
    assert_eq!(
        gateway.pending_attachment_retention_operations().unwrap(),
        vec![gateway_use.clone()],
        "repeated enable must reuse the exact command requirement and use UUID"
    );
    assert!(matches!(
        gateway.begin_send_attempt(queued.command_id).unwrap(),
        PermitDecision::Blocked(_)
    ));
    gateway
        .apply_attachment_retention_response(&response(
            &gateway_use,
            AttachmentUseState::Registered,
        ))
        .unwrap();
    drop(gateway);
    let gateway = unlocked(&gateway_cfg, &vault);
    gateway
        .configure_attachment_retention_protocol(true)
        .unwrap();
    assert!(
        gateway
            .pending_attachment_retention_operations()
            .unwrap()
            .is_empty(),
        "read-only pending does not prepare canonical replay work"
    );
    assert!(matches!(
        gateway.begin_send_attempt(queued.command_id).unwrap(),
        PermitDecision::Blocked(_)
    ));
    let replay = gateway.prepare_attachment_retention_operations(10).unwrap();
    assert_eq!(replay, vec![gateway_use.clone()]);
    gateway
        .apply_attachment_retention_response(&response(
            &gateway_use,
            AttachmentUseState::Registered,
        ))
        .unwrap();
    assert!(matches!(
        gateway.begin_send_attempt(queued.command_id).unwrap(),
        PermitDecision::Permit(_)
    ));
}

#[test]
fn released_lifecycle_before_effect_is_not_treated_as_legacy() {
    let dir = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let configuration = config(&dir, "released-before", &vault);
    let client = unlocked(&configuration, &vault);
    let attachment = prepared(&client, &dir, "released-before.txt");
    let use_operation = client
        .begin_attachment_retention_use(attachment.attachment_id, AttachmentUseKind::PendingSend)
        .unwrap();
    client
        .apply_attachment_retention_response(&response(
            &use_operation,
            AttachmentUseState::Released,
        ))
        .unwrap();
    let queued = client
        .queue_mms(
            OutgoingMms {
                conversation_id: ConversationId::new(),
                recipients: vec![ADDRESS.into()],
                body: "must remain blocked".into(),
                attachment_ids: vec![attachment.attachment_id],
                subject: None,
            },
            route(&configuration),
        )
        .unwrap();
    client
        .mark_attachment_uploaded(attachment.attachment_id, &uuid::Uuid::new_v4().to_string())
        .unwrap();
    assert!(
        client
            .pending_outbox()
            .unwrap()
            .iter()
            .all(|item| item.envelope_id != queued.envelope_id)
    );
}

#[test]
fn negotiated_protocol_atomically_creates_message_history_and_pending_send_uses() {
    let dir = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let configuration = config(&dir, "automatic", &vault);
    let client = unlocked(&configuration, &vault);
    client
        .configure_attachment_retention_protocol(true)
        .unwrap();
    let attachment = prepared(&client, &dir, "automatic.txt");
    assert_eq!(
        client
            .attachment_upload_retention(attachment.attachment_id)
            .unwrap(),
        AttachmentUploadRetention {
            retention_class: AttachmentRetentionClass::MessageMedia,
            reference_tracking: true,
        }
    );
    let queued = client
        .queue_mms(
            OutgoingMms {
                conversation_id: ConversationId::new(),
                recipients: vec![ADDRESS.into()],
                body: "automatic".into(),
                attachment_ids: vec![attachment.attachment_id],
                subject: None,
            },
            route(&configuration),
        )
        .unwrap();
    let operations = client.pending_attachment_retention_operations().unwrap();
    assert_eq!(operations.len(), 2);
    assert!(
        operations
            .iter()
            .any(|item| item.kind == AttachmentUseKind::MessageHistory)
    );
    assert!(
        operations
            .iter()
            .any(|item| item.kind == AttachmentUseKind::PendingSend)
    );
    assert_eq!(
        client
            .attachment_upload_retention(attachment.attachment_id)
            .unwrap(),
        AttachmentUploadRetention {
            retention_class: AttachmentRetentionClass::MessageMedia,
            reference_tracking: true,
        }
    );
    client
        .mark_attachment_uploaded(attachment.attachment_id, &uuid::Uuid::new_v4().to_string())
        .unwrap();
    assert!(
        client
            .pending_outbox()
            .unwrap()
            .iter()
            .all(|item| item.envelope_id != queued.envelope_id)
    );
    let history = operations
        .iter()
        .find(|item| item.kind == AttachmentUseKind::MessageHistory)
        .unwrap();
    client
        .apply_attachment_retention_response(&response(history, AttachmentUseState::Registered))
        .unwrap();
    assert!(
        client
            .pending_outbox()
            .unwrap()
            .iter()
            .all(|item| item.envelope_id != queued.envelope_id)
    );
    let send = operations
        .iter()
        .find(|item| item.kind == AttachmentUseKind::PendingSend)
        .unwrap();
    client
        .apply_attachment_retention_response(&response(send, AttachmentUseState::Registered))
        .unwrap();
    assert!(
        client
            .pending_outbox()
            .unwrap()
            .iter()
            .any(|item| item.envelope_id == queued.envelope_id)
    );
}

#[test]
fn protocol_default_off_preserves_legacy_producer_behavior() {
    let dir = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let configuration = config(&dir, "legacy-default", &vault);
    let client = unlocked(&configuration, &vault);
    let attachment = prepared(&client, &dir, "legacy.txt");
    client
        .queue_mms(
            OutgoingMms {
                conversation_id: ConversationId::new(),
                recipients: vec![ADDRESS.into()],
                body: "legacy".into(),
                attachment_ids: vec![attachment.attachment_id],
                subject: None,
            },
            route(&configuration),
        )
        .unwrap();
    assert!(
        client
            .pending_attachment_retention_operations()
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        client
            .attachment_upload_retention(attachment.attachment_id)
            .unwrap()
            .retention_class,
        AttachmentRetentionClass::Protected
    );
}

#[test]
fn ordinary_incoming_mms_capture_atomically_creates_message_history_protection() {
    let dir = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let configuration = config(&dir, "normal-capture", &vault);
    let client = unlocked(&configuration, &vault);
    client
        .configure_attachment_retention_protocol(true)
        .unwrap();
    let attachment = prepared(&client, &dir, "normal-capture.txt");

    client
        .capture_incoming_mms(IncomingMms {
            conversation_id: None,
            sender_address: ADDRESS.into(),
            body: "normal capture".into(),
            provider_message_id: Some("normal-capture-1".into()),
            imported: false,
            attachment_ids: vec![attachment.attachment_id],
            recipients: vec![ADDRESS.into()],
            subject: None,
        })
        .unwrap();

    let operations = client.pending_attachment_retention_operations().unwrap();
    assert_eq!(operations.len(), 1);
    assert_eq!(operations[0].kind, AttachmentUseKind::MessageHistory);
}

#[test]
fn legacy_attachment_classification_is_immutable_across_manual_uses_and_uploaded_reuse() {
    let dir = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let configuration = config(&dir, "immutable-legacy", &vault);
    let client = unlocked(&configuration, &vault);
    let attachment = prepared(&client, &dir, "immutable-legacy.txt");
    let legacy = AttachmentUploadRetention {
        retention_class: AttachmentRetentionClass::Protected,
        reference_tracking: false,
    };
    assert_eq!(
        client
            .attachment_upload_retention(attachment.attachment_id)
            .unwrap(),
        legacy
    );

    client
        .configure_attachment_retention_protocol(true)
        .unwrap();
    client
        .begin_attachment_retention_use(attachment.attachment_id, AttachmentUseKind::MessageHistory)
        .unwrap();
    assert_eq!(
        client
            .attachment_upload_retention(attachment.attachment_id)
            .unwrap(),
        legacy
    );
    client
        .mark_attachment_uploaded(attachment.attachment_id, &uuid::Uuid::new_v4().to_string())
        .unwrap();
    client
        .begin_attachment_retention_use(attachment.attachment_id, AttachmentUseKind::PendingSend)
        .unwrap();
    assert_eq!(
        client
            .attachment_upload_retention(attachment.attachment_id)
            .unwrap(),
        legacy
    );
}

#[test]
fn tombstoned_live_and_historical_gateway_commands_do_not_abort_ingest_or_create_operations() {
    let dir = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let sender_cfg = config(&dir, "removed-command-sender", &vault);
    let live_cfg = config(&dir, "removed-command-live", &vault);
    let historical_cfg = config(&dir, "removed-command-history", &vault);
    let sender = unlocked(&sender_cfg, &vault);
    let live = unlocked(&live_cfg, &vault);
    let historical = unlocked(&historical_cfg, &vault);
    let attachment = prepared(&sender, &dir, "removed-command.txt");
    let queued = sender
        .queue_mms(
            OutgoingMms {
                conversation_id: ConversationId::new(),
                recipients: vec![ADDRESS.into()],
                body: "removed command".into(),
                attachment_ids: vec![attachment.attachment_id],
                subject: None,
            },
            route(&live_cfg),
        )
        .unwrap();
    sender
        .mark_attachment_uploaded(attachment.attachment_id, &uuid::Uuid::new_v4().to_string())
        .unwrap();
    let mut server = Server::default();
    server.upload(&sender);

    live.configure_attachment_retention_protocol(true).unwrap();
    live.report_attachment_removed_by_storage_policy(attachment.attachment_id)
        .unwrap();
    server.sync(&live);
    assert!(
        live.pending_attachment_retention_operations()
            .unwrap()
            .is_empty()
    );
    assert!(matches!(
        live.begin_send_attempt(queued.command_id).unwrap(),
        PermitDecision::Blocked(_)
    ));
    live.configure_attachment_retention_protocol(true).unwrap();
    assert!(
        live.pending_attachment_retention_operations()
            .unwrap()
            .is_empty()
    );

    historical
        .configure_attachment_retention_protocol(true)
        .unwrap();
    historical
        .report_attachment_removed_by_storage_policy(attachment.attachment_id)
        .unwrap();
    let records = server
        .log
        .iter()
        .enumerate()
        .map(|(index, envelope)| SnapshotRecord {
            cursor: Cursor(index as u64 + 1),
            envelope: envelope.clone(),
        })
        .collect::<Vec<_>>();
    let snapshot = historical
        .begin_snapshot(
            Cursor(records.len() as u64),
            records.len() as u64,
            SnapshotPurpose::Restore,
        )
        .unwrap();
    historical
        .append_snapshot_page(snapshot.generation, &records)
        .unwrap();
    historical.finish_snapshot(snapshot.generation).unwrap();
    historical.apply_pending(100).unwrap();
    assert!(
        historical
            .pending_attachment_retention_operations()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn nonempty_restore_releases_only_after_journal_apply_and_projection_promotion() {
    let dir = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let source = unlocked(&config(&dir, "restore-source", &vault), &vault);
    let target_cfg = config(&dir, "restore-target", &vault);
    let target = unlocked(&target_cfg, &vault);
    let attachment = prepared(&target, &dir, "restore-critical.txt");
    let restore = target
        .begin_attachment_retention_use(
            attachment.attachment_id,
            AttachmentUseKind::RestoreCritical,
        )
        .unwrap();
    target
        .apply_attachment_retention_response(&response(&restore, AttachmentUseState::Registered))
        .unwrap();
    source
        .capture_incoming(incoming("restored", "restore-1"))
        .unwrap();
    let mut server = Server::default();
    server.upload(&source);
    drop(target);

    let locked = open(&target_cfg);
    let snapshot = locked
        .begin_snapshot_with_compaction(Cursor(1), 1, SnapshotPurpose::Restore, Some(1))
        .unwrap();
    locked
        .append_snapshot_page(
            snapshot.generation,
            &[SnapshotRecord {
                cursor: Cursor(1),
                envelope: server.log[0].clone(),
            }],
        )
        .unwrap();
    locked.finish_snapshot(snapshot.generation).unwrap();
    locked.apply_pending(100).unwrap();
    assert_eq!(
        locked.request_attachment_retention_release(restore.use_id),
        Err(Error::Conflict),
        "draining ciphertext while keys are locked is not restore completion"
    );
    locked
        .unlock(&vault.profile, &vault.header, PASSPHRASE)
        .unwrap();
    for _ in 0..3 {
        locked.apply_pending(100).unwrap();
    }
    locked
        .request_attachment_retention_release(restore.use_id)
        .unwrap();

    let failed_restore = locked
        .begin_attachment_retention_use(
            attachment.attachment_id,
            AttachmentUseKind::RestoreCritical,
        )
        .unwrap();
    locked
        .apply_attachment_retention_response(&response(
            &failed_restore,
            AttachmentUseState::Registered,
        ))
        .unwrap();
    source
        .capture_incoming(incoming("tampered", "restore-2"))
        .unwrap();
    server.upload(&source);
    let mut tampered = server.log[1].clone();
    tampered.ciphertext[0] ^= 1;
    let failed = locked
        .begin_snapshot_with_compaction(Cursor(2), 1, SnapshotPurpose::Restore, Some(2))
        .unwrap();
    locked
        .append_snapshot_page(
            failed.generation,
            &[SnapshotRecord {
                cursor: Cursor(2),
                envelope: tampered,
            }],
        )
        .unwrap();
    locked.finish_snapshot(failed.generation).unwrap();
    for _ in 0..3 {
        locked.apply_pending(100).unwrap();
    }
    assert_eq!(
        locked.request_attachment_retention_release(failed_restore.use_id),
        Err(Error::Conflict),
        "a failed projection must retain restore-critical protection"
    );
    assert!(
        locked
            .prepare_attachment_retention_operations(10)
            .unwrap()
            .iter()
            .all(|operation| operation.use_id != failed_restore.use_id
                || operation.operation != AttachmentUseOperationKind::Release),
        "a failed projection must never auto-queue release"
    );
}

#[test]
fn superseded_projection_gc_never_completes_its_bound_restore() {
    let dir = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let source = unlocked(&config(&dir, "superseded-source", &vault), &vault);
    let target_cfg = config(&dir, "superseded-target", &vault);
    let target = unlocked(&target_cfg, &vault);
    let attachment = prepared(&target, &dir, "superseded-restore.txt");
    let restore = target
        .begin_attachment_retention_use(
            attachment.attachment_id,
            AttachmentUseKind::RestoreCritical,
        )
        .unwrap();
    target
        .apply_attachment_retention_response(&response(&restore, AttachmentUseState::Registered))
        .unwrap();
    source
        .capture_notification(NotificationCapture {
            notification_key: "superseded".into(),
            instance: "first".into(),
            package_name: "com.example.restore".into(),
            app_name: "Restore".into(),
            app_icon_png_base64: None,
            title: "Restore".into(),
            text: "projection evidence".into(),
            category: None,
            posted_at: 1,
            dismissible: true,
        })
        .unwrap();
    let mut server = Server::default();
    server.upload(&source);
    drop(target);

    let locked = open(&target_cfg);
    let old = locked
        .begin_snapshot_with_compaction(Cursor(1), 1, SnapshotPurpose::Restore, Some(1))
        .unwrap();
    locked
        .append_snapshot_page(
            old.generation,
            &[SnapshotRecord {
                cursor: Cursor(1),
                envelope: server.log[0].clone(),
            }],
        )
        .unwrap();
    locked.finish_snapshot(old.generation).unwrap();
    locked.apply_pending(100).unwrap();

    let replacement = locked
        .begin_snapshot_with_compaction(Cursor(1), 0, SnapshotPurpose::Resync, Some(2))
        .unwrap();
    locked.finish_snapshot(replacement.generation).unwrap();
    for _ in 0..3 {
        locked.apply_pending(100).unwrap();
    }
    locked
        .unlock(&vault.profile, &vault.header, PASSPHRASE)
        .unwrap();
    let applied = locked.apply_pending(100).unwrap();
    assert_eq!(
        applied.superseded, 1,
        "the newer projection must terminally duplicate the old journal member"
    );
    assert_eq!(
        locked.request_attachment_retention_release(restore.use_id),
        Err(Error::Conflict),
        "supersession and projection GC are durable non-success evidence for the old restore"
    );
    assert!(
        locked
            .prepare_attachment_retention_operations(10)
            .unwrap()
            .iter()
            .all(|operation| operation.use_id != restore.use_id
                || operation.operation != AttachmentUseOperationKind::Release),
        "a duplicate journal row cannot turn a superseded projection into restore success"
    );
}

#[test]
fn disabled_protocol_session_never_reports_ready_after_reconciliation() {
    let dir = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "disabled-ready", &vault), &vault);
    client.mark_attachment_retention_reconciled().unwrap();
    assert!(!client.attachment_retention_protocol_enabled().unwrap());
    assert!(!client.attachment_retention_ready().unwrap());
}

#[test]
fn reconciliation_prepare_replays_same_uuid_and_keeps_effect_blocked_until_canonical_response() {
    let dir = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let configuration = config(&dir, "reconcile-released", &vault);
    let client = unlocked(&configuration, &vault);
    let attachment = prepared(&client, &dir, "reconcile-released.txt");
    client
        .configure_attachment_retention_protocol(true)
        .unwrap();
    let registration = client
        .begin_attachment_retention_use(attachment.attachment_id, AttachmentUseKind::PendingSend)
        .unwrap();
    let queued = client
        .queue_mms(
            OutgoingMms {
                conversation_id: ConversationId::new(),
                recipients: vec![ADDRESS.into()],
                body: "reconcile released".into(),
                attachment_ids: vec![attachment.attachment_id],
                subject: None,
            },
            route(&configuration),
        )
        .unwrap();
    client
        .mark_attachment_uploaded(attachment.attachment_id, &uuid::Uuid::new_v4().to_string())
        .unwrap();
    client
        .apply_attachment_retention_response(&response(
            &registration,
            AttachmentUseState::Registered,
        ))
        .unwrap();
    assert!(
        client
            .pending_outbox()
            .unwrap()
            .iter()
            .any(|item| item.envelope_id == queued.envelope_id)
    );
    drop(client);

    let reopened = unlocked(&configuration, &vault);
    reopened
        .configure_attachment_retention_protocol(true)
        .unwrap();
    assert!(
        reopened
            .pending_outbox()
            .unwrap()
            .iter()
            .all(|item| item.envelope_id != queued.envelope_id),
        "a registered effect must be gated until this session observes canonical state"
    );
    assert!(
        reopened
            .pending_attachment_retention_operations()
            .unwrap()
            .is_empty()
    );
    let replay = reopened
        .prepare_attachment_retention_operations(10)
        .unwrap();
    assert_eq!(replay.len(), 1);
    assert_eq!(replay[0].operation, AttachmentUseOperationKind::Register);
    assert_eq!(replay[0].use_id, registration.use_id);
    assert_eq!(
        replay,
        reopened.pending_attachment_retention_operations().unwrap()
    );
    reopened
        .apply_attachment_retention_response(&response(&registration, AttachmentUseState::Released))
        .unwrap();
    reopened.mark_attachment_retention_reconciled().unwrap();
    assert!(
        reopened
            .pending_outbox()
            .unwrap()
            .iter()
            .all(|item| item.envelope_id != queued.envelope_id),
        "canonical Released must not revive the queued effect"
    );
}

#[test]
fn public_prepare_limit_is_the_enforced_core_boundary() {
    assert_eq!(MAX_PREPARED_ATTACHMENT_RETENTION_OPERATIONS, 256);
}

#[test]
fn reconciliation_prepare_is_bounded_and_reopen_reuses_the_queued_uuid() {
    let dir = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let configuration = config(&dir, "reconcile-bounded", &vault);
    let client = unlocked(&configuration, &vault);
    let attachments = (0..2)
        .map(|index| prepared(&client, &dir, &format!("reconcile-{index}.txt")))
        .collect::<Vec<_>>();
    client
        .configure_attachment_retention_protocol(true)
        .unwrap();
    let mut registrations = Vec::new();
    for (index, attachment) in attachments.into_iter().enumerate() {
        let registration = client
            .begin_attachment_retention_use(
                attachment.attachment_id,
                AttachmentUseKind::PendingSend,
            )
            .unwrap();
        client
            .queue_mms(
                OutgoingMms {
                    conversation_id: ConversationId::new(),
                    recipients: vec![ADDRESS.into()],
                    body: format!("reconcile {index}"),
                    attachment_ids: vec![attachment.attachment_id],
                    subject: None,
                },
                route(&configuration),
            )
            .unwrap();
        client
            .mark_attachment_uploaded(attachment.attachment_id, &uuid::Uuid::new_v4().to_string())
            .unwrap();
        client
            .apply_attachment_retention_response(&response(
                &registration,
                AttachmentUseState::Registered,
            ))
            .unwrap();
        registrations.push(registration);
    }
    drop(client);

    let reopened = unlocked(&configuration, &vault);
    reopened
        .configure_attachment_retention_protocol(true)
        .unwrap();
    assert!(matches!(
        reopened.prepare_attachment_retention_operations(0),
        Err(Error::InvalidRequest(_))
    ));
    assert!(matches!(
        reopened.prepare_attachment_retention_operations(
            MAX_PREPARED_ATTACHMENT_RETENTION_OPERATIONS + 1,
        ),
        Err(Error::InvalidRequest(_))
    ));
    let first = reopened.prepare_attachment_retention_operations(1).unwrap();
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].operation, AttachmentUseOperationKind::Register);
    drop(reopened);

    let reopened = unlocked(&configuration, &vault);
    reopened
        .configure_attachment_retention_protocol(true)
        .unwrap();
    assert_eq!(
        reopened.prepare_attachment_retention_operations(1).unwrap(),
        first,
        "an interrupted batch must replay the exact durable UUID"
    );
    reopened
        .apply_attachment_retention_response(&response(
            registrations
                .iter()
                .find(|item| item.use_id == first[0].use_id)
                .unwrap(),
            AttachmentUseState::Registered,
        ))
        .unwrap();
    let second = reopened.prepare_attachment_retention_operations(1).unwrap();
    assert_eq!(second.len(), 1);
    assert_ne!(second[0].use_id, first[0].use_id);
    assert!(
        reopened.mark_attachment_retention_reconciled().is_err(),
        "reconciliation cannot complete while a relevant canonical observation is missing"
    );
}

#[test]
fn prepare_never_releases_active_unknown_contact_or_incomplete_restore_protection() {
    let dir = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let configuration = config(&dir, "prepare-conservative", &vault);
    let client = unlocked(&configuration, &vault);
    client
        .configure_attachment_retention_protocol(true)
        .unwrap();
    let attachment = prepared(&client, &dir, "prepare-conservative.txt");
    let uses = [
        AttachmentUseKind::PendingSend,
        AttachmentUseKind::ContactPhoto,
        AttachmentUseKind::RestoreCritical,
    ]
    .into_iter()
    .map(|kind| {
        let operation = client
            .begin_attachment_retention_use(attachment.attachment_id, kind)
            .unwrap();
        client
            .apply_attachment_retention_response(&response(
                &operation,
                AttachmentUseState::Registered,
            ))
            .unwrap();
        operation
    })
    .collect::<Vec<_>>();
    client
        .begin_snapshot(Cursor(1), 1, SnapshotPurpose::Restore)
        .unwrap();

    let operations = client.prepare_attachment_retention_operations(10).unwrap();
    assert!(
        operations
            .iter()
            .all(|item| item.operation != AttachmentUseOperationKind::Release)
    );
    for operation in uses {
        assert_eq!(
            client.request_attachment_retention_release(operation.use_id),
            Err(Error::Conflict)
        );
    }
}

#[test]
fn acknowledged_live_history_is_not_mass_replayed_during_reconciliation() {
    let dir = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let configuration = config(&dir, "reconcile-inert-history", &vault);
    let client = unlocked(&configuration, &vault);
    client
        .configure_attachment_retention_protocol(true)
        .unwrap();
    let attachment = prepared(&client, &dir, "reconcile-inert-history.txt");
    let queued = client
        .queue_mms(
            OutgoingMms {
                conversation_id: ConversationId::new(),
                recipients: vec![ADDRESS.into()],
                body: "durable history".into(),
                attachment_ids: vec![attachment.attachment_id],
                subject: None,
            },
            route(&configuration),
        )
        .unwrap();
    client
        .mark_attachment_uploaded(attachment.attachment_id, &uuid::Uuid::new_v4().to_string())
        .unwrap();
    for operation in client.pending_attachment_retention_operations().unwrap() {
        client
            .apply_attachment_retention_response(&response(
                &operation,
                AttachmentUseState::Registered,
            ))
            .unwrap();
    }
    let envelope = client
        .pending_outbox()
        .unwrap()
        .into_iter()
        .find(|item| item.envelope_id == queued.envelope_id)
        .unwrap();
    client.ack_outbox(envelope.envelope_id).unwrap();
    drop(client);

    let reopened = unlocked(&configuration, &vault);
    reopened
        .configure_attachment_retention_protocol(true)
        .unwrap();
    assert!(
        reopened
            .prepare_attachment_retention_operations(256)
            .unwrap()
            .is_empty(),
        "acknowledged history with a live message reference is not outstanding transport work"
    );
}

#[test]
fn restore_invalidation_replays_same_uuid_and_gates_existing_effect() {
    let dir = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let configuration = config(&dir, "reconcile-restore", &vault);
    let client = unlocked(&configuration, &vault);
    let attachment = prepared(&client, &dir, "reconcile-restore.txt");
    client
        .configure_attachment_retention_protocol(true)
        .unwrap();
    let registration = client
        .begin_attachment_retention_use(attachment.attachment_id, AttachmentUseKind::PendingSend)
        .unwrap();
    let queued = client
        .queue_mms(
            OutgoingMms {
                conversation_id: ConversationId::new(),
                recipients: vec![ADDRESS.into()],
                body: "restore reconciliation".into(),
                attachment_ids: vec![attachment.attachment_id],
                subject: None,
            },
            route(&configuration),
        )
        .unwrap();
    client
        .mark_attachment_uploaded(attachment.attachment_id, &uuid::Uuid::new_v4().to_string())
        .unwrap();
    client
        .apply_attachment_retention_response(&response(
            &registration,
            AttachmentUseState::Registered,
        ))
        .unwrap();
    assert!(
        client
            .pending_outbox()
            .unwrap()
            .iter()
            .any(|item| item.envelope_id == queued.envelope_id)
    );

    let snapshot = client
        .begin_snapshot(Cursor(0), 0, SnapshotPurpose::Restore)
        .unwrap();
    client.finish_snapshot(snapshot.generation).unwrap();
    assert!(
        client
            .pending_attachment_retention_operations()
            .unwrap()
            .is_empty()
    );
    assert!(
        client
            .pending_outbox()
            .unwrap()
            .iter()
            .all(|item| item.envelope_id != queued.envelope_id)
    );
    let replay = client.prepare_attachment_retention_operations(1).unwrap();
    assert_eq!(replay.len(), 1);
    assert_eq!(replay[0].operation, AttachmentUseOperationKind::Register);
    assert_eq!(replay[0].use_id, registration.use_id);
    client
        .apply_attachment_retention_response(&response(
            &registration,
            AttachmentUseState::Registered,
        ))
        .unwrap();
    assert!(
        client
            .pending_outbox()
            .unwrap()
            .iter()
            .any(|item| item.envelope_id == queued.envelope_id)
    );
}
