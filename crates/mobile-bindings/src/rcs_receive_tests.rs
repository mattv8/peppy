use crate::rcs_receive::{
    NativeRcsContextExpectation, NativeRcsObservationProvenance, NativeRcsProviderRoute,
    NativeRcsProviderRouteReason, NativeRcsProviderRowRoute,
};
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
    value.to_string()
}

#[test]
fn go_wire_record_ingests_status_replays_and_checkpoints_only_explicitly() {
    let fixture = fixture();
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
            .gateway_record_rcs_recovery_progress(
                fixture.conversation_id.clone(),
                fixture.generation,
                1_000,
                "AAAAAAAAAAFtZXNzYWdlLTE".into()
            )
            .unwrap(),
        "recorded"
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
        .gateway_ingest_rcs_projected_observation(
            fixture.conversation_id.clone(),
            fixture.generation,
            decoded.to_string(),
            // The exported projection models a live Go stream event, not a gap-page record.
            NativeRcsObservationProvenance::Live,
        )
        .unwrap();
    assert_eq!(result.record_outcome, "unmatched_outgoing");
    assert_eq!(result.status_outcome.as_deref(), Some("status_applied"));
}

#[test]
fn projected_observation_wrapper_requires_explicit_provenance() {
    for provenance in [
        NativeRcsObservationProvenance::Live,
        NativeRcsObservationProvenance::Historical,
    ] {
        let fixture = fixture();
        let mut observation = serde_json::from_str::<serde_json::Value>(&go_incoming("")).unwrap();
        if provenance == NativeRcsObservationProvenance::Historical {
            observation["sent_at_ms"] = json!(
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_millis() as i64
                    + 1
            );
        }
        let result = fixture
            .client
            .gateway_ingest_rcs_projected_observation(
                fixture.conversation_id.clone(),
                fixture.generation,
                observation.to_string(),
                provenance,
            )
            .unwrap();
        assert_eq!(result.record_outcome, "inserted");
    }
}

#[test]
fn projected_observation_wrapper_rejects_malformed_records_and_stale_generations_without_writes() {
    let fixture = fixture();
    let before = reads(&fixture);
    assert!(matches!(
        fixture.client.gateway_ingest_rcs_projected_observation(
            fixture.conversation_id.clone(),
            fixture.generation,
            "{".into(),
            NativeRcsObservationProvenance::Live,
        ),
        Err(MobileBindingsError::InvalidRequest)
    ));
    assert_eq!(reads(&fixture), before);
    let result = fixture
        .client
        .gateway_ingest_rcs_projected_observation(
            fixture.conversation_id.clone(),
            fixture.generation + 1,
            go_incoming(""),
            NativeRcsObservationProvenance::Live,
        )
        .unwrap();
    assert_eq!(result.record_outcome, "rejected:ownership_stale");
    assert_eq!(reads(&fixture), before);
}

#[test]
fn fenced_cutover_wrappers_require_current_generations_without_writes() {
    let linked_fixture = fixture();
    let before = reads(&linked_fixture);
    assert!(matches!(
        linked_fixture
            .client
            .gateway_begin_rcs_unlink_cutover_if_current(
                linked_fixture.conversation_id.clone(),
                linked_fixture.generation + 1,
            ),
        Err(MobileBindingsError::InvalidRequest)
    ));
    assert_eq!(reads(&linked_fixture), before);
    let cutover = linked_fixture
        .client
        .gateway_begin_rcs_unlink_cutover_if_current(
            linked_fixture.conversation_id.clone(),
            linked_fixture.generation,
        )
        .unwrap();
    assert_eq!(cutover.ownership_generation, linked_fixture.generation + 1);

    let owner_fixture = fixture();
    with_client(&owner_fixture.client.client, |core| {
        core.record_rcs_link_lifecycle(
            RcsCapabilityState::Unlinked,
            false,
            false,
            false,
            1_000,
            1_000,
        )?;
        core.record_rcs_explicit_unlink_completed(1_000, 1_000)
    })
    .unwrap();
    let generations = owner_fixture
        .client
        .gateway_rcs_unlink_cutover_generations(owner_fixture.conversation_id.clone())
        .unwrap()
        .unwrap();
    assert_eq!(generations.ownership_generation, owner_fixture.generation);
    assert!(matches!(
        owner_fixture.client.gateway_begin_rcs_owner_unlink_cutover(
            owner_fixture.conversation_id.clone(),
            generations.ownership_generation,
            generations.lifecycle_generation + 1,
        ),
        Err(MobileBindingsError::InvalidRequest)
    ));
    let cutover = owner_fixture
        .client
        .gateway_begin_rcs_owner_unlink_cutover(
            owner_fixture.conversation_id.clone(),
            generations.ownership_generation,
            generations.lifecycle_generation,
        )
        .unwrap();
    assert_eq!(cutover.ownership_generation, owner_fixture.generation + 1);
}

#[test]
fn google_event_invalidation_wrapper_rejects_stale_expectations_without_writes() {
    let fixture = fixture();
    let before = reads(&fixture);
    assert!(matches!(
        fixture
            .client
            .gateway_invalidate_rcs_context_for_google_event(
                fixture.conversation_id.clone(),
                "google-conversation".into(),
                NativeRcsContextExpectation {
                    ownership_generation: fixture.generation + 1,
                    link_generation: 1,
                    session_generation: 1,
                    context_revision: 1,
                },
            ),
        Err(MobileBindingsError::InvalidRequest)
    ));
    assert_eq!(reads(&fixture), before);
}

#[test]
fn new_receive_wrappers_reject_closed_clients() {
    let fixture = fixture();
    fixture.client.dispose().unwrap();
    assert!(matches!(
        fixture.client.gateway_ingest_rcs_projected_observation(
            fixture.conversation_id.clone(),
            fixture.generation,
            go_incoming(""),
            NativeRcsObservationProvenance::Live,
        ),
        Err(MobileBindingsError::Closed)
    ));
    assert!(matches!(
        fixture.client.gateway_begin_rcs_unlink_cutover_if_current(
            fixture.conversation_id.clone(),
            fixture.generation,
        ),
        Err(MobileBindingsError::Closed)
    ));
    assert!(matches!(
        fixture
            .client
            .gateway_invalidate_rcs_context_for_google_event(
                fixture.conversation_id.clone(),
                "google-conversation".into(),
                NativeRcsContextExpectation {
                    ownership_generation: fixture.generation,
                    link_generation: 1,
                    session_generation: 1,
                    context_revision: 1,
                },
            ),
        Err(MobileBindingsError::Closed)
    ));
    assert!(matches!(
        fixture
            .client
            .gateway_rcs_unlink_cutover_generations(fixture.conversation_id),
        Err(MobileBindingsError::Closed)
    ));
}

#[test]
fn go_wire_invalid_records_and_stale_generation_do_not_mutate() {
    let fixture = fixture();
    let before = reads(&fixture);
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
        ("cursor", json!("AAAAAAAAAAFtZXNzYWdlLTE")),
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
    assert_eq!(reads(&fixture), before);
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
    database_path: String,
}

fn fixture() -> Fixture {
    let vault_id = Uuid::new_v4().to_string();
    let device_id = Uuid::new_v4().to_string();
    let database_path = std::env::temp_dir()
        .join(format!("peppy-mobile-rcs-{}.sqlite", Uuid::new_v4()))
        .to_string_lossy()
        .into_owned();
    let client = open_native_client(NativeOpenConfig {
        database_path: database_path.clone(),
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
            .begin_rcs_link_ownership(conversation_id.parse().unwrap(), 1)?
            .ownership_generation)
    })
    .unwrap();
    Fixture {
        client,
        conversation_id,
        generation,
        database_path,
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

#[test]
fn gateway_status_and_recovery_wrappers_reject_invalid_input_without_mutating_reads() {
    let fixture = fixture();
    let before = reads(&fixture);
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
        outcome(fixture.client.gateway_record_rcs_recovery_progress(
            fixture.conversation_id.clone(),
            fixture.generation,
            1_000,
            "".into(),
        )),
        "rejected:cursor_invalid"
    );
    assert_eq!(reads(&fixture), before);
    invalid(fixture.client.gateway_record_rcs_recovery_progress(
        fixture.conversation_id.clone(),
        fixture.generation,
        1_000,
        "x".repeat(4_097),
    ));
    assert_eq!(reads(&fixture), before);
    assert_eq!(
        outcome(fixture.client.gateway_record_rcs_recovery_progress(
            fixture.conversation_id.clone(),
            fixture.generation + 1,
            1_000,
            "cursor".into(),
        )),
        "rejected:ownership_stale"
    );
    assert_eq!(
        outcome(fixture.client.gateway_complete_rcs_recovery(
            fixture.conversation_id.clone(),
            fixture.generation + 1,
            1_000,
        )),
        "rejected:ownership_stale"
    );
    assert_eq!(
        outcome(fixture.client.gateway_clear_rcs_recovery_hint(
            fixture.conversation_id.clone(),
            fixture.generation + 1,
        )),
        "rejected:ownership_stale"
    );
    assert_eq!(reads(&fixture), before);
}

#[test]
fn owner_unlink_candidates_require_current_completion_and_are_bounded() {
    let fixture = fixture();
    assert!(matches!(
        fixture.client.gateway_rcs_owner_unlink_cutover_candidates(),
        Err(MobileBindingsError::InvalidRequest)
    ));
    with_client(&fixture.client.client, |core| {
        core.record_rcs_link_lifecycle(
            RcsCapabilityState::Unlinked,
            false,
            false,
            false,
            1_000,
            1_000,
        )?;
        Ok(())
    })
    .unwrap();
    assert!(matches!(
        fixture.client.gateway_rcs_owner_unlink_cutover_candidates(),
        Err(MobileBindingsError::InvalidRequest)
    ));
    with_client(&fixture.client.client, |core| {
        core.record_rcs_explicit_unlink_completed(1_000, 1_000)
    })
    .unwrap();
    let candidates = fixture
        .client
        .gateway_rcs_owner_unlink_cutover_candidates()
        .unwrap();
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].conversation_id, fixture.conversation_id);
    assert_eq!(candidates[0].ownership_generation, fixture.generation);
    assert_eq!(candidates[0].lifecycle_generation, 1);
}

#[test]
fn pre_proof_provider_row_routes_to_provider_for_the_single_link_lane() {
    let fixture = fixture();
    let connection = rusqlite::Connection::open(&fixture.database_path).unwrap();
    connection
        .execute_batch(
            "PRAGMA key = \"x'0707070707070707070707070707070707070707070707070707070707070707'\";",
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO rcs_conversation_mappings(peppy_conversation_id,google_conversation_id,class,proof_version,proof_hash,mapping_revision,verified_at_ms) VALUES(?, 'google-route', 'one_to_one', 2, ?, 1, 1000)",
            rusqlite::params![fixture.conversation_id, vec![0_u8; 32]],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO rcs_mapping_proofs(peppy_conversation_id,mapping_revision,canonical_participant,identity_region,own_identity) VALUES(?,1,'+14155552671','US','+14155550100')",
            [&fixture.conversation_id],
        )
        .unwrap();
    let before = reads(&fixture);
    let outcome = fixture
        .client
        .gateway_route_rcs_provider_row_json(
            serde_json::json!({
                "providerRowId": 41,
                "providerThreadId": 7,
                "providerType": 1,
                "observedAtMs": 1_000,
                "addresses": ["+14155552671"]
            })
            .to_string(),
        )
        .unwrap();
    assert_eq!(
        outcome,
        NativeRcsProviderRoute {
            route: NativeRcsProviderRowRoute::Provider,
            reason: Some(NativeRcsProviderRouteReason::NotRestrictedText),
        }
    );
    assert_eq!(reads(&fixture), before);
    let count: i64 = connection
        .query_row("SELECT COUNT(*) FROM rcs_provider_detectors", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(count, 0);
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

fn claim_json(conversation_id: &str) -> serde_json::Value {
    json!({
        "conversationId": conversation_id,
        "googleConversationId": "google-claim",
        "mappingRevision": 1,
        "providerFloor": 0,
        "expectedLifecycleGeneration": 1,
        "expectedLinkGeneration": 4,
        "expectedSessionGeneration": 7,
        "expectedContextRevision": 1
    })
}

#[test]
fn gateway_claim_json_rejects_unknown_and_missing_fields() {
    let fixture = fixture();
    let mut unknown = claim_json(&fixture.conversation_id);
    unknown["unexpected"] = json!(true);
    assert!(matches!(
        fixture
            .client
            .gateway_claim_rcs_link_ownership_json(unknown.to_string()),
        Err(MobileBindingsError::InvalidRequest)
    ));

    for removed_field in ["historyCursor", "cursorObservedAtMs", "backfillFromMs"] {
        let mut stale = claim_json(&fixture.conversation_id);
        stale[removed_field] = json!("cursor");
        assert!(matches!(
            fixture
                .client
                .gateway_claim_rcs_link_ownership_json(stale.to_string()),
            Err(MobileBindingsError::InvalidRequest)
        ));
    }

    let mut missing = claim_json(&fixture.conversation_id);
    missing.as_object_mut().unwrap().remove("providerFloor");
    assert!(matches!(
        fixture
            .client
            .gateway_claim_rcs_link_ownership_json(missing.to_string()),
        Err(MobileBindingsError::InvalidRequest)
    ));

    let refused = crate::rcs_receive::NativeRcsLinkOwnershipClaim {
        outcome: crate::rcs_receive::NativeRcsLinkOwnershipClaimOutcome::Refused,
        ownership_generation: 0,
        proof_time_ms: 0,
    };
    let absent = claim_json(&fixture.conversation_id);
    assert_eq!(
        fixture
            .client
            .gateway_claim_rcs_link_ownership_json(absent.to_string())
            .unwrap(),
        refused
    );
}

#[test]
fn gateway_cutover_alerts_report_ambiguous_windows_as_records() {
    let fixture = fixture();
    let empty = crate::rcs_receive::NativeRcsCutoverAlerts {
        count: 0,
        conversation_ids: vec![],
    };
    assert_eq!(fixture.client.gateway_rcs_cutover_alerts().unwrap(), empty);

    let conversation: peppy_client_core::ConversationId = fixture.conversation_id.parse().unwrap();
    let generation = fixture.generation;
    with_client(&fixture.client.client, |core| {
        core.record_rcs_provider_detector(
            conversation,
            generation,
            RcsProviderDetector {
                provider_row_id: 11,
                provider_thread_id: 2,
                provider_type: 132,
                observed_at_ms: 101,
                observed_generation: generation,
            },
        )?;
        core.begin_rcs_unlink_cutover(conversation)?;
        Ok(())
    })
    .unwrap();

    assert_eq!(
        fixture.client.gateway_rcs_cutover_alerts().unwrap(),
        crate::rcs_receive::NativeRcsCutoverAlerts {
            count: 1,
            conversation_ids: vec![fixture.conversation_id.clone()],
        }
    );
}
