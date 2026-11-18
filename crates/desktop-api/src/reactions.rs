//! Reaction views and host routing shared by the desktop and browser hosts. Core decides what a
//! reaction may do. This module supplies what the host knows about the composer's route: the
//! route's gateway, its presence, and the text route. Wire names follow the UI contract.

use crate::{GatewayView, compose::find_route};
use peppy_client_core::{
    Client, Conversation, ConversationFolder, ConversationId, Error, GatewayRoute, MessageId,
    MessageReaction, OwnReactionRequest, OwnReactionState, ReactionRefusal, ReactionRequestId,
    ReactionRoute, ReactionSubmit, ReactionSupport, SubmitRefusal,
};
use serde::Serialize;
use std::str::FromStr;

const NONE: &str = "none";

#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageReactionView {
    pub emoji: String,
    pub actors: Vec<ReactionActorView>,
}

#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReactionActorView {
    #[serde(rename = "self")]
    pub is_self: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub address: Option<String>,
}

#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OwnReactionRequestView {
    pub request_id: String,
    /// `null` asks for the user's reaction to be removed.
    pub emoji: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous_emoji: Option<String>,
    pub state: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refusal: Option<&'static str>,
}

#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReactionSubmitView {
    pub accepted: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub route: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<&'static str>,
}

/// The reaction route a conversation offers. `gateway_online` is the presence fact core takes as
/// `gateway_online`; `text_route` is the composer's route for text reactions.
#[derive(Debug, PartialEq, Eq)]
pub struct ConversationReactions {
    pub support: &'static str,
    pub gateway_id: Option<String>,
    pub gateway_online: bool,
    pub text_route: Option<GatewayRoute>,
}

impl ConversationReactions {
    fn none() -> Self {
        Self {
            support: NONE,
            gateway_id: None,
            gateway_online: false,
            text_route: None,
        }
    }
}

/// Reaction facts for one message. `None` when no reaction is known, so the field is omitted.
pub fn reactions_view(reactions: &[MessageReaction]) -> Option<Vec<MessageReactionView>> {
    (!reactions.is_empty()).then(|| {
        reactions
            .iter()
            .map(|reaction| MessageReactionView {
                emoji: reaction.emoji.clone(),
                actors: reaction
                    .actors
                    .iter()
                    .map(|actor| ReactionActorView {
                        is_self: actor.is_self,
                        address: actor.address.clone(),
                    })
                    .collect(),
            })
            .collect()
    })
}

pub fn own_reaction_request_view(request: &OwnReactionRequest) -> OwnReactionRequestView {
    OwnReactionRequestView {
        request_id: request.request_id.to_string(),
        emoji: request.emoji.clone(),
        previous_emoji: request.previous_emoji.clone(),
        state: match request.state {
            OwnReactionState::Sending => "sending",
            OwnReactionState::NotSent => "not_sent",
            OwnReactionState::Unknown => "unknown",
        },
        refusal: request.refusal.as_ref().map(refusal_wire),
    }
}

pub fn reactable_wire(route: Option<ReactionRoute>) -> Option<&'static str> {
    route.map(route_wire)
}

pub fn submit_view(submit: ReactionSubmit) -> ReactionSubmitView {
    ReactionSubmitView {
        accepted: submit.accepted,
        request_id: submit.request_id.map(|id| id.to_string()),
        route: submit.route.map(route_wire),
        reason: submit.reason.map(submit_refusal_wire),
    }
}

fn unsupported() -> ReactionSubmitView {
    ReactionSubmitView {
        accepted: false,
        request_id: None,
        route: None,
        reason: Some(submit_refusal_wire(SubmitRefusal::Unsupported)),
    }
}

fn route_wire(route: ReactionRoute) -> &'static str {
    match route {
        ReactionRoute::Native => "native",
        ReactionRoute::Text => "text",
    }
}

fn refusal_wire(refusal: &ReactionRefusal) -> &'static str {
    match refusal {
        ReactionRefusal::Expired => "expired",
        ReactionRefusal::MessageUnavailable => "message_unavailable",
        ReactionRefusal::NotRcs => "not_rcs",
        ReactionRefusal::LinkUnavailable => "link_unavailable",
        ReactionRefusal::Rejected => "rejected",
        ReactionRefusal::Busy => "busy",
        ReactionRefusal::InvalidEmoji => "invalid_emoji",
    }
}

fn submit_refusal_wire(reason: SubmitRefusal) -> &'static str {
    match reason {
        SubmitRefusal::Offline => "offline",
        SubmitRefusal::Unsupported => "unsupported",
        SubmitRefusal::NeedsPhoneUpdate => "needs_phone_update",
        SubmitRefusal::Busy => "busy",
        SubmitRefusal::NotReactable => "not_reactable",
        SubmitRefusal::InvalidEmoji => "invalid_emoji",
    }
}

fn support_wire(support: ReactionSupport) -> &'static str {
    match support {
        ReactionSupport::Native => "native",
        ReactionSupport::Text => "text",
        ReactionSupport::NeedsPhoneUpdate => "needs_phone_update",
        ReactionSupport::None => NONE,
    }
}

/// The gateway the composer would send through for a conversation. A stored route must name a
/// reported gateway and SIM exactly and is never replaced; without one, a single reported
/// gateway is the implicit choice, as in the composer.
fn composer_gateway<'a>(
    gateways: &'a [GatewayView],
    stored: Option<&GatewayRoute>,
) -> Option<&'a GatewayView> {
    match stored {
        Some(GatewayRoute::Carrier {
            gateway_device_id,
            subscription_id,
        }) => find_route(gateways, &gateway_device_id.to_string(), subscription_id),
        _ => match gateways {
            [only] => Some(only),
            _ => None,
        },
    }
}

fn text_reactions(
    gateways: &[GatewayView],
    stored: Option<&GatewayRoute>,
) -> ConversationReactions {
    let Some(gateway) = composer_gateway(gateways, stored).filter(|gateway| gateway.supports_sms)
    else {
        return ConversationReactions::none();
    };
    let Ok(gateway_device_id) = gateway.id.parse() else {
        return ConversationReactions::none();
    };
    ConversationReactions {
        support: "text",
        gateway_id: Some(gateway.id.clone()),
        gateway_online: gateway.online,
        text_route: Some(GatewayRoute::Carrier {
            gateway_device_id,
            subscription_id: gateway.sim_id.clone(),
        }),
    }
}

/// Routing for one conversation. Spam conversations offer no reactions. Native routes use the
/// RCS gateway core chose; text routes use the composer's gateway, which must support SMS.
pub fn conversation_reactions(
    conversation: &Conversation,
    gateways: &[GatewayView],
    stored_route: Option<&GatewayRoute>,
) -> ConversationReactions {
    if conversation.folder == ConversationFolder::Spam {
        return ConversationReactions::none();
    }
    match conversation.reaction_support {
        ReactionSupport::Native | ReactionSupport::NeedsPhoneUpdate => {
            let gateway_id = conversation.reaction_gateway_id.map(|id| id.to_string());
            let gateway_online = gateway_id.as_ref().is_some_and(|id| {
                gateways
                    .iter()
                    .any(|gateway| gateway.id == *id && gateway.online)
            });
            ConversationReactions {
                support: support_wire(conversation.reaction_support),
                gateway_id,
                gateway_online,
                text_route: None,
            }
        }
        ReactionSupport::Text => text_reactions(gateways, stored_route),
        ReactionSupport::None => ConversationReactions::none(),
    }
}

/// Submits one reaction to a message. The host chooses the gateway presence and text route the
/// message's reaction route uses; core applies the rules.
pub fn react(
    client: &Client,
    message_id: MessageId,
    emoji: Option<String>,
    gateways: &[GatewayView],
) -> Result<ReactionSubmitView, Error> {
    let message = client.message_by_id(message_id)?.ok_or(Error::NotFound)?;
    let conversation_id = message.payload.record.conversation_id;
    let conversation = client
        .list_conversations()?
        .into_iter()
        .find(|conversation| conversation.conversation_id == conversation_id);
    let stored = client
        .compose_drafts()?
        .into_iter()
        .find(|draft| draft.conversation_id == conversation_id)
        .and_then(|draft| draft.route);
    let routing = match conversation {
        Some(conversation) => conversation_reactions(&conversation, gateways, stored.as_ref()),
        None => ConversationReactions::none(),
    };
    if message.reactable.is_some() && routing.support == NONE {
        return Ok(unsupported());
    }
    let (gateway_online, text_route) = match message.reactable {
        Some(ReactionRoute::Native) => (routing.gateway_online, None),
        Some(ReactionRoute::Text) => (routing.gateway_online, routing.text_route),
        None => (false, None),
    };
    client
        .react_to_message(message_id, emoji, gateway_online, text_route)
        .map(submit_view)
}

/// Whether a request owned by `owner` may be touched from `scope`. The main window (no scope)
/// may touch any of this device's requests; a composer touches only its own conversation's, and
/// an unknown request (no owner) is never in a composer's scope.
pub fn request_in_scope(owner: Option<ConversationId>, scope: Option<ConversationId>) -> bool {
    match scope {
        None => true,
        Some(conversation) => owner == Some(conversation),
    }
}

/// Result of a scoped dismissal. `OutsideScope` leaves the request untouched.
#[derive(Debug, PartialEq, Eq)]
pub enum DismissScope {
    Dismissed,
    OutsideScope,
}

/// Dismisses this device's own not-sent, unknown, or stale request. Dismissal is local only.
pub fn dismiss(
    client: &Client,
    request_id: &str,
    scope: Option<ConversationId>,
) -> Result<DismissScope, Error> {
    let request_id = ReactionRequestId::from_str(request_id)
        .map_err(|_| Error::InvalidRequest("The reaction request ID is invalid."))?;
    if scope.is_some() {
        let owner = client.reaction_request_conversation(request_id)?;
        if !request_in_scope(owner, scope) {
            return Ok(DismissScope::OutsideScope);
        }
    }
    client.dismiss_reaction_request(request_id)?;
    Ok(DismissScope::Dismissed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use peppy_client_core::{ConversationId, DeviceId, ReactionActor};
    use serde_json::json;

    const GATEWAY: &str = "00000000-0000-4000-8000-000000000001";

    fn gateway(id: &str, sim: &str, online: bool) -> GatewayView {
        serde_json::from_value(json!({
            "id": id,
            "name": "Phone",
            "simId": sim,
            "online": online,
            "simulated": false,
            "supportsSms": true,
            "supportsMms": false,
            "availability": if online { "connected" } else { "offline" },
        }))
        .unwrap()
    }

    fn conversation(
        support: ReactionSupport,
        gateway: Option<DeviceId>,
        folder: ConversationFolder,
    ) -> Conversation {
        Conversation {
            conversation_id: ConversationId::new(),
            unread_count: 0,
            pinned: false,
            folder,
            folder_reason: None,
            unblock: None,
            can_unblock: false,
            reaction_support: support,
            reaction_gateway_id: gateway,
        }
    }

    fn carrier(gateway: &str, sim: &str) -> GatewayRoute {
        GatewayRoute::Carrier {
            gateway_device_id: gateway.parse().unwrap(),
            subscription_id: sim.into(),
        }
    }

    #[test]
    fn reaction_fields_serialize_with_the_contract_names() {
        let reactions = reactions_view(&[MessageReaction {
            emoji: "👍".into(),
            actors: vec![
                ReactionActor {
                    is_self: true,
                    address: None,
                },
                ReactionActor {
                    is_self: false,
                    address: Some("+15555550100".into()),
                },
            ],
        }])
        .unwrap();
        let value = serde_json::to_value(&reactions).unwrap();
        assert_eq!(value[0]["emoji"], "👍");
        assert_eq!(value[0]["actors"][0]["self"], true);
        assert!(value[0]["actors"][0].get("address").is_none());
        assert_eq!(value[0]["actors"][1]["address"], "+15555550100");
        assert!(value[0]["actors"][1].get("is_self").is_none());
        assert!(reactions_view(&[]).is_none());
    }

    #[test]
    fn own_request_serializes_states_refusals_and_a_null_removal() {
        let request = OwnReactionRequest {
            request_id: ReactionRequestId::new(),
            emoji: None,
            previous_emoji: Some("❤️".into()),
            state: OwnReactionState::NotSent,
            refusal: Some(ReactionRefusal::MessageUnavailable),
        };
        let value = serde_json::to_value(own_reaction_request_view(&request)).unwrap();
        assert!(value["requestId"].is_string());
        assert!(value["emoji"].is_null());
        assert_eq!(value["previousEmoji"], "❤️");
        assert_eq!(value["state"], "not_sent");
        assert_eq!(value["refusal"], "message_unavailable");

        for (state, expected) in [
            (OwnReactionState::Sending, "sending"),
            (OwnReactionState::Unknown, "unknown"),
        ] {
            let request = OwnReactionRequest {
                state,
                refusal: None,
                previous_emoji: None,
                ..request.clone()
            };
            let value = serde_json::to_value(own_reaction_request_view(&request)).unwrap();
            assert_eq!(value["state"], expected);
            assert!(value.get("refusal").is_none());
            assert!(value.get("previousEmoji").is_none());
        }
    }

    #[test]
    fn refusal_and_route_values_match_the_contract() {
        for (refusal, expected) in [
            (ReactionRefusal::Expired, "expired"),
            (ReactionRefusal::MessageUnavailable, "message_unavailable"),
            (ReactionRefusal::NotRcs, "not_rcs"),
            (ReactionRefusal::LinkUnavailable, "link_unavailable"),
            (ReactionRefusal::Rejected, "rejected"),
            (ReactionRefusal::Busy, "busy"),
            (ReactionRefusal::InvalidEmoji, "invalid_emoji"),
        ] {
            assert_eq!(refusal_wire(&refusal), expected);
        }
        assert_eq!(reactable_wire(Some(ReactionRoute::Native)), Some("native"));
        assert_eq!(reactable_wire(Some(ReactionRoute::Text)), Some("text"));
        assert_eq!(reactable_wire(None), None);
        for (support, expected) in [
            (ReactionSupport::Native, "native"),
            (ReactionSupport::Text, "text"),
            (ReactionSupport::NeedsPhoneUpdate, "needs_phone_update"),
            (ReactionSupport::None, "none"),
        ] {
            assert_eq!(support_wire(support), expected);
        }
    }

    #[test]
    fn submit_view_serializes_only_present_fields() {
        let accepted = serde_json::to_value(submit_view(ReactionSubmit {
            accepted: true,
            request_id: None,
            route: Some(ReactionRoute::Text),
            reason: None,
        }))
        .unwrap();
        assert_eq!(accepted, json!({"accepted": true, "route": "text"}));

        let refused = serde_json::to_value(submit_view(ReactionSubmit {
            accepted: false,
            request_id: None,
            route: None,
            reason: Some(SubmitRefusal::NeedsPhoneUpdate),
        }))
        .unwrap();
        assert_eq!(
            refused,
            json!({"accepted": false, "reason": "needs_phone_update"})
        );
    }

    #[test]
    fn native_routing_reports_the_gateway_presence_the_composer_sees() {
        let gateway_id: DeviceId = GATEWAY.parse().unwrap();
        let gateways = [gateway(GATEWAY, "sim-1", false)];
        let routing = conversation_reactions(
            &conversation(
                ReactionSupport::Native,
                Some(gateway_id),
                ConversationFolder::Inbox,
            ),
            &gateways,
            None,
        );
        assert_eq!(routing.support, "native");
        assert_eq!(routing.gateway_id.as_deref(), Some(GATEWAY));
        assert!(!routing.gateway_online);
        assert!(routing.text_route.is_none());

        let online = [gateway(GATEWAY, "sim-1", true)];
        let routing = conversation_reactions(
            &conversation(
                ReactionSupport::Native,
                Some(gateway_id),
                ConversationFolder::Inbox,
            ),
            &online,
            None,
        );
        assert!(routing.gateway_online);
    }

    #[test]
    fn text_routing_follows_the_composer_route_and_refuses_a_stale_one() {
        let text = conversation(ReactionSupport::Text, None, ConversationFolder::Inbox);
        let reported = [gateway(GATEWAY, "sim-1", true)];

        let stored = carrier(GATEWAY, "sim-1");
        let routing = conversation_reactions(&text, &reported, Some(&stored));
        assert_eq!(routing.support, "text");
        assert_eq!(routing.text_route, Some(stored));
        assert!(routing.gateway_online);

        let stale = carrier(GATEWAY, "sim-9");
        let routing = conversation_reactions(&text, &reported, Some(&stale));
        assert_eq!(routing.support, "none");
        assert!(routing.text_route.is_none());
    }

    #[test]
    fn a_composer_only_touches_requests_in_its_own_conversation() {
        let (own, other) = (ConversationId::new(), ConversationId::new());

        assert!(request_in_scope(Some(own), Some(own)));
        assert!(
            !request_in_scope(Some(other), Some(own)),
            "cross-conversation request"
        );
        assert!(!request_in_scope(None, Some(own)), "unknown request");
        assert!(request_in_scope(Some(other), None), "main window");
        assert!(
            request_in_scope(None, None),
            "main window, unknown request reaches core"
        );
    }

    #[test]
    fn dismissing_an_unknown_request_from_a_composer_is_outside_its_scope() {
        let directory = tempfile::TempDir::new().unwrap();
        let client = Client::open(
            peppy_client_core::ClientConfig {
                database_path: directory.path().join("reactions.db"),
                vault_id: peppy_client_core::VaultId::new(),
                device_id: DeviceId::new(),
            },
            peppy_client_core::DatabaseKey::new(&[7; 32]).unwrap(),
        )
        .unwrap();
        let request = ReactionRequestId::new().to_string();

        assert_eq!(
            dismiss(&client, &request, Some(ConversationId::new())).unwrap(),
            DismissScope::OutsideScope
        );
        assert!(
            dismiss(&client, &request, None).is_err(),
            "the main window reaches core, which reports the unknown request"
        );
    }

    #[test]
    fn a_single_reported_gateway_is_the_implicit_text_route() {
        let text = conversation(ReactionSupport::Text, None, ConversationFolder::Inbox);
        let routing = conversation_reactions(&text, &[gateway(GATEWAY, "sim-1", true)], None);
        assert_eq!(routing.support, "text");
        assert_eq!(routing.gateway_id.as_deref(), Some(GATEWAY));

        let two = [
            gateway(GATEWAY, "sim-1", true),
            gateway(GATEWAY, "sim-2", true),
        ];
        assert_eq!(conversation_reactions(&text, &two, None).support, "none");
    }

    #[test]
    fn text_routing_needs_an_sms_capable_route_and_spam_offers_nothing() {
        let text = conversation(ReactionSupport::Text, None, ConversationFolder::Inbox);
        let mut no_sms = gateway(GATEWAY, "sim-1", true);
        no_sms.supports_sms = false;
        assert_eq!(
            conversation_reactions(&text, &[no_sms], None).support,
            "none"
        );

        let spam = conversation(ReactionSupport::Text, None, ConversationFolder::Spam);
        assert_eq!(
            conversation_reactions(&spam, &[gateway(GATEWAY, "sim-1", true)], None).support,
            "none"
        );
    }
}
