//! Cursor-stable, metadata-only media paging around a visible message attachment.

use crate::common::*;
use std::{fs, path::Path};
use tempfile::TempDir;

fn capture_media(
    client: &Client,
    source: &Path,
    count: usize,
    sender_address: &str,
) -> (ConversationId, Vec<(MessageId, AttachmentId)>) {
    let captured: Vec<_> = (0..count)
        .map(|index| {
            let attachment_id = client
                .prepare_attachment(source, "image/png", &format!("image-{index}.png"))
                .unwrap()
                .attachment_id;
            let acquisition = client
                .begin_mms_acquisition(MmsAcquisitionInput {
                    source: MmsSource {
                        source_generation: format!("anchor-paging-{sender_address}"),
                        subscription_id: "sim-1".into(),
                        provider_message_id: format!("media-{index}"),
                        provider_thread_id: None,
                    },
                    direction: Direction::Incoming,
                    sender_address: Some(sender_address.into()),
                    recipients: vec![],
                    subject: None,
                    body: String::new(),
                    imported: false,
                    observed_at_ms: 1_700_000_000_000,
                    transaction_id: None,
                })
                .unwrap();
            client
                .set_mms_acquisition_part(&acquisition.acquisition_id, "image", attachment_id)
                .unwrap();
            (
                client
                    .complete_mms_acquisition(&acquisition.acquisition_id)
                    .unwrap(),
                attachment_id,
            )
        })
        .collect();
    (
        captured[0].0.conversation_id,
        captured
            .into_iter()
            .map(|(item, attachment_id)| (item.message_id, attachment_id))
            .collect(),
    )
}

fn message_ids(items: &[MediaPageItem]) -> Vec<MessageId> {
    items
        .iter()
        .map(|item| item.message.record.message_id)
        .collect()
}

#[test]
fn anchored_paging_walks_more_than_a_hundred_equal_date_media_rows_without_duplicates() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "client", &vault), &vault);
    let source = dir.path().join("image.bin");
    fs::write(&source, b"encrypted only when explicitly opened").unwrap();
    let (conversation_id, rows) = capture_media(&client, &source, 121, ADDRESS);
    let (message_id, attachment_id) = rows[60];
    let around = client
        .media_page_around(conversation_id, attachment_id, 11)
        .unwrap();
    assert_eq!(around.target.message.record.message_id, message_id);
    assert!(around.newer.len() + around.older.len() < 11);

    let mut newer = around.newer.clone();
    let mut newer_cursor = around.newer_cursor;
    while let Some(cursor) = newer_cursor {
        let page = client
            .media_page_newer(conversation_id, &cursor, 7)
            .unwrap();
        newer.extend(page.items);
        newer_cursor = page.next_cursor;
    }
    let mut older = around.older.clone();
    let mut older_cursor = around.older_cursor;
    while let Some(cursor) = older_cursor {
        let page = client
            .media_page(conversation_id, Some(&cursor), 7)
            .unwrap();
        older.extend(page.items);
        older_cursor = page.next_cursor;
    }

    let all = client.media_page(conversation_id, None, 500).unwrap().items;
    assert!(
        all.iter()
            .all(|item| item.cursor.sort_at_ms == 1_700_000_000_000),
        "fixture deliberately gives every MMS the same display date"
    );
    let anchor = all
        .iter()
        .position(|item| item.message.record.message_id == message_id)
        .unwrap();
    let expected_newer: Vec<_> = all[..anchor]
        .iter()
        .rev()
        .map(|item| item.message.record.message_id)
        .collect();
    let expected_older: Vec<_> = all[anchor + 1..]
        .iter()
        .map(|item| item.message.record.message_id)
        .collect();
    assert_eq!(message_ids(&newer), expected_newer);
    assert_eq!(message_ids(&older), expected_older);
}

#[test]
fn reused_attachment_requires_an_explicit_message_anchor_and_cross_conversation_target_is_rejected()
{
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "client", &vault), &vault);
    let source = dir.path().join("image.bin");
    fs::write(&source, b"metadata paging does not decrypt this").unwrap();
    let attachment_id = client
        .prepare_attachment(&source, "image/png", "image.png")
        .unwrap()
        .attachment_id;
    let compose_conversation = ConversationId::new();
    let gateway = config(&dir, "gateway", &vault);
    let first = client
        .queue_mms(
            OutgoingMms {
                conversation_id: compose_conversation,
                recipients: vec![ADDRESS.into()],
                body: "first".into(),
                attachment_ids: vec![attachment_id],
                subject: None,
            },
            route(&gateway),
        )
        .unwrap();
    client
        .queue_mms(
            OutgoingMms {
                conversation_id: compose_conversation,
                recipients: vec![ADDRESS.into()],
                body: "second".into(),
                attachment_ids: vec![attachment_id],
                subject: None,
            },
            route(&gateway),
        )
        .unwrap();
    assert!(matches!(
        client.media_page_around(compose_conversation, attachment_id, 3),
        Err(Error::InvalidRequest("media anchor ambiguous"))
    ));
    assert_eq!(
        client
            .media_page_around_at(compose_conversation, first.message_id, attachment_id, 3)
            .unwrap()
            .target
            .message
            .record
            .message_id,
        first.message_id
    );

    let (first_conversation, _) = capture_media(&client, &source, 1, ADDRESS);
    let (other_conversation, other_rows) = capture_media(&client, &source, 1, "+15555550199");
    let (other_id, other_attachment_id) = other_rows[0];

    assert_ne!(first_conversation, other_conversation);
    assert!(matches!(
        client.media_page_around_at(first_conversation, other_id, other_attachment_id, 3),
        Err(Error::NotFound)
    ));
}
