#[path = "common/mod.rs"]
mod common;

use common::*;
use peppy_protocol::{TypingOriginV2, TypingStateV2};

fn publish_eligible(client: &Client, gateway: DeviceId, conversation_id: ConversationId) {
    client
        .publish_rcs_capability(
            &RcsLinkStatusEvent {
                gateway_device_id: gateway,
                state: RcsCapabilityState::Linked,
                availability: true,
                health: true,
                phone_responsive: true,
                observed_at_ms: 1_000,
                capability_revision: 1,
                feature_bits: 1,
            },
            &[RcsEligibilityProjection {
                conversation_id,
                eligibility: RcsEligibility::Eligible,
                feature_bits: 1,
                mapping_revision: 1,
                observed_at_ms: 1_000,
                gateway_device_id: gateway,
            }],
            1_000,
        )
        .unwrap();
}

#[test]
fn v2_is_disabled_until_a_trusted_context_enables_and_then_seals_a_real_frame() {
    let directory = tempfile::tempdir().unwrap();
    let vault = Vault::new();
    let config = config(&directory, "v2-runtime", &vault);
    let client = unlocked(&config, &vault);
    let conversation = ConversationId::new();
    publish_eligible(&client, config.device_id, conversation);
    let clock = LiveClock {
        wall_ms: 1_000,
        mono_ms: 1_000,
    };
    assert_eq!(
        client
            .seal_typing_signal_v2(
                conversation,
                config.device_id,
                TypingOriginV2::VaultDevice,
                TypingStateV2::Started,
                clock,
            )
            .unwrap_err(),
        LiveError::Disabled
    );
    let generation = client
        .enable_live_typing(
            LiveEnableContext {
                trusted_roster: vec![config.device_id],
                local_gateway_device_id: config.device_id,
                local_executor: LiveExecutor::CurrentGateway,
            },
            clock,
        )
        .unwrap();
    let outbound = client
        .seal_typing_signal_v2(
            conversation,
            config.device_id,
            TypingOriginV2::VaultDevice,
            TypingStateV2::Started,
            LiveClock {
                wall_ms: 1_001,
                mono_ms: 1_001,
            },
        )
        .unwrap()
        .unwrap();
    assert_eq!(outbound.generation, generation);
    assert_eq!(outbound.signal.sequence, "1");
    assert_eq!(outbound.signal.session_id.len(), 22);
    assert!(!outbound.signal.ciphertext_b64.is_empty());
}

#[test]
fn v2_rejects_empty_host_roster() {
    let directory = tempfile::tempdir().unwrap();
    let vault = Vault::new();
    let config = config(&directory, "v2-empty-roster", &vault);
    let client = unlocked(&config, &vault);
    assert_eq!(
        client
            .enable_live_typing(
                LiveEnableContext {
                    trusted_roster: vec![],
                    local_gateway_device_id: config.device_id,
                    local_executor: LiveExecutor::CurrentGateway,
                },
                LiveClock {
                    wall_ms: 1_000,
                    mono_ms: 1_000,
                },
            )
            .unwrap_err(),
        LiveError::InvalidContext
    );
}
