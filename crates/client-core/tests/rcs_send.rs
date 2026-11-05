use super::common::*;

fn observation(participant: &str, revision: u64) -> RcsMappingObservation {
    RcsMappingObservation {
        google_conversation_id: "conversation-local".into(),
        google_candidate_count: 1,
        participants: vec![RcsParticipantObservation {
            identity: participant.into(),
            is_visible: true,
            is_me: false,
        }],
        conversation_type: RcsConversationType::Rcs,
        status: "eligible".into(),
        send_mode: RcsSendMode::Automatic,
        read_only: false,
        default_outgoing: true,
        selected_sim_id: "sim-local".into(),
        selected_sim_own_identity: "own-local".into(),
        identity_region: "ZZ".into(),
        observed_at_ms: 1_000,
        context_revision: 1,
        link_generation: 1,
        session_generation: 1,
        canonicalizer_revision: 1,
        mapping_revision: revision,
        link_state: RcsLinkState::Linked,
    }
}

fn carrier_command() -> (tempfile::TempDir, Vault, ClientConfig, Client, CommandId) {
    let directory = tempfile::tempdir().unwrap();
    let vault = Vault::new();
    let sender_config = config(&directory, "sender", &vault);
    let gateway_config = config(&directory, "gateway", &vault);
    let sender = unlocked(&sender_config, &vault);
    let gateway = unlocked(&gateway_config, &vault);
    let captured = gateway
        .capture_incoming(IncomingSms {
            conversation_id: None,
            sender_address: "peer-local".into(),
            body: String::new(),
            provider_message_id: Some("provider-local".into()),
            imported: true,
        })
        .unwrap();
    let command = sender
        .queue_send(
            OutgoingSms {
                conversation_id: captured.conversation_id,
                recipients: vec!["peer-local".into()],
                body: String::from('\0'),
            },
            GatewayRoute::Carrier {
                gateway_device_id: gateway_config.device_id,
                subscription_id: "sim-local".into(),
            },
        )
        .unwrap();
    let mut server = Server::default();
    server.upload(&sender);
    server.sync(&gateway);
    (
        directory,
        vault,
        gateway_config,
        gateway,
        command.command_id,
    )
}

fn carrier_proof(command: &ReceivedCommand) -> CarrierOnlyProof {
    CarrierOnlyProof {
        conversation_id: command.message.record.conversation_id,
        recipients: command.message.recipients.clone(),
        subscription_id: command.subscription_id.clone(),
        observed_at_ms: 1_000,
        link_generation: 1,
        session_generation: 1,
        context_revision: 1,
    }
}

#[test]
fn unlinked_carrier_remains_allowed() {
    let (_directory, _vault, _config, gateway, command_id) = carrier_command();
    assert!(matches!(
        gateway.begin_send_attempt_at(command_id, 1_000).unwrap(),
        PermitDecision::Permit(_)
    ));
}

#[test]
fn linked_unknown_carrier_is_blocked_without_an_attempt() {
    let (_directory, _vault, _config, gateway, command_id) = carrier_command();
    let mut unknown = observation("peer-local", 1);
    unknown.google_candidate_count = 0;
    assert!(matches!(
        gateway.verify_rcs_mapping(&unknown, 1_000).unwrap(),
        RcsMappingOutcome::Refused(_)
    ));
    assert_eq!(
        gateway.begin_send_attempt_at(command_id, 1_000).unwrap(),
        PermitDecision::Blocked(PermitBlock::CarrierOnlyProofUnavailable)
    );
    assert_eq!(
        gateway.begin_send_attempt_at(command_id, 1_000).unwrap(),
        PermitDecision::Blocked(PermitBlock::CarrierOnlyProofUnavailable)
    );
}

#[test]
fn fresh_current_carrier_only_proof_permits_once_and_survives_reopen() {
    let (_directory, vault, config, gateway, command_id) = carrier_command();
    let mut unknown = observation("peer-local", 1);
    unknown.google_candidate_count = 0;
    gateway.verify_rcs_mapping(&unknown, 1_000).unwrap();
    let command = gateway.pending_commands().unwrap().pop().unwrap();
    gateway
        .record_carrier_only_proof(&carrier_proof(&command), 1_000)
        .unwrap();
    drop(gateway);
    let reopened = unlocked(&config, &vault);
    assert!(matches!(
        reopened.begin_send_attempt_at(command_id, 1_000).unwrap(),
        PermitDecision::Permit(_)
    ));
    assert_eq!(
        reopened.begin_send_attempt_at(command_id, 1_000).unwrap(),
        PermitDecision::AlreadyAttempted(SendState::AttemptRecorded)
    );
}

#[test]
fn stale_wrong_sim_wrong_context_and_proven_rcs_never_fall_back_to_carrier() {
    let (_directory, _vault, _config, gateway, command_id) = carrier_command();
    let mut unknown = observation("peer-local", 1);
    unknown.google_candidate_count = 0;
    gateway.verify_rcs_mapping(&unknown, 1_000).unwrap();
    let command = gateway.pending_commands().unwrap().pop().unwrap();
    let mut stale = carrier_proof(&command);
    stale.observed_at_ms = 1;
    assert!(gateway.record_carrier_only_proof(&stale, 31_001).is_err());
    let mut wrong_sim = carrier_proof(&command);
    wrong_sim.subscription_id = "other-sim".into();
    gateway
        .record_carrier_only_proof(&wrong_sim, 1_000)
        .unwrap();
    assert_eq!(
        gateway.begin_send_attempt_at(command_id, 1_000).unwrap(),
        PermitDecision::Blocked(PermitBlock::CarrierOnlyProofUnavailable)
    );
    let current = carrier_proof(&command);
    gateway.record_carrier_only_proof(&current, 1_000).unwrap();
    let mut changed = unknown;
    changed.context_revision = 2;
    changed.observed_at_ms = 1_001;
    gateway.verify_rcs_mapping(&changed, 1_001).unwrap();
    assert_eq!(
        gateway.begin_send_attempt_at(command_id, 1_001).unwrap(),
        PermitDecision::Blocked(PermitBlock::CarrierOnlyProofUnavailable)
    );
    let mut mapped = observation("peer-local", 2);
    mapped.context_revision = 3;
    mapped.observed_at_ms = 1_002;
    assert!(matches!(
        gateway.verify_rcs_mapping(&mapped, 1_002).unwrap(),
        RcsMappingOutcome::Mapped(_)
    ));
    assert_eq!(
        gateway.begin_send_attempt_at(command_id, 1_002).unwrap(),
        PermitDecision::Blocked(PermitBlock::CarrierOnlyProofUnavailable)
    );
}

#[test]
fn rcs_link_command_gets_one_durable_exact_mapping_permit() {
    let directory = tempfile::tempdir().unwrap();
    let vault = Vault::new();
    let sender_config = config(&directory, "sender", &vault);
    let gateway_config = config(&directory, "gateway", &vault);
    let sender = unlocked(&sender_config, &vault);
    let gateway = unlocked(&gateway_config, &vault);
    let captured = gateway
        .capture_incoming(IncomingSms {
            conversation_id: None,
            sender_address: "peer-local".into(),
            body: String::new(),
            provider_message_id: Some("provider-local".into()),
            imported: true,
        })
        .unwrap();
    let current_observation = observation("peer-local", 1);
    let RcsMappingOutcome::Mapped(mapping) = gateway
        .verify_rcs_mapping(&current_observation, 1_000)
        .unwrap()
    else {
        panic!("expected mapping");
    };

    let queued = sender
        .queue_send(
            OutgoingSms {
                conversation_id: captured.conversation_id,
                recipients: vec!["peer-local".into()],
                body: String::from('\0'),
            },
            GatewayRoute::RcsLink {
                gateway_device_id: gateway_config.device_id,
            },
        )
        .unwrap();
    let mut server = Server::default();
    server.upload(&sender);
    server.sync(&gateway);
    let command_id = queued.command_id;
    assert!(gateway.pending_commands().unwrap().is_empty());

    let permit = gateway
        .begin_rcs_send_attempt(command_id, &current_observation, 1_000)
        .unwrap();
    assert_eq!(permit.conversation_id, captured.conversation_id);
    assert_eq!(
        permit.google_conversation_id,
        mapping.google_conversation_id
    );
    assert_eq!(permit.mapping_revision, mapping.mapping_revision);
    assert_eq!(permit.proof_digest, mapping.proof.digest);
    assert!(uuid::Uuid::parse_str(&permit.tmp_id).is_ok());
    assert_eq!(
        gateway
            .begin_rcs_send_attempt(command_id, &current_observation, 1_000)
            .unwrap_err(),
        Error::AlreadyAttempted(SendState::AttemptRecorded)
    );
    assert_eq!(
        gateway.begin_send_attempt(command_id).unwrap(),
        PermitDecision::Blocked(PermitBlock::RouteUnavailable)
    );
    drop(gateway);
    let reopened = unlocked(&gateway_config, &vault);
    assert_eq!(
        reopened
            .begin_rcs_send_attempt(command_id, &current_observation, 1_000)
            .unwrap_err(),
        Error::AlreadyAttempted(SendState::OutcomeUnknown)
    );
    reopened
        .record_rcs_transport_ack(command_id, 1_001, "transport_ack")
        .unwrap();
    assert_eq!(
        reopened
            .reconcile_rcs_tmp_id(&permit.tmp_id, &permit.google_conversation_id, None)
            .unwrap(),
        Some(SendState::OutcomeUnknown)
    );
}
