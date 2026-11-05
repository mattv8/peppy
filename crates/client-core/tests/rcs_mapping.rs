use super::common::*;

fn observation(participant: &str, revision: u64) -> RcsMappingObservation {
    RcsMappingObservation {
        google_conversation_id: "google-conversation".into(),
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
        selected_sim_id: "sim-primary".into(),
        selected_sim_own_identity: "local-identity".into(),
        identity_region: "ZZ".into(),
        observed_at_ms: 1_000,
        link_generation: 4,
        session_generation: 7,
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
    changed.selected_sim_own_identity = "other-local-identity".into();
    changed.read_only = true;
    assert_eq!(
        client.verify_rcs_mapping(&changed, 1_000).unwrap(),
        RcsMappingOutcome::Refused(RcsMappingRefusal::IneligibleConversation)
    );
    assert!(client.rcs_mapping(conversation_id).unwrap().is_none());

    let mut refreshed = observation("raw-peer", 3);
    refreshed.identity_region = "XY".into();
    refreshed.selected_sim_own_identity = "other-local-identity".into();
    let RcsMappingOutcome::Mapped(refreshed) =
        client.verify_rcs_mapping(&refreshed, 1_000).unwrap()
    else {
        panic!("expected refreshed mapping");
    };
    assert_ne!(first.proof.digest, refreshed.proof.digest);
}

#[test]
fn self_chat_and_nonmonotonic_revision_are_explicit() {
    let (_directory, _config, client, conversation_id) = client_with_participant("local-identity");
    let mut self_chat = observation("local-identity", 1);
    self_chat.participants[0].is_me = true;
    let RcsMappingOutcome::Mapped(mapping) = client.verify_rcs_mapping(&self_chat, 1_000).unwrap()
    else {
        panic!("expected self mapping");
    };
    assert_eq!(mapping.class, RcsMappingClass::SelfChat);
    assert_eq!(mapping.conversation_id, conversation_id);
    assert_eq!(
        client.verify_rcs_mapping(&self_chat, 1_000).unwrap(),
        RcsMappingOutcome::Refused(RcsMappingRefusal::NonMonotonicRevision)
    );
}
