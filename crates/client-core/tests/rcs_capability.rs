use super::common::*;
use peppy_crypto::{KeyPurpose, derive_purpose_key, derive_root_key, encrypt};

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

#[test]
fn invalid_authenticated_capability_events_quarantine_without_wedging_following_updates() {
    let directory = tempfile::tempdir().unwrap();
    let vault = Vault::new();
    let target_config = config(&directory, "rcs-capability-replay-target", &vault);
    let target = unlocked(&target_config, &vault);
    let producer = DeviceId::new();
    let mut oversized = status(producer, u64::MAX, 1_000);
    target
        .ingest(
            &sealed(
                &vault,
                producer,
                1,
                PrivatePayload::RcsLinkStatus(RcsLinkStatusPayload {
                    status: oversized.clone(),
                }),
            ),
            Cursor(1),
        )
        .unwrap();
    oversized.capability_revision = 1;
    target
        .ingest(
            &sealed(
                &vault,
                producer,
                u64::MAX,
                PrivatePayload::RcsLinkStatus(RcsLinkStatusPayload {
                    status: oversized.clone(),
                }),
            ),
            Cursor(2),
        )
        .unwrap();
    target
        .ingest(
            &sealed(
                &vault,
                producer,
                3,
                PrivatePayload::RcsLinkStatus(RcsLinkStatusPayload { status: oversized }),
            ),
            Cursor(3),
        )
        .unwrap();
    let report = target.apply_pending(10).unwrap();
    assert_eq!((report.applied, report.quarantined), (1, 2));
    assert_eq!(
        target
            .quarantined()
            .unwrap()
            .into_iter()
            .map(|record| record.reason)
            .collect::<Vec<_>>(),
        vec![QuarantineReason::InvalidPayload, QuarantineReason::InvalidPayload]
    );
    assert!(target.rcs_gateway_supported(producer, 1_001).unwrap());
    drop(target);
    let reopened = unlocked(&target_config, &vault);
    assert_eq!(reopened.quarantine_count().unwrap(), 2);
    assert!(reopened.rcs_gateway_supported(producer, 1_001).unwrap());
}

#[test]
fn mismatched_authenticated_producer_cannot_claim_a_gateway() {
    let directory = tempfile::tempdir().unwrap();
    let vault = Vault::new();
    let target = unlocked(&config(&directory, "rcs-capability-source", &vault), &vault);
    let producer = DeviceId::new();
    let claimed_gateway = DeviceId::new();
    target
        .ingest(
            &sealed(
                &vault,
                producer,
                1,
                PrivatePayload::RcsLinkStatus(RcsLinkStatusPayload {
                    status: status(claimed_gateway, 1, 1_000),
                }),
            ),
            Cursor(1),
        )
        .unwrap();
    target
        .ingest(
            &sealed(
                &vault,
                producer,
                2,
                PrivatePayload::RcsLinkEligibility(RcsLinkEligibilityPayload {
                    eligibility: eligibility(claimed_gateway, ConversationId::new(), 1, 1_000),
                }),
            ),
            Cursor(2),
        )
        .unwrap();
    let report = target.apply_pending(10).unwrap();
    assert_eq!((report.applied, report.quarantined), (0, 2));
    assert_eq!(target.quarantine_count().unwrap(), 2);
    assert!(
        target
            .rcs_link_status(claimed_gateway, 1_001)
            .unwrap()
            .is_none()
    );
}

#[test]
fn out_of_order_revisions_cannot_restore_an_older_eligible_status() {
    let directory = tempfile::tempdir().unwrap();
    let vault = Vault::new();
    let target = unlocked(&config(&directory, "rcs-capability-order", &vault), &vault);
    let gateway = DeviceId::new();
    let mut unavailable = status(gateway, 2, 1_002);
    unavailable.state = RcsCapabilityState::Unavailable;
    unavailable.availability = false;
    target
        .ingest(
            &sealed(
                &vault,
                gateway,
                20,
                PrivatePayload::RcsLinkStatus(RcsLinkStatusPayload {
                    status: unavailable,
                }),
            ),
            Cursor(1),
        )
        .unwrap();
    target
        .ingest(
            &sealed(
                &vault,
                gateway,
                21,
                PrivatePayload::RcsLinkStatus(RcsLinkStatusPayload {
                    status: status(gateway, 1, 1_001),
                }),
            ),
            Cursor(2),
        )
        .unwrap();
    target.apply_pending(10).unwrap();
    let view = target.rcs_link_status(gateway, 1_003).unwrap().unwrap();
    assert_eq!(view.status.state, RcsCapabilityState::Unavailable);
    assert!(!target.rcs_gateway_supported(gateway, 1_003).unwrap());
}
