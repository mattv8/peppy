use super::*;

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
