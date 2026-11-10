use super::*;
use peppy_client_core::{
    ClientConfig, DatabaseKey, Direction, IncomingSms, MmsAcquisitionInput, MmsSource,
    RcsCapabilityState, RcsLinkStatusEvent, RcsProjectedRecord, RcsProjectedStatus,
    RcsProviderDetector, RcsStatusApplication,
};
use peppy_crypto::{KeyProfile, create_vault_check_header, derive_root_key};
use serde_json::{Value, json};
use std::time::{SystemTime, UNIX_EPOCH};

const PASSPHRASE: &str = "correct horse battery staple";
const SENTINELS: [&str; 4] = [
    "google-conversation-sentinel",
    "google-message-sentinel",
    "tmp-sentinel",
    "cursor-sentinel",
];

fn core() -> (tempfile::TempDir, Client, ConversationId, u64, MessageId) {
    let directory = tempfile::TempDir::new().unwrap();
    let vault_id = VaultId::new();
    let config = ClientConfig {
        database_path: directory.path().join("client.db"),
        vault_id,
        device_id: DeviceId::new(),
    };
    let client = Client::open(config.clone(), DatabaseKey::new(&[7; 32]).unwrap()).unwrap();
    client.set_server_compaction_state(true, true).unwrap();
    let profile = KeyProfile::new(vault_id.0, 1).unwrap();
    let root = derive_root_key(PASSPHRASE, &profile).unwrap();
    let header = create_vault_check_header(&root, profile.clone()).unwrap();
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
    let outcome = client
        .ingest_rcs_projected_record(
            conversation_id,
            ownership.ownership_generation,
            RcsProjectedRecord {
                google_message_id: SENTINELS[1].into(),
                google_conversation_id: SENTINELS[0].into(),
                direction: Direction::Incoming,
                body: "RCS payload".into(),
                sent_at_ms: Some(now + 1_000),
                tmp_id: Some(SENTINELS[2].into()),
                rcs_reported: true,
                e2ee_reported: Some(true),
            },
        )
        .unwrap();
    assert!(
        matches!(outcome, peppy_client_core::RcsReceiveOutcome::Inserted),
        "{outcome:?}"
    );
    client.apply_pending(100).unwrap();
    let first_message_id = client
        .messages(conversation_id)
        .unwrap()
        .pop()
        .unwrap()
        .payload
        .record
        .message_id;
    client
        .apply_rcs_projected_status(
            conversation_id,
            ownership.ownership_generation,
            RcsStatusApplication {
                google_message_id: SENTINELS[1].into(),
                google_conversation_id: SENTINELS[0].into(),
                tmp_id: Some(SENTINELS[2].into()),
                status: RcsProjectedStatus::Delivered,
            },
        )
        .unwrap();
    client
        .ingest_rcs_projected_record(
            conversation_id,
            ownership.ownership_generation,
            RcsProjectedRecord {
                google_message_id: "google-message-latest".into(),
                google_conversation_id: SENTINELS[0].into(),
                direction: Direction::Incoming,
                body: "RCS latest payload".into(),
                sent_at_ms: Some(now + 2_000),
                tmp_id: None,
                rcs_reported: false,
                e2ee_reported: None,
            },
        )
        .unwrap();
    client.apply_pending(100).unwrap();
    client
        .apply_rcs_projected_status(
            conversation_id,
            ownership.ownership_generation,
            RcsStatusApplication {
                google_message_id: "google-message-latest".into(),
                google_conversation_id: SENTINELS[0].into(),
                tmp_id: None,
                status: RcsProjectedStatus::Delivered,
            },
        )
        .unwrap();
    let message_id = client
        .messages_page(conversation_id, None, 1)
        .unwrap()
        .messages
        .pop()
        .unwrap()
        .payload
        .record
        .message_id;
    assert_ne!(message_id, first_message_id);
    (
        directory,
        client,
        conversation_id,
        ownership.ownership_generation,
        message_id,
    )
}

fn browser(client: Client) -> BrowserCore {
    let mut browser = BrowserCore::new(tempfile::TempDir::new().unwrap().keep());
    browser.client = Some(client);
    browser
}

fn dispatch(browser: &mut BrowserCore, command: &str, args: Value) -> Value {
    let response: Value = serde_json::from_str(
        &browser.dispatch(&json!({"command": command, "args": args}).to_string()),
    )
    .unwrap();
    response["value"].clone()
}

fn assert_no_sentinels(value: &Value) {
    let serialized = value.to_string();
    for sentinel in SENTINELS {
        assert!(
            !serialized.contains(sentinel),
            "leaked {sentinel}: {serialized}"
        );
    }
}

#[test]
fn rcs_window_projection_failure_returns_closed_error_without_a_success_view() {
    let (directory, client, conversation_id, _, message_id) = core();
    let mut browser = browser(client);
    let args =
        json!({"direction": "around", "conversationId": conversation_id, "messageId": message_id});
    let healthy = dispatch(&mut browser, "load_message_window", args.clone());
    assert_eq!(healthy["messages"][0]["transport"], "rcs");
    assert_eq!(healthy["messages"][0]["status"], "delivery-confirmed");

    let connection = rusqlite::Connection::open(directory.path().join("client.db")).unwrap();
    connection
        .pragma_update(None, "key", format!("x'{}'", "07".repeat(32)))
        .unwrap();
    connection
        .execute_batch("DROP TABLE rcs_message_status")
        .unwrap();
    let version_before: i64 = connection
        .query_row("PRAGMA data_version", [], |row| row.get(0))
        .unwrap();
    let messages_before = format!(
        "{:?}",
        browser.client().unwrap().messages(conversation_id).unwrap()
    );
    let request: peppy_desktop_api::MessageWindowRequestView =
        serde_json::from_value(args.clone()).unwrap();
    let base_window = browser
        .client()
        .unwrap()
        .load_message_window(request.into())
        .unwrap();
    assert_eq!(
        base_window.messages.len(),
        healthy["messages"].as_array().unwrap().len()
    );

    let response: Value = serde_json::from_str(
        &browser.dispatch(&json!({"command": "load_message_window", "args": args}).to_string()),
    )
    .unwrap();
    assert_eq!(
        response,
        json!({"ok": false, "error": {"code": "core", "message": "The local core rejected the operation."}})
    );
    assert_no_sentinels(&response);
    assert_eq!(
        format!(
            "{:?}",
            browser.client().unwrap().messages(conversation_id).unwrap()
        ),
        messages_before
    );
    let version_after: i64 = connection
        .query_row("PRAGMA data_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version_after, version_before);
}

#[test]
fn rcs_snapshot_history_and_window_project_status_without_leaking_identity() {
    let (_directory, client, conversation_id, _, message_id) = core();
    let mut browser = browser(client);
    let snapshot = dispatch(
        &mut browser,
        "snapshot",
        json!({"conversationId": conversation_id}),
    );
    let history = dispatch(
        &mut browser,
        "load_history",
        json!({"conversationId": conversation_id, "beforeMessageId": message_id}),
    );
    let window = dispatch(
        &mut browser,
        "load_message_window",
        json!({"direction": "around", "conversationId": conversation_id, "messageId": message_id}),
    );
    for (path, message) in [
        ("snapshot", &snapshot["conversations"][0]["messages"][0]),
        ("history", &history["messages"][0]),
        ("window", &window["messages"][0]),
    ] {
        assert_eq!(message["transport"], "rcs", "{path}: {message}");
        assert_eq!(message["status"], "delivery-confirmed");
        assert_eq!(message["rcsReported"], true);
    }
    for value in [&snapshot, &history, &window] {
        assert_no_sentinels(value);
    }
}

const MATRIX_STATUSES: [(RcsProjectedStatus, &str); 5] = [
    (RcsProjectedStatus::Sending, "submitted"),
    (RcsProjectedStatus::Sent, "sent"),
    (RcsProjectedStatus::Failed, "failed-confirmed"),
    (RcsProjectedStatus::Delivered, "delivery-confirmed"),
    (RcsProjectedStatus::Read, "read"),
];

fn unlocked_client(directory: &tempfile::TempDir, device_id: DeviceId) -> Client {
    let vault_id = VaultId::new();
    let config = ClientConfig {
        database_path: directory.path().join("client.db"),
        vault_id,
        device_id,
    };
    let client = Client::open(config, DatabaseKey::new(&[7; 32]).unwrap()).unwrap();
    client.set_server_compaction_state(true, true).unwrap();
    let profile = KeyProfile::new(vault_id.0, 1).unwrap();
    let root = derive_root_key(PASSPHRASE, &profile).unwrap();
    let header = create_vault_check_header(&root, profile.clone()).unwrap();
    client.unlock(&profile, &header, PASSPHRASE).unwrap();
    client
}

fn find_conversation(snapshot: &Value, conversation_id: ConversationId) -> &Value {
    snapshot["conversations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["id"] == conversation_id.to_string())
        .unwrap()
}

fn find_message(messages: &Value, message_id: MessageId) -> &Value {
    messages
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["id"] == message_id.to_string())
        .unwrap_or_else(|| panic!("{message_id} missing from {messages}"))
}

#[test]
fn browser_projections_preserve_media_meta_on_all_paths() {
    let directory = tempfile::TempDir::new().unwrap();
    let client = unlocked_client(&directory, DeviceId::new());
    let original = directory.path().join("photo.png");
    std::fs::write(&original, b"photo bytes").unwrap();
    let attachment = client
        .prepare_attachment(&original, "image/png", "photo.png")
        .unwrap()
        .attachment_id;
    let thumbnail = client
        .prepare_attachment(&original, "image/png", "thumb.png")
        .unwrap()
        .attachment_id;
    let acquisition = client
        .begin_mms_acquisition(MmsAcquisitionInput {
            source: MmsSource {
                source_generation: "media-meta".into(),
                subscription_id: "sim-1".into(),
                provider_message_id: "mms-media-meta".into(),
                provider_thread_id: Some("thread-media-meta".into()),
            },
            direction: Direction::Incoming,
            sender_address: Some("+15555550100".into()),
            recipients: vec![],
            subject: None,
            body: "picture".into(),
            imported: false,
            observed_at_ms: 1,
            transaction_id: None,
        })
        .unwrap();
    client
        .set_mms_acquisition_part(&acquisition.acquisition_id, "original", attachment)
        .unwrap();
    client
        .set_mms_acquisition_part_metadata(
            &acquisition.acquisition_id,
            "original",
            Some(480),
            Some(320),
            None,
            Some(thumbnail),
        )
        .unwrap();
    let mms = client
        .complete_mms_acquisition(&acquisition.acquisition_id)
        .unwrap();
    let sms = client
        .capture_incoming(IncomingSms {
            conversation_id: Some(mms.conversation_id),
            sender_address: "+15555550100".into(),
            body: "plain text".into(),
            provider_message_id: Some("sms-media-meta".into()),
            imported: false,
        })
        .unwrap();
    client.apply_pending(100).unwrap();
    let conversation = mms.conversation_id;
    let mut browser = browser(client);

    let snapshot = dispatch(
        &mut browser,
        "snapshot",
        json!({"conversationId": conversation}),
    );
    let history = dispatch(
        &mut browser,
        "load_history",
        json!({"conversationId": conversation, "beforeMessageId": sms.message_id}),
    );
    let window = dispatch(
        &mut browser,
        "load_message_window",
        json!({"direction": "around", "conversationId": conversation, "messageId": mms.message_id}),
    );

    let snapshot_mms = find_message(
        &find_conversation(&snapshot, conversation)["messages"],
        mms.message_id,
    );
    let expected = &snapshot_mms["attachments"][0]["mediaMeta"];
    assert_eq!(expected["width"], 480, "{snapshot_mms}");
    assert_eq!(expected["height"], 320, "{snapshot_mms}");
    assert!(!expected["thumbnail"].is_null(), "{snapshot_mms}");
    for (path, messages) in [
        ("history", &history["messages"]),
        ("window", &window["messages"]),
    ] {
        let message = find_message(messages, mms.message_id);
        assert_eq!(
            &message["attachments"][0]["mediaMeta"], expected,
            "{path}: {message}"
        );
    }
    let sms_view = find_message(
        &find_conversation(&snapshot, conversation)["messages"],
        sms.message_id,
    );
    assert_eq!(sms_view["attachments"], json!([]), "{sms_view}");
    assert_eq!(find_message(&window["messages"], sms.message_id), sms_view);
}

#[test]
fn browser_rcs_status_matrix() {
    for (status, label) in MATRIX_STATUSES {
        for encryption in [Some(true), Some(false), None] {
            let (_directory, client, conversation, generation) = rcs_conversation(true, encryption);
            client
                .apply_rcs_projected_status(
                    conversation,
                    generation,
                    RcsStatusApplication {
                        google_message_id: "matrix-message".into(),
                        google_conversation_id: "matrix-conversation".into(),
                        tmp_id: None,
                        status,
                    },
                )
                .unwrap();
            let mut browser = browser(client);
            let snapshot = dispatch(
                &mut browser,
                "snapshot",
                json!({"conversationId": conversation}),
            );
            let view = find_conversation(&snapshot, conversation);
            let message = &view["messages"][0];
            assert_eq!(message["transport"], "rcs", "{label}: {message}");
            assert_eq!(message["status"], label, "{message}");
            assert_eq!(message["rcsReported"], true, "{message}");
            assert_optional_bool(&view["rcsEncryption"], encryption);
            assert!(view.get("rcsHistoryGap").is_none(), "{view}");
        }
    }
}

#[test]
fn browser_rcs_unreported_message_without_status_row_projects_no_status() {
    let (_directory, client, conversation, _) = rcs_conversation(false, None);
    let mut browser = browser(client);
    let snapshot = dispatch(
        &mut browser,
        "snapshot",
        json!({"conversationId": conversation}),
    );
    let view = find_conversation(&snapshot, conversation);
    let message = &view["messages"][0];
    assert_eq!(message["transport"], "rcs", "{message}");
    assert!(message.get("status").is_none(), "{message}");
    assert!(message.get("rcsReported").is_none(), "{message}");
    assert!(view.get("rcsEncryption").is_none(), "{view}");
    assert!(view.get("rcsHistoryGap").is_none(), "{view}");
}

#[test]
fn browser_rcs_reported_message_without_status_row_preserves_reported_fact() {
    let (_directory, client, conversation, generation) = rcs_conversation(true, None);
    let message_id = client.messages(conversation).unwrap()[0]
        .payload
        .record
        .message_id;
    client
        .ingest_rcs_projected_record(
            conversation,
            generation,
            RcsProjectedRecord {
                google_message_id: "later-message".into(),
                google_conversation_id: "matrix-conversation".into(),
                direction: Direction::Incoming,
                body: "later body".into(),
                sent_at_ms: Some(now_ms() + 1_000),
                tmp_id: None,
                rcs_reported: false,
                e2ee_reported: None,
            },
        )
        .unwrap();
    client.apply_pending(100).unwrap();
    let later_id = client.messages(conversation).unwrap()[1]
        .payload
        .record
        .message_id;
    let mut browser = browser(client);
    let snapshot = dispatch(
        &mut browser,
        "snapshot",
        json!({"conversationId": conversation}),
    );
    let history = dispatch(
        &mut browser,
        "load_history",
        json!({"conversationId": conversation, "beforeMessageId": later_id}),
    );
    let window = dispatch(
        &mut browser,
        "load_message_window",
        json!({"direction": "around", "conversationId": conversation, "messageId": message_id}),
    );
    for (path, message) in [
        (
            "snapshot",
            find_message(
                &find_conversation(&snapshot, conversation)["messages"],
                message_id,
            ),
        ),
        ("history", find_message(&history["messages"], message_id)),
        ("window", find_message(&window["messages"], message_id)),
    ] {
        assert_eq!(message["transport"], "rcs", "{path}: {message}");
        assert!(message.get("status").is_none(), "{path}: {message}");
        assert_eq!(message["rcsReported"], true, "{path}: {message}");
    }
}

#[test]
fn browser_rcs_history_gap_is_projected_after_detector_overflow() {
    let (_directory, client, conversation, generation) = rcs_conversation(true, Some(true));
    for provider_row_id in 0..=1_024 {
        client
            .record_rcs_provider_detector(
                conversation,
                generation,
                RcsProviderDetector {
                    provider_row_id,
                    provider_thread_id: 1,
                    provider_type: 1,
                    observed_at_ms: provider_row_id,
                    observed_generation: generation,
                },
            )
            .unwrap();
    }
    client
        .ingest_rcs_projected_record(
            conversation,
            generation,
            RcsProjectedRecord {
                google_message_id: "gap-message".into(),
                google_conversation_id: "matrix-conversation".into(),
                direction: Direction::Incoming,
                body: "gap body".into(),
                sent_at_ms: Some(now_ms() + 3_000),
                tmp_id: None,
                rcs_reported: true,
                e2ee_reported: Some(true),
            },
        )
        .unwrap();
    client.apply_pending(100).unwrap();
    let mut browser = browser(client);
    let snapshot = dispatch(
        &mut browser,
        "snapshot",
        json!({"conversationId": conversation}),
    );
    let view = find_conversation(&snapshot, conversation);
    assert_eq!(view["rcsHistoryGap"], true, "{view}");
    assert_eq!(view["rcsEncryption"], true, "{view}");
}

fn assert_optional_bool(value: &Value, expected: Option<bool>) {
    match expected {
        Some(expected) => assert_eq!(value, &json!(expected)),
        None => assert!(
            value.is_null(),
            "encryption reported without a fact: {value}"
        ),
    }
}

/// One RCS conversation with a single inbound message whose `rcs_reported` and encryption
/// facts are supplied by the caller; the message has no status row until a test applies one.
fn rcs_conversation(
    rcs_reported: bool,
    encryption: Option<bool>,
) -> (tempfile::TempDir, Client, ConversationId, u64) {
    let directory = tempfile::TempDir::new().unwrap();
    let device = DeviceId::new();
    let client = unlocked_client(&directory, device);
    let now = now_ms();
    client
        .publish_rcs_capability(
            &RcsLinkStatusEvent {
                gateway_device_id: device,
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
    let conversation = ConversationId::new();
    let ownership = client
        .begin_rcs_link_ownership(conversation, now - 1)
        .unwrap();
    client
        .ingest_rcs_projected_record(
            conversation,
            ownership.ownership_generation,
            RcsProjectedRecord {
                google_message_id: "matrix-message".into(),
                google_conversation_id: "matrix-conversation".into(),
                direction: Direction::Incoming,
                body: "matrix body".into(),
                sent_at_ms: Some(now + 1_000),
                tmp_id: None,
                rcs_reported,
                e2ee_reported: encryption,
            },
        )
        .unwrap();
    client.apply_pending(100).unwrap();
    (
        directory,
        client,
        conversation,
        ownership.ownership_generation,
    )
}
