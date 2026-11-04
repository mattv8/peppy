//! Durable, local-only provider acquisition before an immutable MMS event is published.
use crate::common::*;
use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};
use tempfile::TempDir;

fn input(id: &str, direction: Direction) -> MmsAcquisitionInput {
    MmsAcquisitionInput {
        source: MmsSource {
            source_generation: "phone-install-a".into(),
            subscription_id: "sim-1".into(),
            provider_message_id: id.into(),
            provider_thread_id: Some("thread-7".into()),
        },
        direction,
        sender_address: if direction == Direction::Incoming {
            Some(ADDRESS.into())
        } else {
            None
        },
        recipients: if direction == Direction::Outgoing {
            vec![ADDRESS.into()]
        } else {
            Vec::new()
        },
        subject: Some("subject".into()),
        body: "hello".into(),
        imported: false,
        observed_at_ms: 1,
        is_sent_box: true,
        transaction_id: None,
    }
}

fn attachment(client: &Client, dir: &TempDir, name: &str) -> AttachmentId {
    let path = dir.path().join(name);
    fs::write(&path, b"MMS bytes").unwrap();
    client
        .prepare_attachment(&path, "image/png", name)
        .unwrap()
        .attachment_id
}

fn notification_reply_fixture(
    name: &str,
    body: &str,
    transport: Transport,
) -> (TempDir, Client, Client, QueuedSend) {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let desktop_cfg = config(&dir, &format!("desktop-{name}"), &vault);
    let gateway_cfg = config(&dir, &format!("gateway-{name}"), &vault);
    let desktop = unlocked(&desktop_cfg, &vault);
    let gateway = unlocked(&gateway_cfg, &vault);
    let conversation = ConversationId::new();
    let queued = match transport {
        Transport::Sms => desktop
            .queue_send(sms(conversation, body), route(&gateway_cfg))
            .unwrap(),
        Transport::Mms => desktop
            .queue_mms(
                OutgoingMms {
                    conversation_id: conversation,
                    recipients: vec![ADDRESS.into()],
                    body: body.into(),
                    attachment_ids: vec![],
                    subject: None,
                },
                route(&gateway_cfg),
            )
            .unwrap(),
        _ => unreachable!("notification replies support SMS and MMS commands only"),
    };
    let mut server = Server::default();
    server.upload(&desktop);
    assert_eq!(server.sync(&gateway).applied, 1);
    (dir, desktop, gateway, queued)
}

fn submit_notification_reply(gateway: &Client, command_id: CommandId, submitted_at_ms: i64) {
    assert!(matches!(
        gateway.begin_send_attempt(command_id).unwrap(),
        PermitDecision::Permit(_)
    ));
    gateway
        .record_notification_reply_submission(command_id, submitted_at_ms)
        .unwrap();
}

fn notification_reply_provider(id: &str, body: &str, observed_at_ms: i64) -> MmsAcquisitionInput {
    let mut provider = input(id, Direction::Outgoing);
    provider.transaction_id = Some("proto:google-message".into());
    provider.body = body.into();
    provider.observed_at_ms = observed_at_ms;
    provider
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis()
        .try_into()
        .unwrap()
}

#[test]
fn acquisition_parts_complete_once_and_survives_reopen() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let cfg = config(&dir, "gateway", &vault);
    let client = unlocked(&cfg, &vault);
    let acquisition = client
        .begin_mms_acquisition(input("100", Direction::Incoming))
        .unwrap();
    let part = attachment(&client, &dir, "one.png");
    client
        .set_mms_acquisition_part(&acquisition.acquisition_id, "part-1", part)
        .unwrap();
    let stored_parts = client
        .mms_acquisition_parts(&acquisition.acquisition_id)
        .unwrap();
    assert_eq!(stored_parts.len(), 1);
    assert_eq!(stored_parts[0].provider_part_id, "part-1");
    assert_eq!(stored_parts[0].attachment_id, part);
    assert_eq!(client.mms_pending_media_bytes().unwrap(), 9);
    let completed = client
        .complete_mms_acquisition(&acquisition.acquisition_id)
        .unwrap();
    assert!(!completed.duplicate);
    assert_eq!(
        client
            .complete_mms_acquisition(&acquisition.acquisition_id)
            .unwrap(),
        Captured {
            duplicate: true,
            ..completed.clone()
        }
    );
    assert!(client.mms_acquisitions(10).unwrap().is_empty());
    assert_eq!(
        client
            .mms_acquisition_parts(&acquisition.acquisition_id)
            .unwrap()[0]
            .attachment_id,
        part
    );
    drop(client);
    let reopened = unlocked(&cfg, &vault);
    assert_eq!(
        reopened
            .complete_mms_acquisition(&acquisition.acquisition_id)
            .unwrap(),
        Captured {
            duplicate: true,
            ..completed
        }
    );
}

#[test]
fn part_mapping_reopens_before_complete_and_is_idempotent_after_complete() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let cfg = config(&dir, "gateway", &vault);
    let client = unlocked(&cfg, &vault);
    let acquisition = client
        .begin_mms_acquisition(input("reopen", Direction::Incoming))
        .unwrap();
    let part = attachment(&client, &dir, "reopen.png");
    client
        .set_mms_acquisition_part(&acquisition.acquisition_id, "1", part)
        .unwrap();
    drop(client);
    let client = unlocked(&cfg, &vault);
    assert_eq!(
        client
            .mms_acquisition_parts(&acquisition.acquisition_id)
            .unwrap()[0]
            .attachment_id,
        part
    );
    client
        .set_mms_acquisition_part(&acquisition.acquisition_id, "1", part)
        .unwrap();
    client
        .complete_mms_acquisition(&acquisition.acquisition_id)
        .unwrap();
    client
        .set_mms_acquisition_part(&acquisition.acquisition_id, "1", part)
        .unwrap();
}

#[test]
fn mapped_parts_preserve_order_when_retried_as_an_ordered_prefix() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "gateway", &vault), &vault);
    let acquisition = client
        .begin_mms_acquisition(input("parts", Direction::Incoming))
        .unwrap();
    let first = attachment(&client, &dir, "first.png");
    let second = attachment(&client, &dir, "second.png");
    // A failed copy of part 2 prevents mapping it; retry part 2 before any later part.
    client
        .set_mms_acquisition_part(&acquisition.acquisition_id, "1", first)
        .unwrap();
    client
        .set_mms_acquisition_part(&acquisition.acquisition_id, "2", second)
        .unwrap();
    assert_eq!(
        client
            .mms_acquisition_parts(&acquisition.acquisition_id)
            .unwrap()
            .iter()
            .map(|part| part.provider_part_id.as_str())
            .collect::<Vec<_>>(),
        vec!["1", "2"]
    );
}

#[test]
fn terminal_acquisitions_reject_new_part_mappings_without_consuming_quota() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "gateway", &vault), &vault);
    let blocked = client
        .begin_mms_acquisition(input("blocked-late-part", Direction::Incoming))
        .unwrap();
    let unavailable = client
        .begin_mms_acquisition(input("unavailable-late-part", Direction::Incoming))
        .unwrap();
    client
        .set_mms_acquisition_state(
            &blocked.acquisition_id,
            MmsAcquisitionState::Blocked,
            Some("policy"),
        )
        .unwrap();
    client
        .set_mms_acquisition_state(
            &unavailable.acquisition_id,
            MmsAcquisitionState::Unavailable,
            Some("provider missing"),
        )
        .unwrap();
    let before_quota = client.mms_pending_media_bytes().unwrap();
    let part = attachment(&client, &dir, "late.png");
    for acquisition in [&blocked, &unavailable] {
        assert!(matches!(
            client.set_mms_acquisition_part(&acquisition.acquisition_id, "late-part", part),
            Err(Error::Conflict)
        ));
        assert!(
            client
                .mms_acquisition_parts(&acquisition.acquisition_id)
                .unwrap()
                .is_empty()
        );
    }
    assert_eq!(client.mms_pending_media_bytes().unwrap(), before_quota);
}

#[test]
fn list_matches_repeated_source_lookup_with_state_reason_and_attachments() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "gateway", &vault), &vault);
    let acquisition = client
        .begin_mms_acquisition(input("lookup-list", Direction::Incoming))
        .unwrap();
    let [first, second] = [
        attachment(&client, &dir, "lookup-first.png"),
        attachment(&client, &dir, "lookup-second.png"),
    ];
    client
        .set_mms_acquisition_part(&acquisition.acquisition_id, "first", first)
        .unwrap();
    client
        .set_mms_acquisition_part(&acquisition.acquisition_id, "second", second)
        .unwrap();
    client
        .set_mms_acquisition_state(
            &acquisition.acquisition_id,
            MmsAcquisitionState::Pending,
            Some("waiting for provider"),
        )
        .unwrap();

    let by_source = client
        .begin_mms_acquisition(input("lookup-list", Direction::Incoming))
        .unwrap();
    let listed = client
        .mms_acquisitions(MAX_MMS_ACQUISITIONS)
        .unwrap()
        .into_iter()
        .find(|item| item.acquisition_id == acquisition.acquisition_id)
        .unwrap();

    assert_eq!(by_source, listed);
    assert_eq!(listed.state, MmsAcquisitionState::Pending);
    assert_eq!(listed.reason.as_deref(), Some("waiting for provider"));
    assert_eq!(listed.attachment_ids, vec![first, second]);
}

#[test]
fn source_thread_and_sim_keep_conversations_isolated() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "gateway", &vault), &vault);
    let first = client
        .begin_mms_acquisition(input("1", Direction::Incoming))
        .unwrap();
    let same_thread = client
        .begin_mms_acquisition(input("2", Direction::Incoming))
        .unwrap();
    let mut other_sim = input("3", Direction::Incoming);
    other_sim.source.subscription_id = "sim-2".into();
    let other_sim = client.begin_mms_acquisition(other_sim).unwrap();
    let mut no_thread = input("4", Direction::Incoming);
    no_thread.source.provider_thread_id = None;
    let no_thread = client.begin_mms_acquisition(no_thread).unwrap();
    assert_eq!(first.conversation_id, same_thread.conversation_id);
    assert_ne!(first.conversation_id, other_sim.conversation_id);
    assert_ne!(first.conversation_id, no_thread.conversation_id);
}

#[test]
fn source_and_sim_namespace_same_provider_id_on_completion() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "gateway", &vault), &vault);
    let first = client
        .begin_mms_acquisition(input("same", Direction::Incoming))
        .unwrap();
    let mut other = input("same", Direction::Incoming);
    other.source.subscription_id = "sim-2".into();
    other.source.source_generation = "phone-install-b".into();
    let second = client.begin_mms_acquisition(other).unwrap();
    assert_ne!(first.conversation_id, second.conversation_id);
    client
        .complete_mms_acquisition(&first.acquisition_id)
        .unwrap();
    client
        .complete_mms_acquisition(&second.acquisition_id)
        .unwrap();
}

#[test]
fn thread_anchor_is_immutable_across_late_provider_metadata() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "gateway", &vault), &vault);
    let mut initial = input("anchor", Direction::Incoming);
    initial.source.provider_thread_id = None;
    let acquisition = client.begin_mms_acquisition(initial).unwrap();
    let mut late = input("anchor", Direction::Incoming);
    late.source.provider_thread_id = Some("thread-new".into());
    let updated = client.begin_mms_acquisition(late).unwrap();
    assert_eq!(updated.conversation_id, acquisition.conversation_id);
    assert_eq!(updated.input.source.provider_thread_id, None);
    let threaded = client
        .begin_mms_acquisition(input("threaded", Direction::Incoming))
        .unwrap();
    let mut conflicting = input("threaded", Direction::Incoming);
    conflicting.source.provider_thread_id = Some("different".into());
    assert_eq!(
        client.begin_mms_acquisition(conflicting),
        Err(Error::Conflict)
    );
    assert_eq!(
        client
            .mms_acquisitions(10)
            .unwrap()
            .iter()
            .find(|row| row.acquisition_id == threaded.acquisition_id)
            .unwrap()
            .conversation_id,
        threaded.conversation_id
    );
}

#[test]
fn pending_begin_updates_but_complete_is_immutable_and_outgoing_is_seen_without_command() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "gateway", &vault), &vault);
    let initial = client
        .begin_mms_acquisition(input("10", Direction::Outgoing))
        .unwrap();
    let mut revised = input("10", Direction::Outgoing);
    revised.body = "revised".into();
    assert_eq!(
        client
            .begin_mms_acquisition(revised.clone())
            .unwrap()
            .acquisition_id,
        initial.acquisition_id
    );
    let complete = client
        .complete_mms_acquisition(&initial.acquisition_id)
        .unwrap();
    let mut late = revised;
    late.body = "must not replace".into();
    assert_eq!(
        client.begin_mms_acquisition(late).unwrap().acquisition_id,
        initial.acquisition_id
    );
    let message = only_message(&client, complete.conversation_id);
    assert_eq!(message.payload.body, "revised");
    assert!(message.seen);
    assert_eq!(message.send_state, None);
    assert!(client.pending_commands().unwrap().is_empty());
}

#[test]
fn incomplete_provider_metadata_is_local_until_it_becomes_well_formed() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "gateway", &vault), &vault);
    let mut incomplete = input("partial", Direction::Incoming);
    incomplete.sender_address = None;
    incomplete.body.clear();
    let acquisition = client.begin_mms_acquisition(incomplete.clone()).unwrap();
    assert_eq!(
        client.complete_mms_acquisition(&acquisition.acquisition_id),
        Err(Error::InvalidRequest("MMS sender"))
    );
    incomplete.sender_address = Some(ADDRESS.into());
    incomplete.body = "now complete".into();
    assert_eq!(
        client
            .begin_mms_acquisition(incomplete)
            .unwrap()
            .acquisition_id,
        acquisition.acquisition_id
    );
    client
        .complete_mms_acquisition(&acquisition.acquisition_id)
        .unwrap();
}

#[test]
fn checkpoint_is_numeric_monotonic_and_pending_acquisitions_stay_local() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "gateway", &vault), &vault);
    client
        .set_mms_scan_checkpoint("generation", "sim", false, "9")
        .unwrap();
    client
        .set_mms_scan_checkpoint("generation", "sim", false, "10")
        .unwrap();
    client
        .set_mms_scan_checkpoint("generation", "sim", false, "2")
        .unwrap();
    assert_eq!(
        client
            .mms_scan_checkpoint("generation", "sim", false)
            .unwrap()
            .as_deref(),
        Some("10")
    );
    client
        .begin_mms_acquisition(input("unsealed", Direction::Incoming))
        .unwrap();
    assert!(client.pending_outbox().unwrap().is_empty());
    assert_eq!(
        client.set_mms_scan_checkpoint("generation", "sim", false, "not-a-number"),
        Err(Error::InvalidRequest("MMS checkpoint"))
    );
    assert_eq!(
        client.set_mms_scan_checkpoint("generation", "sim", false, "18446744073709551616"),
        Err(Error::InvalidRequest("MMS checkpoint"))
    );
    client
        .set_mms_scan_checkpoint("generation", "sim", true, "1")
        .unwrap();
    assert_eq!(
        client
            .mms_scan_checkpoint("generation", "sim", true)
            .unwrap()
            .as_deref(),
        Some("1")
    );
}

#[test]
fn imported_capture_is_seen_and_completion_does_not_create_a_command() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "gateway", &vault), &vault);
    let mut historical = input("history", Direction::Incoming);
    historical.imported = true;
    let acquisition = client.begin_mms_acquisition(historical).unwrap();
    let completed = client
        .complete_mms_acquisition(&acquisition.acquisition_id)
        .unwrap();
    assert!(only_message(&client, completed.conversation_id).seen);
    assert!(client.pending_commands().unwrap().is_empty());
}

#[test]
fn reserved_mms_sequence_survives_interleaved_sms_without_address_hijack() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "gateway", &vault), &vault);
    let mut group = input("reserved", Direction::Incoming);
    group.recipients = vec!["+15555550101".into()];
    let acquisition = client.begin_mms_acquisition(group).unwrap();
    client
        .capture_incoming(IncomingSms {
            conversation_id: Some(ConversationId::new()),
            sender_address: ADDRESS.into(),
            body: "separate SMS".into(),
            provider_message_id: Some("sms-interleaved".into()),
            imported: false,
        })
        .unwrap();
    assert!(
        !client
            .complete_mms_acquisition(&acquisition.acquisition_id)
            .unwrap()
            .duplicate
    );
}

#[test]
fn text_only_group_and_attachment_only_mms_complete() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "gateway", &vault), &vault);
    let mut group = input("group", Direction::Incoming);
    group.recipients = vec!["+15555550101".into(), "+15555550102".into()];
    let group = client.begin_mms_acquisition(group).unwrap();
    client
        .complete_mms_acquisition(&group.acquisition_id)
        .unwrap();
    let mut media_only = input("media", Direction::Incoming);
    media_only.body.clear();
    let media_only = client.begin_mms_acquisition(media_only).unwrap();
    let part = attachment(&client, &dir, "media-only.png");
    client
        .set_mms_acquisition_part(&media_only.acquisition_id, "1", part)
        .unwrap();
    client
        .complete_mms_acquisition(&media_only.acquisition_id)
        .unwrap();
}

#[test]
fn acquisition_count_limit_rejects_without_eviction() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "gateway", &vault), &vault);
    for number in 0..1_000 {
        client
            .begin_mms_acquisition(input(&number.to_string(), Direction::Incoming))
            .unwrap();
    }
    assert_eq!(
        client.begin_mms_acquisition(input("overflow", Direction::Incoming)),
        Err(Error::MmsAcquisitionLimit)
    );
    assert_eq!(client.mms_acquisitions(1_001).unwrap().len(), 1_000);
}

#[test]
fn checked_compose_transport_uses_latest_message_and_rejects_a_stale_classification() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let gateway_cfg = config(&dir, "gateway-route", &vault);
    let client = unlocked(&config(&dir, "desktop", &vault), &vault);
    let conversation = ConversationId::new();
    client
        .capture_incoming_mms(IncomingMms {
            conversation_id: Some(conversation),
            sender_address: ADDRESS.into(),
            body: "old picture".into(),
            provider_message_id: Some("old-mms".into()),
            imported: false,
            attachment_ids: vec![],
            recipients: vec![],
            subject: None,
        })
        .unwrap();
    client
        .capture_incoming(IncomingSms {
            conversation_id: Some(conversation),
            sender_address: ADDRESS.into(),
            body: "latest sms".into(),
            provider_message_id: Some("latest-sms".into()),
            imported: false,
        })
        .unwrap();
    let draft = client.create_compose_draft(Some(conversation)).unwrap();
    let draft = client
        .save_compose_draft(
            draft.draft_id,
            draft.revision,
            ComposeDraftUpdate {
                text: "reply".into(),
                recipients: vec![ADDRESS.into()],
                attachment_ids: vec![],
                route: Some(route(&gateway_cfg)),
            },
        )
        .unwrap();
    assert_eq!(
        client.send_compose_draft_checked_transport(draft.draft_id, draft.revision, Transport::Mms),
        Err(Error::InvalidRequest("compose transport changed"))
    );
    let queued = client
        .send_compose_draft_checked_transport(draft.draft_id, draft.revision, Transport::Sms)
        .unwrap();
    assert_eq!(
        client
            .messages(conversation)
            .unwrap()
            .iter()
            .find(|m| m.payload.record.message_id == queued.message_id)
            .unwrap()
            .payload
            .transport,
        Transport::Sms
    );
}

#[test]
fn queue_mms_rejects_an_oversized_subject() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let gateway_cfg = config(&dir, "gateway-route", &vault);
    let client = unlocked(&config(&dir, "desktop", &vault), &vault);
    assert_eq!(
        client.queue_mms(
            OutgoingMms {
                conversation_id: ConversationId::new(),
                recipients: vec![ADDRESS.into()],
                body: "body".into(),
                attachment_ids: vec![],
                subject: Some("x".repeat(65_537)),
            },
            route(&gateway_cfg)
        ),
        Err(Error::InvalidRequest("subject"))
    );
}

#[test]
fn sent_provider_acquisition_reconciles_exact_transaction_to_existing_mms() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let desktop_cfg = config(&dir, "desktop-send", &vault);
    let gateway_cfg = config(&dir, "gateway-send", &vault);
    let desktop = unlocked(&desktop_cfg, &vault);
    let gateway = unlocked(&gateway_cfg, &vault);
    let conversation = ConversationId::new();
    let queued = desktop
        .queue_mms(
            OutgoingMms {
                conversation_id: conversation,
                recipients: vec![ADDRESS.into()],
                body: "sent once".into(),
                attachment_ids: vec![],
                subject: Some("subject".into()),
            },
            route(&gateway_cfg),
        )
        .unwrap();
    let mut server = Server::default();
    server.upload(&desktop);
    assert_eq!(server.sync(&gateway).applied, 1);
    assert!(matches!(
        gateway.begin_send_attempt(queued.command_id).unwrap(),
        PermitDecision::Permit(_)
    ));
    gateway
        .record_send_result(queued.command_id, SendResult::Sent)
        .unwrap();

    let mut provider = input("provider-sent", Direction::Outgoing);
    provider.transaction_id = Some(format!("peppy-{}", queued.command_id));
    let acquisition = gateway.begin_mms_acquisition(provider).unwrap();
    let outbox_before_reconciliation = gateway.pending_outbox().unwrap().len();
    let captured = gateway
        .complete_mms_acquisition(&acquisition.acquisition_id)
        .unwrap();
    assert_eq!(captured.message_id, queued.message_id);
    assert_eq!(captured.conversation_id, conversation);
    let messages = gateway.messages(conversation).unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].payload.provider_message_id, None);
    assert_eq!(messages[0].payload.mms_context, None);

    let snapshot = gateway
        .begin_snapshot(Cursor(1), 1, SnapshotPurpose::Resync)
        .unwrap();
    gateway
        .append_snapshot_page(
            snapshot.generation,
            &[SnapshotRecord {
                cursor: Cursor(1),
                envelope: server.log[0].clone(),
            }],
        )
        .unwrap();
    gateway.finish_snapshot(snapshot.generation).unwrap();
    let replay = gateway.apply_pending(100).unwrap();
    assert_eq!((replay.applied, replay.quarantined), (0, 0));

    assert_eq!(
        gateway.pending_outbox().unwrap().len(),
        outbox_before_reconciliation,
        "reconciliation must not publish another message or command"
    );
}

#[test]
fn notification_reply_submission_reconciles_an_outgoing_provider_row_once() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let desktop_cfg = config(&dir, "desktop-notification-reply", &vault);
    let gateway_cfg = config(&dir, "gateway-notification-reply", &vault);
    let desktop = unlocked(&desktop_cfg, &vault);
    let gateway = unlocked(&gateway_cfg, &vault);
    let conversation = ConversationId::new();
    let queued = desktop
        .queue_mms(
            OutgoingMms {
                conversation_id: conversation,
                recipients: vec![ADDRESS.into()],
                body: "  caf\u{00e9}  ".into(),
                attachment_ids: vec![],
                subject: None,
            },
            route(&gateway_cfg),
        )
        .unwrap();
    let mut server = Server::default();
    server.upload(&desktop);
    assert_eq!(server.sync(&gateway).applied, 1);
    assert!(matches!(
        gateway.begin_send_attempt(queued.command_id).unwrap(),
        PermitDecision::Permit(_)
    ));
    let submitted_at_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis()
        .try_into()
        .unwrap();
    gateway
        .record_notification_reply_submission(
            queued.command_id,
            submitted_at_ms - 24 * 60 * 60 * 1000 - 1,
        )
        .unwrap();
    gateway
        .record_notification_reply_submission(queued.command_id, submitted_at_ms)
        .unwrap();
    gateway
        .record_notification_reply_submission(queued.command_id, submitted_at_ms)
        .unwrap();

    let mut provider = input("notification-reply", Direction::Outgoing);
    provider.transaction_id = Some("proto:google-message".into());
    provider.subject = Some("unrelated provider subject".into());
    provider.body = "cafe\u{301}".into();
    provider.observed_at_ms = submitted_at_ms + 1;
    let acquisition = gateway.begin_mms_acquisition(provider).unwrap();

    assert_eq!(acquisition.state, MmsAcquisitionState::Complete);
    assert_eq!(acquisition.conversation_id, conversation);
    assert_eq!(
        gateway
            .complete_mms_acquisition(&acquisition.acquisition_id)
            .unwrap()
            .message_id,
        queued.message_id
    );
    assert_eq!(gateway.messages(conversation).unwrap().len(), 1);
    assert!(matches!(
        gateway.begin_send_attempt(queued.command_id).unwrap(),
        PermitDecision::AlreadyAttempted(SendState::Sent)
    ));
}

#[test]
fn notification_reply_submission_requires_a_gateway_permit() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let desktop_cfg = config(&dir, "desktop-notification-permit", &vault);
    let gateway_cfg = config(&dir, "gateway-notification-permit", &vault);
    let desktop = unlocked(&desktop_cfg, &vault);
    let gateway = unlocked(&gateway_cfg, &vault);
    let queued = desktop
        .queue_mms(
            OutgoingMms {
                conversation_id: ConversationId::new(),
                recipients: vec![ADDRESS.into()],
                body: "needs permit".into(),
                attachment_ids: vec![],
                subject: None,
            },
            route(&gateway_cfg),
        )
        .unwrap();
    let mut server = Server::default();
    server.upload(&desktop);
    server.sync(&gateway);

    assert_eq!(
        gateway.record_notification_reply_submission(queued.command_id, i64::MAX),
        Err(Error::InvalidRequest(
            "notification reply submission requires an active send permit"
        ))
    );
}

#[test]
fn canonical_phone_e164_is_strict_and_uses_the_core_region() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "canonical-phone", &vault), &vault);
    assert_eq!(
        client.canonical_phone_e164("+1 202-555-0100").unwrap(),
        client.canonical_phone_e164("(202) 555-0100").unwrap()
    );
    assert_eq!(client.canonical_phone_e164("5550100").unwrap(), None);
    assert_ne!(
        client.canonical_phone_e164("(202) 555-0100").unwrap(),
        client.canonical_phone_e164("(303) 555-0100").unwrap()
    );
    assert_eq!(client.canonical_phone_e164("12345").unwrap(), None);
    assert_eq!(
        client.canonical_phone_e164("person@example.com").unwrap(),
        None
    );
}

#[test]
fn notification_reply_rows_must_be_sent_and_unresolved() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let desktop_cfg = config(&dir, "desktop-notification-terminal", &vault);
    let gateway_cfg = config(&dir, "gateway-notification-terminal", &vault);
    let desktop = unlocked(&desktop_cfg, &vault);
    let gateway = unlocked(&gateway_cfg, &vault);
    let queued = desktop
        .queue_mms(
            OutgoingMms {
                conversation_id: ConversationId::new(),
                recipients: vec![ADDRESS.into()],
                body: "terminal evidence".into(),
                attachment_ids: vec![],
                subject: None,
            },
            route(&gateway_cfg),
        )
        .unwrap();
    let mut server = Server::default();
    server.upload(&desktop);
    server.sync(&gateway);
    assert!(matches!(
        gateway.begin_send_attempt(queued.command_id).unwrap(),
        PermitDecision::Permit(_)
    ));
    let now: i64 = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis()
        .try_into()
        .unwrap();
    gateway
        .record_notification_reply_submission(queued.command_id, now)
        .unwrap();
    gateway
        .record_send_result(queued.command_id, SendResult::FailedBeforeSubmission)
        .unwrap();
    let mut provider = input("terminal-notification-reply", Direction::Outgoing);
    provider.body = "terminal evidence".into();
    provider.observed_at_ms = now;
    provider.is_sent_box = false;
    assert_eq!(
        gateway.begin_mms_acquisition(provider).unwrap().state,
        MmsAcquisitionState::Pending
    );
}

#[test]
fn notification_reply_submission_does_not_reconcile_ambiguous_or_different_bodies() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let desktop_cfg = config(&dir, "desktop-notification-ambiguous", &vault);
    let gateway_cfg = config(&dir, "gateway-notification-ambiguous", &vault);
    let desktop = unlocked(&desktop_cfg, &vault);
    let gateway = unlocked(&gateway_cfg, &vault);
    let conversation = ConversationId::new();
    let mut commands = Vec::new();
    for _ in 0..2 {
        commands.push(
            desktop
                .queue_mms(
                    OutgoingMms {
                        conversation_id: conversation,
                        recipients: vec![ADDRESS.into()],
                        body: "same body".into(),
                        attachment_ids: vec![],
                        subject: None,
                    },
                    route(&gateway_cfg),
                )
                .unwrap(),
        );
    }
    let mut server = Server::default();
    server.upload(&desktop);
    assert_eq!(server.sync(&gateway).applied, 2);
    let submitted_at_ms: i64 = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis()
        .try_into()
        .unwrap();
    for command in commands {
        assert!(matches!(
            gateway.begin_send_attempt(command.command_id).unwrap(),
            PermitDecision::Permit(_)
        ));
        gateway
            .record_notification_reply_submission(command.command_id, submitted_at_ms)
            .unwrap();
    }

    let mut provider = input("ambiguous-notification-reply", Direction::Outgoing);
    provider.transaction_id = Some("proto:google-message".into());
    provider.body = "same body".into();
    provider.observed_at_ms = submitted_at_ms;
    let acquisition = gateway.begin_mms_acquisition(provider).unwrap();
    assert_eq!(acquisition.state, MmsAcquisitionState::Pending);

    let mut different = input("different-notification-reply", Direction::Outgoing);
    different.transaction_id = Some("proto:google-message".into());
    different.body = "different body".into();
    different.observed_at_ms = submitted_at_ms;
    assert_eq!(
        gateway.begin_mms_acquisition(different).unwrap().state,
        MmsAcquisitionState::Pending
    );
}

#[test]
fn notification_reply_reconciliation_honors_provider_date_window_boundaries() {
    // MmsAcquisitionInput exposes provider timestamps in milliseconds, so these tests can cover
    // the exact inclusive bounds and the adjacent millisecond without provider-date rounding.
    let submitted_at_ms = now_ms();
    for (name, offset_ms, matches_submission) in [
        ("lower-bound", -5_000, true),
        ("before-lower-bound", -5_001, false),
        ("upper-bound", 10 * 60 * 1000, true),
        ("after-upper-bound", 10 * 60 * 1000 + 1, false),
    ] {
        let (_dir, _desktop, gateway, queued) =
            notification_reply_fixture(name, name, Transport::Mms);
        submit_notification_reply(&gateway, queued.command_id, submitted_at_ms);
        let acquisition = gateway
            .begin_mms_acquisition(notification_reply_provider(
                name,
                name,
                submitted_at_ms + offset_ms,
            ))
            .unwrap();

        if matches_submission {
            assert_eq!(acquisition.state, MmsAcquisitionState::Complete, "{name}");
            assert_eq!(
                gateway
                    .complete_mms_acquisition(&acquisition.acquisition_id)
                    .unwrap()
                    .message_id,
                queued.message_id,
                "{name}"
            );
        } else {
            assert_eq!(acquisition.state, MmsAcquisitionState::Pending, "{name}");
            assert_ne!(
                gateway
                    .complete_mms_acquisition(&acquisition.acquisition_id)
                    .unwrap()
                    .message_id,
                queued.message_id,
                "{name}"
            );
            assert!(matches!(
                gateway.begin_send_attempt(queued.command_id).unwrap(),
                PermitDecision::AlreadyAttempted(SendState::AttemptRecorded)
            ));
        }
    }
}

#[test]
fn notification_reply_does_not_reconcile_a_same_body_row_from_another_conversation() {
    let submitted_at_ms = now_ms();
    let (_dir, _desktop, gateway, queued) =
        notification_reply_fixture("wrong-conversation", "same body", Transport::Mms);
    submit_notification_reply(&gateway, queued.command_id, submitted_at_ms);
    let mut provider =
        notification_reply_provider("wrong-conversation", "same body", submitted_at_ms);
    provider.source.provider_thread_id = Some("thread-b".into());
    provider.recipients = vec!["+15555550101".into()];
    let acquisition = gateway.begin_mms_acquisition(provider).unwrap();

    assert_eq!(acquisition.state, MmsAcquisitionState::Pending);
    let captured = gateway
        .complete_mms_acquisition(&acquisition.acquisition_id)
        .unwrap();
    assert_ne!(captured.message_id, queued.message_id);
    assert_eq!(gateway.messages(captured.conversation_id).unwrap().len(), 1);
    assert!(matches!(
        gateway.begin_send_attempt(queued.command_id).unwrap(),
        PermitDecision::AlreadyAttempted(SendState::AttemptRecorded)
    ));
}

#[test]
fn notification_reply_reconciles_an_sms_command_with_a_matching_provider_row() {
    let submitted_at_ms = now_ms();
    let (_dir, _desktop, gateway, queued) =
        notification_reply_fixture("sms-command", "SMS reply", Transport::Sms);
    submit_notification_reply(&gateway, queued.command_id, submitted_at_ms);
    let acquisition = gateway
        .begin_mms_acquisition(notification_reply_provider(
            "sms-command",
            "SMS reply",
            submitted_at_ms,
        ))
        .unwrap();

    assert_eq!(acquisition.state, MmsAcquisitionState::Complete);
    assert_eq!(
        gateway
            .complete_mms_acquisition(&acquisition.acquisition_id)
            .unwrap()
            .message_id,
        queued.message_id
    );
    assert_eq!(
        gateway.messages(acquisition.conversation_id).unwrap().len(),
        1
    );
    assert!(matches!(
        gateway.begin_send_attempt(queued.command_id).unwrap(),
        PermitDecision::AlreadyAttempted(SendState::Sent)
    ));
}

#[test]
fn terminal_notification_reply_results_remove_submission_and_do_not_wedge_capture() {
    for (name, result) in [
        (
            "failed-before-submission",
            SendResult::FailedBeforeSubmission,
        ),
        ("failed-confirmed", SendResult::FailedConfirmed),
    ] {
        let submitted_at_ms = now_ms();
        let (_dir, _desktop, gateway, queued) =
            notification_reply_fixture(name, "terminal reply", Transport::Mms);
        submit_notification_reply(&gateway, queued.command_id, submitted_at_ms);
        gateway
            .record_send_result(queued.command_id, result)
            .unwrap();
        let acquisition = gateway
            .begin_mms_acquisition(notification_reply_provider(
                name,
                "terminal reply",
                submitted_at_ms,
            ))
            .unwrap();

        assert_eq!(acquisition.state, MmsAcquisitionState::Pending, "{name}");
        assert_ne!(
            gateway
                .complete_mms_acquisition(&acquisition.acquisition_id)
                .unwrap()
                .message_id,
            queued.message_id,
            "{name}"
        );
    }
}

#[test]
fn second_identical_notification_reply_provider_row_creates_a_new_message() {
    let submitted_at_ms = now_ms();
    let (_dir, _desktop, gateway, queued) =
        notification_reply_fixture("second-row", "same reply", Transport::Mms);
    submit_notification_reply(&gateway, queued.command_id, submitted_at_ms);
    let first = gateway
        .begin_mms_acquisition(notification_reply_provider(
            "first-row",
            "same reply",
            submitted_at_ms,
        ))
        .unwrap();
    assert_eq!(first.state, MmsAcquisitionState::Complete);
    gateway
        .complete_mms_acquisition(&first.acquisition_id)
        .unwrap();

    let second = gateway
        .begin_mms_acquisition(notification_reply_provider(
            "second-row",
            "same reply",
            submitted_at_ms,
        ))
        .unwrap();
    assert_eq!(second.state, MmsAcquisitionState::Pending);
    assert_ne!(
        gateway
            .complete_mms_acquisition(&second.acquisition_id)
            .unwrap()
            .message_id,
        queued.message_id
    );
}

#[test]
fn notification_reply_does_not_reconcile_a_row_from_another_subscription() {
    let submitted_at_ms = now_ms();
    let (_dir, _desktop, gateway, queued) = notification_reply_fixture(
        "subscription-mismatch",
        "subscription reply",
        Transport::Mms,
    );
    submit_notification_reply(&gateway, queued.command_id, submitted_at_ms);
    let mut provider = notification_reply_provider(
        "subscription-mismatch",
        "subscription reply",
        submitted_at_ms,
    );
    provider.source.subscription_id = "sim-2".into();
    let acquisition = gateway.begin_mms_acquisition(provider).unwrap();

    assert_eq!(acquisition.state, MmsAcquisitionState::Pending);
    assert_ne!(
        gateway
            .complete_mms_acquisition(&acquisition.acquisition_id)
            .unwrap()
            .message_id,
        queued.message_id
    );
}

#[test]
fn notification_reply_does_not_reconcile_imported_provider_history() {
    let submitted_at_ms = now_ms();
    let (_dir, _desktop, gateway, queued) =
        notification_reply_fixture("imported-row", "history reply", Transport::Mms);
    submit_notification_reply(&gateway, queued.command_id, submitted_at_ms);
    let mut provider =
        notification_reply_provider("imported-row", "history reply", submitted_at_ms);
    provider.imported = true;
    let acquisition = gateway.begin_mms_acquisition(provider).unwrap();

    assert_eq!(acquisition.state, MmsAcquisitionState::Pending);
    assert_ne!(
        gateway
            .complete_mms_acquisition(&acquisition.acquisition_id)
            .unwrap()
            .message_id,
        queued.message_id
    );
}

#[test]
fn notification_reply_submission_rejects_submitted_and_unknown_commands() {
    let submitted_at_ms = now_ms();
    let (_dir, _desktop, gateway, queued) =
        notification_reply_fixture("submission-state", "state reply", Transport::Mms);
    submit_notification_reply(&gateway, queued.command_id, submitted_at_ms);
    gateway
        .record_send_result(queued.command_id, SendResult::Submitted)
        .unwrap();

    let expected = Err(Error::InvalidRequest(
        "notification reply submission requires an active send permit",
    ));
    assert_eq!(
        gateway.record_notification_reply_submission(queued.command_id, submitted_at_ms),
        expected
    );
    assert_eq!(
        gateway.record_notification_reply_submission(CommandId::new(), submitted_at_ms),
        expected
    );
}

#[test]
fn legacy_incoming_mms_capture_validates_recipients_and_subject() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "legacy-mms", &vault), &vault);
    let make = |recipients: Vec<String>, subject: Option<String>| IncomingMms {
        conversation_id: Some(ConversationId::new()),
        sender_address: ADDRESS.into(),
        body: "body".into(),
        provider_message_id: None,
        imported: false,
        attachment_ids: vec![],
        recipients,
        subject,
    };
    assert_eq!(
        client.capture_incoming_mms(make(vec!["x".repeat(257)], None)),
        Err(Error::InvalidRequest("address"))
    );
    assert_eq!(
        client.capture_incoming_mms(make(vec![], Some("x".repeat(65_537)))),
        Err(Error::InvalidRequest("subject"))
    );
}

#[test]
fn own_send_with_media_reconciles_at_begin_without_copy_or_payload_rewrite() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let desktop_cfg = config(&dir, "desktop-media-send", &vault);
    let gateway_cfg = config(&dir, "gateway-media-send", &vault);
    let desktop = unlocked(&desktop_cfg, &vault);
    let gateway = unlocked(&gateway_cfg, &vault);
    let source = dir.path().join("command-media.png");
    fs::write(&source, b"real command media").unwrap();
    let media = desktop
        .prepare_attachment(&source, "image/png", "command.png")
        .unwrap();
    let conversation = ConversationId::new();
    let queued = desktop
        .queue_mms(
            OutgoingMms {
                conversation_id: conversation,
                recipients: vec![ADDRESS.into()],
                body: "sent with media".into(),
                attachment_ids: vec![media.attachment_id],
                subject: None,
            },
            route(&gateway_cfg),
        )
        .unwrap();
    let ciphertext = fs::read(desktop.native_cipher_file(media.attachment_id).unwrap()).unwrap();
    desktop
        .mark_attachment_uploaded(media.attachment_id, &uuid::Uuid::new_v4().to_string())
        .unwrap();
    let mut server = Server::default();
    server.upload(&desktop);
    assert_eq!(server.sync(&gateway).applied, 1);
    let downloaded = dir.path().join("gateway-command-media.bin");
    fs::write(&downloaded, ciphertext).unwrap();
    gateway
        .install_downloaded_attachment(media.attachment_id, &downloaded)
        .unwrap();
    assert!(matches!(
        gateway.begin_send_attempt(queued.command_id).unwrap(),
        PermitDecision::Permit(_)
    ));
    gateway
        .record_send_result(queued.command_id, SendResult::Sent)
        .unwrap();

    let before_uploads = gateway.pending_uploads().unwrap();
    let before_quota = gateway.mms_pending_media_bytes().unwrap();
    let mut provider = input("provider-media-sent", Direction::Outgoing);
    provider.source.provider_thread_id = Some("provider-thread-media".into());
    provider.transaction_id = Some(format!("peppy-{}", queued.command_id));
    let acquisition = gateway.begin_mms_acquisition(provider).unwrap();
    assert_eq!(acquisition.state, MmsAcquisitionState::Complete);
    assert_eq!(
        gateway
            .complete_mms_acquisition(&acquisition.acquisition_id)
            .unwrap()
            .message_id,
        queued.message_id
    );
    assert_eq!(acquisition.conversation_id, conversation);
    assert!(acquisition.attachment_ids.is_empty());
    assert_eq!(gateway.mms_pending_media_bytes().unwrap(), before_quota);
    assert_eq!(gateway.pending_uploads().unwrap(), before_uploads);
    let original = only_message(&gateway, conversation);
    assert_eq!(original.payload.provider_message_id, None);
    assert_eq!(original.payload.mms_context, None);

    let mut reply = input("provider-incoming-reply", Direction::Incoming);
    reply.source.provider_thread_id = Some("provider-thread-media".into());
    assert_eq!(
        gateway
            .begin_mms_acquisition(reply)
            .unwrap()
            .conversation_id,
        conversation
    );
}

#[test]
fn command_media_demand_is_limited_to_the_live_designated_gateway() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let desktop_cfg = config(&dir, "demand-desktop", &vault);
    let gateway_cfg = config(&dir, "demand-gateway", &vault);
    let desktop = unlocked(&desktop_cfg, &vault);
    let gateway = unlocked(&gateway_cfg, &vault);
    let other = unlocked(&config(&dir, "demand-other", &vault), &vault);
    let source = dir.path().join("demand.png");
    fs::write(&source, b"demand media").unwrap();
    let media = desktop
        .prepare_attachment(&source, "image/png", "demand.png")
        .unwrap();
    desktop
        .queue_mms(
            OutgoingMms {
                conversation_id: ConversationId::new(),
                recipients: vec![ADDRESS.into()],
                body: String::new(),
                attachment_ids: vec![media.attachment_id],
                subject: None,
            },
            route(&gateway_cfg),
        )
        .unwrap();
    desktop
        .mark_attachment_uploaded(media.attachment_id, &uuid::Uuid::new_v4().to_string())
        .unwrap();
    let mut server = Server::default();
    server.upload(&desktop);
    assert_eq!(server.sync(&other).applied, 1);
    assert!(
        !other
            .attachment_download_requested(media.attachment_id)
            .unwrap()
    );
    assert_eq!(server.sync(&gateway).applied, 1);
    assert!(
        gateway
            .attachment_download_requested(media.attachment_id)
            .unwrap()
    );
}

#[test]
fn late_own_send_reconciliation_unlinks_and_discards_partial_copies() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let desktop_cfg = config(&dir, "desktop-late", &vault);
    let gateway_cfg = config(&dir, "gateway-late", &vault);
    let desktop = unlocked(&desktop_cfg, &vault);
    let gateway = unlocked(&gateway_cfg, &vault);
    let conversation = ConversationId::new();
    let queued = desktop
        .queue_mms(
            OutgoingMms {
                conversation_id: conversation,
                recipients: vec![ADDRESS.into()],
                body: "late evidence".into(),
                attachment_ids: vec![],
                subject: None,
            },
            route(&gateway_cfg),
        )
        .unwrap();
    let mut server = Server::default();
    server.upload(&desktop);
    server.sync(&gateway);

    let mut provider = input("partial-own", Direction::Outgoing);
    provider.source.provider_thread_id = Some("late-thread".into());
    let partial = gateway.begin_mms_acquisition(provider.clone()).unwrap();
    let mut waiting_reply = input("waiting-reply", Direction::Incoming);
    waiting_reply.source.provider_thread_id = Some("late-thread".into());
    let waiting = gateway.begin_mms_acquisition(waiting_reply).unwrap();
    assert_eq!(waiting.conversation_id, partial.conversation_id);

    let copied = attachment(&gateway, &dir, "copied-own.png");
    gateway
        .set_mms_acquisition_part(&partial.acquisition_id, "provider-part", copied)
        .unwrap();
    assert_eq!(gateway.mms_pending_media_bytes().unwrap(), 9);

    assert!(matches!(
        gateway.begin_send_attempt(queued.command_id).unwrap(),
        PermitDecision::Permit(_)
    ));
    provider.transaction_id = Some(format!("peppy-{}", queued.command_id));
    let linked = gateway.begin_mms_acquisition(provider).unwrap();
    assert_eq!(linked.state, MmsAcquisitionState::Complete);
    assert_eq!(linked.conversation_id, conversation);
    assert!(linked.attachment_ids.is_empty());
    assert_eq!(gateway.mms_pending_media_bytes().unwrap(), 0);
    assert_eq!(gateway.attachment_info(copied), Err(Error::NotFound));
    let migrated = gateway
        .mms_acquisitions(MAX_MMS_ACQUISITIONS)
        .unwrap()
        .into_iter()
        .find(|item| item.acquisition_id == waiting.acquisition_id)
        .unwrap();
    assert_eq!(migrated.conversation_id, conversation);

    let mut later_reply = input("later-reply", Direction::Incoming);
    later_reply.source.provider_thread_id = Some("late-thread".into());
    assert_eq!(
        gateway
            .begin_mms_acquisition(later_reply)
            .unwrap()
            .conversation_id,
        conversation
    );
}

#[test]
fn ambiguous_peppy_transaction_is_terminal_without_media_quota() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "ambiguous", &vault), &vault);
    let mut provider = input("ambiguous-provider", Direction::Outgoing);
    provider.transaction_id = Some(format!("peppy-{}", uuid::Uuid::new_v4()));
    let acquisition = client.begin_mms_acquisition(provider).unwrap();
    assert_eq!(acquisition.state, MmsAcquisitionState::Unavailable);
    assert_eq!(
        acquisition.reason.as_deref(),
        Some("transaction id reconciliation unavailable")
    );
    assert!(acquisition.attachment_ids.is_empty());
    assert_eq!(client.mms_pending_media_bytes().unwrap(), 0);
}

#[test]
fn terminal_outgoing_row_ignores_sent_box_flag_on_rescan() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "terminal-sent-box", &vault), &vault);
    let mut legacy = input("legacy-sent-box", Direction::Outgoing);
    legacy.transaction_id = Some(format!("peppy-{}", uuid::Uuid::new_v4()));
    legacy.is_sent_box = false;
    let terminal = client.begin_mms_acquisition(legacy.clone()).unwrap();
    assert_eq!(terminal.state, MmsAcquisitionState::Unavailable);

    legacy.is_sent_box = true;
    let MmsBeginOutcome::Acquired(rescanned) =
        client.begin_mms_acquisition_outcome(legacy).unwrap()
    else {
        panic!("terminal rescan must return the existing acquisition, not quarantine");
    };
    assert_eq!(rescanned.acquisition_id, terminal.acquisition_id);
    assert_eq!(rescanned.state, MmsAcquisitionState::Unavailable);
}

#[test]
fn terminal_history_retains_newest_observed_rows_without_consuming_active_capacity() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "terminal-history", &vault), &vault);
    let mut newest = input("newest-terminal", Direction::Incoming);
    newest.subject = Some("x".repeat(65_537));
    newest.observed_at_ms = 10_000;
    let newest_id = client
        .begin_mms_acquisition(newest.clone())
        .unwrap()
        .acquisition_id;
    let mut oldest_id = None;
    for number in 0..=MAX_MMS_ACQUISITIONS {
        let mut terminal = input(&format!("terminal-{number}"), Direction::Incoming);
        terminal.subject = Some("x".repeat(65_537));
        terminal.observed_at_ms = i64::try_from(number).unwrap() + 1;
        let acquisition = client.begin_mms_acquisition(terminal).unwrap();
        if number == 0 {
            oldest_id = Some(acquisition.acquisition_id);
        }
        assert_eq!(acquisition.state, MmsAcquisitionState::Unavailable);
    }
    let active = client
        .begin_mms_acquisition(input("new-active", Direction::Incoming))
        .unwrap();
    assert_eq!(active.state, MmsAcquisitionState::Pending);
    let visible = client.mms_acquisitions(MAX_MMS_ACQUISITIONS).unwrap();
    assert!(
        visible
            .iter()
            .any(|row| row.acquisition_id == active.acquisition_id)
    );
    assert_eq!(
        client.begin_mms_acquisition(newest).unwrap().acquisition_id,
        newest_id
    );
    assert!(visible.iter().any(|row| {
        row.input.source.provider_message_id == format!("terminal-{MAX_MMS_ACQUISITIONS}")
            && row.reason.as_deref() == Some("provider metadata unavailable")
    }));

    let mut first_again = input("terminal-0", Direction::Incoming);
    first_again.subject = Some("x".repeat(65_537));
    assert_ne!(
        client
            .begin_mms_acquisition(first_again)
            .unwrap()
            .acquisition_id,
        oldest_id.unwrap()
    );
}

#[test]
fn blocked_mapped_media_is_released_and_manual_recheck_recopies_once() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "blocked-mapped", &vault), &vault);
    let plans = client
        .plan_mms_backfill_scans("phone-install-a", "sim-1", "1")
        .unwrap();
    let history_bound = plans
        .iter()
        .find(|plan| plan.phase == MmsBackfillPhase::History)
        .unwrap()
        .history_upper_bound
        .clone()
        .unwrap();
    let acquisition = client
        .begin_mms_acquisition(input("blocked-mapped", Direction::Incoming))
        .unwrap();
    let original = attachment(&client, &dir, "blocked-original.png");
    let thumbnail = attachment(&client, &dir, "blocked-thumbnail.png");
    client
        .set_mms_acquisition_part(&acquisition.acquisition_id, "part", original)
        .unwrap();
    client
        .set_mms_acquisition_part_metadata(
            &acquisition.acquisition_id,
            "part",
            None,
            None,
            None,
            Some(thumbnail),
        )
        .unwrap();

    client
        .set_mms_acquisition_state(
            &acquisition.acquisition_id,
            MmsAcquisitionState::Blocked,
            Some("terminal provider limitation"),
        )
        .unwrap();

    assert!(
        client
            .mms_acquisition_parts(&acquisition.acquisition_id)
            .unwrap()
            .is_empty()
    );
    assert_eq!(client.mms_pending_media_bytes().unwrap(), 0);
    assert_eq!(client.attachment_info(original), Err(Error::NotFound));
    assert_eq!(client.attachment_info(thumbnail), Err(Error::NotFound));
    assert!(
        client
            .finish_mms_backfill_phase(
                "phone-install-a",
                "sim-1",
                MmsBackfillPhase::History,
                &history_bound,
                true,
            )
            .unwrap()
    );

    assert_eq!(
        client
            .request_mms_recheck("phone-install-a", "sim-1", 1)
            .unwrap(),
        1
    );
    let reopened = client
        .begin_mms_acquisition(input("blocked-mapped", Direction::Incoming))
        .unwrap();
    assert_eq!(reopened.acquisition_id, acquisition.acquisition_id);
    let recopied = attachment(&client, &dir, "recopied-original.png");
    client
        .set_mms_acquisition_part(&acquisition.acquisition_id, "part", recopied)
        .unwrap();
    let captured = client
        .complete_mms_acquisition(&acquisition.acquisition_id)
        .unwrap();
    assert!(!captured.duplicate);
    assert_eq!(client.messages(captured.conversation_id).unwrap().len(), 1);
}
