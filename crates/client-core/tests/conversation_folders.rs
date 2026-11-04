//! Durable replicated conversation-folder regression coverage.
use crate::common::*;
use peppy_client_core::ConversationFolder;
use tempfile::TempDir;

/// Captures an incoming message whose sort time is exactly `at_ms`, so watermark comparisons are
/// deterministic instead of depending on wall-clock ties.
fn capture_at(
    client: &Client,
    conversation: Option<ConversationId>,
    body: &str,
    provider_id: &str,
    at_ms: i64,
) -> Captured {
    let mut sms = incoming(body, provider_id);
    sms.conversation_id = conversation;
    client
        .capture_incoming_broadcast(sms, None, Some(at_ms))
        .unwrap()
}

fn folder_of(client: &Client, conversation: ConversationId) -> ConversationFolder {
    client
        .list_conversations()
        .unwrap()
        .into_iter()
        .find(|row| row.conversation_id == conversation)
        .unwrap()
        .folder
}

#[test]
fn folder_changes_are_visible() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "folder", &vault), &vault);
    let conversation = client
        .capture_incoming(incoming("hello", "folder-1"))
        .unwrap()
        .conversation_id;
    client
        .set_conversation_folder(conversation, ConversationFolder::Archived)
        .unwrap();
    assert_eq!(
        client.list_conversations().unwrap()[0].folder,
        ConversationFolder::Archived
    );
    client
        .set_conversation_folder(conversation, ConversationFolder::Inbox)
        .unwrap();
    assert_eq!(
        client.list_conversations().unwrap()[0].folder,
        ConversationFolder::Inbox
    );
}

#[test]
fn archived_conversations_resurface_but_spam_does_not() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "folder-resurface", &vault), &vault);
    let conversation =
        capture_at(&client, None, "one", "folder-resurface-1", 1_000).conversation_id;
    client
        .set_conversation_folder(conversation, ConversationFolder::Archived)
        .unwrap();
    capture_at(
        &client,
        Some(conversation),
        "two",
        "folder-resurface-2",
        2_000,
    );
    assert_eq!(
        client.list_conversations().unwrap()[0].folder,
        ConversationFolder::Inbox
    );
    client
        .set_conversation_folder(conversation, ConversationFolder::Spam)
        .unwrap();
    capture_at(
        &client,
        Some(conversation),
        "three",
        "folder-resurface-3",
        3_000,
    );
    assert_eq!(
        client.list_conversations().unwrap()[0].folder,
        ConversationFolder::Spam
    );
}

#[test]
fn outgoing_send_after_archive_resurfaces_to_inbox() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let gateway = config(&dir, "folder-outgoing-gateway", &vault);
    let client = unlocked(&config(&dir, "folder-outgoing", &vault), &vault);
    let conversation =
        capture_at(&client, None, "hello", "folder-outgoing-1", 1_000).conversation_id;
    client
        .set_conversation_folder(conversation, ConversationFolder::Archived)
        .unwrap();
    assert_eq!(
        folder_of(&client, conversation),
        ConversationFolder::Archived
    );
    client
        .queue_send(sms(conversation, "reply"), route(&gateway))
        .unwrap();
    assert_eq!(folder_of(&client, conversation), ConversationFolder::Inbox);
}

#[test]
fn message_at_archive_watermark_stays_archived() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "folder-watermark-tie", &vault), &vault);
    let conversation =
        capture_at(&client, None, "one", "folder-watermark-1", 1_000).conversation_id;
    client
        .set_conversation_folder(conversation, ConversationFolder::Archived)
        .unwrap();
    capture_at(
        &client,
        Some(conversation),
        "same instant",
        "folder-watermark-2",
        1_000,
    );
    assert_eq!(
        folder_of(&client, conversation),
        ConversationFolder::Archived
    );
}

#[test]
fn imported_older_message_stays_archived() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "folder-imported-older", &vault), &vault);
    let conversation =
        capture_at(&client, None, "newest", "folder-imported-1", 2_000).conversation_id;
    client
        .set_conversation_folder(conversation, ConversationFolder::Archived)
        .unwrap();
    let mut imported = incoming("old history", "folder-imported-2");
    imported.conversation_id = Some(conversation);
    imported.imported = true;
    client
        .capture_incoming_broadcast(imported, None, Some(500))
        .unwrap();
    assert_eq!(
        folder_of(&client, conversation),
        ConversationFolder::Archived
    );
}

#[test]
fn rearchiving_recomputes_the_watermark_past_an_intermediate_message() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "folder-rearchive", &vault), &vault);
    let conversation =
        capture_at(&client, None, "first", "folder-rearchive-1", 1_000).conversation_id;
    client
        .set_conversation_folder(conversation, ConversationFolder::Archived)
        .unwrap();
    capture_at(
        &client,
        Some(conversation),
        "between",
        "folder-rearchive-2",
        2_000,
    );
    assert_eq!(folder_of(&client, conversation), ConversationFolder::Inbox);
    client
        .set_conversation_folder(conversation, ConversationFolder::Archived)
        .unwrap();
    assert_eq!(
        folder_of(&client, conversation),
        ConversationFolder::Archived
    );
    capture_at(
        &client,
        Some(conversation),
        "older than watermark",
        "folder-rearchive-3",
        1_500,
    );
    assert_eq!(
        folder_of(&client, conversation),
        ConversationFolder::Archived
    );
}

#[test]
fn archived_unread_resurfaces_with_new_message_and_spam_unread_stays_excluded() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "folder-unread-resurface", &vault), &vault);
    let archived = capture_at(
        &client,
        None,
        "old unread",
        "folder-unread-resurface-1",
        1_000,
    );
    client
        .set_conversation_folder(archived.conversation_id, ConversationFolder::Archived)
        .unwrap();
    assert!(client.unread_message_batches().unwrap().is_empty());
    let fresh = capture_at(
        &client,
        Some(archived.conversation_id),
        "new",
        "folder-unread-resurface-2",
        2_000,
    );
    let unread: Vec<MessageId> = client
        .unread_message_batches()
        .unwrap()
        .into_iter()
        .flatten()
        .collect();
    assert!(unread.contains(&archived.message_id));
    assert!(unread.contains(&fresh.message_id));

    let spam = capture_at(
        &client,
        None,
        "spam unread",
        "folder-unread-resurface-3",
        3_000,
    );
    client
        .set_conversation_folder(spam.conversation_id, ConversationFolder::Spam)
        .unwrap();
    capture_at(
        &client,
        Some(spam.conversation_id),
        "more spam",
        "folder-unread-resurface-4",
        4_000,
    );
    let unread: Vec<MessageId> = client
        .unread_message_batches()
        .unwrap()
        .into_iter()
        .flatten()
        .collect();
    assert!(!unread.contains(&spam.message_id));
}

#[test]
fn remote_spam_event_clears_an_already_applied_message_banner() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let writer = unlocked(&config(&dir, "folder-banner-remote-writer", &vault), &vault);
    let desktop = unlocked(
        &config(&dir, "folder-banner-remote-desktop", &vault),
        &vault,
    );
    let conversation = writer
        .capture_incoming(incoming("hello", "folder-banner-remote"))
        .unwrap()
        .conversation_id;
    let mut server = Server::default();
    server.upload(&writer);
    server.sync(&desktop);
    assert_eq!(desktop.pending_banner_candidates(10).unwrap().len(), 1);

    writer
        .set_conversation_folder(conversation, ConversationFolder::Spam)
        .unwrap();
    server.upload(&writer);
    server.sync(&desktop);
    assert!(desktop.pending_banner_candidates(10).unwrap().is_empty());

    writer
        .set_conversation_folder(conversation, ConversationFolder::Inbox)
        .unwrap();
    server.upload(&writer);
    server.sync(&desktop);
    assert!(
        desktop.pending_banner_candidates(10).unwrap().is_empty(),
        "the candidate must be deleted, not only hidden"
    );
}

#[test]
fn local_spam_clears_pending_message_banner_and_it_stays_gone() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let writer = unlocked(&config(&dir, "folder-banner-local-writer", &vault), &vault);
    let desktop = unlocked(&config(&dir, "folder-banner-local-desktop", &vault), &vault);
    let conversation = writer
        .capture_incoming(incoming("hello", "folder-banner-local"))
        .unwrap()
        .conversation_id;
    let mut server = Server::default();
    server.upload(&writer);
    server.sync(&desktop);
    assert_eq!(desktop.pending_banner_candidates(10).unwrap().len(), 1);
    desktop
        .set_conversation_folder(conversation, ConversationFolder::Spam)
        .unwrap();
    assert!(desktop.pending_banner_candidates(10).unwrap().is_empty());
    desktop
        .set_conversation_folder(conversation, ConversationFolder::Inbox)
        .unwrap();
    assert!(
        desktop.pending_banner_candidates(10).unwrap().is_empty(),
        "the candidate must be deleted, not only hidden"
    );
}

#[test]
fn archived_conversation_still_yields_a_banner_candidate() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let writer = unlocked(
        &config(&dir, "folder-banner-archived-writer", &vault),
        &vault,
    );
    let desktop = unlocked(
        &config(&dir, "folder-banner-archived-desktop", &vault),
        &vault,
    );
    let conversation = writer
        .capture_incoming_broadcast(
            incoming("old", "folder-banner-archived-1"),
            None,
            Some(1_000),
        )
        .unwrap()
        .conversation_id;
    let mut server = Server::default();
    server.upload(&writer);
    server.sync(&desktop);
    let old_candidates = desktop.pending_banner_candidates(10).unwrap();
    assert_eq!(old_candidates.len(), 1);
    desktop
        .ack_banner_candidates(
            old_candidates
                .into_iter()
                .map(|candidate| candidate.id)
                .collect(),
        )
        .unwrap();

    desktop
        .set_conversation_folder(conversation, ConversationFolder::Archived)
        .unwrap();
    writer
        .capture_incoming_broadcast(
            IncomingSms {
                conversation_id: Some(conversation),
                ..incoming("new", "folder-banner-archived-2")
            },
            None,
            Some(2_000),
        )
        .unwrap();
    server.upload(&writer);
    server.sync(&desktop);
    let candidates = desktop.pending_banner_candidates(10).unwrap();
    assert_eq!(candidates.len(), 1);
    assert_eq!(
        candidates[0].conversation_id.as_deref(),
        Some(conversation.to_string().as_str())
    );
}

#[test]
fn folder_state_survives_authoritative_snapshot_on_second_client() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let writer = unlocked(&config(&dir, "folder-snapshot-writer", &vault), &vault);
    let peer = unlocked(&config(&dir, "folder-snapshot-peer", &vault), &vault);
    let conversation = writer
        .capture_incoming(incoming("hello", "folder-snapshot"))
        .unwrap()
        .conversation_id;
    writer
        .set_conversation_folder(conversation, ConversationFolder::Archived)
        .unwrap();
    let mut server = Server::default();
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
            Some(1),
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
    assert_eq!(folder_of(&peer, conversation), ConversationFolder::Archived);
}

fn import_empty_snapshot(client: &Client, high_water: u64, generation: u64) {
    let session = client
        .begin_snapshot_with_compaction(
            Cursor(high_water),
            0,
            SnapshotPurpose::Resync,
            Some(generation),
        )
        .unwrap();
    client.finish_snapshot(session.generation).unwrap();
    loop {
        let report = client.apply_pending(100).unwrap();
        if report.applied + report.drained + report.quarantined + report.superseded == 0
            && report.snapshot_remaining == 0
        {
            break;
        }
    }
}

#[test]
fn empty_authoritative_snapshot_removes_a_synced_folder_state() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let writer = unlocked(
        &config(&dir, "folder-snapshot-remove-writer", &vault),
        &vault,
    );
    let peer = unlocked(&config(&dir, "folder-snapshot-remove-peer", &vault), &vault);
    let conversation = writer
        .capture_incoming(incoming("hello", "folder-snapshot-remove"))
        .unwrap()
        .conversation_id;
    writer
        .set_conversation_folder(conversation, ConversationFolder::Spam)
        .unwrap();
    let mut server = Server::default();
    server.upload(&writer);
    server.sync(&peer);
    assert_eq!(folder_of(&peer, conversation), ConversationFolder::Spam);

    import_empty_snapshot(&peer, 2, 2);
    assert_eq!(folder_of(&peer, conversation), ConversationFolder::Inbox);
}

#[test]
fn empty_authoritative_snapshot_keeps_unsent_local_archive_and_its_watermark() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "folder-snapshot-local", &vault), &vault);
    let conversation =
        capture_at(&client, None, "hello", "folder-snapshot-local-1", 1_000).conversation_id;
    client
        .set_conversation_folder(conversation, ConversationFolder::Archived)
        .unwrap();
    import_empty_snapshot(&client, 0, 1);
    assert_eq!(
        folder_of(&client, conversation),
        ConversationFolder::Archived
    );

    capture_at(
        &client,
        Some(conversation),
        "older than watermark",
        "folder-snapshot-local-2",
        500,
    );
    assert_eq!(
        folder_of(&client, conversation),
        ConversationFolder::Archived
    );
    capture_at(
        &client,
        Some(conversation),
        "newer",
        "folder-snapshot-local-3",
        2_000,
    );
    assert_eq!(folder_of(&client, conversation), ConversationFolder::Inbox);
}

#[test]
fn second_folder_write_supersedes_the_first() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "folder-supersedes", &vault), &vault);
    let conversation = client
        .capture_incoming(incoming("hello", "folder-supersedes"))
        .unwrap()
        .conversation_id;
    client
        .set_conversation_folder(conversation, ConversationFolder::Archived)
        .unwrap();
    let first = client.pending_outbox().unwrap().pop().unwrap();
    client
        .set_conversation_folder(conversation, ConversationFolder::Spam)
        .unwrap();
    let second = client.pending_outbox().unwrap().pop().unwrap();
    let compaction = second
        .compaction
        .as_ref()
        .expect("folder events are compactable");
    assert!(
        compaction
            .supersedes
            .iter()
            .any(
                |reference| reference.producer_device_id == first.producer_device_id
                    && reference.producer_sequence.0 == first.producer_sequence.0
            ),
        "the second folder event must supersede the first"
    );
}

#[test]
fn equal_revision_folder_writes_converge_by_writer_identity() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let left = unlocked(&config(&dir, "folder-tie-left", &vault), &vault);
    let right = unlocked(&config(&dir, "folder-tie-right", &vault), &vault);
    let conversation = left
        .capture_incoming(incoming("hello", "folder-tie"))
        .unwrap()
        .conversation_id;
    let mut server = Server::default();
    server.upload(&left);
    server.sync(&right);

    left.set_conversation_folder(conversation, ConversationFolder::Archived)
        .unwrap();
    right
        .set_conversation_folder(conversation, ConversationFolder::Spam)
        .unwrap();
    let left_event = left.pending_outbox().unwrap().pop().unwrap();
    let right_event = right.pending_outbox().unwrap().pop().unwrap();
    let expected =
        if left_event.producer_device_id.to_string() > right_event.producer_device_id.to_string() {
            ConversationFolder::Archived
        } else {
            ConversationFolder::Spam
        };
    for events in [[&left_event, &right_event], [&right_event, &left_event]] {
        let peer = unlocked(
            &config(
                &dir,
                &format!("folder-tie-{}", events[0].envelope_id),
                &vault,
            ),
            &vault,
        );
        peer.ingest(&server.log[0], Cursor(1)).unwrap();
        peer.apply_pending(10).unwrap();
        peer.ingest(events[0], Cursor(2)).unwrap();
        peer.ingest(events[1], Cursor(3)).unwrap();
        peer.apply_pending(10).unwrap();
        assert_eq!(peer.list_conversations().unwrap()[0].folder, expected);
    }
}

#[test]
fn folder_rejects_unknown_conversation_without_an_outbox_write() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "folder-invalid", &vault), &vault);
    let outbox_before = client.pending_outbox().unwrap().len();
    assert!(matches!(
        client.set_conversation_folder(ConversationId::new(), ConversationFolder::Archived),
        Err(Error::InvalidRequest("conversation folder"))
    ));
    assert_eq!(client.pending_outbox().unwrap().len(), outbox_before);
}
