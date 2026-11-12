use super::common::*;
use peppy_crypto::{KeyPurpose, derive_purpose_key, derive_root_key, encrypt};
use rusqlite::Connection;
use std::collections::HashSet;

fn sealed(vault: &Vault, producer: DeviceId, sequence: u64, payload: PrivatePayload) -> Envelope {
    let root = derive_root_key(PASSPHRASE, &vault.profile).unwrap();
    let key = derive_purpose_key(&root, &vault.profile, KeyPurpose::Event).unwrap();
    let mut envelope = Envelope {
        protocol_version: 1,
        envelope_id: EnvelopeId::new(),
        command_id: None,
        vault_id: vault.id,
        producer_device_id: producer,
        producer_sequence: SourceSequence(sequence),
        key_epoch: 1,
        crypto_suite: vault.profile.crypto_suite,
        profile_fingerprint: vault.profile.fingerprint().unwrap(),
        purpose: EnvelopePurpose::Event,
        route: None,
        compaction: None,
        origin_tag: None,
        ciphertext: Vec::new(),
    };
    let plain = serde_json::to_vec(&payload).unwrap();
    let encrypted = encrypt(&key, &envelope.aad_bytes().unwrap(), &plain).unwrap();
    envelope.ciphertext = [encrypted.nonce.as_slice(), &encrypted.ciphertext].concat();
    envelope
}

fn status(gateway_device_id: DeviceId, revision: u64, observed_at_ms: i64) -> RcsLinkStatusEvent {
    RcsLinkStatusEvent {
        gateway_device_id,
        state: RcsCapabilityState::Linked,
        availability: true,
        health: true,
        phone_responsive: true,
        observed_at_ms,
        capability_revision: revision,
        feature_bits: 1,
        linked_since_ms: Some(observed_at_ms),
    }
}

fn eligibility(
    gateway_device_id: DeviceId,
    conversation_id: ConversationId,
    revision: u64,
    observed_at_ms: i64,
) -> RcsEligibilityProjection {
    RcsEligibilityProjection {
        conversation_id,
        eligibility: RcsEligibility::Eligible,
        feature_bits: 1,
        mapping_revision: revision,
        observed_at_ms,
        gateway_device_id,
    }
}

fn mapping_observation(peer: &str) -> RcsMappingObservation {
    RcsMappingObservation {
        google_conversation_id: "google-conversation".into(),
        google_candidate_count: 1,
        participants: vec![RcsParticipantObservation {
            identity: peer.into(),
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

#[test]
fn payloads_are_strict_and_content_free() {
    let status = serde_json::json!({
        "kind": "rcs_link_status",
        "status": {
            "gateway_device_id": DeviceId::new(), "state": "linked", "availability": true,
            "health": true, "phone_responsive": true, "observed_at_ms": 1,
            "capability_revision": 1, "feature_bits": 0
        }
    });
    let eligibility = serde_json::json!({
        "kind": "rcs_link_eligibility",
        "eligibility": {
            "conversation_id": ConversationId::new(), "eligibility": "eligible", "feature_bits": 0,
            "mapping_revision": 1, "observed_at_ms": 1, "gateway_device_id": DeviceId::new()
        }
    });
    for valid in [status, eligibility] {
        assert!(serde_json::from_value::<PrivatePayload>(valid.clone()).is_ok());
        for forbidden in ["google_id", "phone", "name", "token", "stop_code"] {
            let mut invalid = valid.clone();
            invalid
                .as_object_mut()
                .unwrap()
                .insert(forbidden.into(), serde_json::json!("x"));
            assert!(serde_json::from_value::<PrivatePayload>(invalid).is_err());
        }
    }
}

#[test]
fn publisher_is_bounded_current_and_expires_stale_projections() {
    let directory = tempfile::tempdir().unwrap();
    let vault = Vault::new();
    let config = config(&directory, "rcs-capability", &vault);
    let client = unlocked(&config, &vault);
    let conversation = ConversationId::new();
    let status = status(config.device_id, 1, 1_000);
    let projection = eligibility(config.device_id, conversation, 1, 1_000);

    assert_eq!(
        client
            .publish_rcs_capability(&status, std::slice::from_ref(&projection), 1_000)
            .unwrap(),
        2
    );
    assert!(
        client
            .rcs_gateway_supported(config.device_id, 1_001)
            .unwrap()
    );
    assert_eq!(
        client
            .publish_rcs_capability(&status, std::slice::from_ref(&projection), 1_001)
            .unwrap(),
        0
    );
    assert_eq!(
        client
            .publish_rcs_capability(
                &RcsLinkStatusEvent {
                    capability_revision: 2,
                    observed_at_ms: 1_000 + RCS_CAPABILITY_KEEPALIVE_MS,
                    linked_since_ms: Some(1_000),
                    ..status
                },
                &[projection],
                1_000 + RCS_CAPABILITY_KEEPALIVE_MS,
            )
            .unwrap(),
        2
    );
    assert_eq!(
        client
            .rcs_eligibility_projection(
                conversation,
                config.device_id,
                1_000 + RCS_CAPABILITY_STALE_MS + 1
            )
            .unwrap()
            .unwrap()
            .eligibility,
        RcsEligibility::Eligible
    );
    assert_eq!(
        client
            .rcs_eligibility_projection(
                conversation,
                config.device_id,
                1_000 + RCS_CAPABILITY_KEEPALIVE_MS + RCS_CAPABILITY_STALE_MS + 1,
            )
            .unwrap()
            .unwrap()
            .eligibility,
        RcsEligibility::Unknown
    );
}

#[test]
fn old_eligibility_stays_current_until_the_gateway_status_expires() {
    let directory = tempfile::tempdir().unwrap();
    let vault = Vault::new();
    let config = config(&directory, "rcs-capability-old-eligibility", &vault);
    let client = unlocked(&config, &vault);
    let conversation = ConversationId::new();
    let eligibility_observed_at_ms = 1_000;
    let status_observed_at_ms = eligibility_observed_at_ms + 24 * 60 * 60 * 1_000;
    let mut link_status = status(config.device_id, 2, status_observed_at_ms);
    link_status.linked_since_ms = Some(eligibility_observed_at_ms);

    client
        .publish_rcs_capability(
            &status(config.device_id, 1, eligibility_observed_at_ms),
            &[eligibility(
                config.device_id,
                conversation,
                1,
                eligibility_observed_at_ms,
            )],
            eligibility_observed_at_ms,
        )
        .unwrap();
    client
        .publish_rcs_capability(&link_status, &[], status_observed_at_ms)
        .unwrap();

    assert_eq!(
        client
            .rcs_eligibility_projection(
                conversation,
                config.device_id,
                status_observed_at_ms + RCS_CAPABILITY_STALE_MS,
            )
            .unwrap()
            .unwrap()
            .eligibility,
        RcsEligibility::Eligible
    );
    assert_eq!(
        client
            .rcs_eligibility_projection(
                conversation,
                config.device_id,
                status_observed_at_ms + RCS_CAPABILITY_STALE_MS + 1,
            )
            .unwrap()
            .unwrap()
            .eligibility,
        RcsEligibility::Unknown
    );
}

#[test]
fn future_dated_eligibility_is_unknown_while_gateway_is_current() {
    let directory = tempfile::tempdir().unwrap();
    let vault = Vault::new();
    let conversation = ConversationId::new();
    let now_ms = 1_000;

    let producer = DeviceId::new();
    let peer_config = config(&directory, "rcs-capability-future-eligibility-peer", &vault);
    let peer = unlocked(&peer_config, &vault);
    for (sequence, payload) in [
        PrivatePayload::RcsLinkStatus(RcsLinkStatusPayload {
            status: status(producer, 1, now_ms),
        }),
        PrivatePayload::RcsLinkEligibility(RcsLinkEligibilityPayload {
            eligibility: eligibility(producer, conversation, 1, now_ms + 1),
        }),
    ]
    .into_iter()
    .enumerate()
    {
        assert_eq!(
            peer.ingest(
                &sealed(&vault, producer, sequence as u64 + 1, payload),
                Cursor(sequence as u64 + 1)
            )
            .unwrap(),
            IngestResult::Journaled
        );
    }
    assert_eq!(peer.apply_pending(10).unwrap().applied, 2);

    assert_eq!(
        peer.rcs_eligibility_projection(conversation, producer, now_ms)
            .unwrap()
            .unwrap()
            .eligibility,
        RcsEligibility::Unknown
    );
}

#[test]
fn replacement_events_self_supersede_per_gateway_and_conversation() {
    let directory = tempfile::tempdir().unwrap();
    let vault = Vault::new();
    let config = config(&directory, "rcs-capability-compact", &vault);
    let client = unlocked(&config, &vault);
    let conversation = ConversationId::new();
    let first = eligibility(config.device_id, conversation, 1, 1_000);
    client
        .publish_rcs_capability(&status(config.device_id, 1, 1_000), &[first], 1_000)
        .unwrap();
    let first = client
        .pending_outbox()
        .unwrap()
        .into_iter()
        .find(|event| {
            event
                .compaction
                .as_ref()
                .is_some_and(|metadata| !metadata.checkpoint)
                && event.producer_sequence.0 > 0
        })
        .unwrap();
    let second = eligibility(config.device_id, conversation, 2, 2_000);
    client
        .publish_rcs_capability(&status(config.device_id, 2, 2_000), &[second], 2_000)
        .unwrap();
    assert!(client.pending_outbox().unwrap().iter().any(|event| {
        event.compaction.as_ref().is_some_and(|metadata| {
            metadata
                .supersedes
                .iter()
                .any(|reference| reference.producer_sequence == first.producer_sequence)
        })
    }));
}

#[test]
fn invalid_sql_revisions_and_future_observations_never_grant_capability() {
    let directory = tempfile::tempdir().unwrap();
    let vault = Vault::new();
    let config = config(&directory, "rcs-capability-bounds", &vault);
    let client = unlocked(&config, &vault);
    let conversation = ConversationId::new();
    let mut oversized = status(config.device_id, u64::MAX, 1_000);
    assert!(matches!(
        client.publish_rcs_capability(&oversized, &[], 1_000),
        Err(Error::InvalidRequest(_))
    ));
    assert!(client.pending_outbox().unwrap().is_empty());

    oversized.capability_revision = 1;
    oversized.observed_at_ms = i64::MAX;
    assert!(
        client
            .publish_rcs_capability(&oversized, &[], 1_000)
            .is_err()
    );

    let status = status(config.device_id, 2, 1_000);
    let projection = eligibility(config.device_id, conversation, 2, 1_000);
    client
        .publish_rcs_capability(&status, &[projection], 1_000)
        .unwrap();
    assert!(
        !client
            .rcs_gateway_supported(config.device_id, i64::MAX)
            .unwrap()
    );
    assert_eq!(
        client
            .rcs_eligibility_projection(conversation, config.device_id, i64::MAX)
            .unwrap()
            .unwrap()
            .eligibility,
        RcsEligibility::Unknown
    );
}

#[test]
fn eligibility_requires_a_current_linked_gateway() {
    let directory = tempfile::tempdir().unwrap();
    let vault = Vault::new();
    let config = config(&directory, "rcs-capability-unlinked", &vault);
    let client = unlocked(&config, &vault);
    let conversation = ConversationId::new();
    client
        .publish_rcs_capability(
            &status(config.device_id, 1, 1_000),
            &[eligibility(config.device_id, conversation, 1, 1_000)],
            1_000,
        )
        .unwrap();
    let mut unlinked = status(config.device_id, 2, 1_001);
    unlinked.state = RcsCapabilityState::Unlinked;
    client
        .publish_rcs_capability(&unlinked, &[], 1_001)
        .unwrap();
    assert_eq!(
        client
            .rcs_eligibility_projection(conversation, config.device_id, 1_001)
            .unwrap()
            .unwrap()
            .eligibility,
        RcsEligibility::Unknown
    );
}

#[test]
fn relink_without_reproof_keeps_old_eligibility_unknown() {
    let directory = tempfile::tempdir().unwrap();
    let vault = Vault::new();
    let source_config = config(&directory, "rcs-capability-relink-source", &vault);
    let source = unlocked(&source_config, &vault);
    let gateway = source_config.device_id;
    let conversation = ConversationId::new();
    let mut server = Server::default();

    source
        .publish_rcs_capability(
            &status(gateway, 1, 1_000),
            &[eligibility(gateway, conversation, 1, 1_000)],
            1_000,
        )
        .unwrap();
    assert_eq!(server.upload(&source), 2);
    let mut unlinked = status(gateway, 2, 1_001);
    unlinked.state = RcsCapabilityState::Unlinked;
    unlinked.availability = false;
    source
        .publish_rcs_capability(&unlinked, &[], 1_001)
        .unwrap();
    assert_eq!(server.upload(&source), 1);
    source
        .publish_rcs_capability(&status(gateway, 3, 1_002), &[], 1_002)
        .unwrap();
    assert_eq!(server.upload(&source), 1);
    assert_eq!(
        source
            .rcs_eligibility_projection(conversation, gateway, 1_002)
            .unwrap()
            .unwrap()
            .eligibility,
        RcsEligibility::Unknown
    );

    let replay_config = config(&directory, "rcs-capability-relink-replay", &vault);
    let replay = unlocked(&replay_config, &vault);
    for (index, envelope) in server.log.iter().enumerate() {
        assert_eq!(
            replay.ingest(envelope, Cursor(index as u64 + 1)).unwrap(),
            IngestResult::Journaled
        );
    }
    assert_eq!(settle(&replay).quarantined, 0);
    assert_eq!(
        replay
            .rcs_eligibility_projection(conversation, gateway, 1_002)
            .unwrap()
            .unwrap()
            .eligibility,
        RcsEligibility::Unknown
    );

    let superseded = server
        .log
        .iter()
        .filter_map(|envelope| envelope.compaction.as_ref())
        .flat_map(|metadata| metadata.supersedes.iter())
        .map(|reference| (reference.producer_device_id, reference.producer_sequence.0))
        .collect::<HashSet<_>>();
    let snapshot = server
        .log
        .iter()
        .enumerate()
        .filter(|(_, envelope)| {
            !superseded.contains(&(envelope.producer_device_id, envelope.producer_sequence.0))
        })
        .map(|(index, envelope)| SnapshotRecord {
            cursor: Cursor(index as u64 + 1),
            envelope: envelope.clone(),
        })
        .collect::<Vec<_>>();
    assert_eq!(
        snapshot.len(),
        2,
        "latest status plus old eligibility retained"
    );

    let target_config = config(&directory, "rcs-capability-relink-snapshot", &vault);
    let target = unlocked(&target_config, &vault);
    let session = target
        .begin_snapshot_with_compaction(
            Cursor(server.log.len() as u64),
            snapshot.len() as u64,
            SnapshotPurpose::Resync,
            Some(1),
        )
        .unwrap();
    target
        .append_snapshot_page(session.generation, &snapshot)
        .unwrap();
    target.finish_snapshot(session.generation).unwrap();
    assert_eq!(settle(&target).quarantined, 0);
    assert_eq!(
        target
            .rcs_eligibility_projection(conversation, gateway, 1_002)
            .unwrap()
            .unwrap()
            .eligibility,
        RcsEligibility::Unknown
    );
    drop(target);

    let reopened = unlocked(&target_config, &vault);
    assert_eq!(
        reopened
            .rcs_eligibility_projection(conversation, gateway, 1_002)
            .unwrap()
            .unwrap()
            .eligibility,
        RcsEligibility::Unknown
    );
}

#[test]
fn compacted_import_with_omitted_relink_fences_older_eligibility() {
    let directory = tempfile::tempdir().unwrap();
    let vault = Vault::new();
    let source_config = config(&directory, "rcs-capability-existing-source", &vault);
    let source = unlocked(&source_config, &vault);
    let gateway = source_config.device_id;
    let conversation = ConversationId::new();
    let mut server = Server::default();

    source
        .publish_rcs_capability(
            &status(gateway, 1, 1_000),
            &[eligibility(gateway, conversation, 1, 1_000)],
            1_000,
        )
        .unwrap();
    assert_eq!(server.upload(&source), 2);

    let target_config = config(&directory, "rcs-capability-existing-target", &vault);
    let target = unlocked(&target_config, &vault);
    for (index, envelope) in server.log.iter().enumerate() {
        target.ingest(envelope, Cursor(index as u64 + 1)).unwrap();
    }
    assert_eq!(settle(&target).quarantined, 0);

    let mut unlinked = status(gateway, 2, 1_001);
    unlinked.state = RcsCapabilityState::Unlinked;
    unlinked.availability = false;
    source
        .publish_rcs_capability(&unlinked, &[], 1_001)
        .unwrap();
    source
        .publish_rcs_capability(&status(gateway, 3, 1_002), &[], 1_002)
        .unwrap();
    assert_eq!(server.upload(&source), 2);

    let superseded = server
        .log
        .iter()
        .filter_map(|envelope| envelope.compaction.as_ref())
        .flat_map(|metadata| metadata.supersedes.iter())
        .map(|reference| (reference.producer_device_id, reference.producer_sequence.0))
        .collect::<HashSet<_>>();
    let snapshot = server
        .log
        .iter()
        .enumerate()
        .filter(|(_, envelope)| {
            !superseded.contains(&(envelope.producer_device_id, envelope.producer_sequence.0))
        })
        .map(|(index, envelope)| SnapshotRecord {
            cursor: Cursor(index as u64 + 1),
            envelope: envelope.clone(),
        })
        .collect::<Vec<_>>();
    let session = target
        .begin_snapshot_with_compaction(
            Cursor(server.log.len() as u64),
            snapshot.len() as u64,
            SnapshotPurpose::Resync,
            Some(1),
        )
        .unwrap();
    target
        .append_snapshot_page(session.generation, &snapshot)
        .unwrap();
    target.finish_snapshot(session.generation).unwrap();
    assert_eq!(settle(&target).quarantined, 0);
    assert_eq!(
        target
            .rcs_eligibility_projection(conversation, gateway, 1_002)
            .unwrap()
            .unwrap()
            .eligibility,
        RcsEligibility::Unknown
    );
}

#[test]
fn missing_fence_metadata_recovers_on_fresh_eligibility() {
    let directory = tempfile::tempdir().unwrap();
    let vault = Vault::new();
    let source_config = config(&directory, "rcs-capability-missing-fence-source", &vault);
    let source = unlocked(&source_config, &vault);
    let gateway = source_config.device_id;
    let conversation = ConversationId::new();
    let mut server = Server::default();

    source
        .publish_rcs_capability(&status(gateway, 1, 1_000), &[], 1_000)
        .unwrap();
    assert_eq!(server.upload(&source), 1);
    let target_config = config(&directory, "rcs-capability-missing-fence-target", &vault);
    let target = unlocked(&target_config, &vault);
    target.ingest(&server.log[0], Cursor(1)).unwrap();
    assert_eq!(settle(&target).quarantined, 0);

    source
        .publish_rcs_capability(
            &status(gateway, 2, 1_001),
            &[eligibility(gateway, conversation, 1, 1_001)],
            1_001,
        )
        .unwrap();
    assert_eq!(server.upload(&source), 2);
    target.ingest(&server.log[1], Cursor(2)).unwrap();
    target.ingest(&server.log[2], Cursor(3)).unwrap();
    assert_eq!(settle(&target).quarantined, 0);
    assert_eq!(
        target
            .rcs_eligibility_projection(conversation, gateway, 1_001)
            .unwrap()
            .unwrap()
            .eligibility,
        RcsEligibility::Eligible
    );
}

#[test]
fn linked_heartbeat_with_interleaved_messages_preserves_eligibility() {
    let directory = tempfile::tempdir().unwrap();
    let vault = Vault::new();
    let config = config(&directory, "rcs-capability-heartbeat", &vault);
    let client = unlocked(&config, &vault);
    let conversation = ConversationId::new();

    client
        .record_rcs_link_lifecycle(RcsCapabilityState::Linked, true, true, true, 1_000, 1_000)
        .unwrap();
    client
        .publish_rcs_capability(
            &status(config.device_id, 2, 1_000),
            &[eligibility(config.device_id, conversation, 1, 1_000)],
            1_000,
        )
        .unwrap();
    client
        .record_rcs_link_lifecycle(RcsCapabilityState::Linked, true, true, true, 1_001, 1_001)
        .unwrap();

    assert_eq!(
        client
            .rcs_eligibility_projection(conversation, config.device_id, 1_001)
            .unwrap()
            .unwrap()
            .eligibility,
        RcsEligibility::Eligible
    );
}

#[test]
fn state_change_clears_carrier_proof() {
    let directory = tempfile::tempdir().unwrap();
    let vault = Vault::new();
    let config = config(&directory, "rcs-capability-state-change", &vault);
    let client = unlocked(&config, &vault);
    let conversation = ConversationId::new();

    client
        .record_rcs_link_lifecycle(RcsCapabilityState::Linked, true, true, true, 1_000, 1_000)
        .unwrap();
    client
        .publish_rcs_capability(
            &status(config.device_id, 2, 1_000),
            &[eligibility(config.device_id, conversation, 1, 1_000)],
            1_000,
        )
        .unwrap();
    client
        .record_rcs_link_lifecycle(
            RcsCapabilityState::NeedsRelink,
            true,
            false,
            false,
            1_001,
            1_001,
        )
        .unwrap();

    assert_eq!(
        client
            .rcs_eligibility_projection(conversation, config.device_id, 1_001)
            .unwrap()
            .unwrap()
            .eligibility,
        RcsEligibility::Unknown
    );
}

#[test]
fn oversized_batch_and_future_local_evidence_leave_no_partial_events() {
    let directory = tempfile::tempdir().unwrap();
    let vault = Vault::new();
    let config = config(&directory, "rcs-capability-batch", &vault);
    let client = unlocked(&config, &vault);
    let rows = (0..RCS_CAPABILITY_PUBLISH_BATCH + 1)
        .map(|_| eligibility(config.device_id, ConversationId::new(), 1, 1_000))
        .collect::<Vec<_>>();
    assert!(matches!(
        client.publish_rcs_capability(&status(config.device_id, 1, 1_000), &rows, 1_000),
        Err(Error::InvalidRequest(_))
    ));
    assert!(client.pending_outbox().unwrap().is_empty());

    assert!(matches!(
        client.publish_rcs_capability(&status(config.device_id, 1, 1_001), &[], 1_000),
        Err(Error::InvalidRequest(_))
    ));
    assert!(client.pending_outbox().unwrap().is_empty());
}

#[test]
fn status_accessor_preserves_state_but_marks_stale_status_unknown_to_consumers() {
    let directory = tempfile::tempdir().unwrap();
    let vault = Vault::new();
    let config = config(&directory, "rcs-capability-status", &vault);
    let client = unlocked(&config, &vault);
    let mut unavailable = status(config.device_id, 1, 1_000);
    unavailable.state = RcsCapabilityState::Unavailable;
    unavailable.availability = false;
    client
        .publish_rcs_capability(&unavailable, &[], 1_000)
        .unwrap();
    let view = client
        .rcs_link_status(config.device_id, 1_001)
        .unwrap()
        .unwrap();
    assert_eq!(view.status.state, RcsCapabilityState::Unavailable);
    assert!(view.current);
    assert!(
        !client
            .rcs_gateway_supported(config.device_id, 1_001)
            .unwrap()
    );
    assert!(
        !client
            .rcs_link_status(config.device_id, 1_000 + RCS_CAPABILITY_STALE_MS + 1)
            .unwrap()
            .unwrap()
            .current
    );
}

fn journal_outcomes(config: &ClientConfig) -> Vec<(u64, String, Option<String>)> {
    let connection = Connection::open(&config.database_path).unwrap();
    connection
        .execute_batch(
            "PRAGMA key = \"x'0707070707070707070707070707070707070707070707070707070707070707'\";",
        )
        .unwrap();
    connection
        .prepare("SELECT cursor,status,reason FROM journal ORDER BY cursor")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
}

fn settle(client: &Client) -> ApplyReport {
    let mut total = ApplyReport::default();
    loop {
        let step = client.apply_pending(100).unwrap();
        total.applied += step.applied;
        total.quarantined += step.quarantined;
        total.drained += step.drained;
        total.superseded += step.superseded;
        if step.applied + step.quarantined + step.drained + step.superseded == 0
            && step.snapshot_remaining == 0
        {
            return total;
        }
    }
}

#[test]
fn invalid_authenticated_capability_events_quarantine_without_wedging_following_updates() {
    let directory = tempfile::tempdir().unwrap();
    let vault = Vault::new();
    let target_config = config(&directory, "rcs-capability-replay-target", &vault);
    let target = unlocked(&target_config, &vault);
    let producer = DeviceId::new();
    let conversation = ConversationId::new();

    let invalid = [
        sealed(
            &vault,
            producer,
            1,
            PrivatePayload::RcsLinkEligibility(RcsLinkEligibilityPayload {
                eligibility: eligibility(producer, conversation, u64::MAX, 1_000),
            }),
        ),
        sealed(
            &vault,
            producer,
            2,
            PrivatePayload::RcsLinkStatus(RcsLinkStatusPayload {
                status: status(producer, u64::MAX, 1_000),
            }),
        ),
        sealed(
            &vault,
            producer,
            i64::MAX as u64 + 1,
            PrivatePayload::RcsLinkStatus(RcsLinkStatusPayload {
                status: status(producer, 1, 1_000),
            }),
        ),
    ];
    let valid = [
        sealed(
            &vault,
            producer,
            4,
            PrivatePayload::RcsLinkStatus(RcsLinkStatusPayload {
                status: status(producer, 2, 1_004),
            }),
        ),
        sealed(
            &vault,
            producer,
            5,
            PrivatePayload::RcsLinkEligibility(RcsLinkEligibilityPayload {
                eligibility: eligibility(producer, conversation, 2, 1_004),
            }),
        ),
    ];
    for (index, envelope) in invalid.iter().chain(&valid).enumerate() {
        assert_eq!(
            target.ingest(envelope, Cursor(index as u64 + 1)).unwrap(),
            IngestResult::Journaled
        );
    }

    let report = target.apply_pending(10).unwrap();
    assert_eq!((report.applied, report.quarantined), (2, 3));
    assert_eq!(
        target.replay_progress().unwrap(),
        ReplayProgress {
            received_through: Cursor(5),
            processed_through: Cursor(5),
            waiting_for_keys: 0,
        }
    );
    assert!(target.rcs_gateway_supported(producer, 1_005).unwrap());
    assert_eq!(
        target
            .rcs_eligibility_projection(conversation, producer, 1_005)
            .unwrap()
            .unwrap()
            .eligibility,
        RcsEligibility::Eligible
    );
    drop(target);

    assert_eq!(
        journal_outcomes(&target_config),
        vec![
            (1, "quarantined".into(), Some("invalid_payload".into())),
            (2, "quarantined".into(), Some("invalid_payload".into())),
            (3, "quarantined".into(), Some("invalid_payload".into())),
            (4, "applied".into(), None),
            (5, "applied".into(), None),
        ]
    );
    let reopened = unlocked(&target_config, &vault);
    assert_eq!(reopened.quarantine_count().unwrap(), 3);
    assert_eq!(
        reopened.replay_progress().unwrap().processed_through,
        Cursor(5)
    );
    assert_eq!(reopened.apply_pending(10).unwrap(), ApplyReport::default());
    assert!(reopened.rcs_gateway_supported(producer, 1_005).unwrap());
    assert_eq!(
        reopened
            .rcs_eligibility_projection(conversation, producer, 1_005)
            .unwrap()
            .unwrap()
            .eligibility,
        RcsEligibility::Eligible
    );
}

#[test]
fn mismatched_authenticated_producer_cannot_claim_a_gateway_or_wedge_valid_updates() {
    let directory = tempfile::tempdir().unwrap();
    let vault = Vault::new();
    let target_config = config(&directory, "rcs-capability-source", &vault);
    let target = unlocked(&target_config, &vault);
    let producer = DeviceId::new();
    let claimed_gateway = DeviceId::new();
    let conversation = ConversationId::new();
    let envelopes = [
        sealed(
            &vault,
            producer,
            1,
            PrivatePayload::RcsLinkStatus(RcsLinkStatusPayload {
                status: status(claimed_gateway, 1, 1_000),
            }),
        ),
        sealed(
            &vault,
            producer,
            2,
            PrivatePayload::RcsLinkEligibility(RcsLinkEligibilityPayload {
                eligibility: eligibility(claimed_gateway, conversation, 1, 1_000),
            }),
        ),
        sealed(
            &vault,
            producer,
            3,
            PrivatePayload::RcsLinkStatus(RcsLinkStatusPayload {
                status: status(producer, 2, 1_002),
            }),
        ),
        sealed(
            &vault,
            producer,
            4,
            PrivatePayload::RcsLinkEligibility(RcsLinkEligibilityPayload {
                eligibility: eligibility(producer, conversation, 2, 1_002),
            }),
        ),
    ];
    for (index, envelope) in envelopes.iter().enumerate() {
        assert_eq!(
            target.ingest(envelope, Cursor(index as u64 + 1)).unwrap(),
            IngestResult::Journaled
        );
    }

    let report = target.apply_pending(10).unwrap();
    assert_eq!((report.applied, report.quarantined), (2, 2));
    assert_eq!(
        target
            .quarantined()
            .unwrap()
            .into_iter()
            .map(|record| record.reason)
            .collect::<Vec<_>>(),
        vec![
            QuarantineReason::InvalidPayload,
            QuarantineReason::InvalidPayload,
        ]
    );
    assert!(
        target
            .rcs_link_status(claimed_gateway, 1_003)
            .unwrap()
            .is_none()
    );
    assert!(
        target
            .rcs_eligibility_projection(conversation, claimed_gateway, 1_003)
            .unwrap()
            .is_none()
    );
    assert!(target.rcs_gateway_supported(producer, 1_003).unwrap());
    assert_eq!(
        target
            .rcs_eligibility_projection(conversation, producer, 1_003)
            .unwrap()
            .unwrap()
            .eligibility,
        RcsEligibility::Eligible
    );
    assert_eq!(
        target.replay_progress().unwrap().processed_through,
        Cursor(4)
    );
    drop(target);

    assert_eq!(
        journal_outcomes(&target_config),
        vec![
            (1, "quarantined".into(), Some("invalid_payload".into())),
            (2, "quarantined".into(), Some("invalid_payload".into())),
            (3, "applied".into(), None),
            (4, "applied".into(), None),
        ]
    );
    let reopened = unlocked(&target_config, &vault);
    assert_eq!(reopened.apply_pending(10).unwrap(), ApplyReport::default());
    assert!(reopened.rcs_gateway_supported(producer, 1_003).unwrap());
    assert_eq!(
        reopened
            .rcs_eligibility_projection(conversation, producer, 1_003)
            .unwrap()
            .unwrap()
            .eligibility,
        RcsEligibility::Eligible
    );
}

#[test]
fn equal_revisions_use_producer_sequence_without_allowing_revision_rollback() {
    let directory = tempfile::tempdir().unwrap();
    let vault = Vault::new();
    let target_config = config(&directory, "rcs-capability-order", &vault);
    let target = unlocked(&target_config, &vault);
    let gateway = DeviceId::new();
    let conversation = ConversationId::new();
    let stored_eligibility = || {
        let connection = Connection::open(&target_config.database_path).unwrap();
        connection
            .execute_batch(
                "PRAGMA key = \"x'0707070707070707070707070707070707070707070707070707070707070707'\";",
            )
            .unwrap();
        connection
            .query_row(
                "SELECT eligibility,mapping_revision,observed_at_ms FROM rcs_link_eligibility_projection WHERE gateway_device_id=? AND conversation_id=?",
                [gateway.to_string(), conversation.to_string()],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, u64>(1)?, row.get::<_, i64>(2)?)),
            )
            .unwrap()
    };

    let mut unavailable = status(gateway, 5, 1_000);
    unavailable.state = RcsCapabilityState::Unavailable;
    unavailable.availability = false;
    let mut not_eligible = eligibility(gateway, conversation, 5, 1_000);
    not_eligible.eligibility = RcsEligibility::NotEligible;
    let initial = [
        sealed(
            &vault,
            gateway,
            20,
            PrivatePayload::RcsLinkStatus(RcsLinkStatusPayload {
                status: unavailable.clone(),
            }),
        ),
        sealed(
            &vault,
            gateway,
            20,
            PrivatePayload::RcsLinkEligibility(RcsLinkEligibilityPayload {
                eligibility: not_eligible.clone(),
            }),
        ),
    ];
    let lower_sequence = [
        sealed(
            &vault,
            gateway,
            19,
            PrivatePayload::RcsLinkStatus(RcsLinkStatusPayload {
                status: status(gateway, 5, 1_001),
            }),
        ),
        sealed(
            &vault,
            gateway,
            19,
            PrivatePayload::RcsLinkEligibility(RcsLinkEligibilityPayload {
                eligibility: eligibility(gateway, conversation, 5, 1_001),
            }),
        ),
    ];
    let higher_sequence = [
        sealed(
            &vault,
            gateway,
            21,
            PrivatePayload::RcsLinkStatus(RcsLinkStatusPayload {
                status: status(gateway, 5, 1_002),
            }),
        ),
        sealed(
            &vault,
            gateway,
            21,
            PrivatePayload::RcsLinkEligibility(RcsLinkEligibilityPayload {
                eligibility: eligibility(gateway, conversation, 5, 1_002),
            }),
        ),
    ];
    let mut older_unavailable = status(gateway, 4, 1_003);
    older_unavailable.state = RcsCapabilityState::Unavailable;
    older_unavailable.availability = false;
    let mut older_not_eligible = eligibility(gateway, conversation, 4, 1_003);
    older_not_eligible.eligibility = RcsEligibility::NotEligible;
    let older_revision = [
        sealed(
            &vault,
            gateway,
            22,
            PrivatePayload::RcsLinkStatus(RcsLinkStatusPayload {
                status: older_unavailable,
            }),
        ),
        sealed(
            &vault,
            gateway,
            22,
            PrivatePayload::RcsLinkEligibility(RcsLinkEligibilityPayload {
                eligibility: older_not_eligible,
            }),
        ),
    ];

    let mut cursor = 0_u64;
    let mut ingest_and_apply = |envelopes: &[Envelope]| {
        for envelope in envelopes {
            cursor += 1;
            assert_eq!(
                target.ingest(envelope, Cursor(cursor)).unwrap(),
                IngestResult::Journaled
            );
        }
        let report = target.apply_pending(20).unwrap();
        assert_eq!((report.applied, report.quarantined), (2, 0));
        assert_eq!(
            target.replay_progress().unwrap().processed_through,
            Cursor(cursor)
        );
    };

    ingest_and_apply(&initial);
    let status = target.rcs_link_status(gateway, 1_004).unwrap().unwrap();
    assert_eq!(status.status.capability_revision, 5);
    assert_eq!(status.status.state, RcsCapabilityState::Unavailable);
    assert_eq!(status.status.observed_at_ms, 1_000);
    let projection = target
        .rcs_eligibility_projection(conversation, gateway, 1_004)
        .unwrap()
        .unwrap();
    assert_eq!(projection.mapping_revision, 5);
    assert_eq!(projection.observed_at_ms, 1_000);
    assert_eq!(projection.eligibility, RcsEligibility::Unknown);
    assert_eq!(stored_eligibility(), ("not_eligible".into(), 5, 1_000));

    ingest_and_apply(&lower_sequence);
    let status = target.rcs_link_status(gateway, 1_004).unwrap().unwrap();
    assert_eq!(status.status.capability_revision, 5);
    assert_eq!(status.status.state, RcsCapabilityState::Unavailable);
    assert_eq!(status.status.observed_at_ms, 1_000);
    let projection = target
        .rcs_eligibility_projection(conversation, gateway, 1_004)
        .unwrap()
        .unwrap();
    assert_eq!(projection.mapping_revision, 5);
    assert_eq!(projection.observed_at_ms, 1_000);
    assert_eq!(projection.eligibility, RcsEligibility::Unknown);
    assert_eq!(stored_eligibility(), ("not_eligible".into(), 5, 1_000));

    ingest_and_apply(&higher_sequence);
    let status = target.rcs_link_status(gateway, 1_004).unwrap().unwrap();
    assert_eq!(status.status.capability_revision, 5);
    assert_eq!(status.status.state, RcsCapabilityState::Linked);
    assert_eq!(status.status.observed_at_ms, 1_002);
    let projection = target
        .rcs_eligibility_projection(conversation, gateway, 1_004)
        .unwrap()
        .unwrap();
    assert_eq!(projection.mapping_revision, 5);
    assert_eq!(projection.observed_at_ms, 1_002);
    assert_eq!(projection.eligibility, RcsEligibility::Eligible);
    assert_eq!(stored_eligibility(), ("eligible".into(), 5, 1_002));

    ingest_and_apply(&older_revision);
    let status = target.rcs_link_status(gateway, 1_004).unwrap().unwrap();
    assert_eq!(status.status.capability_revision, 5);
    assert_eq!(status.status.state, RcsCapabilityState::Linked);
    assert_eq!(status.status.observed_at_ms, 1_002);
    let projection = target
        .rcs_eligibility_projection(conversation, gateway, 1_004)
        .unwrap()
        .unwrap();
    assert_eq!(projection.mapping_revision, 5);
    assert_eq!(projection.observed_at_ms, 1_002);
    assert_eq!(projection.eligibility, RcsEligibility::Eligible);
    assert_eq!(stored_eligibility(), ("eligible".into(), 5, 1_002));
}

#[test]
fn compacted_capability_snapshot_and_historical_replay_do_not_resurrect_linkage() {
    let directory = tempfile::tempdir().unwrap();
    let vault = Vault::new();
    let source_config = config(&directory, "rcs-capability-snapshot-source", &vault);
    let source = unlocked(&source_config, &vault);
    let gateway = source_config.device_id;
    let conversation = ConversationId::new();
    let mut server = Server::default();

    source
        .publish_rcs_capability(
            &status(gateway, 1, 1_000),
            &[eligibility(gateway, conversation, 1, 1_000)],
            1_000,
        )
        .unwrap();
    assert_eq!(server.upload(&source), 2);
    let historical_linked = server.log[0].clone();
    let mut unlinked = status(gateway, 2, 1_001);
    unlinked.state = RcsCapabilityState::Unlinked;
    unlinked.availability = false;
    source
        .publish_rcs_capability(&unlinked, &[], 1_001)
        .unwrap();
    assert_eq!(server.upload(&source), 1);
    assert_eq!(server.log.len(), 3);

    let superseded = server
        .log
        .iter()
        .filter_map(|envelope| envelope.compaction.as_ref())
        .flat_map(|metadata| metadata.supersedes.iter())
        .map(|reference| (reference.producer_device_id, reference.producer_sequence.0))
        .collect::<HashSet<_>>();
    let snapshot = server
        .log
        .iter()
        .enumerate()
        .filter(|(_, envelope)| {
            !superseded.contains(&(envelope.producer_device_id, envelope.producer_sequence.0))
        })
        .map(|(index, envelope)| SnapshotRecord {
            cursor: Cursor(index as u64 + 1),
            envelope: envelope.clone(),
        })
        .collect::<Vec<_>>();
    assert_eq!(
        snapshot.len(),
        2,
        "latest status plus old eligibility retained"
    );

    let target_config = config(&directory, "rcs-capability-snapshot-target", &vault);
    let target = unlocked(&target_config, &vault);
    let session = target
        .begin_snapshot_with_compaction(
            Cursor(server.log.len() as u64),
            snapshot.len() as u64,
            SnapshotPurpose::Resync,
            Some(1),
        )
        .unwrap();
    assert_eq!(session.received_records, 0);
    let appended = target
        .append_snapshot_page(session.generation, &snapshot)
        .unwrap();
    assert_eq!(appended.received_records, 2);
    let published = target.finish_snapshot(session.generation).unwrap();
    assert_eq!(
        (
            published.journaled,
            published.duplicate,
            published.receive_cursor
        ),
        (2, 0, 3)
    );
    let applied = settle(&target);
    assert_eq!((applied.applied, applied.quarantined), (2, 0));
    assert_eq!(target.receive_cursor().unwrap(), Cursor(3));
    assert!(!target.rcs_gateway_supported(gateway, 1_002).unwrap());
    assert_eq!(
        target
            .rcs_link_status(gateway, 1_002)
            .unwrap()
            .unwrap()
            .status
            .state,
        RcsCapabilityState::Unlinked
    );
    assert_eq!(
        target
            .rcs_eligibility_projection(conversation, gateway, 1_002)
            .unwrap()
            .unwrap()
            .eligibility,
        RcsEligibility::Unknown
    );

    assert_eq!(
        target.ingest(&historical_linked, Cursor(4)).unwrap(),
        IngestResult::Journaled
    );
    let replay = target.apply_pending(10).unwrap();
    assert_eq!((replay.applied, replay.quarantined), (1, 0));
    assert_eq!(
        target.replay_progress().unwrap().processed_through,
        Cursor(4)
    );
    assert!(!target.rcs_gateway_supported(gateway, 1_002).unwrap());
    assert_eq!(
        target
            .rcs_link_status(gateway, 1_002)
            .unwrap()
            .unwrap()
            .status
            .state,
        RcsCapabilityState::Unlinked
    );
    assert_eq!(
        target
            .rcs_eligibility_projection(conversation, gateway, 1_002)
            .unwrap()
            .unwrap()
            .eligibility,
        RcsEligibility::Unknown
    );
    drop(target);

    let reopened = unlocked(&target_config, &vault);
    assert!(!reopened.rcs_gateway_supported(gateway, 1_002).unwrap());
    assert_eq!(
        reopened
            .rcs_eligibility_projection(conversation, gateway, 1_002)
            .unwrap()
            .unwrap()
            .eligibility,
        RcsEligibility::Unknown
    );
    assert_eq!(
        reopened.replay_progress().unwrap().processed_through,
        Cursor(4)
    );
}

#[test]
fn missing_linked_since_projects_unknown() {
    let directory = tempfile::tempdir().unwrap();
    let vault = Vault::new();
    let config = config(&directory, "rcs-capability-missing-linked-since", &vault);
    let client = unlocked(&config, &vault);
    let conversation = ConversationId::new();
    let mut linked = status(config.device_id, 1, 1_000);
    linked.linked_since_ms = None;

    client
        .publish_rcs_capability(
            &linked,
            &[eligibility(config.device_id, conversation, 1, 1_000)],
            1_000,
        )
        .unwrap();
    assert_eq!(
        client
            .rcs_eligibility_projection(conversation, config.device_id, 1_000)
            .unwrap()
            .unwrap()
            .eligibility,
        RcsEligibility::Unknown
    );
}

#[test]
fn linked_status_without_evidence_clears_stored_marker() {
    let directory = tempfile::tempdir().unwrap();
    let vault = Vault::new();
    let config = config(&directory, "rcs-capability-cleared-linked-since", &vault);
    let client = unlocked(&config, &vault);
    let conversation = ConversationId::new();

    client
        .publish_rcs_capability(
            &status(config.device_id, 1, 1_000),
            &[eligibility(config.device_id, conversation, 1, 1_000)],
            1_000,
        )
        .unwrap();
    let mut linked_without_evidence = status(config.device_id, 2, 1_001);
    linked_without_evidence.linked_since_ms = None;
    client
        .publish_rcs_capability(&linked_without_evidence, &[], 1_001)
        .unwrap();

    assert_eq!(
        client
            .rcs_eligibility_projection(conversation, config.device_id, 1_001)
            .unwrap()
            .unwrap()
            .eligibility,
        RcsEligibility::Unknown
    );
    drop(client);

    let reopened = unlocked(&config, &vault);
    assert_eq!(
        reopened
            .rcs_eligibility_projection(conversation, config.device_id, 1_001)
            .unwrap()
            .unwrap()
            .eligibility,
        RcsEligibility::Unknown
    );
}

#[test]
fn relink_after_clock_rollback_keeps_old_eligibility_unknown() {
    let directory = tempfile::tempdir().unwrap();
    let vault = Vault::new();
    let config = config(&directory, "rcs-capability-clock-rollback", &vault);
    let client = unlocked(&config, &vault);
    let conversation = ConversationId::new();

    client
        .record_rcs_link_lifecycle(
            RcsCapabilityState::Linked,
            true,
            true,
            true,
            100_000,
            100_000,
        )
        .unwrap();
    client
        .publish_rcs_capability(
            &status(config.device_id, 2, 110_000),
            &[eligibility(config.device_id, conversation, 1, 110_000)],
            110_000,
        )
        .unwrap();
    client
        .record_rcs_link_lifecycle(
            RcsCapabilityState::Unlinked,
            false,
            false,
            false,
            110_001,
            110_001,
        )
        .unwrap();
    client
        .record_rcs_link_lifecycle(
            RcsCapabilityState::Linked,
            true,
            true,
            true,
            105_000,
            105_000,
        )
        .unwrap();

    assert_eq!(
        client
            .rcs_eligibility_projection(conversation, config.device_id, 110_000)
            .unwrap()
            .unwrap()
            .eligibility,
        RcsEligibility::Unknown
    );
}

#[test]
fn lifecycle_publisher_derives_eligibility_and_monotonic_revisions_atomically() {
    let directory = tempfile::tempdir().unwrap();
    let vault = Vault::new();
    let config = config(&directory, "rcs-capability-derived", &vault);
    let client = unlocked(&config, &vault);
    let peer = "+12025550123";
    let mapped = client
        .capture_incoming(IncomingSms {
            conversation_id: None,
            sender_address: peer.into(),
            body: String::new(),
            provider_message_id: Some("provider-derived".into()),
            imported: true,
        })
        .unwrap()
        .conversation_id;
    let unmapped = ConversationId::new();
    assert!(matches!(
        client
            .verify_rcs_mapping(&mapping_observation(peer), 1_000)
            .unwrap(),
        RcsMappingOutcome::Mapped(_)
    ));
    client
        .record_rcs_link_lifecycle(RcsCapabilityState::Linked, true, true, true, 1_000, 1_000)
        .unwrap();

    assert_eq!(
        client
            .publish_rcs_eligibility_from_lifecycle(&[mapped, unmapped], 1_000)
            .unwrap(),
        2
    );
    let first_mapped = client
        .rcs_eligibility_projection(mapped, config.device_id, 1_000)
        .unwrap()
        .unwrap();
    let first_unmapped = client
        .rcs_eligibility_projection(unmapped, config.device_id, 1_000)
        .unwrap()
        .unwrap();
    assert_eq!(first_mapped.eligibility, RcsEligibility::Eligible);
    assert_eq!(first_unmapped.eligibility, RcsEligibility::NotEligible);

    client
        .record_rcs_link_lifecycle(
            RcsCapabilityState::NeedsRelink,
            false,
            false,
            false,
            1_001,
            1_001,
        )
        .unwrap();
    client
        .publish_rcs_eligibility_from_lifecycle(&[mapped], 1_001)
        .unwrap();
    let unavailable = client
        .rcs_eligibility_projection(mapped, config.device_id, 1_001)
        .unwrap()
        .unwrap();
    assert_eq!(unavailable.eligibility, RcsEligibility::Unknown);
    assert!(unavailable.mapping_revision > first_mapped.mapping_revision);

    client
        .record_rcs_link_lifecycle(RcsCapabilityState::Linked, true, true, true, 1_002, 1_002)
        .unwrap();
    client
        .publish_rcs_eligibility_from_lifecycle(&[mapped], 1_002)
        .unwrap();
    let invalidated = client
        .rcs_eligibility_projection(mapped, config.device_id, 1_002)
        .unwrap()
        .unwrap();
    assert_eq!(invalidated.eligibility, RcsEligibility::NotEligible);
    assert!(invalidated.mapping_revision > unavailable.mapping_revision);
}

#[test]
fn unchanged_eligibility_is_published_once_until_the_republish_floor_elapses() {
    let directory = tempfile::tempdir().unwrap();
    let vault = Vault::new();
    let config = config(&directory, "rcs-eligibility-idempotent", &vault);
    let client = unlocked(&config, &vault);
    let conversation = ConversationId::new();
    client
        .record_rcs_link_lifecycle(RcsCapabilityState::Linked, true, true, true, 1_000, 1_000)
        .unwrap();
    assert_eq!(
        client
            .publish_rcs_eligibility_from_lifecycle(&[conversation], 1_000)
            .unwrap(),
        1
    );
    let queued = client.pending_outbox_count().unwrap();
    let first = client
        .rcs_eligibility_projection(conversation, config.device_id, 1_000)
        .unwrap()
        .unwrap();

    let before_floor = 1_000 + RCS_ELIGIBILITY_REPUBLISH_FLOOR_MS - 1;
    assert_eq!(
        client
            .publish_rcs_eligibility_from_lifecycle(&[conversation], before_floor)
            .unwrap(),
        0
    );
    assert_eq!(client.pending_outbox_count().unwrap(), queued);
    assert_eq!(
        client
            .rcs_eligibility_projection(conversation, config.device_id, before_floor)
            .unwrap()
            .unwrap(),
        first
    );

    let at_floor = 1_000 + RCS_ELIGIBILITY_REPUBLISH_FLOOR_MS;
    assert_eq!(
        client
            .publish_rcs_eligibility_from_lifecycle(&[conversation], at_floor)
            .unwrap(),
        1
    );
    assert_eq!(client.pending_outbox_count().unwrap(), queued + 1);
}

#[test]
fn unchanged_eligibility_is_published_again_after_the_link_returns() {
    let directory = tempfile::tempdir().unwrap();
    let vault = Vault::new();
    let config = config(&directory, "rcs-eligibility-relinked", &vault);
    let client = unlocked(&config, &vault);
    let conversation = ConversationId::new();
    client
        .record_rcs_link_lifecycle(RcsCapabilityState::Linked, true, true, true, 1_000, 1_000)
        .unwrap();
    client
        .publish_rcs_eligibility_from_lifecycle(&[conversation], 1_000)
        .unwrap();
    client
        .record_rcs_link_lifecycle(
            RcsCapabilityState::Unavailable,
            false,
            false,
            false,
            1_001,
            1_001,
        )
        .unwrap();
    client
        .record_rcs_link_lifecycle(RcsCapabilityState::Linked, true, true, true, 1_002, 1_002)
        .unwrap();

    assert_eq!(
        client
            .publish_rcs_eligibility_from_lifecycle(&[conversation], 1_002)
            .unwrap(),
        0
    );
    assert_eq!(
        client
            .rcs_eligibility_projection(conversation, config.device_id, 1_002)
            .unwrap()
            .unwrap()
            .eligibility,
        RcsEligibility::NotEligible
    );
}

#[test]
fn linked_record_refreshes_an_invalidated_mapping_after_return() {
    let directory = tempfile::tempdir().unwrap();
    let vault = Vault::new();
    let config = config(&directory, "rcs-capability-return", &vault);
    let client = unlocked(&config, &vault);
    let mapped = client
        .capture_incoming(IncomingSms {
            conversation_id: None,
            sender_address: "+12025550123".into(),
            body: String::new(),
            provider_message_id: Some("provider-return".into()),
            imported: true,
        })
        .unwrap()
        .conversation_id;
    assert!(matches!(
        client
            .verify_rcs_mapping(&mapping_observation("+12025550123"), 1_000)
            .unwrap(),
        RcsMappingOutcome::Mapped(_)
    ));
    client
        .record_rcs_link_lifecycle(RcsCapabilityState::Linked, true, true, true, 1_000, 1_000)
        .unwrap();
    assert_eq!(
        client
            .publish_rcs_eligibility_from_lifecycle(&[mapped], 1_000)
            .unwrap(),
        1
    );
    drop(client);
    let connection = Connection::open(&config.database_path).unwrap();
    connection
        .execute_batch(
            "PRAGMA key = \"x'0707070707070707070707070707070707070707070707070707070707070707'\";",
        )
        .unwrap();
    connection
        .execute(
            "UPDATE rcs_conversation_mappings SET invalidated_at_ms=1001,invalidation_code='test' WHERE peppy_conversation_id=?",
            [mapped.to_string()],
        )
        .unwrap();
    drop(connection);
    let client = unlocked(&config, &vault);
    client
        .record_rcs_link_lifecycle(
            RcsCapabilityState::Unavailable,
            false,
            false,
            false,
            1_001,
            1_001,
        )
        .unwrap();
    client
        .record_rcs_link_lifecycle(RcsCapabilityState::Linked, true, true, true, 1_002, 1_002)
        .unwrap();

    assert_eq!(
        client
            .rcs_eligibility_projection(mapped, config.device_id, 1_002)
            .unwrap()
            .unwrap()
            .eligibility,
        RcsEligibility::NotEligible
    );
}

#[test]
fn linked_record_refreshes_published_mapped_and_unmapped_eligibility_after_return() {
    let directory = tempfile::tempdir().unwrap();
    let vault = Vault::new();
    let config = config(&directory, "rcs-capability-return-active", &vault);
    let client = unlocked(&config, &vault);
    let mapped = client
        .capture_incoming(IncomingSms {
            conversation_id: None,
            sender_address: "+12025550123".into(),
            body: String::new(),
            provider_message_id: Some("provider-return-active".into()),
            imported: true,
        })
        .unwrap()
        .conversation_id;
    assert!(matches!(
        client
            .verify_rcs_mapping(&mapping_observation("+12025550123"), 1_000)
            .unwrap(),
        RcsMappingOutcome::Mapped(_)
    ));
    let unmapped = ConversationId::new();
    client
        .record_rcs_link_lifecycle(RcsCapabilityState::Linked, true, true, true, 1_000, 1_000)
        .unwrap();
    assert_eq!(
        client
            .publish_rcs_eligibility_from_lifecycle(&[mapped, unmapped], 1_000)
            .unwrap(),
        2
    );
    client
        .record_rcs_link_lifecycle(
            RcsCapabilityState::Unavailable,
            false,
            false,
            false,
            1_001,
            1_001,
        )
        .unwrap();
    client
        .record_rcs_link_lifecycle(RcsCapabilityState::Linked, true, true, true, 1_002, 1_002)
        .unwrap();
    assert_eq!(
        client
            .rcs_eligibility_projection(mapped, config.device_id, 1_002)
            .unwrap()
            .unwrap()
            .eligibility,
        RcsEligibility::Eligible
    );
    assert_eq!(
        client
            .rcs_eligibility_projection(unmapped, config.device_id, 1_002)
            .unwrap()
            .unwrap()
            .eligibility,
        RcsEligibility::NotEligible
    );
}

#[test]
fn unchanged_linked_since_keeps_old_eligibility_current() {
    let directory = tempfile::tempdir().unwrap();
    let vault = Vault::new();
    let config = config(&directory, "rcs-capability-keepalive", &vault);
    let client = unlocked(&config, &vault);
    let conversation = ConversationId::new();
    let started = 1_000;
    client
        .record_rcs_link_lifecycle(
            RcsCapabilityState::Linked,
            true,
            true,
            true,
            started,
            started,
        )
        .unwrap();
    client
        .publish_rcs_eligibility_from_lifecycle(&[conversation], started)
        .unwrap();
    let first = client
        .rcs_eligibility_projection(conversation, config.device_id, started)
        .unwrap()
        .unwrap();
    for now in [
        started + 10 * 60 * 1_000,
        started + 20 * 60 * 1_000,
        started + 30 * 60 * 1_000,
    ] {
        client
            .record_rcs_link_lifecycle(RcsCapabilityState::Linked, true, true, true, now, now)
            .unwrap();
    }
    let refreshed = client
        .rcs_eligibility_projection(conversation, config.device_id, started + 31 * 60 * 1_000)
        .unwrap()
        .unwrap();
    assert_eq!(refreshed.eligibility, RcsEligibility::NotEligible);
    assert_eq!(first.eligibility, RcsEligibility::NotEligible);
    assert_eq!(client.pending_outbox_count().unwrap(), 5);
}

#[test]
fn linked_record_before_keepalive_adds_only_its_status_event() {
    let directory = tempfile::tempdir().unwrap();
    let vault = Vault::new();
    let config = config(&directory, "rcs-capability-no-churn", &vault);
    let client = unlocked(&config, &vault);
    let conversation = ConversationId::new();
    client
        .record_rcs_link_lifecycle(RcsCapabilityState::Linked, true, true, true, 1_000, 1_000)
        .unwrap();
    client
        .publish_rcs_eligibility_from_lifecycle(&[conversation], 1_000)
        .unwrap();
    let before = client.pending_outbox_count().unwrap();
    client
        .record_rcs_link_lifecycle(RcsCapabilityState::Linked, true, true, true, 1_001, 1_001)
        .unwrap();
    assert_eq!(client.pending_outbox_count().unwrap(), before + 1);
}

#[test]
fn linked_record_refreshes_all_due_published_rows() {
    let directory = tempfile::tempdir().unwrap();
    let vault = Vault::new();
    let config = config(&directory, "rcs-capability-drain", &vault);
    let client = unlocked(&config, &vault);
    let conversations = (0..RCS_CAPABILITY_PUBLISH_BATCH + 1)
        .map(|_| ConversationId::new())
        .collect::<Vec<_>>();
    client
        .record_rcs_link_lifecycle(RcsCapabilityState::Linked, true, true, true, 1_000, 1_000)
        .unwrap();
    for batch in conversations.chunks(RCS_CAPABILITY_PUBLISH_BATCH) {
        client
            .publish_rcs_eligibility_from_lifecycle(batch, 1_000)
            .unwrap();
    }
    let before = client.pending_outbox_count().unwrap();
    client
        .record_rcs_link_lifecycle(
            RcsCapabilityState::Unavailable,
            false,
            false,
            false,
            1_001,
            1_001,
        )
        .unwrap();
    client
        .record_rcs_link_lifecycle(RcsCapabilityState::Linked, true, true, true, 1_002, 1_002)
        .unwrap();
    assert_eq!(client.pending_outbox_count().unwrap(), before + 103);
}

#[test]
fn replayed_refreshed_eligibility_is_current_on_another_client() {
    let directory = tempfile::tempdir().unwrap();
    let vault = Vault::new();
    let source_config = config(&directory, "rcs-capability-replay-source", &vault);
    let source = unlocked(&source_config, &vault);
    let conversation = ConversationId::new();
    let mut server = Server::default();
    source
        .record_rcs_link_lifecycle(RcsCapabilityState::Linked, true, true, true, 1_000, 1_000)
        .unwrap();
    source
        .publish_rcs_eligibility_from_lifecycle(&[conversation], 1_000)
        .unwrap();
    server.upload(&source);
    source
        .record_rcs_link_lifecycle(
            RcsCapabilityState::Unavailable,
            false,
            false,
            false,
            1_001,
            1_001,
        )
        .unwrap();
    source
        .record_rcs_link_lifecycle(RcsCapabilityState::Linked, true, true, true, 1_002, 1_002)
        .unwrap();
    server.upload(&source);
    let peer_config = config(&directory, "rcs-capability-replay-peer", &vault);
    let peer = unlocked(&peer_config, &vault);
    for (index, envelope) in server.log.iter().enumerate() {
        peer.ingest(envelope, Cursor(index as u64 + 1)).unwrap();
    }
    assert_eq!(settle(&peer).quarantined, 0);
    assert_eq!(
        peer.rcs_eligibility_projection(conversation, source_config.device_id, 1_002)
            .unwrap()
            .unwrap()
            .eligibility,
        RcsEligibility::NotEligible
    );
}

#[test]
fn non_linked_record_does_not_refresh_eligibility() {
    let directory = tempfile::tempdir().unwrap();
    let vault = Vault::new();
    let config = config(&directory, "rcs-capability-non-linked", &vault);
    let client = unlocked(&config, &vault);
    let conversation = ConversationId::new();
    client
        .record_rcs_link_lifecycle(RcsCapabilityState::Linked, true, true, true, 1_000, 1_000)
        .unwrap();
    client
        .publish_rcs_eligibility_from_lifecycle(&[conversation], 1_000)
        .unwrap();
    let before = client.pending_outbox_count().unwrap();
    client
        .record_rcs_link_lifecycle(
            RcsCapabilityState::Unavailable,
            false,
            false,
            false,
            1_001,
            1_001,
        )
        .unwrap();
    assert_eq!(client.pending_outbox_count().unwrap(), before + 1);
}

#[test]
fn linked_record_with_no_published_eligibility_adds_only_status() {
    let directory = tempfile::tempdir().unwrap();
    let vault = Vault::new();
    let config = config(&directory, "rcs-capability-empty", &vault);
    let client = unlocked(&config, &vault);
    client
        .record_rcs_link_lifecycle(RcsCapabilityState::Linked, true, true, true, 1_000, 1_000)
        .unwrap();
    assert_eq!(client.pending_outbox_count().unwrap(), 1);
}

#[test]
fn linked_record_rederives_due_raw_capability_from_local_mappings() {
    let directory = tempfile::tempdir().unwrap();
    let vault = Vault::new();
    let config = config(&directory, "rcs-capability-raw", &vault);
    let client = unlocked(&config, &vault);
    let conversation = ConversationId::new();
    client
        .record_rcs_link_lifecycle(RcsCapabilityState::Linked, true, true, true, 1_000, 1_000)
        .unwrap();
    client
        .publish_rcs_capability(
            &status(config.device_id, 2, 1_000),
            &[eligibility(config.device_id, conversation, 1, 1_000)],
            1_000,
        )
        .unwrap();
    client
        .record_rcs_link_lifecycle(
            RcsCapabilityState::Unavailable,
            false,
            false,
            false,
            1_001,
            1_001,
        )
        .unwrap();
    client
        .record_rcs_link_lifecycle(RcsCapabilityState::Linked, true, true, true, 1_002, 1_002)
        .unwrap();
    assert_eq!(
        client
            .rcs_eligibility_projection(conversation, config.device_id, 1_002,)
            .unwrap()
            .unwrap()
            .eligibility,
        RcsEligibility::NotEligible
    );
}

#[test]
fn linked_record_before_linked_since_after_clock_rollback_skips_eligibility_refresh() {
    let directory = tempfile::tempdir().unwrap();
    let vault = Vault::new();
    let config = config(&directory, "rcs-capability-refresh-rollback", &vault);
    let client = unlocked(&config, &vault);
    let conversation = ConversationId::new();
    client
        .record_rcs_link_lifecycle(RcsCapabilityState::Linked, true, true, true, 1_000, 1_000)
        .unwrap();
    client
        .publish_rcs_eligibility_from_lifecycle(&[conversation], 1_000)
        .unwrap();
    client
        .record_rcs_link_lifecycle(
            RcsCapabilityState::Unavailable,
            false,
            false,
            false,
            1_001,
            1_001,
        )
        .unwrap();
    let before = client.pending_outbox_count().unwrap();
    client
        .record_rcs_link_lifecycle(RcsCapabilityState::Linked, true, true, true, 999, 999)
        .unwrap();
    assert_eq!(client.pending_outbox_count().unwrap(), before + 1);
}

#[test]
fn current_gateway_status_keeps_eligibility_current_before_the_next_linked_record() {
    let directory = tempfile::tempdir().unwrap();
    let vault = Vault::new();
    let config = config(&directory, "rcs-capability-residual-window", &vault);
    let client = unlocked(&config, &vault);
    let conversation = ConversationId::new();
    let started = 1_000;
    client
        .record_rcs_link_lifecycle(
            RcsCapabilityState::Linked,
            true,
            true,
            true,
            started,
            started,
        )
        .unwrap();
    client
        .publish_rcs_eligibility_from_lifecycle(&[conversation], started)
        .unwrap();
    let just_before = started + RCS_CAPABILITY_KEEPALIVE_MS - 1;
    client
        .record_rcs_link_lifecycle(
            RcsCapabilityState::Linked,
            true,
            true,
            true,
            just_before,
            just_before,
        )
        .unwrap();
    let current_status = started + 20 * 60 * 1_000;
    let mut current_status = status(config.device_id, 3, current_status);
    current_status.linked_since_ms = Some(started);
    client
        .publish_rcs_capability(&current_status, &[], current_status.observed_at_ms)
        .unwrap();
    let expired = started + 30 * 60 * 1_000 + 1;
    assert_eq!(
        client
            .rcs_eligibility_projection(conversation, config.device_id, expired)
            .unwrap()
            .unwrap()
            .eligibility,
        RcsEligibility::NotEligible
    );
    client
        .record_rcs_link_lifecycle(
            RcsCapabilityState::Linked,
            true,
            true,
            true,
            expired,
            expired,
        )
        .unwrap();
    assert_eq!(
        client
            .rcs_eligibility_projection(conversation, config.device_id, expired)
            .unwrap()
            .unwrap()
            .eligibility,
        RcsEligibility::NotEligible
    );
}

#[test]
fn failed_due_row_is_contained_while_other_rows_refresh() {
    let directory = tempfile::tempdir().unwrap();
    let vault = Vault::new();
    let config = config(&directory, "rcs-capability-contained-row", &vault);
    let client = unlocked(&config, &vault);
    let broken = ConversationId::new();
    let healthy = ConversationId::new();
    client
        .record_rcs_link_lifecycle(RcsCapabilityState::Linked, true, true, true, 1_000, 1_000)
        .unwrap();
    client
        .publish_rcs_eligibility_from_lifecycle(&[broken, healthy], 1_000)
        .unwrap();
    let broken_before = client
        .rcs_eligibility_projection(broken, config.device_id, 1_000)
        .unwrap()
        .unwrap();
    let before = client.pending_outbox_count().unwrap();
    drop(client);
    let connection = Connection::open(&config.database_path).unwrap();
    connection
        .execute_batch(
            "PRAGMA key = \"x'0707070707070707070707070707070707070707070707070707070707070707'\";",
        )
        .unwrap();
    let broken_identity = format!("eligibility:{}:{broken}", config.device_id);
    connection
        .execute_batch(&format!(
            "CREATE TRIGGER fail_eligibility_refresh BEFORE UPDATE ON rcs_capability_published \
             WHEN NEW.identity = '{broken_identity}' BEGIN SELECT RAISE(ABORT, 'x'); END;"
        ))
        .unwrap();
    drop(connection);
    let client = unlocked(&config, &vault);
    client
        .record_rcs_link_lifecycle(
            RcsCapabilityState::Unavailable,
            false,
            false,
            false,
            1_001,
            1_001,
        )
        .unwrap();
    client
        .record_rcs_link_lifecycle(RcsCapabilityState::Linked, true, true, true, 1_002, 1_002)
        .unwrap();
    assert_eq!(client.pending_outbox_count().unwrap(), before + 3);
    assert_eq!(
        client
            .rcs_eligibility_projection(broken, config.device_id, 1_002,)
            .unwrap()
            .unwrap()
            .observed_at_ms,
        broken_before.observed_at_ms
    );
    assert_eq!(
        client
            .rcs_eligibility_projection(healthy, config.device_id, 1_002,)
            .unwrap()
            .unwrap()
            .observed_at_ms,
        1_002
    );
}
