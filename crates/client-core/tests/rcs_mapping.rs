use super::common::*;

fn observation(participant: &str, revision: u64) -> RcsMappingObservation {
    RcsMappingObservation {
        google_conversation_id: "google-conversation".into(),
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
                link_generation: 4,
                session_generation: 7,
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
        identity_region: "ZZ".into(),
        observed_at_ms: 1_000,
        link_generation: 4,
        session_generation: 7,
        context_revision: 1,
        canonicalizer_revision: 1,
        mapping_revision: revision,
        link_state: RcsLinkState::Linked,
    }
}

fn client_with_participant(
    participant: &str,
) -> (tempfile::TempDir, ClientConfig, Client, ConversationId) {
    let directory = tempfile::tempdir().unwrap();
    let vault = Vault::new();
    let config = config(&directory, "rcs-mapping", &vault);
    let client = open(&config);
    let captured = client
        .capture_incoming(IncomingSms {
            conversation_id: None,
            sender_address: participant.into(),
            body: String::new(),
            provider_message_id: Some("provider-row".into()),
            imported: true,
        })
        .unwrap();
    (directory, config, client, captured.conversation_id)
}

fn capture_in_conversation(
    client: &Client,
    participant: &str,
    provider_id: &str,
) -> ConversationId {
    let conversation_id = ConversationId::new();
    client
        .capture_incoming(IncomingSms {
            conversation_id: Some(conversation_id),
            sender_address: participant.into(),
            body: String::new(),
            provider_message_id: Some(provider_id.into()),
            imported: true,
        })
        .unwrap();
    conversation_id
}

#[test]
fn verified_mapping_is_fresh_local_and_survives_reopen() {
    let (_directory, config, client, conversation_id) = client_with_participant("raw-peer");
    let mapped = client
        .verify_rcs_mapping(&observation("raw-peer", 1), 1_000)
        .unwrap();
    let RcsMappingOutcome::Mapped(mapping) = mapped else {
        panic!("expected mapping");
    };
    assert_eq!(mapping.conversation_id, conversation_id);
    assert_eq!(mapping.proof.canonical_participant, "raw:raw-peer");
    assert_eq!(
        client.rcs_mapping(conversation_id).unwrap(),
        Some(mapping.clone())
    );
    drop(client);
    let reopened = open(&config);
    assert_eq!(
        reopened.rcs_mapping(conversation_id).unwrap(),
        Some(mapping)
    );
}

#[test]
fn refuses_stale_ambiguous_and_invalid_non_e164_observations() {
    let (_directory, _config, client, _conversation_id) = client_with_participant("raw-peer");
    let mut stale = observation("raw-peer", 1);
    stale.observed_at_ms = 1_000 - RCS_MAPPING_FRESHNESS_MS - 1;
    let mut ambiguous = observation("raw-peer", 2);
    ambiguous.participants.push(RcsParticipantObservation {
        identity: "other-peer".into(),
        google_participant_id: "other-google-peer".into(),
        identifier_type: 1,
        is_visible: true,
        is_me: false,
    });
    let missing = observation("unseen-raw-peer", 3);
    let mut multiple_google = observation("raw-peer", 4);
    multiple_google.google_candidate_count = 2;

    for (input, reason) in [
        (stale, RcsMappingRefusal::StaleObservation),
        (ambiguous, RcsMappingRefusal::ParticipantAmbiguous),
        (missing, RcsMappingRefusal::PeppyConversationMissing),
        (
            multiple_google,
            RcsMappingRefusal::GoogleConversationAmbiguous,
        ),
    ] {
        assert_eq!(
            client.verify_rcs_mapping(&input, 1_000).unwrap(),
            RcsMappingOutcome::Refused(reason)
        );
    }
}

#[test]
fn mapping_binds_region_identity_and_live_conversation_facts() {
    let (_directory, _config, client, conversation_id) = client_with_participant("raw-peer");
    let first = client
        .verify_rcs_mapping(&observation("raw-peer", 1), 1_000)
        .unwrap();
    let RcsMappingOutcome::Mapped(first) = first else {
        panic!("expected mapping");
    };

    let mut changed = observation("raw-peer", 2);
    changed.identity_region = "XY".into();
    changed.selected_sim_own_identity = "+12025550101".into();
    changed.context_revision = 2;
    changed.carrier_binding.own_number_raw = "+12025550101".into();
    changed.carrier_binding.route_context_revision = 2;
    changed.outgoing.own_identity_raw = "+12025550101".into();
    changed.native_evidence.current_settings_sims[0].own_identity_raw = "+12025550101".into();
    changed.native_evidence.lifetime.context_revision = 2;
    changed.read_only = true;
    assert_eq!(
        client.verify_rcs_mapping(&changed, 1_000).unwrap(),
        RcsMappingOutcome::Refused(RcsMappingRefusal::IneligibleConversation)
    );
    assert!(client.rcs_mapping(conversation_id).unwrap().is_none());

    let mut refreshed = observation("raw-peer", 3);
    refreshed.identity_region = "XY".into();
    refreshed.selected_sim_own_identity = "+12025550101".into();
    refreshed.context_revision = 2;
    refreshed.carrier_binding.own_number_raw = "+12025550101".into();
    refreshed.carrier_binding.route_context_revision = 2;
    refreshed.outgoing.own_identity_raw = "+12025550101".into();
    refreshed.native_evidence.current_settings_sims[0].own_identity_raw = "+12025550101".into();
    refreshed.native_evidence.lifetime.context_revision = 2;
    let RcsMappingOutcome::Mapped(refreshed) =
        client.verify_rcs_mapping(&refreshed, 1_000).unwrap()
    else {
        panic!("expected refreshed mapping");
    };
    assert_ne!(first.proof.digest, refreshed.proof.digest);
}

#[test]
fn self_chat_is_explicitly_unsupported() {
    let (_directory, _config, client, _conversation_id) = client_with_participant("+12025550100");
    let mut self_chat = observation("+12025550100", 1);
    self_chat.participants[0].is_me = true;
    assert_eq!(
        client.verify_rcs_mapping(&self_chat, 1_000).unwrap(),
        RcsMappingOutcome::Refused(RcsMappingRefusal::SelfChatUnsupported)
    );
}

#[test]
fn refuses_raw_identity_with_multiple_authoritative_message_conversations() {
    let directory = tempfile::tempdir().unwrap();
    let vault = Vault::new();
    let config = config(&directory, "rcs-mapping-duplicates", &vault);
    let client = open(&config);
    capture_in_conversation(&client, "raw-peer", "row-a");
    capture_in_conversation(&client, "raw-peer", "row-b");

    assert_eq!(
        client
            .verify_rcs_mapping(&observation("raw-peer", 1), 1_000)
            .unwrap(),
        RcsMappingOutcome::Refused(RcsMappingRefusal::PeppyConversationAmbiguous)
    );
}

#[test]
fn raw_national_identity_never_matches_inferred_international_alias() {
    let (_directory, _config, client, _conversation_id) = client_with_participant("2025550123");
    let international = format!("+{}{}", "1202", "5550123");
    assert_eq!(
        client
            .verify_rcs_mapping(&observation(&international, 1), 1_000)
            .unwrap(),
        RcsMappingOutcome::Refused(RcsMappingRefusal::PeppyConversationMissing)
    );
}

#[test]
fn formatted_explicit_international_identity_is_canonical_equivalent() {
    let international = format!("+{}{}", "1202", "5550123");
    let formatted = format!("+{} {}", "1", "202 555 0123");
    let (_directory, _config, client, _conversation_id) = client_with_participant(&formatted);
    assert!(matches!(
        client
            .verify_rcs_mapping(&observation(&international, 1), 1_000)
            .unwrap(),
        RcsMappingOutcome::Mapped(_)
    ));
}

#[test]
fn context_and_conflict_invalidate_all_affected_mappings() {
    let directory = tempfile::tempdir().unwrap();
    let vault = Vault::new();
    let config = config(&directory, "rcs-mapping-context", &vault);
    let client = open(&config);
    let conversation_a = capture_in_conversation(&client, "peer-a", "row-a");
    let conversation_b = capture_in_conversation(&client, "peer-b", "row-b");
    let mut mapping_a = observation("peer-a", 1);
    mapping_a.google_conversation_id = "google-a".into();
    let mut mapping_b = observation("peer-b", 1);
    mapping_b.google_conversation_id = "google-b".into();
    assert!(matches!(
        client.verify_rcs_mapping(&mapping_a, 1_000).unwrap(),
        RcsMappingOutcome::Mapped(_)
    ));
    assert!(matches!(
        client.verify_rcs_mapping(&mapping_b, 1_000).unwrap(),
        RcsMappingOutcome::Mapped(_)
    ));

    let mut conflict = observation("peer-a", 2);
    conflict.google_conversation_id = "google-b".into();
    assert_eq!(
        client.verify_rcs_mapping(&conflict, 1_000).unwrap(),
        RcsMappingOutcome::Refused(RcsMappingRefusal::MappingConflict)
    );
    assert!(client.rcs_mapping(conversation_a).unwrap().is_none());
    assert!(client.rcs_mapping(conversation_b).unwrap().is_none());

    let mut refreshed = mapping_a.clone();
    refreshed.mapping_revision = 3;
    refreshed.session_generation = 8;
    refreshed.context_revision = 2;
    refreshed.carrier_binding.route_context_revision = 2;
    refreshed.native_evidence.lifetime.session_generation = 8;
    refreshed.native_evidence.lifetime.context_revision = 2;
    assert!(matches!(
        client.verify_rcs_mapping(&refreshed, 1_000).unwrap(),
        RcsMappingOutcome::Mapped(_)
    ));
    assert!(client.rcs_mapping(conversation_b).unwrap().is_none());

    let mut delayed = mapping_b.clone();
    delayed.mapping_revision = 2;
    delayed.context_revision = 1;
    assert_eq!(
        client.verify_rcs_mapping(&delayed, 1_000).unwrap(),
        RcsMappingOutcome::Refused(RcsMappingRefusal::RetiredContext)
    );

    let mut terminal = refreshed;
    terminal.google_conversation_id.clear();
    terminal.link_state = RcsLinkState::Unlinked;
    terminal.session_generation = 9;
    assert!(matches!(
        client.verify_rcs_mapping(&terminal, 1_000).unwrap(),
        RcsMappingOutcome::Refused(_)
    ));
    assert!(client.rcs_mapping(conversation_a).unwrap().is_none());
}

#[test]
fn empty_participants_do_not_create_or_change_a_mapping_candidate() {
    let (_directory, _config, client, _conversation_id) = client_with_participant("raw-peer");
    let mut mixed = observation("raw-peer", 1);
    mixed.participants.insert(
        0,
        RcsParticipantObservation {
            identity: "  ".into(),
            google_participant_id: "blank".into(),
            identifier_type: 1,
            is_visible: true,
            is_me: false,
        },
    );
    assert_eq!(
        client.verify_rcs_mapping(&mixed, 1_000).unwrap(),
        RcsMappingOutcome::Refused(RcsMappingRefusal::InvalidIdentity)
    );
    let mut empty = mixed;
    empty.mapping_revision = 2;
    empty.participants = vec![RcsParticipantObservation {
        identity: String::new(),
        google_participant_id: "empty".into(),
        identifier_type: 1,
        is_visible: true,
        is_me: false,
    }];
    assert_eq!(
        client.verify_rcs_mapping(&empty, 1_000).unwrap(),
        RcsMappingOutcome::Refused(RcsMappingRefusal::InvalidIdentity)
    );
    let mut malformed = observation("raw-peer", 3);
    malformed.participants.push(RcsParticipantObservation {
        identity: "+not-an-identity".into(),
        google_participant_id: "malformed".into(),
        identifier_type: 1,
        is_visible: true,
        is_me: false,
    });
    assert_eq!(
        client.verify_rcs_mapping(&malformed, 1_000).unwrap(),
        RcsMappingOutcome::Refused(RcsMappingRefusal::InvalidIdentity)
    );
}

#[test]
fn ineligible_route_context_is_refused() {
    let (_directory, _config, client, _conversation_id) = client_with_participant("raw-peer");
    let mut malformed_route = observation("raw-peer", 1);
    malformed_route.selected_sim_id = "google-outgoing".into();
    assert_eq!(
        client.verify_rcs_mapping(&malformed_route, 1_000).unwrap(),
        RcsMappingOutcome::Refused(RcsMappingRefusal::IneligibleConversation)
    );
}

#[test]
fn malformed_or_multiple_google_observation_revokes_a_stored_mapping() {
    let (_directory, _config, client, conversation_id) = client_with_participant("raw-peer");
    assert!(matches!(
        client
            .verify_rcs_mapping(&observation("raw-peer", 1), 1_000)
            .unwrap(),
        RcsMappingOutcome::Mapped(_)
    ));
    let mut malformed = observation("raw-peer", 2);
    malformed.participants.push(RcsParticipantObservation {
        identity: "+not-an-identity".into(),
        google_participant_id: "malformed-google-peer".into(),
        identifier_type: 1,
        is_visible: true,
        is_me: false,
    });
    assert_eq!(
        client.verify_rcs_mapping(&malformed, 1_000).unwrap(),
        RcsMappingOutcome::Refused(RcsMappingRefusal::InvalidIdentity)
    );
    assert!(client.rcs_mapping(conversation_id).unwrap().is_none());

    let fresh = observation("raw-peer", 3);
    assert!(matches!(
        client.verify_rcs_mapping(&fresh, 1_000).unwrap(),
        RcsMappingOutcome::Mapped(_)
    ));
    let mut multiple = observation("raw-peer", 4);
    multiple.google_candidate_count = 2;
    assert_eq!(
        client.verify_rcs_mapping(&multiple, 1_000).unwrap(),
        RcsMappingOutcome::Refused(RcsMappingRefusal::GoogleConversationAmbiguous)
    );
    assert!(client.rcs_mapping(conversation_id).unwrap().is_none());
}

#[test]
fn self_chat_observations_are_refused_without_a_mapping() {
    let (_directory, _config, client, conversation_id) = client_with_participant("+12025550100");
    let mut original = observation("+12025550100", 1);
    original.google_conversation_id = "google-a".into();
    original.participants[0].is_me = true;
    assert_eq!(
        client.verify_rcs_mapping(&original, 1_000).unwrap(),
        RcsMappingOutcome::Refused(RcsMappingRefusal::SelfChatUnsupported)
    );
    let mut conflicting = original.clone();
    conflicting.google_conversation_id = "google-b".into();
    conflicting.mapping_revision = 2;
    assert_eq!(
        client.verify_rcs_mapping(&conflicting, 1_000).unwrap(),
        RcsMappingOutcome::Refused(RcsMappingRefusal::SelfChatUnsupported)
    );
    assert!(client.rcs_mapping(conversation_id).unwrap().is_none());
}

#[test]
fn older_mapping_revision_preserves_newer_authority() {
    let (_directory, _config, client, conversation_id) = client_with_participant("raw-peer");
    let newer = observation("raw-peer", 2);
    let RcsMappingOutcome::Mapped(mapping) = client.verify_rcs_mapping(&newer, 1_000).unwrap()
    else {
        panic!("expected newer mapping");
    };

    assert_eq!(
        client
            .verify_rcs_mapping(&observation("raw-peer", 1), 1_000)
            .unwrap(),
        RcsMappingOutcome::Refused(RcsMappingRefusal::NonMonotonicRevision)
    );
    assert_eq!(client.rcs_mapping(conversation_id).unwrap(), Some(mapping));
}

#[test]
fn settings_continuity_and_generation_evidence_fail_closed_without_a_receipt_ttl() {
    for mutation in 0..8 {
        let (_directory, _config, client, _conversation_id) = client_with_participant("raw-peer");
        let mut current = observation("raw-peer", 1);
        match mutation {
            0 => current.native_evidence.settings_observed_at_ms = 0,
            1 => current.native_evidence.settings_observed_at_ms = 1_001,
            2 => current.native_evidence.lifetime.link_generation = 0,
            3 => current.native_evidence.lifetime.session_generation = 0,
            4 => current.native_evidence.lifetime.run_generation = 0,
            5 => current.native_evidence.settings_stream_epoch = 0,
            6 => current.native_evidence.settings_revision = 0,
            7 => current.native_evidence.settings_currentness = NativeSettingsCurrentness::Unproven,
            _ => unreachable!(),
        }
        assert_eq!(
            client.verify_rcs_mapping(&current, 1_000).unwrap(),
            RcsMappingOutcome::Refused(RcsMappingRefusal::IneligibleConversation)
        );
    }

    let (_directory, _config, client, _conversation_id) = client_with_participant("raw-peer");
    let mut old_receipt = observation("raw-peer", 1);
    old_receipt.native_evidence.settings_observed_at_ms = 1;
    assert!(matches!(
        client.verify_rcs_mapping(&old_receipt, 1_000).unwrap(),
        RcsMappingOutcome::Mapped(_)
    ));
}

#[test]
fn native_prepare_observation_rejects_unknown_fields() {
    let observation = observation("raw-peer", 1);
    let mut value = serde_json::to_value(&observation).unwrap();
    value
        .as_object_mut()
        .unwrap()
        .insert("unexpected".into(), true.into());
    assert!(serde_json::from_value::<RcsMappingObservation>(value).is_err());

    let schema = restricted_native_rcs_handoff_schema();
    assert!(schema["prepare_request"].is_object());
    assert!(schema["prepare_observation"].is_object());
    assert!(schema["commit_projection"].is_object());
}

fn encrypted_mapping_connection(config: &ClientConfig) -> rusqlite::Connection {
    let connection = rusqlite::Connection::open(&config.database_path).unwrap();
    connection
        .execute_batch(
            "PRAGMA key = \"x'0707070707070707070707070707070707070707070707070707070707070707'\";",
        )
        .unwrap();
    connection
}

#[test]
fn legacy_context_requires_a_fresh_v2_observation_and_preserves_revision_fences() {
    let (_directory, config, client, conversation_id) = client_with_participant("raw-peer");
    assert!(matches!(
        client
            .verify_rcs_mapping(&observation("raw-peer", 1), 1_000)
            .unwrap(),
        RcsMappingOutcome::Mapped(_)
    ));
    drop(client);

    let connection = encrypted_mapping_connection(&config);
    connection
        .execute(
            "UPDATE metadata SET v=? WHERE k='rcs_mapping_context_v1'",
            [r#"{"link_generation":1,"session_generation":1,"context_revision":1}"#],
        )
        .unwrap();
    drop(connection);

    let client = open(&config);
    let mut retired = observation("raw-peer", 2);
    retired.context_revision = 0;
    retired.native_evidence.lifetime.context_revision = 0;
    retired.carrier_binding.route_context_revision = 0;
    assert_eq!(
        client.verify_rcs_mapping(&retired, 1_000).unwrap(),
        RcsMappingOutcome::Refused(RcsMappingRefusal::RetiredContext)
    );
    assert!(client.rcs_mapping(conversation_id).unwrap().is_some());

    let mut fresh = observation("raw-peer", 2);
    fresh.context_revision = 2;
    fresh.native_evidence.lifetime.context_revision = 2;
    fresh.carrier_binding.route_context_revision = 2;
    let RcsMappingOutcome::Mapped(mapping) = client.verify_rcs_mapping(&fresh, 1_000).unwrap()
    else {
        panic!("fresh v2 observation should replace the legacy context");
    };
    assert_eq!(mapping.mapping_revision, 2);

    let mut replay = observation("raw-peer", 1);
    replay.context_revision = 1;
    replay.native_evidence.lifetime.context_revision = 1;
    replay.carrier_binding.route_context_revision = 1;
    assert_eq!(
        client.verify_rcs_mapping(&replay, 1_000).unwrap(),
        RcsMappingOutcome::Refused(RcsMappingRefusal::RetiredContext)
    );
}

#[test]
fn same_revision_payload_contradiction_invalidates_old_mapping_authority() {
    let (_directory, _config, client, conversation_id) = client_with_participant("raw-peer");
    assert!(matches!(
        client
            .verify_rcs_mapping(&observation("raw-peer", 1), 1_000)
            .unwrap(),
        RcsMappingOutcome::Mapped(_)
    ));
    let mut contradictory = observation("raw-peer", 2);
    contradictory.mapping_revision = 2;
    contradictory.outgoing.sim_payload.two += 1;

    assert_eq!(
        client.verify_rcs_mapping(&contradictory, 1_000).unwrap(),
        RcsMappingOutcome::Refused(RcsMappingRefusal::RetiredContext)
    );
    assert!(client.rcs_mapping(conversation_id).unwrap().is_none());
}

#[test]
fn initial_mapping_requires_one_sim_participant_id_join_before_card_validation() {
    let (_directory, _config, client, _conversation_id) = client_with_participant("raw-peer");
    let mut mismatched = observation("raw-peer", 1);
    mismatched.outgoing.outgoing_id = "conversation-outgoing".into();
    mismatched.native_evidence.current_settings_sims[0].outgoing_id =
        "conversation-outgoing".into();
    assert_eq!(
        client.verify_rcs_mapping(&mismatched, 1_000).unwrap(),
        RcsMappingOutcome::Refused(RcsMappingRefusal::IneligibleConversation)
    );

    let (_directory, _config, client, _conversation_id) = client_with_participant("raw-peer");
    let mut duplicate = observation("raw-peer", 1);
    let mut other = duplicate.native_evidence.current_settings_sims[0].clone();
    other.sim_payload.sim_number += 1;
    other.own_identity_raw = "+12025550101".into();
    other.rcs_chats_enabled = false;
    duplicate.native_evidence.current_settings_sims.push(other);
    assert_eq!(
        client.verify_rcs_mapping(&duplicate, 1_000).unwrap(),
        RcsMappingOutcome::Refused(RcsMappingRefusal::IneligibleConversation)
    );
}

#[test]
fn bootstrap_resolution_uses_the_same_canonical_lookup_as_verify() {
    let (_directory, _config, client, conversation_id) = client_with_participant("raw-peer");
    let google_id = observation("raw-peer", 1).google_conversation_id;
    assert_eq!(
        client
            .resolve_rcs_bootstrap_conversation("raw-peer", &google_id)
            .unwrap(),
        RcsBootstrapResolution::Resolved { conversation_id }
    );
    assert!(matches!(
        client
            .verify_rcs_mapping(&observation("raw-peer", 1), 1_000)
            .unwrap(),
        RcsMappingOutcome::Mapped(mapping) if mapping.conversation_id == conversation_id
    ));

    let international = format!("+{}{}", "1202", "5550123");
    let formatted = format!("+{} {}", "1", "202 555 0123");
    let (_directory, _config, client, conversation_id) = client_with_participant(&formatted);
    assert_eq!(
        client
            .resolve_rcs_bootstrap_conversation(&international, &google_id)
            .unwrap(),
        RcsBootstrapResolution::Resolved { conversation_id }
    );
}

#[test]
fn bootstrap_resolution_refuses_missing_ambiguous_invalid_and_conflicting_candidates() {
    let (_directory, _config, client, _conversation_id) = client_with_participant("2025550123");
    let google_id = observation("raw-peer", 1).google_conversation_id;
    let international = format!("+{}{}", "1202", "5550123");
    assert_eq!(
        client
            .resolve_rcs_bootstrap_conversation(&international, &google_id)
            .unwrap(),
        RcsBootstrapResolution::Missing
    );
    assert_eq!(
        client
            .resolve_rcs_bootstrap_conversation("   ", &google_id)
            .unwrap(),
        RcsBootstrapResolution::InvalidIdentity
    );

    capture_in_conversation(&client, "raw-dup", "dup-a");
    capture_in_conversation(&client, "raw-dup", "dup-b");
    assert_eq!(
        client
            .resolve_rcs_bootstrap_conversation("raw-dup", &google_id)
            .unwrap(),
        RcsBootstrapResolution::Ambiguous
    );

    let (_directory, _config, client, _conversation_id) = client_with_participant("raw-peer");
    assert!(matches!(
        client
            .verify_rcs_mapping(&observation("raw-peer", 1), 1_000)
            .unwrap(),
        RcsMappingOutcome::Mapped(_)
    ));
    assert_eq!(
        client
            .resolve_rcs_bootstrap_conversation("raw-peer", "another-google-conversation")
            .unwrap(),
        RcsBootstrapResolution::AlreadyMappedElsewhere
    );
    capture_in_conversation(&client, "raw-other", "other-row");
    assert_eq!(
        client
            .resolve_rcs_bootstrap_conversation("raw-other", &google_id)
            .unwrap(),
        RcsBootstrapResolution::AlreadyMappedElsewhere
    );
}

#[test]
fn bootstrap_batch_matches_single_candidate_resolution_in_input_order() {
    let (_directory, _config, client, conv_e164) = client_with_participant("+1 202 555 0123");
    let conv_nat = capture_in_conversation(&client, "2025550123", "nat-row");
    let conv_raw = capture_in_conversation(&client, "raw-peer", "peer-row");
    capture_in_conversation(&client, "raw-dup", "dup-a");
    capture_in_conversation(&client, "raw-dup", "dup-b");
    assert!(matches!(
        client
            .verify_rcs_mapping(&observation("raw-peer", 1), 1_000)
            .unwrap(),
        RcsMappingOutcome::Mapped(mapping) if mapping.conversation_id == conv_raw
    ));
    capture_in_conversation(&client, "raw-other", "other-row");

    let google = "google-conversation";
    let unmapped = "google-unmapped";
    let candidates = [
        ("raw-other", google),
        ("raw-dup", unmapped),
        ("   ", google),
        ("+12025550199", unmapped),
        ("+12025550123", unmapped),
        ("2025550123", unmapped),
        ("raw-peer", "another-google-conversation"),
        ("raw-peer", google),
    ];
    let expected = [
        RcsBootstrapResolution::AlreadyMappedElsewhere,
        RcsBootstrapResolution::Ambiguous,
        RcsBootstrapResolution::InvalidIdentity,
        RcsBootstrapResolution::Missing,
        RcsBootstrapResolution::Resolved {
            conversation_id: conv_e164,
        },
        RcsBootstrapResolution::Resolved {
            conversation_id: conv_nat,
        },
        RcsBootstrapResolution::AlreadyMappedElsewhere,
        RcsBootstrapResolution::Resolved {
            conversation_id: conv_raw,
        },
    ];
    let batch = client
        .resolve_rcs_bootstrap_conversations(&candidates)
        .unwrap();
    assert_eq!(batch, expected);
    for ((identity, google_id), resolution) in candidates.iter().zip(&batch) {
        assert_eq!(
            client
                .resolve_rcs_bootstrap_conversation(identity, google_id)
                .unwrap(),
            *resolution
        );
    }
}

#[test]
fn bootstrap_batch_accepts_forty_and_rejects_forty_one_candidates() {
    let (_directory, _config, client, _conversation_id) = client_with_participant("raw-peer");
    let google_id = observation("raw-peer", 1).google_conversation_id;
    let candidate = ("raw-peer", google_id.as_str());
    assert_eq!(
        client
            .resolve_rcs_bootstrap_conversations(&[candidate; 40])
            .unwrap()
            .len(),
        40
    );
    assert_eq!(
        client.resolve_rcs_bootstrap_conversations(&[candidate; 41]),
        Err(Error::InvalidRequest("too many bootstrap candidates"))
    );
}
