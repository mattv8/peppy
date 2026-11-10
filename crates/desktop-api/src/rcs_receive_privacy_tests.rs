//! Desktop RCS views must expose display state only: provider conversation ids, provider message
//! ids, tmp ids and history cursors never reach serialized output.
use crate::{message_window_view, rcs_receive};
use peppy_client_core::{
    Client, ClientConfig, ConversationId, DatabaseKey, DeviceId, Direction, KeyProfile, Message,
    MessageWindowRequest, RcsCapabilityState, RcsIncomingAttachment, RcsLinkStatusEvent,
    RcsMediaSyncState, RcsProjectedRecord, RcsProjectedStatus, RcsStatusApplication,
    VaultCheckHeader, VaultId,
};
use serde_json::Value;
use std::str::FromStr;
use std::time::{SystemTime, UNIX_EPOCH};

const PASSPHRASE: &str = "correct horse battery staple";
/// Vault check fixture for `VAULT_ID` and `PASSPHRASE`, generated once with `peppy-crypto`.
const VAULT_ID: &str = "5f0c1e2a-7d3b-4c8e-9a61-2b4d6e8f0a13";
const PROFILE_JSON: &str = r#"{"crypto_suite":1,"salt":[54,208,209,217,14,82,42,154,194,83,247,255,21,42,176,158],"vault_id":"5f0c1e2a-7d3b-4c8e-9a61-2b4d6e8f0a13","key_epoch":1}"#;
const HEADER_JSON: &str = r#"{"profile":{"crypto_suite":1,"salt":[54,208,209,217,14,82,42,154,194,83,247,255,21,42,176,158],"vault_id":"5f0c1e2a-7d3b-4c8e-9a61-2b4d6e8f0a13","key_epoch":1},"check":{"nonce":[77,20,10,223,176,136,108,85,203,248,129,211,130,3,132,243,206,240,129,129,7,238,114,199],"ciphertext":[119,163,102,119,74,120,192,185,183,202,167,186,64,164,166,104,195,10,123,217,213,97,99,247,187,63,119,163,50,222,158,26,7,58,253,8]}}"#;
const GOOGLE_CONVERSATION_SENTINEL: &str = "desktop-api-google-conversation-sentinel";
const GOOGLE_MESSAGE_SENTINEL: &str = "desktop-api-google-message-sentinel";
const TMP_ID_SENTINEL: &str = "desktop-api-tmp-sentinel";
const HISTORY_CURSOR_SENTINEL: &str = "desktop-api-history-cursor-sentinel";
const SENTINELS: [&str; 7] = [
    GOOGLE_CONVERSATION_SENTINEL,
    GOOGLE_MESSAGE_SENTINEL,
    TMP_ID_SENTINEL,
    HISTORY_CURSOR_SENTINEL,
    "desktop-api-google-media-ref-sentinel",
    "desktop-api-google-media-ref-sentinel-1",
    "desktop-api-google-media-ref-sentinel-2",
];
const DELIVERED_BODY: &str = "RCS payload";
const LATEST_BODY: &str = "RCS latest payload";

struct Fixture {
    _directory: tempfile::TempDir,
    client: Client,
    conversation_id: ConversationId,
    ownership_generation: u64,
}

fn seeded_client() -> Fixture {
    let directory = tempfile::TempDir::new().unwrap();
    let vault_id = VaultId::from_str(VAULT_ID).unwrap();
    let config = ClientConfig {
        database_path: directory.path().join("client.db"),
        vault_id,
        device_id: DeviceId::new(),
    };
    let client = Client::open(config.clone(), DatabaseKey::new(&[7; 32]).unwrap()).unwrap();
    client.set_server_compaction_state(true, true).unwrap();
    let profile: KeyProfile = serde_json::from_str(PROFILE_JSON).unwrap();
    let header: VaultCheckHeader = serde_json::from_str(HEADER_JSON).unwrap();
    client.unlock(&profile, &header, PASSPHRASE).unwrap();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    client
        .publish_rcs_capability(
            &RcsLinkStatusEvent {
                gateway_device_id: config.device_id,
                state: RcsCapabilityState::Linked,
                availability: true,
                health: true,
                phone_responsive: true,
                observed_at_ms: now,
                capability_revision: 1,
                feature_bits: 1,
                linked_since_ms: Some(now),
            },
            &[],
            now,
        )
        .unwrap();
    let conversation_id = ConversationId::new();
    let ownership = client
        .begin_rcs_link_ownership(conversation_id, now - 1)
        .unwrap();
    let generation = ownership.ownership_generation;
    client
        .ingest_rcs_projected_record(
            conversation_id,
            generation,
            RcsProjectedRecord {
                google_message_id: GOOGLE_MESSAGE_SENTINEL.into(),
                google_conversation_id: GOOGLE_CONVERSATION_SENTINEL.into(),
                direction: Direction::Incoming,
                body: DELIVERED_BODY.into(),
                sent_at_ms: Some(now + 1_000),
                tmp_id: Some(TMP_ID_SENTINEL.into()),
                rcs_reported: true,
                e2ee_reported: Some(true),
                attachments: vec![
                    RcsIncomingAttachment {
                        original_index: 0,
                        media_type: "image/jpeg".into(),
                        display_name: "receipt.jpg".into(),
                        plaintext_bytes: 42,
                        too_large: false,
                        media_ref: "desktop-api-google-media-ref-sentinel".into(),
                    },
                    RcsIncomingAttachment {
                        original_index: 1,
                        media_type: "application/pdf".into(),
                        display_name: "invoice.pdf".into(),
                        plaintext_bytes: 84,
                        too_large: false,
                        media_ref: "desktop-api-google-media-ref-sentinel-1".into(),
                    },
                    RcsIncomingAttachment {
                        original_index: 2,
                        media_type: "image/jpeg".into(),
                        display_name: "panorama.jpg".into(),
                        plaintext_bytes: 126,
                        too_large: true,
                        media_ref: "desktop-api-google-media-ref-sentinel-2".into(),
                    },
                ],
            },
        )
        .unwrap();
    client.apply_pending(100).unwrap();
    client
        .apply_rcs_projected_status(
            conversation_id,
            generation,
            RcsStatusApplication {
                google_message_id: GOOGLE_MESSAGE_SENTINEL.into(),
                google_conversation_id: GOOGLE_CONVERSATION_SENTINEL.into(),
                tmp_id: Some(TMP_ID_SENTINEL.into()),
                status: RcsProjectedStatus::Delivered,
            },
        )
        .unwrap();
    client
        .ingest_rcs_projected_record(
            conversation_id,
            generation,
            RcsProjectedRecord {
                google_message_id: "google-message-latest".into(),
                google_conversation_id: GOOGLE_CONVERSATION_SENTINEL.into(),
                direction: Direction::Incoming,
                body: LATEST_BODY.into(),
                sent_at_ms: Some(now + 2_000),
                tmp_id: None,
                rcs_reported: true,
                e2ee_reported: None,
                attachments: vec![],
            },
        )
        .unwrap();
    client.apply_pending(100).unwrap();
    Fixture {
        _directory: directory,
        client,
        conversation_id,
        ownership_generation: generation,
    }
}

#[test]
fn incoming_rcs_placeholders_project_each_receive_state_without_compressing_indices() {
    for state in [
        RcsMediaSyncState::Downloading,
        RcsMediaSyncState::QuotaFull,
        RcsMediaSyncState::Failed,
        RcsMediaSyncState::Expired,
    ] {
        let fixture = seeded_client();
        let messages = fixture.client.messages(fixture.conversation_id).unwrap();
        let message = find_body(&messages, DELIVERED_BODY);
        if state != RcsMediaSyncState::Downloading {
            fixture
                .client
                .mark_rcs_media_download_state(message.payload.record.message_id, 0, state)
                .unwrap();
        }
        let view = serde_json::to_value(
            rcs_receive::message_view(&fixture.client, message, vec![]).unwrap(),
        )
        .unwrap();
        assert_eq!(view["body"], DELIVERED_BODY);
        assert_eq!(view["attachments"][0]["index"], 0);
        assert_eq!(view["attachments"][0]["rcsState"], state.code());
        assert_eq!(view["attachments"][0]["name"], "receipt.jpg");
        assert_eq!(view["attachments"][0]["mediaType"], "image/jpeg");
        assert_eq!(view["attachments"][0]["byteSize"], 42);
    }

    let fixture = seeded_client();
    let messages = fixture.client.messages(fixture.conversation_id).unwrap();
    let message = find_body(&messages, DELIVERED_BODY);
    let message_id = message.payload.record.message_id;
    fixture
        .client
        .mark_rcs_media_download_state(message_id, 1, RcsMediaSyncState::QuotaFull)
        .unwrap();
    let view =
        serde_json::to_value(rcs_receive::message_view(&fixture.client, message, vec![]).unwrap())
            .unwrap();
    assert_eq!(view["attachments"][0]["index"], 0);
    assert_eq!(view["attachments"][1]["index"], 1);
    assert_eq!(view["attachments"][1]["rcsState"], "quota_full");
    assert_eq!(view["attachments"][2]["index"], 2);
    assert_eq!(view["attachments"][2]["rcsState"], "too_large");
}

#[test]
fn remote_sparse_stored_attachment_projects_beside_failed_and_oversized_slots() {
    let fixture = seeded_client();
    let messages = fixture.client.messages(fixture.conversation_id).unwrap();
    let id = find_body(&messages, DELIVERED_BODY)
        .payload
        .record
        .message_id;
    fixture
        .client
        .mark_rcs_media_download_state(id, 0, RcsMediaSyncState::Failed)
        .unwrap();
    let token = match fixture
        .client
        .admit_rcs_media_download(id, 1, Some(u64::MAX))
        .unwrap()
    {
        peppy_client_core::ReceiveDownloadAdmission::Held { token } => token,
        other => panic!("unexpected admission: {other:?}"),
    };
    let path = fixture._directory.path().join("invoice.pdf");
    std::fs::write(&path, [7_u8; 84]).unwrap();
    let attachment = fixture
        .client
        .import_rcs_media_download(id, 1, &token, &path)
        .unwrap();
    fixture
        .client
        .mark_attachment_uploaded(attachment.attachment_id, &DeviceId::new().to_string())
        .unwrap();
    fixture.client.finalize_rcs_media_download(id, 1).unwrap();

    let remote = Client::open(
        ClientConfig {
            database_path: fixture._directory.path().join("remote.db"),
            vault_id: VaultId::from_str(VAULT_ID).unwrap(),
            device_id: DeviceId::new(),
        },
        DatabaseKey::new(&[7; 32]).unwrap(),
    )
    .unwrap();
    remote
        .unlock(
            &serde_json::from_str(PROFILE_JSON).unwrap(),
            &serde_json::from_str(HEADER_JSON).unwrap(),
            PASSPHRASE,
        )
        .unwrap();
    for (index, envelope) in fixture.client.pending_outbox().unwrap().iter().enumerate() {
        remote
            .ingest(envelope, peppy_client_core::Cursor(index as u64 + 1))
            .unwrap();
    }
    assert_eq!(remote.apply_pending(100).unwrap().quarantined, 0);
    let messages = remote.messages(fixture.conversation_id).unwrap();
    let message = find_body(&messages, DELIVERED_BODY);
    let attachments = message
        .payload
        .record
        .attachments
        .iter()
        .map(|reference| {
            crate::attachment_view(
                &remote.attachment_info(reference.attachment_id).unwrap(),
                None,
                Some("fixture-preview".into()),
            )
        })
        .collect();
    let view =
        serde_json::to_value(rcs_receive::message_view(&remote, message, attachments).unwrap())
            .unwrap();
    let slots = view["attachments"].as_array().unwrap();
    assert_eq!(slots.len(), 3);
    assert_eq!(slots[0]["index"], 0);
    assert_eq!(slots[0]["rcsState"], "failed");
    assert_eq!(slots[1]["index"], 1);
    assert_eq!(slots[1]["rcsState"], "stored");
    assert_eq!(slots[1]["id"], attachment.attachment_id.to_string());
    assert_eq!(slots[1]["name"], "invoice.pdf");
    assert_eq!(slots[1]["previewUrl"], "fixture-preview");
    assert_eq!(slots[2]["index"], 2);
    assert_eq!(slots[2]["rcsState"], "too_large");
    assert_no_sentinels("remote sparse media", &view);
    assert!(remote.pending_rcs_media_receive_tasks().unwrap().is_empty());
}

#[test]
fn rcs_media_never_receives_a_carrier_mms_split_plan() {
    let fixture = seeded_client();
    for mime in ["application/pdf", "video/mp4", "image/jpeg"] {
        let path = fixture._directory.path().join("outgoing-media");
        std::fs::write(&path, vec![7_u8; 1024 * 1024]).unwrap();
        let attachment = fixture
            .client
            .prepare_attachment(&path, mime, "media")
            .unwrap();
        let input = crate::compose::DraftInput {
            id: String::new(),
            conversation_id: fixture.conversation_id.to_string(),
            text: "caption".into(),
            recipient_ids: vec!["+14155552671".into()],
            attachment_ids: vec![attachment.attachment_id.to_string()],
            gateway_id: Some(DeviceId::new().to_string()),
            sim_id: Some("sim".into()),
            expected_revision: "0".into(),
            group_name: None,
            expected_member_revision: None,
            group_create_confirm_duplicate: None,
        };
        let gateway = crate::GatewayView {
            id: input.gateway_id.clone().unwrap(),
            name: "Gateway".into(),
            sim_id: "sim".into(),
            online: true,
            simulated: false,
            supports_sms: true,
            supports_mms: true,
            availability: crate::GatewayAvailability::Connected,
            last_seen_age_seconds: None,
            presence: None,
            wake_route: false,
            capability_note: None,
            mms_content_version: Some(4),
            mms_max_bytes: Some(300 * 1024),
            mms_limit_source: None,
            mms_max_recipients: None,
        };
        let result =
            crate::compose::prepared_draft_send_parts(&fixture.client, &input, Some(&[gateway]));
        assert!(matches!(result, Ok(None)));
    }
}

fn find_body<'a>(messages: &'a [Message], body: &str) -> &'a Message {
    messages
        .iter()
        .find(|message| message.payload.body == body)
        .unwrap_or_else(|| panic!("missing message with body {body:?}"))
}

fn assert_no_sentinels(path: &str, value: &Value) {
    let serialized = value.to_string();
    for sentinel in SENTINELS {
        assert!(
            !serialized.contains(sentinel),
            "{path} leaked {sentinel}: {serialized}"
        );
    }
}

/// Positive check: the delivered RCS message keeps transport and status in the view.
fn assert_delivered_rcs(path: &str, messages: &[Value]) {
    let message = messages
        .iter()
        .find(|message| message["body"] == DELIVERED_BODY)
        .unwrap_or_else(|| panic!("{path}: missing delivered message: {messages:?}"));
    assert_eq!(message["transport"], "rcs", "{path}: {message}");
    assert_eq!(message["status"], "delivery-confirmed", "{path}: {message}");
    assert_eq!(message["rcsReported"], true, "{path}: {message}");
}

#[test]
fn desktop_rcs_views_do_not_leak_provider_identity() {
    let fixture = seeded_client();
    let client = &fixture.client;
    let conversation_id = fixture.conversation_id;
    let all = client.messages(conversation_id).unwrap();
    let delivered_id = find_body(&all, DELIVERED_BODY).payload.record.message_id;
    let latest_id = find_body(&all, LATEST_BODY).payload.record.message_id;

    let snapshot: Vec<Value> = all
        .iter()
        .map(|message| {
            serde_json::to_value(rcs_receive::message_view(client, message, vec![]).unwrap())
                .unwrap()
        })
        .collect();
    let history: Vec<Value> = client
        .messages_page(conversation_id, Some(latest_id), 50)
        .unwrap()
        .messages
        .iter()
        .map(|message| {
            serde_json::to_value(rcs_receive::message_view(client, message, vec![]).unwrap())
                .unwrap()
        })
        .collect();
    let window = client
        .load_message_window(MessageWindowRequest::Around {
            conversation_id: conversation_id.to_string(),
            message_id: delivered_id.to_string(),
        })
        .unwrap();
    let window = serde_json::to_value(message_window_view(window, |message| {
        rcs_receive::message_view(client, &message, vec![]).unwrap()
    }))
    .unwrap();
    let window_messages = window["messages"].as_array().unwrap().clone();
    let metadata = serde_json::to_value(
        client
            .rcs_conversation_metadata(conversation_id)
            .unwrap()
            .expect("RCS conversation metadata"),
    )
    .unwrap();

    assert_delivered_rcs("snapshot", &snapshot);
    assert_delivered_rcs("history", &history);
    assert_delivered_rcs("window", &window_messages);
    let reported_without_status = find_body(&all, LATEST_BODY);
    let reported_without_status = serde_json::to_value(
        rcs_receive::message_view(client, reported_without_status, vec![]).unwrap(),
    )
    .unwrap();
    assert_eq!(reported_without_status["rcsReported"], true);
    assert!(reported_without_status.get("status").is_none());
    assert_eq!(metadata["rcs_encryption"], true, "{metadata}");
    for (path, value) in [
        ("snapshot", Value::Array(snapshot)),
        ("history", Value::Array(history)),
        ("window", window),
        ("metadata", metadata),
    ] {
        assert_no_sentinels(path, &value);
    }
}

#[test]
fn read_status_does_not_reuse_delivery_timestamp() {
    let fixture = seeded_client();
    let mut delivered = find_body(
        &fixture.client.messages(fixture.conversation_id).unwrap(),
        DELIVERED_BODY,
    )
    .clone();
    delivered.status_at_ms = Some(12_345);

    fixture
        .client
        .apply_rcs_projected_status(
            fixture.conversation_id,
            fixture.ownership_generation,
            RcsStatusApplication {
                google_message_id: GOOGLE_MESSAGE_SENTINEL.into(),
                google_conversation_id: GOOGLE_CONVERSATION_SENTINEL.into(),
                tmp_id: Some(TMP_ID_SENTINEL.into()),
                status: RcsProjectedStatus::Read,
            },
        )
        .unwrap();

    let view = rcs_receive::message_view(&fixture.client, &delivered, vec![]).unwrap();
    assert_eq!(view.status, Some("read"));
    assert_eq!(view.status_at, None);
    assert!(view.rcs_reported);
}
