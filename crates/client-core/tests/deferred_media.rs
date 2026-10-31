use crate::common::*;

/// The pre-feature MMS shape, deliberately without any deferred event variants.
#[derive(serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum LegacyEvent {
    MmsMessage {
        message: MessagePayload,
        media: Vec<MediaDescriptor>,
        #[serde(default)]
        media_meta: Vec<MmsMediaMeta>,
    },
}
fn legacy_messages(server: &Server, vault: &Vault) -> Vec<MessageId> {
    use peppy_crypto::{
        EncryptedEnvelope, KeyPurpose, decrypt, derive_purpose_key, derive_root_key,
    };
    let root = derive_root_key(PASSPHRASE, &vault.profile).unwrap();
    let key = derive_purpose_key(&root, &vault.profile, KeyPurpose::Event).unwrap();
    server
        .log
        .iter()
        .filter_map(|envelope| {
            assert_eq!(envelope.purpose, EnvelopePurpose::Event);
            let (nonce, ciphertext) = envelope.ciphertext.split_at(24);
            let encrypted = EncryptedEnvelope {
                nonce: nonce.try_into().unwrap(),
                ciphertext: ciphertext.to_vec(),
            };
            let plain = decrypt(&key, &envelope.aad_bytes().unwrap(), &encrypted).unwrap();
            match serde_json::from_slice::<LegacyEvent>(&plain) {
                Ok(LegacyEvent::MmsMessage {
                    message,
                    media,
                    media_meta,
                }) => {
                    assert_eq!(media.len(), message.record.attachments.len());
                    assert_eq!(media_meta.len(), media.len());
                    for object in media {
                        let value = serde_json::to_value(object).unwrap();
                        assert!(
                            uuid::Uuid::parse_str(value["remote_object_id"].as_str().unwrap())
                                .is_ok()
                        );
                    }
                    Some(message.record.message_id)
                }
                Err(_) => None, // Old authenticated EVENT receivers consume unknown kinds without effects.
            }
        })
        .collect()
}

#[test]
fn deferred_policy_does_not_grant_unknown_attachment_demand() {
    let dir = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "phone", &vault), &vault);
    client.enable_deferred_gateway_media(false, true).unwrap();
    assert!(
        client
            .request_mms_media_sync(&[AttachmentId::new()])
            .is_err()
    );
    assert!(client.pending_outbox().unwrap().is_empty());
}

fn captured_image(
    phone: &Client,
    dir: &tempfile::TempDir,
    imported: bool,
) -> (Captured, AttachmentId, AttachmentId) {
    let input = MmsAcquisitionInput {
        source: MmsSource {
            source_generation: "phone".into(),
            subscription_id: "sim-1".into(),
            provider_message_id: "100".into(),
            provider_thread_id: Some("7".into()),
        },
        direction: Direction::Incoming,
        sender_address: Some(ADDRESS.into()),
        recipients: vec![],
        subject: None,
        body: "caption".into(),
        imported,
        observed_at_ms: 1,
        transaction_id: None,
    };
    let a = phone.begin_mms_acquisition(input).unwrap();
    let file = dir.path().join("image.png");
    image::RgbImage::from_pixel(4, 4, image::Rgb([24, 65, 112]))
        .save(&file)
        .unwrap();
    let original = phone
        .prepare_attachment(&file, "image/png", "original.png")
        .unwrap()
        .attachment_id;
    let preview = phone
        .prepare_attachment(&file, "image/png", "preview.png")
        .unwrap()
        .attachment_id;
    phone
        .set_mms_acquisition_part(&a.acquisition_id, "1", original)
        .unwrap();
    phone
        .set_mms_acquisition_part_metadata(
            &a.acquisition_id,
            "1",
            Some(4),
            Some(4),
            None,
            Some(preview),
        )
        .unwrap();
    (
        phone.complete_mms_acquisition(&a.acquisition_id).unwrap(),
        original,
        preview,
    )
}

#[test]
fn preview_then_manual_original_keeps_one_message_and_legacy_fallback() {
    let dir = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let phone = unlocked(&config(&dir, "phone", &vault), &vault);
    let desk = unlocked(&config(&dir, "desk", &vault), &vault);
    phone.enable_deferred_gateway_media(false, true).unwrap();
    let (message, original, preview) = captured_image(&phone, &dir, false);
    assert_eq!(
        phone
            .pending_gateway_media_uploads()
            .unwrap()
            .iter()
            .map(|o| o.attachment_id)
            .collect::<Vec<_>>(),
        vec![preview]
    );
    assert!(phone.pending_outbox().unwrap().is_empty());
    phone
        .mark_attachment_uploaded(preview, &preview.to_string())
        .unwrap();
    let mut server = Server::default();
    assert_eq!(server.upload(&phone), 1);
    assert!(legacy_messages(&server, &vault).is_empty());
    assert_eq!(server.sync(&desk).quarantined, 0);
    assert_eq!(
        only_message(&desk, message.conversation_id)
            .payload
            .record
            .message_id,
        message.message_id
    );
    assert!(matches!(
        desk.deferred_media_status(original).unwrap(),
        Some(DeferredMediaStatus::Held)
    ));
    assert!(
        !desk
            .pending_downloads()
            .unwrap()
            .iter()
            .any(|o| o.attachment_id == original)
    );
    assert_eq!(
        desk.request_mms_media_sync(&[original]).unwrap().requested,
        vec![original]
    );
    assert_eq!(
        desk.request_mms_media_sync(&[original])
            .unwrap()
            .already_requested,
        vec![original]
    );
    assert_eq!(server.upload(&desk), 1);
    assert_eq!(server.sync(&phone).quarantined, 0);
    assert!(
        phone
            .pending_gateway_media_uploads()
            .unwrap()
            .iter()
            .any(|o| o.attachment_id == original)
    );
    phone
        .mark_attachment_uploaded(original, &original.to_string())
        .unwrap();
    assert_eq!(
        server.upload(&phone),
        2,
        "binding plus original legacy event"
    );
    assert_eq!(server.sync(&desk).quarantined, 0);
    assert_eq!(desk.messages(message.conversation_id).unwrap().len(), 1);
    assert_eq!(legacy_messages(&server, &vault), vec![message.message_id]);
    assert!(matches!(
        desk.deferred_media_status(original).unwrap(),
        Some(DeferredMediaStatus::Available)
    ));
    assert!(
        desk.pending_downloads()
            .unwrap()
            .iter()
            .any(|o| o.attachment_id == original)
    );
    let late = unlocked(&config(&dir, "late", &vault), &vault);
    assert_eq!(server.sync(&late).quarantined, 0);
    assert_eq!(late.messages(message.conversation_id).unwrap().len(), 1);
    let restored = unlocked(&config(&dir, "restored", &vault), &vault);
    let records: Vec<_> = server
        .log
        .iter()
        .enumerate()
        .map(|(i, e)| SnapshotRecord {
            cursor: Cursor(i as u64 + 1),
            envelope: e.clone(),
        })
        .collect();
    let snap = restored
        .begin_snapshot(
            Cursor(records.len() as u64),
            records.len() as u64,
            SnapshotPurpose::Resync,
        )
        .unwrap();
    restored
        .append_snapshot_page(snap.generation, &records)
        .unwrap();
    restored.finish_snapshot(snap.generation).unwrap();
    restored.apply_pending(100).unwrap();
    assert_eq!(restored.messages(message.conversation_id).unwrap().len(), 1);
    assert!(matches!(
        restored.deferred_media_status(original).unwrap(),
        Some(DeferredMediaStatus::Available)
    ));
    assert!(
        !restored.attachment_download_requested(original).unwrap(),
        "historical demand must not reauthorize cellular transfer"
    );
    let reversed = unlocked(&config(&dir, "reversed", &vault), &vault);
    let reverse_server = Server {
        log: server.log.iter().rev().cloned().collect(),
    };
    assert_eq!(
        reverse_server.sync(&reversed).quarantined,
        0,
        "binding before preview must converge"
    );
    assert_eq!(reversed.messages(message.conversation_id).unwrap().len(), 1);
    assert!(matches!(
        reversed.deferred_media_status(original).unwrap(),
        Some(DeferredMediaStatus::Available)
    ));
}

#[test]
fn imported_media_stays_wifi_only_and_demand_batch_is_atomic() {
    let dir = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let phone = unlocked(&config(&dir, "phone", &vault), &vault);
    phone.enable_deferred_gateway_media(false, true).unwrap();
    let (_, _, preview) = captured_image(&phone, &dir, true);
    assert!(phone.pending_gateway_media_uploads().unwrap().is_empty());
    phone.enable_deferred_gateway_media(true, true).unwrap();
    assert!(
        phone
            .pending_gateway_media_uploads()
            .unwrap()
            .iter()
            .any(|o| o.attachment_id == preview)
    );
}

#[test]
fn invalid_member_rolls_back_entire_manual_request() {
    let dir = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let phone = unlocked(&config(&dir, "phone", &vault), &vault);
    phone.enable_deferred_gateway_media(false, true).unwrap();
    let (_, original, preview) = captured_image(&phone, &dir, false);
    phone
        .mark_attachment_uploaded(preview, &preview.to_string())
        .unwrap();
    let desk = unlocked(&config(&dir, "desk", &vault), &vault);
    let mut server = Server::default();
    server.upload(&phone);
    server.sync(&desk);
    assert!(
        desk.request_mms_media_sync(&[original, AttachmentId::new()])
            .is_err()
    );
    assert!(!desk.attachment_download_requested(original).unwrap());
    assert!(desk.pending_outbox().unwrap().is_empty());
    assert!(matches!(
        desk.deferred_media_status(original).unwrap(),
        Some(DeferredMediaStatus::Held)
    ));
}

#[test]
fn default_legacy_sender_keeps_original_dependency_and_no_new_events() {
    let dir = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let phone = unlocked(&config(&dir, "phone", &vault), &vault);
    let (_, original, preview) = captured_image(&phone, &dir, false);
    phone
        .mark_attachment_uploaded(preview, &preview.to_string())
        .unwrap();
    assert!(phone.pending_outbox().unwrap().is_empty());
    assert!(phone.deferred_media_status(original).unwrap().is_none());
    phone
        .mark_attachment_uploaded(original, &original.to_string())
        .unwrap();
    assert_eq!(phone.pending_outbox().unwrap().len(), 1);
}

#[test]
fn cellular_downloads_are_limited_to_active_carrier_work() {
    let dir = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let sender = unlocked(&config(&dir, "sender", &vault), &vault);
    let gateway_config = config(&dir, "gateway", &vault);
    let gateway = unlocked(&gateway_config, &vault);
    let (_, original, preview) = captured_image(&sender, &dir, false);
    sender
        .mark_attachment_uploaded(preview, &preview.to_string())
        .unwrap();
    sender
        .mark_attachment_uploaded(original, &original.to_string())
        .unwrap();
    let mut server = Server::default();
    server.upload(&sender);
    server.sync(&gateway);
    gateway.enable_deferred_gateway_media(false, true).unwrap();
    assert!(
        gateway
            .pending_gateway_media_downloads()
            .unwrap()
            .is_empty()
    );
    sender
        .queue_mms(
            OutgoingMms {
                conversation_id: ConversationId::new(),
                recipients: vec![ADDRESS.into()],
                body: "send this image".into(),
                attachment_ids: vec![original],
                subject: None,
            },
            route(&gateway_config),
        )
        .unwrap();
    server.upload(&sender);
    server.sync(&gateway);
    assert_eq!(
        gateway
            .pending_gateway_media_downloads()
            .unwrap()
            .iter()
            .map(|o| o.attachment_id)
            .collect::<Vec<_>>(),
        vec![original]
    );
    gateway.enable_deferred_gateway_media(true, true).unwrap();
    assert!(
        gateway
            .pending_gateway_media_downloads()
            .unwrap()
            .iter()
            .any(|o| o.attachment_id == preview)
    );
}

#[test]
fn failed_original_sync_is_durable_retryable_and_success_wins() {
    let dir = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let phone = unlocked(&config(&dir, "phone", &vault), &vault);
    let desk_config = config(&dir, "desk", &vault);
    let desk = unlocked(&desk_config, &vault);
    phone.enable_deferred_gateway_media(false, true).unwrap();
    let (_, original, preview) = captured_image(&phone, &dir, false);
    phone
        .mark_attachment_uploaded(preview, &preview.to_string())
        .unwrap();
    let mut server = Server::default();
    server.upload(&phone);
    server.sync(&desk);
    assert!(
        !phone
            .record_gateway_media_failure(original, "transient")
            .unwrap(),
        "ordinary Wi-Fi failures must not publish manual-request failure events"
    );
    assert!(phone.pending_outbox().unwrap().is_empty());
    desk.request_mms_media_sync(&[original]).unwrap();
    server.upload(&desk);
    server.sync(&phone);
    assert!(
        !desk
            .record_gateway_media_failure(original, "transient")
            .unwrap(),
        "only capture gateway reports upload failure"
    );
    assert!(
        phone
            .record_gateway_media_failure(original, "transient")
            .unwrap()
    );
    assert!(
        !phone
            .record_gateway_media_failure(original, "transient")
            .unwrap(),
        "unchanged failures do not enqueue a storm"
    );
    assert_eq!(server.upload(&phone), 1);
    assert_eq!(server.sync(&desk).quarantined, 0);
    assert!(matches!(
        desk.deferred_media_status(original).unwrap(),
        Some(DeferredMediaStatus::Retryable { .. })
    ));
    drop(desk);
    let desk = unlocked(&desk_config, &vault);
    assert!(matches!(
        desk.deferred_media_status(original).unwrap(),
        Some(DeferredMediaStatus::Retryable { .. })
    ));
    assert_eq!(
        desk.request_mms_media_sync(&[original]).unwrap().requested,
        vec![original]
    );
    server.upload(&desk);
    server.sync(&phone);
    phone
        .mark_attachment_uploaded(original, &original.to_string())
        .unwrap();
    server.upload(&phone);
    server.sync(&desk);
    assert!(matches!(
        desk.deferred_media_status(original).unwrap(),
        Some(DeferredMediaStatus::Available)
    ));
}
