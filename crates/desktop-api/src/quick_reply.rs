//! Desktop-facing conversion for notification replies. Route health remains advisory here; core
//! resolves recipients and revalidates transport and group state at queue time.
use crate::{
    GatewayView,
    compose::{ComposeError, gateway_sendable},
};
use peppy_client_core::{
    Client, CommandId, ConversationId, DeviceId, GatewayRoute, QueuedSend,
    QuickReplyInput as CoreQuickReply, RcsCapabilityState, RcsEligibility, Transport,
};
use serde::Deserialize;
use std::str::FromStr;
use std::time::{SystemTime, UNIX_EPOCH};

/// Read-only RCS facts used by the desktop advisory policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RcsSendPolicyInput {
    pub known_rcs_chat: bool,
    pub gateway_state: Option<RcsCapabilityState>,
    pub gateway_current: bool,
    pub gateway_available: bool,
    pub gateway_healthy: bool,
    pub gateway_phone_responsive: bool,
    pub gateway_expires_at_ms: Option<i64>,
    pub eligibility: Option<RcsEligibility>,
    pub eligibility_expires_at_ms: Option<i64>,
    pub now_ms: i64,
}

/// The desktop's non-authoritative RCS send advice.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RcsSendBlock {
    Allow,
    Unproven,
    Relink,
    Phone,
}

/// Mirrors the renderer's RCS send advice without granting carrier fallback authority.
pub fn rcs_send_block(input: RcsSendPolicyInput) -> RcsSendBlock {
    let gateway_is_current = input.gateway_state == Some(RcsCapabilityState::Linked)
        && input.gateway_current
        && input.gateway_available
        && input.gateway_healthy
        && input.gateway_phone_responsive
        && expires_after(input.gateway_expires_at_ms, input.now_ms);
    let eligibility_is_current = input.eligibility == Some(RcsEligibility::Eligible)
        && expires_after(input.eligibility_expires_at_ms, input.now_ms);
    if gateway_is_current && eligibility_is_current {
        return RcsSendBlock::Allow;
    }
    if !input.known_rcs_chat
        || input
            .gateway_state
            .is_none_or(|state| state == RcsCapabilityState::Unlinked)
    {
        return RcsSendBlock::Allow;
    }
    if matches!(
        input.gateway_state,
        Some(RcsCapabilityState::NeedsRelink | RcsCapabilityState::SignedOut)
    ) {
        return RcsSendBlock::Relink;
    }
    if input.gateway_state == Some(RcsCapabilityState::Unavailable) || !gateway_is_current {
        return RcsSendBlock::Phone;
    }
    if input.eligibility == Some(RcsEligibility::NotEligible)
        && expires_after(input.eligibility_expires_at_ms, input.now_ms)
    {
        RcsSendBlock::Allow
    } else {
        RcsSendBlock::Unproven
    }
}

fn expires_after(expires_at_ms: Option<i64>, now_ms: i64) -> bool {
    expires_at_ms.is_some_and(|expires_at_ms| now_ms < expires_at_ms)
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QuickReplyInput {
    pub command_id: String,
    pub conversation_id: String,
    pub body: String,
    pub gateway_id: String,
    #[serde(default)]
    pub sim_id: Option<String>,
    pub expected_transport: Transport,
    #[serde(default)]
    pub expected_member_revision: Option<String>,
}

/// Applies the host's current gateway availability check before queueing the pinned reply.
pub fn queue_quick_reply(
    client: &Client,
    input: QuickReplyInput,
    gateways: Option<&[GatewayView]>,
) -> Result<QueuedSend, ComposeError> {
    let input = core_input(input)?;
    if let Some(queued) = client
        .reconcile_quick_reply(&input)
        .map_err(ComposeError::Core)?
    {
        return Ok(queued);
    }
    let gateway = gateways
        .and_then(|gateways| {
            gateways
                .iter()
                .find(|gateway| route_matches_gateway(&input.route, gateway))
        })
        .ok_or_else(|| {
            ui(
                "gateway-unavailable",
                "The selected phone is no longer available.",
            )
        })?;
    gateway_sendable(gateway)?;
    match input.route {
        GatewayRoute::Carrier { .. } => validate_carrier_route(client, &input, gateway)?,
        GatewayRoute::RcsLink { gateway_device_id } => {
            validate_rcs_route(client, input.conversation_id, gateway_device_id)?;
        }
    }
    client.queue_quick_reply(input).map_err(ComposeError::Core)
}

/// Reconciles an existing notification reply before the host performs mutable preflight.
pub fn reconcile_quick_reply(
    client: &Client,
    input: QuickReplyInput,
) -> Result<Option<QueuedSend>, ComposeError> {
    client
        .reconcile_quick_reply(&core_input(input)?)
        .map_err(ComposeError::Core)
}

fn validate_carrier_route(
    client: &Client,
    input: &CoreQuickReply,
    gateway: &GatewayView,
) -> Result<(), ComposeError> {
    if input.expected_transport == Transport::Sms && !gateway.supports_sms {
        return Err(ui(
            "gateway-unsupported",
            "The selected phone cannot send this reply.",
        ));
    }
    if input.expected_transport != Transport::Mms {
        return Ok(());
    }
    if !gateway.supports_mms {
        return Err(ui(
            "mms-unsupported",
            "The selected phone/SIM cannot send MMS.",
        ));
    }
    let context = client
        .quick_reply_context(input.conversation_id)
        .map_err(ComposeError::Core)?;
    let max_recipients = gateway.mms_max_recipients.unwrap_or(20);
    if context.recipients.len() > max_recipients {
        return Err(ui(
            "mms-recipients-unsupported",
            "The selected phone cannot send to every group member.",
        ));
    }
    let content_version = gateway.mms_content_version.ok_or_else(|| {
        ui(
            "mms-capabilities-unknown",
            "The selected phone has not reported MMS capabilities.",
        )
    })?;
    if content_version == 0 {
        return Err(ui("mms-unsupported", "The selected phone cannot send MMS."));
    }
    if let Some(max_bytes) = gateway.mms_max_bytes
        && input.body.len() as u64 > max_bytes
    {
        return Err(ui(
            "mms-too-large",
            "This reply is too large for the selected phone.",
        ));
    }
    Ok(())
}

fn validate_rcs_route(
    client: &Client,
    conversation_id: ConversationId,
    gateway_device_id: DeviceId,
) -> Result<(), ComposeError> {
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| ui("rcs-unavailable", "RCS status is unavailable."))?
        .as_millis()
        .try_into()
        .map_err(|_| ui("rcs-unavailable", "RCS status is unavailable."))?;
    let status = client
        .rcs_link_status(gateway_device_id, now_ms)
        .map_err(ComposeError::Core)?;
    let Some(status) = status else {
        return Err(ui("rcs-unavailable", "RCS status is unavailable."));
    };
    let eligibility = client
        .rcs_eligibility_projection(conversation_id, gateway_device_id, now_ms)
        .map_err(ComposeError::Core)?;
    let gateway_expires_at_ms = status
        .status
        .observed_at_ms
        .checked_add(peppy_client_core::RCS_CAPABILITY_STALE_MS)
        .and_then(|value| value.checked_add(1));
    let policy = rcs_send_block(RcsSendPolicyInput {
        known_rcs_chat: true,
        gateway_state: Some(status.status.state),
        gateway_current: status.current,
        gateway_available: status.status.availability,
        gateway_healthy: status.status.health,
        gateway_phone_responsive: status.status.phone_responsive,
        gateway_expires_at_ms,
        eligibility: eligibility.as_ref().map(|value| value.eligibility),
        eligibility_expires_at_ms: eligibility.and(gateway_expires_at_ms),
        now_ms,
    });
    match policy {
        RcsSendBlock::Allow if status.status.state == RcsCapabilityState::Linked => Ok(()),
        RcsSendBlock::Relink => Err(ui("rcs-relink", "RCS needs to be linked again.")),
        RcsSendBlock::Unproven => Err(ui(
            "rcs-unproven",
            "RCS eligibility has not been confirmed.",
        )),
        RcsSendBlock::Allow | RcsSendBlock::Phone => {
            Err(ui("rcs-unavailable", "RCS is not currently available."))
        }
    }
}

fn core_input(input: QuickReplyInput) -> Result<CoreQuickReply, ComposeError> {
    let route = match input.sim_id.filter(|sim_id| !sim_id.is_empty()) {
        Some(subscription_id) => GatewayRoute::Carrier {
            gateway_device_id: parse_device(&input.gateway_id)?,
            subscription_id,
        },
        None => GatewayRoute::RcsLink {
            gateway_device_id: parse_device(&input.gateway_id)?,
        },
    };
    let expected_member_revision = input
        .expected_member_revision
        .map(|revision| {
            revision
                .parse()
                .map_err(|_| ui("invalid-group", "The group revision is invalid."))
        })
        .transpose()?;
    Ok(CoreQuickReply {
        command_id: CommandId::from_str(&input.command_id)
            .map_err(|_| ui("invalid-reply", "The reply identifier is invalid."))?,
        conversation_id: ConversationId::from_str(&input.conversation_id)
            .map_err(|_| ui("invalid-conversation", "The conversation is invalid."))?,
        body: input.body,
        route,
        expected_transport: input.expected_transport,
        expected_member_revision,
    })
}

fn parse_device(value: &str) -> Result<DeviceId, ComposeError> {
    DeviceId::from_str(value).map_err(|_| ui("invalid-route", "The phone ID is invalid."))
}

fn route_matches_gateway(route: &GatewayRoute, gateway: &GatewayView) -> bool {
    match route {
        GatewayRoute::Carrier {
            gateway_device_id,
            subscription_id,
        } => gateway.id == gateway_device_id.to_string() && gateway.sim_id == *subscription_id,
        GatewayRoute::RcsLink { gateway_device_id } => gateway.id == gateway_device_id.to_string(),
    }
}

fn ui(code: &'static str, message: &'static str) -> ComposeError {
    ComposeError::Ui {
        code,
        message: message.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct PolicyFixture {
        gateway: RcsCapabilityState,
        #[serde(default)]
        current: bool,
        #[serde(default)]
        available: bool,
        #[serde(default)]
        healthy: bool,
        #[serde(default)]
        responsive: bool,
        #[serde(default)]
        eligible: Option<RcsEligibility>,
        #[serde(default)]
        expired: bool,
        #[serde(default = "known_rcs")]
        known_rcs: bool,
        expected: String,
    }

    fn known_rcs() -> bool {
        true
    }

    #[test]
    fn rcs_route_accepts_old_eligibility_while_the_gateway_status_is_fresh() {
        use peppy_client_core::{
            ClientConfig, DatabaseKey, RcsEligibilityProjection, RcsLinkStatusEvent, VaultId,
        };

        const DAY_MS: i64 = 24 * 60 * 60 * 1_000;
        let gateway = DeviceId::new();
        let conversation = ConversationId::new();
        let directory =
            std::env::temp_dir().join(format!("desktop-quick-reply-{}", uuid::Uuid::new_v4()));
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
        let now_ms: i64 = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis()
            .try_into()
            .unwrap();
        client
            .publish_rcs_capability(
                &RcsLinkStatusEvent {
                    gateway_device_id: gateway,
                    state: RcsCapabilityState::Linked,
                    availability: true,
                    health: true,
                    phone_responsive: true,
                    observed_at_ms: now_ms,
                    capability_revision: 1,
                    feature_bits: 0,
                    linked_since_ms: Some(now_ms - DAY_MS),
                },
                &[RcsEligibilityProjection {
                    conversation_id: conversation,
                    eligibility: RcsEligibility::Eligible,
                    feature_bits: 0,
                    mapping_revision: 1,
                    observed_at_ms: now_ms - DAY_MS,
                    gateway_device_id: gateway,
                }],
                now_ms,
            )
            .unwrap();

        let result = validate_rcs_route(&client, conversation, gateway);
        drop(client);
        let _ = std::fs::remove_dir_all(&directory);

        assert!(result.is_ok());
    }

    #[test]
    fn quick_reply_rejects_an_invalid_gateway_id() {
        assert!(parse_device("not-a-device").is_err());
    }

    #[test]
    fn rcs_send_policy_matches_shared_notification_fixture() {
        let fixtures: Vec<PolicyFixture> = serde_json::from_str(include_str!(
            "../tests/fixtures/notification-rcs-policy.json"
        ))
        .expect("shared policy fixture is valid");

        for fixture in fixtures {
            let expires_at_ms = (!fixture.expired).then_some(101);
            let actual = rcs_send_block(RcsSendPolicyInput {
                known_rcs_chat: fixture.known_rcs,
                gateway_state: Some(fixture.gateway),
                gateway_current: fixture.current,
                gateway_available: fixture.available,
                gateway_healthy: fixture.healthy,
                gateway_phone_responsive: fixture.responsive,
                gateway_expires_at_ms: expires_at_ms,
                eligibility: fixture.eligible,
                eligibility_expires_at_ms: expires_at_ms,
                now_ms: 100,
            });
            let expected = match fixture.expected.as_str() {
                "allow" => RcsSendBlock::Allow,
                "phone" => RcsSendBlock::Phone,
                "relink" => RcsSendBlock::Relink,
                "unproven" => RcsSendBlock::Unproven,
                unexpected => panic!("unknown fixture result: {unexpected}"),
            };
            assert_eq!(actual, expected, "fixture: {}", fixture.expected);
        }
    }

    #[test]
    fn rcs_send_policy_keeps_old_eligibility_current_until_gateway_deadline() {
        let status_deadline = 1_001;
        let input = RcsSendPolicyInput {
            known_rcs_chat: true,
            gateway_state: Some(RcsCapabilityState::Linked),
            gateway_current: true,
            gateway_available: true,
            gateway_healthy: true,
            gateway_phone_responsive: true,
            gateway_expires_at_ms: Some(status_deadline),
            eligibility: Some(RcsEligibility::Eligible),
            eligibility_expires_at_ms: Some(status_deadline),
            now_ms: 1_000,
        };

        assert_eq!(rcs_send_block(input), RcsSendBlock::Allow);
        assert_eq!(
            rcs_send_block(RcsSendPolicyInput {
                now_ms: status_deadline,
                ..input
            }),
            RcsSendBlock::Phone
        );
    }
}
