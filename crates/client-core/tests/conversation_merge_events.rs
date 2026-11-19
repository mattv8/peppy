use crate::common::*;

const PEER_NUMBER: &str = "+12025550101";

#[test]
fn pin_and_folder_winners_are_independent_of_merge_order() {
    let directory = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let writer = unlocked(&config(&directory, "order-writer", &vault), &vault);
    let before = unlocked(&config(&directory, "order-before", &vault), &vault);
    let after = unlocked(&config(&directory, "order-after", &vault), &vault);
    let mut ids = [ConversationId::new(), ConversationId::new()];
    ids.sort_by_key(ToString::to_string);
    let [survivor, source] = ids;
    for (id, provider) in [(survivor, "order-1"), (source, "order-2")] {
        let mut message = incoming("hello", provider);
        message.conversation_id = Some(id);
        message.sender_address = PEER_NUMBER.into();
        writer.capture_incoming(message).unwrap();
    }
    writer.set_conversation_pinned(source, true).unwrap();
    writer
        .set_conversation_folder(source, ConversationFolder::Spam)
        .unwrap();
    writer.set_conversation_pinned(survivor, false).unwrap();
    writer
        .set_conversation_folder(survivor, ConversationFolder::Inbox)
        .unwrap();
    let mut server = Server::default();
    server.upload(&writer);
    assert_eq!(server.log.len(), 6);
    for (cursor, index) in [2, 3, 4, 5, 0, 1].into_iter().enumerate() {
        before
            .ingest(&server.log[index], Cursor(cursor as u64 + 1))
            .unwrap();
        before.apply_pending(100).unwrap();
    }
    while before.converge_conversations(100).unwrap() {}
    for index in 0..2 {
        after
            .ingest(&server.log[index], Cursor(index as u64 + 1))
            .unwrap();
    }
    after.apply_pending(100).unwrap();
    while after.converge_conversations(100).unwrap() {}
    for index in 2..6 {
        after
            .ingest(&server.log[index], Cursor(index as u64 + 1))
            .unwrap();
        after.apply_pending(100).unwrap();
    }
    for client in [&before, &after] {
        assert_eq!(
            client.conversation_redirect(source).unwrap(),
            Some(survivor)
        );
        let row = client
            .list_conversations()
            .unwrap()
            .into_iter()
            .find(|row| row.conversation_id == survivor)
            .unwrap();
        assert!(!row.pinned);
        assert_eq!(row.folder, ConversationFolder::Inbox);
    }
}

#[test]
fn live_pin_and_folder_on_source_apply_to_survivor() {
    let directory = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let writer = unlocked(&config(&directory, "merge-event-writer", &vault), &vault);
    let peer = unlocked(&config(&directory, "merge-event-peer", &vault), &vault);
    let mut ids = [ConversationId::new(), ConversationId::new()];
    ids.sort_by_key(ToString::to_string);
    let [survivor, source] = ids;
    for (id, provider) in [(survivor, "event-1"), (source, "event-2")] {
        let mut message = incoming("hello", provider);
        message.conversation_id = Some(id);
        message.sender_address = PEER_NUMBER.into();
        writer.capture_incoming(message).unwrap();
    }
    let mut server = Server::default();
    server.upload(&writer);
    server.sync(&peer);
    while peer.converge_conversations(100).unwrap() {}
    assert_eq!(peer.conversation_redirect(source).unwrap(), Some(survivor));
    writer.set_conversation_pinned(source, true).unwrap();
    writer
        .set_conversation_folder(source, ConversationFolder::Spam)
        .unwrap();
    server.upload(&writer);
    server.sync(&peer);
    let row = peer
        .list_conversations()
        .unwrap()
        .into_iter()
        .find(|row| row.conversation_id == survivor)
        .unwrap();
    assert!(row.pinned);
    assert_eq!(row.folder, ConversationFolder::Spam);
    peer.set_conversation_pinned(survivor, false).unwrap();
    peer.set_conversation_folder(survivor, ConversationFolder::Inbox)
        .unwrap();
    let row = peer
        .list_conversations()
        .unwrap()
        .into_iter()
        .find(|row| row.conversation_id == survivor)
        .unwrap();
    assert!(!row.pinned);
    assert_eq!(row.folder, ConversationFolder::Inbox);
}

#[test]
fn snapshot_pin_and_folder_on_source_promote_to_survivor() {
    let directory = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let writer = unlocked(&config(&directory, "merge-snapshot-writer", &vault), &vault);
    let peer = unlocked(&config(&directory, "merge-snapshot-peer", &vault), &vault);
    let mut ids = [ConversationId::new(), ConversationId::new()];
    ids.sort_by_key(ToString::to_string);
    let [survivor, source] = ids;
    for (id, provider) in [(survivor, "snapshot-1"), (source, "snapshot-2")] {
        let mut message = incoming("hello", provider);
        message.conversation_id = Some(id);
        message.sender_address = PEER_NUMBER.into();
        writer.capture_incoming(message).unwrap();
    }
    let mut server = Server::default();
    server.upload(&writer);
    server.sync(&peer);
    while peer.converge_conversations(100).unwrap() {}
    assert_eq!(peer.conversation_redirect(source).unwrap(), Some(survivor));
    writer.set_server_compaction_state(true, true).unwrap();
    writer.set_conversation_pinned(source, true).unwrap();
    writer
        .set_conversation_folder(source, ConversationFolder::Spam)
        .unwrap();
    server.upload(&writer);
    let records: Vec<SnapshotRecord> = server
        .log
        .iter()
        .enumerate()
        .map(|(index, envelope)| SnapshotRecord {
            cursor: Cursor(index as u64 + 1),
            envelope: envelope.clone(),
        })
        .collect();
    let session = peer
        .begin_snapshot_with_compaction(
            Cursor(records.len() as u64),
            records.len() as u64,
            SnapshotPurpose::Resync,
            Some(3),
        )
        .unwrap();
    peer.append_snapshot_page(session.generation, &records)
        .unwrap();
    peer.finish_snapshot(session.generation).unwrap();
    loop {
        let report = peer.apply_pending(100).unwrap();
        if report.applied + report.drained + report.quarantined + report.superseded == 0
            && report.snapshot_remaining == 0
        {
            break;
        }
    }
    let row = peer
        .list_conversations()
        .unwrap()
        .into_iter()
        .find(|row| row.conversation_id == survivor)
        .unwrap();
    assert!(row.pinned);
    assert_eq!(row.folder, ConversationFolder::Spam);
}

#[test]
fn quick_reply_uses_messages_from_a_merged_source() {
    let directory = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let config = config(&directory, "merge-quick-reply", &vault);
    let client = unlocked(&config, &vault);
    let mut ids = [ConversationId::new(), ConversationId::new()];
    ids.sort_by_key(ToString::to_string);
    let [survivor, source] = ids;
    for (id, provider) in [(survivor, "merge-1"), (source, "merge-2")] {
        client
            .capture_incoming(IncomingSms {
                conversation_id: Some(id),
                sender_address: PEER_NUMBER.into(),
                body: "hello".into(),
                provider_message_id: Some(provider.into()),
                imported: false,
            })
            .unwrap();
    }
    let input = QuickReplyInput {
        command_id: CommandId::new(),
        conversation_id: source,
        body: "reply".into(),
        route: route(&config),
        expected_transport: Transport::Sms,
        expected_member_revision: None,
    };
    let queued = client.queue_quick_reply(input.clone()).unwrap();
    while client.converge_conversations(100).unwrap() {}
    assert_eq!(
        client.conversation_redirect(source).unwrap(),
        Some(survivor)
    );
    assert_eq!(
        client
            .reconcile_quick_reply(&QuickReplyInput {
                conversation_id: survivor,
                ..input
            })
            .unwrap(),
        Some(queued)
    );
}

#[test]
fn group_metadata_keeps_its_original_id_after_merge() {
    let directory = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let writer_config = config(&directory, "merge-group-writer", &vault);
    let writer = unlocked(&writer_config, &vault);
    let peer = unlocked(&config(&directory, "merge-group-peer", &vault), &vault);
    let mut ids = [ConversationId::new(), ConversationId::new()];
    ids.sort_by_key(ToString::to_string);
    let [survivor, source] = ids;
    for (id, provider) in [(survivor, "group-1"), (source, "group-2")] {
        let mut message = incoming("hello", provider);
        message.conversation_id = Some(id);
        message.sender_address = PEER_NUMBER.into();
        writer.capture_incoming(message).unwrap();
    }
    let mut server = Server::default();
    server.upload(&writer);
    server.sync(&peer);
    while peer.converge_conversations(100).unwrap() {}
    assert_eq!(peer.conversation_redirect(source).unwrap(), Some(survivor));
    writer
        .capture_incoming_mms(IncomingMms {
            conversation_id: Some(source),
            sender_address: PEER_NUMBER.into(),
            body: "group".into(),
            provider_message_id: Some("group-3".into()),
            imported: false,
            attachment_ids: Vec::new(),
            recipients: vec!["+12025550102".into(), "+12025550103".into()],
            subject: None,
        })
        .unwrap();
    writer
        .queue_quick_reply(QuickReplyInput {
            command_id: CommandId::new(),
            conversation_id: source,
            body: "group reply".into(),
            route: route(&writer_config),
            expected_transport: Transport::Mms,
            expected_member_revision: None,
        })
        .unwrap();
    server.upload(&writer);
    server.sync(&peer);
    assert_eq!(peer.group_roster(source).unwrap().len(), 3);
    assert!(peer.group_roster(survivor).unwrap().is_empty());
}
