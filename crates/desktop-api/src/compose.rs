//! Shared local draft composition policy for native and browser hosts.
use crate::{
    DraftView, GatewayAvailability, GatewayView, SendResultView, draft_view, group_refusal_view,
    mms_plan::{MmsImage, MmsPlan, MmsPlanError, plan_mms},
};
use peppy_client_core::{
    AttachmentId, Client, CommandId, ComposeDraft, ComposeDraftUpdate, ConversationId, DeviceId,
    Direction, DraftId, DraftSendParts, Error as CoreError, GatewayRoute, GroupDraftSend, Message,
    SendRefusalReason, Transport, compose_transport,
};
use serde::{Deserialize, Serialize};
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

#[derive(Deserialize, Serialize)]
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
    /// Group name for a draft to two or more members; carried into a create request only.
    #[serde(default)]
    pub group_name: Option<String>,
    /// The member revision the composer showed (decimal `u64`); group sends only.
    #[serde(default)]
    pub expected_member_revision: Option<String>,
    /// Set only by "Create it again" after an unknown create outcome.
    #[serde(default)]
    pub group_create_confirm_duplicate: Option<bool>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscardDraftInput {
    pub id: String,
    pub conversation_id: String,
    pub expected_revision: String,
}

pub enum ComposeError {
    Ui {
        code: &'static str,
        message: String,
    },
    Core(CoreError),
    AfterRoute {
        source: CoreError,
        current_revision: u64,
    },
}

fn ui(code: &'static str, message: &'static str) -> ComposeError {
    ComposeError::Ui {
        code,
        message: message.to_owned(),
    }
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
    Ok(GatewayRoute::Carrier {
        gateway_device_id: DeviceId::from_str(gateway)
            .map_err(|_| ui("invalid-route", "The phone ID is invalid."))?,
        subscription_id: sim.to_owned(),
    })
}
pub(crate) fn gateway_sendable(gateway: &GatewayView) -> Result<(), ComposeError> {
    let (code, message) = match gateway.availability {
        GatewayAvailability::Offline => (
            "gateway-offline",
            "The selected phone is offline; the draft was kept.",
        ),
        GatewayAvailability::NeverConnected => (
            "gateway-not-connected",
            "The selected phone has not connected yet; the draft was kept.",
        ),
        GatewayAvailability::NoSmsSim => (
            "gateway-no-sms-sim",
            "The selected phone has no SMS SIM; the draft was kept.",
        ),
        GatewayAvailability::SmsPermissionNeeded => (
            "gateway-permission-needed",
            "The selected phone needs SMS permission; the draft was kept.",
        ),
        GatewayAvailability::SmsUnavailable | GatewayAvailability::UnsupportedPlatform => (
            "gateway-unsupported",
            "The selected phone/SIM cannot send SMS; the draft was kept.",
        ),
        GatewayAvailability::CapabilitiesUnknown => (
            "gateway-capabilities-unknown",
            "The selected phone has not reported valid SMS capabilities; the draft was kept.",
        ),
        GatewayAvailability::Unknown
        | GatewayAvailability::Connected
        | GatewayAvailability::Delayed => {
            return Ok(());
        }
    };
    Err(ui(code, message))
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
    let Some(latest) = client
        .latest_message(conversation)
        .map_err(ComposeError::Core)?
    else {
        return Ok((Vec::new(), None));
    };
    let mut transport = latest.payload.transport;
    if transport == Transport::Mms && latest.payload.direction == Direction::Outgoing {
        for index in 0..latest.payload.record.attachments.len() {
            if client
                .rcs_media_outgoing_status(latest.payload.record.message_id, index as u32)
                .map_err(ComposeError::Core)?
                .is_some()
            {
                transport = Transport::Rcs;
                break;
            }
        }
    }
    if transport == Transport::Mms {
        let context = client
            .mms_reply_context(conversation)
            .map_err(ComposeError::Core)?;
        return Ok((context.recipients, Some(transport)));
    }
    Ok((other_party(&latest), Some(transport)))
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
    let is_new = DraftId::from_str(&input.id).is_err();
    let recipients = input
        .recipient_ids
        .iter()
        .map(|value| normalize_recipient(value))
        .collect::<Result<Vec<_>, _>>()?;
    let attachment_ids = check_attachments(client, &input.attachment_ids)?;
    let requested_route = match (
        input.gateway_id.as_deref().unwrap_or(""),
        input.sim_id.as_deref().unwrap_or(""),
    ) {
        ("", "") => None,
        (gateway, sim) if !gateway.is_empty() && !sim.is_empty() => {
            Some(parse_route(gateway, sim)?)
        }
        _ => return Err(ui("invalid-route", "Select a phone and SIM together.")),
    };
    let group_name = draft_group_name(&input.group_name, recipients.len())?;
    let update = ComposeDraftUpdate {
        group_name,
        text: input.text.clone(),
        recipients,
        attachment_ids,
        route: requested_route,
    };
    if is_new && input.conversation_id.is_empty() {
        if expected != 0 {
            return Err(ui(
                "invalid-draft",
                "A new draft must start at revision zero.",
            ));
        }
        return client
            .save_new_compose_draft(update)
            .map(|draft| draft_view(&draft))
            .map_err(ComposeError::Core);
    }
    let draft_id = match DraftId::from_str(&input.id) {
        Ok(id) => id,
        Err(_) => {
            let conversation = ConversationId::from_str(&input.conversation_id)
                .map_err(|_| ui("invalid-draft", "The conversation ID is invalid."))?;
            client
                .create_compose_draft(Some(conversation))
                .map_err(ComposeError::Core)?
                .draft_id
        }
    };
    let current = validate_draft_source(client, draft_id, &input.conversation_id)?;
    let route = update
        .route
        .clone()
        .or_else(|| current.as_ref().and_then(|draft| draft.route.clone()));
    let Some(current) = current else {
        // Let core report a redirect as `draft-moved` instead of masking it as not-found.
        return client
            .save_compose_draft(
                draft_id,
                expected,
                ComposeDraftUpdate {
                    group_name: update.group_name,
                    text: update.text,
                    recipients: update.recipients,
                    attachment_ids: update.attachment_ids,
                    route,
                },
            )
            .map(|draft| draft_view(&draft))
            .map_err(ComposeError::Core);
    };
    if !input.conversation_id.is_empty()
        && input.conversation_id != current.conversation_id.to_string()
    {
        return Err(ui(
            "invalid-draft",
            "The draft does not belong to this conversation.",
        ));
    }
    let saved = client
        .save_compose_draft(
            draft_id,
            expected,
            ComposeDraftUpdate {
                group_name: update.group_name,
                text: update.text,
                recipients: update.recipients,
                attachment_ids: update.attachment_ids,
                route,
            },
        )
        .map_err(ComposeError::Core)?;
    Ok(draft_view(&saved))
}

/// Longest group name a draft keeps, in Unicode code points (the composer's field limit).
const GROUP_NAME_MAX_CHARS: usize = 100;

/// The group name a draft keeps. Only a draft to two or more members has one; an empty or
/// blank name is no name.
fn draft_group_name(
    name: &Option<String>,
    recipient_count: usize,
) -> Result<Option<String>, ComposeError> {
    let Some(name) = name.as_deref().filter(|_| recipient_count > 1) else {
        return Ok(None);
    };
    if name.trim().is_empty() {
        return Ok(None);
    }
    if name.chars().count() > GROUP_NAME_MAX_CHARS {
        return Err(ui("invalid-draft", "The group name is too long."));
    }
    Ok(Some(name.to_owned()))
}

pub fn discard_draft(client: &Client, input: &DiscardDraftInput) -> Result<(), ComposeError> {
    let draft_id = DraftId::from_str(&input.id)
        .map_err(|_| ui("invalid-draft", "The draft ID is invalid."))?;
    let conversation = ConversationId::from_str(&input.conversation_id)
        .map_err(|_| ui("invalid-draft", "The conversation ID is invalid."))?;
    let expected = revision(&input.expected_revision)?;
    if let Some(draft) = client.compose_draft(draft_id).map_err(ComposeError::Core)?
        && draft.conversation_id != conversation
    {
        return Err(ui(
            "invalid-draft",
            "The draft does not belong to this conversation.",
        ));
    }
    client
        .discard_compose_draft(draft_id, expected)
        .map_err(ComposeError::Core)
}

pub fn send_draft(
    client: &Client,
    input: &DraftInput,
    gateways: Option<&[GatewayView]>,
) -> Result<SendResultView, ComposeError> {
    let prepared = prepare_draft_send(client, input, gateways)?;
    queue_prepared_draft_send(client, &prepared, gateways, None)
}

/// Runs the same local composition policy as `send_draft`, then queues the command ID already
/// accepted by the draft-sync service. Hosts must only call this after that acceptance.
pub fn send_draft_accepted(
    client: &Client,
    input: &DraftInput,
    _gateways: Option<&[GatewayView]>,
    command_id: CommandId,
) -> Result<SendResultView, ComposeError> {
    let draft_id = DraftId::from_str(&input.id)
        .map_err(|_| ui("invalid-draft", "Save the draft before sending it."))?;
    let revision = revision(&input.expected_revision)?;
    let parts = client
        .draft_send_intent_parts_count(draft_id, command_id)
        .map_err(ComposeError::Core)?
        .filter(|count| *count > 1);
    client
        .send_compose_draft_accepted_with_parts(draft_id, revision, command_id, None)
        .map_err(|source| ComposeError::AfterRoute {
            source,
            current_revision: revision,
        })?;
    Ok(SendResultView {
        accepted: true,
        status: "queued-local",
        reason: None,
        revision: Some((revision + 1).to_string()),
        parts,
        group: None,
        refused_attachment_ids: None,
    })
}

/// Validates a stored draft and persists canonical route/recipient fields before a host creates
/// a remote send intent. The returned input must be used for both intent creation and acceptance.
pub fn prepare_draft_send(
    client: &Client,
    input: &DraftInput,
    gateways: Option<&[GatewayView]>,
) -> Result<DraftInput, ComposeError> {
    let draft_id = DraftId::from_str(&input.id)
        .map_err(|_| ui("invalid-draft", "Save the draft before sending it."))?;
    let expected = revision(&input.expected_revision)?;
    let (gateway_id, sim_id) = match (input.gateway_id.as_deref(), input.sim_id.as_deref()) {
        (Some(gateway), Some(sim)) if !gateway.is_empty() && !sim.is_empty() => (gateway, sim),
        _ => {
            return Err(ui(
                "invalid-route",
                "Select a phone and SIM before sending.",
            ));
        }
    };
    let gateways = gateways.ok_or_else(|| {
        ui(
            "gateways-unknown",
            "Phone capabilities have not been loaded from the server yet; the draft was kept.",
        )
    })?;
    let gateway = find_route(gateways, gateway_id, sim_id).ok_or_else(|| {
        ui(
            "gateway-unavailable",
            "The selected phone/SIM is not currently reported by the server; the draft was kept.",
        )
    })?;
    if !gateway.supports_sms {
        return Err(ui(
            "gateway-unsupported",
            "The selected phone/SIM cannot send SMS; the draft was kept.",
        ));
    }
    gateway_sendable(gateway)?;
    validate_draft_source(client, draft_id, &input.conversation_id)?;
    let stored: ComposeDraft = client
        .resolve_compose_draft_for_send(draft_id, expected)
        .map_err(ComposeError::Core)?;
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
    let rcs_media_route = !attachment_info.is_empty()
        && (latest_transport == Some(Transport::Rcs)
            || group_route_live(client, stored.conversation_id)?
            || group_creation_media_route(
                client,
                stored.conversation_id,
                gateway_id,
                recipients.len(),
                input.group_create_confirm_duplicate.unwrap_or(false),
            )?);
    let requires_mms = !rcs_media_route
        && compose_transport(&recipients, &stored.text, !attachment_info.is_empty())
            == Transport::Mms;
    if requires_mms {
        if !gateway.supports_mms || gateway.mms_content_version.unwrap_or(0) < 2 {
            return Err(ui(
                "mms-unsupported",
                "The selected phone/SIM does not report MMS capability version 2; the draft was kept.",
            ));
        }
        if recipients.len() > gateway.mms_max_recipients.unwrap_or(MAX_RECIPIENTS) {
            return Err(ui(
                "mms-too-many-recipients",
                "This MMS phone route does not support that many recipients; the draft was kept.",
            ));
        }
        let images = attachment_info
            .iter()
            .map(|info| MmsImage {
                id: info.attachment_id.to_string(),
                bytes: info.plaintext_bytes,
                media_type: info.media_type.clone(),
            })
            .collect::<Vec<_>>();
        if let Err(error) = plan_mms(
            gateway.mms_max_bytes.unwrap_or(FALLBACK_MMS_MAX_BYTES),
            gateway.mms_content_version.unwrap_or(0),
            &stored.text,
            &images,
        ) {
            let message = match error {
                MmsPlanError::TooLarge => {
                    "The MMS content already exceeds this route's size limit; the draft was kept."
                        .to_owned()
                }
                MmsPlanError::NonResizableImageTooLarge { image_id } => attachment_info
                    .iter()
                    .find(|info| info.attachment_id.to_string() == image_id)
                    .map(|info| format!("{} cannot be resized to fit this route's MMS size limit; the draft was kept.", info.display_name))
                    .unwrap_or_else(|| "An attachment cannot be resized to fit this route's MMS size limit; the draft was kept.".to_owned()),
            };
            return Err(ComposeError::Ui {
                code: "mms-too-large",
                message,
            });
        }
    }
    let (draft_id, revision) =
        if stored.route.as_ref() != Some(&route) || recipients != stored.recipients {
            let saved = client
                .save_compose_draft(
                    draft_id,
                    expected,
                    ComposeDraftUpdate {
                        group_name: draft_group_name(&stored.group_name, recipients.len())?,
                        text: stored.text.clone(),
                        recipients: recipients.clone(),
                        attachment_ids: stored.attachment_ids.clone(),
                        route: Some(route),
                    },
                )
                .map_err(ComposeError::Core)?;
            if saved.draft_id != draft_id || saved.conversation_id != stored.conversation_id {
                return Err(ComposeError::Core(CoreError::DraftMoved {
                    draft_id: saved.draft_id,
                    conversation_id: saved.conversation_id,
                    current_revision: saved.revision,
                }));
            }
            (saved.draft_id, saved.revision)
        } else {
            (draft_id, expected)
        };
    // A route/recipient update can move a draft. Re-resolve immediately before intent creation.
    client
        .resolve_compose_draft_for_send(draft_id, revision)
        .map_err(ComposeError::Core)?;
    Ok(DraftInput {
        id: draft_id.to_string(),
        conversation_id: stored.conversation_id.to_string(),
        text: stored.text,
        recipient_ids: recipients,
        attachment_ids,
        gateway_id: Some(gateway_id.to_owned()),
        sim_id: Some(sim_id.to_owned()),
        expected_revision: revision.to_string(),
        group_name: input.group_name.clone(),
        expected_member_revision: input.expected_member_revision.clone(),
        group_create_confirm_duplicate: input.group_create_confirm_duplicate,
    })
}

fn queue_prepared_draft_send(
    client: &Client,
    input: &DraftInput,
    gateways: Option<&[GatewayView]>,
    command_id: Option<CommandId>,
) -> Result<SendResultView, ComposeError> {
    let draft_id = DraftId::from_str(&input.id)
        .map_err(|_| ui("invalid-draft", "Save the draft before sending it."))?;
    let revision = revision(&input.expected_revision)?;
    let stored = client
        .resolve_compose_draft_for_send(draft_id, revision)
        .map_err(ComposeError::Core)?;
    let parts = command_id
        .is_none()
        .then(|| prepared_draft_send_parts(client, input, gateways))
        .transpose()?;
    let transport = compose_transport(
        &stored.recipients,
        &stored.text,
        !stored.attachment_ids.is_empty(),
    );
    let group = match (command_id, parts.as_ref().and_then(Option::as_ref)) {
        (None, None) if stored.recipients.len() > 1 => Some(group_send(input)?),
        _ => None,
    };
    let sent = match (command_id, parts.as_ref().and_then(Option::as_ref), group) {
        (Some(command_id), _, _) => {
            client.send_compose_draft_accepted_with_parts(draft_id, revision, command_id, None)
        }
        (None, Some(parts), _) => client
            .send_compose_draft_with_parts(draft_id, revision, parts.clone())
            .map(|_| ()),
        (None, None, Some(group)) => client
            .send_compose_draft_for_group_checked_transport(draft_id, revision, transport, group)
            .map(|_| ()),
        (None, None, None) => client
            .send_compose_draft_checked_transport(draft_id, revision, transport)
            .map(|_| ()),
    };
    if matches!(sent, Err(CoreError::GroupMembersChanged)) {
        return members_changed_refusal(client, stored.conversation_id, revision);
    }
    if let Err(source) = sent {
        let current_revision = match &source {
            CoreError::DraftMoved {
                current_revision, ..
            } => *current_revision,
            _ => client
                .compose_draft(draft_id)
                .ok()
                .flatten()
                .map(|draft| draft.revision)
                .unwrap_or(revision),
        };
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
        parts: parts
            .flatten()
            .map(|parts| (parts.image_groups.len() + usize::from(parts.caption)) as u32),
        group: None,
        refused_attachment_ids: None,
    })
}

/// Whether a group's RCS route is live or paused, the states where core blocks carrier media.
fn group_route_live(
    client: &Client,
    conversation_id: ConversationId,
) -> Result<bool, ComposeError> {
    Ok(client
        .group_view(conversation_id)
        .map_err(ComposeError::Core)?
        .is_some_and(|view| {
            matches!(
                view.state,
                peppy_client_core::GroupLinkState::Live | peppy_client_core::GroupLinkState::Paused
            )
        }))
}

/// Advisory compose policy only. The gateway still owns the one-use create and send permits.
fn group_creation_media_route(
    client: &Client,
    conversation: ConversationId,
    gateway_id: &str,
    recipient_count: usize,
    confirm_duplicate: bool,
) -> Result<bool, ComposeError> {
    if recipient_count < 2 {
        return Ok(false);
    }
    if client
        .group_view(conversation)
        .map_err(ComposeError::Core)?
        .is_some_and(|view| {
            !view.can_create && !(confirm_duplicate && view.create_needs_confirmation)
        })
    {
        return Ok(false);
    }
    let gateway = DeviceId::from_str(gateway_id)
        .map_err(|_| ui("invalid-route", "The phone ID is invalid."))?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64;
    client
        .rcs_gateway_supported(gateway, now)
        .map_err(ComposeError::Core)
}

/// Group fields of a draft send. The revision is the composer's expectation; core refuses a mismatch.
fn group_send(input: &DraftInput) -> Result<GroupDraftSend, ComposeError> {
    Ok(GroupDraftSend {
        group_name: input.group_name.clone(),
        expected_member_revision: input
            .expected_member_revision
            .as_deref()
            .map(revision)
            .transpose()?,
        group_create_confirm_duplicate: input.group_create_confirm_duplicate.unwrap_or(false),
    })
}

/// A group send refused before anything was queued: the draft is kept at `revision`, and the
/// refusal carries the group's current members so the composer can show them.
fn members_changed_refusal(
    client: &Client,
    conversation_id: ConversationId,
    revision: u64,
) -> Result<SendResultView, ComposeError> {
    Ok(SendResultView {
        accepted: false,
        status: "failed-before-submit",
        reason: Some(SendRefusalReason::MembersChanged.code().to_owned()),
        revision: Some(revision.to_string()),
        parts: None,
        group: group_refusal_view(client, conversation_id).map_err(ComposeError::Core)?,
        refused_attachment_ids: None,
    })
}

/// The group fields to persist with a remote draft-send intent: present for a send to two or
/// more members, so the accepted send carries the same expectations as a local one.
pub fn prepared_draft_send_group(
    input: &DraftInput,
) -> Result<Option<GroupDraftSend>, ComposeError> {
    if input.recipient_ids.len() < 2 {
        return Ok(None);
    }
    group_send(input).map(Some)
}

/// Computes the immutable split plan to persist with a remote draft-send intent.
pub fn prepared_draft_send_parts(
    client: &Client,
    input: &DraftInput,
    gateways: Option<&[GatewayView]>,
) -> Result<Option<DraftSendParts>, ComposeError> {
    if input.attachment_ids.is_empty() {
        return Ok(None);
    }
    if let Ok(conversation) = ConversationId::from_str(&input.conversation_id) {
        let (_, transport) = conversation_context(client, conversation)?;
        if transport == Some(Transport::Rcs) || group_route_live(client, conversation)? {
            return Ok(None);
        }
    }
    let (Some(gateway_id), Some(sim_id), Some(gateways)) = (
        input.gateway_id.as_deref(),
        input.sim_id.as_deref(),
        gateways,
    ) else {
        return Ok(None);
    };
    let Some(gateway) = find_route(gateways, gateway_id, sim_id) else {
        return Ok(None);
    };
    if let Ok(conversation) = ConversationId::from_str(&input.conversation_id)
        && group_creation_media_route(
            client,
            conversation,
            gateway_id,
            input.recipient_ids.len(),
            input.group_create_confirm_duplicate.unwrap_or(false),
        )?
    {
        return Ok(None);
    }
    let images = input
        .attachment_ids
        .iter()
        .map(|id| {
            let id = AttachmentId::from_str(id)
                .map_err(|_| ui("invalid-attachment", "The attachment ID is invalid."))?;
            let info = client.attachment_info(id).map_err(ComposeError::Core)?;
            Ok(MmsImage {
                id: id.to_string(),
                bytes: info.plaintext_bytes,
                media_type: info.media_type,
            })
        })
        .collect::<Result<Vec<_>, ComposeError>>()?;
    let plan = plan_mms(
        gateway.mms_max_bytes.unwrap_or(FALLBACK_MMS_MAX_BYTES),
        gateway.mms_content_version.unwrap_or(0),
        &input.text,
        &images,
    )
    .map_err(|_| {
        ui(
            "mms-too-large",
            "The MMS content exceeds this route's size limit; the draft was kept.",
        )
    })?;
    let MmsPlan::Split {
        caption,
        image_groups,
    } = plan
    else {
        return Ok(None);
    };
    Ok(Some(DraftSendParts {
        caption,
        image_groups: image_groups
            .into_iter()
            .map(|group| {
                group
                    .into_iter()
                    .map(|id| {
                        AttachmentId::from_str(&id)
                            .map_err(|_| ComposeError::Core(CoreError::Database))
                    })
                    .collect()
            })
            .collect::<Result<_, _>>()?,
    }))
}

/// Ensures a draft's live or redirected source belongs to the caller before any operation that
/// could expose a moved target or persist a route.
fn validate_draft_source(
    client: &Client,
    draft_id: DraftId,
    conversation_id: &str,
) -> Result<Option<ComposeDraft>, ComposeError> {
    let current = client.compose_draft(draft_id).map_err(ComposeError::Core)?;
    if conversation_id.is_empty() {
        return Ok(current);
    }
    let source = current
        .as_ref()
        .map(|draft| draft.conversation_id)
        .or(client
            .compose_draft_source_conversation(draft_id)
            .map_err(ComposeError::Core)?);
    if source.is_some_and(|source| source.to_string() != conversation_id) {
        return Err(ui(
            "invalid-draft",
            "The draft does not belong to this conversation.",
        ));
    }
    Ok(current)
}

#[cfg(test)]
mod tests {
    use super::*;
    use peppy_client_core::{
        ClientConfig, DatabaseKey, DraftPushOutcome, DraftTerminalReasonCore, IncomingSms,
        RemoteDraftChange, VaultId,
    };
    use peppy_crypto::{KeyProfile, create_vault_check_header, derive_root_key};

    const PASSPHRASE: &str = "compose test passphrase";
    const GATEWAY_ID: &str = "00000000-0000-4000-8000-000000000001";

    struct Fixture {
        _directory: tempfile::TempDir,
        client: Client,
    }

    fn fixture() -> Fixture {
        fixture_for_device(DeviceId::new())
    }

    fn fixture_for_device(device_id: DeviceId) -> Fixture {
        let directory = tempfile::tempdir().unwrap();
        let vault_id = VaultId::new();
        let client = Client::open(
            ClientConfig {
                database_path: directory.path().join("compose.db"),
                vault_id,
                device_id,
            },
            DatabaseKey::new(&[7; 32]).unwrap(),
        )
        .unwrap();
        let profile = KeyProfile::new(vault_id.0, 1).unwrap();
        let root = derive_root_key(PASSPHRASE, &profile).unwrap();
        let header = create_vault_check_header(&root, profile.clone()).unwrap();
        client.unlock(&profile, &header, PASSPHRASE).unwrap();
        Fixture {
            _directory: directory,
            client,
        }
    }

    fn input(draft: &ComposeDraft) -> DraftInput {
        DraftInput {
            id: draft.draft_id.to_string(),
            conversation_id: draft.conversation_id.to_string(),
            text: draft.text.clone(),
            recipient_ids: draft.recipients.clone(),
            attachment_ids: draft
                .attachment_ids
                .iter()
                .map(ToString::to_string)
                .collect(),
            gateway_id: Some(GATEWAY_ID.into()),
            sim_id: Some("sim".into()),
            expected_revision: draft.revision.to_string(),
            group_name: None,
            expected_member_revision: None,
            group_create_confirm_duplicate: None,
        }
    }

    fn saved_draft(client: &Client, recipients: Vec<&str>) -> ComposeDraft {
        let draft = client.recipientless_compose_draft().unwrap();
        client
            .save_compose_draft(
                draft.draft_id,
                draft.revision,
                ComposeDraftUpdate {
                    group_name: None,
                    text: "ready".into(),
                    recipients: recipients.into_iter().map(str::to_owned).collect(),
                    attachment_ids: vec![],
                    route: None,
                },
            )
            .unwrap()
    }

    #[test]
    fn the_next_media_draft_keeps_rcs_after_an_mms_shaped_rcs_send() {
        let fixture = fixture();
        let source = fixture._directory.path().join("media.bin");
        std::fs::write(&source, b"media").unwrap();
        let attachment = fixture
            .client
            .prepare_attachment(&source, "image/jpeg", "photo.jpg")
            .unwrap();
        let conversation = ConversationId::new();
        let queued = fixture
            .client
            .queue_mms(
                peppy_client_core::OutgoingMms {
                    conversation_id: conversation,
                    recipients: vec!["+14155552671".into()],
                    body: "caption".into(),
                    attachment_ids: vec![attachment.attachment_id],
                    subject: None,
                },
                parse_route(GATEWAY_ID, "sim").ok().unwrap(),
            )
            .unwrap();
        assert_eq!(
            conversation_context(&fixture.client, conversation)
                .ok()
                .unwrap()
                .1,
            Some(Transport::Mms)
        );
        let db = rusqlite::Connection::open(fixture._directory.path().join("compose.db")).unwrap();
        db.execute_batch(
            "PRAGMA key = \"x'0707070707070707070707070707070707070707070707070707070707070707'\";",
        )
        .unwrap();
        db.execute("INSERT INTO rcs_media_send_status(message_id,attachment_index,state) VALUES(?,0,'sent')", [queued.message_id.to_string()]).unwrap();
        assert_eq!(
            conversation_context(&fixture.client, conversation)
                .ok()
                .unwrap()
                .1,
            Some(Transport::Rcs)
        );
    }

    #[test]
    fn a_first_rcs_group_media_send_keeps_one_album_and_its_create_fields() {
        let device: DeviceId = GATEWAY_ID.parse().unwrap();
        let fixture = fixture_for_device(device);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        fixture
            .client
            .publish_rcs_capability(
                &peppy_client_core::RcsLinkStatusEvent {
                    gateway_device_id: device,
                    state: peppy_client_core::RcsCapabilityState::Linked,
                    availability: true,
                    health: true,
                    phone_responsive: true,
                    observed_at_ms: now,
                    capability_revision: 1,
                    feature_bits: 1,
                    linked_since_ms: Some(now),
                },
                &[],
                now,
            )
            .unwrap();
        let mut ids = Vec::new();
        for (index, mime) in ["application/pdf", "video/mp4"].into_iter().enumerate() {
            let source = fixture._directory.path().join(format!("group-{index}"));
            std::fs::write(&source, vec![7_u8; 1024 * 1024]).unwrap();
            let attachment = fixture
                .client
                .prepare_attachment(&source, mime, "media")
                .unwrap();
            fixture
                .client
                .mark_attachment_uploaded(
                    attachment.attachment_id,
                    &uuid::Uuid::new_v4().to_string(),
                )
                .unwrap();
            ids.push(attachment.attachment_id);
        }
        let draft = fixture
            .client
            .save_new_compose_draft(ComposeDraftUpdate {
                group_name: Some("Media group".into()),
                text: "one caption".into(),
                recipients: vec!["+14155552671".into(), "+14155552672".into()],
                attachment_ids: ids,
                route: Some(parse_route(GATEWAY_ID, "sim").ok().unwrap()),
            })
            .unwrap();
        let mut request = input(&draft);
        request.group_name = Some("Media group".into());
        let result = send_draft(
            &fixture.client,
            &request,
            Some(&[gateway(GatewayAvailability::Connected)]),
        );
        assert!(
            result.is_ok(),
            "a capable RCS create route must not invoke the MMS split planner"
        );
        let messages = fixture.client.messages(draft.conversation_id).unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].payload.record.attachments.len(), 2);
        assert_eq!(messages[0].payload.body, "one caption");
        assert_eq!(
            fixture
                .client
                .group_view(draft.conversation_id)
                .unwrap()
                .unwrap()
                .members
                .len(),
            2
        );
    }

    fn mark_synced(client: &Client, draft_id: DraftId) {
        let push = client
            .pending_draft_pushes()
            .unwrap()
            .into_iter()
            .find(|push| push.draft_id == draft_id)
            .unwrap();
        client
            .record_draft_push(
                draft_id,
                push.revision_id,
                DraftPushOutcome::Accepted {
                    branch_id: uuid::Uuid::new_v4(),
                    expires_at_ms: 1_000,
                    conflict: false,
                },
            )
            .unwrap();
    }

    fn prepared(result: Result<DraftInput, ComposeError>) -> DraftInput {
        match result {
            Ok(input) => input,
            Err(ComposeError::Ui { code, .. }) => panic!("draft preflight should succeed: {code}"),
            Err(_) => panic!("draft preflight should succeed with a core error"),
        }
    }

    fn gateway(availability: GatewayAvailability) -> GatewayView {
        GatewayView {
            id: GATEWAY_ID.into(),
            name: "Gateway".into(),
            sim_id: "sim".into(),
            online: false,
            simulated: false,
            supports_sms: true,
            supports_mms: true,
            availability,
            last_seen_age_seconds: None,
            presence: None,
            wake_route: false,
            capability_note: None,
            mms_content_version: Some(2),
            mms_max_bytes: None,
            mms_limit_source: None,
            mms_max_recipients: None,
        }
    }

    #[test]
    fn send_guard_blocks_unavailable_states_and_keeps_unknown_sendable() {
        for availability in [
            GatewayAvailability::Offline,
            GatewayAvailability::NeverConnected,
            GatewayAvailability::NoSmsSim,
            GatewayAvailability::SmsPermissionNeeded,
            GatewayAvailability::SmsUnavailable,
            GatewayAvailability::CapabilitiesUnknown,
            GatewayAvailability::UnsupportedPlatform,
        ] {
            assert!(
                gateway_sendable(&gateway(availability)).is_err(),
                "{availability:?}"
            );
        }
        for availability in [
            GatewayAvailability::Unknown,
            GatewayAvailability::Connected,
            GatewayAvailability::Delayed,
        ] {
            assert!(
                gateway_sendable(&gateway(availability)).is_ok(),
                "{availability:?}"
            );
        }
    }

    #[test]
    fn prepare_rejects_invalid_source_route_and_mms_limits_before_send_intent() {
        let fixture = fixture();
        let draft = saved_draft(&fixture.client, vec!["+14155552671", "+14155552672"]);
        let draft_input = input(&draft);
        let gateways = [gateway(GatewayAvailability::Connected)];

        let mut foreign = input(&draft);
        foreign.conversation_id = ConversationId::new().to_string();
        assert!(prepare_draft_send(&fixture.client, &foreign, Some(&gateways)).is_err());

        let mut unavailable = input(&draft);
        unavailable.gateway_id = Some("other".into());
        assert!(prepare_draft_send(&fixture.client, &unavailable, Some(&gateways)).is_err());

        let mut no_mms = gateway(GatewayAvailability::Connected);
        no_mms.supports_mms = false;
        assert!(prepare_draft_send(&fixture.client, &draft_input, Some(&[no_mms])).is_err());

        let mut too_small = gateway(GatewayAvailability::Connected);
        too_small.mms_max_bytes = Some(1);
        assert!(prepare_draft_send(&fixture.client, &draft_input, Some(&[too_small])).is_err());
        assert_eq!(
            fixture
                .client
                .compose_draft(draft.draft_id)
                .unwrap()
                .unwrap()
                .revision,
            draft.revision
        );
    }

    #[test]
    fn prepare_canonicalizes_before_the_sync_send_gate() {
        let fixture = fixture();
        let captured = fixture
            .client
            .capture_incoming(IncomingSms {
                conversation_id: None,
                sender_address: "+14155552671".into(),
                body: "history".into(),
                provider_message_id: Some("compose-context".into()),
                imported: false,
            })
            .unwrap();
        let saved = match save_draft(
            &fixture.client,
            &DraftInput {
                id: String::new(),
                conversation_id: captured.conversation_id.to_string(),
                text: "reply".into(),
                recipient_ids: vec![],
                attachment_ids: vec![],
                gateway_id: None,
                sim_id: None,
                expected_revision: "0".into(),
                group_name: None,
                expected_member_revision: None,
                group_create_confirm_duplicate: None,
            },
        ) {
            Ok(saved) => saved,
            Err(_) => panic!("draft save should succeed"),
        };
        let draft = fixture
            .client
            .compose_draft(saved.id.parse().unwrap())
            .unwrap()
            .unwrap();
        mark_synced(&fixture.client, draft.draft_id);

        let prepared = prepared(prepare_draft_send(
            &fixture.client,
            &input(&draft),
            Some(&[gateway(GatewayAvailability::Connected)]),
        ));
        let stored = fixture
            .client
            .compose_draft(draft.draft_id)
            .unwrap()
            .unwrap();
        assert_eq!(prepared.expected_revision, stored.revision.to_string());
        assert_eq!(prepared.recipient_ids, ["+14155552671"]);
        assert!(matches!(
            stored.route.as_ref(),
            Some(GatewayRoute::Carrier { subscription_id, .. }) if subscription_id == "sim"
        ));
        assert_eq!(
            fixture
                .client
                .begin_draft_send(draft.draft_id, stored.revision, None),
            Err(CoreError::DraftNotSynced)
        );
    }

    #[test]
    fn accepted_send_uses_the_durable_intent_instead_of_mutable_draft_input() {
        let fixture = fixture();
        let draft = saved_draft(&fixture.client, vec!["+14155552671"]);
        let prepared = prepared(prepare_draft_send(
            &fixture.client,
            &input(&draft),
            Some(&[gateway(GatewayAvailability::Connected)]),
        ));
        let prepared_id = DraftId::from_str(&prepared.id).unwrap();
        let prepared_revision = prepared.expected_revision.parse().unwrap();
        mark_synced(&fixture.client, prepared_id);
        let command = fixture
            .client
            .begin_draft_send(
                prepared_id,
                prepared.expected_revision.parse().unwrap(),
                match prepared_draft_send_parts(
                    &fixture.client,
                    &prepared,
                    Some(&[gateway(GatewayAvailability::Connected)]),
                ) {
                    Ok(parts) => parts,
                    Err(_) => panic!("prepared send should have a valid plan"),
                },
            )
            .unwrap()
            .remote
            .unwrap()
            .command_id;

        let mut changed = prepared;
        changed.text = "changed after approval".into();
        let accepted = match send_draft_accepted(
            &fixture.client,
            &changed,
            Some(&[gateway(GatewayAvailability::Connected)]),
            command,
        ) {
            Ok(accepted) => accepted,
            Err(_) => panic!("accepted send should queue the sealed command"),
        };
        assert!(accepted.accepted);
        assert_eq!(
            fixture
                .client
                .begin_draft_send(prepared_id, prepared_revision, None)
                .unwrap()
                .remote
                .unwrap()
                .command_id,
            command
        );
    }

    #[test]
    fn accepted_group_send_carries_the_group_fields_from_its_intent() {
        let fixture = fixture();
        let draft = saved_draft(&fixture.client, vec!["+14155552671", "+14155552672"]);
        let mut named = input(&draft);
        named.group_name = Some("Weekend".into());
        let saved = match save_draft(&fixture.client, &named) {
            Ok(view) => view,
            Err(_) => panic!("a named group draft should save"),
        };
        assert_eq!(saved.group_name.as_deref(), Some("Weekend"));

        let current = fixture
            .client
            .compose_draft(DraftId::from_str(&saved.id).unwrap())
            .unwrap()
            .unwrap();
        let prepared = prepared(prepare_draft_send(
            &fixture.client,
            &input(&current),
            Some(&[gateway(GatewayAvailability::Connected)]),
        ));
        let draft_id = DraftId::from_str(&prepared.id).unwrap();
        mark_synced(&fixture.client, draft_id);
        let group = GroupDraftSend {
            group_name: Some("Weekend".into()),
            expected_member_revision: None,
            group_create_confirm_duplicate: false,
        };
        let command = fixture
            .client
            .begin_draft_send_for_group(
                draft_id,
                prepared.expected_revision.parse().unwrap(),
                None,
                Some(group),
            )
            .unwrap()
            .remote
            .unwrap()
            .command_id;

        send_draft_accepted(
            &fixture.client,
            &prepared,
            Some(&[gateway(GatewayAvailability::Connected)]),
            command,
        )
        .unwrap_or_else(|_| panic!("the accepted group send should queue"));
        // A group send writes the roster with the intent's name; a 1:1 send would write none.
        let view = fixture
            .client
            .group_view(ConversationId::from_str(&prepared.conversation_id).unwrap())
            .unwrap()
            .expect("the accepted group send writes its roster");
        assert_eq!(view.name.as_deref(), Some("Weekend"));
        assert_eq!(view.members.len(), 2);
    }

    #[test]
    fn group_name_is_kept_only_for_a_group_and_capped() {
        let kept = |name: &str, recipients| {
            draft_group_name(&Some(name.into()), recipients)
                .ok()
                .flatten()
        };
        assert_eq!(kept("Weekend", 2).as_deref(), Some("Weekend"));
        assert_eq!(kept("Weekend", 1), None);
        assert_eq!(kept("  ", 3), None);
        let long = "x".repeat(GROUP_NAME_MAX_CHARS + 1);
        assert!(draft_group_name(&Some(long), 2).is_err());
    }

    #[test]
    fn accepted_send_is_idempotent_after_the_command_is_already_queued() {
        let fixture = fixture();
        let draft = saved_draft(&fixture.client, vec!["+14155552671"]);
        let prepared = prepared(prepare_draft_send(
            &fixture.client,
            &input(&draft),
            Some(&[gateway(GatewayAvailability::Connected)]),
        ));
        let draft_id = DraftId::from_str(&prepared.id).unwrap();
        mark_synced(&fixture.client, draft_id);
        let command = fixture
            .client
            .begin_draft_send(draft_id, prepared.expected_revision.parse().unwrap(), None)
            .unwrap()
            .remote
            .unwrap()
            .command_id;

        assert!(
            send_draft_accepted(
                &fixture.client,
                &prepared,
                Some(&[gateway(GatewayAvailability::Connected)]),
                command,
            )
            .is_ok()
        );

        assert!(
            send_draft_accepted(
                &fixture.client,
                &prepared,
                Some(&[gateway(GatewayAvailability::Connected)]),
                command,
            )
            .is_ok()
        );
    }

    #[test]
    fn accepted_split_send_reports_persisted_parts_after_terminal_recovery() {
        let fixture = fixture();
        let attachment_ids: Vec<AttachmentId> = (0..6)
            .map(|index| {
                let source = fixture._directory.path().join(format!("image-{index}.jpg"));
                std::fs::write(&source, vec![0; 100_000]).unwrap();
                fixture
                    .client
                    .prepare_attachment(&source, "image/jpeg", "image.jpg")
                    .unwrap()
                    .attachment_id
            })
            .collect();
        let draft = saved_draft(&fixture.client, vec!["+14155552671"]);
        let draft = fixture
            .client
            .save_compose_draft(
                draft.draft_id,
                draft.revision,
                ComposeDraftUpdate {
                    group_name: None,
                    text: "caption".into(),
                    recipients: draft.recipients,
                    attachment_ids: attachment_ids.clone(),
                    route: None,
                },
            )
            .unwrap();
        for attachment_id in attachment_ids {
            fixture
                .client
                .mark_attachment_uploaded(attachment_id, &uuid::Uuid::new_v4().to_string())
                .unwrap();
        }
        let mut planned_gateway = gateway(GatewayAvailability::Connected);
        planned_gateway.mms_content_version = Some(4);
        let prepared = prepared(prepare_draft_send(
            &fixture.client,
            &input(&draft),
            Some(&[planned_gateway.clone()]),
        ));
        let draft_id = DraftId::from_str(&prepared.id).unwrap();
        mark_synced(&fixture.client, draft_id);
        let command = fixture
            .client
            .begin_draft_send(
                draft_id,
                prepared.expected_revision.parse().unwrap(),
                match prepared_draft_send_parts(
                    &fixture.client,
                    &prepared,
                    Some(&[planned_gateway]),
                ) {
                    Ok(parts) => parts,
                    Err(_) => panic!("prepared split send should have a valid plan"),
                },
            )
            .unwrap()
            .remote
            .unwrap()
            .command_id;

        fixture
            .client
            .apply_draft_changes(
                vec![RemoteDraftChange::Terminal {
                    draft_id,
                    reason: DraftTerminalReasonCore::Sent,
                    command_id: Some(command),
                }],
                "1",
            )
            .unwrap();

        let first = match send_draft_accepted(&fixture.client, &prepared, None, command) {
            Ok(result) => result,
            Err(_) => panic!("terminal-first accepted send should report its durable part count"),
        };
        let repeated = match send_draft_accepted(&fixture.client, &prepared, None, command) {
            Ok(result) => result,
            Err(_) => panic!("repeated accepted send should report its durable part count"),
        };
        assert_eq!(first.parts, Some(3));
        assert_eq!(repeated.parts, Some(3));
    }

    #[test]
    fn sms_send_does_not_run_the_mms_split_planner() {
        let fixture = fixture();
        let mut draft = saved_draft(&fixture.client, vec!["+14155552671"]);
        draft = fixture
            .client
            .save_compose_draft(
                draft.draft_id,
                draft.revision,
                ComposeDraftUpdate {
                    group_name: None,
                    text: "body".into(),
                    recipients: draft.recipients.clone(),
                    attachment_ids: vec![],
                    route: draft.route.clone(),
                },
            )
            .unwrap();
        let mut constrained_gateway = gateway(GatewayAvailability::Connected);
        constrained_gateway.mms_content_version = Some(3);
        constrained_gateway.mms_max_bytes = Some(2);

        assert!(
            send_draft(
                &fixture.client,
                &input(&draft),
                Some(&[constrained_gateway])
            )
            .is_ok()
        );
    }

    fn group_input(expected_member_revision: Option<&str>) -> DraftInput {
        DraftInput {
            id: DraftId::new().to_string(),
            conversation_id: String::new(),
            text: "hi".into(),
            recipient_ids: vec!["+15551230001".into(), "+15551230002".into()],
            attachment_ids: vec![],
            gateway_id: None,
            sim_id: None,
            expected_revision: "1".into(),
            group_name: Some("Weekend".into()),
            expected_member_revision: expected_member_revision.map(str::to_owned),
            group_create_confirm_duplicate: Some(true),
        }
    }

    #[test]
    fn group_send_fields_carry_the_composer_expectation() {
        let fields = group_send(&group_input(Some("42")))
            .ok()
            .expect("valid fields");

        assert_eq!(
            fields,
            GroupDraftSend {
                group_name: Some("Weekend".into()),
                expected_member_revision: Some(42),
                group_create_confirm_duplicate: true,
            }
        );
        assert_eq!(
            group_send(&group_input(None))
                .ok()
                .expect("valid fields")
                .expected_member_revision,
            None
        );
    }

    #[test]
    fn group_send_rejects_a_malformed_member_revision() {
        assert!(group_send(&group_input(Some("4x"))).is_err());
        assert!(group_send(&group_input(Some("-1"))).is_err());
    }

    #[test]
    fn members_changed_refusal_keeps_the_draft_revision_and_reason() {
        let fixture = fixture();
        let conversation_id = ConversationId::new();

        let refused = members_changed_refusal(&fixture.client, conversation_id, 7)
            .ok()
            .expect("refusal view");

        assert!(!refused.accepted);
        assert_eq!(refused.status, "failed-before-submit");
        assert_eq!(refused.reason.as_deref(), Some("members_changed"));
        assert_eq!(refused.revision.as_deref(), Some("7"));
        assert!(refused.group.is_none());
    }

    #[test]
    fn group_send_writes_the_roster_and_queues_one_command() {
        let fixture = fixture();
        let draft = saved_draft(&fixture.client, vec!["+14155552671", "+14155552672"]);
        let gateways = [gateway(GatewayAvailability::Connected)];
        let mut group = input(&draft);
        group.group_name = Some("Weekend".into());

        let sent = send_draft(&fixture.client, &group, Some(&gateways))
            .ok()
            .expect("group send is accepted");

        assert!(sent.accepted);
        assert_eq!(sent.reason, None);
        assert!(sent.group.is_none());
        let view = fixture
            .client
            .group_view(draft.conversation_id)
            .unwrap()
            .expect("the first group send writes the roster");
        assert_eq!(view.name.as_deref(), Some("Weekend"));
        assert_eq!(view.members.len(), 2);
        assert!(view.can_create);
    }
}
