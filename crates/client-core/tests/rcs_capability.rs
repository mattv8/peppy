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
            .publish_rcs_capability(&status, &[projection], 1_000 + RCS_CAPABILITY_KEEPALIVE_MS)
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
