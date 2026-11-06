use super::*;
use crate::{NativeOpenConfig, create_vault_material, open_native_client};
use peppy_client_core::{
    Cursor, GatewayRoute, IncomingSms, NativeCarrierRouteBinding, NativeOwnNumberSource,
    NativeRcsEvidence, NativeRcsLifetime, NativeRcsOutgoing, NativeRcsSimPayload,
    NativeSettingsCurrentness, NativeSettingsProvenance, OutgoingSms, RcsConversationType,
    RcsLinkState, RcsMappingObservation, RcsParticipantObservation, RcsSendMode,
};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Barrier, mpsc};
use uuid::Uuid;

const PASSPHRASE: &str = "correct horse battery staple";

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("peppy-mobile-rcs-native-{}", Uuid::new_v4()));
        std::fs::create_dir(&path).unwrap();
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

struct Fixture {
    gateway: Arc<NativeClient>,
    _sender: Arc<NativeClient>,
    command_id: String,
    observation: RcsMappingObservation,
    _temp_dir: TempDir,
}

fn fixture() -> Fixture {
    fixture_with_google_conversation_id("conversation-local")
}

fn fixture_with_google_conversation_id(google_conversation_id: &str) -> Fixture {
    let temp_dir = TempDir::new();
    let vault_id = Uuid::new_v4().to_string();
    let sender = native_client(&temp_dir, &vault_id, Uuid::new_v4().to_string());
    let gateway_device_id = Uuid::new_v4();
    let gateway = native_client(&temp_dir, &vault_id, gateway_device_id.to_string());
    let material = create_vault_material(vault_id, PASSPHRASE.into()).unwrap();
    for client in [&sender, &gateway] {
        client
            .unlock(
                material.profile_json.clone(),
                material.header_json.clone(),
                PASSPHRASE.into(),
            )
            .unwrap();
    }

    let mut observation = observation();
    observation.google_conversation_id = google_conversation_id.into();
    let conversation_id = with_client(&gateway.client, |core| {
        let captured = core.capture_incoming(IncomingSms {
            conversation_id: None,
            sender_address: "peer-local".into(),
            body: String::new(),
            provider_message_id: Some("provider-rcs-local".into()),
            imported: true,
        })?;
        core.verify_rcs_mapping(&observation, 1_000)?;
        Ok(captured.conversation_id)
    })
    .unwrap();
    let command_id = with_client(&sender.client, |core| {
        Ok(core
            .queue_send(
                OutgoingSms {
                    conversation_id,
                    recipients: vec!["peer-local".into()],
                    body: String::from('\0'),
                },
                GatewayRoute::RcsLink {
                    gateway_device_id: gateway_device_id.to_string().parse().unwrap(),
                },
            )?
            .command_id)
    })
    .unwrap();
    deliver(&sender, &gateway);

    Fixture {
        gateway,
        _sender: sender,
        command_id: command_id.to_string(),
        observation,
        _temp_dir: temp_dir,
    }
}

fn native_client(temp_dir: &TempDir, vault_id: &str, device_id: String) -> Arc<NativeClient> {
    open_native_client(NativeOpenConfig {
        database_path: temp_dir
            .path()
            .join(format!("peppy-mobile-rcs-native-{}.sqlite", Uuid::new_v4()))
            .to_string_lossy()
            .into_owned(),
        vault_id: vault_id.into(),
        device_id,
        database_key: vec![7; 32],
    })
    .unwrap()
}

fn deliver(sender: &NativeClient, gateway: &NativeClient) {
    let envelopes = with_client(&sender.client, |core| {
        let envelopes = core.pending_outbox()?;
        for envelope in &envelopes {
            core.ack_outbox(envelope.envelope_id)?;
        }
        Ok(envelopes)
    })
    .unwrap();
    with_client(&gateway.client, |core| {
        for (index, envelope) in envelopes.iter().enumerate() {
            core.ingest(envelope, Cursor(index as u64 + 1))?;
        }
        core.apply_pending(100)?;
        Ok(())
    })
    .unwrap();
}

fn observation() -> RcsMappingObservation {
    RcsMappingObservation {
        google_conversation_id: "conversation-local".into(),
        google_candidate_count: 1,
        participants: vec![RcsParticipantObservation {
            identity: "peer-local".into(),
            google_participant_id: "google-peer".into(),
            identifier_type: 1,
            is_visible: true,
            is_me: false,
        }],
        conversation_type: RcsConversationType::Rcs,
        status: "eligible".into(),
        send_mode: RcsSendMode::Automatic,
        read_only: false,
        default_outgoing: true,
        selected_sim_id: "android-subscription-7".into(),
        selected_sim_own_identity: "+12025550100".into(),
        carrier_binding: NativeCarrierRouteBinding {
            carrier_route_id: "android-subscription-7".into(),
            android_subscription_id: 7,
            is_current_default_sms: true,
            own_number_raw: "+12025550100".into(),
            own_number_source: NativeOwnNumberSource::SubscriptionManagerPhoneNumber,
            route_observed_at_ms: 1_000,
            route_context_revision: 1,
            same_handset_evidence_id: "verified-handset".into(),
        },
        outgoing: NativeRcsOutgoing {
            outgoing_id: "google-outgoing".into(),
            sim_participant_id: "google-outgoing".into(),
            own_identity_raw: "+12025550100".into(),
            sim_payload: NativeRcsSimPayload {
                two: 2,
                sim_number: 7,
            },
            rcs_chats_enabled: true,
        },
        native_evidence: NativeRcsEvidence {
            lifetime: NativeRcsLifetime {
                runtime_id: "runtime".into(),
                run_generation: 1,
                link_generation: 1,
                session_generation: 1,
                context_revision: 1,
            },
            lookup_id: "lookup".into(),
            lookup_complete: true,
            google_candidate_count: 1,
            settings_revision: 1,
            settings_provenance: NativeSettingsProvenance::CurrentEvent,
            settings_observed_at_ms: 1_000,
            settings_stream_epoch: 1,
            settings_currentness: NativeSettingsCurrentness::ContinuousVerifiedFeed,
            current_settings_sims: vec![NativeRcsOutgoing {
                outgoing_id: "google-outgoing".into(),
                sim_participant_id: "google-outgoing".into(),
                own_identity_raw: "+12025550100".into(),
                sim_payload: NativeRcsSimPayload {
                    two: 2,
                    sim_number: 7,
                },
                rcs_chats_enabled: true,
            }],
            settings_validated_at_ms: 1_000,
            conversation_observed_at_ms: 1_000,
        },
        identity_region: "US".into(),
        observed_at_ms: 1_000,
        context_revision: 1,
        link_generation: 1,
        session_generation: 1,
        canonicalizer_revision: 1,
        mapping_revision: 1,
        link_state: RcsLinkState::Linked,
    }
}

fn native_preparation(fixture: &Fixture) -> Arc<NativeRcsSendPreparation> {
    gateway_prepare_native_rcs_send(fixture.gateway.clone(), fixture.command_id.clone(), 1_000)
        .unwrap()
}

fn observation_json(fixture: &Fixture) -> String {
    serde_json::to_string(&fixture.observation).unwrap()
}

#[test]
fn factory_facts_and_begin_use_the_real_stored_mapping_and_core_projection() {
    let fixture = fixture();
    let expected_facts = with_client(&fixture.gateway.client, |core| {
        let command = core.pending_automatic_commands()?.pop().unwrap();
        let mapping = core
            .rcs_mapping(command.message.record.conversation_id)?
            .unwrap();
        Ok(json!({
            "request": {
                "command_id": fixture.command_id,
                "peppy_conversation_id": mapping.conversation_id.to_string(),
                "canonical_recipient": mapping.proof.canonical_participant,
            },
            "stored_mapping": {
                "conversation_id": mapping.conversation_id.to_string(),
                "google_conversation_id": mapping.google_conversation_id,
                "class": "OneToOne",
                "proof_version": mapping.proof_version,
                "proof": {
                    "canonical_participant": mapping.proof.canonical_participant,
                    "identity_region": mapping.proof.identity_region,
                    "own_identity": mapping.proof.own_identity,
                    "digest": mapping.proof.digest,
                },
                "mapping_revision": mapping.mapping_revision,
                "verified_at_ms": mapping.verified_at_ms,
            }
        }))
    })
    .unwrap();
    let preparation = native_preparation(&fixture);

    assert_eq!(
        serde_json::from_str::<Value>(&preparation.facts_json().unwrap()).unwrap(),
        expected_facts
    );
    let facts = expected_facts;
    assert_eq!(
        facts["stored_mapping"]["proof"]["digest"]
            .as_array()
            .unwrap()
            .len(),
        32
    );
    let committed = preparation
        .begin(observation_json(&fixture), 1_000)
        .unwrap();
    let projection: Value = serde_json::from_str(&committed.commit_json()).unwrap();
    assert_eq!(projection["command_id"], fixture.command_id);
    assert_eq!(projection["message_body"], "\u{0000}");
    assert_eq!(
        projection["target"],
        json!({
            "proof_version": facts["stored_mapping"]["proof_version"],
            "peppy_conversation_id": facts["request"]["peppy_conversation_id"],
            "google_conversation_id": fixture.observation.google_conversation_id,
            "conversation_type": fixture.observation.conversation_type,
            "status": fixture.observation.status,
            "send_mode": fixture.observation.send_mode,
            "read_only": fixture.observation.read_only,
            "default_outgoing": fixture.observation.default_outgoing,
            "participants": fixture.observation.participants,
            "class": "OneToOne",
            "canonical_recipient": facts["request"]["canonical_recipient"],
            "canonical_own_identity": facts["stored_mapping"]["proof"]["own_identity"],
            "identity_region": fixture.observation.identity_region,
            "canonicalizer_revision": fixture.observation.canonicalizer_revision,
            "selected_sim_id": fixture.observation.selected_sim_id,
            "carrier_binding": fixture.observation.carrier_binding,
            "outgoing": fixture.observation.outgoing,
            "evidence": fixture.observation.native_evidence,
        })
    );
    assert_eq!(
        projection["proof_digest"],
        facts["stored_mapping"]["proof"]["digest"]
    );
    assert!(matches!(
        gateway_prepare_native_rcs_send(fixture.gateway, fixture.command_id, 1_000),
        Err(MobileBindingsError::Conflict)
    ));
}

#[test]
fn serialize_commit_preserves_the_exact_core_projection_bytes() {
    let fixture = fixture();
    let projection = with_client(&fixture.gateway.client, |core| {
        let preparation =
            core.prepare_native_rcs_send(fixture.command_id.parse().unwrap(), 1_000)?;
        core.begin_native_rcs_send(preparation, &fixture.observation, 1_000)
    })
    .unwrap();
    let expected = serde_json::to_string(&projection).unwrap();

    assert_eq!(
        serialize_commit(projection).unwrap().commit_json(),
        expected
    );
}

#[test]
fn invalid_observations_consume_the_preparation_before_parsing() {
    for input in [
        "{".into(),
        "{}".into(),
        r#"{"google_conversation_id":"a","google_conversation_id":"b"}"#.into(),
        nested_unknown_observation(),
        r#"{"google_conversation_id":1}"#.into(),
        format!(
            "{}{}",
            serde_json::to_string(&observation()).unwrap(),
            " ".repeat(RCS_NATIVE_JSON_CAP)
        ),
    ] {
        let fixture = fixture();
        let preparation = native_preparation(&fixture);
        assert!(matches!(
            preparation.begin(input, 1_000),
            Err(MobileBindingsError::InvalidRequest)
        ));
        assert!(matches!(
            preparation.facts_json(),
            Err(MobileBindingsError::Conflict)
        ));
        assert!(matches!(
            preparation.begin(observation_json(&fixture), 1_000),
            Err(MobileBindingsError::Conflict)
        ));
        assert!(
            native_preparation(&fixture)
                .begin(observation_json(&fixture), 1_000)
                .is_ok()
        );
    }
}

fn nested_unknown_observation() -> String {
    let mut observation: Value = serde_json::to_value(observation()).unwrap();
    observation["native_evidence"]["lifetime"]["unexpected"] = json!(true);
    observation.to_string()
}

#[test]
fn stale_observation_consumes_the_preparation_without_an_attempt() {
    let fixture = fixture();
    let preparation = native_preparation(&fixture);
    let mut stale = fixture.observation.clone();
    stale.observed_at_ms = 1;
    assert!(matches!(
        preparation.begin(serde_json::to_string(&stale).unwrap(), 1_000),
        Err(MobileBindingsError::InvalidRequest)
    ));
    assert!(matches!(
        preparation.facts_json(),
        Err(MobileBindingsError::Conflict)
    ));
    let fresh = native_preparation(&fixture);
    assert!(fresh.begin(observation_json(&fixture), 1_000).is_ok());
}

#[test]
fn aliases_and_separate_preparations_cannot_admit_twice() {
    let fixture = fixture();
    let first = native_preparation(&fixture);
    let alias = first.clone();
    let second = native_preparation(&fixture);
    let barrier = Arc::new(Barrier::new(3));
    let spawn_begin = |preparation: Arc<NativeRcsSendPreparation>, barrier: Arc<Barrier>| {
        let observation = observation_json(&fixture);
        std::thread::spawn(move || {
            barrier.wait();
            preparation.begin(observation, 1_000)
        })
    };
    let first_result = spawn_begin(first.clone(), barrier.clone());
    let alias_result = spawn_begin(alias, barrier.clone());
    barrier.wait();
    let results = [first_result.join().unwrap(), alias_result.join().unwrap()];
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|result| matches!(result, Err(MobileBindingsError::Conflict)))
            .count(),
        1
    );
    assert!(matches!(
        first.begin(observation_json(&fixture), 1_000),
        Err(MobileBindingsError::Conflict)
    ));
    assert!(matches!(
        second.begin(observation_json(&fixture), 1_000),
        Err(MobileBindingsError::Conflict | MobileBindingsError::InvalidRequest)
    ));
}

#[test]
fn independently_prepared_objects_race_for_one_core_attempt() {
    let fixture = fixture();
    let first = native_preparation(&fixture);
    let second = native_preparation(&fixture);
    let barrier = Arc::new(Barrier::new(3));
    let spawn_begin = |preparation: Arc<NativeRcsSendPreparation>, barrier: Arc<Barrier>| {
        let observation = observation_json(&fixture);
        std::thread::spawn(move || {
            barrier.wait();
            preparation.begin(observation, 1_000)
        })
    };
    let first_result = spawn_begin(first, barrier.clone());
    let second_result = spawn_begin(second, barrier.clone());
    barrier.wait();
    let results = [first_result.join().unwrap(), second_result.join().unwrap()];

    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|result| matches!(result, Err(MobileBindingsError::Conflict)))
            .count(),
        1
    );
    assert!(matches!(
        gateway_prepare_native_rcs_send(fixture.gateway, fixture.command_id, 1_000),
        Err(MobileBindingsError::Conflict)
    ));
}

#[test]
fn disposal_rejects_new_facts_and_preserves_issued_commit_bytes() {
    let closed_fixture = fixture();
    let preparation = native_preparation(&closed_fixture);
    closed_fixture.gateway.dispose().unwrap();
    assert!(matches!(
        preparation.facts_json(),
        Err(MobileBindingsError::Closed)
    ));
    assert!(matches!(
        preparation.begin(observation_json(&closed_fixture), 1_000),
        Err(MobileBindingsError::Closed)
    ));

    let issued_fixture = fixture();
    let committed = native_preparation(&issued_fixture)
        .begin(observation_json(&issued_fixture), 1_000)
        .unwrap();
    let expected = committed.commit_json();
    issued_fixture.gateway.dispose().unwrap();
    assert_eq!(committed.commit_json(), expected);
}

#[test]
fn disposal_precedes_consumed_preparation_conflicts() {
    let fixture = fixture();
    let preparation = native_preparation(&fixture);
    assert!(matches!(
        preparation.begin("{".into(), 1_000),
        Err(MobileBindingsError::InvalidRequest)
    ));

    fixture.gateway.dispose().unwrap();
    assert!(matches!(
        preparation.facts_json(),
        Err(MobileBindingsError::Closed)
    ));
    assert!(matches!(
        preparation.begin(observation_json(&fixture), 1_000),
        Err(MobileBindingsError::Closed)
    ));
}

#[test]
fn admitted_begin_finishes_after_dispose() {
    let fixture = fixture();
    let preparation = native_preparation(&fixture);
    let (admitted_tx, admitted_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let gateway = fixture.gateway.clone();
    let observation = observation_json(&fixture);
    let worker = std::thread::spawn(move || {
        with_client(&gateway.client, |core| {
            admitted_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            preparation.begin_admitted(core, observation, 1_000)
        })
    });

    admitted_rx.recv().unwrap();
    fixture.gateway.dispose().unwrap();
    release_tx.send(()).unwrap();
    assert!(worker.join().unwrap().is_ok());
}

#[test]
fn factory_rejects_an_oversized_actual_facts_projection_before_attempting() {
    let google_conversation_id = "g".repeat(RCS_NATIVE_JSON_CAP);
    let fixture = fixture_with_google_conversation_id(&google_conversation_id);

    assert!(matches!(
        gateway_prepare_native_rcs_send(fixture.gateway.clone(), fixture.command_id.clone(), 1_000),
        Err(MobileBindingsError::InvalidRequest)
    ));
    let projection = with_client(&fixture.gateway.client, |core| {
        let preparation =
            core.prepare_native_rcs_send(fixture.command_id.parse().unwrap(), 1_000)?;
        core.begin_native_rcs_send(preparation, &fixture.observation, 1_000)
    });
    assert!(projection.is_ok());
}

#[test]
fn closed_owner_rejects_factory_admission() {
    let client = Arc::new(NativeClient {
        client: Mutex::new(None),
    });
    assert!(matches!(
        gateway_prepare_native_rcs_send(client, "not-a-command".into(), 0),
        Err(MobileBindingsError::Closed)
    ));
}
