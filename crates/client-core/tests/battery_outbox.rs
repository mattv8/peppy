//! Public outbox ordering and release contracts for the bounded lookup indexes.

use crate::common::*;
use serde_json::json;
use std::fs;
use tempfile::TempDir;

fn capture(client: &Client, conversation_id: ConversationId, provider_id: &str) {
    client
        .capture_incoming(IncomingSms {
            conversation_id: Some(conversation_id),
            sender_address: ADDRESS.into(),
            body: provider_id.into(),
            provider_message_id: Some(provider_id.into()),
            imported: false,
        })
        .unwrap();
}

fn photo_file(dir: &TempDir) -> std::path::PathBuf {
    let path = dir.path().join("photo.png");
    image::RgbImage::from_pixel(300, 200, image::Rgb([1, 2, 3]))
        .save_with_format(&path, image::ImageFormat::Png)
        .unwrap();
    path
}

#[test]
fn pending_outbox_returns_oldest_unacknowledged_envelopes_with_identical_retries() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "client", &vault), &vault);
    let conversation = ConversationId::new();
    capture(&client, conversation, "one");
    capture(&client, conversation, "two");
    capture(&client, conversation, "three");

    let first_read = client.pending_outbox().unwrap();
    let retry = client.pending_outbox().unwrap();
    assert_eq!(
        first_read, retry,
        "retry must preserve the original wire bytes"
    );
    assert_eq!(
        first_read
            .iter()
            .map(|envelope| envelope.producer_sequence.0)
            .collect::<Vec<_>>(),
        vec![1, 2, 3]
    );
    assert_eq!(
        client.pending_outbox_batch(2).unwrap(),
        first_read[..2].to_vec()
    );

    client.ack_outbox(first_read[0].envelope_id).unwrap();
    assert_eq!(client.pending_outbox().unwrap(), first_read[1..].to_vec());
}

#[test]
fn uploaded_media_releases_the_earlier_held_envelope_before_later_ready_work() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client_config = config(&dir, "client", &vault);
    let gateway_config = config(&dir, "gateway", &vault);
    let client = unlocked(&client_config, &vault);
    let path = dir.path().join("attachment.bin");
    fs::write(&path, b"media").unwrap();
    let attachment = client
        .prepare_attachment(&path, "application/octet-stream", "attachment.bin")
        .unwrap();
    let held = client
        .queue_mms(
            OutgoingMms {
                conversation_id: ConversationId::new(),
                recipients: vec![ADDRESS.into()],
                body: "held media".into(),
                attachment_ids: vec![attachment.attachment_id],
                subject: None,
            },
            route(&gateway_config),
        )
        .unwrap();
    capture(&client, ConversationId::new(), "ready-after-media");

    let ready = client.pending_outbox().unwrap();
    assert_eq!(ready.len(), 1, "held media must not be sealed early");
    client
        .mark_attachment_uploaded(attachment.attachment_id, &uuid::Uuid::new_v4().to_string())
        .unwrap();

    let released = client.pending_outbox().unwrap();
    assert_eq!(released.len(), 2);
    assert_eq!(released[0].command_id, Some(held.command_id));
    assert_eq!(released[1], ready[0]);
}

#[test]
fn registered_contact_photo_releases_the_earlier_held_envelope_before_later_ready_work() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client_config = config(&dir, "client", &vault);
    let client = unlocked(&client_config, &vault);
    let photo = client.prepare_contact_photo(&photo_file(&dir)).unwrap();
    let book_id = "book";
    client
        .capture_contact_book(
            &json!({
                "schema_version": 1,
                "book": {
                    "id": book_id,
                    "owner_device_id": client_config.device_id.to_string(),
                    "generation": "1",
                    "state": "active",
                    "capabilities": {"read": true, "write": true, "photo": true},
                    "policy": {"remote_edits": "auto"}
                },
                "contacts": [{
                    "id": "contact",
                    "book_id": book_id,
                    "display_name": "Ada",
                    "name": {"given": "Ada", "family": "Lovelace"},
                    "phones": [], "emails": [], "addresses": [],
                    "provenance": {"source_id": "native:contact", "account_id": "native:account", "read_only": false},
                    "photo": {"attachment_id": photo.attachment_id.to_string(), "state": "pending_upload"}
                }]
            })
            .to_string(),
        )
        .unwrap();
    let book_event = client.pending_outbox().unwrap();
    assert_eq!(
        book_event.len(),
        2,
        "book metadata and the names-first contact revision are eligible initially"
    );
    capture(&client, ConversationId::new(), "ready-after-photo");

    let ready = client.pending_outbox().unwrap();
    assert_eq!(
        ready.len(),
        3,
        "only the full photo follow-up waits for upload and registration"
    );
    client
        .mark_attachment_uploaded(photo.attachment_id, &uuid::Uuid::new_v4().to_string())
        .unwrap();
    assert_eq!(client.pending_outbox().unwrap(), ready);

    let state: serde_json::Value =
        serde_json::from_str(&client.contact_photo_transfer_state_json().unwrap()).unwrap();
    let registration = &state["registrations"].as_array().unwrap()[0];
    client
        .acknowledge_contact_photo_reference_json(
            &json!({
                "schema_version": 1,
                "envelope_id": registration["envelope_id"],
                "attachment_id": registration["attachment_id"]
            })
            .to_string(),
        )
        .unwrap();

    let released = client.pending_outbox().unwrap();
    assert_eq!(released.len(), 4);
    // Live work keeps its priority. Within contact priority, the names-first predecessor,
    // released photo follow-up, then book state retain producer-sequence order.
    assert_eq!(released[0], ready[0]);
    assert_eq!(released[1], ready[1]);
    assert_eq!(
        released[2].envelope_id.to_string(),
        registration["envelope_id"]
    );
    assert_eq!(released[3], ready[2]);
}
