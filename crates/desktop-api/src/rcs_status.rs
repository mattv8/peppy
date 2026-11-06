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

pub struct SnapshotAccess {
    pub active_conversation: Option<ConversationId>,
    pub active_is_existing: bool,
    pub roster_validated: bool,
    pub unlocked: bool,
    pub now_ms: i64,
}

/// Produces bounded display state only for host-validated, non-simulated gateway devices.
/// The caller supplies the exact selected existing conversation and unlock state so this view
/// cannot survive a lock, identity switch, or draft-only selection.
pub fn snapshot_at(
    client: &Client,
    gateways: &[GatewayView],
    revoked_device_ids: &HashSet<String>,
    access: SnapshotAccess,
) -> Result<Option<RcsStatusSnapshot>, Error> {
    if !access.unlocked || !access.roster_validated {
        return Ok(None);
    }

    let gateway_ids = validated_gateway_ids(gateways, revoked_device_ids);
    let mut statuses = Vec::new();
    let mut eligibilities = Vec::new();
    for gateway_id in gateway_ids {
        let Some(status) = client.rcs_link_status(gateway_id, access.now_ms)? else {
            continue;
        };
        let current_gateway = current_gateway(&status);
        statuses.push(gateway_status(gateway_id, &status));
        if access.active_is_existing
            && current_gateway
            && let Some(conversation_id) = access.active_conversation
            && let Some(projection) =
                client.rcs_eligibility_projection(conversation_id, gateway_id, access.now_ms)?
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
                && gateway.availability != crate::GatewayAvailability::UnsupportedPlatform
                && !revoked_device_ids.contains(&gateway.id)
                && seen.insert(gateway.id.as_str())
        })
        .filter_map(|gateway| gateway.id.parse().ok())
        .collect()
}

fn current_gateway(status: &RcsLinkStatusProjection) -> bool {
    expiry(status.status.observed_at_ms).is_some()
        && status.current
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
    use crate::gateways::{CapabilitiesResponse, DevicesResponse, gateway_views};
    use peppy_client_core::{
        ClientConfig, DatabaseKey, RcsEligibilityProjection, RcsLinkStatusEvent, VaultId,
    };
    use std::path::PathBuf;

    struct ClientFixture {
        client: Client,
        gateway: DeviceId,
        directory: PathBuf,
    }

    impl Drop for ClientFixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.directory);
        }
    }

    fn fixture() -> ClientFixture {
        let gateway = DeviceId::new();
        let directory = std::env::temp_dir().join(format!("desktop-rcs-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();
        let client = Client::open(
            ClientConfig {
                database_path: directory.join("client.db"),
                vault_id: VaultId::new(),
                device_id: gateway,
            },
            DatabaseKey::new(&[7; 32]).unwrap(),
        )
        .unwrap();
        ClientFixture {
            client,
            gateway,
            directory,
        }
    }

    fn gateway_view(gateway: DeviceId) -> GatewayView {
        GatewayView {
            id: gateway.to_string(),
            name: "Gateway".into(),
            sim_id: "sim".into(),
            online: true,
            simulated: false,
            supports_sms: true,
            supports_mms: true,
            availability: crate::GatewayAvailability::Connected,
            last_seen_age_seconds: None,
            presence: None,
            wake_route: false,
            capability_note: None,
            mms_content_version: None,
            mms_max_bytes: None,
            mms_limit_source: None,
            mms_max_recipients: None,
        }
    }

    #[test]
    fn snapshot_projects_real_core_rcs_freshness_boundaries() {
        let fixture = fixture();
        let conversation = ConversationId::new();
        let observed_at_ms = 1_000;
        fixture
            .client
            .publish_rcs_capability(
                &RcsLinkStatusEvent {
                    gateway_device_id: fixture.gateway,
                    state: RcsCapabilityState::Linked,
                    availability: true,
                    health: true,
                    phone_responsive: true,
                    observed_at_ms,
                    capability_revision: 1,
                    feature_bits: 1,
                },
                &[RcsEligibilityProjection {
                    conversation_id: conversation,
                    eligibility: RcsEligibility::Eligible,
                    feature_bits: 1,
                    mapping_revision: 1,
                    observed_at_ms,
                    gateway_device_id: fixture.gateway,
                }],
                observed_at_ms,
            )
            .unwrap();

        let snapshot = snapshot_at(
            &fixture.client,
            &[gateway_view(fixture.gateway)],
            &HashSet::new(),
            SnapshotAccess {
                active_conversation: Some(conversation),
                active_is_existing: true,
                roster_validated: true,
                unlocked: true,
                now_ms: observed_at_ms + RCS_CAPABILITY_STALE_MS,
            },
        )
        .unwrap()
        .unwrap();

        assert!(snapshot.gateways[0].current);
        assert_eq!(
            snapshot.eligibilities[0].eligibility,
            RcsEligibilityState::Eligible
        );

        for now_ms in [
            observed_at_ms + RCS_CAPABILITY_STALE_MS + 1,
            observed_at_ms - 1,
        ] {
            let snapshot = snapshot_at(
                &fixture.client,
                &[gateway_view(fixture.gateway)],
                &HashSet::new(),
                SnapshotAccess {
                    active_conversation: Some(conversation),
                    active_is_existing: true,
                    roster_validated: true,
                    unlocked: true,
                    now_ms,
                },
            )
            .unwrap()
            .unwrap();

            assert!(!snapshot.gateways[0].current);
            assert!(snapshot.eligibilities.is_empty());
        }
    }

    #[test]
    fn snapshot_projects_a_fresh_terminal_core_status_without_eligibility() {
        let fixture = fixture();
        let observed_at_ms = 1_000;
        fixture
            .client
            .publish_rcs_capability(
                &RcsLinkStatusEvent {
                    gateway_device_id: fixture.gateway,
                    state: RcsCapabilityState::Unlinked,
                    availability: false,
                    health: false,
                    phone_responsive: false,
                    observed_at_ms,
                    capability_revision: 1,
                    feature_bits: 0,
                },
                &[],
                observed_at_ms,
            )
            .unwrap();

        let snapshot = snapshot_at(
            &fixture.client,
            &[gateway_view(fixture.gateway)],
            &HashSet::new(),
            SnapshotAccess {
                active_conversation: Some(ConversationId::new()),
                active_is_existing: true,
                roster_validated: true,
                unlocked: true,
                now_ms: observed_at_ms,
            },
        )
        .unwrap()
        .unwrap();

        assert_eq!(snapshot.gateways[0].state, RcsGatewayState::Unlinked);
        assert!(snapshot.gateways[0].current);
        assert!(!snapshot.gateways[0].healthy);
        assert!(snapshot.eligibilities.is_empty());
    }

    #[test]
    fn expiry_matches_core_inclusive_currentness_boundary() {
        assert_eq!(expiry(1_000), Some(1_000 + RCS_CAPABILITY_STALE_MS + 1));
        assert_eq!(expiry(-1), None);
        assert_eq!(expiry(i64::MAX), None);
    }

    #[test]
    fn ios_sms_report_is_not_an_rcs_gateway() {
        let id = uuid::Uuid::new_v4().to_string();
        let devices: DevicesResponse = serde_json::from_value(serde_json::json!({
            "devices": [{"device_id": id, "role": "owner", "kind": "ios", "revoked": false}]
        }))
        .unwrap();
        let capabilities: CapabilitiesResponse = serde_json::from_value(serde_json::json!({
            "capabilities": [{
                "device_id": id,
                "simulator": false,
                "capabilities": {"sims": [{"subscription_id": "sim", "sms": "available", "mms": "unavailable"}]}
            }]
        }))
        .unwrap();
        let gateways = gateway_views(&devices, &capabilities);

        assert!(gateways[0].supports_sms);
        assert_eq!(
            gateways[0].availability,
            crate::GatewayAvailability::UnsupportedPlatform
        );
        assert!(validated_gateway_ids(&gateways, &HashSet::new()).is_empty());
    }

    #[test]
    fn simulated_sms_report_is_not_an_rcs_gateway() {
        let id = uuid::Uuid::new_v4().to_string();
        let devices: DevicesResponse = serde_json::from_value(serde_json::json!({
            "devices": [{"device_id": id, "role": "gateway", "revoked": false}]
        }))
        .unwrap();
        let capabilities: CapabilitiesResponse = serde_json::from_value(serde_json::json!({
            "capabilities": [{
                "device_id": id,
                "simulator": true,
                "capabilities": {"sims": [{"subscription_id": "sim", "sms": "available", "mms": "unavailable"}]}
            }]
        }))
        .unwrap();
        let gateways = gateway_views(&devices, &capabilities);

        assert!(gateways[0].supports_sms && gateways[0].simulated);
        assert!(validated_gateway_ids(&gateways, &HashSet::new()).is_empty());
    }

    #[test]
    fn current_gateway_rejects_terminal_and_overflowing_statuses() {
        let gateway = DeviceId::new();
        let status = |state, observed_at_ms, current| RcsLinkStatusProjection {
            status: peppy_client_core::RcsLinkStatusEvent {
                gateway_device_id: gateway,
                state,
                availability: true,
                health: true,
                phone_responsive: true,
                observed_at_ms,
                capability_revision: u64::MAX,
                feature_bits: u32::MAX,
            },
            current,
        };

        assert!(current_gateway(&status(
            RcsCapabilityState::Linked,
            1,
            true
        )));
        assert!(!current_gateway(&status(
            RcsCapabilityState::Unlinked,
            1,
            true
        )));
        assert!(!current_gateway(&status(
            RcsCapabilityState::Linked,
            i64::MAX,
            true
        )));
        assert!(!current_gateway(&status(
            RcsCapabilityState::Linked,
            1,
            false
        )));
        let view = gateway_status(gateway, &status(RcsCapabilityState::Linked, 1, true));
        assert_eq!(view.capability_revision, u64::MAX.to_string());
        assert_eq!(view.observed_at_ms, "1");
    }
}
