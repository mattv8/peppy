use crate::GatewayView;
use peppy_client_core::{
    Client, ConversationId, DeviceId, Error, RCS_CAPABILITY_STALE_MS, RcsCapabilityState,
    RcsEligibility, RcsLinkStatusProjection,
};
use serde::Serialize;
use std::collections::HashSet;
use ts_rs::TS;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename = "RcsStatusSnapshot", rename_all = "camelCase")]
pub struct RcsStatusSnapshot {
    pub gateways: Vec<RcsGatewayStatus>,
    pub eligibilities: Vec<RcsConversationEligibility>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename = "RcsGatewayStatus", rename_all = "camelCase")]
pub struct RcsGatewayStatus {
    pub gateway_id: String,
    pub state: RcsGatewayState,
    pub current: bool,
    pub available: bool,
    pub healthy: bool,
    pub phone_responsive: bool,
    pub observed_at_ms: String,
    pub capability_revision: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub expires_at_ms: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(rename = "RcsGatewayState")]
pub enum RcsGatewayState {
    Linked,
    Unlinked,
    NeedsRelink,
    SignedOut,
    Unavailable,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename = "RcsConversationEligibility", rename_all = "camelCase")]
pub struct RcsConversationEligibility {
    pub conversation_id: String,
    pub gateway_id: String,
    pub eligibility: RcsEligibilityState,
    pub observed_at_ms: String,
    pub mapping_revision: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub expires_at_ms: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(rename = "RcsEligibilityState")]
pub enum RcsEligibilityState {
    Eligible,
    NotEligible,
    Unknown,
}

/// Produces bounded display state only for host-validated, non-simulated gateway devices.
/// The caller supplies the exact selected existing conversation and unlock state so this view
/// cannot survive a lock, identity switch, or draft-only selection.
pub fn snapshot_at(
    client: &Client,
    gateways: &[GatewayView],
    revoked_device_ids: &HashSet<String>,
    active_conversation: Option<ConversationId>,
    active_is_existing: bool,
    unlocked: bool,
    now_ms: i64,
) -> Result<Option<RcsStatusSnapshot>, Error> {
    if !unlocked {
        return Ok(None);
    }

    let gateway_ids = validated_gateway_ids(gateways, revoked_device_ids);
    let mut statuses = Vec::new();
    let mut eligibilities = Vec::new();
    for gateway_id in gateway_ids {
        let Some(status) = client.rcs_link_status(gateway_id, now_ms)? else {
            continue;
        };
        let current_gateway = current_gateway(&status);
        statuses.push(gateway_status(gateway_id, &status));
        if active_is_existing
            && current_gateway
            && let Some(conversation_id) = active_conversation
            && let Some(projection) =
                client.rcs_eligibility_projection(conversation_id, gateway_id, now_ms)?
        {
            eligibilities.push(RcsConversationEligibility {
                conversation_id: projection.conversation_id.to_string(),
                gateway_id: projection.gateway_device_id.to_string(),
                eligibility: match projection.eligibility {
                    RcsEligibility::Eligible => RcsEligibilityState::Eligible,
                    RcsEligibility::NotEligible => RcsEligibilityState::NotEligible,
                    RcsEligibility::Unknown => RcsEligibilityState::Unknown,
                },
                observed_at_ms: projection.observed_at_ms.to_string(),
                mapping_revision: projection.mapping_revision.to_string(),
                expires_at_ms: expiry(projection.observed_at_ms).map(|value| value.to_string()),
            });
        }
    }
    Ok(
        (!statuses.is_empty() || !eligibilities.is_empty()).then_some(RcsStatusSnapshot {
            gateways: statuses,
            eligibilities,
        }),
    )
}

fn validated_gateway_ids(
    gateways: &[GatewayView],
    revoked_device_ids: &HashSet<String>,
) -> Vec<DeviceId> {
    let mut seen = HashSet::new();
    gateways
        .iter()
        .filter(|gateway| {
            !gateway.simulated
                && gateway.supports_sms
                && !revoked_device_ids.contains(&gateway.id)
                && seen.insert(gateway.id.as_str())
        })
        .filter_map(|gateway| gateway.id.parse().ok())
        .collect()
}

fn current_gateway(status: &RcsLinkStatusProjection) -> bool {
    status.current
        && matches!(status.status.state, RcsCapabilityState::Linked)
        && status.status.availability
        && status.status.health
        && status.status.phone_responsive
}

fn gateway_status(gateway_id: DeviceId, status: &RcsLinkStatusProjection) -> RcsGatewayStatus {
    RcsGatewayStatus {
        gateway_id: gateway_id.to_string(),
        state: match status.status.state {
            RcsCapabilityState::Linked => RcsGatewayState::Linked,
            RcsCapabilityState::Unlinked => RcsGatewayState::Unlinked,
            RcsCapabilityState::NeedsRelink => RcsGatewayState::NeedsRelink,
            RcsCapabilityState::SignedOut => RcsGatewayState::SignedOut,
            RcsCapabilityState::Unavailable => RcsGatewayState::Unavailable,
        },
        current: status.current && expiry(status.status.observed_at_ms).is_some(),
        available: status.status.availability,
        healthy: status.status.health,
        phone_responsive: status.status.phone_responsive,
        observed_at_ms: status.status.observed_at_ms.to_string(),
        capability_revision: status.status.capability_revision.to_string(),
        expires_at_ms: expiry(status.status.observed_at_ms).map(|value| value.to_string()),
    }
}

fn expiry(observed_at_ms: i64) -> Option<i64> {
    (observed_at_ms >= 0)
        .then_some(observed_at_ms)
        .and_then(|value| value.checked_add(RCS_CAPABILITY_STALE_MS))
        .and_then(|value| value.checked_add(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expiry_matches_core_inclusive_currentness_boundary() {
        assert_eq!(expiry(1_000), Some(1_000 + RCS_CAPABILITY_STALE_MS + 1));
        assert_eq!(expiry(-1), None);
        assert_eq!(expiry(i64::MAX), None);
    }
}
