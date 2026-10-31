//! Gateway-only MMS ciphertext cleanup retains remote restoration metadata.

use crate::common::*;
use std::fs;
use tempfile::TempDir;

fn input(provider_message_id: &str) -> MmsAcquisitionInput {
    MmsAcquisitionInput {
        source: MmsSource {
            source_generation: "gateway-mms-cleanup".into(),
            subscription_id: "sim-1".into(),
            provider_message_id: provider_message_id.into(),
            provider_thread_id: Some("thread-1".into()),
        },
        direction: Direction::Incoming,
        sender_address: Some(ADDRESS.into()),
        recipients: Vec::new(),
        subject: None,
        body: "image".into(),
        imported: false,
        observed_at_ms: 1,
        transaction_id: None,
    }
}

fn prepare(client: &Client, directory: &TempDir, name: &str) -> AttachmentId {
    let path = directory.path().join(name);
    fs::write(
        &path,
        [
            0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, b'I', b'H',
            b'D', b'R', 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x04, 0x00, 0x00,
            0x00, 0xb5, 0x1c, 0x0c, 0x02, 0x00, 0x00, 0x00, 0x0b, b'I', b'D', b'A', b'T', 0x78,
            0x9c, 0x63, 0x60, 0x60, 0x60, 0x00, 0x00, 0x00, 0x04, 0x00, 0x01, 0xa3, 0x0a, 0x15,
            0x6e, 0x00, 0x00, 0x00, 0x00, b'I', b'E', b'N', b'D', 0xae, 0x42, 0x60, 0x82,
        ],
    )
    .unwrap();
    client
        .prepare_attachment(&path, "image/png", name)
        .unwrap()
        .attachment_id
}

#[test]
fn gateway_cleanup_waits_for_ack_then_removes_only_completed_mms_original_and_preview() {
    let directory = TempDir::new().unwrap();
    let vault = Vault::new();
    let config = config(&directory, "gateway", &vault);
    let client = unlocked(&config, &vault);
    let acquisition = client.begin_mms_acquisition(input("42")).unwrap();
    let original = prepare(&client, &directory, "original.png");
    let preview = prepare(&client, &directory, "preview.png");
    let unrelated = prepare(&client, &directory, "unrelated.png");
    client
        .set_mms_acquisition_part(&acquisition.acquisition_id, "original", original)
        .unwrap();
    client
        .set_mms_acquisition_part_metadata(
            &acquisition.acquisition_id,
            "original",
            Some(1),
            Some(1),
            None,
            Some(preview),
        )
        .unwrap();
    client
        .complete_mms_acquisition(&acquisition.acquisition_id)
        .unwrap();
    // Finish legacy bootstrap before these attachments become uploaded: the later candidate must
    // arrive through the acknowledgement hook rather than a repeated attachment-history scan.
    client.cleanup_uploaded_gateway_mms_ciphertext(10).unwrap();
    client.cleanup_uploaded_gateway_mms_ciphertext(10).unwrap();

    for object in client.pending_uploads().unwrap() {
        client
            .mark_attachment_uploaded(object.attachment_id, &uuid::Uuid::new_v4().to_string())
            .unwrap();
    }
    assert_eq!(
        client.cleanup_uploaded_gateway_mms_ciphertext(10).unwrap(),
        GatewayMmsCleanup::default(),
        "the queued event still references both capture copies"
    );

    let mut server = Server::default();
    assert_eq!(server.upload(&client), 1);
    let original_path = client.native_cipher_file(original).unwrap();
    let preview_path = client.native_cipher_file(preview).unwrap();
    let unrelated_path = client.native_cipher_file(unrelated).unwrap();
    let original_ciphertext = fs::read(&original_path).unwrap();
    client.retain_attachment_ciphertext(original).unwrap();
    let cleanup = client.cleanup_uploaded_gateway_mms_ciphertext(10).unwrap();
    assert_eq!(cleanup.marked, 1);
    assert_eq!(cleanup.unlinked, 1);
    assert_eq!(cleanup.pending_unlink, 0);
    assert!(
        original_path.exists(),
        "an active reader defers original cleanup"
    );
    assert!(!preview_path.exists());
    assert!(
        unrelated_path.exists(),
        "non-MMS uploads are never cleanup candidates"
    );
    assert!(client.release_attachment_ciphertext(original).unwrap());
    let released_cleanup = client.cleanup_uploaded_gateway_mms_ciphertext(10).unwrap();
    assert_eq!(released_cleanup.marked, 1);
    assert_eq!(released_cleanup.unlinked, 1);
    assert_eq!(released_cleanup.pending_unlink, 0);
    assert!(!original_path.exists());
    assert!(matches!(
        client.native_cipher_file(original),
        Err(Error::InvalidMedia)
    ));
    assert!(
        !client
            .local_attachment_ids(None, 100)
            .unwrap()
            .contains(&original),
        "remote-only capture ciphertext is absent from local checkpoint inventory"
    );

    let pending_bytes = client.mms_pending_media_bytes().unwrap();
    client.request_attachment_download(original).unwrap();
    assert_eq!(
        client.mms_pending_media_bytes().unwrap(),
        pending_bytes,
        "explicit rehydration does not re-consume MMS capture capacity"
    );
    let downloads = client.pending_downloads().unwrap();
    assert_eq!(downloads.len(), 1);
    assert_eq!(downloads[0].attachment_id, original);
    let downloaded = directory.path().join("original-rehydrated.cipher");
    fs::write(&downloaded, original_ciphertext).unwrap();
    client
        .install_downloaded_attachment(original, &downloaded)
        .unwrap();
    assert!(client.native_cipher_file(original).unwrap().exists());
    let plaintext = client.open_native_plaintext(original).unwrap();
    assert!(plaintext.path().exists());
    drop(plaintext);
    assert!(
        client
            .local_attachment_ids(None, 100)
            .unwrap()
            .contains(&original),
        "rehydrated ciphertext returns to browser/native checkpoint inventory"
    );
    assert_eq!(
        client.cleanup_uploaded_gateway_mms_ciphertext(10).unwrap(),
        GatewayMmsCleanup::default(),
        "the same gateway pass leaves a verified rehydrated copy available for readers"
    );
    assert!(client.native_cipher_file(original).unwrap().exists());

    drop(client);
    let reopened = unlocked(&config, &vault);
    assert_eq!(
        reopened
            .cleanup_uploaded_gateway_mms_ciphertext(10)
            .unwrap(),
        GatewayMmsCleanup::default(),
        "completed durable markers do not cause an open-time sweep or repeat unlink"
    );
}
