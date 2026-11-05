use super::common::*;

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
    let value = serde_json::json!({
        "kind": "rcs_link_status",
        "status": {
            "gateway_device_id": DeviceId::new(), "state": "linked", "availability": true,
            "health": true, "phone_responsive": true, "observed_at_ms": 1,
            "capability_revision": 1, "feature_bits": 0, "upstream_identifier": "forbidden"
        }
    });
    assert!(serde_json::from_value::<PrivatePayload>(value).is_err());
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
            .publish_rcs_capability(&status, &[projection.clone()], 1_000)
            .unwrap(),
        2
    );
    assert!(client
        .rcs_gateway_supported(config.device_id, 1_001)
        .unwrap());
    assert_eq!(
        client
            .publish_rcs_capability(&status, &[projection.clone()], 1_001)
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
    assert!(client.pending_outbox().unwrap().iter().any(|event| event
        .compaction
        .as_ref()
        .is_some_and(|metadata| metadata
            .supersedes
            .iter()
            .any(|reference| reference.producer_sequence == first.producer_sequence))));
}
