use super::*;
use crate::{NativeAttachmentState, NativeOpenConfig, create_vault_material, open_native_client};
use peppy_client_core::{
    Cursor, GatewayRoute, IncomingSms, NativeCarrierRouteBinding, NativeOwnNumberSource,
    NativeRcsEvidence, NativeRcsLifetime, NativeRcsOutgoing, NativeRcsSimPayload,
    NativeSettingsCurrentness, NativeSettingsProvenance, OutgoingMms, OutgoingSms, PermitBlock,
    PermitDecision, RcsCapabilityState, RcsConversationType, RcsEligibility, RcsLinkState,
    RcsMappingObservation, RcsParticipantObservation, RcsSendMode,
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
    sender: Arc<NativeClient>,
    next_cursor: u64,
    command_id: String,
    conversation_id: String,
    gateway_device_id: String,
    database_path: PathBuf,
    observation: RcsMappingObservation,
    _temp_dir: TempDir,
}

fn fixture() -> Fixture {
    fixture_with_google_conversation_id("conversation-local")
}

fn fixture_with_google_conversation_id(google_conversation_id: &str) -> Fixture {
    fixture_with_google_conversation_id_and_peer(google_conversation_id, "peer-local")
}

fn fixture_with_google_conversation_id_and_peer(
    google_conversation_id: &str,
    peer: &str,
) -> Fixture {
    fixture_with_route(google_conversation_id, peer, None)
}

fn carrier_fixture_with_google_conversation_id_and_peer(
    google_conversation_id: &str,
    peer: &str,
) -> Fixture {
    carrier_fixture_with_google_conversation_id_and_peer_on_sim(
        google_conversation_id,
        peer,
        "android-subscription-7",
        7,
    )
}

fn carrier_fixture_with_google_conversation_id_and_peer_on_sim(
    google_conversation_id: &str,
    peer: &str,
    carrier_subscription_id: &str,
    sim_number: i32,
) -> Fixture {
    fixture_with_route(
        google_conversation_id,
        peer,
        Some((carrier_subscription_id, sim_number)),
    )
}

fn fixture_with_route(
    google_conversation_id: &str,
    peer: &str,
    carrier_route: Option<(&str, i32)>,
) -> Fixture {
    let temp_dir = TempDir::new();
    let vault_id = Uuid::new_v4().to_string();
    let sender = native_client(&temp_dir, &vault_id, Uuid::new_v4().to_string());
    let gateway_device_id = Uuid::new_v4();
    let gateway_device_id_text = gateway_device_id.to_string();
    let database_path = temp_dir.path().join(format!(
        "peppy-mobile-rcs-native-{gateway_device_id}.sqlite"
    ));
    let gateway = native_client(&temp_dir, &vault_id, gateway_device_id_text.clone());
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
    observation.participants[0].identity = peer.into();
    if let Some((subscription_id, sim_number)) = carrier_route {
        observation.selected_sim_id = subscription_id.into();
        observation.carrier_binding.carrier_route_id = subscription_id.into();
        observation.carrier_binding.android_subscription_id = sim_number;
        observation.outgoing.sim_payload.sim_number = sim_number;
        observation.native_evidence.current_settings_sims[0]
            .sim_payload
            .sim_number = sim_number;
    }
    let conversation_id = with_client(&gateway.client, |core| {
        let captured = core.capture_incoming(IncomingSms {
            conversation_id: None,
            sender_address: peer.into(),
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
                    recipients: vec![peer.into()],
                    body: String::from('\0'),
                },
                match carrier_route {
                    Some((subscription_id, _)) => GatewayRoute::Carrier {
                        gateway_device_id: gateway_device_id.to_string().parse().unwrap(),
                        subscription_id: subscription_id.into(),
                    },
                    None => GatewayRoute::RcsLink {
                        gateway_device_id: gateway_device_id.to_string().parse().unwrap(),
                    },
                },
            )?
            .command_id)
    })
    .unwrap();
    let next_cursor = deliver(&sender, &gateway) + 1;

    Fixture {
        gateway,
        sender,
        next_cursor,
        command_id: command_id.to_string(),
        conversation_id: conversation_id.to_string(),
        gateway_device_id: gateway_device_id_text,
        database_path,
        observation,
        _temp_dir: temp_dir,
    }
}

fn native_client(temp_dir: &TempDir, vault_id: &str, device_id: String) -> Arc<NativeClient> {
    open_native_client(NativeOpenConfig {
        database_path: temp_dir
            .path()
            .join(format!("peppy-mobile-rcs-native-{device_id}.sqlite"))
            .to_string_lossy()
            .into_owned(),
        vault_id: vault_id.into(),
        device_id,
        database_key: vec![7; 32],
    })
    .unwrap()
}

fn deliver(sender: &NativeClient, gateway: &NativeClient) -> u64 {
    deliver_from(sender, gateway, 1)
}

fn deliver_from(sender: &NativeClient, gateway: &NativeClient, first_cursor: u64) -> u64 {
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
            core.ingest(envelope, Cursor(first_cursor + index as u64))?;
        }
        core.apply_pending(100)?;
        Ok(())
    })
    .unwrap();
    first_cursor + envelopes.len() as u64 - 1
}

fn record_linked_gateway(gateway: &NativeClient, gateway_device_id: &str, now_ms: i64) {
    gateway
        .record_rcs_link_lifecycle(
            crate::rcs_status::NativeRcsLinkState::Linked,
            true,
            true,
            true,
            now_ms,
            now_ms,
        )
        .unwrap();
    let status = gateway
        .rcs_link_status(gateway_device_id.into(), now_ms)
        .unwrap()
        .unwrap();
    assert_eq!(status.state, crate::rcs_status::NativeRcsLinkState::Linked);
    assert!(status.current);
    assert!(status.availability);
    assert!(status.health);
    assert!(status.phone_responsive);
}

fn install_sender_ciphertext_on_gateway(
    sender: &NativeClient,
    gateway: &NativeClient,
    attachment_id: String,
    downloaded_path: &Path,
) {
    let source = sender
        .native_cipher_file_for_upload(attachment_id.clone())
        .unwrap();
    std::fs::copy(source, downloaded_path).unwrap();
    gateway
        .install_downloaded_attachment(
            attachment_id.clone(),
            downloaded_path.to_string_lossy().into_owned(),
        )
        .unwrap();
    assert_eq!(
        gateway.attachment_info(attachment_id).unwrap().state,
        NativeAttachmentState::Available
    );
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
        selected_sim_own_identity: "+12025550101".into(),
        carrier_binding: NativeCarrierRouteBinding {
            carrier_route_id: "android-subscription-7".into(),
            android_subscription_id: 7,
            is_current_default_sms: true,
            own_number_raw: "+12025550101".into(),
            own_number_source: NativeOwnNumberSource::SubscriptionManagerPhoneNumber,
            route_observed_at_ms: 1_000,
            route_context_revision: 1,
            same_handset_evidence_id: "verified-handset".into(),
        },
        outgoing: NativeRcsOutgoing {
            outgoing_id: "google-outgoing".into(),
            sim_participant_id: "google-outgoing".into(),
            own_identity_raw: "+12025550101".into(),
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
                own_identity_raw: "+12025550101".into(),
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

fn candidate_snapshot(google_conversation_id: &str, recipient: &str) -> String {
    json!({
        "code": "conversation_candidate_snapshot",
        "snapshot_token": "1:1:1",
        "generation": 1,
        "stream_epoch": 1,
        "revision": 1,
        "run_incarnation": "runtime",
        "pairing_evidence_id": "pairing",
        "complete": true,
        "conversations": [{
            "google_conversation_id": google_conversation_id,
            "conversation_type": 2,
            "conversation_status": 1,
            "classification": "eligible",
            "group": false,
            "visible_non_self_count": 1,
            "last_message_ms": 0,
            "participants": [{
                "participant_id": "google-peer",
                "identifier_type": 1,
                "raw_number": recipient,
                "formatted_number": "",
                "participant_id_present": true,
                "number_present": true,
                "visible": true,
                "self": false,
                "canonical_address": recipient,
                "canonical_address_present": true,
                "display_name": "",
            }],
            "name": "",
            "is_group_chat": false,
            "read_only": false,
            "invisible_participant_count": 0,
            "member_digest": "",
        }],
    })
    .to_string()
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
fn pending_native_rcs_commands_returns_the_sanitized_candidate_shape() {
    let fixture = fixture();
    with_client(&fixture.gateway.client, |core| {
        core.record_rcs_link_lifecycle(
            peppy_client_core::RcsCapabilityState::Linked,
            true,
            true,
            true,
            1_000,
            1_000,
        )
    })
    .unwrap();

    assert_eq!(
        gateway_pending_native_rcs_commands(fixture.gateway.clone()).unwrap(),
        vec![NativePendingRcsCommand {
            command_id: fixture.command_id,
            conversation_id: fixture.conversation_id,
            recipient: "peer-local".into(),
        }]
    );
}

#[test]
fn pending_native_rcs_commands_returns_closed_after_dispose() {
    let fixture = fixture();
    fixture.gateway.dispose().unwrap();
    assert!(matches!(
        gateway_pending_native_rcs_commands(fixture.gateway),
        Err(MobileBindingsError::Closed)
    ));
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
fn carrier_begin_refusals_are_binding_invalid_requests_without_attempts() {
    for wrong_sim in [true, false] {
        let fixture = carrier_fixture_with_google_conversation_id_and_peer(
            "conversation-carrier-refusal",
            "+12025550123",
        );
        let preparation = native_preparation(&fixture);
        let mut observation = fixture.observation.clone();
        if wrong_sim {
            observation.selected_sim_id = "android-subscription-8".into();
        } else {
            observation.participants[0].identity = "+12025550124".into();
        }
        assert!(matches!(
            preparation.begin(serde_json::to_string(&observation).unwrap(), 1_000),
            Err(MobileBindingsError::InvalidRequest)
        ));
        let db = persisted::Connection::open(&fixture.database_path);
        assert!(
            db.query(&format!(
                "SELECT state FROM attempts WHERE command_id='{}'",
                fixture.command_id
            ))
            .is_empty()
        );
    }
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

#[test]
fn trusted_wrappers_use_core_values_and_reject_closed_or_oversized_input() {
    let fixture = fixture();
    assert_eq!(
        rcs_canonicalizer_revision(),
        peppy_client_core::RCS_CANONICALIZER_REVISION
    );
    assert_eq!(
        gateway_verify_rcs_mapping(fixture.gateway.clone(), observation_json(&fixture), 1_000)
            .unwrap(),
        NativeRcsMappingVerification::Refused
    );
    assert_eq!(
        gateway_next_rcs_context_tuple(
            fixture.gateway.clone(),
            "runtime".into(),
            1,
            1,
            "verified-handset".into(),
        )
        .unwrap(),
        NativeRcsContextTuple {
            link_generation: 1,
            session_generation: 1,
            context_revision: 2,
        }
    );
    let conversation_id = with_client(&fixture.gateway.client, |core| {
        Ok(core
            .pending_automatic_commands()?
            .pop()
            .unwrap()
            .message
            .record
            .conversation_id)
    })
    .unwrap()
    .to_string();
    assert_eq!(
        gateway_next_rcs_mapping_revision(fixture.gateway.clone(), conversation_id.clone())
            .unwrap(),
        2
    );
    let mut ambiguous = fixture.observation.clone();
    ambiguous.google_candidate_count = 0;
    ambiguous.native_evidence.google_candidate_count = 0;
    assert_eq!(
        gateway_verify_rcs_mapping(
            fixture.gateway.clone(),
            serde_json::to_string(&ambiguous).unwrap(),
            1_000,
        )
        .unwrap(),
        NativeRcsMappingVerification::GoogleConversationAmbiguous
    );
    assert!(matches!(
        gateway_verify_rcs_mapping(fixture.gateway.clone(), "{".into(), 1_000),
        Err(MobileBindingsError::InvalidRequest)
    ));
    assert!(matches!(
        gateway_verify_rcs_mapping(
            fixture.gateway.clone(),
            "x".repeat(RCS_NATIVE_JSON_CAP + 1),
            1_000
        ),
        Err(MobileBindingsError::InvalidRequest)
    ));
    assert!(matches!(
        gateway_classify_rcs_target(
            fixture.gateway.clone(),
            conversation_id,
            "peer-local".into(),
            "x".repeat(RCS_NATIVE_SNAPSHOT_CAP + 1),
            1_000,
        ),
        Err(MobileBindingsError::InvalidRequest)
    ));

    let closed = Arc::new(NativeClient {
        client: Mutex::new(None),
    });
    assert!(matches!(
        gateway_next_rcs_context_tuple(closed, "runtime".into(), 1, 1, "pairing".into()),
        Err(MobileBindingsError::Closed)
    ));
}

#[test]
fn committed_handle_keeps_core_issued_identifiers_without_reparsing_commit_json() {
    let fixture = fixture();
    let committed = native_preparation(&fixture)
        .begin(observation_json(&fixture), 1_000)
        .unwrap();
    let projection: Value = serde_json::from_str(&committed.commit_json()).unwrap();
    assert_eq!(
        committed.command_id(),
        projection["command_id"].as_str().unwrap()
    );
    assert_eq!(committed.tmp_id(), projection["tmp_id"].as_str().unwrap());
    assert_eq!(
        committed.google_conversation_id(),
        projection["target"]["google_conversation_id"]
            .as_str()
            .unwrap()
    );
}

#[test]
fn trusted_effectful_wrappers_forward_core_results_without_cross_owner_mutation() {
    let recipient = "+12025550123";
    let fixture = fixture_with_google_conversation_id_and_peer("conversation-found", recipient);
    let conversation_id = with_client(&fixture.gateway.client, |core| {
        Ok(core
            .pending_automatic_commands()?
            .pop()
            .unwrap()
            .message
            .record
            .conversation_id
            .to_string())
    })
    .unwrap();
    assert_eq!(
        gateway_classify_rcs_target(
            fixture.gateway.clone(),
            conversation_id.clone(),
            recipient.into(),
            candidate_snapshot("conversation-found", recipient),
            1_000,
        )
        .unwrap(),
        NativeRcsTargetDecision::Found {
            google_conversation_id: "conversation-found".into(),
            snapshot_token: "1:1:1".into(),
        }
    );
    assert_eq!(
        gateway_classify_rcs_target(
            fixture.gateway.clone(),
            conversation_id.clone(),
            recipient.into(),
            "{".into(),
            1_000,
        )
        .unwrap(),
        NativeRcsTargetDecision::Unknown
    );
    let absence = fixture_with_google_conversation_id_and_peer("conversation-absence", recipient);
    let absence_conversation_id = with_client(&absence.gateway.client, |core| {
        Ok(core
            .pending_automatic_commands()?
            .pop()
            .unwrap()
            .message
            .record
            .conversation_id
            .to_string())
    })
    .unwrap();
    let mut complete_absence: Value =
        serde_json::from_str(&candidate_snapshot("unused", recipient)).unwrap();
    complete_absence["conversations"] = json!([]);
    assert_eq!(
        gateway_classify_rcs_target(
            absence.gateway.clone(),
            absence_conversation_id.clone(),
            recipient.into(),
            complete_absence.to_string(),
            1_000,
        )
        .unwrap(),
        NativeRcsTargetDecision::NotFoundComplete
    );

    let absence_db = persisted::Connection::open(&absence.database_path);
    assert_eq!(absence_db.query(&format!(
        "SELECT invalidated_at_ms,invalidation_code FROM rcs_conversation_mappings WHERE peppy_conversation_id='{absence_conversation_id}'"
    )), vec![vec![Some("1000".into()), Some("complete_absence".into())]]);
    let proof = absence_db.query(&format!(
        "SELECT v FROM metadata WHERE k='rcs_carrier_only_proof_v1/{absence_conversation_id}'"
    ));
    assert_eq!(proof.len(), 1);
    let proof: Value = serde_json::from_str(proof[0][0].as_ref().unwrap()).unwrap();
    assert_eq!(proof["canonical_recipient"], recipient);
    assert_eq!(proof["subscription_id"], "android-subscription-7");

    let submitted =
        fixture_with_google_conversation_id_and_peer("conversation-submitted", recipient);
    let submitted_commit = native_preparation(&submitted)
        .begin(observation_json(&submitted), 1_000)
        .unwrap();
    gateway_record_rcs_submitted(submitted.gateway.clone(), submitted.command_id.clone()).unwrap();
    assert_persisted_send_state(&submitted, "submitted_to_os");
    assert!(
        gateway_record_rcs_pre_effect_refusal(
            submitted.gateway.clone(),
            submitted_commit.command_id(),
            submitted_commit.tmp_id(),
            submitted_commit.google_conversation_id(),
            false,
            1_000,
        )
        .is_err()
    );

    let refused = fixture_with_google_conversation_id_and_peer("conversation-refused", recipient);
    let committed = native_preparation(&refused)
        .begin(observation_json(&refused), 1_000)
        .unwrap();
    gateway_record_rcs_pre_effect_refusal(
        refused.gateway.clone(),
        committed.command_id(),
        committed.tmp_id(),
        committed.google_conversation_id(),
        false,
        1_000,
    )
    .unwrap();

    assert_persisted_send_state(&refused, "failed_before_submission");

    let acknowledged = fixture_with_google_conversation_id_and_peer("conversation-ack", recipient);
    let committed = native_preparation(&acknowledged)
        .begin(observation_json(&acknowledged), 1_000)
        .unwrap();
    gateway_record_rcs_transport_ack(acknowledged.gateway.clone(), committed.command_id(), 1_001)
        .unwrap();
    assert!(
        gateway_record_rcs_pre_effect_refusal(
            acknowledged.gateway.clone(),
            committed.command_id(),
            committed.tmp_id(),
            committed.google_conversation_id(),
            false,
            1_000,
        )
        .is_err()
    );

    let ack_db = persisted::Connection::open(&acknowledged.database_path);
    assert_eq!(ack_db.query(&format!(
        "SELECT transport_ack_at_ms,transport_ack_code FROM rcs_send_ledger WHERE command_id='{}'", acknowledged.command_id
    )), vec![vec![Some("1001".into()), Some("accepted".into())]]);

    let other = fixture_with_google_conversation_id_and_peer("conversation-other", recipient);
    assert!(gateway_record_rcs_submitted(other.gateway, refused.command_id.clone()).is_err());
    let closed = Arc::new(NativeClient {
        client: Mutex::new(None),
    });
    assert!(matches!(
        gateway_verify_rcs_mapping(closed.clone(), "{".into(), 1_000),
        Err(MobileBindingsError::Closed)
    ));
    assert!(matches!(
        gateway_classify_rcs_target(
            closed,
            conversation_id,
            recipient.into(),
            "x".repeat(RCS_NATIVE_SNAPSHOT_CAP + 1),
            1_000,
        ),
        Err(MobileBindingsError::Closed)
    ));
}

// Test-only access to the SQLCipher already linked by core. The bindings crate
// intentionally has no direct rusqlite dependency; keep this read-only inspector
// here rather than adding a production/test API or changing the dependency surface.
mod persisted {
    use std::ffi::{CStr, CString, c_char, c_int, c_void};
    use std::path::Path;
    use std::ptr;

    unsafe extern "C" {
        fn sqlite3_open_v2(
            path: *const c_char,
            db: *mut *mut c_void,
            flags: c_int,
            vfs: *const c_char,
        ) -> c_int;
        fn sqlite3_get_table(
            db: *mut c_void,
            sql: *const c_char,
            result: *mut *mut *mut c_char,
            rows: *mut c_int,
            columns: *mut c_int,
            error: *mut *mut c_char,
        ) -> c_int;
        fn sqlite3_free_table(result: *mut *mut c_char);
        fn sqlite3_close(db: *mut c_void) -> c_int;
    }

    pub struct Connection(*mut c_void);

    impl Connection {
        pub fn open(path: &Path) -> Self {
            let path = CString::new(path.to_str().unwrap()).unwrap();
            let mut db = ptr::null_mut();
            // SAFETY: valid NUL-terminated path and writable output pointer; flags=READONLY.
            let code = unsafe { sqlite3_open_v2(path.as_ptr(), &mut db, 1, ptr::null()) };
            let connection = Self(db);
            assert_eq!(code, 0, "open read-only fixture database");
            // Match core's raw-key syntax, not SQLCipher passphrase derivation.
            connection.query(&format!("PRAGMA key = \"x'{}'\"", "07".repeat(32)));
            connection
        }

        pub fn query(&self, sql: &str) -> Vec<Vec<Option<String>>> {
            let sql = CString::new(sql).unwrap();
            let mut result = ptr::null_mut();
            let mut rows = 0;
            let mut columns = 0;
            // SAFETY: live database, valid SQL, and writable outputs. No error allocation requested.
            let code = unsafe {
                sqlite3_get_table(
                    self.0,
                    sql.as_ptr(),
                    &mut result,
                    &mut rows,
                    &mut columns,
                    ptr::null_mut(),
                )
            };
            assert_eq!(code, 0, "fixture query: {sql:?}");
            // sqlite3_get_table owns a rectangular array, preceded by one header row.
            // Copy every value before freeing that array; SQL NULL is a null pointer.
            let values = (0..rows as usize)
                .map(|row| {
                    (0..columns as usize)
                        .map(|column| {
                            // SAFETY: row/column are within the successful get_table result.
                            let value =
                                unsafe { *result.add((row + 1) * columns as usize + column) };
                            if value.is_null() {
                                None
                            } else {
                                // SAFETY: SQLite supplies a NUL-terminated string for each non-NULL cell.
                                Some(
                                    unsafe { CStr::from_ptr(value) }
                                        .to_str()
                                        .unwrap()
                                        .to_owned(),
                                )
                            }
                        })
                        .collect()
                })
                .collect();
            // SAFETY: result was allocated by the successful sqlite3_get_table call above.
            unsafe { sqlite3_free_table(result) };
            values
        }

        pub fn send_rows(&self) -> Vec<Vec<Vec<Option<String>>>> {
            [
                "attempts",
                "rcs_send_ledger",
                "send_status",
                "outbox",
                "metadata",
            ]
            .into_iter()
            .map(|table| {
                // quote() preserves BLOBs as hex and distinguishes NULL from text.
                let columns = self
                    .query(&format!("PRAGMA table_info({table})"))
                    .into_iter()
                    .map(|row| format!("quote(\"{}\")", row[1].as_ref().unwrap()))
                    .collect::<Vec<_>>()
                    .join(",");
                self.query(&format!("SELECT {columns} FROM {table} ORDER BY rowid"))
            })
            .collect()
        }
    }

    impl Drop for Connection {
        fn drop(&mut self) {
            // SAFETY: this helper uniquely owns the connection; get_table leaves no statements.
            unsafe { sqlite3_close(self.0) };
        }
    }
}

fn assert_persisted_send_state(fixture: &Fixture, expected: &str) {
    let db = persisted::Connection::open(&fixture.database_path);
    for table in ["attempts", "send_status"] {
        assert_eq!(
            db.query(&format!(
                "SELECT state FROM {table} WHERE command_id='{}'",
                fixture.command_id
            )),
            vec![vec![Some(expected.into())]]
        );
    }
}

#[test]
fn trusted_mapping_wrapper_returns_mapped_and_persists_the_exact_mapping() {
    let fixture =
        fixture_with_google_conversation_id_and_peer("conversation-mapped", "+12025550123");
    let mut observation = fixture.observation.clone();
    observation.mapping_revision = gateway_next_rcs_mapping_revision(
        fixture.gateway.clone(),
        with_client(&fixture.gateway.client, |core| {
            Ok(core
                .pending_automatic_commands()?
                .pop()
                .unwrap()
                .message
                .record
                .conversation_id
                .to_string())
        })
        .unwrap(),
    )
    .unwrap();
    assert_eq!(
        gateway_verify_rcs_mapping(
            fixture.gateway.clone(),
            serde_json::to_string(&observation).unwrap(),
            1_000
        )
        .unwrap(),
        NativeRcsMappingVerification::Mapped
    );
    let db = persisted::Connection::open(&fixture.database_path);
    assert_eq!(db.query("SELECT google_conversation_id,mapping_revision FROM rcs_conversation_mappings WHERE invalidated_at_ms IS NULL"), vec![vec![Some("conversation-mapped".into()), Some("2".into())]]);
}

#[test]
fn all_native_recorders_reject_closed_and_wrong_owners_without_mutating_either_database() {
    let owner = fixture_with_google_conversation_id_and_peer("conversation-owner", "+12025550123");
    let committed = native_preparation(&owner)
        .begin(observation_json(&owner), 1_000)
        .unwrap();
    let other = fixture_with_google_conversation_id_and_peer("conversation-other", "+12025550123");
    native_preparation(&other)
        .begin(observation_json(&other), 1_000)
        .unwrap();
    let owner_db = persisted::Connection::open(&owner.database_path);
    let other_db = persisted::Connection::open(&other.database_path);
    let owner_before = owner_db.send_rows();
    let other_before = other_db.send_rows();
    for recorder in 0..4 {
        let result = match recorder {
            0 => gateway_record_rcs_transport_ack(
                other.gateway.clone(),
                committed.command_id(),
                1_001,
            ),
            1 => gateway_record_rcs_submitted(other.gateway.clone(), committed.command_id()),
            2 => gateway_record_rcs_pre_effect_refusal(
                other.gateway.clone(),
                committed.command_id(),
                committed.tmp_id(),
                committed.google_conversation_id(),
                false,
                1_000,
            ),
            _ => gateway_record_rcs_outcome_unknown(
                other.gateway.clone(),
                committed.command_id(),
                committed.tmp_id(),
                committed.google_conversation_id(),
            ),
        };
        assert!(result.is_err());
        assert_eq!(owner_db.send_rows(), owner_before);
        assert_eq!(other_db.send_rows(), other_before);
    }

    owner.gateway.dispose().unwrap();
    for command_id in [committed.command_id(), "malformed-command".into()] {
        for result in [
            gateway_record_rcs_transport_ack(owner.gateway.clone(), command_id.clone(), 1_001),
            gateway_record_rcs_submitted(owner.gateway.clone(), command_id.clone()),
            gateway_record_rcs_pre_effect_refusal(
                owner.gateway.clone(),
                command_id,
                committed.tmp_id(),
                committed.google_conversation_id(),
                false,
                1_000,
            ),
            gateway_record_rcs_outcome_unknown(
                owner.gateway.clone(),
                committed.command_id(),
                committed.tmp_id(),
                committed.google_conversation_id(),
            ),
        ] {
            assert!(matches!(result, Err(MobileBindingsError::Closed)));
        }
    }
    assert_eq!(owner_db.send_rows(), owner_before);
    assert_eq!(other_db.send_rows(), other_before);
}

#[test]
fn native_outcome_unknown_recorder_is_exact_and_closed_clients_are_rejected() {
    let fixture = fixture();
    let committed = native_preparation(&fixture)
        .begin(observation_json(&fixture), 1_000)
        .unwrap();
    assert!(
        gateway_record_rcs_outcome_unknown(
            fixture.gateway.clone(),
            committed.command_id(),
            "foreign-tmp".into(),
            committed.google_conversation_id(),
        )
        .is_err()
    );
    gateway_record_rcs_outcome_unknown(
        fixture.gateway.clone(),
        committed.command_id(),
        committed.tmp_id(),
        committed.google_conversation_id(),
    )
    .unwrap();
    assert_persisted_send_state(&fixture, "outcome_unknown");
    fixture.gateway.dispose().unwrap();
    assert!(matches!(
        gateway_record_rcs_outcome_unknown(
            fixture.gateway.clone(),
            committed.command_id(),
            committed.tmp_id(),
            committed.google_conversation_id(),
        ),
        Err(MobileBindingsError::Closed)
    ));
}

#[test]
fn explicit_unlink_completion_wrapper_requires_current_unlinked_lifecycle() {
    let fixture = fixture();
    assert!(
        gateway_record_rcs_explicit_unlink_completed(fixture.gateway.clone(), 1_000, 1_000)
            .is_err()
    );
    with_client(&fixture.gateway.client, |core| {
        core.record_rcs_link_lifecycle(
            peppy_client_core::RcsCapabilityState::Unlinked,
            false,
            false,
            false,
            1_000,
            1_000,
        )
    })
    .unwrap();
    gateway_record_rcs_explicit_unlink_completed(fixture.gateway.clone(), 1_000, 1_000).unwrap();
    fixture.gateway.dispose().unwrap();
    assert!(matches!(
        gateway_record_rcs_explicit_unlink_completed(fixture.gateway.clone(), 1_000, 1_000),
        Err(MobileBindingsError::Closed)
    ));
}

#[path = "rcs_native_tests/rcs_native_live_ticket_tests.rs"]
mod rcs_native_live_ticket_tests;

#[path = "rcs_native_tests/group_fixture.rs"]
mod group_fixture;

#[path = "rcs_native_tests/rcs_native_group_binding_tests.rs"]
mod rcs_native_group_binding_tests;

#[path = "rcs_native_tests/rcs_native_group_receive_tests.rs"]
mod rcs_native_group_receive_tests;

#[test]
fn eligibility_publication_is_derived_from_core_mapping_and_revisions() {
    let fixture = fixture();
    with_client(&fixture.gateway.client, |core| {
        core.record_rcs_link_lifecycle(RcsCapabilityState::Linked, true, true, true, 1_000, 1_000)
    })
    .unwrap();
    let unmapped = Uuid::new_v4().to_string();

    assert_eq!(
        gateway_publish_rcs_eligibility(
            fixture.gateway.clone(),
            vec![
                NativeRcsEligibilityPublication {
                    conversation_id: fixture.conversation_id.clone(),
                },
                NativeRcsEligibilityPublication {
                    conversation_id: unmapped.clone(),
                },
            ],
            1_000,
        )
        .unwrap(),
        2
    );
    let (mapped, absent) = with_client(&fixture.gateway.client, |core| {
        let gateway = core.device_id()?;
        Ok((
            core.rcs_eligibility_projection(
                fixture.conversation_id.parse().unwrap(),
                gateway,
                1_000,
            )?
            .unwrap(),
            core.rcs_eligibility_projection(unmapped.parse().unwrap(), gateway, 1_000)?
                .unwrap(),
        ))
    })
    .unwrap();
    assert_eq!(mapped.eligibility, RcsEligibility::Eligible);
    assert_eq!(absent.eligibility, RcsEligibility::NotEligible);
    assert_ne!(mapped.mapping_revision, 999);
    assert_ne!(absent.mapping_revision, 998);
    let outbox_before_rejection = with_client(&fixture.gateway.client, |core| {
        Ok(core.pending_outbox()?.len())
    })
    .unwrap();
    assert!(matches!(
        gateway_publish_rcs_eligibility(
            fixture.gateway.clone(),
            vec![
                NativeRcsEligibilityPublication {
                    conversation_id: fixture.conversation_id.clone(),
                },
                NativeRcsEligibilityPublication {
                    conversation_id: fixture.conversation_id.clone(),
                },
            ],
            1_000,
        ),
        Err(MobileBindingsError::InvalidRequest)
    ));
    assert_eq!(
        with_client(&fixture.gateway.client, |core| {
            Ok(core.pending_outbox()?.len())
        })
        .unwrap(),
        outbox_before_rejection
    );

    gateway_publish_rcs_eligibility(
        fixture.gateway.clone(),
        vec![NativeRcsEligibilityPublication {
            conversation_id: fixture.conversation_id.clone(),
        }],
        1_000,
    )
    .unwrap();
    let next = with_client(&fixture.gateway.client, |core| {
        core.rcs_eligibility_projection(
            fixture.conversation_id.parse().unwrap(),
            core.device_id()?,
            1_000,
        )
    })
    .unwrap()
    .unwrap();
    assert!(next.mapping_revision > mapped.mapping_revision);
    assert_eq!(next.eligibility, RcsEligibility::Eligible);

    with_client(&fixture.gateway.client, |core| {
        core.record_rcs_link_lifecycle(
            RcsCapabilityState::NeedsRelink,
            false,
            false,
            false,
            1_001,
            1_001,
        )
    })
    .unwrap();
    gateway_publish_rcs_eligibility(
        fixture.gateway.clone(),
        vec![NativeRcsEligibilityPublication {
            conversation_id: fixture.conversation_id.clone(),
        }],
        1_001,
    )
    .unwrap();
    let unavailable = with_client(&fixture.gateway.client, |core| {
        core.rcs_eligibility_projection(
            fixture.conversation_id.parse().unwrap(),
            core.device_id()?,
            1_001,
        )
    })
    .unwrap()
    .unwrap();
    assert_eq!(unavailable.eligibility, RcsEligibility::Unknown);
    assert!(unavailable.mapping_revision > next.mapping_revision);
}

#[test]
fn receive_context_exports_require_current_local_link_ownership() {
    let fixture = fixture();
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    let generation = with_client(&fixture.gateway.client, |core| {
        core.record_rcs_link_lifecycle(
            RcsCapabilityState::Linked,
            true,
            true,
            true,
            now_ms,
            now_ms,
        )?;
        Ok(core
            .begin_rcs_link_ownership(fixture.conversation_id.parse().unwrap(), 1)?
            .ownership_generation)
    })
    .unwrap();
    let outbox_before_reads = with_client(&fixture.gateway.client, |core| {
        Ok(core.pending_outbox()?.len())
    })
    .unwrap();

    let context = fixture
        .gateway
        .gateway_rcs_receive_context(fixture.observation.google_conversation_id.clone())
        .unwrap()
        .unwrap();
    assert_eq!(context.conversation_id, fixture.conversation_id);
    assert_eq!(
        context.google_conversation_id,
        fixture.observation.google_conversation_id
    );
    assert_eq!(context.ownership_generation, generation);
    assert_eq!(context.expectation.ownership_generation, generation);
    assert_eq!(context.expectation.link_generation, 1);
    assert_eq!(context.expectation.session_generation, 1);
    assert_eq!(context.expectation.context_revision, 1);
    assert!(context.watermark_ms > 0);
    assert!(context.resume_hint.is_none());
    assert!(!context.incomplete);
    let owned = fixture
        .gateway
        .gateway_rcs_link_owned_conversations()
        .unwrap();
    assert_eq!(owned.len(), 1);
    assert_eq!(
        owned[0].google_conversation_id,
        context.google_conversation_id
    );
    assert_eq!(owned[0].expectation, context.expectation);
    assert_eq!(
        with_client(&fixture.gateway.client, |core| {
            Ok(core.pending_outbox()?.len())
        })
        .unwrap(),
        outbox_before_reads
    );
    let mut changed_expectation = context.expectation.clone();
    changed_expectation.context_revision += 1;
    assert!(matches!(
        fixture
            .gateway
            .gateway_invalidate_rcs_context_for_google_event(
                context.conversation_id.clone(),
                context.google_conversation_id.clone(),
                changed_expectation,
            ),
        Err(MobileBindingsError::InvalidRequest)
    ));
    assert_eq!(
        fixture
            .gateway
            .gateway_invalidate_rcs_context_for_google_event(
                context.conversation_id.clone(),
                context.google_conversation_id.clone(),
                context.expectation,
            )
            .unwrap(),
        2
    );

    with_client(&fixture.gateway.client, |core| {
        core.begin_rcs_unlink_cutover(fixture.conversation_id.parse().unwrap())?;
        Ok(())
    })
    .unwrap();
    let outbox_before_provider_reads = with_client(&fixture.gateway.client, |core| {
        Ok(core.pending_outbox()?.len())
    })
    .unwrap();
    assert!(
        fixture
            .gateway
            .gateway_rcs_receive_context(fixture.observation.google_conversation_id.clone())
            .unwrap()
            .is_none()
    );
    assert!(
        fixture
            .gateway
            .gateway_rcs_link_owned_conversations()
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        with_client(&fixture.gateway.client, |core| {
            Ok(core.pending_outbox()?.len())
        })
        .unwrap(),
        outbox_before_provider_reads
    );

    with_client(&fixture.gateway.client, |core| {
        core.record_rcs_link_lifecycle(
            RcsCapabilityState::SignedOut,
            false,
            false,
            false,
            now_ms + 1,
            now_ms + 1,
        )
    })
    .unwrap();
    assert!(matches!(
        fixture
            .gateway
            .gateway_rcs_receive_context(fixture.observation.google_conversation_id.clone()),
        Err(MobileBindingsError::InvalidRequest)
    ));
    assert!(matches!(
        fixture.gateway.gateway_rcs_link_owned_conversations(),
        Err(MobileBindingsError::InvalidRequest)
    ));
}

#[test]
fn bootstrap_batch_json_resolves_candidates_in_input_order() {
    let fixture = fixture();
    let candidates = json!([
        {"participantIdentity": "peer-local", "googleConversationId": "conversation-local"},
        {"participantIdentity": "   ", "googleConversationId": "conversation-local"},
        {"participantIdentity": "unknown-peer", "googleConversationId": "conversation-local"},
    ])
    .to_string();
    assert_eq!(
        gateway_resolve_rcs_bootstrap_conversations(fixture.gateway.clone(), candidates).unwrap(),
        vec![
            NativeRcsBootstrapResolution::Resolved {
                conversation_id: fixture.conversation_id.clone(),
            },
            NativeRcsBootstrapResolution::InvalidIdentity,
            NativeRcsBootstrapResolution::Missing,
        ]
    );
}

#[test]
fn bootstrap_batch_json_rejects_unknown_fields_oversize_and_too_many_candidates() {
    let fixture = fixture();
    let unknown = json!([
        {"participantIdentity": "peer-local", "googleConversationId": "conversation-local", "extra": true},
    ])
    .to_string();
    assert!(matches!(
        gateway_resolve_rcs_bootstrap_conversations(fixture.gateway.clone(), unknown),
        Err(MobileBindingsError::InvalidRequest)
    ));
    let oversize = json!(
        (0..40)
            .map(|_| json!({"participantIdentity": "a".repeat(2_000), "googleConversationId": "conversation-local"}))
            .collect::<Vec<_>>()
    )
    .to_string();
    assert!(oversize.len() > 65_536);
    assert!(matches!(
        gateway_resolve_rcs_bootstrap_conversations(fixture.gateway.clone(), oversize),
        Err(MobileBindingsError::InvalidRequest)
    ));
    let too_many = json!(
        (0..41)
            .map(|index| json!({"participantIdentity": format!("peer-{index}"), "googleConversationId": "conversation-local"}))
            .collect::<Vec<_>>()
    )
    .to_string();
    assert!(matches!(
        gateway_resolve_rcs_bootstrap_conversations(fixture.gateway.clone(), too_many),
        Err(MobileBindingsError::InvalidRequest)
    ));
}

#[test]
fn receive_context_exports_refuse_a_foreign_current_gateway_and_closed_client() {
    let fixture = fixture();
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    with_client(&fixture.gateway.client, |core| {
        core.record_rcs_link_lifecycle(
            RcsCapabilityState::Linked,
            true,
            true,
            true,
            now_ms,
            now_ms,
        )?;
        core.begin_rcs_link_ownership(fixture.conversation_id.parse().unwrap(), 1)?;
        Ok(())
    })
    .unwrap();
    with_client(&fixture.sender.client, |core| {
        core.record_rcs_link_lifecycle(RcsCapabilityState::Linked, true, true, true, now_ms, now_ms)
    })
    .unwrap();
    deliver_from(&fixture.sender, &fixture.gateway, fixture.next_cursor);

    assert!(matches!(
        fixture
            .gateway
            .gateway_rcs_receive_context(fixture.observation.google_conversation_id.clone()),
        Err(MobileBindingsError::InvalidRequest)
    ));
    assert!(matches!(
        fixture.gateway.gateway_rcs_link_owned_conversations(),
        Err(MobileBindingsError::InvalidRequest)
    ));
    fixture.gateway.dispose().unwrap();
    assert!(matches!(
        fixture
            .gateway
            .gateway_rcs_receive_context(fixture.observation.google_conversation_id),
        Err(MobileBindingsError::Closed)
    ));
    assert!(matches!(
        fixture.gateway.gateway_rcs_link_owned_conversations(),
        Err(MobileBindingsError::Closed)
    ));
}

#[test]
fn one_to_one_preparation_has_no_member_digest_and_keeps_the_three_key_request() {
    let fixture = fixture();
    let preparation = native_preparation(&fixture);
    assert_eq!(preparation.expected_member_digest(), None);
    let facts: Value = serde_json::from_str(&preparation.facts_json().unwrap()).unwrap();
    let request = facts["request"].as_object().unwrap();
    assert_eq!(request.len(), 3);
    assert!(!request.contains_key("expected_member_digest"));
    assert_eq!(facts["stored_mapping"]["class"], "OneToOne");
}

#[test]
fn group_members_changed_is_its_own_mobile_error() {
    assert!(matches!(
        MobileBindingsError::from(peppy_client_core::Error::GroupMembersChanged),
        MobileBindingsError::GroupMembersChanged
    ));
}

#[test]
fn group_verification_refuses_a_one_to_one_observation_without_touching_the_mapping() {
    let fixture = fixture();
    let verification = gateway_verify_rcs_group_observation(
        fixture.gateway.clone(),
        observation_json(&fixture),
        1_000,
    )
    .unwrap();
    assert_eq!(verification, NativeRcsGroupVerification::Refused);
    assert!(native_preparation(&fixture).facts_json().is_ok());
}
