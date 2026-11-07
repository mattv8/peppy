use crate::{
    MobileBindingsError, NativeClient, NativeOpenConfig, create_vault_material, open_native_client,
    with_client,
};
use peppy_client_core::{DeviceId, RcsCapabilityState, RcsLinkStatusEvent, RcsProviderDetector};
use serde_json::json;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use uuid::Uuid;

const PASSPHRASE: &str = "correct horse battery staple";

// Verbatim record object from gmlink/receive_test.go: TestProjectRcsMessage's JSON assertion.
const GO_RECORD: &str = r#"{"google_message_id":"message-1","google_conversation_id":"conversation-1","direction":"outgoing","body":"synthetic body","sent_at_ms":1735689600123,"status":"delivered","tmp_id":"tmp-1","rcs_reported":true}"#;

fn go_incoming(status: &str) -> String {
    let mut value: serde_json::Value = serde_json::from_str(GO_RECORD).unwrap();
    value["direction"] = json!("incoming");
    value["status"] = json!(status);
    value["cursor"] = json!("AAAAAAAAAAFtZXNzYWdlLTE");
    value.to_string()
}

fn history_token(fixture: &Fixture) -> Option<String> {
    with_client(&fixture.client.client, |core| {
        Ok(core
            .rcs_receive_state(fixture.conversation_id.parse().unwrap())?
            .unwrap()
            .history_token)
    })
    .unwrap()
}

#[test]
fn go_wire_record_ingests_status_replays_and_checkpoints_only_explicitly() {
    let fixture = fixture();
    let original_token = history_token(&fixture);
    for (expected, expected_status) in [
        ("inserted", "status_applied"),
        ("duplicate", "status_unchanged"),
    ] {
        let result = fixture
            .client
            .gateway_ingest_go_rcs_record_json(
                fixture.conversation_id.clone(),
                fixture.generation,
                go_incoming("delivered"),
            )
            .unwrap();
        assert_eq!(result.record_outcome, expected);
        assert_eq!(result.status_outcome.as_deref(), Some(expected_status));
        assert_eq!(result.cursor.as_deref(), Some("AAAAAAAAAAFtZXNzYWdlLTE"));
        assert_eq!(history_token(&fixture), original_token);
        let output = format!("{result:?}");
        for identifier in ["message-1", "conversation-1", "tmp-1", "synthetic body"] {
            assert!(!output.contains(identifier));
        }
    }
    assert_eq!(
        fixture
            .client
            .messages(fixture.conversation_id.clone())
            .unwrap()[0]
            .rcs_status
            .as_deref(),
        Some("delivered")
    );
    assert_eq!(
        fixture
            .client
            .gateway_checkpoint_rcs_history(
                fixture.conversation_id.clone(),
                fixture.generation,
                "AAAAAAAAAAFtZXNzYWdlLTE".into()
            )
            .unwrap(),
        "recorded"
    );
    assert_eq!(
        history_token(&fixture).as_deref(),
        Some("AAAAAAAAAAFtZXNzYWdlLTE")
    );
}

#[test]
fn go_wire_record_empty_status_and_original_go_sample_are_accepted() {
    let fixture = fixture();
    let result = fixture
        .client
        .gateway_ingest_go_rcs_record_json(
            fixture.conversation_id.clone(),
            fixture.generation,
            go_incoming(""),
        )
        .unwrap();
    assert_eq!(result.record_outcome, "inserted");
    assert_eq!(result.status_outcome, None);
    // Go always emits the status key. Empty string means no inline application;
    // omitting it is not Go's wire shape and is rejected by the required-key test.
    let fixture = self::fixture();
    let result = fixture
        .client
        .gateway_ingest_go_rcs_record_json(
            fixture.conversation_id.clone(),
            fixture.generation,
            GO_RECORD.into(),
        )
        .unwrap();
    assert_eq!(result.record_outcome, "unmatched_outgoing");
    assert_eq!(result.status_outcome.as_deref(), Some("status_applied"));
    assert_eq!(result.cursor, None);
}

#[test]
fn go_exported_receive_record_ingests_through_mobile_boundary() {
    let Ok(path) = std::env::var("PEPPY_RCS_GO_RECEIVE_RECORD_PATH") else {
        return;
    };
    let record = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read Go receive record fixture {path:?}: {error}"));
    let projection: serde_json::Value = serde_json::from_str(&record)
        .unwrap_or_else(|error| panic!("decode Go receive record fixture: {error}"));
    assert_eq!(projection["code"], "ok");
    let decoded = &projection["record"];
    assert_eq!(decoded["google_message_id"], "synthetic-message");
    assert_eq!(decoded["google_conversation_id"], "synthetic-conversation");
    assert_eq!(decoded["tmp_id"], "synthetic-tmp");
    assert_eq!(decoded["status"], "delivered");

    let fixture = fixture();
    let result = fixture
        .client
        .gateway_ingest_go_rcs_record_json(
            fixture.conversation_id.clone(),
            fixture.generation,
            decoded.to_string(),
        )
        .unwrap();
    assert_eq!(result.record_outcome, "unmatched_outgoing");
    assert_eq!(result.status_outcome.as_deref(), Some("status_applied"));
}

#[test]
fn go_wire_invalid_records_and_stale_generation_do_not_mutate() {
    let fixture = fixture();
    let before = reads(&fixture);
    let token = history_token(&fixture);
    let valid: serde_json::Value = serde_json::from_str(&go_incoming("sent")).unwrap();
    let mut inputs = vec!["{".into(), " ".repeat(65_537)];
    for key in [
        "google_message_id",
        "google_conversation_id",
        "direction",
        "body",
        "sent_at_ms",
        "status",
        "rcs_reported",
    ] {
        let mut missing = valid.clone();
        missing.as_object_mut().unwrap().remove(key);
        inputs.push(missing.to_string());
    }
    for (key, value) in [
        ("unknown", json!(true)),
        ("googleMessageId", json!("id")),
        ("google_message_id", json!("x".repeat(513))),
        ("google_conversation_id", json!("x".repeat(513))),
        ("tmp_id", json!("x".repeat(513))),
        ("body", json!("x".repeat(16_385))),
        ("cursor", json!("x".repeat(4_097))),
        ("cursor", json!("not/a/token")),
        ("direction", json!("sideways")),
        ("status", json!("untrusted")),
    ] {
        let mut changed = valid.clone();
        changed[key] = value;
        inputs.push(changed.to_string());
    }
    for input in inputs {
        assert!(matches!(
            fixture.client.gateway_ingest_go_rcs_record_json(
                fixture.conversation_id.clone(),
                fixture.generation,
                input,
            ),
            Err(MobileBindingsError::InvalidRequest)
        ));
        assert_eq!(reads(&fixture), before);
        assert_eq!(history_token(&fixture), token);
    }
    let result = fixture
        .client
        .gateway_ingest_go_rcs_record_json(
            fixture.conversation_id.clone(),
            fixture.generation + 1,
            valid.to_string(),
        )
        .unwrap();
    assert_eq!(result.record_outcome, "rejected:ownership_stale");
    assert_eq!(result.status_outcome, None);
    assert_eq!(result.cursor, None);
    assert_eq!(reads(&fixture), before);
    assert_eq!(history_token(&fixture), token);
    assert_eq!(
        fixture
            .client
            .gateway_ingest_go_rcs_record_json(
                fixture.conversation_id.clone(),
                fixture.generation,
                valid.to_string(),
            )
            .unwrap()
            .record_outcome,
        "inserted"
    );
}

struct Fixture {
    client: Arc<NativeClient>,
    conversation_id: String,
    generation: u64,
}

fn fixture() -> Fixture {
    let vault_id = Uuid::new_v4().to_string();
    let device_id = Uuid::new_v4().to_string();
    let database_path = std::env::temp_dir()
        .join(format!("peppy-mobile-rcs-{}.sqlite", Uuid::new_v4()))
        .to_string_lossy()
        .into_owned();
    let client = open_native_client(NativeOpenConfig {
        database_path,
        vault_id: vault_id.clone(),
        device_id: device_id.clone(),
        database_key: vec![7; 32],
    })
    .unwrap();
    let material = create_vault_material(vault_id, PASSPHRASE.into()).unwrap();
    client
        .unlock(
            material.profile_json,
            material.header_json,
            PASSPHRASE.into(),
        )
        .unwrap();

    let conversation_id = Uuid::new_v4().to_string();
    let generation = with_client(&client.client, |core| {
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        core.publish_rcs_capability(
            &RcsLinkStatusEvent {
                gateway_device_id: device_id.parse::<DeviceId>().unwrap(),
                state: RcsCapabilityState::Linked,
                availability: true,
                health: true,
                phone_responsive: true,
                observed_at_ms: now_ms,
                capability_revision: 1,
                feature_bits: 1,
                linked_since_ms: Some(now_ms),
            },
            &[],
            now_ms,
        )?;
        Ok(core
            .begin_rcs_link_ownership(conversation_id.parse().unwrap(), "cursor", 1)?
            .ownership_generation)
    })
    .unwrap();
    Fixture {
        client,
        conversation_id,
        generation,
    }
}

fn reads(fixture: &Fixture) -> String {
    format!(
        "{:?}{:?}",
        fixture.client.list_conversations().unwrap(),
        fixture
            .client
            .messages(fixture.conversation_id.clone())
            .unwrap()
    )
}

fn invalid(result: Result<String, MobileBindingsError>) {
    assert!(matches!(result, Err(MobileBindingsError::InvalidRequest)));
}

fn outcome(result: Result<String, MobileBindingsError>) -> String {
    result.unwrap()
}

fn record(
    google_message_id: &str,
    google_conversation_id: &str,
    tmp_id: Option<&str>,
    reported: bool,
    encrypted: Option<bool>,
) -> String {
    let sent_at_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    json!({
        "googleMessageId": google_message_id,
        "googleConversationId": google_conversation_id,
        "direction": "incoming",
        "body": "body",
        "sentAtMs": sent_at_ms,
        "tmpId": tmp_id,
        "rcsReported": reported,
        "e2eeReported": encrypted,
        "historyToken": "cursorfixture0001",
    })
    .to_string()
}

#[test]
fn gateway_wrappers_reject_invalid_input_without_mutating_reads() {
    let fixture = fixture();
    let before = reads(&fixture);
    let invalid_records = [
        "{".to_owned(),
        json!({"googleMessageId":"id"}).to_string(),
        json!({
            "googleMessageId":"id", "googleConversationId":"conversation", "direction":"incoming",
            "body":"body", "rcsReported":true, "unexpected":true
        })
        .to_string(),
        record(&"m".repeat(65_537), "conversation", None, true, None),
        record("id", &"c".repeat(65_537), None, true, None),
        record("id", "conversation", Some(&"t".repeat(65_537)), true, None),
    ];
    for input in invalid_records {
        invalid(fixture.client.gateway_ingest_rcs_record_json(
            fixture.conversation_id.clone(),
            fixture.generation,
            input,
        ));
        assert_eq!(reads(&fixture), before);
    }
    assert_eq!(
        outcome(fixture.client.gateway_ingest_rcs_record_json(
            fixture.conversation_id.clone(),
            fixture.generation + 1,
            record("id", "conversation", None, true, None),
        )),
        "rejected:ownership_stale"
    );
    assert_eq!(reads(&fixture), before);

    let invalid_statuses = [
        "{".to_owned(),
        json!({"googleMessageId":"id"}).to_string(),
        json!({
            "googleMessageId":"id", "googleConversationId":"conversation", "status":"sent", "unexpected":true
        })
        .to_string(),
        json!({
            "googleMessageId":"id", "googleConversationId":"conversation", "tmpId":"t", "status":"sent"
        })
        .to_string()
        + &" ".repeat(65_536),
    ];
    for input in invalid_statuses {
        invalid(fixture.client.gateway_apply_rcs_status_json(
            fixture.conversation_id.clone(),
            fixture.generation,
            input,
        ));
        assert_eq!(reads(&fixture), before);
    }
    assert_eq!(outcome(fixture.client.gateway_apply_rcs_status_json(
            fixture.conversation_id.clone(),
            fixture.generation + 1,
            json!({"googleMessageId":"id", "googleConversationId":"conversation", "status":"sent"})
                .to_string(),
        )), "rejected:ownership_stale");
    assert_eq!(reads(&fixture), before);

    assert_eq!(
        outcome(fixture.client.gateway_checkpoint_rcs_history(
            fixture.conversation_id.clone(),
            fixture.generation,
            "".into(),
        )),
        "rejected:cursor_invalid"
    );
    assert_eq!(reads(&fixture), before);
    invalid(fixture.client.gateway_checkpoint_rcs_history(
        fixture.conversation_id.clone(),
        fixture.generation,
        "x".repeat(4_097),
    ));
    assert_eq!(reads(&fixture), before);
    assert_eq!(
        outcome(fixture.client.gateway_checkpoint_rcs_history(
            fixture.conversation_id.clone(),
            fixture.generation + 1,
            "cursor".into(),
        )),
        "rejected:ownership_stale"
    );
    assert_eq!(reads(&fixture), before);
}

#[test]
fn gateway_wrappers_accept_valid_records_and_closed_outcomes() {
    let fixture = fixture();
    assert_eq!(
        outcome(fixture.client.gateway_ingest_rcs_record_json(
            fixture.conversation_id.clone(),
            fixture.generation,
            record("message", "conversation", Some("tmp"), true, Some(true)),
        )),
        "inserted"
    );
    assert_eq!(outcome(fixture.client.gateway_apply_rcs_status_json(
            fixture.conversation_id.clone(),
            fixture.generation,
            json!({"googleMessageId":"message", "googleConversationId":"conversation", "tmpId":"tmp", "status":"read"}).to_string(),
        )), "status_applied");
    assert_eq!(
        outcome(fixture.client.gateway_checkpoint_rcs_history(
            fixture.conversation_id.clone(),
            fixture.generation,
            "cursor-next".into(),
        )),
        "recorded"
    );
}

#[test]
fn mobile_read_dtos_exclude_rcs_provider_identity_and_cursor() {
    let fixture = fixture();
    assert_eq!(
        outcome(fixture.client.gateway_ingest_rcs_record_json(
            fixture.conversation_id.clone(),
            fixture.generation,
            record(
                "SENTINELgmsg7f3",
                "SENTINELgconv7f3",
                Some("SENTINELtmp7f3"),
                true,
                Some(true),
            ),
        )),
        "inserted"
    );
    let output = reads(&fixture);
    for sentinel in [
        "SENTINELgmsg7f3",
        "SENTINELgconv7f3",
        "SENTINELtmp7f3",
        "cursorfixture0001",
    ] {
        assert!(!output.contains(sentinel), "read DTO leaked {sentinel}");
    }
}

#[test]
fn mobile_rcs_status_and_metadata_mapping_is_sanitized() {
    let status_fixture = fixture();
    for (index, (status, reported, encrypted)) in [
        ("sending", true, Some(true)),
        ("sent", true, Some(false)),
        ("failed", false, Some(true)),
        ("delivered", true, None),
        ("read", true, None),
    ]
    .into_iter()
    .enumerate()
    {
        let message_id = format!("message-{index}");
        assert_eq!(
            outcome(status_fixture.client.gateway_ingest_rcs_record_json(
                status_fixture.conversation_id.clone(),
                status_fixture.generation,
                record(&message_id, "conversation", None, reported, encrypted),
            )),
            "inserted"
        );
        assert_eq!(outcome(status_fixture.client.gateway_apply_rcs_status_json(
                status_fixture.conversation_id.clone(),
                status_fixture.generation,
                json!({"googleMessageId":message_id, "googleConversationId":"conversation", "status":status}).to_string(),
            )), "status_applied");
    }
    let messages = status_fixture
        .client
        .messages(status_fixture.conversation_id.clone())
        .unwrap();
    assert_eq!(messages.len(), 5);
    for (message, expected) in
        messages
            .iter()
            .zip(["sending", "sent", "failed", "delivered", "read"])
    {
        assert_eq!(message.transport, "rcs");
        assert_eq!(message.rcs_status.as_deref(), Some(expected));
        assert!(message.rcs_reported);
    }
    for (reported, encrypted, expected) in [
        (true, Some(true), Some(true)),
        (true, Some(false), Some(false)),
        (false, Some(true), None),
    ] {
        let fixture = fixture();
        assert_eq!(
            outcome(fixture.client.gateway_ingest_rcs_record_json(
                fixture.conversation_id.clone(),
                fixture.generation,
                record("metadata", "conversation", None, reported, encrypted),
            )),
            "inserted"
        );
        assert_eq!(
            fixture
                .client
                .messages(fixture.conversation_id.clone())
                .unwrap()
                .pop()
                .unwrap()
                .rcs_reported,
            reported
        );
        let conversation = fixture.client.list_conversations().unwrap().pop().unwrap();
        assert_eq!(conversation.rcs_encryption, expected);
        assert!(!conversation.rcs_history_gap);
    }

    with_client(&status_fixture.client.client, |core| {
        let conversation_id = status_fixture.conversation_id.parse().unwrap();
        core.record_rcs_provider_detector(
            conversation_id,
            status_fixture.generation,
            RcsProviderDetector {
                provider_row_id: 1,
                provider_thread_id: 1,
                provider_type: 1,
                observed_at_ms: 1,
                observed_generation: status_fixture.generation,
            },
        )?;
        core.begin_rcs_unlink_cutover(conversation_id)?;
        Ok(())
    })
    .unwrap();
    let conversation = status_fixture
        .client
        .list_conversations()
        .unwrap()
        .pop()
        .unwrap();
    assert!(conversation.rcs_history_gap);
}

fn detector(provider_row_id: i64, observed_generation: u64) -> String {
    json!({
        "providerRowId": provider_row_id,
        "providerThreadId": 1,
        "providerType": 1,
        "observedAtMs": 1,
        "observedGeneration": observed_generation,
    })
    .to_string()
}

#[test]
fn gateway_detector_wrapper_rejects_invalid_input_without_mutation() {
    let fixture = fixture();
    let before = reads(&fixture);
    let invalid_detectors = [
        "{".to_owned(),
        json!({"providerRowId": 1}).to_string(),
        json!({
            "providerRowId": 1, "providerThreadId": 1, "providerType": 1,
            "observedAtMs": 1, "observedGeneration": fixture.generation, "unexpected": true
        })
        .to_string(),
        detector(-1, fixture.generation),
        detector(1, fixture.generation) + &" ".repeat(65_536),
    ];
    for input in invalid_detectors {
        invalid(fixture.client.gateway_record_rcs_provider_detector_json(
            fixture.conversation_id.clone(),
            fixture.generation,
            input,
        ));
        assert_eq!(reads(&fixture), before);
    }
    invalid(fixture.client.gateway_record_rcs_provider_detector_json(
        "c".repeat(65_537),
        fixture.generation,
        detector(1, fixture.generation),
    ));
    assert_eq!(reads(&fixture), before);
    assert_eq!(
        outcome(fixture.client.gateway_record_rcs_provider_detector_json(
            fixture.conversation_id.clone(),
            fixture.generation + 1,
            detector(1, fixture.generation),
        )),
        "rejected:ownership_stale"
    );
    assert_eq!(reads(&fixture), before);
    // Rejected attempts must not have inserted provider row 1: a fresh record still reports Recorded.
    assert_eq!(
        outcome(fixture.client.gateway_record_rcs_provider_detector_json(
            fixture.conversation_id.clone(),
            fixture.generation,
            detector(1, fixture.generation),
        )),
        "recorded"
    );
}

#[test]
fn gateway_detector_wrapper_accepts_valid_detector_with_closed_outcomes() {
    let fixture = fixture();
    for (provider_row_id, expected) in [(1, "recorded"), (1, "duplicate"), (2, "recorded")] {
        assert_eq!(
            outcome(fixture.client.gateway_record_rcs_provider_detector_json(
                fixture.conversation_id.clone(),
                fixture.generation,
                detector(provider_row_id, fixture.generation),
            )),
            expected
        );
    }
}
