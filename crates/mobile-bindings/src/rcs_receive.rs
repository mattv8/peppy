use crate::{MobileBindingsError, NativeClient, NativeConversation, NativeMessage, with_client};
use peppy_client_core::{
    Client, Conversation, ConversationId, Direction, Message, RcsContextExpectation, RcsCutover,
    RcsIdentityOutcome, RcsObservationProvenance, RcsOwnerUnlinkCutoverCandidate,
    RcsProjectedRecord, RcsProjectedStatus, RcsProviderDetector, RcsProviderRow,
    RcsProviderRowRoute, RcsReceiveCode, RcsReceiveContext, RcsReceiveOutcome, RcsReceiveOwner,
    RcsStatusApplication, RcsUnlinkCutoverGenerations, Transport,
};
use serde::Deserialize;

pub(crate) fn conversation_view(
    client: &Client,
    item: Conversation,
) -> Result<NativeConversation, peppy_client_core::Error> {
    let metadata = client.rcs_conversation_metadata(item.conversation_id)?;
    Ok(NativeConversation {
        conversation_id: item.conversation_id.to_string(),
        unread_count: item.unread_count,
        pinned: item.pinned,
        rcs_encryption: metadata.as_ref().and_then(|value| value.rcs_encryption),
        rcs_history_gap: metadata.is_some_and(|value| value.rcs_history_gap),
    })
}

pub(crate) fn message_view(
    client: &Client,
    message: Message,
) -> Result<NativeMessage, peppy_client_core::Error> {
    let (rcs, rcs_reported) = if message.payload.transport == Transport::Rcs {
        let message_id = message.payload.record.message_id;
        (
            client.rcs_message_status(message_id)?,
            client.rcs_message_reported(message_id)?.unwrap_or(false),
        )
    } else {
        (None, false)
    };
    Ok(NativeMessage {
        message_id: message.payload.record.message_id.to_string(),
        conversation_id: message.payload.record.conversation_id.to_string(),
        sender_address: message.payload.sender_address,
        recipients: message.payload.recipients,
        body: message.payload.body,
        subject: message.payload.subject,
        transport: match message.payload.transport {
            Transport::Sms => "sms",
            Transport::Mms => "mms",
            Transport::Rcs => "rcs",
        }
        .into(),
        attachment_ids: message
            .payload
            .record
            .attachments
            .into_iter()
            .map(|item| item.attachment_id.to_string())
            .collect(),
        incoming: message.payload.direction == Direction::Incoming,
        seen: message.seen,
        send_state: message
            .send_state
            .map(|state| crate::send_state_name(state).to_owned()),
        sent_at_ms: message.payload.sent_at_ms.filter(|value| *value > 0),
        rcs_status: rcs.as_ref().map(|value| {
            match value.status {
                peppy_client_core::RcsMessageStatus::Sending => "sending",
                peppy_client_core::RcsMessageStatus::Sent => "sent",
                peppy_client_core::RcsMessageStatus::Delivered => "delivered",
                peppy_client_core::RcsMessageStatus::Read => "read",
                peppy_client_core::RcsMessageStatus::Failed => "failed",
            }
            .into()
        }),
        rcs_reported,
    })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ProjectedRecord {
    google_message_id: String,
    google_conversation_id: String,
    direction: String,
    body: String,
    sent_at_ms: Option<i64>,
    tmp_id: Option<String>,
    rcs_reported: bool,
    e2ee_reported: Option<bool>,
    history_token: Option<String>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StatusRecord {
    google_message_id: String,
    google_conversation_id: String,
    tmp_id: Option<String>,
    status: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ProviderDetector {
    provider_row_id: i64,
    provider_thread_id: i64,
    provider_type: i64,
    observed_at_ms: i64,
    observed_generation: u64,
}

fn receive_code(value: RcsReceiveCode) -> &'static str {
    match value {
        RcsReceiveCode::OwnershipStale => "ownership_stale",
        RcsReceiveCode::ConversationUnproven => "conversation_unproven",
        RcsReceiveCode::IdentityConflict => "identity_conflict",
        RcsReceiveCode::CursorInvalid => "cursor_invalid",
        RcsReceiveCode::ProviderFloor => "provider_floor",
        RcsReceiveCode::GatewayUnavailable => "gateway_unavailable",
    }
}
fn receive_outcome(value: RcsReceiveOutcome) -> String {
    match value {
        RcsReceiveOutcome::Inserted => "inserted",
        RcsReceiveOutcome::Duplicate => "duplicate",
        RcsReceiveOutcome::UnmatchedOutgoing => "unmatched_outgoing",
        RcsReceiveOutcome::OutgoingMatched => "outgoing_matched",
        RcsReceiveOutcome::StatusApplied => "status_applied",
        RcsReceiveOutcome::StatusUnchanged => "status_unchanged",
        RcsReceiveOutcome::Rejected(code) => return format!("rejected:{}", receive_code(code)),
    }
    .into()
}
const GO_RECORD_JSON_CAP: usize = 65_536;
const GO_ID_CAP: usize = 512;
const GO_BODY_CAP: usize = 16_384;
const GO_CURSOR_CAP: usize = 4_096;

/// Exact shape of Go's `rcsReceiveRecord` (`apps/android/googlemessages/gmlink/receive.go`).
/// `cursor`, `tmp_id`, and `e2ee_reported` are `omitempty` on the Go side; all other keys are always emitted.
#[derive(Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
struct GoReceiveRecord {
    google_message_id: String,
    google_conversation_id: String,
    cursor: Option<String>,
    direction: String,
    body: String,
    sent_at_ms: i64,
    status: String,
    tmp_id: Option<String>,
    rcs_reported: bool,
    e2ee_reported: Option<bool>,
}

struct GoReceiveParts {
    record: RcsProjectedRecord,
    status: Option<RcsStatusApplication>,
    cursor: Option<String>,
}

fn bounded_text(value: &str, max: usize) -> Result<(), MobileBindingsError> {
    if value.is_empty() || value.len() > max {
        return Err(MobileBindingsError::InvalidRequest);
    }
    Ok(())
}

fn valid_go_cursor(value: &str) -> Result<(), MobileBindingsError> {
    bounded_text(value, GO_CURSOR_CAP)?;
    if !value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Err(MobileBindingsError::InvalidRequest);
    }
    Ok(())
}

impl GoReceiveRecord {
    fn into_parts(self) -> Result<GoReceiveParts, MobileBindingsError> {
        bounded_text(&self.google_message_id, GO_ID_CAP)?;
        bounded_text(&self.google_conversation_id, GO_ID_CAP)?;
        if self.body.len() > GO_BODY_CAP {
            return Err(MobileBindingsError::InvalidRequest);
        }
        if let Some(tmp_id) = self.tmp_id.as_deref() {
            bounded_text(tmp_id, GO_ID_CAP)?;
        }
        if let Some(cursor) = self.cursor.as_deref() {
            valid_go_cursor(cursor)?;
        }
        let direction = match self.direction.as_str() {
            "incoming" => Direction::Incoming,
            "outgoing" => Direction::Outgoing,
            _ => return Err(MobileBindingsError::InvalidRequest),
        };
        let status = match self.status.as_str() {
            "" => None,
            "sending" => Some(RcsProjectedStatus::Sending),
            "sent" => Some(RcsProjectedStatus::Sent),
            "failed" => Some(RcsProjectedStatus::Failed),
            "delivered" => Some(RcsProjectedStatus::Delivered),
            "read" => Some(RcsProjectedStatus::Read),
            _ => return Err(MobileBindingsError::InvalidRequest),
        };
        let sent_at_ms = Some(self.sent_at_ms);
        let e2ee_reported = self.rcs_reported.then_some(self.e2ee_reported).flatten();
        Ok(GoReceiveParts {
            record: RcsProjectedRecord {
                google_message_id: self.google_message_id.clone(),
                google_conversation_id: self.google_conversation_id.clone(),
                direction,
                body: self.body,
                sent_at_ms,
                tmp_id: self.tmp_id.clone(),
                rcs_reported: self.rcs_reported,
                e2ee_reported,
                // The cursor is never checkpointed inside ingest; the caller persists it after success.
                history_token: None,
            },
            status: status.map(|status| RcsStatusApplication {
                google_message_id: self.google_message_id,
                google_conversation_id: self.google_conversation_id,
                tmp_id: self.tmp_id,
                status,
            }),
            cursor: self.cursor,
        })
    }
}

/// Closed outcomes plus the opaque Google page cursor. The cursor is present only when the
/// record was accepted (not rejected) and carried one; the caller hands it back to
/// `gateway_checkpoint_rcs_history` only after this call has succeeded.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct NativeRcsIngestOutcome {
    pub record_outcome: String,
    pub status_outcome: Option<String>,
    pub cursor: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum NativeRcsObservationProvenance {
    Live,
    Historical,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct NativeRcsContextExpectation {
    pub ownership_generation: u64,
    pub link_generation: u64,
    pub session_generation: u64,
    pub context_revision: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct NativeRcsCutover {
    pub ownership_generation: u64,
    pub freeze_detector_floor: i64,
    pub ambiguous_detector_alert: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct NativeRcsUnlinkCutoverGenerations {
    pub ownership_generation: u64,
    pub lifecycle_generation: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct NativeRcsOwnerUnlinkCutoverCandidate {
    pub conversation_id: String,
    pub ownership_generation: u64,
    pub lifecycle_generation: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum NativeRcsProviderRowRoute {
    Provider,
    DetectorRecorded,
    Alert,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ProviderRowJson {
    provider_row_id: i64,
    provider_thread_id: i64,
    provider_type: i64,
    observed_at_ms: i64,
    addresses: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum NativeRcsReceiveOwner {
    Link,
    Provider,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct NativeRcsReceiveContext {
    pub conversation_id: String,
    pub google_conversation_id: String,
    pub ownership_generation: u64,
    pub lifecycle_generation: u64,
    pub owner: NativeRcsReceiveOwner,
    pub history_token: Option<String>,
    pub expectation: NativeRcsContextExpectation,
}

fn observation_provenance(value: NativeRcsObservationProvenance) -> RcsObservationProvenance {
    match value {
        NativeRcsObservationProvenance::Live => RcsObservationProvenance::Live,
        NativeRcsObservationProvenance::Historical => RcsObservationProvenance::Historical,
    }
}

fn cutover_view(value: RcsCutover) -> NativeRcsCutover {
    NativeRcsCutover {
        ownership_generation: value.ownership_generation,
        freeze_detector_floor: value.freeze_detector_floor,
        ambiguous_detector_alert: value.ambiguous_detector_alert,
    }
}

fn cutover_generations_view(
    value: RcsUnlinkCutoverGenerations,
) -> NativeRcsUnlinkCutoverGenerations {
    NativeRcsUnlinkCutoverGenerations {
        ownership_generation: value.ownership_generation,
        lifecycle_generation: value.lifecycle_generation,
    }
}

fn unlink_candidate_view(
    value: RcsOwnerUnlinkCutoverCandidate,
) -> NativeRcsOwnerUnlinkCutoverCandidate {
    NativeRcsOwnerUnlinkCutoverCandidate {
        conversation_id: value.conversation_id.to_string(),
        ownership_generation: value.ownership_generation,
        lifecycle_generation: value.lifecycle_generation,
    }
}

fn provider_row_route(value: RcsProviderRowRoute) -> NativeRcsProviderRowRoute {
    match value {
        RcsProviderRowRoute::Provider => NativeRcsProviderRowRoute::Provider,
        RcsProviderRowRoute::DetectorRecorded => NativeRcsProviderRowRoute::DetectorRecorded,
        RcsProviderRowRoute::Alert => NativeRcsProviderRowRoute::Alert,
    }
}

fn receive_context_view(value: RcsReceiveContext) -> NativeRcsReceiveContext {
    NativeRcsReceiveContext {
        conversation_id: value.conversation_id.to_string(),
        google_conversation_id: value.google_conversation_id,
        ownership_generation: value.ownership_generation,
        lifecycle_generation: value.lifecycle_generation,
        owner: match value.owner {
            RcsReceiveOwner::Link => NativeRcsReceiveOwner::Link,
            RcsReceiveOwner::Provider => NativeRcsReceiveOwner::Provider,
        },
        history_token: value.history_token,
        expectation: NativeRcsContextExpectation {
            ownership_generation: value.expectation.ownership_generation,
            link_generation: value.expectation.link_generation,
            session_generation: value.expectation.session_generation,
            context_revision: value.expectation.context_revision,
        },
    }
}

fn ingest_go_record(
    owner: &NativeClient,
    conversation_id: String,
    ownership_generation: u64,
    go_record_json: String,
    ingest: impl FnOnce(
        &Client,
        ConversationId,
        u64,
        RcsProjectedRecord,
    ) -> Result<RcsReceiveOutcome, peppy_client_core::Error>,
) -> Result<NativeRcsIngestOutcome, MobileBindingsError> {
    if go_record_json.len() > GO_RECORD_JSON_CAP {
        return Err(MobileBindingsError::InvalidRequest);
    }
    let conversation = conversation_id
        .parse()
        .map_err(|_| MobileBindingsError::InvalidRequest)?;
    let go_record: GoReceiveRecord =
        serde_json::from_str(&go_record_json).map_err(|_| MobileBindingsError::InvalidRequest)?;
    let GoReceiveParts {
        record,
        status,
        cursor,
    } = go_record.into_parts()?;
    with_client(&owner.client, |client| {
        let record_outcome = ingest(client, conversation, ownership_generation, record)?;
        if matches!(record_outcome, RcsReceiveOutcome::Rejected(_)) {
            return Ok(NativeRcsIngestOutcome {
                record_outcome: receive_outcome(record_outcome),
                status_outcome: None,
                cursor: None,
            });
        }
        let status_outcome = status
            .map(|status| {
                client.apply_rcs_projected_status(conversation, ownership_generation, status)
            })
            .transpose()?;
        let cursor = if status_outcome
            .is_some_and(|outcome| matches!(outcome, RcsReceiveOutcome::Rejected(_)))
        {
            None
        } else {
            cursor
        };
        Ok(NativeRcsIngestOutcome {
            record_outcome: receive_outcome(record_outcome),
            status_outcome: status_outcome.map(receive_outcome),
            cursor,
        })
    })
}

fn identity_outcome(value: RcsIdentityOutcome) -> String {
    match value {
        RcsIdentityOutcome::Recorded => "recorded".into(),
        RcsIdentityOutcome::Duplicate => "duplicate".into(),
        RcsIdentityOutcome::Rejected(code) => format!("rejected:{}", receive_code(code)),
    }
}

#[uniffi::export]
impl NativeClient {
    /// Accepts exactly the record object Go emits (`rcsReceiveRecord`). Ingest runs without a
    /// checkpoint; an inline status is applied only after an accepted ingest. Retries are
    /// replay-safe: ingest reports `duplicate` and the status is idempotent.
    pub fn gateway_ingest_go_rcs_record_json(
        &self,
        conversation_id: String,
        ownership_generation: u64,
        go_record_json: String,
    ) -> Result<NativeRcsIngestOutcome, MobileBindingsError> {
        ingest_go_record(
            self,
            conversation_id,
            ownership_generation,
            go_record_json,
            |client, conversation, generation, record| {
                client.ingest_rcs_projected_record(conversation, generation, record)
            },
        )
    }
    pub fn gateway_ingest_rcs_projected_observation(
        &self,
        conversation_id: String,
        ownership_generation: u64,
        go_record_json: String,
        provenance: NativeRcsObservationProvenance,
    ) -> Result<NativeRcsIngestOutcome, MobileBindingsError> {
        let provenance = observation_provenance(provenance);
        ingest_go_record(
            self,
            conversation_id,
            ownership_generation,
            go_record_json,
            |client, conversation, generation, record| {
                client.ingest_rcs_projected_observation(
                    conversation,
                    generation,
                    record,
                    provenance,
                )
            },
        )
    }
    pub fn gateway_ingest_rcs_record_json(
        &self,
        conversation_id: String,
        ownership_generation: u64,
        record_json: String,
    ) -> Result<String, MobileBindingsError> {
        if record_json.len() > 65_536 {
            return Err(MobileBindingsError::InvalidRequest);
        }
        let record: ProjectedRecord =
            serde_json::from_str(&record_json).map_err(|_| MobileBindingsError::InvalidRequest)?;
        let direction = match record.direction.as_str() {
            "incoming" => Direction::Incoming,
            "outgoing" => Direction::Outgoing,
            _ => return Err(MobileBindingsError::InvalidRequest),
        };
        with_client(&self.client, |client| {
            client.ingest_rcs_projected_record(
                conversation_id.parse().map_err(|_| {
                    peppy_client_core::Error::InvalidRequest("invalid conversation")
                })?,
                ownership_generation,
                RcsProjectedRecord {
                    google_message_id: record.google_message_id,
                    google_conversation_id: record.google_conversation_id,
                    direction,
                    body: record.body,
                    sent_at_ms: record.sent_at_ms,
                    tmp_id: record.tmp_id,
                    rcs_reported: record.rcs_reported,
                    e2ee_reported: record
                        .rcs_reported
                        .then_some(record.e2ee_reported)
                        .flatten(),
                    history_token: record.history_token,
                },
            )
        })
        .map(receive_outcome)
    }
    pub fn gateway_apply_rcs_status_json(
        &self,
        conversation_id: String,
        ownership_generation: u64,
        status_json: String,
    ) -> Result<String, MobileBindingsError> {
        if status_json.len() > 65_536 {
            return Err(MobileBindingsError::InvalidRequest);
        }
        let status: StatusRecord =
            serde_json::from_str(&status_json).map_err(|_| MobileBindingsError::InvalidRequest)?;
        let projected_status = match status.status.as_str() {
            "sending" => RcsProjectedStatus::Sending,
            "sent" => RcsProjectedStatus::Sent,
            "failed" => RcsProjectedStatus::Failed,
            "delivered" => RcsProjectedStatus::Delivered,
            "read" => RcsProjectedStatus::Read,
            _ => return Err(MobileBindingsError::InvalidRequest),
        };
        with_client(&self.client, |client| {
            client.apply_rcs_projected_status(
                conversation_id.parse().map_err(|_| {
                    peppy_client_core::Error::InvalidRequest("invalid conversation")
                })?,
                ownership_generation,
                RcsStatusApplication {
                    google_message_id: status.google_message_id,
                    google_conversation_id: status.google_conversation_id,
                    tmp_id: status.tmp_id,
                    status: projected_status,
                },
            )
        })
        .map(receive_outcome)
    }
    pub fn gateway_checkpoint_rcs_history(
        &self,
        conversation_id: String,
        ownership_generation: u64,
        history_token: String,
    ) -> Result<String, MobileBindingsError> {
        if history_token.len() > 4_096 {
            return Err(MobileBindingsError::InvalidRequest);
        }
        with_client(&self.client, |client| {
            client.checkpoint_rcs_history(
                conversation_id.parse().map_err(|_| {
                    peppy_client_core::Error::InvalidRequest("invalid conversation")
                })?,
                ownership_generation,
                &history_token,
            )
        })
        .map(identity_outcome)
    }
    pub fn gateway_rcs_owner_unlink_cutover_candidates(
        &self,
    ) -> Result<Vec<NativeRcsOwnerUnlinkCutoverCandidate>, MobileBindingsError> {
        with_client(&self.client, |client| {
            client.rcs_owner_unlink_cutover_candidates()
        })
        .map(|values| values.into_iter().map(unlink_candidate_view).collect())
    }

    pub fn gateway_route_rcs_provider_row_json(
        &self,
        provider_row_json: String,
    ) -> Result<NativeRcsProviderRowRoute, MobileBindingsError> {
        if provider_row_json.len() > GO_RECORD_JSON_CAP {
            return Err(MobileBindingsError::InvalidRequest);
        }
        let row: ProviderRowJson = serde_json::from_str(&provider_row_json)
            .map_err(|_| MobileBindingsError::InvalidRequest)?;
        with_client(&self.client, |client| {
            client.route_rcs_provider_row(RcsProviderRow {
                provider_row_id: row.provider_row_id,
                provider_thread_id: row.provider_thread_id,
                provider_type: row.provider_type,
                observed_at_ms: row.observed_at_ms,
                addresses: row.addresses,
            })
        })
        .map(provider_row_route)
    }

    pub fn gateway_record_rcs_provider_detector_json(
        &self,
        conversation_id: String,
        ownership_generation: u64,
        detector_json: String,
    ) -> Result<String, MobileBindingsError> {
        if detector_json.len() > 65_536 {
            return Err(MobileBindingsError::InvalidRequest);
        }
        let detector: ProviderDetector = serde_json::from_str(&detector_json)
            .map_err(|_| MobileBindingsError::InvalidRequest)?;
        with_client(&self.client, |client| {
            client.record_rcs_provider_detector(
                conversation_id.parse().map_err(|_| {
                    peppy_client_core::Error::InvalidRequest("invalid conversation")
                })?,
                ownership_generation,
                RcsProviderDetector {
                    provider_row_id: detector.provider_row_id,
                    provider_thread_id: detector.provider_thread_id,
                    provider_type: detector.provider_type,
                    observed_at_ms: detector.observed_at_ms,
                    observed_generation: detector.observed_generation,
                },
            )
        })
        .map(identity_outcome)
    }
    pub fn gateway_begin_rcs_unlink_cutover_if_current(
        &self,
        conversation_id: String,
        expected_ownership_generation: u64,
    ) -> Result<NativeRcsCutover, MobileBindingsError> {
        let conversation = conversation_id
            .parse()
            .map_err(|_| MobileBindingsError::InvalidRequest)?;
        with_client(&self.client, |client| {
            client.begin_rcs_unlink_cutover_if_current(conversation, expected_ownership_generation)
        })
        .map(cutover_view)
    }
    pub fn gateway_begin_rcs_owner_unlink_cutover(
        &self,
        conversation_id: String,
        expected_ownership_generation: u64,
        expected_lifecycle_generation: u64,
    ) -> Result<NativeRcsCutover, MobileBindingsError> {
        let conversation = conversation_id
            .parse()
            .map_err(|_| MobileBindingsError::InvalidRequest)?;
        with_client(&self.client, |client| {
            client.begin_rcs_owner_unlink_cutover(
                conversation,
                expected_ownership_generation,
                expected_lifecycle_generation,
            )
        })
        .map(cutover_view)
    }
    pub fn gateway_invalidate_rcs_context_for_google_event(
        &self,
        conversation_id: String,
        google_conversation_id: String,
        expected: NativeRcsContextExpectation,
    ) -> Result<u64, MobileBindingsError> {
        let conversation = conversation_id
            .parse()
            .map_err(|_| MobileBindingsError::InvalidRequest)?;
        with_client(&self.client, |client| {
            client.invalidate_rcs_context_for_google_event(
                conversation,
                &google_conversation_id,
                RcsContextExpectation {
                    ownership_generation: expected.ownership_generation,
                    link_generation: expected.link_generation,
                    session_generation: expected.session_generation,
                    context_revision: expected.context_revision,
                },
            )
        })
    }
    pub fn gateway_rcs_unlink_cutover_generations(
        &self,
        conversation_id: String,
    ) -> Result<Option<NativeRcsUnlinkCutoverGenerations>, MobileBindingsError> {
        let conversation = conversation_id
            .parse()
            .map_err(|_| MobileBindingsError::InvalidRequest)?;
        with_client(&self.client, |client| {
            client.rcs_unlink_cutover_generations(conversation)
        })
        .map(|value| value.map(cutover_generations_view))
    }
    pub fn gateway_rcs_receive_context(
        &self,
        google_conversation_id: String,
    ) -> Result<Option<NativeRcsReceiveContext>, MobileBindingsError> {
        bounded_text(&google_conversation_id, GO_ID_CAP)?;
        with_client(&self.client, |client| {
            client.rcs_receive_context(&google_conversation_id)
        })
        .map(|value| value.map(receive_context_view))
    }
    pub fn gateway_rcs_link_owned_conversations(
        &self,
    ) -> Result<Vec<NativeRcsReceiveContext>, MobileBindingsError> {
        with_client(&self.client, |client| client.rcs_link_owned_conversations())
            .map(|contexts| contexts.into_iter().map(receive_context_view).collect())
    }
}
