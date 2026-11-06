use crate::{MobileBindingsError, NativeClient, NativeConversation, NativeMessage, with_client};
use peppy_client_core::{
    Client, Conversation, Direction, Message, RcsIdentityOutcome, RcsProjectedRecord,
    RcsProjectedStatus, RcsProviderDetector, RcsReceiveCode, RcsReceiveOutcome,
    RcsStatusApplication, Transport,
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
    let rcs = if message.payload.transport == Transport::Rcs {
        client.rcs_message_status(message.payload.record.message_id)?
    } else {
        None
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
        rcs_reported: rcs.is_some_and(|value| value.rcs_reported),
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
fn identity_outcome(value: RcsIdentityOutcome) -> String {
    match value {
        RcsIdentityOutcome::Recorded => "recorded".into(),
        RcsIdentityOutcome::Duplicate => "duplicate".into(),
        RcsIdentityOutcome::Rejected(code) => format!("rejected:{}", receive_code(code)),
    }
}

#[uniffi::export]
impl NativeClient {
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
}
