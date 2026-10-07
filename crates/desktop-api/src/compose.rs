//! Shared local draft composition policy for native and browser hosts.
use crate::{DraftView, GatewayView, SendResultView, draft_view};
use peppy_client_core::{
    AttachmentId, Client, ComposeDraft, ComposeDraftUpdate, ConversationId, DeviceId, Direction,
    DraftId, Error as CoreError, GatewayRoute, Message, Transport,
};
use serde::Deserialize;
use std::str::FromStr;

const MAX_ATTACHMENTS: usize = 10;
const MAX_BODY_BYTES: usize = 64 * 1024;
const MAX_RECIPIENTS: usize = 20;
const FALLBACK_MMS_MAX_BYTES: u64 = 300 * 1024;

/// Selects the exact reported gateway/SIM without treating unknown presence as offline.
pub fn find_route<'a>(
    gateways: &'a [GatewayView],
    gateway_id: &str,
    sim_id: &str,
) -> Option<&'a GatewayView> {
    gateways.iter().find(|gateway| {
        gateway.id == gateway_id && gateway.sim_id == sim_id && !gateway.sim_id.is_empty()
    })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DraftInput {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub conversation_id: String,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub recipient_ids: Vec<String>,
    #[serde(default)]
    pub attachment_ids: Vec<String>,
    #[serde(default)]
    pub gateway_id: Option<String>,
    #[serde(default)]
    pub sim_id: Option<String>,
    #[serde(default)]
    pub expected_revision: String,
}

pub enum ComposeError {
    Ui {
        code: &'static str,
        message: &'static str,
    },
    ReplyBlocked(String),
    Core(CoreError),
    AfterRoute {
        source: CoreError,
        current_revision: u64,
    },
}

fn ui(code: &'static str, message: &'static str) -> ComposeError {
    ComposeError::Ui { code, message }
}
fn revision(value: &str) -> Result<u64, ComposeError> {
    value
        .parse()
        .map_err(|_| ui("invalid-draft", "The draft revision is invalid."))
}

/// Accepts international (`+CC…`) and national numbers and short codes; separators are removed.
pub fn normalize_recipient(value: &str) -> Result<String, ComposeError> {
    let compact: String = value
        .trim()
        .chars()
        .filter(|c| !matches!(c, ' ' | '-' | '(' | ')' | '.'))
        .collect();
    let (plus, digits) = match compact.strip_prefix('+') {
        Some(rest) => (true, rest),
        None => (false, compact.as_str()),
    };
    if (3..=15).contains(&digits.len()) && digits.bytes().all(|b| b.is_ascii_digit()) {
        Ok(if plus {
            format!("+{digits}")
        } else {
            digits.to_owned()
        })
    } else {
        Err(ui(
            "invalid-recipient",
            "Recipients must be phone numbers (international +CC format, national digits, or a short code). Other address types are not supported.",
        ))
    }
}
fn parse_route(gateway: &str, sim: &str) -> Result<GatewayRoute, ComposeError> {
    Ok(GatewayRoute {
        gateway_device_id: DeviceId::from_str(gateway)
            .map_err(|_| ui("invalid-route", "The gateway ID is invalid."))?,
        subscription_id: sim.to_owned(),
    })
}
fn other_party(message: &Message) -> Vec<String> {
    match message.payload.direction {
        Direction::Incoming => message.payload.sender_address.clone().into_iter().collect(),
        Direction::Outgoing => message.payload.recipients.clone(),
    }
}
fn conversation_context(
    client: &Client,
    conversation: ConversationId,
) -> Result<(Vec<String>, Option<Transport>), ComposeError> {
    let messages = client.messages(conversation).map_err(ComposeError::Core)?;
    let Some(latest) = messages.last() else {
        return Ok((Vec::new(), None));
    };
    let transport = latest.payload.transport;
    if transport == Transport::Mms {
        let context = client
            .mms_reply_context(conversation)
            .map_err(ComposeError::Core)?;
        if let Some(reason) = context.blocked_reason {
            return Err(ComposeError::ReplyBlocked(reason));
        }
        return Ok((context.recipients, Some(transport)));
    }
    Ok((other_party(latest), Some(transport)))
}
fn check_attachments(client: &Client, ids: &[String]) -> Result<Vec<AttachmentId>, ComposeError> {
    if ids.len() > MAX_ATTACHMENTS {
        return Err(ui(
            "invalid-attachment",
            "A message can carry at most 10 attachments.",
        ));
    }
    ids.iter()
        .map(|id| {
            let id = AttachmentId::from_str(id)
                .map_err(|_| ui("invalid-attachment", "The attachment ID is invalid."))?;
            let info = client.attachment_info(id).map_err(ComposeError::Core)?;
            if !info.state.is_local() {
                return Err(ui(
                    "invalid-attachment",
                    "Only attachments added on this device can be sent.",
                ));
            }
            Ok(id)
        })
        .collect()
}

pub fn save_draft(client: &Client, input: &DraftInput) -> Result<DraftView, ComposeError> {
    let expected = revision(&input.expected_revision)?;
    let draft_id = match DraftId::from_str(&input.id) {
        Ok(id) => id,
        Err(_) => {
            let conversation = match input.conversation_id.as_str() {
                "" => None,
                id => Some(
                    ConversationId::from_str(id)
                        .map_err(|_| ui("invalid-draft", "The conversation ID is invalid."))?,
                ),
            };
            client
                .create_compose_draft(conversation)
                .map_err(ComposeError::Core)?
                .draft_id
        }
    };
    let current = client
        .compose_draft(draft_id)
        .map_err(ComposeError::Core)?
        .ok_or(ComposeError::Core(CoreError::NotFound))?;
    if !input.conversation_id.is_empty()
        && input.conversation_id != current.conversation_id.to_string()
    {
        return Err(ui(
            "invalid-draft",
            "The draft does not belong to this conversation.",
        ));
    }
    let route = match (
        input.gateway_id.as_deref().unwrap_or(""),
        input.sim_id.as_deref().unwrap_or(""),
    ) {
        ("", "") => current.route.clone(),
        (gateway, sim) if !gateway.is_empty() && !sim.is_empty() => {
            Some(parse_route(gateway, sim)?)
        }
        _ => return Err(ui("invalid-route", "Select a gateway and SIM together.")),
    };
    let recipients = input
        .recipient_ids
        .iter()
        .map(|value| normalize_recipient(value))
        .collect::<Result<_, _>>()?;
    let saved = client
        .save_compose_draft(
            draft_id,
            expected,
            ComposeDraftUpdate {
                text: input.text.clone(),
                recipients,
                attachment_ids: check_attachments(client, &input.attachment_ids)?,
                route,
            },
        )
        .map_err(ComposeError::Core)?;
    Ok(draft_view(&saved))
}

pub fn send_draft(
    client: &Client,
    input: &DraftInput,
    gateways: Option<&[GatewayView]>,
) -> Result<SendResultView, ComposeError> {
    let draft_id = DraftId::from_str(&input.id)
        .map_err(|_| ui("invalid-draft", "Save the draft before sending it."))?;
    let expected = revision(&input.expected_revision)?;
    let (gateway_id, sim_id) = match (input.gateway_id.as_deref(), input.sim_id.as_deref()) {
        (Some(gateway), Some(sim)) if !gateway.is_empty() && !sim.is_empty() => (gateway, sim),
        _ => {
            return Err(ui(
                "invalid-route",
                "Select a gateway and SIM before sending.",
            ));
        }
    };
    let stored: ComposeDraft = client
        .compose_draft(draft_id)
        .map_err(ComposeError::Core)?
        .ok_or(ComposeError::Core(CoreError::NotFound))?;
    if !input.conversation_id.is_empty()
        && input.conversation_id != stored.conversation_id.to_string()
    {
        return Err(ui(
            "invalid-draft",
            "The draft does not belong to this conversation.",
        ));
    }
    if stored.revision != expected {
        return Err(ComposeError::Core(CoreError::StaleDraft {
            current_revision: stored.revision,
        }));
    }
    let gateways = gateways.ok_or_else(|| {
        ui(
            "gateways-unknown",
            "Gateway capabilities have not been loaded from the server yet; the draft was kept.",
        )
    })?;
    let gateway = find_route(gateways, gateway_id, sim_id).ok_or_else(|| {
        ui(
            "gateway-unavailable",
            "The selected gateway/SIM is not currently reported by the server; the draft was kept.",
        )
    })?;
    if !gateway.supports_sms {
        return Err(ui(
            "gateway-unsupported",
            "The selected gateway/SIM cannot send SMS; the draft was kept.",
        ));
    }
    let route = parse_route(gateway_id, sim_id)?;
    let (derived_recipients, latest_transport) =
        conversation_context(client, stored.conversation_id)?;
    let recipients = if stored.recipients.is_empty() {
        derived_recipients
    } else {
        stored.recipients.clone()
    };
    if recipients.is_empty() {
        return Err(ui(
            "invalid-recipient",
            "Add a recipient before sending; the draft was kept.",
        ));
    }
    if stored.text.trim().is_empty() && stored.attachment_ids.is_empty() {
        return Err(ui(
            "empty-message",
            "Write a message or add an attachment before sending.",
        ));
    }
    if stored.text.len() > MAX_BODY_BYTES {
        return Err(ui("message-too-long", "The message is too long to send."));
    }
    if recipients.len() > MAX_RECIPIENTS || recipients.iter().any(|r| r.is_empty() || r.len() > 256)
    {
        return Err(ui(
            "invalid-recipient",
            "The recipients are not valid for sending.",
        ));
    }
    let attachment_ids: Vec<String> = stored
        .attachment_ids
        .iter()
        .map(ToString::to_string)
        .collect();
    let attachment_info = attachment_ids
        .iter()
        .map(|id| {
            AttachmentId::from_str(id)
                .map_err(|_| ui("invalid-attachment", "The attachment ID is invalid."))
                .and_then(|id| client.attachment_info(id).map_err(ComposeError::Core))
        })
        .collect::<Result<Vec<_>, _>>()?;
    check_attachments(client, &attachment_ids)?;
    let requires_mms = !attachment_info.is_empty()
        || recipients.len() > 1
        || latest_transport == Some(Transport::Mms);
    if requires_mms {
        if !gateway.supports_mms || gateway.mms_content_version.unwrap_or(0) < 2 {
            return Err(ui(
                "mms-unsupported",
                "The selected gateway/SIM does not report MMS capability version 2; the draft was kept.",
            ));
        }
        if recipients.len() > gateway.mms_max_recipients.unwrap_or(MAX_RECIPIENTS) {
            return Err(ui(
                "mms-too-many-recipients",
                "This MMS route does not support that many recipients; the draft was kept.",
            ));
        }
        let bytes = attachment_info
            .iter()
            .try_fold(0u64, |total, info| {
                total.checked_add(info.plaintext_bytes).ok_or_else(|| {
                    ui(
                        "mms-too-large",
                        "The MMS is too large to send; the draft was kept.",
                    )
                })
            })?
            .checked_add(stored.text.len() as u64)
            .ok_or_else(|| {
                ui(
                    "mms-too-large",
                    "The MMS is too large to send; the draft was kept.",
                )
            })?;
        if bytes > gateway.mms_max_bytes.unwrap_or(FALLBACK_MMS_MAX_BYTES) {
            return Err(ui(
                "mms-too-large",
                "The MMS content already exceeds this route's size limit; the draft was kept.",
            ));
        }
    }
    let revision = if stored.route.as_ref() != Some(&route) || recipients != stored.recipients {
        client
            .save_compose_draft(
                draft_id,
                expected,
                ComposeDraftUpdate {
                    text: stored.text.clone(),
                    recipients,
                    attachment_ids: stored.attachment_ids.clone(),
                    route: Some(route),
                },
            )
            .map_err(ComposeError::Core)?
            .revision
    } else {
        expected
    };
    let transport = if requires_mms {
        Transport::Mms
    } else {
        Transport::Sms
    };
    if let Err(source) = client.send_compose_draft_checked_transport(draft_id, revision, transport)
    {
        let current_revision = client
            .compose_draft(draft_id)
            .ok()
            .flatten()
            .map(|draft| draft.revision)
            .unwrap_or(revision);
        return Err(ComposeError::AfterRoute {
            source,
            current_revision,
        });
    }
    Ok(SendResultView {
        accepted: true,
        status: "queued-local",
        reason: None,
        revision: Some((revision + 1).to_string()),
    })
}
