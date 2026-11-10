use super::*;

#[test]
#[ignore = "requires an explicitly generated Go observation fixture"]
fn actual_go_observations_verify_classify_and_begin() {
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct GoFixture {
        route: String,
        target: String,
        snapshot: String,
        now_ms: i64,
    }
    let path = std::env::var("PEPPY_RCS_GO_OBSERVATION_PATH").expect("Go fixture path required");
    let fixture: GoFixture = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    let route: RcsMappingObservation = serde_json::from_str(&fixture.route).unwrap();
    let target: RcsMappingObservation = serde_json::from_str(&fixture.target).unwrap();
    let (_directory, _vault, config, gateway, command_id, _) =
        rcs_command_for_peer_with_body("+12025550123", "+12025550123", "Rust-owned body".into());
    assert_eq!(
        gateway.verify_rcs_mapping(&route, fixture.now_ms).unwrap(),
        RcsMappingOutcome::Refused(
            peppy_client_core::RcsMappingRefusal::GoogleConversationAmbiguous
        )
    );
    let conversation_id = command_conversation(&gateway);
    assert!(
        gateway
            .carrier_route_context(&route.selected_sim_id)
            .unwrap()
            .is_some()
    );
    assert!(
        matches!(gateway.classify_and_commit_rcs_target(conversation_id, "+12025550123", &fixture.snapshot, fixture.now_ms).unwrap(),
        peppy_client_core::RcsTargetDecision::Found { google_conversation_id, .. } if google_conversation_id == target.google_conversation_id)
    );
    assert!(matches!(
        gateway.verify_rcs_mapping(&target, fixture.now_ms).unwrap(),
        RcsMappingOutcome::Mapped(_)
    ));
    let preparation = gateway
        .prepare_native_rcs_send(command_id, fixture.now_ms)
        .unwrap();
    let committed = gateway
        .begin_native_rcs_send(preparation, &target, fixture.now_ms)
        .unwrap();
    assert_eq!(committed.message_body, "Rust-owned body");
    assert_eq!(
        committed.target.google_conversation_id,
        target.google_conversation_id
    );
    assert_eq!(attempt_count(&config, command_id), 1);
    if let Ok(path) = std::env::var("PEPPY_RCS_NATIVE_FIXTURE_PATH") {
        std::fs::write(path, serde_json::to_vec(&committed).unwrap()).unwrap();
    }
}
use rusqlite::OptionalExtension;

#[test]
fn native_preparation_does_not_admit_an_attempt_and_begin_uses_the_stored_command() {
    let (_directory, _vault, config, gateway, command_id, observation) = rcs_command("peer-local");
    let preparation = gateway.prepare_native_rcs_send(command_id, 1_000).unwrap();

    assert_eq!(preparation.request().command_id, command_id.to_string());
    assert_eq!(
        preparation.request().peppy_conversation_id,
        preparation.mapping().conversation_id.to_string()
    );
    assert_eq!(preparation.request().canonical_recipient, "raw:peer-local");
    assert_eq!(attempt_count(&config, command_id), 0);
    assert_eq!(rcs_ledger_count(&config, command_id), 0);

    let expected_mapping = preparation.mapping().clone();
    let projection = gateway
        .begin_native_rcs_send(preparation, &observation, 1_000)
        .unwrap();

    assert_eq!(projection.command_id, command_id.to_string());
    assert_eq!(
        projection.mapping_revision,
        expected_mapping.mapping_revision
    );
    assert_eq!(projection.proof_digest, expected_mapping.proof.digest);
    assert_eq!(projection.proof_version, expected_mapping.proof_version);
    assert!(!projection.tmp_id.is_empty());
    assert_eq!(projection.message_body, String::from('\0'));
    assert_eq!(
        projection.target.google_conversation_id,
        expected_mapping.google_conversation_id
    );
    assert_eq!(attempt_count(&config, command_id), 1);
    assert_eq!(rcs_ledger_count(&config, command_id), 1);

    let serialized = serde_json::to_value(&projection).unwrap();
    assert_eq!(serialized["proof_digest"].as_array().unwrap().len(), 32);
    assert!(
        serialized["proof_digest"]
            .as_array()
            .unwrap()
            .iter()
            .zip(expected_mapping.proof.digest)
            .all(|(value, expected)| value.as_u64() == Some(u64::from(expected)))
    );
    assert_eq!(
        serialized["target"]["evidence"],
        serde_json::to_value(&observation.native_evidence).unwrap()
    );
    assert_eq!(
        serialized["target"]["carrier_binding"],
        serde_json::to_value(&observation.carrier_binding).unwrap()
    );
    assert_eq!(
        serialized["target"]["outgoing"],
        serde_json::to_value(&observation.outgoing).unwrap()
    );
    assert_eq!(
        projection.target.peppy_conversation_id,
        expected_mapping.conversation_id.to_string()
    );
    assert_eq!(
        projection.target.proof_version,
        expected_mapping.proof_version
    );
    assert_eq!(
        projection.target.canonical_recipient,
        expected_mapping.proof.canonical_participant
    );
    assert_eq!(
        projection.target.canonical_own_identity,
        expected_mapping.proof.own_identity
    );
    assert_eq!(
        projection.target.identity_region,
        expected_mapping.proof.identity_region
    );
    assert_eq!(
        projection.target.google_conversation_id,
        observation.google_conversation_id
    );
    assert_eq!(
        projection.target.conversation_type,
        observation.conversation_type
    );
    assert_eq!(projection.target.status, observation.status);
    assert_eq!(projection.target.send_mode, observation.send_mode);
    assert_eq!(projection.target.read_only, observation.read_only);
    assert_eq!(
        projection.target.default_outgoing,
        observation.default_outgoing
    );
    assert_eq!(
        projection.target.selected_sim_id,
        observation.selected_sim_id
    );
    assert_eq!(
        projection.target.canonicalizer_revision,
        observation.canonicalizer_revision
    );
    assert_eq!(
        projection.target.participants.len(),
        observation.participants.len()
    );
    for (actual, expected) in projection
        .target
        .participants
        .iter()
        .zip(&observation.participants)
    {
        assert_eq!(actual.identity, expected.identity);
        assert_eq!(actual.google_participant_id, expected.google_participant_id);
        assert_eq!(actual.identifier_type, expected.identifier_type);
        assert_eq!(actual.is_visible, expected.is_visible);
        assert_eq!(actual.is_me, expected.is_me);
    }
    assert_eq!(
        expected_mapping.class,
        peppy_client_core::RcsMappingClass::OneToOne
    );
    assert_eq!(
        projection.target.class,
        peppy_client_core::NativeRcsTargetClass::OneToOne
    );
    assert_eq!(projection.target.evidence, observation.native_evidence);
    assert_eq!(projection.target.outgoing, observation.outgoing);
    assert_eq!(
        projection.target.carrier_binding,
        observation.carrier_binding
    );

    if let Some(path) = std::env::var_os("PEPPY_RCS_NATIVE_FIXTURE_PATH") {
        std::fs::write(path, serde_json::to_vec(&projection).unwrap()).unwrap();
    }
}

#[test]
fn native_preparation_accepts_a_4096_byte_utf8_body() {
    let body = "é".repeat(2_048);
    let (_directory, _vault, config, gateway, command_id, observation) =
        rcs_command_with_body("peer-local", body.clone());
    let preparation = gateway.prepare_native_rcs_send(command_id, 1_000).unwrap();

    let projection = gateway
        .begin_native_rcs_send(preparation, &observation, 1_000)
        .unwrap();
    assert_eq!(projection.message_body, body);
    assert_eq!(attempt_count(&config, command_id), 1);
}

fn replace_stored_body(config: &ClientConfig, command_id: CommandId, body: String) {
    let connection = encrypted_connection(config);
    let payload: String = connection
        .query_row(
            "SELECT payload FROM command_ledger WHERE command_id=?",
            [command_id.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    let mut message: MessagePayload = serde_json::from_str(&payload).unwrap();
    message.body = body;
    connection
        .execute(
            "UPDATE command_ledger SET payload=? WHERE command_id=?",
            rusqlite::params![
                serde_json::to_string(&message).unwrap(),
                command_id.to_string()
            ],
        )
        .unwrap();
}

#[test]
fn native_preparation_rejects_empty_and_exactly_4097_byte_stored_bodies() {
    for body in [String::new(), format!("{}a", "é".repeat(2_048))] {
        let (_directory, _vault, config, gateway, command_id, _observation) =
            rcs_command("peer-local");
        replace_stored_body(&config, command_id, body);

        assert!(matches!(
            gateway.prepare_native_rcs_send(command_id, 1_000),
            Err(Error::InvalidRequest("RCS body out of bounds"))
        ));
        assert_eq!(attempt_count(&config, command_id), 0);
        assert_eq!(rcs_ledger_count(&config, command_id), 0);
    }
}

#[test]
fn native_begin_rechecks_replaced_empty_and_exactly_4097_byte_stored_bodies() {
    for body in [String::new(), format!("{}a", "é".repeat(2_048))] {
        let (_directory, _vault, config, gateway, command_id, observation) =
            rcs_command("peer-local");
        let preparation = gateway.prepare_native_rcs_send(command_id, 1_000).unwrap();
        replace_stored_body(&config, command_id, body);

        assert!(matches!(
            gateway.begin_native_rcs_send(preparation, &observation, 1_000),
            Err(Error::InvalidRequest("RCS body out of bounds"))
        ));
        assert_eq!(attempt_count(&config, command_id), 0);
        assert_eq!(rcs_ledger_count(&config, command_id), 0);
    }
}

#[test]
fn context_tuple_allocator_fences_go_lifecycles_without_using_core_generations() {
    let (_directory, _vault, config, gateway, command_id, _observation) = rcs_command("peer-local");

    assert_eq!(
        gateway
            .next_rcs_context_tuple("runtime", 1, 1, "verified-handset")
            .unwrap(),
        RcsContextTuple {
            link_generation: 1,
            session_generation: 1,
            context_revision: 2,
        }
    );
    assert_eq!(
        gateway
            .next_rcs_context_tuple("new-runtime", 1, 1, "verified-handset")
            .unwrap(),
        RcsContextTuple {
            link_generation: 1,
            session_generation: 2,
            context_revision: 1,
        }
    );
    assert_eq!(
        gateway
            .next_rcs_context_tuple("runtime", 2, 1, "verified-handset")
            .unwrap(),
        RcsContextTuple {
            link_generation: 1,
            session_generation: 2,
            context_revision: 1,
        }
    );
    assert_eq!(
        gateway
            .next_rcs_context_tuple("runtime", 1, 2, "verified-handset")
            .unwrap(),
        RcsContextTuple {
            link_generation: 1,
            session_generation: 2,
            context_revision: 1,
        }
    );
    assert_eq!(
        gateway
            .next_rcs_context_tuple("runtime", 1, 1, "new-pairing")
            .unwrap(),
        RcsContextTuple {
            link_generation: 2,
            session_generation: 1,
            context_revision: 1,
        }
    );
    let preparation = gateway.prepare_native_rcs_send(command_id, 1_000).unwrap();
    assert_eq!(
        gateway
            .next_rcs_mapping_revision(preparation.mapping().conversation_id)
            .unwrap(),
        2
    );
    let conversation_id = preparation.mapping().conversation_id;
    let connection = encrypted_connection(&config);
    connection
        .execute(
            "UPDATE rcs_conversation_mappings SET invalidated_at_ms=1 WHERE peppy_conversation_id=?",
            [conversation_id.to_string()],
        )
        .unwrap();
    connection
        .execute(
            "UPDATE rcs_mapping_proofs SET mapping_revision=7 WHERE peppy_conversation_id=?",
            [conversation_id.to_string()],
        )
        .unwrap();
    assert_eq!(
        gateway.next_rcs_mapping_revision(conversation_id).unwrap(),
        8
    );
}

const RCS_CONTEXT_KEY: &str = "rcs_mapping_context_v1";
const RCS_ACCEPTED_ROUTE_KEY: &str = "rcs_accepted_carrier_route_v1";

fn persisted_metadata(config: &ClientConfig, key: &str) -> Option<String> {
    encrypted_connection(config)
        .query_row("SELECT v FROM metadata WHERE k=?", [key], |row| row.get(0))
        .optional()
        .unwrap()
}

fn replace_metadata(config: &ClientConfig, key: &str, value: &str) {
    encrypted_connection(config)
        .execute(
            "INSERT INTO metadata(k,v) VALUES(?,?) ON CONFLICT(k) DO UPDATE SET v=excluded.v",
            [key, value],
        )
        .unwrap();
}

fn apply_tuple(
    observation: &mut RcsMappingObservation,
    tuple: RcsContextTuple,
    mapping_revision: u64,
    observed_at_ms: i64,
) {
    observation.link_generation = tuple.link_generation;
    observation.session_generation = tuple.session_generation;
    observation.context_revision = tuple.context_revision;
    observation.mapping_revision = mapping_revision;
    observation.observed_at_ms = observed_at_ms;
    observation.carrier_binding.route_context_revision = tuple.context_revision;
    observation.carrier_binding.route_observed_at_ms = observed_at_ms;
    observation.native_evidence.lifetime.link_generation = tuple.link_generation;
    observation.native_evidence.lifetime.session_generation = tuple.session_generation;
    observation.native_evidence.lifetime.context_revision = tuple.context_revision;
    observation.native_evidence.settings_observed_at_ms = observed_at_ms;
    observation.native_evidence.settings_validated_at_ms = observed_at_ms;
    observation.native_evidence.conversation_observed_at_ms = observed_at_ms;
}

fn candidate_snapshot(
    runtime_id: &str,
    run_generation: u64,
    stream_epoch: u64,
    revision: u64,
    pairing_evidence_id: &str,
    google_conversation_id: &str,
    recipient: &str,
) -> String {
    serde_json::json!({
        "code": "conversation_candidate_snapshot",
        "snapshot_token": format!("{run_generation}:{stream_epoch}:{revision}"),
        "generation": run_generation,
        "stream_epoch": stream_epoch,
        "revision": revision,
        "run_incarnation": runtime_id,
        "pairing_evidence_id": pairing_evidence_id,
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
            }],
        }],
    })
    .to_string()
}

#[test]
fn persisted_refresh_then_retry_uses_the_same_frozen_target() {
    let recipient = "+12025550123";
    let (_directory, _vault, config, gateway, command_id, mut observation) =
        rcs_command_for_peer(recipient);
    let first_tuple = gateway
        .next_rcs_context_tuple("runtime", 1, 1, "verified-handset")
        .unwrap();
    apply_tuple(&mut observation, first_tuple, 2, 1_001);
    assert!(matches!(
        gateway.verify_rcs_mapping(&observation, 1_001),
        Ok(RcsMappingOutcome::Mapped(_))
    ));
    let context: serde_json::Value = serde_json::from_str(
        &persisted_metadata(&config, RCS_CONTEXT_KEY).expect("accepted context must persist"),
    )
    .unwrap();
    assert_eq!(context["go_runtime_id"], "runtime");
    assert_eq!(context["go_run_generation"], 1);
    assert_eq!(context["go_stream_epoch"], 1);
    assert_eq!(context["go_pairing_evidence_id"], "verified-handset");

    let first = gateway
        .begin_rcs_send_attempt(command_id, &observation, 1_001)
        .unwrap();
    gateway
        .record_send_result(command_id, SendResult::FailedBeforeSubmission)
        .unwrap();
    let retry_tuple = gateway
        .next_rcs_context_tuple("runtime", 1, 1, "verified-handset")
        .unwrap();
    assert_eq!(
        retry_tuple,
        RcsContextTuple {
            link_generation: 1,
            session_generation: 1,
            context_revision: 3,
        }
    );
    apply_tuple(&mut observation, retry_tuple, 3, 1_002);
    gateway.verify_rcs_mapping(&observation, 1_002).unwrap();

    let retry = gateway
        .retry_rcs_failed_before_submission(command_id, &observation, 1_002)
        .unwrap();
    assert_eq!(retry.tmp_id(), first.tmp_id());
    assert_eq!(
        retry.google_conversation_id(),
        first.google_conversation_id()
    );
}

#[test]
fn lifecycle_changes_fence_old_snapshots_and_failed_retries() {
    struct LifecycleCase {
        name: &'static str,
        runtime_id: &'static str,
        run_generation: u64,
        stream_epoch: u64,
        pairing_id: &'static str,
        expected: RcsContextTuple,
        changes_contents: bool,
    }

    for case in [
        LifecycleCase {
            name: "runtime UUID",
            runtime_id: "runtime-next",
            run_generation: 1,
            stream_epoch: 1,
            pairing_id: "verified-handset",
            expected: RcsContextTuple {
                link_generation: 1,
                session_generation: 2,
                context_revision: 1,
            },
            changes_contents: true,
        },
        LifecycleCase {
            name: "run generation",
            runtime_id: "runtime",
            run_generation: 41,
            stream_epoch: 1,
            pairing_id: "verified-handset",
            expected: RcsContextTuple {
                link_generation: 1,
                session_generation: 2,
                context_revision: 1,
            },
            changes_contents: false,
        },
        LifecycleCase {
            name: "stream epoch",
            runtime_id: "runtime",
            run_generation: 1,
            stream_epoch: 73,
            pairing_id: "verified-handset",
            expected: RcsContextTuple {
                link_generation: 1,
                session_generation: 2,
                context_revision: 1,
            },
            changes_contents: false,
        },
        LifecycleCase {
            name: "pairing evidence",
            runtime_id: "runtime-repaired",
            run_generation: 1,
            stream_epoch: 1,
            pairing_id: "repaired-handset",
            expected: RcsContextTuple {
                link_generation: 2,
                session_generation: 1,
                context_revision: 1,
            },
            changes_contents: false,
        },
    ] {
        let recipient = "+12025550123";
        let (_directory, _vault, _config, gateway, command_id, mut successor) =
            rcs_command_for_peer(recipient);
        let conversation_id = command_conversation(&gateway);
        gateway
            .begin_rcs_send_attempt(command_id, &successor, 1_000)
            .unwrap();
        gateway
            .record_send_result(command_id, SendResult::FailedBeforeSubmission)
            .unwrap();

        let tuple = gateway
            .next_rcs_context_tuple(
                case.runtime_id,
                case.run_generation,
                case.stream_epoch,
                case.pairing_id,
            )
            .unwrap();
        assert_eq!(
            tuple, case.expected,
            "{} must advance only the core lifecycle",
            case.name
        );
        successor.native_evidence.lifetime.runtime_id = case.runtime_id.into();
        successor.native_evidence.lifetime.run_generation = case.run_generation;
        successor.native_evidence.settings_stream_epoch = case.stream_epoch;
        successor.carrier_binding.same_handset_evidence_id = case.pairing_id.into();
        if case.changes_contents {
            successor.google_conversation_id = "conversation-next".into();
        }
        apply_tuple(&mut successor, tuple, 2, 1_001);
        gateway.verify_rcs_mapping(&successor, 1_001).unwrap();

        let old_snapshot = candidate_snapshot(
            "runtime",
            1,
            1,
            1,
            "verified-handset",
            "conversation-local",
            recipient,
        );
        assert!(matches!(
            gateway.classify_and_commit_rcs_target(
                conversation_id,
                recipient,
                &old_snapshot,
                1_001
            ),
            Ok(RcsTargetClassification::Unknown { .. })
        ));
        let fresh_snapshot = candidate_snapshot(
            case.runtime_id,
            case.run_generation,
            case.stream_epoch,
            1,
            case.pairing_id,
            &successor.google_conversation_id,
            recipient,
        );
        assert!(matches!(
            gateway.classify_and_commit_rcs_target(
                conversation_id,
                recipient,
                &fresh_snapshot,
                1_001
            ),
            Ok(RcsTargetClassification::Found { .. })
        ));
        assert!(
            gateway
                .retry_rcs_failed_before_submission(command_id, &successor, 1_001)
                .is_err()
        );
    }
}

#[test]
fn public_allocators_refuse_persisted_exhaustion_without_mutating_state() {
    let (_directory, _vault, config, gateway, command_id, _observation) = rcs_command("peer-local");
    let valid_context = persisted_metadata(&config, RCS_CONTEXT_KEY)
        .expect("existing fixture must persist a context");
    encrypted_connection(&config)
        .execute(
            "DELETE FROM metadata WHERE k IN (?,?)",
            [RCS_CONTEXT_KEY, RCS_ACCEPTED_ROUTE_KEY],
        )
        .unwrap();
    assert_eq!(
        gateway
            .next_rcs_context_tuple("runtime", 1, 1, "verified-handset")
            .unwrap(),
        RcsContextTuple {
            link_generation: 1,
            session_generation: 1,
            context_revision: 1
        }
    );

    replace_metadata(&config, RCS_CONTEXT_KEY, &valid_context);
    let mut sqlite_max: serde_json::Value = serde_json::from_str(&valid_context).unwrap();
    sqlite_max["link_generation"] = serde_json::json!(i64::MAX);
    replace_metadata(&config, RCS_CONTEXT_KEY, &sqlite_max.to_string());
    let before = persisted_metadata(&config, RCS_CONTEXT_KEY);
    assert!(matches!(
        gateway.next_rcs_context_tuple("runtime", 1, 1, "new-pairing"),
        Err(Error::InvalidRequest("RCS context tuple exhausted"))
    ));
    assert_eq!(persisted_metadata(&config, RCS_CONTEXT_KEY), before);

    let mut u64_max: serde_json::Value = serde_json::from_str(&valid_context).unwrap();
    u64_max["context_revision"] = serde_json::json!(u64::MAX);
    replace_metadata(&config, RCS_CONTEXT_KEY, &u64_max.to_string());
    let before = persisted_metadata(&config, RCS_CONTEXT_KEY);
    assert!(matches!(
        gateway.next_rcs_context_tuple("runtime", 1, 1, "verified-handset"),
        Err(Error::InvalidRequest("RCS context tuple exhausted"))
    ));
    assert_eq!(persisted_metadata(&config, RCS_CONTEXT_KEY), before);

    let conversation_id = gateway
        .prepare_native_rcs_send(command_id, 1_000)
        .unwrap()
        .mapping()
        .conversation_id;
    encrypted_connection(&config)
        .execute(
            "UPDATE rcs_mapping_proofs SET mapping_revision=? WHERE peppy_conversation_id=?",
            rusqlite::params![i64::MAX, conversation_id.to_string()],
        )
        .unwrap();
    let proof_before: i64 = encrypted_connection(&config)
        .query_row(
            "SELECT mapping_revision FROM rcs_mapping_proofs WHERE peppy_conversation_id=?",
            [conversation_id.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    assert!(matches!(
        gateway.next_rcs_mapping_revision(conversation_id),
        Err(Error::InvalidRequest("RCS mapping revision exhausted"))
    ));
    assert_eq!(
        encrypted_connection(&config)
            .query_row(
                "SELECT mapping_revision FROM rcs_mapping_proofs WHERE peppy_conversation_id=?",
                [conversation_id.to_string()],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        proof_before
    );
}

#[test]
fn legacy_context_requires_a_genuine_refresh_before_classification() {
    let recipient = "+12025550123";
    let (_directory, _vault, config, gateway, _command_id, mut observation) =
        rcs_command_for_peer(recipient);
    let conversation_id = command_conversation(&gateway);
    for key in [RCS_CONTEXT_KEY, RCS_ACCEPTED_ROUTE_KEY] {
        let mut value: serde_json::Value = serde_json::from_str(
            &persisted_metadata(&config, key).expect("fixture must persist context and route"),
        )
        .unwrap();
        let context = if key == RCS_CONTEXT_KEY {
            &mut value
        } else {
            value
                .get_mut("context")
                .expect("accepted route stores its context")
        };
        context.as_object_mut().unwrap().remove("go_runtime_id");
        context.as_object_mut().unwrap().remove("go_run_generation");
        context.as_object_mut().unwrap().remove("go_stream_epoch");
        context
            .as_object_mut()
            .unwrap()
            .remove("go_pairing_evidence_id");
        replace_metadata(&config, key, &value.to_string());
    }
    let legacy_snapshot = candidate_snapshot(
        "runtime",
        1,
        1,
        1,
        "verified-handset",
        "conversation-local",
        recipient,
    );
    assert!(matches!(
        gateway.classify_and_commit_rcs_target(conversation_id, recipient, &legacy_snapshot, 1_000),
        Ok(RcsTargetClassification::Unknown { .. })
    ));

    let tuple = gateway
        .next_rcs_context_tuple("runtime-refresh", 41, 73, "verified-handset")
        .unwrap();
    observation.native_evidence.lifetime.runtime_id = "runtime-refresh".into();
    observation.native_evidence.lifetime.run_generation = 41;
    observation.native_evidence.settings_stream_epoch = 73;
    apply_tuple(&mut observation, tuple, 2, 1_001);
    gateway.verify_rcs_mapping(&observation, 1_001).unwrap();
    let fresh_snapshot = candidate_snapshot(
        "runtime-refresh",
        41,
        73,
        1,
        "verified-handset",
        "conversation-local",
        recipient,
    );
    assert!(matches!(
        gateway.classify_and_commit_rcs_target(conversation_id, recipient, &fresh_snapshot, 1_001),
        Ok(RcsTargetClassification::Found { .. })
    ));
}

#[test]
fn native_preparation_refuses_missing_historical_attempted_carrier_and_wrong_recipient_commands() {
    let (_directory, _vault, config, gateway, command_id, _observation) = rcs_command("peer-local");
    encrypted_connection(&config)
        .execute(
            "DELETE FROM command_ledger WHERE command_id=?",
            [command_id.to_string()],
        )
        .unwrap();
    assert!(matches!(
        gateway.prepare_native_rcs_send(command_id, 1_000),
        Err(Error::NotFound)
    ));
    assert_eq!(attempt_count(&config, command_id), 0);
    assert_eq!(rcs_ledger_count(&config, command_id), 0);

    let (_directory, _vault, config, gateway, command_id, _observation) = rcs_command("peer-local");
    encrypted_connection(&config)
        .execute(
            "UPDATE command_ledger SET historical=1 WHERE command_id=?",
            [command_id.to_string()],
        )
        .unwrap();
    assert!(matches!(
        gateway.prepare_native_rcs_send(command_id, 1_000),
        Err(Error::InvalidRequest("historical command"))
    ));
    assert_eq!(attempt_count(&config, command_id), 0);
    assert_eq!(rcs_ledger_count(&config, command_id), 0);

    let (_directory, _vault, config, gateway, command_id) = carrier_command();
    assert!(matches!(
        gateway.prepare_native_rcs_send(command_id, 1_000),
        Err(Error::InvalidRequest("RCS route required"))
    ));
    assert_eq!(attempt_count(&config, command_id), 0);
    assert_eq!(rcs_ledger_count(&config, command_id), 0);

    let (_directory, _vault, config, gateway, command_id, _observation) = rcs_command("other-peer");
    assert!(matches!(
        gateway.prepare_native_rcs_send(command_id, 1_000),
        Err(Error::InvalidRequest("RCS recipient is not proven"))
    ));
    assert_eq!(attempt_count(&config, command_id), 0);
    assert_eq!(rcs_ledger_count(&config, command_id), 0);

    let (_directory, _vault, _config, gateway, command_id, observation) = rcs_command("peer-local");
    gateway
        .begin_rcs_send_attempt(command_id, &observation, 1_000)
        .unwrap();
    assert!(matches!(
        gateway.prepare_native_rcs_send(command_id, 1_000),
        Err(Error::AlreadyAttempted(_))
    ));
}

#[test]
fn native_begin_refuses_replaced_or_invalidated_prepared_mappings_without_an_attempt() {
    let (_directory, _vault, config, gateway, command_id, observation) = rcs_command("peer-local");
    let preparation = gateway.prepare_native_rcs_send(command_id, 1_000).unwrap();
    encrypted_connection(&config)
        .execute(
            "UPDATE rcs_mapping_proofs SET identity_region='CA' WHERE peppy_conversation_id=?",
            [preparation.mapping().conversation_id.to_string()],
        )
        .unwrap();
    assert!(matches!(
        gateway.begin_native_rcs_send(preparation, &observation, 1_000),
        Err(Error::InvalidRequest("mapping changed"))
    ));
    assert_eq!(attempt_count(&config, command_id), 0);
    assert_eq!(rcs_ledger_count(&config, command_id), 0);

    let (_directory, _vault, config, gateway, command_id, observation) = rcs_command("peer-local");
    let preparation = gateway.prepare_native_rcs_send(command_id, 1_000).unwrap();
    encrypted_connection(&config)
        .execute(
            "UPDATE rcs_conversation_mappings SET invalidated_at_ms=1001 WHERE peppy_conversation_id=?",
            [preparation.mapping().conversation_id.to_string()],
        )
        .unwrap();
    assert!(matches!(
        gateway.begin_native_rcs_send(preparation, &observation, 1_000),
        Err(Error::InvalidRequest("mapping changed"))
    ));
    assert_eq!(attempt_count(&config, command_id), 0);
    assert_eq!(rcs_ledger_count(&config, command_id), 0);

    let (_directory, _vault, config, gateway, command_id, observation) = rcs_command("peer-local");
    let preparation = gateway.prepare_native_rcs_send(command_id, 1_000).unwrap();
    let replacement = fresh_reobservation(observation);
    assert!(matches!(
        gateway.verify_rcs_mapping(&replacement, 1_001).unwrap(),
        RcsMappingOutcome::Mapped(_)
    ));
    assert!(matches!(
        gateway.begin_native_rcs_send(preparation, &replacement, 1_001),
        Err(Error::InvalidRequest("mapping changed"))
    ));
    assert_eq!(attempt_count(&config, command_id), 0);
    assert_eq!(rcs_ledger_count(&config, command_id), 0);

    let (_directory, _vault, config, gateway, command_id, observation) = rcs_command("peer-local");
    let preparation = gateway.prepare_native_rcs_send(command_id, 1_000).unwrap();
    encrypted_connection(&config)
        .execute(
            "UPDATE rcs_conversation_mappings SET verified_at_ms=999 WHERE peppy_conversation_id=?",
            [preparation.mapping().conversation_id.to_string()],
        )
        .unwrap();
    let mut current = observation;
    current.observed_at_ms = 999;
    current.native_evidence.settings_observed_at_ms = 999;
    current.native_evidence.settings_validated_at_ms = 999;
    current.native_evidence.conversation_observed_at_ms = 999;
    current.carrier_binding.route_observed_at_ms = 999;
    assert!(matches!(
        gateway.begin_native_rcs_send(preparation, &current, 1_000),
        Err(Error::InvalidRequest("mapping changed"))
    ));
    assert_eq!(attempt_count(&config, command_id), 0);
    assert_eq!(rcs_ledger_count(&config, command_id), 0);
}

#[test]
fn native_begin_refuses_stale_partial_and_wrong_route_observations_without_an_attempt() {
    let (_directory, _vault, config, gateway, command_id, mut observation) =
        rcs_command("peer-local");
    let preparation = gateway.prepare_native_rcs_send(command_id, 1_000).unwrap();
    observation.observed_at_ms = 0;
    assert!(matches!(
        gateway.begin_native_rcs_send(preparation, &observation, 31_001),
        Err(Error::InvalidRequest("mapping unproven"))
    ));
    assert_eq!(attempt_count(&config, command_id), 0);
    assert_eq!(rcs_ledger_count(&config, command_id), 0);

    let (_directory, _vault, config, gateway, command_id, mut observation) =
        rcs_command("peer-local");
    let preparation = gateway.prepare_native_rcs_send(command_id, 1_000).unwrap();
    observation.native_evidence.lookup_complete = false;
    assert!(matches!(
        gateway.begin_native_rcs_send(preparation, &observation, 1_000),
        Err(Error::InvalidRequest("mapping unproven"))
    ));
    assert_eq!(attempt_count(&config, command_id), 0);
    assert_eq!(rcs_ledger_count(&config, command_id), 0);

    let (_directory, _vault, config, gateway, command_id, mut observation) =
        rcs_command("peer-local");
    let preparation = gateway.prepare_native_rcs_send(command_id, 1_000).unwrap();
    observation.carrier_binding.is_current_default_sms = false;
    assert!(matches!(
        gateway.begin_native_rcs_send(preparation, &observation, 1_000),
        Err(Error::InvalidRequest("mapping unproven"))
    ));
    assert_eq!(attempt_count(&config, command_id), 0);
    assert_eq!(rcs_ledger_count(&config, command_id), 0);

    let (_directory, _vault, config, gateway, command_id, mut observation) =
        rcs_command("peer-local");
    let preparation = gateway.prepare_native_rcs_send(command_id, 1_000).unwrap();
    observation.native_evidence.lifetime.run_generation = 0;
    assert!(matches!(
        gateway.begin_native_rcs_send(preparation, &observation, 1_000),
        Err(Error::InvalidRequest("mapping unproven"))
    ));
    assert_eq!(attempt_count(&config, command_id), 0);
    assert_eq!(rcs_ledger_count(&config, command_id), 0);

    let (_directory, _vault, config, gateway, command_id, mut observation) =
        rcs_command("peer-local");
    let preparation = gateway.prepare_native_rcs_send(command_id, 1_000).unwrap();
    observation.native_evidence.lifetime.link_generation += 1;
    assert!(matches!(
        gateway.begin_native_rcs_send(preparation, &observation, 1_000),
        Err(Error::InvalidRequest("mapping unproven"))
    ));
    assert_eq!(attempt_count(&config, command_id), 0);
    assert_eq!(rcs_ledger_count(&config, command_id), 0);

    let (_directory, _vault, config, gateway, command_id, mut observation) =
        rcs_command("peer-local");
    let preparation = gateway.prepare_native_rcs_send(command_id, 1_000).unwrap();
    observation.native_evidence.settings_currentness = NativeSettingsCurrentness::Unproven;
    assert!(matches!(
        gateway.begin_native_rcs_send(preparation, &observation, 1_000),
        Err(Error::InvalidRequest("mapping unproven"))
    ));
    assert_eq!(attempt_count(&config, command_id), 0);
    assert_eq!(rcs_ledger_count(&config, command_id), 0);
}

#[test]
fn native_preparations_are_one_shot_across_reopen() {
    let (_directory, vault, config, gateway, command_id, observation) = rcs_command("peer-local");
    let first = gateway.prepare_native_rcs_send(command_id, 1_000).unwrap();
    let second = gateway.prepare_native_rcs_send(command_id, 1_000).unwrap();
    gateway
        .begin_native_rcs_send(first, &observation, 1_000)
        .unwrap();
    assert!(matches!(
        gateway.begin_native_rcs_send(second, &observation, 1_000),
        Err(Error::AlreadyAttempted(_))
    ));
    drop(gateway);
    let reopened = unlocked(&config, &vault);
    let state: String = encrypted_connection(&config)
        .query_row(
            "SELECT state FROM attempts WHERE command_id=?",
            [command_id.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(state, "outcome_unknown");
    assert!(matches!(
        reopened.prepare_native_rcs_send(command_id, 1_000),
        Err(Error::AlreadyAttempted(SendState::OutcomeUnknown))
    ));
    assert!(matches!(
        reopened.begin_automatic_send_attempt(command_id, Some(&observation), 1_000),
        Ok(AutomaticPermitDecision::AlreadyAttempted(
            SendState::OutcomeUnknown
        ))
    ));
    assert_eq!(attempt_count(&config, command_id), 1);
    assert_eq!(rcs_ledger_count(&config, command_id), 1);
}

#[test]
fn native_preparation_rejects_unsupported_or_stale_stored_mappings() {
    let (_directory, _vault, config, gateway, command_id, _observation) = rcs_command("peer-local");
    encrypted_connection(&config)
        .execute(
            "UPDATE rcs_conversation_mappings SET proof_version=1 WHERE peppy_conversation_id=?",
            [gateway
                .rcs_mapping(command_conversation(&gateway))
                .unwrap()
                .unwrap()
                .conversation_id
                .to_string()],
        )
        .unwrap();
    assert!(matches!(
        gateway.prepare_native_rcs_send(command_id, 1_000),
        Err(Error::InvalidRequest("mapping unproven"))
    ));
    assert_eq!(attempt_count(&config, command_id), 0);
    assert_eq!(rcs_ledger_count(&config, command_id), 0);

    let (_directory, _vault, config, gateway, command_id, _observation) = rcs_command("peer-local");
    encrypted_connection(&config)
        .execute(
            "UPDATE rcs_conversation_mappings SET verified_at_ms=0 WHERE peppy_conversation_id=?",
            [gateway
                .rcs_mapping(command_conversation(&gateway))
                .unwrap()
                .unwrap()
                .conversation_id
                .to_string()],
        )
        .unwrap();
    assert!(matches!(
        gateway.prepare_native_rcs_send(command_id, 31_001),
        Err(Error::InvalidRequest("mapping unproven"))
    ));
    assert_eq!(attempt_count(&config, command_id), 0);
    assert_eq!(rcs_ledger_count(&config, command_id), 0);
}

#[test]
fn concurrent_native_begins_create_one_attempt() {
    let (_directory, _vault, config, gateway, command_id, observation) = rcs_command("peer-local");
    let first = gateway.prepare_native_rcs_send(command_id, 1_000).unwrap();
    let second = gateway.prepare_native_rcs_send(command_id, 1_000).unwrap();
    let other = gateway.clone();
    let other_observation = observation.clone();
    let timeout = std::time::Duration::from_secs(30);
    let (start_first, wait_first) = std::sync::mpsc::channel();
    let (start_second, wait_second) = std::sync::mpsc::channel();
    let (results, completed) = std::sync::mpsc::channel();
    let (ready, waiting) = std::sync::mpsc::channel();
    let other_ready = ready.clone();
    let other_results = results.clone();
    let first_thread = std::thread::spawn(move || {
        ready.send(()).unwrap();
        wait_first.recv_timeout(timeout).unwrap();
        results
            .send(gateway.begin_native_rcs_send(first, &observation, 1_000))
            .unwrap();
    });
    let second_thread = std::thread::spawn(move || {
        other_ready.send(()).unwrap();
        wait_second.recv_timeout(timeout).unwrap();
        other_results
            .send(other.begin_native_rcs_send(second, &other_observation, 1_000))
            .unwrap();
    });
    // Release both workers before waiting/asserting so neither latch survives a failure.
    let first_ready = waiting.recv_timeout(timeout);
    let second_ready = waiting.recv_timeout(timeout);
    let first_released = start_first.send(());
    let second_released = start_second.send(());
    first_released.unwrap();
    second_released.unwrap();
    first_ready.unwrap();
    second_ready.unwrap();
    let first_result = completed.recv_timeout(timeout).unwrap();
    let second_result = completed.recv_timeout(timeout).unwrap();
    first_thread.join().unwrap();
    second_thread.join().unwrap();
    assert_eq!(
        usize::from(first_result.is_ok()) + usize::from(second_result.is_ok()),
        1
    );
    assert!(
        matches!(first_result, Err(Error::AlreadyAttempted(_)))
            || matches!(second_result, Err(Error::AlreadyAttempted(_)))
    );
    assert_eq!(attempt_count(&config, command_id), 1);
    assert_eq!(rcs_ledger_count(&config, command_id), 1);
}

#[test]
fn native_preparation_refuses_nontext_command_without_an_attempt() {
    let (_directory, _vault, config, gateway, command_id, _observation) = rcs_command("peer-local");
    let connection = encrypted_connection(&config);
    let payload: String = connection
        .query_row(
            "SELECT payload FROM command_ledger WHERE command_id=?",
            [command_id.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    let mut message: MessagePayload = serde_json::from_str(&payload).unwrap();
    message.transport = Transport::Mms;
    connection
        .execute(
            "UPDATE command_ledger SET payload=? WHERE command_id=?",
            rusqlite::params![
                serde_json::to_string(&message).unwrap(),
                command_id.to_string()
            ],
        )
        .unwrap();
    assert!(matches!(
        gateway.prepare_native_rcs_send(command_id, 1_000),
        Err(Error::InvalidRequest("RCS text required"))
    ));
    assert_eq!(attempt_count(&config, command_id), 0);
    assert_eq!(rcs_ledger_count(&config, command_id), 0);
}

#[test]
fn native_pre_effect_refusal_requires_the_exact_unacknowledged_rcs_attempt() {
    let (_directory, _vault, config, gateway, command_id, observation) = rcs_command("peer-local");
    let permit = gateway
        .begin_rcs_send_attempt(command_id, &observation, 1_000)
        .unwrap();
    let tmp_id = permit.tmp_id().to_owned();
    let google_conversation_id = permit.google_conversation_id().to_owned();
    let outbox_before = gateway.pending_outbox().unwrap().len();

    for (wrong_tmp_id, wrong_google_conversation_id) in [
        ("wrong-tmp", google_conversation_id.as_str()),
        (tmp_id.as_str(), "wrong-google-conversation"),
    ] {
        assert!(
            gateway
                .record_native_rcs_pre_effect_refusal(
                    command_id,
                    wrong_tmp_id,
                    wrong_google_conversation_id,
                )
                .is_err()
        );
        let state: String = encrypted_connection(&config)
            .query_row(
                "SELECT state FROM attempts WHERE command_id=?",
                [command_id.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(state, "attempt_recorded");
        assert_eq!(gateway.pending_outbox().unwrap().len(), outbox_before);
    }

    assert_eq!(
        gateway
            .record_native_rcs_pre_effect_refusal(command_id, &tmp_id, &google_conversation_id)
            .unwrap(),
        SendState::FailedBeforeSubmission
    );
    let outbox_after_failure = gateway.pending_outbox().unwrap().len();
    assert_eq!(outbox_after_failure, outbox_before + 1);
    assert_eq!(
        gateway
            .record_native_rcs_pre_effect_refusal(command_id, &tmp_id, &google_conversation_id)
            .unwrap(),
        SendState::FailedBeforeSubmission
    );
    assert_eq!(
        gateway.pending_outbox().unwrap().len(),
        outbox_after_failure
    );

    let (_directory, _vault, _config, acknowledged, command_id, observation) =
        rcs_command("peer-local");
    let permit = acknowledged
        .begin_rcs_send_attempt(command_id, &observation, 1_000)
        .unwrap();
    acknowledged
        .record_rcs_transport_ack(command_id, 1_001, "accepted")
        .unwrap();
    assert!(
        acknowledged
            .record_native_rcs_pre_effect_refusal(
                command_id,
                permit.tmp_id(),
                permit.google_conversation_id()
            )
            .is_err()
    );

    for result in [SendResult::Submitted, SendResult::Sent] {
        let (_directory, _vault, _config, gateway, command_id, observation) =
            rcs_command("peer-local");
        let permit = gateway
            .begin_rcs_send_attempt(command_id, &observation, 1_000)
            .unwrap();
        gateway.record_send_result(command_id, result).unwrap();
        assert!(
            gateway
                .record_native_rcs_pre_effect_refusal(
                    command_id,
                    permit.tmp_id(),
                    permit.google_conversation_id()
                )
                .is_err()
        );
    }

    let (_directory, _vault, config, gateway, command_id, observation) = rcs_command("peer-local");
    let permit = gateway
        .begin_rcs_send_attempt(command_id, &observation, 1_000)
        .unwrap();
    encrypted_connection(&config)
        .execute(
            "UPDATE attempts SET state='outcome_unknown' WHERE command_id=?",
            [command_id.to_string()],
        )
        .unwrap();
    assert!(
        gateway
            .record_native_rcs_pre_effect_refusal(
                command_id,
                permit.tmp_id(),
                permit.google_conversation_id(),
            )
            .is_err()
    );

    let (_directory, _vault, _config, gateway, command_id) = carrier_command();
    assert!(
        gateway
            .record_native_rcs_pre_effect_refusal(command_id, "tmp", "google")
            .is_err()
    );
}

#[test]
fn native_outcome_unknown_requires_the_exact_attempt_and_is_idempotent() {
    let (_directory, _vault, _config, gateway, command_id, observation) = rcs_command("peer-local");
    let permit = gateway
        .begin_rcs_send_attempt(command_id, &observation, 1_000)
        .unwrap();
    let tmp_id = permit.tmp_id().to_owned();
    let google_conversation_id = permit.google_conversation_id().to_owned();
    let outbox_before = gateway.pending_outbox().unwrap().len();

    assert!(
        gateway
            .record_native_rcs_outcome_unknown(command_id, "wrong-tmp", &google_conversation_id)
            .is_err()
    );
    assert_eq!(
        gateway
            .record_native_rcs_outcome_unknown(command_id, &tmp_id, &google_conversation_id)
            .unwrap(),
        SendState::OutcomeUnknown
    );
    let outbox_after_unknown = gateway.pending_outbox().unwrap().len();
    assert_eq!(outbox_after_unknown, outbox_before + 1);
    assert_eq!(
        gateway
            .record_native_rcs_outcome_unknown(command_id, &tmp_id, &google_conversation_id)
            .unwrap(),
        SendState::OutcomeUnknown
    );
    assert_eq!(
        gateway.pending_outbox().unwrap().len(),
        outbox_after_unknown
    );

    let (_directory, _vault, _config, gateway, command_id, observation) = rcs_command("peer-local");
    let permit = gateway
        .begin_rcs_send_attempt(command_id, &observation, 1_000)
        .unwrap();
    let tmp_id = permit.tmp_id().to_owned();
    let google_conversation_id = permit.google_conversation_id().to_owned();
    gateway.record_rcs_submitted(command_id).unwrap();
    assert_eq!(
        gateway
            .record_native_rcs_outcome_unknown(command_id, &tmp_id, &google_conversation_id)
            .unwrap(),
        SendState::SubmittedToOs
    );
}

#[test]
fn native_pre_effect_refusal_rolls_back_when_status_outbox_insert_aborts() {
    let (_directory, _vault, config, gateway, command_id, observation) = rcs_command("peer-local");
    let permit = gateway
        .begin_rcs_send_attempt(command_id, &observation, 1_000)
        .unwrap();
    let connection = encrypted_connection(&config);
    let state = || {
        (
            connection
                .query_row(
                    "SELECT state FROM attempts WHERE command_id=?",
                    [command_id.to_string()],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            connection
                .query_row(
                    "SELECT state FROM send_status WHERE command_id=?",
                    [command_id.to_string()],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            connection
                .query_row("SELECT COUNT(*) FROM outbox", [], |row| {
                    row.get::<_, i64>(0)
                })
                .unwrap(),
            connection
                .query_row(
                    "SELECT v FROM metadata WHERE k='producer_sequence'",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
        )
    };
    let before = state();
    connection
        .execute_batch(
            "CREATE TRIGGER reject_native_refusal_outbox BEFORE INSERT ON outbox BEGIN SELECT RAISE(ABORT, 'reject'); END;",
        )
        .unwrap();
    assert!(matches!(
        gateway.record_native_rcs_pre_effect_refusal(
            command_id,
            permit.tmp_id(),
            permit.google_conversation_id(),
        ),
        Err(Error::Database)
    ));
    assert_eq!(state(), before);
    connection
        .execute_batch("DROP TRIGGER reject_native_refusal_outbox")
        .unwrap();
    assert_eq!(
        gateway
            .record_native_rcs_pre_effect_refusal(
                command_id,
                permit.tmp_id(),
                permit.google_conversation_id(),
            )
            .unwrap(),
        SendState::FailedBeforeSubmission
    );
    let after = state();
    assert_eq!(after.0, "failed_before_submission");
    assert_eq!(after.1, "failed_before_submission");
    assert_eq!(after.2, before.2 + 1);
    assert_eq!(
        after.3.parse::<u64>().unwrap(),
        before.3.parse::<u64>().unwrap() + 1
    );
}

// Compare complete rows (including ciphertext and producer metadata), not only counts.
fn refusal_persisted_rows(config: &ClientConfig) -> Vec<Vec<Vec<rusqlite::types::Value>>> {
    let connection = encrypted_connection(config);
    [
        "attempts",
        "rcs_send_ledger",
        "send_status",
        "outbox",
        "metadata",
    ]
    .into_iter()
    .map(|table| {
        let mut query = connection
            .prepare(&format!("SELECT * FROM {table} ORDER BY rowid"))
            .unwrap();
        let columns = query.column_count();
        query
            .query_map([], |row| {
                (0..columns).map(|column| row.get(column)).collect()
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    })
    .collect()
}

fn assert_refusal_unchanged(
    gateway: &Client,
    config: &ClientConfig,
    command_id: CommandId,
    tmp_id: &str,
    google_id: &str,
) {
    let before = refusal_persisted_rows(config);
    assert!(
        gateway
            .record_native_rcs_pre_effect_refusal(command_id, tmp_id, google_id)
            .is_err()
    );
    assert_eq!(refusal_persisted_rows(config), before);
}

#[test]
fn native_refusal_rechecks_identity_and_ack_even_for_an_already_failed_attempt() {
    let (directory, vault, config, gateway, command_id, observation) = rcs_command("peer-local");
    let first = gateway
        .begin_rcs_send_attempt(command_id, &observation, 1_000)
        .unwrap();

    // Receive and admit a second real command in the same gateway database.
    let sender_config = ClientConfig {
        database_path: directory.path().join("second-sender.db"),
        vault_id: config.vault_id,
        device_id: DeviceId::new(),
    };
    let sender = unlocked(&sender_config, &vault);
    let second = sender
        .queue_send(
            OutgoingSms {
                conversation_id: first.conversation_id(),
                recipients: vec!["peer-local".into()],
                body: "second command".into(),
            },
            GatewayRoute::RcsLink {
                gateway_device_id: config.device_id,
            },
        )
        .unwrap();
    let mut cursor = gateway.receive_cursor().unwrap().0;
    for envelope in sender.pending_outbox().unwrap() {
        cursor += 1;
        gateway.ingest(&envelope, Cursor(cursor)).unwrap();
    }
    gateway.apply_pending(100).unwrap();
    let second = gateway
        .begin_rcs_send_attempt(second.command_id, &observation, 1_000)
        .unwrap();
    assert_ne!(first.tmp_id(), second.tmp_id());
    assert_refusal_unchanged(
        &gateway,
        &config,
        first.command_id(),
        second.tmp_id(),
        second.google_conversation_id(),
    );
    assert_refusal_unchanged(
        &gateway,
        &config,
        CommandId::new(),
        first.tmp_id(),
        first.google_conversation_id(),
    );
    assert_refusal_unchanged(
        &gateway,
        &config,
        command_id,
        "wrong-tmp",
        first.google_conversation_id(),
    );
    assert_refusal_unchanged(
        &gateway,
        &config,
        command_id,
        first.tmp_id(),
        "wrong-google",
    );
    gateway
        .record_native_rcs_pre_effect_refusal(
            command_id,
            first.tmp_id(),
            first.google_conversation_id(),
        )
        .unwrap();
    assert_refusal_unchanged(
        &gateway,
        &config,
        command_id,
        "wrong-tmp",
        first.google_conversation_id(),
    );
    assert_refusal_unchanged(
        &gateway,
        &config,
        command_id,
        first.tmp_id(),
        "wrong-google",
    );
    let before = refusal_persisted_rows(&config);
    assert_eq!(
        gateway
            .record_native_rcs_pre_effect_refusal(
                command_id,
                first.tmp_id(),
                first.google_conversation_id()
            )
            .unwrap(),
        SendState::FailedBeforeSubmission
    );
    assert_eq!(refusal_persisted_rows(&config), before);
    gateway
        .record_rcs_transport_ack(command_id, 1_001, "accepted")
        .unwrap();
    assert_refusal_unchanged(
        &gateway,
        &config,
        command_id,
        first.tmp_id(),
        first.google_conversation_id(),
    );
}

#[test]
fn native_refusal_cannot_rewrite_acknowledged_submitted_sent_delivered_or_reopened_attempts() {
    for result in [
        SendResult::Submitted,
        SendResult::Sent,
        SendResult::Delivered,
    ] {
        let (_directory, _vault, config, gateway, command_id, observation) =
            rcs_command("peer-local");
        let permit = gateway
            .begin_rcs_send_attempt(command_id, &observation, 1_000)
            .unwrap();
        gateway.record_send_result(command_id, result).unwrap();
        assert_refusal_unchanged(
            &gateway,
            &config,
            command_id,
            permit.tmp_id(),
            permit.google_conversation_id(),
        );
    }
    let (_directory, vault, config, gateway, command_id, observation) = rcs_command("peer-local");
    let permit = gateway
        .begin_rcs_send_attempt(command_id, &observation, 1_000)
        .unwrap();
    gateway
        .record_rcs_transport_ack(command_id, 1_001, "accepted")
        .unwrap();
    assert_refusal_unchanged(
        &gateway,
        &config,
        command_id,
        permit.tmp_id(),
        permit.google_conversation_id(),
    );
    drop(gateway);
    let reopened = unlocked(&config, &vault);
    assert!(matches!(
        reopened
            .begin_automatic_send_attempt(command_id, Some(&observation), 1_000)
            .unwrap(),
        AutomaticPermitDecision::AlreadyAttempted(SendState::OutcomeUnknown)
    ));
    assert_refusal_unchanged(
        &reopened,
        &config,
        command_id,
        permit.tmp_id(),
        permit.google_conversation_id(),
    );

    // Reopen without an ack too: otherwise the ack check alone masks the state fence.
    let (_directory, vault, config, gateway, command_id, observation) = rcs_command("peer-local");
    let permit = gateway
        .begin_rcs_send_attempt(command_id, &observation, 1_000)
        .unwrap();
    drop(gateway);
    let reopened = unlocked(&config, &vault);
    assert!(matches!(
        reopened
            .begin_automatic_send_attempt(command_id, Some(&observation), 1_000)
            .unwrap(),
        AutomaticPermitDecision::AlreadyAttempted(SendState::OutcomeUnknown)
    ));
    assert_refusal_unchanged(
        &reopened,
        &config,
        command_id,
        permit.tmp_id(),
        permit.google_conversation_id(),
    );
}

#[test]
fn native_refusal_rejects_real_carrier_attempt_and_retained_ledger_corruption() {
    let (_directory, _vault, config, gateway, command_id) = carrier_command();
    assert!(matches!(
        gateway.begin_send_attempt_at(command_id, 1_000).unwrap(),
        PermitDecision::Permit(_)
    ));
    assert_eq!(attempt_count(&config, command_id), 1);
    assert_refusal_unchanged(&gateway, &config, command_id, "tmp", "google");

    // Deliberate corruption fixture: retain an exact RCS ledger while changing only
    // finalized_transport. A normal carrier attempt has no RCS ledger and cannot
    // independently discriminate the finalized_transport predicate.
    let (_directory, _vault, config, gateway, command_id, observation) = rcs_command("peer-local");
    let permit = gateway
        .begin_rcs_send_attempt(command_id, &observation, 1_000)
        .unwrap();
    encrypted_connection(&config)
        .execute(
            "UPDATE attempts SET finalized_transport='carrier' WHERE command_id=?",
            [command_id.to_string()],
        )
        .unwrap();
    assert_refusal_unchanged(
        &gateway,
        &config,
        command_id,
        permit.tmp_id(),
        permit.google_conversation_id(),
    );
}
