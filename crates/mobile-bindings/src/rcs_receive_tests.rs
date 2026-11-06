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
        "historyToken": "SENTINELcursor7f3",
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
        "SENTINELcursor7f3",
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
        assert!(
            !fixture
                .client
                .messages(fixture.conversation_id.clone())
                .unwrap()
                .pop()
                .unwrap()
                .rcs_reported
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
