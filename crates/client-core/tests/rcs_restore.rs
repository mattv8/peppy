//! A restored client database copy must never authorize a second RCS send for a command whose
//! first attempt was already durable in the original database. Core cannot detect rollback, so
//! the pre-begin copy test asserts only what the restored file itself guarantees.
use super::common::*;
use std::fs;

const PEER: &str = "peer-local";

struct RcsLink {
    directory: tempfile::TempDir,
    vault: Vault,
    config: ClientConfig,
    command_id: CommandId,
    observation: RcsMappingObservation,
}

fn observation() -> RcsMappingObservation {
    RcsMappingObservation {
        google_conversation_id: "conversation-local".into(),
        google_candidate_count: 1,
        participants: vec![RcsParticipantObservation {
            identity: PEER.into(),
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

/// Builds a verified mapping and one queued `RcsLink` command, then closes every client so the
/// database files are quiescent before any copy is taken.
fn rcs_link_command() -> RcsLink {
    let directory = tempfile::tempdir().unwrap();
    let vault = Vault::new();
    let sender_config = config(&directory, "sender-restore", &vault);
    let gateway_config = config(&directory, "gateway-restore", &vault);
    let sender = unlocked(&sender_config, &vault);
    let gateway = unlocked(&gateway_config, &vault);
    let captured = gateway
        .capture_incoming(IncomingSms {
            conversation_id: None,
            sender_address: PEER.into(),
            body: String::new(),
            provider_message_id: Some("provider-restore-local".into()),
            imported: true,
        })
        .unwrap();
    let current = observation();
    assert!(matches!(
        gateway.verify_rcs_mapping(&current, 1_000).unwrap(),
        RcsMappingOutcome::Mapped(_)
    ));
    let queued = sender
        .queue_send(
            OutgoingSms {
                conversation_id: captured.conversation_id,
                recipients: vec![PEER.into()],
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
    drop(gateway);
    drop(sender);
    RcsLink {
        directory,
        vault,
        config: gateway_config,
        command_id: queued.command_id,
        observation: current,
    }
}

/// Copies the closed gateway database file. The client never sets a WAL journal mode, so the
/// main file is the complete committed state once every connection is dropped.
fn copy_database(link: &RcsLink, name: &str) -> ClientConfig {
    let path = link.directory.path().join(format!("{name}.db"));
    fs::copy(&link.config.database_path, &path).unwrap();
    ClientConfig {
        database_path: path,
        ..link.config.clone()
    }
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

fn attempt_state(config: &ClientConfig, command_id: CommandId) -> String {
    encrypted_connection(config)
        .query_row(
            "SELECT state FROM attempts WHERE command_id=?",
            [command_id.to_string()],
            |row| row.get(0),
        )
        .unwrap()
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

#[test]
fn restored_copy_of_an_in_flight_native_attempt_never_authorizes_a_second_send() {
    let link = rcs_link_command();
    let command_id = link.command_id;
    let gateway = unlocked(&link.config, &link.vault);
    let preparation = gateway.prepare_native_rcs_send(command_id, 1_000).unwrap();
    gateway
        .begin_native_rcs_send(preparation, &link.observation, 1_000)
        .unwrap();
    assert_eq!(attempt_state(&link.config, command_id), "attempt_recorded");
    drop(gateway);

    let restored = copy_database(&link, "restored-in-flight");
    let reopened = unlocked(&restored, &link.vault);
    assert_eq!(attempt_state(&restored, command_id), "outcome_unknown");
    assert_eq!(attempt_count(&restored, command_id), 1);
    assert_eq!(rcs_ledger_count(&restored, command_id), 1);

    assert!(matches!(
        reopened.prepare_native_rcs_send(command_id, 1_000),
        Err(Error::AlreadyAttempted(SendState::OutcomeUnknown))
    ));
    assert_eq!(
        reopened
            .begin_rcs_send_attempt(command_id, &link.observation, 1_000)
            .unwrap_err(),
        Error::AlreadyAttempted(SendState::OutcomeUnknown)
    );
    assert_eq!(
        reopened
            .begin_automatic_send_attempt(command_id, Some(&link.observation), 1_000)
            .unwrap(),
        AutomaticPermitDecision::AlreadyAttempted(SendState::OutcomeUnknown)
    );
    assert!(
        !matches!(
            reopened.begin_send_attempt_at(command_id, 1_000).unwrap(),
            PermitDecision::Permit(_)
        ),
        "a restored copy must not yield a carrier-style permit for an RCS command"
    );
    assert!(
        reopened
            .retry_rcs_failed_before_submission(command_id, &link.observation, 1_001)
            .is_err()
    );
    assert!(
        reopened
            .pending_automatic_commands()
            .unwrap()
            .iter()
            .all(|pending| pending.command_id != command_id),
        "an attempted command must not be offered for automatic routing again"
    );
    assert_eq!(attempt_count(&restored, command_id), 1);
    assert_eq!(rcs_ledger_count(&restored, command_id), 1);
}

#[test]
fn copy_taken_before_begin_stays_refused_in_the_original_and_is_a_documented_residual() {
    let link = rcs_link_command();
    let command_id = link.command_id;
    // Taken while the command is queued and the mapping is verified, before any attempt.
    let pre_begin = copy_database(&link, "restored-before-begin");

    let original = unlocked(&link.config, &link.vault);
    original
        .begin_rcs_send_attempt(command_id, &link.observation, 1_000)
        .unwrap();
    drop(original);

    let reopened = unlocked(&link.config, &link.vault);
    assert_eq!(
        reopened
            .begin_rcs_send_attempt(command_id, &link.observation, 1_000)
            .unwrap_err(),
        Error::AlreadyAttempted(SendState::OutcomeUnknown)
    );
    assert_eq!(attempt_count(&link.config, command_id), 1);
    drop(reopened);

    // The copy is a faithful snapshot of the pre-attempt state. Core cannot tell it apart from
    // a current database, so only the file's own contents are asserted here.
    assert_eq!(attempt_count(&pre_begin, command_id), 0);
    assert_eq!(rcs_ledger_count(&pre_begin, command_id), 0);
}
