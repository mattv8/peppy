use super::*;
use peppy_client_core::{
    ClientConfig, DatabaseKey, Direction, RcsCapabilityState, RcsLinkStatusEvent,
    RcsProjectedRecord, RcsProjectedStatus, RcsStatusApplication,
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
        .begin_rcs_link_ownership(conversation_id, SENTINELS[3], now - 1)
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
                history_token: Some(SENTINELS[3].into()),
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
                history_token: None,
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
