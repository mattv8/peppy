//! Desktop RCS views must expose display state only: provider conversation ids, provider message
//! ids, tmp ids and history cursors never reach serialized output.
use crate::{message_window_view, rcs_receive};
use peppy_client_core::{
    Client, ClientConfig, ConversationId, DatabaseKey, DeviceId, Direction, KeyProfile, Message,
    MessageWindowRequest, RcsCapabilityState, RcsLinkStatusEvent, RcsProjectedRecord,
    RcsProjectedStatus, RcsStatusApplication, VaultCheckHeader, VaultId,
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
const SENTINELS: [&str; 4] = [
    GOOGLE_CONVERSATION_SENTINEL,
    GOOGLE_MESSAGE_SENTINEL,
    TMP_ID_SENTINEL,
    HISTORY_CURSOR_SENTINEL,
];
const DELIVERED_BODY: &str = "RCS payload";
const LATEST_BODY: &str = "RCS latest payload";

struct Fixture {
    _directory: tempfile::TempDir,
    client: Client,
    conversation_id: ConversationId,
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
        .begin_rcs_link_ownership(conversation_id, HISTORY_CURSOR_SENTINEL, now - 1)
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
                history_token: Some(HISTORY_CURSOR_SENTINEL.into()),
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
                rcs_reported: false,
                e2ee_reported: None,
                history_token: None,
            },
        )
        .unwrap();
    client.apply_pending(100).unwrap();
    Fixture {
        _directory: directory,
        client,
        conversation_id,
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
