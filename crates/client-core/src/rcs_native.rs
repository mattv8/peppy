//! Restricted local-native RCS handoff contract.
//!
//! These records are intentionally separate from vault events and shared conversation DTOs. They
//! are the only JSON-schema-capable projection an Android transport may receive after core has
//! admitted an RCS attempt.
use crate::{
    CommandId, ConversationId, Error, GroupSendAuthority, RcsCapabilityState, RcsConversationType,
    RcsMapping, RcsMappingObservation, RcsSendMode, rcs_group, rcs_mapping,
};
use rusqlite::{Connection, params};
use schemars::{JsonSchema, schema_for};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

pub const NATIVE_RCS_COMMAND_PAGE_SIZE: usize = 100;
pub const NATIVE_CARRIER_INTENT_CANDIDATES: usize = 8;
const CARRIER_CURSOR_META: &str = "native_rcs_carrier_candidate_cursor_v1";

/// A received RCS-link command that may be observed by the trusted native coordinator.
/// This is only a candidate; prepare and begin remain the transport-admission boundary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingNativeRcsCommand {
    pub command_id: CommandId,
    pub conversation_id: ConversationId,
    pub recipient: String,
}

pub(crate) fn pending_commands(
    conn: &Connection,
    active_epoch: Option<u32>,
) -> Result<Vec<PendingNativeRcsCommand>, Error> {
    pending_commands_for(conn, active_epoch, CandidateKind::Text)
}

/// The upload coordinator must not infer media work from the text candidate list. Only commands
/// with a materialized, nonterminal media ledger row are returned.
pub(crate) fn pending_media_commands(
    conn: &Connection,
    active_epoch: Option<u32>,
) -> Result<Vec<PendingNativeRcsCommand>, Error> {
    pending_commands_for(conn, active_epoch, CandidateKind::Media)
}

#[derive(Clone, Copy)]
enum CandidateKind {
    Text,
    Media,
}

fn pending_commands_for(
    conn: &Connection,
    active_epoch: Option<u32>,
    kind: CandidateKind,
) -> Result<Vec<PendingNativeRcsCommand>, Error> {
    if rcs_mapping::recorded_link_lifecycle(conn)? != Some(RcsCapabilityState::Linked) {
        return Ok(Vec::new());
    }
    let Some(active_epoch) = active_epoch else {
        return Ok(Vec::new());
    };
    let mut query = conn.prepare(
        "SELECT l.local_order,l.command_id FROM command_ledger l LEFT JOIN attempts a ON a.command_id=l.command_id WHERE a.command_id IS NULL AND l.historical=0 AND l.key_epoch=? AND NOT EXISTS (SELECT 1 FROM send_cancellations c WHERE c.command_id=l.command_id AND c.state IN ('requested','confirmed')) AND (l.local_order>? OR (l.local_order=? AND l.command_id>?)) ORDER BY l.local_order,l.command_id LIMIT ?",
    )?;
    // The client store mutex prevents ledger growth during this scan. Each batch advances the
    // keyset cursor, so one call examines at most the qualifying raw rows present on entry.
    let mut cursor = (i64::MIN, String::new());
    let mut rcs_candidates = Vec::new();
    let mut carrier_candidates = Vec::new();
    let mut rcs_conversations = HashSet::new();
    loop {
        let rows = query
            .query_map(
                params![
                    active_epoch,
                    cursor.0,
                    cursor.0,
                    cursor.1,
                    NATIVE_RCS_COMMAND_PAGE_SIZE as i64
                ],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
            )?
            .collect::<Result<Vec<_>, _>>()?;
        let Some(last) = rows.last() else {
            break;
        };
        cursor = last.clone();
        let exhausted = rows.len() < NATIVE_RCS_COMMAND_PAGE_SIZE;
        for (_, command_id) in rows {
            if let Some((candidate, carrier_intent)) =
                candidate(conn, active_epoch, command_id, kind)?
            {
                if carrier_intent {
                    if !carrier_candidates
                        .iter()
                        .any(|existing: &PendingNativeRcsCommand| {
                            existing.conversation_id == candidate.conversation_id
                        })
                    {
                        carrier_candidates.push(candidate);
                    }
                } else if rcs_candidates.len() < NATIVE_RCS_COMMAND_PAGE_SIZE {
                    rcs_conversations.insert(candidate.conversation_id);
                    rcs_candidates.push(candidate);
                }
            }
        }
        if exhausted
            || (matches!(kind, CandidateKind::Media)
                && rcs_candidates.len() == NATIVE_RCS_COMMAND_PAGE_SIZE)
        {
            break;
        }
    }
    let mut seen_conversations = rcs_conversations;
    carrier_candidates.retain(|candidate| seen_conversations.insert(candidate.conversation_id));
    let carrier_limit = NATIVE_CARRIER_INTENT_CANDIDATES
        .min(NATIVE_RCS_COMMAND_PAGE_SIZE.saturating_sub(rcs_candidates.len()));
    if carrier_limit > 0 && !carrier_candidates.is_empty() {
        let cursor = crate::get_meta(conn, CARRIER_CURSOR_META)?;
        let start = cursor
            .as_deref()
            .and_then(|command_id| {
                carrier_candidates
                    .iter()
                    .position(|candidate| candidate.command_id.to_string() == command_id)
            })
            .map_or(0, |index| (index + 1) % carrier_candidates.len());
        let selected = carrier_candidates
            .iter()
            .cycle()
            .skip(start)
            .take(carrier_limit.min(carrier_candidates.len()))
            .cloned()
            .collect::<Vec<_>>();
        crate::set_meta(
            conn,
            CARRIER_CURSOR_META,
            &selected.last().unwrap().command_id.to_string(),
        )?;
        rcs_candidates.extend(selected);
    }
    Ok(rcs_candidates)
}

fn candidate(
    conn: &Connection,
    active_epoch: u32,
    command_id: String,
    kind: CandidateKind,
) -> Result<Option<(PendingNativeRcsCommand, bool)>, Error> {
    let command_id: CommandId = command_id
        .parse()
        .map_err(|_| Error::InvalidRequest("invalid command id"))?;
    let cancelled: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM send_cancellations WHERE command_id=? AND state IN ('requested','confirmed'))",
        [command_id.to_string()],
        |row| row.get(0),
    )?;
    if cancelled {
        return Ok(None);
    }
    let result = match kind {
        CandidateKind::Text => {
            crate::unattempted_native_message(conn, Some(active_epoch), command_id)
                .map(|pending| (pending.message, Some(pending.subscription_id)))
        }
        CandidateKind::Media => {
            if crate::rcs_media::next_attachment(conn, command_id)?.is_none() {
                return Ok(None);
            }
            crate::unattempted_rcs_media_message(conn, Some(active_epoch), command_id)
                .map(|message| (message, None))
        }
    };
    let (message, subscription_id) = match result {
        Ok(pending) => pending,
        Err(Error::AlreadyAttempted(_) | Error::InvalidRequest(_)) => return Ok(None),
        Err(error) => return Err(error),
    };
    if let Some(subscription_id) = subscription_id.filter(|id| !id.is_empty())
        && !rcs_group::is_group_conversation(conn, message.record.conversation_id)?
    {
        let [recipient] = message.recipients.as_slice() else {
            return Ok(None);
        };
        let carrier_admission = crate::rcs_send::carrier_admission(
            conn,
            message.record.conversation_id,
            &message.recipients,
            message.transport,
            !message.record.attachments.is_empty(),
            &subscription_id,
            crate::notifications::now_ms(),
            None,
        )?;
        if matches!(
            carrier_admission,
            crate::rcs_send::CarrierAdmission::Permitted
                | crate::rcs_send::CarrierAdmission::ShortCodeSms
                | crate::rcs_send::CarrierAdmission::GroupMmsPreM4
        ) {
            return Ok(None);
        }
        return Ok(Some((
            PendingNativeRcsCommand {
                command_id,
                conversation_id: message.record.conversation_id,
                recipient: recipient.clone(),
            },
            true,
        )));
    }
    let Some(mapping) = rcs_mapping::get(conn, message.record.conversation_id)? else {
        return group_candidate(conn, command_id, &message)
            .map(|candidate| candidate.map(|candidate| (candidate, false)));
    };
    if crate::require_exact_rcs_recipient(&message, &mapping).is_err() {
        return Ok(None);
    }
    let [recipient] = message.recipients.as_slice() else {
        return Ok(None);
    };
    Ok(Some((
        PendingNativeRcsCommand {
            command_id,
            conversation_id: message.record.conversation_id,
            recipient: recipient.clone(),
        },
        false,
    )))
}

/// A queued text to a proved group is a native candidate; its recipient is the member digest.
fn group_candidate(
    conn: &Connection,
    command_id: CommandId,
    message: &crate::MessagePayload,
) -> Result<Option<PendingNativeRcsCommand>, Error> {
    let conversation_id = message.record.conversation_id;
    Ok(
        rcs_group::native_candidate_digest(conn, conversation_id, command_id, &message.recipients)?
            .map(|digest| PendingNativeRcsCommand {
                command_id,
                conversation_id,
                recipient: digest,
            }),
    )
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NativeRcsLifetime {
    pub runtime_id: String,
    pub run_generation: u64,
    pub link_generation: u64,
    pub session_generation: u64,
    pub context_revision: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NativeRcsParticipant {
    pub identity: String,
    pub google_participant_id: String,
    pub identifier_type: i32,
    pub is_visible: bool,
    pub is_me: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum NativeOwnNumberSource {
    SubscriptionManagerPhoneNumber,
    TelephonyLine1Number,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NativeCarrierRouteBinding {
    pub carrier_route_id: String,
    pub android_subscription_id: i32,
    pub is_current_default_sms: bool,
    pub own_number_raw: String,
    pub own_number_source: NativeOwnNumberSource,
    pub route_observed_at_ms: i64,
    pub route_context_revision: u64,
    pub same_handset_evidence_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NativeRcsSimPayload {
    pub two: i32,
    pub sim_number: i32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NativeRcsOutgoing {
    pub outgoing_id: String,
    pub sim_participant_id: String,
    pub own_identity_raw: String,
    pub sim_payload: NativeRcsSimPayload,
    pub rcs_chats_enabled: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum NativeSettingsProvenance {
    CurrentEvent,
    OldEvent,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum NativeSettingsCurrentness {
    ContinuousVerifiedFeed,
    Unproven,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NativeRcsEvidence {
    pub lifetime: NativeRcsLifetime,
    pub lookup_id: String,
    pub lookup_complete: bool,
    pub google_candidate_count: u32,
    pub settings_revision: u64,
    pub settings_provenance: NativeSettingsProvenance,
    pub settings_observed_at_ms: i64,
    pub settings_stream_epoch: u64,
    pub settings_currentness: NativeSettingsCurrentness,
    /// Every current Google settings SIM record; this is never a shared DTO.
    pub current_settings_sims: Vec<NativeRcsOutgoing>,
    pub settings_validated_at_ms: i64,
    pub conversation_observed_at_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum NativeRcsTargetClass {
    OneToOne,
    SelfChat,
    Group,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LocalRcsTargetSnapshot {
    pub proof_version: u32,
    pub peppy_conversation_id: String,
    pub google_conversation_id: String,
    pub conversation_type: RcsConversationType,
    pub status: String,
    pub send_mode: RcsSendMode,
    pub read_only: bool,
    pub default_outgoing: bool,
    pub participants: Vec<NativeRcsParticipant>,
    pub class: NativeRcsTargetClass,
    pub canonical_recipient: String,
    pub canonical_own_identity: String,
    pub identity_region: String,
    pub canonicalizer_revision: u32,
    pub selected_sim_id: String,
    pub carrier_binding: NativeCarrierRouteBinding,
    pub outgoing: NativeRcsOutgoing,
    pub evidence: NativeRcsEvidence,
}

/// Private commit input emitted only by core from a durable `RcsPermit`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NativeRcsCommitProjection {
    pub command_id: String,
    pub tmp_id: String,
    pub mapping_revision: u64,
    pub proof_version: u32,
    pub proof_digest: Vec<u8>,
    pub message_body: String,
    /// Gateway-local Google media reference. This is only emitted to the trusted native host,
    /// never in a synced payload or renderer DTO.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media: Option<NativeRcsMedia>,
    pub target: LocalRcsTargetSnapshot,
    /// Group targets only: the stored member digest Go rechecks immediately before sending.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_member_digest: Option<String>,
}

/// The exact media part Go derives into its `SendMessageRequest`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NativeRcsMedia {
    pub media_id: String,
    pub key: Vec<u8>,
    pub mime: String,
    pub size: u64,
    pub name: String,
}

/// Core-owned input for asking the native host to observe one exact local RCS target.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NativeRcsPrepareRequest {
    pub command_id: String,
    pub peppy_conversation_id: String,
    pub canonical_recipient: String,
}

/// The stored authority a native preparation was captured from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NativeRcsPreparationTarget {
    OneToOne(RcsMapping),
    Group(GroupSendAuthority),
}

/// Core-owned native RCS preparation that retains the stored authority it was captured from.
pub struct NativeRcsPreparation {
    command_id: CommandId,
    request: NativeRcsPrepareRequest,
    target: NativeRcsPreparationTarget,
    media: Option<NativeRcsMedia>,
    attachment_index: Option<u32>,
}

impl NativeRcsPreparation {
    pub(crate) fn new(command_id: CommandId, mapping: RcsMapping) -> Self {
        let request = NativeRcsPrepareRequest {
            command_id: command_id.to_string(),
            peppy_conversation_id: mapping.conversation_id.to_string(),
            canonical_recipient: mapping.proof.canonical_participant.clone(),
        };
        Self {
            command_id,
            request,
            target: NativeRcsPreparationTarget::OneToOne(mapping),
            media: None,
            attachment_index: None,
        }
    }

    /// Group targets carry the member digest as their canonical recipient, as the Go facts require.
    pub(crate) fn new_group(command_id: CommandId, authority: GroupSendAuthority) -> Self {
        let request = NativeRcsPrepareRequest {
            command_id: command_id.to_string(),
            peppy_conversation_id: authority.conversation_id.to_string(),
            canonical_recipient: authority.member_set_digest.clone(),
        };
        Self {
            command_id,
            request,
            target: NativeRcsPreparationTarget::Group(authority),
            media: None,
            attachment_index: None,
        }
    }

    pub(crate) fn with_media(mut self, attachment_index: u32, media: NativeRcsMedia) -> Self {
        self.media = Some(media);
        self.attachment_index = Some(attachment_index);
        self
    }

    pub fn request(&self) -> &NativeRcsPrepareRequest {
        &self.request
    }

    pub fn target(&self) -> &NativeRcsPreparationTarget {
        &self.target
    }

    /// The 1:1 mapping, or `None` for a group preparation.
    pub fn mapping(&self) -> Option<&RcsMapping> {
        match &self.target {
            NativeRcsPreparationTarget::OneToOne(mapping) => Some(mapping),
            NativeRcsPreparationTarget::Group(_) => None,
        }
    }

    /// The member digest the Go prepare request must carry. Only group targets have one; a 1:1
    /// request stays byte-identical because it omits the field.
    pub fn expected_member_digest(&self) -> Option<&str> {
        match &self.target {
            NativeRcsPreparationTarget::OneToOne(_) => None,
            NativeRcsPreparationTarget::Group(authority) => Some(&authority.member_set_digest),
        }
    }

    pub(crate) fn into_parts(
        self,
    ) -> (
        CommandId,
        NativeRcsPreparationTarget,
        Option<(u32, NativeRcsMedia)>,
    ) {
        (
            self.command_id,
            self.target,
            self.attachment_index.zip(self.media),
        )
    }
}

/// Schema bundle for the restricted local-native handoff only; it is not an event or public relay API.
pub fn restricted_native_rcs_handoff_schema() -> serde_json::Value {
    serde_json::json!({
        "prepare_request": schema_for!(NativeRcsPrepareRequest),
        "prepare_observation": schema_for!(RcsMappingObservation),
        "commit_projection": schema_for!(NativeRcsCommitProjection),
    })
}
