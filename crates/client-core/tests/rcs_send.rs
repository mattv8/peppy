use super::common::*;
use std::fs;
use std::sync::{Arc, Barrier};

fn observation(participant: &str, revision: u64) -> RcsMappingObservation {
    RcsMappingObservation {
        google_conversation_id: "conversation-local".into(),
        google_candidate_count: 1,
        participants: vec![RcsParticipantObservation {
            identity: participant.into(),
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
        mapping_revision: revision,
        link_state: RcsLinkState::Linked,
    }
}

fn capture_us_region(gateway: &Client, gateway_config: &ClientConfig) {
    gateway
        .capture_contact_book(
            &serde_json::json!({
                "schema_version": 1,
                "book": {
                    "id": "carrier-region",
                    "owner_device_id": gateway_config.device_id.to_string(),
                    "generation": "1",
                    "state": "active",
                    "region": "US",
                },
                "contacts": [],
            })
            .to_string(),
        )
        .unwrap();
}

fn carrier_command() -> (tempfile::TempDir, Vault, ClientConfig, Client, CommandId) {
    carrier_command_on_sim("android-subscription-7")
}

fn carrier_command_on_sim(
    subscription_id: &str,
) -> (tempfile::TempDir, Vault, ClientConfig, Client, CommandId) {
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
                subscription_id: subscription_id.into(),
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

/// A received text-only MMS command on the carrier route, as a composer queues a text reply in a
/// thread whose latest message is MMS.
fn carrier_text_mms_command(
    recipients: &[&str],
) -> (tempfile::TempDir, Vault, ClientConfig, Client, CommandId) {
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
        .queue_mms(
            OutgoingMms {
                conversation_id: captured.conversation_id,
                recipients: recipients.iter().map(|peer| (*peer).into()).collect(),
                body: "text reply".into(),
                attachment_ids: Vec::new(),
                subject: None,
            },
            GatewayRoute::Carrier {
                gateway_device_id: gateway_config.device_id,
                subscription_id: "android-subscription-7".into(),
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
        canonical_recipient: "peer-local".into(),
        identity_region: "US".into(),
        canonicalizer_revision: 1,
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
fn first_use_unlinked_lifecycle_keeps_carrier_permitted_after_reopen() {
    let (_directory, vault, config, gateway, command_id) = carrier_command();
    gateway
        .record_rcs_link_lifecycle(
            RcsCapabilityState::Unlinked,
            false,
            false,
            false,
            1_000,
            1_000,
        )
        .unwrap();
    drop(gateway);

    let reopened = unlocked(&config, &vault);
    assert!(matches!(
        reopened.begin_send_attempt_at(command_id, 1_000).unwrap(),
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
fn recorded_linked_lifecycle_blocks_carrier_without_proof() {
    let (_directory, _vault, _config, gateway, command_id) = carrier_command();
    gateway
        .record_rcs_link_lifecycle(RcsCapabilityState::Linked, true, true, true, 1_000, 1_000)
        .unwrap();
    assert_eq!(
        gateway.begin_send_attempt_at(command_id, 1_000).unwrap(),
        PermitDecision::Blocked(PermitBlock::CarrierOnlyProofUnavailable)
    );
}

#[test]
fn fresh_current_carrier_only_proof_permits_once_and_survives_reopen() {
    let (_directory, vault, config, gateway, command_id) = carrier_command();
    capture_us_region(&gateway, &config);
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
    let (_directory, _vault, config, gateway, command_id) = carrier_command();
    capture_us_region(&gateway, &config);
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
    mapped.carrier_binding.route_context_revision = 3;
    mapped.native_evidence.lifetime.context_revision = 3;
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
    let pending = gateway.pending_automatic_commands().unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].command_id, command_id);
    assert_eq!(pending[0].original_subscription_id, None);

    let permit = gateway
        .begin_rcs_send_attempt(command_id, &current_observation, 1_000)
        .unwrap();
    assert_eq!(permit.conversation_id(), captured.conversation_id);
    assert_eq!(
        permit.google_conversation_id(),
        mapping.google_conversation_id
    );
    assert_eq!(permit.mapping_revision(), mapping.mapping_revision);
    assert_eq!(permit.proof_digest(), mapping.proof.digest);
    assert_eq!(permit.target().canonical_recipient, "raw:peer-local");
    assert_eq!(permit.target().selected_sim_id, "android-subscription-7");
    assert_eq!(permit.target().outgoing.outgoing_id, "google-outgoing");
    assert_eq!(permit.target().outgoing.sim_payload.two, 2);
    assert_eq!(permit.target().outgoing.sim_payload.sim_number, 7);
    assert_eq!(permit.target().carrier_binding.android_subscription_id, 7);
    assert_ne!(
        permit.target().selected_sim_id,
        permit.target().outgoing.outgoing_id
    );
    assert_eq!(
        permit.target().participants[0].google_participant_id,
        "google-peer"
    );
    assert_eq!(
        permit.target().peppy_conversation_id,
        captured.conversation_id.to_string()
    );
    assert_eq!(permit.target().conversation_type, RcsConversationType::Rcs);
    assert_eq!(permit.target().status, "eligible");
    assert_eq!(permit.target().send_mode, RcsSendMode::Automatic);
    assert!(!permit.target().read_only);
    let projection = permit.native_commit_projection();
    assert_eq!(projection.proof_version, permit.target().proof_version);
    let serialized = serde_json::to_value(projection).unwrap();
    assert_eq!(
        serialized["target"]["peppy_conversation_id"],
        captured.conversation_id.to_string()
    );
    assert_eq!(serialized["target"]["conversation_type"], "Rcs");
    assert_eq!(serialized["target"]["status"], "eligible");
    assert_eq!(serialized["target"]["send_mode"], "Automatic");
    assert_eq!(serialized["target"]["read_only"], false);
    assert!(uuid::Uuid::parse_str(permit.tmp_id()).is_ok());
    assert_eq!(
        gateway
            .begin_rcs_send_attempt(command_id, &current_observation, 1_000)
            .unwrap_err(),
        Error::AlreadyAttempted(SendState::AttemptRecorded)
    );
    assert_eq!(
        gateway.begin_send_attempt(command_id).unwrap(),
        PermitDecision::AlreadyAttempted(SendState::AttemptRecorded)
    );
    assert_eq!(attempt_count(&gateway_config, command_id), 1);
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
            .reconcile_rcs_tmp_id(permit.tmp_id(), permit.google_conversation_id(), None)
            .unwrap(),
        Some(SendState::OutcomeUnknown)
    );
}

#[test]
fn automatic_admission_upgrades_a_legacy_carrier_command_only_with_an_exact_mapping() {
    let (_directory, _vault, _config, gateway, command_id) = carrier_command();
    let current = observation("peer-local", 1);
    assert!(matches!(
        gateway.verify_rcs_mapping(&current, 1_000).unwrap(),
        RcsMappingOutcome::Mapped(_)
    ));

    let AutomaticPermitDecision::Rcs(permit) = gateway
        .begin_automatic_send_attempt(command_id, Some(&current), 1_000)
        .unwrap()
    else {
        panic!("expected an RCS permit");
    };
    assert!(uuid::Uuid::parse_str(permit.tmp_id()).is_ok());
    assert_eq!(
        gateway
            .begin_automatic_send_attempt(command_id, Some(&current), 1_001)
            .unwrap(),
        AutomaticPermitDecision::AlreadyAttempted(SendState::AttemptRecorded)
    );
}

#[test]
fn automatic_admission_invalidates_carrier_proof_for_same_context_contradictions() {
    let (_directory, vault, config, gateway, command_id) = carrier_command();
    capture_us_region(&gateway, &config);
    let mut unknown = observation("peer-local", 1);
    unknown.google_candidate_count = 0;
    gateway.verify_rcs_mapping(&unknown, 1_000).unwrap();
    let command = gateway.pending_commands().unwrap().pop().unwrap();
    gateway
        .record_carrier_only_proof(&carrier_proof(&command), 1_000)
        .unwrap();
    let contradictory = observation("different-peer-local", 1);

    assert_eq!(
        gateway
            .begin_automatic_send_attempt(command_id, Some(&contradictory), 1_000)
            .unwrap(),
        AutomaticPermitDecision::Blocked(PermitBlock::MappingUnproven)
    );
    assert_eq!(
        gateway
            .begin_automatic_send_attempt(command_id, None, 1_000)
            .unwrap(),
        AutomaticPermitDecision::Blocked(PermitBlock::CarrierOnlyProofUnavailable)
    );
    drop(gateway);
    assert_eq!(
        unlocked(&config, &vault)
            .begin_automatic_send_attempt(command_id, None, 1_000)
            .unwrap(),
        AutomaticPermitDecision::Blocked(PermitBlock::CarrierOnlyProofUnavailable)
    );
}

#[test]
fn automatic_admission_rejects_a_valid_mapping_on_a_different_command_sim() {
    let (_directory, _vault, config, gateway, command_id) =
        carrier_command_on_sim("android-subscription-8");
    let current = observation("peer-local", 1);
    assert!(matches!(
        gateway.verify_rcs_mapping(&current, 1_000).unwrap(),
        RcsMappingOutcome::Mapped(_)
    ));
    assert_eq!(
        gateway
            .begin_automatic_send_attempt(command_id, Some(&current), 1_000)
            .unwrap(),
        AutomaticPermitDecision::Blocked(PermitBlock::MappingUnproven)
    );
    assert_eq!(attempt_count(&config, command_id), 0);
    assert_eq!(rcs_ledger_count(&config, command_id), 0);

    let (_directory, _vault, config, gateway, command_id) = carrier_command();
    let current = observation("peer-local", 1);
    assert!(matches!(
        gateway.verify_rcs_mapping(&current, 1_000).unwrap(),
        RcsMappingOutcome::Mapped(_)
    ));
    assert!(matches!(
        gateway
            .begin_automatic_send_attempt(command_id, Some(&current), 1_000)
            .unwrap(),
        AutomaticPermitDecision::Rcs(_)
    ));
    assert_eq!(attempt_count(&config, command_id), 1);
    assert_eq!(rcs_ledger_count(&config, command_id), 1);
}

fn encrypted_connection(config: &ClientConfig) -> rusqlite::Connection {
    let connection = rusqlite::Connection::open(&config.database_path).unwrap();
    connection
        .execute_batch(
            "PRAGMA key = \"x'0707070707070707070707070707070707070707070707070707070707070707'\";",
        )
        .unwrap();
    connection
}

fn attachment_response(
    operation: &PendingAttachmentUseOperation,
    state: AttachmentUseState,
) -> AttachmentUseResponse {
    AttachmentUseResponse {
        use_id: operation.use_id,
        attachment_id: operation.attachment_id,
        device_id: operation.device_id,
        kind: operation.kind,
        state,
    }
}

fn carrier_mms_command(
    retention_enabled: bool,
) -> (
    tempfile::TempDir,
    Vault,
    ClientConfig,
    Client,
    Client,
    CommandId,
    AttachmentInfo,
    ConversationId,
) {
    let directory = tempfile::tempdir().unwrap();
    let vault = Vault::new();
    let sender_config = config(&directory, "sender-media", &vault);
    let gateway_config = config(&directory, "gateway-media", &vault);
    let sender = unlocked(&sender_config, &vault);
    let gateway = unlocked(&gateway_config, &vault);
    gateway
        .configure_attachment_retention_protocol(retention_enabled)
        .unwrap();
    let captured = gateway
        .capture_incoming(IncomingSms {
            conversation_id: None,
            sender_address: "peer-local".into(),
            body: String::new(),
            provider_message_id: Some("provider-media-local".into()),
            imported: true,
        })
        .unwrap();
    let source = directory.path().join("automatic-media.txt");
    fs::write(&source, b"automatic media").unwrap();
    let attachment = sender
        .prepare_attachment(&source, "text/plain", "automatic-media.txt")
        .unwrap();
    let sender_use = sender
        .begin_attachment_retention_use(attachment.attachment_id, AttachmentUseKind::PendingSend)
        .unwrap();
    let queued = sender
        .queue_mms(
            OutgoingMms {
                conversation_id: captured.conversation_id,
                recipients: vec!["peer-local".into()],
                body: "media".into(),
                attachment_ids: vec![attachment.attachment_id],
                subject: None,
            },
            GatewayRoute::Carrier {
                gateway_device_id: gateway_config.device_id,
                subscription_id: "sim-local".into(),
            },
        )
        .unwrap();
    sender
        .mark_attachment_uploaded(attachment.attachment_id, &uuid::Uuid::new_v4().to_string())
        .unwrap();
    sender
        .apply_attachment_retention_response(&attachment_response(
            &sender_use,
            AttachmentUseState::Registered,
        ))
        .unwrap();
    let mut server = Server::default();
    server.upload(&sender);
    server.sync(&gateway);
    (
        directory,
        vault,
        gateway_config,
        sender,
        gateway,
        queued.command_id,
        attachment,
        captured.conversation_id,
    )
}

fn record_media_carrier_proof(
    gateway: &Client,
    gateway_config: &ClientConfig,
    conversation_id: ConversationId,
) {
    capture_us_region(gateway, gateway_config);
    let mut unknown = observation("peer-local", 1);
    unknown.google_candidate_count = 0;
    gateway.verify_rcs_mapping(&unknown, 1_000).unwrap();
    gateway
        .record_carrier_only_proof(
            &CarrierOnlyProof {
                conversation_id,
                recipients: vec!["peer-local".into()],
                canonical_recipient: "peer-local".into(),
                identity_region: "US".into(),
                canonicalizer_revision: 1,
                subscription_id: "sim-local".into(),
                observed_at_ms: 1_000,
                link_generation: 1,
                session_generation: 1,
                context_revision: 1,
            },
            1_000,
        )
        .unwrap();
}

fn attempt_count(config: &ClientConfig, command_id: CommandId) -> i64 {
    encrypted_connection(config)
        .query_row(
            "SELECT COUNT(*) FROM attempts WHERE command_id=?",
            [command_id.to_string()],
            |row| row.get(0),
        )
        .unwrap()
}

fn rcs_ledger_count(config: &ClientConfig, command_id: CommandId) -> i64 {
    encrypted_connection(config)
        .query_row(
            "SELECT COUNT(*) FROM rcs_send_ledger WHERE command_id=?",
            [command_id.to_string()],
            |row| row.get(0),
        )
        .unwrap()
}

fn rcs_command(
    recipient: &str,
) -> (
    tempfile::TempDir,
    Vault,
    ClientConfig,
    Client,
    CommandId,
    RcsMappingObservation,
) {
    rcs_command_with_body(recipient, String::from('\0'))
}

fn rcs_command_with_body(
    recipient: &str,
    body: String,
) -> (
    tempfile::TempDir,
    Vault,
    ClientConfig,
    Client,
    CommandId,
    RcsMappingObservation,
) {
    rcs_command_for_peer_with_body("peer-local", recipient, body)
}

fn rcs_command_for_peer(
    recipient: &str,
) -> (
    tempfile::TempDir,
    Vault,
    ClientConfig,
    Client,
    CommandId,
    RcsMappingObservation,
) {
    rcs_command_for_peer_with_body(recipient, recipient, String::from('\0'))
}

fn rcs_command_for_peer_with_body(
    peer: &str,
    recipient: &str,
    body: String,
) -> (
    tempfile::TempDir,
    Vault,
    ClientConfig,
    Client,
    CommandId,
    RcsMappingObservation,
) {
    let directory = tempfile::tempdir().unwrap();
    let vault = Vault::new();
    let sender_config = config(&directory, "sender-rcs", &vault);
    let gateway_config = config(&directory, "gateway-rcs", &vault);
    let sender = unlocked(&sender_config, &vault);
    let gateway = unlocked(&gateway_config, &vault);
    let captured = gateway
        .capture_incoming(IncomingSms {
            conversation_id: None,
            sender_address: peer.into(),
            body: String::new(),
            provider_message_id: Some("provider-rcs-local".into()),
            imported: true,
        })
        .unwrap();
    let current_observation = observation(peer, 1);
    assert!(matches!(
        gateway
            .verify_rcs_mapping(&current_observation, 1_000)
            .unwrap(),
        RcsMappingOutcome::Mapped(_)
    ));
    let queued = sender
        .queue_send(
            OutgoingSms {
                conversation_id: captured.conversation_id,
                recipients: vec![recipient.into()],
                body,
            },
            GatewayRoute::RcsLink {
                gateway_device_id: gateway_config.device_id,
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
        queued.command_id,
        current_observation,
    )
}

#[path = "rcs_native_handoff.rs"]
mod native_handoff;

#[test]
fn terminal_lifecycle_states_refuse_every_rcs_permit_entry_point() {
    for state in [
        RcsCapabilityState::Unlinked,
        RcsCapabilityState::SignedOut,
        RcsCapabilityState::NeedsRelink,
    ] {
        let (_directory, _vault, _config, gateway, command_id, observation) =
            rcs_command("peer-local");
        let preparation = gateway.prepare_native_rcs_send(command_id, 1_000).unwrap();
        gateway
            .record_rcs_link_lifecycle(state, false, false, false, 1_001, 1_001)
            .unwrap();

        assert!(
            gateway
                .begin_rcs_send_attempt(command_id, &observation, 1_001)
                .is_err()
        );
        assert!(gateway.prepare_native_rcs_send(command_id, 1_001).is_err());
        assert!(
            gateway
                .begin_native_rcs_send(preparation, &observation, 1_001)
                .is_err()
        );
        assert_eq!(
            gateway
                .begin_automatic_send_attempt(command_id, Some(&observation), 1_001)
                .unwrap(),
            AutomaticPermitDecision::Blocked(PermitBlock::MappingUnproven)
        );

        let (_directory, _vault, _config, gateway, command_id, observation) =
            rcs_command("peer-local");
        let permit = gateway
            .begin_rcs_send_attempt(command_id, &observation, 1_000)
            .unwrap();
        gateway
            .record_native_rcs_pre_effect_refusal(
                command_id,
                permit.tmp_id(),
                permit.google_conversation_id(),
                false,
                1_000,
            )
            .unwrap();
        gateway
            .record_rcs_link_lifecycle(state, false, false, false, 1_001, 1_001)
            .unwrap();
        assert!(
            gateway
                .retry_rcs_failed_before_submission(command_id, &observation, 1_001)
                .is_err()
        );
    }
}

#[test]
fn current_lookup_withdrawal_revokes_strict_authority_across_reopen() {
    let (_directory, vault, config, gateway, command_id, current) = rcs_command("peer-local");
    let conversation_id = command_conversation(&gateway);
    let mut withdrawn = current.clone();
    withdrawn.native_evidence.lookup_complete = false;
    withdrawn.native_evidence.google_candidate_count = 2;

    assert!(
        gateway
            .begin_rcs_send_attempt(command_id, &withdrawn, 1_000)
            .is_err()
    );
    assert_eq!(attempt_count(&config, command_id), 0);
    drop(gateway);
    let reopened = unlocked(&config, &vault);
    assert!(reopened.rcs_mapping(conversation_id).unwrap().is_none());
    assert!(
        reopened
            .begin_rcs_send_attempt(command_id, &current, 1_000)
            .is_err()
    );
    assert_eq!(attempt_count(&config, command_id), 0);

    let fresh = fresh_reobservation(current);
    assert!(matches!(
        reopened.verify_rcs_mapping(&fresh, 1_001).unwrap(),
        RcsMappingOutcome::Mapped(_)
    ));
}

fn fresh_reobservation(mut observation: RcsMappingObservation) -> RcsMappingObservation {
    observation.mapping_revision += 1;
    observation.observed_at_ms = 1_001;
    observation.native_evidence.settings_validated_at_ms = 1_001;
    observation.native_evidence.conversation_observed_at_ms = 1_001;
    observation.carrier_binding.route_observed_at_ms = 1_001;
    observation
}

fn command_conversation(gateway: &Client) -> ConversationId {
    gateway.pending_automatic_commands().unwrap()[0]
        .message
        .record
        .conversation_id
}

#[test]
fn strict_admission_refuses_same_revision_payload_contradictions() {
    let (_directory, vault, config, gateway, command_id, mut current) = rcs_command("peer-local");
    let conversation_id = command_conversation(&gateway);
    current.outgoing.sim_payload.two += 1;

    assert!(
        gateway
            .begin_rcs_send_attempt(command_id, &current, 1_000)
            .is_err()
    );
    assert!(gateway.rcs_mapping(conversation_id).unwrap().is_none());
    assert_eq!(attempt_count(&config, command_id), 0);
    drop(gateway);
    let reopened = unlocked(&config, &vault);
    assert!(reopened.rcs_mapping(conversation_id).unwrap().is_none());
    assert_eq!(attempt_count(&config, command_id), 0);
}

#[test]
fn contradictory_admission_revokes_mapping_durably_without_creating_permits() {
    let (_directory, vault, config, gateway, command_id, mut current) = rcs_command("peer-local");
    let conversation_id = command_conversation(&gateway);
    current.outgoing.sim_payload.two += 1;
    assert!(
        gateway
            .begin_rcs_send_attempt(command_id, &current, 1_000)
            .is_err()
    );
    assert_eq!(attempt_count(&config, command_id), 0);
    drop(gateway);
    let reopened = unlocked(&config, &vault);
    assert!(reopened.rcs_mapping(conversation_id).unwrap().is_none());
    assert_eq!(attempt_count(&config, command_id), 0);

    let (_directory, vault, config, gateway, command_id, mut current) = rcs_command("peer-local");
    let conversation_id = command_conversation(&gateway);
    current.outgoing.sim_payload.two += 1;
    assert_eq!(
        gateway
            .begin_automatic_send_attempt(command_id, Some(&current), 1_000)
            .unwrap(),
        AutomaticPermitDecision::Blocked(PermitBlock::MappingUnproven)
    );
    assert_eq!(attempt_count(&config, command_id), 0);
    drop(gateway);
    let reopened = unlocked(&config, &vault);
    assert!(reopened.rcs_mapping(conversation_id).unwrap().is_none());
    assert_eq!(attempt_count(&config, command_id), 0);

    let (_directory, vault, config, gateway, command_id, current) = rcs_command("peer-local");
    let conversation_id = command_conversation(&gateway);
    let initial = gateway
        .begin_rcs_send_attempt(command_id, &current, 1_000)
        .unwrap();
    gateway
        .record_send_result(command_id, SendResult::FailedBeforeSubmission)
        .unwrap();
    let mut contradictory = current;
    contradictory.outgoing.sim_payload.two += 1;
    assert!(
        gateway
            .retry_rcs_failed_before_submission(command_id, &contradictory, 1_001)
            .is_err()
    );
    assert_eq!(attempt_count(&config, command_id), 1);
    drop(gateway);
    let reopened = unlocked(&config, &vault);
    assert!(reopened.rcs_mapping(conversation_id).unwrap().is_none());
    let persisted_tmp_id: String = encrypted_connection(&config)
        .query_row(
            "SELECT tmp_id FROM rcs_send_ledger WHERE command_id=?",
            [command_id.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(persisted_tmp_id, initial.tmp_id());
    assert_eq!(attempt_count(&config, command_id), 1);
}

#[test]
fn strict_admission_refuses_mismatched_or_duplicate_google_sim_joins() {
    for mutation in 0..5 {
        let (_directory, _vault, _config, gateway, command_id, mut current) =
            rcs_command("peer-local");
        if mutation == 0 {
            current.outgoing.outgoing_id = "conversation-outgoing".into();
            current.native_evidence.current_settings_sims[0].outgoing_id =
                "conversation-outgoing".into();
        } else {
            let mut duplicate = current.native_evidence.current_settings_sims[0].clone();
            match mutation {
                1 => duplicate.sim_payload.two += 1,
                2 => duplicate.own_identity_raw = "+12025550101".into(),
                3 => duplicate.rcs_chats_enabled = false,
                4 => {
                    duplicate.sim_payload.sim_number += 1;
                    duplicate.own_identity_raw = "+12025550101".into();
                    duplicate.rcs_chats_enabled = false;
                }
                _ => unreachable!(),
            }
            current
                .native_evidence
                .current_settings_sims
                .push(duplicate);
        }

        assert!(
            gateway
                .begin_rcs_send_attempt(command_id, &current, 1_000)
                .is_err()
        );
    }
}

#[test]
fn duplicate_sim_join_withdrawal_revokes_automatic_authority_across_reopen() {
    let (_directory, vault, config, gateway, command_id, current) = rcs_command("peer-local");
    let conversation_id = command_conversation(&gateway);
    let mut withdrawn = current.clone();
    let mut duplicate = withdrawn.native_evidence.current_settings_sims[0].clone();
    duplicate.sim_payload.sim_number += 1;
    duplicate.own_identity_raw = "+12025550101".into();
    duplicate.rcs_chats_enabled = false;
    withdrawn
        .native_evidence
        .current_settings_sims
        .push(duplicate);

    assert_eq!(
        gateway
            .begin_automatic_send_attempt(command_id, Some(&withdrawn), 1_000)
            .unwrap(),
        AutomaticPermitDecision::Blocked(PermitBlock::MappingUnproven)
    );
    assert_eq!(attempt_count(&config, command_id), 0);
    assert_eq!(rcs_ledger_count(&config, command_id), 0);
    drop(gateway);
    let reopened = unlocked(&config, &vault);
    assert!(reopened.rcs_mapping(conversation_id).unwrap().is_none());
    assert_eq!(
        reopened
            .begin_automatic_send_attempt(command_id, Some(&current), 1_000)
            .unwrap(),
        AutomaticPermitDecision::Blocked(PermitBlock::MappingUnproven)
    );
    assert_eq!(attempt_count(&config, command_id), 0);
}

#[test]
fn duplicate_sim_join_withdrawal_preserves_failed_retry_state_until_reverified() {
    let (_directory, vault, config, gateway, command_id, current) = rcs_command("peer-local");
    let conversation_id = command_conversation(&gateway);
    let first = gateway
        .begin_rcs_send_attempt(command_id, &current, 1_000)
        .unwrap();
    let tmp_id = first.tmp_id().to_owned();
    gateway
        .record_send_result(command_id, SendResult::FailedBeforeSubmission)
        .unwrap();
    let mut withdrawn = current.clone();
    let mut duplicate = withdrawn.native_evidence.current_settings_sims[0].clone();
    duplicate.sim_payload.two += 1;
    withdrawn
        .native_evidence
        .current_settings_sims
        .push(duplicate);

    assert!(
        gateway
            .retry_rcs_failed_before_submission(command_id, &withdrawn, 1_001)
            .is_err()
    );
    assert_eq!(attempt_count(&config, command_id), 1);
    drop(gateway);
    let reopened = unlocked(&config, &vault);
    assert!(reopened.rcs_mapping(conversation_id).unwrap().is_none());
    assert!(
        reopened
            .retry_rcs_failed_before_submission(command_id, &current, 1_001)
            .is_err()
    );
    assert_eq!(attempt_count(&config, command_id), 1);
    assert_eq!(
        encrypted_connection(&config)
            .query_row(
                "SELECT tmp_id FROM rcs_send_ledger WHERE command_id=?",
                [command_id.to_string()],
                |row| row.get::<_, String>(0),
            )
            .unwrap(),
        tmp_id
    );

    let fresh = fresh_reobservation(current);
    assert!(matches!(
        reopened.verify_rcs_mapping(&fresh, 1_001).unwrap(),
        RcsMappingOutcome::Mapped(_)
    ));
    let retry = reopened
        .retry_rcs_failed_before_submission(command_id, &fresh, 1_001)
        .unwrap();
    assert_eq!(retry.tmp_id(), tmp_id);
}

#[test]
fn actual_google_and_os_target_mutations_all_refuse_strict_admission() {
    for mutation in 0..6 {
        let (_directory, _vault, _config, gateway, command_id, mut current) =
            rcs_command("peer-local");
        match mutation {
            0 => current.native_evidence.current_settings_sims[0].outgoing_id = "changed".into(),
            1 => {
                current.native_evidence.current_settings_sims[0].sim_participant_id =
                    "changed".into()
            }
            2 => current.participants[0].identifier_type += 1,
            3 => current.participants[0].is_visible = false,
            4 => current.participants[0].is_me = true,
            5 => current.carrier_binding.own_number_raw = "+12025550101".into(),
            _ => unreachable!(),
        }
        assert!(
            gateway
                .begin_rcs_send_attempt(command_id, &current, 1_000)
                .is_err()
        );
    }
}

#[test]
fn legacy_retry_identity_never_silently_upgrades() {
    let (_directory, vault, config, gateway, command_id, current) = rcs_command("peer-local");
    gateway
        .begin_rcs_send_attempt(command_id, &current, 1_000)
        .unwrap();
    gateway
        .record_send_result(command_id, SendResult::FailedBeforeSubmission)
        .unwrap();
    drop(gateway);
    let connection = encrypted_connection(&config);
    let key: String = connection
        .query_row(
            "SELECT k FROM metadata WHERE k LIKE 'rcs_target_identity_v1/%'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    connection
        .execute(
            "UPDATE metadata SET v=? WHERE k=?",
            [
                r#"{"version":1,"google_conversation_id":"conversation-local"}"#,
                &key,
            ],
        )
        .unwrap();
    drop(connection);
    let gateway = unlocked(&config, &vault);

    assert!(
        gateway
            .retry_rcs_failed_before_submission(command_id, &current, 1_001)
            .is_err()
    );
}

#[test]
fn rcs_retry_is_refused_after_restore_guard_is_recorded() {
    let (_directory, _vault, _config, gateway, command_id, current) = rcs_command("peer-local");
    gateway
        .begin_rcs_send_attempt(command_id, &current, 1_000)
        .unwrap();
    gateway
        .record_send_result(command_id, SendResult::FailedBeforeSubmission)
        .unwrap();
    gateway.mark_restored_gateway().unwrap();

    assert_eq!(
        gateway
            .retry_rcs_failed_before_submission(command_id, &current, 1_001)
            .unwrap_err(),
        Error::InvalidRequest("restore guarded")
    );
}

#[test]
fn rcs_retry_is_refused_after_a_newer_epoch_is_activated() {
    let (_directory, vault, _config, gateway, command_id, current) = rcs_command("peer-local");
    gateway
        .begin_rcs_send_attempt(command_id, &current, 1_000)
        .unwrap();
    gateway
        .record_send_result(command_id, SendResult::FailedBeforeSubmission)
        .unwrap();
    let epoch_two = Vault::epoch(vault.id, 2);
    gateway
        .unlock(&epoch_two.profile, &epoch_two.header, PASSPHRASE)
        .unwrap();
    gateway.activate_epoch(2).unwrap();

    assert!(
        gateway
            .retry_rcs_failed_before_submission(command_id, &current, 1_001)
            .is_err()
    );
}

#[test]
fn rcs_retry_is_refused_when_the_received_command_is_historical() {
    let (_directory, vault, config, gateway, command_id, current) = rcs_command("peer-local");
    gateway
        .begin_rcs_send_attempt(command_id, &current, 1_000)
        .unwrap();
    gateway
        .record_send_result(command_id, SendResult::FailedBeforeSubmission)
        .unwrap();
    drop(gateway);
    let connection = encrypted_connection(&config);
    connection
        .execute(
            "UPDATE command_ledger SET historical=1 WHERE command_id=?",
            [command_id.to_string()],
        )
        .unwrap();
    drop(connection);
    let gateway = unlocked(&config, &vault);

    assert!(
        gateway
            .retry_rcs_failed_before_submission(command_id, &current, 1_001)
            .is_err()
    );
}

#[test]
fn rcs_initial_permit_binds_the_command_recipient_to_the_mapping_participant() {
    let (_directory, _vault, _config, gateway, command_id, current) =
        rcs_command("different-peer-local");

    assert!(
        gateway
            .begin_rcs_send_attempt(command_id, &current, 1_000)
            .is_err()
    );
}

#[test]
fn rcs_retry_rechecks_the_command_recipient_against_the_mapping_participant() {
    let (_directory, vault, config, gateway, command_id, current) = rcs_command("peer-local");
    gateway
        .begin_rcs_send_attempt(command_id, &current, 1_000)
        .unwrap();
    gateway
        .record_send_result(command_id, SendResult::FailedBeforeSubmission)
        .unwrap();
    drop(gateway);
    let connection = encrypted_connection(&config);
    let payload: String = connection
        .query_row(
            "SELECT payload FROM command_ledger WHERE command_id=?",
            [command_id.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    let mut payload: serde_json::Value = serde_json::from_str(&payload).unwrap();
    payload["recipients"] = serde_json::json!(["different-peer-local"]);
    connection
        .execute(
            "UPDATE command_ledger SET payload=? WHERE command_id=?",
            [payload.to_string(), command_id.to_string()],
        )
        .unwrap();
    drop(connection);
    let gateway = unlocked(&config, &vault);

    assert!(
        gateway
            .retry_rcs_failed_before_submission(command_id, &current, 1_001)
            .is_err()
    );
}

#[test]
fn rcs_retry_accepts_a_fresh_same_target_proof_after_the_old_proof_expires() {
    let (_directory, _vault, _config, gateway, command_id, current) = rcs_command("peer-local");
    let first = gateway
        .begin_rcs_send_attempt(command_id, &current, 1_000)
        .unwrap();
    gateway
        .record_send_result(command_id, SendResult::FailedBeforeSubmission)
        .unwrap();
    let mut refreshed = current;
    refreshed.mapping_revision = 2;
    refreshed.context_revision = 2;
    refreshed.observed_at_ms = 31_001;
    refreshed.carrier_binding.route_context_revision = 2;
    refreshed.carrier_binding.route_observed_at_ms = 31_001;
    refreshed.native_evidence.lifetime.context_revision = 2;
    refreshed.native_evidence.settings_validated_at_ms = 31_001;
    refreshed.native_evidence.conversation_observed_at_ms = 31_001;
    gateway.verify_rcs_mapping(&refreshed, 31_001).unwrap();

    let retry = gateway
        .retry_rcs_failed_before_submission(command_id, &refreshed, 31_001)
        .unwrap();
    assert_eq!(retry.command_id(), first.command_id());
    assert_eq!(retry.tmp_id(), first.tmp_id());
    assert_eq!(retry.mapping_revision(), 2);
}

#[test]
fn rcs_retry_refuses_changed_sim_or_link_session() {
    for changed_session in [false, true] {
        let (_directory, _vault, _config, gateway, command_id, current) = rcs_command("peer-local");
        gateway
            .begin_rcs_send_attempt(command_id, &current, 1_000)
            .unwrap();
        gateway
            .record_send_result(command_id, SendResult::FailedBeforeSubmission)
            .unwrap();
        let mut changed = current;
        changed.mapping_revision = 2;
        changed.context_revision = 2;
        changed.observed_at_ms = 1_001;
        changed.carrier_binding.route_context_revision = 2;
        changed.native_evidence.lifetime.context_revision = 2;
        changed.native_evidence.settings_validated_at_ms = 1_001;
        changed.native_evidence.conversation_observed_at_ms = 1_001;
        if changed_session {
            changed.session_generation = 2;
            changed.native_evidence.lifetime.session_generation = 2;
        } else {
            changed.selected_sim_id = "android-subscription-8".into();
            changed.carrier_binding.carrier_route_id = "android-subscription-8".into();
            changed.carrier_binding.android_subscription_id = 8;
        }
        assert!(matches!(
            gateway.verify_rcs_mapping(&changed, 1_001),
            Ok(RcsMappingOutcome::Mapped(_))
        ));
        assert!(
            gateway
                .retry_rcs_failed_before_submission(command_id, &changed, 1_001)
                .is_err()
        );
    }
}

#[test]
fn explicit_unknown_resend_becomes_received_after_server_echo() {
    let (_directory, vault, config, gateway, command_id, current) = rcs_command("peer-local");
    let original = gateway
        .begin_rcs_send_attempt(command_id, &current, 1_000)
        .unwrap();
    drop(gateway);
    let gateway = unlocked(&config, &vault);
    let queued = gateway.resend_rcs_outcome_unknown(command_id).unwrap();
    assert_ne!(queued.command_id, command_id);
    assert_eq!(
        gateway
            .reconcile_rcs_tmp_id(original.tmp_id(), original.google_conversation_id(), None)
            .unwrap(),
        Some(SendState::OutcomeUnknown)
    );
    let connection = encrypted_connection(&config);
    let (tmp_id, retry_of): (String, String) = connection
        .query_row(
            "SELECT tmp_id,retry_of_command_id FROM rcs_send_ledger WHERE command_id=?",
            [queued.command_id.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_ne!(tmp_id, original.tmp_id());
    assert_eq!(retry_of, command_id.to_string());

    let mut server = Server::default();
    server.upload(&gateway);
    let echo = server
        .log
        .iter()
        .find(|envelope| envelope.command_id == Some(queued.command_id))
        .unwrap();
    let cursor = Cursor(gateway.receive_cursor().unwrap().0 + 1);
    assert_eq!(
        gateway.ingest(echo, cursor).unwrap(),
        IngestResult::Journaled
    );
    assert_eq!(gateway.apply_pending(100).unwrap().applied, 1);
    assert_eq!(
        gateway
            .reconcile_rcs_tmp_id(original.tmp_id(), original.google_conversation_id(), None)
            .unwrap(),
        Some(SendState::OutcomeUnknown)
    );
    gateway
        .begin_rcs_send_attempt(queued.command_id, &current, 1_000)
        .unwrap();
    assert_eq!(
        gateway.ingest(echo, cursor).unwrap(),
        IngestResult::Duplicate
    );
    assert_eq!(gateway.apply_pending(100).unwrap().applied, 0);
    assert_eq!(
        gateway
            .begin_rcs_send_attempt(queued.command_id, &current, 1_000)
            .unwrap_err(),
        Error::AlreadyAttempted(SendState::AttemptRecorded)
    );
}

#[test]
fn carrier_proof_rejects_future_extreme_overflow_and_expired_times() {
    let (_directory, _vault, _config, gateway, _command_id) = carrier_command();
    let mut unknown = observation("peer-local", 1);
    unknown.google_candidate_count = 0;
    gateway.verify_rcs_mapping(&unknown, 1_000).unwrap();
    let command = gateway.pending_commands().unwrap().pop().unwrap();
    let mut proof = carrier_proof(&command);

    proof.observed_at_ms = 1_001;
    assert!(gateway.record_carrier_only_proof(&proof, 1_000).is_err());
    proof.observed_at_ms = i64::MIN;
    assert!(gateway.record_carrier_only_proof(&proof, 0).is_err());
    proof.observed_at_ms = i64::MAX;
    assert!(gateway.record_carrier_only_proof(&proof, i64::MAX).is_err());
    proof.observed_at_ms = 1_000;
    assert!(gateway.record_carrier_only_proof(&proof, 31_001).is_err());
}

#[test]
fn clock_rollback_invalidates_carrier_proof_until_a_new_lookup_is_recorded() {
    let (_directory, vault, config, gateway, command_id) = carrier_command();
    capture_us_region(&gateway, &config);
    let mut unknown = observation("peer-local", 1);
    unknown.google_candidate_count = 0;
    gateway.verify_rcs_mapping(&unknown, 1_000).unwrap();
    let command = gateway.pending_commands().unwrap().pop().unwrap();
    let mut proof = carrier_proof(&command);
    gateway.record_carrier_only_proof(&proof, 1_000).unwrap();
    assert_eq!(
        gateway.begin_send_attempt_at(command_id, 999).unwrap(),
        PermitDecision::Blocked(PermitBlock::CarrierOnlyProofUnavailable)
    );
    assert_eq!(
        gateway.begin_send_attempt_at(command_id, 1_001).unwrap(),
        PermitDecision::Blocked(PermitBlock::CarrierOnlyProofUnavailable)
    );

    drop(gateway);
    let gateway = unlocked(&config, &vault);
    assert_eq!(
        gateway.begin_send_attempt_at(command_id, 1_001).unwrap(),
        PermitDecision::Blocked(PermitBlock::CarrierOnlyProofUnavailable)
    );

    proof.observed_at_ms = 999;
    gateway.record_carrier_only_proof(&proof, 999).unwrap();
    assert!(matches!(
        gateway.begin_send_attempt_at(command_id, 999).unwrap(),
        PermitDecision::Permit(_)
    ));
}

#[test]
fn explicit_unlink_restores_carrier_even_after_invalidating_a_historical_rcs_mapping() {
    let (_directory, vault, config, gateway, command_id) = carrier_command();
    let mut linked = observation("peer-local", 1);
    linked.google_candidate_count = 0;
    gateway.verify_rcs_mapping(&linked, 1_000).unwrap();
    assert_eq!(
        gateway.begin_send_attempt_at(command_id, 1_000).unwrap(),
        PermitDecision::Blocked(PermitBlock::CarrierOnlyProofUnavailable)
    );
    let mut unlinked = linked;
    unlinked.context_revision = 2;
    unlinked.mapping_revision = 2;
    unlinked.observed_at_ms = 1_001;
    unlinked.link_state = RcsLinkState::Unlinked;
    gateway
        .record_rcs_link_lifecycle(
            RcsCapabilityState::Unlinked,
            false,
            false,
            false,
            1_001,
            1_001,
        )
        .unwrap();
    gateway.update_rcs_context(&unlinked, 1_001).unwrap();
    drop(gateway);
    let gateway = unlocked(&config, &vault);
    assert!(matches!(
        gateway.begin_send_attempt_at(command_id, 1_001).unwrap(),
        PermitDecision::Permit(_)
    ));

    let (_directory, vault, config, gateway, command_id) = carrier_command();
    let conversation_id = gateway
        .pending_commands()
        .unwrap()
        .pop()
        .unwrap()
        .message
        .record
        .conversation_id;
    gateway
        .record_rcs_link_lifecycle(RcsCapabilityState::Linked, true, true, true, 1_000, 1_000)
        .unwrap();
    let mapped = observation("peer-local", 1);
    gateway.verify_rcs_mapping(&mapped, 1_000).unwrap();
    gateway
        .record_rcs_link_lifecycle(
            RcsCapabilityState::Unlinked,
            false,
            false,
            false,
            1_001,
            1_001,
        )
        .unwrap();
    assert!(gateway.rcs_mapping(conversation_id).unwrap().is_none());
    let mut unlinked = mapped;
    unlinked.context_revision = 2;
    unlinked.mapping_revision = 2;
    unlinked.observed_at_ms = 1_001;
    unlinked.link_state = RcsLinkState::Unlinked;
    gateway.update_rcs_context(&unlinked, 1_001).unwrap();
    drop(gateway);
    let gateway = unlocked(&config, &vault);
    assert!(matches!(
        gateway.begin_send_attempt_at(command_id, 1_001).unwrap(),
        PermitDecision::Permit(_)
    ));
}

#[test]
fn durable_explicit_unlink_completion_restores_carrier_idempotently_without_rcs_authority() {
    let (_directory, vault, config, gateway, command_id) = carrier_command();
    let conversation_id = gateway
        .pending_commands()
        .unwrap()
        .pop()
        .unwrap()
        .message
        .record
        .conversation_id;
    gateway
        .record_rcs_link_lifecycle(RcsCapabilityState::Linked, true, true, true, 1_000, 1_000)
        .unwrap();
    let mapped = observation("peer-local", 1);
    gateway.verify_rcs_mapping(&mapped, 1_000).unwrap();
    gateway
        .record_rcs_link_lifecycle(
            RcsCapabilityState::Unlinked,
            false,
            false,
            false,
            1_001,
            1_001,
        )
        .unwrap();
    gateway
        .record_rcs_explicit_unlink_completed(1_001, 1_001)
        .unwrap();
    gateway
        .record_rcs_explicit_unlink_completed(1_001, 1_001)
        .unwrap();
    assert!(gateway.rcs_mapping(conversation_id).unwrap().is_none());
    drop(gateway);
    let reopened = unlocked(&config, &vault);
    assert!(matches!(
        reopened.begin_send_attempt_at(command_id, 1_001).unwrap(),
        PermitDecision::Permit(_)
    ));
}

#[test]
fn lifecycle_only_explicit_unlink_completion_restores_both_carrier_entry_points_after_reopen() {
    for automatic in [false, true] {
        let (_directory, vault, config, gateway, command_id) = carrier_command();
        gateway
            .record_rcs_link_lifecycle(RcsCapabilityState::Linked, true, true, true, 1_000, 1_000)
            .unwrap();
        gateway
            .record_rcs_link_lifecycle(
                RcsCapabilityState::Unlinked,
                false,
                false,
                false,
                1_001,
                1_001,
            )
            .unwrap();
        gateway
            .record_rcs_explicit_unlink_completed(1_001, 1_001)
            .unwrap();
        drop(gateway);

        let reopened = unlocked(&config, &vault);
        if automatic {
            assert!(matches!(
                reopened
                    .begin_automatic_send_attempt(command_id, None, 1_001)
                    .unwrap(),
                AutomaticPermitDecision::Carrier(_)
            ));
        } else {
            assert!(matches!(
                reopened.begin_send_attempt_at(command_id, 1_001).unwrap(),
                PermitDecision::Permit(_)
            ));
        }
    }
}

#[test]
fn media_ledger_permanently_excludes_both_carrier_paths_after_unlink() {
    for state in ["pending", "accepted", "outcome_unknown"] {
        let (_directory, _vault, config, sender, gateway, command, attachment, _) =
            carrier_mms_command(false);
        gateway
            .install_downloaded_attachment(
                attachment.attachment_id,
                &sender.native_cipher_file(attachment.attachment_id).unwrap(),
            )
            .unwrap();
        gateway
            .record_rcs_link_lifecycle(RcsCapabilityState::Linked, true, true, true, 1_000, 1_000)
            .unwrap();
        let db = rusqlite::Connection::open(&config.database_path).unwrap();
        db.execute_batch(
            "PRAGMA key = \"x'0707070707070707070707070707070707070707070707070707070707070707'\";",
        )
        .unwrap();
        db.execute("INSERT INTO rcs_media_send_ledger(command_id,attachment_index,attachment_id,state,tmp_id) VALUES(?,0,?,?,'media-tmp')",
            rusqlite::params![command.to_string(), attachment.attachment_id.to_string(), state]).unwrap();
        gateway
            .record_rcs_link_lifecycle(
                RcsCapabilityState::Unlinked,
                false,
                false,
                false,
                1_001,
                1_001,
            )
            .unwrap();
        gateway
            .record_rcs_explicit_unlink_completed(1_001, 1_001)
            .unwrap();
        assert_eq!(
            gateway.begin_send_attempt_at(command, 1_001).unwrap(),
            PermitDecision::Blocked(PermitBlock::CarrierOnlyProofUnavailable),
            "{state}"
        );
        assert_eq!(
            gateway
                .begin_automatic_send_attempt(command, None, 1_001)
                .unwrap(),
            AutomaticPermitDecision::Blocked(PermitBlock::CarrierOnlyProofUnavailable),
            "{state}"
        );
        assert!(
            gateway
                .pending_commands()
                .unwrap()
                .iter()
                .all(|item| item.command_id != command)
        );
        assert!(
            gateway
                .pending_automatic_commands()
                .unwrap()
                .iter()
                .all(|item| item.command_id != command)
        );
        assert_eq!(attempt_count(&config, command), 0);
    }
}

#[test]
fn media_store_reopen_recovers_every_interrupted_phase_once() {
    for (state, expected) in [
        ("uploading", "pending"),
        ("reference_ready", "pending"),
        ("permit_issued", "outcome_unknown"),
        ("committed", "outcome_unknown"),
    ] {
        let (_directory, vault, config, _sender, gateway, command, attachment, _) =
            carrier_mms_command(false);
        let db = rusqlite::Connection::open(&config.database_path).unwrap();
        db.execute_batch(
            "PRAGMA key = \"x'0707070707070707070707070707070707070707070707070707070707070707'\";",
        )
        .unwrap();
        db.execute("INSERT INTO rcs_media_send_ledger(command_id,attachment_index,attachment_id,state,tmp_id) VALUES(?,0,?,?,'interrupted-media')", rusqlite::params![command.to_string(), attachment.attachment_id.to_string(), state]).unwrap();
        drop(gateway);
        let reopened = unlocked(&config, &vault);
        let stored: String = db
            .query_row(
                "SELECT state FROM rcs_media_send_ledger WHERE command_id=?",
                [command.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(stored, expected, "{state}");
        let id: String = db
            .query_row(
                "SELECT message_id FROM commands WHERE command_id=?",
                [command.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        let projected = reopened
            .rcs_media_outgoing_status(id.parse().unwrap(), 0)
            .unwrap()
            .unwrap();
        assert_eq!(
            projected.state,
            if expected == "pending" {
                RcsMediaSyncState::Sending
            } else {
                RcsMediaSyncState::OutcomeUnknown
            }
        );
        let count = reopened.pending_outbox_count().unwrap();
        let shared = unlocked(&config, &vault);
        assert_eq!(
            shared.pending_outbox_count().unwrap(),
            count,
            "second handle must not recover a live store"
        );
    }
}

#[test]
fn old_epoch_media_does_not_poison_discovery_or_erase_per_index_outcome() {
    for (initial, expected) in [
        ("accepted", "accepted"),
        ("outcome_unknown", "outcome_unknown"),
        ("committed", "outcome_unknown"),
        ("reference_ready", "refused"),
    ] {
        let (_directory, vault, config, _sender, gateway, command, attachment, conversation) =
            carrier_mms_command(false);
        let db = rusqlite::Connection::open(&config.database_path).unwrap();
        db.execute_batch(
            "PRAGMA key = \"x'0707070707070707070707070707070707070707070707070707070707070707'\";",
        )
        .unwrap();
        db.execute("INSERT INTO rcs_media_send_ledger(command_id,attachment_index,attachment_id,state,tmp_id) VALUES(?,0,?,?,'epoch-media')", rusqlite::params![command.to_string(), attachment.attachment_id.to_string(), initial]).unwrap();
        let next = Vault::epoch(vault.id, 2);
        gateway
            .unlock(&next.profile, &next.header, PASSPHRASE)
            .unwrap();
        gateway.activate_epoch(2).unwrap();
        assert!(gateway.pending_rcs_media_commands(1_000).is_ok());
        let message = gateway
            .messages(conversation)
            .unwrap()
            .into_iter()
            .find(|message| message.payload.direction == Direction::Outgoing)
            .unwrap();
        assert_ne!(message.send_state, Some(SendState::FailedBeforeSubmission));
        let state: String = db
            .query_row(
                "SELECT state FROM rcs_media_send_ledger WHERE command_id=?",
                [command.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(state, expected);
    }
}

#[test]
fn lifecycle_only_unlink_without_completion_stays_blocked_after_reopen() {
    let (_directory, vault, config, gateway, command_id) = carrier_command();
    gateway
        .record_rcs_link_lifecycle(RcsCapabilityState::Linked, true, true, true, 1_000, 1_000)
        .unwrap();
    gateway
        .record_rcs_link_lifecycle(
            RcsCapabilityState::Unlinked,
            false,
            false,
            false,
            1_001,
            1_001,
        )
        .unwrap();
    drop(gateway);

    let reopened = unlocked(&config, &vault);
    assert_eq!(
        reopened.begin_send_attempt_at(command_id, 1_001).unwrap(),
        PermitDecision::Blocked(PermitBlock::CarrierOnlyProofUnavailable)
    );
}

#[test]
fn lifecycle_only_completion_is_revoked_by_successor_lifecycles() {
    for successor in [RcsCapabilityState::SignedOut, RcsCapabilityState::Unlinked] {
        let (_directory, vault, config, gateway, command_id) = carrier_command();
        gateway
            .record_rcs_link_lifecycle(RcsCapabilityState::Linked, true, true, true, 1_000, 1_000)
            .unwrap();
        gateway
            .record_rcs_link_lifecycle(
                RcsCapabilityState::Unlinked,
                false,
                false,
                false,
                1_001,
                1_001,
            )
            .unwrap();
        gateway
            .record_rcs_explicit_unlink_completed(1_001, 1_001)
            .unwrap();
        gateway
            .record_rcs_link_lifecycle(RcsCapabilityState::Linked, true, true, true, 1_002, 1_002)
            .unwrap();
        gateway
            .record_rcs_link_lifecycle(successor, false, false, false, 1_003, 1_003)
            .unwrap();
        drop(gateway);

        let reopened = unlocked(&config, &vault);
        assert_eq!(
            reopened.begin_send_attempt_at(command_id, 1_003).unwrap(),
            PermitDecision::Blocked(PermitBlock::CarrierOnlyProofUnavailable)
        );
    }
}

#[test]
fn first_use_lifecycle_only_completion_is_a_permitted_no_context_noop() {
    let (_directory, vault, config, gateway, command_id) = carrier_command();
    gateway
        .record_rcs_link_lifecycle(
            RcsCapabilityState::Unlinked,
            false,
            false,
            false,
            1_000,
            1_000,
        )
        .unwrap();
    gateway
        .record_rcs_explicit_unlink_completed(1_000, 1_000)
        .unwrap();
    drop(gateway);

    let reopened = unlocked(&config, &vault);
    assert!(matches!(
        reopened.begin_send_attempt_at(command_id, 1_000).unwrap(),
        PermitDecision::Permit(_)
    ));
}

#[test]
fn explicit_unlink_completion_rejects_missing_terminal_and_stale_lifecycles() {
    let (_directory, _vault, _config, gateway, _command_id) = carrier_command();
    assert!(
        gateway
            .record_rcs_explicit_unlink_completed(1_000, 1_000)
            .is_err()
    );

    for state in [
        RcsCapabilityState::Linked,
        RcsCapabilityState::SignedOut,
        RcsCapabilityState::NeedsRelink,
    ] {
        gateway
            .record_rcs_link_lifecycle(state, false, false, false, 1_000, 1_000)
            .unwrap();
        assert!(
            gateway
                .record_rcs_explicit_unlink_completed(1_000, 1_000)
                .is_err()
        );
    }

    gateway
        .record_rcs_link_lifecycle(
            RcsCapabilityState::Unlinked,
            false,
            false,
            false,
            1_001,
            1_001,
        )
        .unwrap();
    assert!(
        gateway
            .record_rcs_explicit_unlink_completed(1_002, 1_001)
            .is_err()
    );
    assert!(
        gateway
            .record_rcs_explicit_unlink_completed(0, 1_001)
            .is_err()
    );
    let (_directory, _vault, _config, fresh_gateway, _command_id) = carrier_command();
    fresh_gateway
        .record_rcs_link_lifecycle(RcsCapabilityState::Linked, true, true, true, 1_000, 1_000)
        .unwrap();
    fresh_gateway
        .verify_rcs_mapping(&observation("peer-local", 1), 1_000)
        .unwrap();
    fresh_gateway
        .record_rcs_link_lifecycle(
            RcsCapabilityState::Unlinked,
            false,
            false,
            false,
            1_001,
            1_001,
        )
        .unwrap();
    fresh_gateway
        .record_rcs_explicit_unlink_completed(1_001, 1_001)
        .unwrap();

    let (_directory, _vault, _config, stale_gateway, stale_command_id) = carrier_command();
    stale_gateway
        .record_rcs_link_lifecycle(RcsCapabilityState::Linked, true, true, true, 1_000, 1_000)
        .unwrap();
    stale_gateway
        .verify_rcs_mapping(&observation("peer-local", 1), 1_000)
        .unwrap();
    stale_gateway
        .record_rcs_link_lifecycle(
            RcsCapabilityState::Unlinked,
            false,
            false,
            false,
            1_001,
            1_001,
        )
        .unwrap();
    assert!(
        stale_gateway
            .record_rcs_explicit_unlink_completed(31_002, 31_002)
            .is_err()
    );
    assert_eq!(
        stale_gateway
            .begin_send_attempt_at(stale_command_id, 31_002)
            .unwrap(),
        PermitDecision::Blocked(PermitBlock::CarrierOnlyProofUnavailable)
    );
}

#[test]
fn explicit_unlink_authority_expires_on_every_successor_lifecycle_generation() {
    for successor in [
        RcsCapabilityState::Linked,
        RcsCapabilityState::SignedOut,
        RcsCapabilityState::NeedsRelink,
        RcsCapabilityState::Unlinked,
    ] {
        let (_directory, vault, config, gateway, command_id) = carrier_command();
        gateway
            .record_rcs_link_lifecycle(RcsCapabilityState::Linked, true, true, true, 1_000, 1_000)
            .unwrap();
        gateway
            .verify_rcs_mapping(&observation("peer-local", 1), 1_000)
            .unwrap();
        gateway
            .record_rcs_link_lifecycle(
                RcsCapabilityState::Unlinked,
                false,
                false,
                false,
                1_001,
                1_001,
            )
            .unwrap();
        gateway
            .record_rcs_explicit_unlink_completed(1_001, 1_001)
            .unwrap();
        if successor == RcsCapabilityState::Unlinked {
            gateway
                .record_rcs_link_lifecycle(
                    RcsCapabilityState::Linked,
                    true,
                    true,
                    true,
                    1_002,
                    1_002,
                )
                .unwrap();
            gateway
                .record_rcs_link_lifecycle(successor, false, false, false, 1_003, 1_003)
                .unwrap();
        } else {
            gateway
                .record_rcs_link_lifecycle(successor, false, false, false, 1_002, 1_002)
                .unwrap();
        }
        drop(gateway);

        let reopened = unlocked(&config, &vault);
        assert_eq!(
            reopened
                .begin_automatic_send_attempt(command_id, None, 1_003)
                .unwrap(),
            AutomaticPermitDecision::Blocked(PermitBlock::CarrierOnlyProofUnavailable)
        );
        assert_eq!(
            reopened.begin_send_attempt_at(command_id, 1_003).unwrap(),
            PermitDecision::Blocked(PermitBlock::CarrierOnlyProofUnavailable)
        );
    }
}

#[test]
fn an_unlinked_observation_without_explicit_owner_action_does_not_enable_carrier() {
    let (_directory, _vault, _config, gateway, command_id) = carrier_command();
    let mut linked = observation("peer-local", 1);
    linked.google_candidate_count = 0;
    gateway.verify_rcs_mapping(&linked, 1_000).unwrap();
    let mut incidental_unlinked = linked;
    incidental_unlinked.context_revision = 2;
    incidental_unlinked.mapping_revision = 2;
    incidental_unlinked.observed_at_ms = 1_001;
    incidental_unlinked.link_state = RcsLinkState::Unlinked;
    gateway
        .verify_rcs_mapping(&incidental_unlinked, 1_001)
        .unwrap();

    assert_eq!(
        gateway.begin_send_attempt_at(command_id, 1_001).unwrap(),
        PermitDecision::Blocked(PermitBlock::CarrierOnlyProofUnavailable)
    );
}

#[test]
fn observed_unlink_after_a_link_blocks_carrier_after_reopen() {
    let (_directory, vault, config, gateway, command_id) = carrier_command();
    let conversation_id = gateway
        .pending_commands()
        .unwrap()
        .pop()
        .unwrap()
        .message
        .record
        .conversation_id;
    gateway
        .record_rcs_link_lifecycle(RcsCapabilityState::Linked, true, true, true, 1_000, 1_000)
        .unwrap();
    gateway
        .verify_rcs_mapping(&observation("peer-local", 1), 1_000)
        .unwrap();
    gateway
        .record_rcs_link_lifecycle(
            RcsCapabilityState::Unlinked,
            false,
            false,
            false,
            1_001,
            1_001,
        )
        .unwrap();
    assert!(gateway.rcs_mapping(conversation_id).unwrap().is_none());
    drop(gateway);

    let reopened = unlocked(&config, &vault);
    assert_eq!(
        reopened.begin_send_attempt_at(command_id, 1_001).unwrap(),
        PermitDecision::Blocked(PermitBlock::CarrierOnlyProofUnavailable)
    );
}

#[test]
fn previously_linked_terminal_lifecycle_states_block_carrier_after_reopen() {
    for state in [
        RcsCapabilityState::SignedOut,
        RcsCapabilityState::NeedsRelink,
    ] {
        let (_directory, vault, config, gateway, command_id) = carrier_command();
        gateway
            .record_rcs_link_lifecycle(RcsCapabilityState::Linked, true, true, true, 1_000, 1_000)
            .unwrap();
        gateway
            .record_rcs_link_lifecycle(state, false, false, false, 1_001, 1_001)
            .unwrap();
        drop(gateway);

        let reopened = unlocked(&config, &vault);
        assert_eq!(
            reopened.begin_send_attempt_at(command_id, 1_001).unwrap(),
            PermitDecision::Blocked(PermitBlock::CarrierOnlyProofUnavailable)
        );
    }
}

#[test]
fn terminal_link_states_do_not_fall_back_to_carrier() {
    for link_state in [RcsLinkState::NeedsRelink, RcsLinkState::SignedOut] {
        let (_directory, _vault, _config, gateway, command_id) = carrier_command();
        gateway
            .record_rcs_link_lifecycle(RcsCapabilityState::Linked, true, true, true, 999, 999)
            .unwrap();
        let mut unavailable = observation("peer-local", 1);
        unavailable.google_candidate_count = 0;
        unavailable.link_state = link_state;
        gateway.verify_rcs_mapping(&unavailable, 1_000).unwrap();
        assert_eq!(
            gateway.begin_send_attempt_at(command_id, 1_000).unwrap(),
            PermitDecision::Blocked(PermitBlock::CarrierOnlyProofUnavailable)
        );
    }
}

#[test]
fn terminal_context_cannot_be_relabelled_as_an_explicit_unlink() {
    for link_state in [RcsLinkState::NeedsRelink, RcsLinkState::SignedOut] {
        let (_directory, _vault, _config, gateway, command_id) = carrier_command();
        gateway
            .record_rcs_link_lifecycle(RcsCapabilityState::Linked, true, true, true, 999, 999)
            .unwrap();
        let mut terminal = observation("peer-local", 1);
        terminal.google_candidate_count = 0;
        terminal.link_state = link_state;
        gateway.verify_rcs_mapping(&terminal, 1_000).unwrap();
        let mut unlinked = terminal;
        unlinked.link_state = RcsLinkState::Unlinked;
        assert!(gateway.update_rcs_context(&unlinked, 1_000).is_err());
        assert_eq!(
            gateway.begin_send_attempt_at(command_id, 1_000).unwrap(),
            PermitDecision::Blocked(PermitBlock::CarrierOnlyProofUnavailable)
        );
    }
}

#[test]
fn carrier_only_proof_rejects_multiple_or_mixed_raw_recipients() {
    let (_directory, _vault, _config, gateway, _command_id) = carrier_command();
    let mut unknown = observation("peer-local", 1);
    unknown.google_candidate_count = 0;
    gateway.verify_rcs_mapping(&unknown, 1_000).unwrap();
    let command = gateway.pending_commands().unwrap().pop().unwrap();
    let mut proof = carrier_proof(&command);
    proof.recipients = vec!["peer-local".into(), "different-peer-local".into()];
    assert!(gateway.record_carrier_only_proof(&proof, 1_000).is_err());
}

#[test]
fn automatic_locally_available_unregistered_media_invalidates_contradicted_proof_durably() {
    let (_directory, vault, config, sender, gateway, command_id, attachment, conversation_id) =
        carrier_mms_command(true);
    gateway
        .install_downloaded_attachment(
            attachment.attachment_id,
            &sender.native_cipher_file(attachment.attachment_id).unwrap(),
        )
        .unwrap();
    record_media_carrier_proof(&gateway, &config, conversation_id);
    let retention_use = gateway
        .pending_attachment_retention_operations()
        .unwrap()
        .into_iter()
        .find(|operation| operation.kind == AttachmentUseKind::PendingSend)
        .unwrap();
    let contradictory = observation("different-peer-local", 1);

    assert_eq!(
        gateway
            .begin_automatic_send_attempt(command_id, Some(&contradictory), 1_000)
            .unwrap(),
        AutomaticPermitDecision::Blocked(PermitBlock::MediaUnavailable)
    );
    assert_eq!(attempt_count(&config, command_id), 0);
    gateway
        .apply_attachment_retention_response(&attachment_response(
            &retention_use,
            AttachmentUseState::Registered,
        ))
        .unwrap();
    drop(gateway);
    let reopened = unlocked(&config, &vault);
    reopened
        .configure_attachment_retention_protocol(true)
        .unwrap();
    for operation in reopened
        .prepare_attachment_retention_operations(100)
        .unwrap()
    {
        reopened
            .apply_attachment_retention_response(&attachment_response(
                &operation,
                AttachmentUseState::Registered,
            ))
            .unwrap();
    }
    reopened.mark_attachment_retention_reconciled().unwrap();
    assert_eq!(
        reopened
            .begin_automatic_send_attempt(command_id, None, 1_001)
            .unwrap(),
        AutomaticPermitDecision::Blocked(PermitBlock::CarrierOnlyProofUnavailable)
    );
    assert_eq!(attempt_count(&config, command_id), 0);
}

#[test]
fn automatic_download_pending_media_invalidates_contradicted_proof_durably() {
    let (_directory, vault, config, sender, gateway, command_id, attachment, conversation_id) =
        carrier_mms_command(false);
    record_media_carrier_proof(&gateway, &config, conversation_id);
    let contradictory = observation("different-peer-local", 1);

    assert_eq!(
        gateway
            .begin_automatic_send_attempt(command_id, Some(&contradictory), 1_000)
            .unwrap(),
        AutomaticPermitDecision::Blocked(PermitBlock::MediaUnavailable)
    );
    assert_eq!(attempt_count(&config, command_id), 0);
    gateway
        .install_downloaded_attachment(
            attachment.attachment_id,
            &sender.native_cipher_file(attachment.attachment_id).unwrap(),
        )
        .unwrap();
    drop(gateway);
    let reopened = unlocked(&config, &vault);
    assert_eq!(
        reopened
            .begin_automatic_send_attempt(command_id, None, 1_001)
            .unwrap(),
        AutomaticPermitDecision::Blocked(PermitBlock::CarrierOnlyProofUnavailable)
    );
    assert_eq!(attempt_count(&config, command_id), 0);
}

#[test]
fn automatic_fresh_carrier_proof_permits_original_sim_once_and_rcs_link_never_carrier() {
    let (_directory, _vault, config, gateway, command_id) = carrier_command();
    capture_us_region(&gateway, &config);
    let mut unknown = observation("peer-local", 1);
    unknown.google_candidate_count = 0;
    gateway.verify_rcs_mapping(&unknown, 1_000).unwrap();
    let command = gateway.pending_commands().unwrap().pop().unwrap();
    gateway
        .record_carrier_only_proof(&carrier_proof(&command), 1_000)
        .unwrap();

    let AutomaticPermitDecision::Carrier(permitted) = gateway
        .begin_automatic_send_attempt(command_id, None, 1_000)
        .unwrap()
    else {
        panic!("fresh proof must permit carrier");
    };
    assert_eq!(permitted.subscription_id, "android-subscription-7");
    assert_eq!(
        gateway
            .begin_automatic_send_attempt(command_id, None, 1_001)
            .unwrap(),
        AutomaticPermitDecision::AlreadyAttempted(SendState::AttemptRecorded)
    );
    assert_eq!(attempt_count(&config, command_id), 1);

    let (_directory, _vault, config, gateway, command_id, _current) = rcs_command("peer-local");
    assert_eq!(
        gateway
            .begin_automatic_send_attempt(command_id, None, 1_000)
            .unwrap(),
        AutomaticPermitDecision::Blocked(PermitBlock::MappingUnproven)
    );
    assert_eq!(attempt_count(&config, command_id), 0);
}

#[test]
fn automatic_restore_history_and_epoch_fences_reject_without_attempts() {
    let (_directory, _vault, config, gateway, command_id) = carrier_command();
    gateway.mark_restored_gateway().unwrap();
    assert_eq!(
        gateway
            .begin_automatic_send_attempt(command_id, None, 1_000)
            .unwrap(),
        AutomaticPermitDecision::Blocked(PermitBlock::RestoreGuarded)
    );
    assert_eq!(attempt_count(&config, command_id), 0);

    let (_directory, vault, config, gateway, command_id) = carrier_command();
    drop(gateway);
    encrypted_connection(&config)
        .execute(
            "UPDATE command_ledger SET historical=1 WHERE command_id=?",
            [command_id.to_string()],
        )
        .unwrap();
    let gateway = unlocked(&config, &vault);
    assert_eq!(
        gateway
            .begin_automatic_send_attempt(command_id, None, 1_000)
            .unwrap(),
        AutomaticPermitDecision::Blocked(PermitBlock::Historical)
    );
    assert_eq!(attempt_count(&config, command_id), 0);

    let (_directory, vault, config, gateway, command_id) = carrier_command();
    let epoch_two = Vault::epoch(vault.id, 2);
    gateway
        .unlock(&epoch_two.profile, &epoch_two.header, PASSPHRASE)
        .unwrap();
    gateway.activate_epoch(2).unwrap();
    assert_eq!(
        gateway
            .begin_automatic_send_attempt(command_id, None, 1_000)
            .unwrap(),
        AutomaticPermitDecision::Blocked(PermitBlock::StaleEpoch)
    );
    assert_eq!(attempt_count(&config, command_id), 0);
}

#[test]
fn concurrent_cross_api_admissions_create_exactly_one_rcs_attempt_and_uuid() {
    let (_directory, _vault, config, gateway, command_id, current) = rcs_command("peer-local");
    let barrier = Arc::new(Barrier::new(2));
    let automatic_client = gateway.clone();
    let rcs_client = gateway.clone();
    let automatic_observation = current.clone();
    let rcs_observation = current;
    let automatic_barrier = barrier.clone();
    let rcs_barrier = barrier;

    let (automatic, strict) = std::thread::scope(|scope| {
        let automatic = scope.spawn(|| {
            automatic_barrier.wait();
            automatic_client.begin_automatic_send_attempt(
                command_id,
                Some(&automatic_observation),
                1_000,
            )
        });
        let strict = scope.spawn(|| {
            rcs_barrier.wait();
            rcs_client.begin_rcs_send_attempt(command_id, &rcs_observation, 1_000)
        });
        (automatic.join().unwrap(), strict.join().unwrap())
    });

    let winning_tmp_id = match (automatic, strict) {
        (
            Ok(AutomaticPermitDecision::Rcs(permit)),
            Err(Error::AlreadyAttempted(SendState::AttemptRecorded)),
        ) => permit.tmp_id().to_owned(),
        (Ok(AutomaticPermitDecision::AlreadyAttempted(SendState::AttemptRecorded)), Ok(permit)) => {
            permit.tmp_id().to_owned()
        }
        results => panic!("expected one permit and one recorded-attempt refusal, got {results:?}"),
    };
    assert_eq!(attempt_count(&config, command_id), 1);
    let connection = encrypted_connection(&config);
    let (attempts, tmp_id): (i64, String) = connection
        .query_row(
            "SELECT COUNT(*),r.tmp_id FROM attempts a JOIN rcs_send_ledger r USING(command_id) WHERE a.command_id=? AND a.finalized_transport='rcs'",
            [command_id.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(attempts, 1);
    assert_eq!(tmp_id, winning_tmp_id);
}

#[test]
fn automatic_carrier_upgrade_remains_frozen_as_rcs_outcome_unknown_after_reopen() {
    let (_directory, vault, config, gateway, command_id) = carrier_command();
    let current = observation("peer-local", 1);
    gateway.verify_rcs_mapping(&current, 1_000).unwrap();
    let AutomaticPermitDecision::Rcs(first) = gateway
        .begin_automatic_send_attempt(command_id, Some(&current), 1_000)
        .unwrap()
    else {
        panic!("expected RCS upgrade");
    };
    drop(gateway);

    let reopened = unlocked(&config, &vault);
    assert_eq!(
        reopened
            .begin_automatic_send_attempt(command_id, None, 1_001)
            .unwrap(),
        AutomaticPermitDecision::AlreadyAttempted(SendState::OutcomeUnknown)
    );
    let connection = encrypted_connection(&config);
    let (transport, tmp_id): (String, String) = connection
        .query_row(
            "SELECT a.finalized_transport,r.tmp_id FROM attempts a JOIN rcs_send_ledger r USING(command_id) WHERE a.command_id=?",
            [command_id.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(transport, "rcs");
    assert_eq!(tmp_id, first.tmp_id());
}

#[test]
fn automatic_original_sim_mismatch_invalidates_existing_proof_across_reopen() {
    let (_directory, vault, config, gateway, command_id) =
        carrier_command_on_sim("android-subscription-8");
    capture_us_region(&gateway, &config);
    let mut unknown = observation("peer-local", 1);
    unknown.google_candidate_count = 0;
    gateway.verify_rcs_mapping(&unknown, 1_000).unwrap();
    let command = gateway.pending_commands().unwrap().pop().unwrap();
    gateway
        .record_carrier_only_proof(&carrier_proof(&command), 1_000)
        .unwrap();
    let wrong_sim = observation("peer-local", 2);
    assert!(matches!(
        gateway.verify_rcs_mapping(&wrong_sim, 1_000).unwrap(),
        RcsMappingOutcome::Mapped(_)
    ));

    assert_eq!(
        gateway
            .begin_automatic_send_attempt(command_id, Some(&wrong_sim), 1_000)
            .unwrap(),
        AutomaticPermitDecision::Blocked(PermitBlock::MappingUnproven)
    );
    assert_eq!(
        gateway
            .begin_automatic_send_attempt(command_id, None, 1_001)
            .unwrap(),
        AutomaticPermitDecision::Blocked(PermitBlock::CarrierOnlyProofUnavailable)
    );
    drop(gateway);
    assert_eq!(
        unlocked(&config, &vault)
            .begin_automatic_send_attempt(command_id, None, 1_001)
            .unwrap(),
        AutomaticPermitDecision::Blocked(PermitBlock::CarrierOnlyProofUnavailable)
    );
    assert_eq!(attempt_count(&config, command_id), 0);
    assert_eq!(rcs_ledger_count(&config, command_id), 0);
}

#[test]
fn upgraded_carrier_rcs_retry_and_explicit_unknown_resend_preserve_finalization_rules() {
    let (_directory, vault, config, gateway, command_id) = carrier_command();
    let current = observation("peer-local", 1);
    gateway.verify_rcs_mapping(&current, 1_000).unwrap();
    let AutomaticPermitDecision::Rcs(first) = gateway
        .begin_automatic_send_attempt(command_id, Some(&current), 1_000)
        .unwrap()
    else {
        panic!("expected RCS upgrade");
    };
    gateway
        .record_send_result(command_id, SendResult::FailedBeforeSubmission)
        .unwrap();
    let retry = gateway
        .retry_rcs_failed_before_submission(command_id, &current, 1_001)
        .unwrap();
    assert_eq!(retry.tmp_id(), first.tmp_id());

    drop(gateway);
    let gateway = unlocked(&config, &vault);
    let resend = gateway.resend_rcs_outcome_unknown(command_id).unwrap();
    let connection = encrypted_connection(&config);
    let resend_tmp_id: String = connection
        .query_row(
            "SELECT tmp_id FROM rcs_send_ledger WHERE command_id=?",
            [resend.command_id.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    assert_ne!(resend_tmp_id, first.tmp_id());
    assert_eq!(attempt_count(&config, resend.command_id), 0);
    assert_eq!(
        gateway
            .begin_automatic_send_attempt(resend.command_id, Some(&current), 1_002)
            .unwrap(),
        AutomaticPermitDecision::Blocked(PermitBlock::NotReceived)
    );
    assert_eq!(attempt_count(&config, resend.command_id), 0);
}

#[test]
fn stale_foreign_and_database_error_refusals_do_not_revoke_current_mapping() {
    for refusal in 0..2 {
        let (_directory, vault, config, gateway, command_id, current) = rcs_command("peer-local");
        let conversation_id = command_conversation(&gateway);
        let mut refused = current;
        let now_ms = if refusal == 0 {
            1_000 + RCS_MAPPING_FRESHNESS_MS + 1
        } else {
            refused.google_conversation_id = "foreign-google-conversation".into();
            1_000
        };
        assert!(
            gateway
                .begin_rcs_send_attempt(command_id, &refused, now_ms)
                .is_err()
        );
        assert_eq!(attempt_count(&config, command_id), 0);
        drop(gateway);
        let reopened = unlocked(&config, &vault);
        assert!(reopened.rcs_mapping(conversation_id).unwrap().is_some());
    }

    let (_directory, vault, config, gateway, command_id, current) = rcs_command("peer-local");
    let conversation_id = command_conversation(&gateway);
    drop(gateway);
    let connection = encrypted_connection(&config);
    connection
        .execute(
            "UPDATE metadata SET v='not-json' WHERE k='rcs_mapping_context_v1'",
            [],
        )
        .unwrap();
    drop(connection);
    let reopened = unlocked(&config, &vault);
    assert_eq!(
        reopened
            .begin_rcs_send_attempt(command_id, &current, 1_000)
            .unwrap_err(),
        Error::Database
    );
    assert!(reopened.rcs_mapping(conversation_id).unwrap().is_some());
    assert_eq!(attempt_count(&config, command_id), 0);
}

#[test]
fn stale_future_and_foreign_routing_refusals_preserve_current_authority() {
    let (_directory, vault, config, gateway, command_id, current) = rcs_command("peer-local");
    let mut stale = current.clone();
    stale.observed_at_ms = 1_000 - RCS_MAPPING_FRESHNESS_MS - 1;
    stale.outgoing.sim_payload.two += 1;
    assert!(
        gateway
            .begin_rcs_send_attempt(command_id, &stale, 1_000)
            .is_err()
    );
    assert_eq!(attempt_count(&config, command_id), 0);
    assert_eq!(rcs_ledger_count(&config, command_id), 0);
    drop(gateway);
    let reopened = unlocked(&config, &vault);
    let permit = reopened
        .begin_rcs_send_attempt(command_id, &current, 1_000)
        .unwrap();
    assert!(uuid::Uuid::parse_str(permit.tmp_id()).is_ok());

    let (_directory, vault, config, gateway, command_id, current) = rcs_command("peer-local");
    let mut future = current.clone();
    future.observed_at_ms = 1_001;
    future.outgoing.sim_payload.two += 1;
    assert_eq!(
        gateway
            .begin_automatic_send_attempt(command_id, Some(&future), 1_000)
            .unwrap(),
        AutomaticPermitDecision::Blocked(PermitBlock::MappingUnproven)
    );
    assert_eq!(attempt_count(&config, command_id), 0);
    assert_eq!(rcs_ledger_count(&config, command_id), 0);
    drop(gateway);
    let reopened = unlocked(&config, &vault);
    assert!(matches!(
        reopened
            .begin_automatic_send_attempt(command_id, Some(&current), 1_000)
            .unwrap(),
        AutomaticPermitDecision::Rcs(_)
    ));

    let (_directory, vault, config, gateway, command_id, current) = rcs_command("peer-local");
    let first = gateway
        .begin_rcs_send_attempt(command_id, &current, 1_000)
        .unwrap();
    let tmp_id = first.tmp_id().to_owned();
    gateway
        .record_send_result(command_id, SendResult::FailedBeforeSubmission)
        .unwrap();
    let mut foreign = current.clone();
    foreign.google_conversation_id = "foreign-google-conversation".into();
    foreign.outgoing.sim_payload.two += 1;
    assert!(
        gateway
            .retry_rcs_failed_before_submission(command_id, &foreign, 1_000)
            .is_err()
    );
    assert_eq!(attempt_count(&config, command_id), 1);
    drop(gateway);
    let reopened = unlocked(&config, &vault);
    let retry = reopened
        .retry_rcs_failed_before_submission(command_id, &current, 1_000)
        .unwrap();
    assert_eq!(retry.tmp_id(), tmp_id);
    assert_eq!(attempt_count(&config, command_id), 1);
}

#[test]
fn admission_does_not_recover_or_replace_legacy_context() {
    let (_directory, vault, config, gateway, command_id, current) = rcs_command("peer-local");
    let conversation_id = command_conversation(&gateway);
    drop(gateway);
    let connection = encrypted_connection(&config);
    connection
        .execute(
            "UPDATE metadata SET v=? WHERE k='rcs_mapping_context_v1'",
            [r#"{"link_generation":1,"session_generation":1,"context_revision":1}"#],
        )
        .unwrap();
    drop(connection);

    let reopened = unlocked(&config, &vault);
    assert!(
        reopened
            .begin_rcs_send_attempt(command_id, &current, 1_000)
            .is_err()
    );
    assert!(reopened.rcs_mapping(conversation_id).unwrap().is_some());
    assert_eq!(attempt_count(&config, command_id), 0);
    assert_eq!(rcs_ledger_count(&config, command_id), 0);

    let fresh = fresh_reobservation(current);
    assert!(matches!(
        reopened.verify_rcs_mapping(&fresh, 1_001).unwrap(),
        RcsMappingOutcome::Mapped(_)
    ));
}

fn explicit_resend_echo() -> (
    tempfile::TempDir,
    Vault,
    ClientConfig,
    Client,
    CommandId,
    RcsMappingObservation,
    Envelope,
) {
    let (directory, vault, config, gateway, command_id, observation) = rcs_command("peer-local");
    gateway
        .begin_rcs_send_attempt(command_id, &observation, 1_000)
        .unwrap();
    drop(gateway);
    let gateway = unlocked(&config, &vault);
    let queued = gateway.resend_rcs_outcome_unknown(command_id).unwrap();
    let mut server = Server::default();
    server.upload(&gateway);
    let echo = server
        .log
        .into_iter()
        .find(|envelope| envelope.command_id == Some(queued.command_id))
        .unwrap();
    (
        directory,
        vault,
        config,
        gateway,
        queued.command_id,
        observation,
        echo,
    )
}

#[test]
fn explicit_resend_echo_at_a_different_cursor_remains_duplicate() {
    let (_directory, _vault, _config, gateway, command_id, observation, echo) =
        explicit_resend_echo();
    let cursor = Cursor(gateway.receive_cursor().unwrap().0 + 1);
    assert_eq!(
        gateway.ingest(&echo, cursor).unwrap(),
        IngestResult::Journaled
    );
    gateway.apply_pending(100).unwrap();
    gateway
        .begin_rcs_send_attempt(command_id, &observation, 1_000)
        .unwrap();

    assert_eq!(
        gateway.ingest(&echo, Cursor(cursor.0 + 1)).unwrap(),
        IngestResult::Duplicate
    );
    assert_eq!(gateway.apply_pending(100).unwrap().applied, 0);
    assert_eq!(
        gateway
            .begin_rcs_send_attempt(command_id, &observation, 1_000)
            .unwrap_err(),
        Error::AlreadyAttempted(SendState::AttemptRecorded)
    );
}

#[test]
fn explicit_resend_echo_from_snapshot_never_receives_a_permit() {
    let (_directory, _vault, _config, gateway, command_id, observation, echo) =
        explicit_resend_echo();
    let cursor = Cursor(gateway.receive_cursor().unwrap().0 + 1);
    let snapshot = gateway
        .begin_snapshot(cursor, 1, SnapshotPurpose::Resync)
        .unwrap();
    gateway
        .append_snapshot_page(
            snapshot.generation,
            &[SnapshotRecord {
                cursor,
                envelope: echo,
            }],
        )
        .unwrap();
    gateway.finish_snapshot(snapshot.generation).unwrap();
    gateway.apply_pending(100).unwrap();

    assert!(
        gateway
            .begin_rcs_send_attempt(command_id, &observation, 1_000)
            .is_err()
    );
}

#[test]
fn explicit_resend_echo_retired_by_epoch_cutover_never_receives_a_permit() {
    let (_directory, vault, _config, gateway, command_id, observation, echo) =
        explicit_resend_echo();
    let cursor = Cursor(gateway.receive_cursor().unwrap().0 + 1);
    assert_eq!(
        gateway.ingest(&echo, cursor).unwrap(),
        IngestResult::Journaled
    );
    gateway.apply_pending(100).unwrap();
    let epoch_two = Vault::epoch(vault.id, 2);
    gateway
        .unlock(&epoch_two.profile, &epoch_two.header, PASSPHRASE)
        .unwrap();
    gateway.activate_epoch(2).unwrap();

    assert!(
        gateway
            .begin_rcs_send_attempt(command_id, &observation, 1_000)
            .is_err()
    );
}

#[test]
fn explicit_resend_echo_under_restore_guard_never_receives_a_permit() {
    let (_directory, _vault, _config, gateway, command_id, observation, echo) =
        explicit_resend_echo();
    let cursor = Cursor(gateway.receive_cursor().unwrap().0 + 1);
    assert_eq!(
        gateway.ingest(&echo, cursor).unwrap(),
        IngestResult::Journaled
    );
    gateway.apply_pending(100).unwrap();
    gateway.mark_restored_gateway().unwrap();

    assert!(
        gateway
            .begin_rcs_send_attempt(command_id, &observation, 1_000)
            .is_err()
    );
}

#[test]
fn non_resend_self_addressed_rcs_echo_stays_duplicate() {
    let (_directory, _vault, config, gateway, _command_id, _observation) =
        rcs_command("peer-local");
    let queued = gateway
        .queue_send(
            OutgoingSms {
                conversation_id: command_conversation(&gateway),
                recipients: vec!["peer-local".into()],
                body: String::from('\0'),
            },
            GatewayRoute::RcsLink {
                gateway_device_id: config.device_id,
            },
        )
        .unwrap();
    let mut server = Server::default();
    server.upload(&gateway);
    let echo = server
        .log
        .into_iter()
        .find(|envelope| envelope.command_id == Some(queued.command_id))
        .unwrap();
    let cursor = Cursor(gateway.receive_cursor().unwrap().0 + 1);

    assert_eq!(
        gateway.ingest(&echo, cursor).unwrap(),
        IngestResult::Duplicate
    );
    assert_eq!(gateway.apply_pending(100).unwrap().applied, 0);
}

#[test]
fn resend_addressed_to_another_gateway_stays_duplicate_and_has_no_authority() {
    let (_directory, _vault, config, gateway, _command_id, _observation) =
        rcs_command("peer-local");
    let queued = gateway
        .queue_send(
            OutgoingSms {
                conversation_id: command_conversation(&gateway),
                recipients: vec!["peer-local".into()],
                body: String::from('\0'),
            },
            GatewayRoute::RcsLink {
                gateway_device_id: DeviceId::new(),
            },
        )
        .unwrap();
    let connection = encrypted_connection(&config);
    connection
        .execute(
            "INSERT INTO rcs_send_ledger(command_id,tmp_id,mapping_revision,proof_digest,google_conversation_id,attempted_at_ms,retry_of_command_id) VALUES(?,?,?,?,?,?,?)",
            rusqlite::params![
                queued.command_id.to_string(),
                uuid::Uuid::new_v4().to_string(),
                1_u64,
                vec![0_u8; 32],
                "conversation-local",
                0_i64,
                CommandId::new().to_string(),
            ],
        )
        .unwrap();
    drop(connection);
    let mut server = Server::default();
    server.upload(&gateway);
    let echo = server
        .log
        .into_iter()
        .find(|envelope| envelope.command_id == Some(queued.command_id))
        .unwrap();
    let cursor = Cursor(gateway.receive_cursor().unwrap().0 + 1);

    assert_eq!(
        gateway.ingest(&echo, cursor).unwrap(),
        IngestResult::Duplicate
    );
    assert_eq!(gateway.apply_pending(100).unwrap().applied, 0);
    assert_eq!(
        gateway
            .begin_rcs_send_attempt(queued.command_id, &observation("peer-local", 1), 1_000)
            .unwrap_err(),
        Error::NotFound
    );
}
