use peppy_client_core::{
    Client, ConversationId, DeviceId, LiveClock, LiveEnableContext, LiveEnableGeneration,
    LiveError, LiveExecutor,
};
use peppy_desktop_api::rcs_status::{
    snapshot_at, RcsEligibilityState, RcsGatewayState, RcsStatusSnapshot, SnapshotAccess,
};

use crate::session::NetStatus;

pub(crate) type GatewayDeviceId = DeviceId;

pub(crate) struct RcsSnapshotInputs<'a> {
    pub client: &'a Client,
    pub now_ms: i64,
}

pub(crate) fn resolve_typing_target(
    status: &NetStatus,
    inputs: RcsSnapshotInputs<'_>,
    conversation_id: ConversationId,
) -> Option<GatewayDeviceId> {
    let snapshot = snapshot(status, inputs, Some(conversation_id))?;
    typing_target(&snapshot, conversation_id)
}

pub(crate) fn enable_context(
    status: &NetStatus,
    inputs: RcsSnapshotInputs<'_>,
) -> Option<LiveEnableContext> {
    let trusted_roster = trusted_roster(status)?;
    let snapshot = snapshot(status, inputs, None)?;
    let gateway = unique_current_linked_gateway(&snapshot)?;
    trusted_roster
        .contains(&gateway)
        .then_some(LiveEnableContext {
            trusted_roster,
            local_gateway_device_id: gateway,
            local_executor: LiveExecutor::None,
        })
}

fn snapshot(
    status: &NetStatus,
    inputs: RcsSnapshotInputs<'_>,
    active_conversation: Option<ConversationId>,
) -> Option<RcsStatusSnapshot> {
    let active_is_existing = active_conversation.is_some();
    snapshot_at(
        inputs.client,
        &status.gateways,
        &status.revoked_device_ids,
        SnapshotAccess {
            active_conversation,
            active_is_existing,
            roster_validated: status.rcs_roster_validated,
            unlocked: true,
            now_ms: inputs.now_ms,
        },
    )
    .ok()
    .flatten()
}

fn trusted_roster(status: &NetStatus) -> Option<Vec<DeviceId>> {
    (!status.rcs_live_roster.is_empty() && status.rcs_roster_validated)
        .then(|| {
            status
                .rcs_live_roster
                .iter()
                .map(|device_id| device_id.parse().ok())
                .collect()
        })
        .flatten()
}

fn typing_target(
    snapshot: &RcsStatusSnapshot,
    conversation_id: ConversationId,
) -> Option<DeviceId> {
    let candidates: Vec<_> = snapshot
        .eligibilities
        .iter()
        .filter(|eligibility| {
            eligibility.conversation_id == conversation_id.to_string()
                && eligibility.eligibility == RcsEligibilityState::Eligible
                && snapshot.gateways.iter().any(|gateway| {
                    gateway.gateway_id == eligibility.gateway_id
                        && gateway.current
                        && gateway.state == RcsGatewayState::Linked
                })
        })
        .filter_map(|eligibility| eligibility.gateway_id.parse().ok())
        .collect();
    unique(candidates)
}

fn unique_current_linked_gateway(snapshot: &RcsStatusSnapshot) -> Option<DeviceId> {
    unique(
        snapshot
            .gateways
            .iter()
            .filter(|gateway| gateway.current && gateway.state == RcsGatewayState::Linked)
            .filter_map(|gateway| gateway.gateway_id.parse().ok())
            .collect(),
    )
}

fn unique(candidates: Vec<DeviceId>) -> Option<DeviceId> {
    let candidate = candidates.first()?.to_owned();
    candidates
        .iter()
        .all(|other| *other == candidate)
        .then_some(candidate)
}

#[derive(Default)]
pub(crate) struct LiveTypingState {
    context: Option<LiveEnableContext>,
    generation: Option<LiveEnableGeneration>,
}

impl LiveTypingState {
    pub(crate) fn generation(&self) -> Option<LiveEnableGeneration> {
        self.generation
    }

    pub(crate) fn apply(
        &mut self,
        client: &Client,
        context: Option<LiveEnableContext>,
        clock: LiveClock,
    ) -> Result<Option<LiveEnableGeneration>, LiveError> {
        if context == self.context {
            return Ok(self.generation);
        }
        let generation = match context.as_ref() {
            Some(context) => Some(client.enable_live_typing(context.clone(), clock)?),
            None => {
                client
                    .disable_live_typing()
                    .map_err(|_| LiveError::InvalidContext)?;
                None
            }
        };
        self.context = context;
        self.generation = generation;
        Ok(generation)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        session::Session,
        sync::apply_gateway_refresh,
        tests::{fixture_at, gateway, open, PHRASE},
    };
    use peppy_client_core::{
        RcsCapabilityState, RcsEligibility, RcsEligibilityProjection, RcsLinkStatusEvent,
    };
    use peppy_desktop_api::{
        gateways::{CapabilitiesResponse, DevicesResponse},
        RcsConversationEligibility, RcsGatewayStatus,
    };

    const NOW_MS: i64 = 10_000;

    fn inputs(session: &Session) -> RcsSnapshotInputs<'_> {
        RcsSnapshotInputs {
            client: &session.client,
            now_ms: NOW_MS,
        }
    }

    fn clock() -> LiveClock {
        LiveClock {
            wall_ms: NOW_MS,
            mono_ms: 1,
        }
    }

    fn roster_refresh(session: &Session, device_id: &str, revoked: bool) {
        let devices: DevicesResponse = serde_json::from_value(serde_json::json!({"devices": [
            {"device_id": device_id, "role": "gateway", "revoked": revoked}
        ]}))
        .unwrap();
        let capabilities: CapabilitiesResponse =
            serde_json::from_value(serde_json::json!({"capabilities": []})).unwrap();
        apply_gateway_refresh(session, &devices, &capabilities);
    }

    fn install_gateway(session: &Session, device_id: &str) {
        session.set_status(|status| {
            let mut view = gateway(device_id, true, false);
            view.simulated = false;
            status.gateways = vec![view];
        });
    }

    fn session_with_linked_eligible_gateway() -> (crate::tests::Fixture, Session, ConversationId) {
        let fixture = fixture_at("http://127.0.0.1:9");
        let session = open(&fixture, &fixture.binding, &[]);
        session
            .client
            .unlock(&fixture.profile, &fixture.header, PHRASE)
            .unwrap();
        let gateway_device_id: DeviceId = fixture.binding.device_id.parse().unwrap();
        let conversation_id = ConversationId::new();
        session
            .client
            .publish_rcs_capability(
                &RcsLinkStatusEvent {
                    gateway_device_id,
                    state: RcsCapabilityState::Linked,
                    availability: true,
                    health: true,
                    phone_responsive: true,
                    observed_at_ms: NOW_MS,
                    capability_revision: 1,
                    feature_bits: 1,
                },
                &[RcsEligibilityProjection {
                    conversation_id,
                    eligibility: RcsEligibility::Eligible,
                    feature_bits: 1,
                    mapping_revision: 1,
                    observed_at_ms: NOW_MS,
                    gateway_device_id,
                }],
                NOW_MS,
            )
            .unwrap();
        install_gateway(&session, &fixture.binding.device_id);
        (fixture, session, conversation_id)
    }

    #[test]
    fn no_roster_does_not_enable_live_typing() {
        let (_, session, _) = session_with_linked_eligible_gateway();
        let status = session.status.lock().unwrap();
        let mut state = LiveTypingState::default();
        assert_eq!(
            state
                .apply(
                    &session.client,
                    enable_context(&status, inputs(&session)),
                    clock()
                )
                .unwrap(),
            None
        );
        assert_eq!(
            session.client.set_typing_sharing(true),
            Err(LiveError::Disabled)
        );
    }

    #[test]
    fn gateway_refresh_roster_enables_live_typing() {
        let (fixture, session, _) = session_with_linked_eligible_gateway();
        roster_refresh(&session, &fixture.binding.device_id, false);
        install_gateway(&session, &fixture.binding.device_id);
        let status = session.status.lock().unwrap();
        let context = enable_context(&status, inputs(&session)).unwrap();
        assert_eq!(
            context.local_executor,
            peppy_client_core::LiveExecutor::None
        );
        assert_eq!(context.trusted_roster.len(), 1);
        let generation = LiveTypingState::default()
            .apply(&session.client, Some(context), clock())
            .unwrap();
        assert!(generation.is_some());
    }

    #[test]
    fn revocation_disables_live_typing() {
        let (fixture, session, _) = session_with_linked_eligible_gateway();
        roster_refresh(&session, &fixture.binding.device_id, false);
        install_gateway(&session, &fixture.binding.device_id);
        let mut state = LiveTypingState::default();
        let context = {
            let status = session.status.lock().unwrap();
            enable_context(&status, inputs(&session))
        };
        state.apply(&session.client, context, clock()).unwrap();
        roster_refresh(&session, &fixture.binding.device_id, true);
        let context = {
            let status = session.status.lock().unwrap();
            enable_context(&status, inputs(&session))
        };
        state.apply(&session.client, context, clock()).unwrap();
        assert_eq!(state.generation(), None);
        assert_eq!(
            session.client.set_typing_sharing(true),
            Err(LiveError::Disabled)
        );
    }

    #[test]
    fn ineligible_conversation_has_no_target() {
        let (_, session, _) = session_with_linked_eligible_gateway();
        let status = session.status.lock().unwrap();
        assert!(resolve_typing_target(&status, inputs(&session), ConversationId::new()).is_none());
    }

    #[test]
    fn ambiguous_gateways_have_no_target() {
        let conversation_id = ConversationId::new();
        let gateway_a = DeviceId::new();
        let gateway_b = DeviceId::new();
        let snapshot = RcsStatusSnapshot {
            gateways: vec![gateway_status(gateway_a), gateway_status(gateway_b)],
            eligibilities: vec![
                eligibility(conversation_id, gateway_a),
                eligibility(conversation_id, gateway_b),
            ],
        };
        assert!(typing_target(&snapshot, conversation_id).is_none());
    }

    #[test]
    fn roster_change_creates_a_new_generation() {
        let (fixture, session, _) = session_with_linked_eligible_gateway();
        roster_refresh(&session, &fixture.binding.device_id, false);
        install_gateway(&session, &fixture.binding.device_id);
        let mut state = LiveTypingState::default();
        let first = {
            let status = session.status.lock().unwrap();
            state.apply(
                &session.client,
                enable_context(&status, inputs(&session)),
                clock(),
            )
        }
        .unwrap()
        .unwrap();
        session.set_status(|status| status.rcs_live_roster.push(DeviceId::new().to_string()));
        let second = {
            let status = session.status.lock().unwrap();
            state.apply(
                &session.client,
                enable_context(&status, inputs(&session)),
                clock(),
            )
        }
        .unwrap()
        .unwrap();
        assert_ne!(first, second);
    }

    fn gateway_status(gateway_id: DeviceId) -> RcsGatewayStatus {
        RcsGatewayStatus {
            gateway_id: gateway_id.to_string(),
            state: RcsGatewayState::Linked,
            current: true,
            available: true,
            healthy: true,
            phone_responsive: true,
            observed_at_ms: NOW_MS.to_string(),
            capability_revision: "1".into(),
            expires_at_ms: Some((NOW_MS + 1).to_string()),
        }
    }

    fn eligibility(
        conversation_id: ConversationId,
        gateway_id: DeviceId,
    ) -> RcsConversationEligibility {
        RcsConversationEligibility {
            conversation_id: conversation_id.to_string(),
            gateway_id: gateway_id.to_string(),
            eligibility: RcsEligibilityState::Eligible,
            observed_at_ms: NOW_MS.to_string(),
            mapping_revision: "1".into(),
            expires_at_ms: Some((NOW_MS + 1).to_string()),
        }
    }
}
