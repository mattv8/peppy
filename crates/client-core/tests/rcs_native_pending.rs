use super::common::*;
use std::fs;

const PEER: &str = "peer-local";

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

struct Fixture {
    _directory: tempfile::TempDir,
    vault: Vault,
    gateway_config: ClientConfig,
    sender: Client,
    gateway: Client,
    server: Server,
    conversation_id: ConversationId,
}

impl Fixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let vault = Vault::new();
        let sender_config = config(&directory, "sender-native-pending", &vault);
        let gateway_config = config(&directory, "gateway-native-pending", &vault);
        let sender = unlocked(&sender_config, &vault);
        let gateway = unlocked(&gateway_config, &vault);
        let conversation_id = gateway
            .capture_incoming(IncomingSms {
                conversation_id: None,
                sender_address: PEER.into(),
                body: String::new(),
                provider_message_id: Some("provider-native-pending".into()),
                imported: true,
            })
            .unwrap()
            .conversation_id;
        gateway
            .record_rcs_link_lifecycle(RcsCapabilityState::Linked, true, true, true, 1_000, 1_000)
            .unwrap();
        gateway.verify_rcs_mapping(&observation(), 1_000).unwrap();
        Self {
            _directory: directory,
            vault,
            gateway_config,
            sender,
            gateway,
            server: Server::default(),
            conversation_id,
        }
    }

    fn queue(&mut self) -> CommandId {
        let command_id = self
            .sender
            .queue_send(
                OutgoingSms {
                    conversation_id: self.conversation_id,
                    recipients: vec![PEER.into()],
                    body: "native pending".into(),
                },
                GatewayRoute::RcsLink {
                    gateway_device_id: self.gateway_config.device_id,
                },
            )
            .unwrap()
            .command_id;
        self.server.upload(&self.sender);
        self.server.sync(&self.gateway);
        command_id
    }

    fn queue_carrier_for(&mut self, conversation_id: ConversationId, peer: &str) -> CommandId {
        let command_id = self
            .sender
            .queue_send(
                OutgoingSms {
                    conversation_id,
                    recipients: vec![peer.into()],
                    body: "carrier pending".into(),
                },
                GatewayRoute::Carrier {
                    gateway_device_id: self.gateway_config.device_id,
                    subscription_id: "android-subscription-7".into(),
                },
            )
            .unwrap()
            .command_id;
        self.server.upload(&self.sender);
        self.server.sync(&self.gateway);
        command_id
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

fn pending_ids(client: &Client) -> Vec<CommandId> {
    client
        .pending_native_rcs_commands()
        .unwrap()
        .into_iter()
        .map(|command| command.command_id)
        .collect()
}

#[test]
fn native_candidates_exclude_every_recorded_attempt_state() {
    let mut fixture = Fixture::new();
    let attempt_recorded = fixture.queue();
    let outcome_unknown = fixture.queue();
    let submitted = fixture.queue();
    let acknowledged = fixture.queue();

    for command_id in [attempt_recorded, outcome_unknown, submitted, acknowledged] {
        assert!(pending_ids(&fixture.gateway).contains(&command_id));
    }
    fixture
        .gateway
        .begin_native_rcs_send(
            fixture
                .gateway
                .prepare_native_rcs_send(attempt_recorded, 1_000)
                .unwrap(),
            &observation(),
            1_000,
        )
        .unwrap();
    let unknown_projection = fixture
        .gateway
        .begin_native_rcs_send(
            fixture
                .gateway
                .prepare_native_rcs_send(outcome_unknown, 1_000)
                .unwrap(),
            &observation(),
            1_000,
        )
        .unwrap();
    fixture
        .gateway
        .record_native_rcs_outcome_unknown(
            outcome_unknown,
            &unknown_projection.tmp_id,
            &unknown_projection.target.google_conversation_id,
        )
        .unwrap();
    fixture
        .gateway
        .begin_native_rcs_send(
            fixture
                .gateway
                .prepare_native_rcs_send(submitted, 1_000)
                .unwrap(),
            &observation(),
            1_000,
        )
        .unwrap();
    fixture.gateway.record_rcs_submitted(submitted).unwrap();
    fixture
        .gateway
        .begin_native_rcs_send(
            fixture
                .gateway
                .prepare_native_rcs_send(acknowledged, 1_000)
                .unwrap(),
            &observation(),
            1_000,
        )
        .unwrap();
    fixture.gateway.record_rcs_submitted(acknowledged).unwrap();
    fixture
        .gateway
        .record_rcs_transport_ack(acknowledged, 1_001, "accepted")
        .unwrap();

    assert!(pending_ids(&fixture.gateway).is_empty());
}

#[test]
fn native_candidates_require_linked_lifecycle_and_a_live_exact_mapping() {
    for state in [
        RcsCapabilityState::SignedOut,
        RcsCapabilityState::NeedsRelink,
        RcsCapabilityState::Unlinked,
    ] {
        let mut fixture = Fixture::new();
        let command_id = fixture.queue();
        let connection = encrypted_connection(&fixture.gateway_config);
        connection
            .execute(
                "UPDATE metadata SET v=? WHERE k='rcs_link_lifecycle_v1'",
                [serde_json::to_string(&state).unwrap()],
            )
            .unwrap();
        assert!(pending_ids(&fixture.gateway).is_empty());
        connection
            .execute(
                "UPDATE metadata SET v=? WHERE k='rcs_link_lifecycle_v1'",
                [serde_json::to_string(&RcsCapabilityState::Linked).unwrap()],
            )
            .unwrap();
        assert_eq!(pending_ids(&fixture.gateway), vec![command_id]);
    }
}

#[test]
fn native_candidates_include_carrier_texts_without_a_mapping() {
    let mut fixture = Fixture::new();
    let rcs = fixture.queue();
    let unmapped_conversation = fixture
        .gateway
        .capture_incoming(IncomingSms {
            conversation_id: None,
            sender_address: "+12025550124".into(),
            body: String::new(),
            provider_message_id: Some("provider-native-carrier".into()),
            imported: true,
        })
        .unwrap()
        .conversation_id;
    let carrier = fixture.queue_carrier_for(unmapped_conversation, "+12025550124");
    assert_eq!(pending_ids(&fixture.gateway), vec![rcs, carrier]);

    fixture
        .gateway
        .record_rcs_link_lifecycle(
            RcsCapabilityState::NeedsRelink,
            false,
            false,
            false,
            2_000,
            2_000,
        )
        .unwrap();
    fixture
        .gateway
        .record_rcs_link_lifecycle(RcsCapabilityState::Linked, true, true, true, 3_000, 3_000)
        .unwrap();
    assert_eq!(pending_ids(&fixture.gateway), vec![carrier]);
}

#[test]
fn native_carrier_candidates_are_one_per_conversation_and_capped_at_eight() {
    let mut fixture = Fixture::new();
    let first_conversation = fixture
        .gateway
        .capture_incoming(IncomingSms {
            conversation_id: None,
            sender_address: "+12025550130".into(),
            body: String::new(),
            provider_message_id: Some("provider-native-cap-first".into()),
            imported: true,
        })
        .unwrap()
        .conversation_id;
    let first = fixture.queue_carrier_for(first_conversation, "+12025550130");
    let duplicate = fixture.queue_carrier_for(first_conversation, "+12025550130");
    let mut candidates = vec![first];
    for index in 1..9 {
        let peer = format!("+120255501{index:02}");
        let conversation = fixture
            .gateway
            .capture_incoming(IncomingSms {
                conversation_id: None,
                sender_address: peer.clone(),
                body: String::new(),
                provider_message_id: Some(format!("provider-native-cap-{index}")),
                imported: true,
            })
            .unwrap()
            .conversation_id;
        candidates.push(fixture.queue_carrier_for(conversation, &peer));
    }

    let pending = pending_ids(&fixture.gateway);
    assert_eq!(pending.len(), 8);
    assert_eq!(pending[0], first);
    assert!(!pending.contains(&duplicate));
    assert_eq!(pending, candidates[..8]);

    let next = pending_ids(&fixture.gateway);
    assert_eq!(next.len(), 8);
    assert_eq!(next[0], candidates[8]);
    assert_eq!(next[1], candidates[0]);

    let Fixture {
        _directory,
        vault,
        gateway_config,
        sender,
        gateway,
        ..
    } = fixture;
    drop(gateway);
    drop(sender);
    let reopened = unlocked(&gateway_config, &vault);
    let resumed = pending_ids(&reopened);
    assert_eq!(resumed.len(), 8);
    assert_eq!(resumed[0], candidates[7]);
    assert_eq!(resumed[1], candidates[8]);
}

#[test]
fn native_candidates_prioritize_explicit_commands_within_the_combined_page_cap() {
    let mut fixture = Fixture::new();
    let mut carrier = Vec::new();
    for index in 0..8 {
        let peer = format!("+120255502{index:02}");
        let conversation = fixture
            .gateway
            .capture_incoming(IncomingSms {
                conversation_id: None,
                sender_address: peer.clone(),
                body: String::new(),
                provider_message_id: Some(format!("provider-native-mixed-{index}")),
                imported: true,
            })
            .unwrap()
            .conversation_id;
        carrier.push(fixture.queue_carrier_for(conversation, &peer));
    }
    let explicit = (0..96).map(|_| fixture.queue()).collect::<Vec<_>>();

    let pending = pending_ids(&fixture.gateway);
    assert_eq!(pending.len(), 100);
    assert_eq!(&pending[..96], explicit.as_slice());
    assert_eq!(&pending[96..], &carrier[..4]);
}

#[test]
fn native_candidates_exclude_imported_history_and_restored_commands() {
    let mut fixture = Fixture::new();
    let historical = fixture
        .sender
        .queue_send(
            OutgoingSms {
                conversation_id: fixture.conversation_id,
                recipients: vec![PEER.into()],
                body: "snapshot history".into(),
            },
            GatewayRoute::RcsLink {
                gateway_device_id: fixture.gateway_config.device_id,
            },
        )
        .unwrap()
        .command_id;
    fixture.server.upload(&fixture.sender);
    let records = fixture
        .server
        .log
        .iter()
        .enumerate()
        .map(|(index, envelope)| SnapshotRecord {
            cursor: Cursor(index as u64 + 1),
            envelope: envelope.clone(),
        })
        .collect::<Vec<_>>();
    let high_water = records.len() as u64;
    let session = fixture
        .gateway
        .begin_snapshot(Cursor(high_water), high_water, SnapshotPurpose::Resync)
        .unwrap();
    fixture
        .gateway
        .append_snapshot_page(session.generation, &records)
        .unwrap();
    fixture.gateway.finish_snapshot(session.generation).unwrap();
    fixture.gateway.apply_pending(100).unwrap();
    assert!(!pending_ids(&fixture.gateway).contains(&historical));

    let restored = fixture.queue();
    assert_eq!(pending_ids(&fixture.gateway), vec![restored]);
    let Fixture {
        _directory: directory,
        vault,
        gateway_config,
        sender,
        gateway,
        ..
    } = fixture;
    drop(gateway);
    drop(sender);
    let restored_path = directory.path().join("restored-native-pending.db");
    fs::copy(&gateway_config.database_path, &restored_path).unwrap();
    let restored_config = ClientConfig {
        database_path: restored_path,
        ..gateway_config
    };
    let restored_gateway = unlocked(&restored_config, &vault);
    restored_gateway.mark_restored_gateway().unwrap();
    assert!(pending_ids(&restored_gateway).is_empty());
}

#[test]
fn native_candidates_are_stably_bounded_and_identical_after_reopen() {
    let mut fixture = Fixture::new();
    let mut queued = Vec::new();
    for _ in 0..=100 {
        queued.push(fixture.queue());
    }
    let first = fixture.gateway.pending_native_rcs_commands().unwrap();
    assert_eq!(first.len(), 100);
    assert_eq!(
        first
            .iter()
            .map(|command| command.command_id)
            .collect::<Vec<_>>(),
        queued[..100]
    );

    let Fixture {
        _directory,
        vault,
        gateway_config,
        sender,
        gateway,
        ..
    } = fixture;
    drop(gateway);
    drop(sender);
    let reopened = unlocked(&gateway_config, &vault);
    assert_eq!(reopened.pending_native_rcs_commands().unwrap(), first);
}

#[test]
fn ineligible_head_rows_do_not_starve_later_native_candidates() {
    let mut fixture = Fixture::new();
    let unmapped_conversation = ConversationId::new();
    for _ in 0..150 {
        fixture
            .sender
            .queue_send(
                OutgoingSms {
                    conversation_id: unmapped_conversation,
                    recipients: vec![PEER.into()],
                    body: "unmapped pending".into(),
                },
                GatewayRoute::RcsLink {
                    gateway_device_id: fixture.gateway_config.device_id,
                },
            )
            .unwrap();
    }
    let eligible = fixture
        .sender
        .queue_send(
            OutgoingSms {
                conversation_id: fixture.conversation_id,
                recipients: vec![PEER.into()],
                body: "eligible after unmapped head".into(),
            },
            GatewayRoute::RcsLink {
                gateway_device_id: fixture.gateway_config.device_id,
            },
        )
        .unwrap()
        .command_id;
    fixture.server.upload(&fixture.sender);
    fixture.server.sync(&fixture.gateway);
    fixture.server.sync(&fixture.gateway);
    assert!(
        fixture
            .gateway
            .rcs_mapping(fixture.conversation_id)
            .unwrap()
            .is_some()
    );

    assert_eq!(pending_ids(&fixture.gateway), vec![eligible]);
}
