use crate::{
    MobileBindingsError, NativeClient, NativeConversation, NativeMessage, attachment_view,
    with_client,
};
use peppy_client_core::{
    Client, Conversation, ConversationId, Direction, Message, RcsContextExpectation, RcsCutover,
    RcsGroupSender, RcsIdentityOutcome, RcsIncomingAttachment, RcsLinkOwnershipClaim,
    RcsLinkOwnershipClaimOutcome, RcsObservationProvenance, RcsOwnerUnlinkCutoverCandidate,
    RcsProjectedRecord, RcsProjectedStatus, RcsProviderDetector, RcsProviderRouteReason,
    RcsProviderRow, RcsProviderRowRoute, RcsProviderRowRouteResult, RcsReceiveCode,
    RcsReceiveContext, RcsReceiveOutcome, RcsReceiveOwner, RcsRecoveryOutcome,
    RcsStatusApplication, RcsUnlinkCutoverGenerations, Transport,
};
use serde::Deserialize;

/// Native-host-only receive work. `media_ref` is an opaque Google credential and must never be
/// forwarded to UI code or serialized into app diagnostics.
#[derive(Clone, uniffi::Record)]
pub struct NativePendingRcsMediaReceive {
    pub message_id: String,
    pub attachment_index: u32,
    pub media_ref: String,
    pub media_type: String,
    pub display_name: String,
    pub plaintext_bytes: u64,
    pub state: String,
}

/// Result of a trusted host's local pre-download capacity check. A held token is opaque and can
/// only import the exact message/index it was issued for.
#[derive(Clone, Debug, uniffi::Enum)]
pub enum NativeRcsMediaDownloadAdmission {
    Held { token: String },
    ExistingPrepared { attachment_id: String },
    TooLarge,
    QuotaFull,
    CapacityUnavailable,
}

fn receive_admission_view(
    admission: peppy_client_core::ReceiveDownloadAdmission,
) -> NativeRcsMediaDownloadAdmission {
    match admission {
        peppy_client_core::ReceiveDownloadAdmission::Held { token } => {
            NativeRcsMediaDownloadAdmission::Held { token }
        }
        peppy_client_core::ReceiveDownloadAdmission::ExistingPrepared { attachment_id } => {
            NativeRcsMediaDownloadAdmission::ExistingPrepared {
                attachment_id: attachment_id.to_string(),
            }
        }
        peppy_client_core::ReceiveDownloadAdmission::TooLarge => {
            NativeRcsMediaDownloadAdmission::TooLarge
        }
        peppy_client_core::ReceiveDownloadAdmission::QuotaFull => {
            NativeRcsMediaDownloadAdmission::QuotaFull
        }
        peppy_client_core::ReceiveDownloadAdmission::CapacityUnavailable => {
            NativeRcsMediaDownloadAdmission::CapacityUnavailable
        }
    }
}

fn pending_receive_view(
    task: peppy_client_core::PendingReceiveTask,
) -> NativePendingRcsMediaReceive {
    NativePendingRcsMediaReceive {
        message_id: task.message_id.to_string(),
        attachment_index: task.attachment_index,
        media_ref: task.media_ref,
        media_type: task.media_type,
        display_name: task.display_name,
        plaintext_bytes: task.plaintext_bytes,
        state: task.state.code().into(),
    }
}

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
    let is_rcs = message.payload.transport == Transport::Rcs;
    let (rcs, rcs_reported) = if is_rcs || message.payload.direction == Direction::Outgoing {
        let message_id = message.payload.record.message_id;
        (
            client.rcs_message_status(message_id)?,
            client.rcs_message_reported(message_id)?,
        )
    } else {
        (None, None)
    };
    let has_rcs_evidence = rcs.is_some() || rcs_reported == Some(true);
    Ok(NativeMessage {
        message_id: message.payload.record.message_id.to_string(),
        conversation_id: message.payload.record.conversation_id.to_string(),
        sender_address: message.payload.sender_address,
        recipients: message.payload.recipients,
        body: message.payload.body,
        subject: message.payload.subject,
        transport: if is_rcs || has_rcs_evidence {
            "rcs"
        } else {
            match message.payload.transport {
                Transport::Sms => "sms",
                Transport::Mms => "mms",
                Transport::Rcs => "rcs",
            }
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
        send_refusal: message.send_refusal.map(|reason| reason.code().to_owned()),
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
        rcs_reported: rcs_reported.unwrap_or(false),
    })
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
const MAX_ALTERNATE_MESSAGE_IDS: usize = 4;

/// Exact shape of Go's `rcsReceiveRecord` (`apps/android/googlemessages/gmlink/receive.go`).
/// `tmp_id` and `e2ee_reported` are `omitempty` on the Go side; all other keys are always emitted.
/// Unknown keys (including a per-record `cursor`) are rejected.
#[derive(Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
struct GoReceiveRecord {
    google_message_id: String,
    google_conversation_id: String,
    direction: String,
    body: String,
    sent_at_ms: i64,
    status: String,
    tmp_id: Option<String>,
    rcs_reported: bool,
    e2ee_reported: Option<bool>,
    /// Group incoming messages only: canonical E.164 of the visible sender.
    #[serde(default)]
    sender_address: Option<String>,
    /// Group incoming messages only: the sender could not be resolved.
    #[serde(default)]
    sender_unknown: bool,
    /// Group outgoing messages only: sent from another device of this vault.
    #[serde(default)]
    self_sent: bool,
    #[serde(default)]
    attachments: Vec<GoReceiveAttachment>,
    /// Other ids Google gives the same message; `omitempty` on the Go side.
    #[serde(default)]
    alternate_message_ids: Vec<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
struct GoReceiveAttachment {
    index: u32,
    mime: String,
    size: u64,
    name: String,
    media_ref: String,
    too_large: bool,
}

struct GoReceiveParts {
    record: RcsProjectedRecord,
    group_sender: Option<RcsGroupSender>,
    status: Option<RcsStatusApplication>,
}

/// Maps Go's group sender fields. Exactly one of `sender_address` or `sender_unknown` may appear
/// on an incoming message, and `self_sent` is valid only on an outgoing one.
fn group_sender(
    direction: Direction,
    sender_address: Option<String>,
    sender_unknown: bool,
    self_sent: bool,
) -> Result<Option<RcsGroupSender>, MobileBindingsError> {
    let attributed = match sender_address {
        Some(address) => {
            bounded_text(&address, GO_ID_CAP)?;
            Some(address)
        }
        None => None,
    };
    match (direction, attributed, sender_unknown, self_sent) {
        (Direction::Incoming, Some(address), false, false) => {
            Ok(Some(RcsGroupSender::Address(address)))
        }
        (Direction::Incoming, None, true, false) => Ok(Some(RcsGroupSender::UnknownMember)),
        (Direction::Outgoing, None, false, true) => Ok(Some(RcsGroupSender::SelfSent)),
        (_, None, false, false) => Ok(None),
        _ => Err(MobileBindingsError::InvalidRequest),
    }
}

fn bounded_text(value: &str, max: usize) -> Result<(), MobileBindingsError> {
    if value.is_empty() || value.len() > max {
        return Err(MobileBindingsError::InvalidRequest);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use peppy_client_core::{
        ClientConfig, DatabaseKey, DeviceId, MessagePayload, MessageRecord, SourceSequence, VaultId,
    };

    const SQLCIPHER_KEY: &str =
        "x'0909090909090909090909090909090909090909090909090909090909090909'";

    fn message(transport: Transport) -> Message {
        Message {
            payload: MessagePayload {
                record: MessageRecord {
                    message_id: peppy_client_core::MessageId::new(),
                    conversation_id: ConversationId::new(),
                    source_sequence: SourceSequence(1),
                    attachments: vec![],
                },
                source_device_id: DeviceId::new(),
                provider_message_id: None,
                sender_address: None,
                recipients: vec!["+14155552671".into()],
                subject: None,
                body: "native RCS text".into(),
                transport,
                direction: Direction::Outgoing,
                rcs_capture: false,
                imported: false,
                sent_at_ms: Some(1),
                ordered_at_ms: None,
                mms_context: None,
            },
            seen: true,
            send_state: None,
            status_at_ms: Some(2),
            send_refusal: None,
        }
    }

    fn client() -> (Client, std::path::PathBuf) {
        let path = std::env::temp_dir().join(format!("peppy-rcs-view-{}.db", uuid::Uuid::new_v4()));
        let client = Client::open(
            ClientConfig {
                database_path: path.clone(),
                vault_id: VaultId::new(),
                device_id: DeviceId::new(),
            },
            DatabaseKey::new(&[9; 32]).unwrap(),
        )
        .unwrap();
        (client, path)
    }

    fn seed(client_path: &std::path::Path, message: &Message, rank: i64, reported: bool) {
        let db = rusqlite::Connection::open(client_path).unwrap();
        db.execute_batch(&format!("PRAGMA key = \"{SQLCIPHER_KEY}\";"))
            .unwrap();
        db.execute(
            "INSERT INTO rcs_message_status(message_id,status_rank,rcs_reported) VALUES(?,?,?)",
            rusqlite::params![
                message.payload.record.message_id.to_string(),
                rank,
                reported
            ],
        )
        .unwrap();
    }

    fn remove_database(path: &std::path::Path) {
        for suffix in ["", "-wal", "-shm"] {
            let mut file = path.as_os_str().to_owned();
            file.push(suffix);
            let _ = std::fs::remove_file(file);
        }
    }

    #[test]
    fn outgoing_mms_and_sms_without_rcs_evidence_keep_their_transport() {
        let (client, path) = client();
        for (transport, expected) in [(Transport::Mms, "mms"), (Transport::Sms, "sms")] {
            let view = message_view(&client, message(transport)).unwrap();
            assert_eq!(view.transport, expected);
            assert_eq!(view.rcs_status, None);
            assert!(!view.rcs_reported);
        }
        drop(client);
        remove_database(&path);
    }

    #[test]
    fn outgoing_mms_and_sms_with_read_evidence_project_as_rcs() {
        let (client, path) = client();
        for transport in [Transport::Mms, Transport::Sms] {
            let message = message(transport);
            seed(&path, &message, 5, true);

            let view = message_view(&client, message).unwrap();

            assert_eq!(view.transport, "rcs");
            assert_eq!(view.rcs_status.as_deref(), Some("read"));
            assert!(view.rcs_reported);
        }
        drop(client);
        remove_database(&path);
    }

    #[test]
    fn outgoing_mms_with_echo_only_evidence_projects_as_rcs() {
        let (client, path) = client();
        let message = message(Transport::Mms);
        seed(&path, &message, 0, true);

        let view = message_view(&client, message).unwrap();

        assert_eq!(view.transport, "rcs");
        assert_eq!(view.rcs_status, None);
        assert!(view.rcs_reported);
        drop(client);
        remove_database(&path);
    }

    #[test]
    fn outgoing_mms_and_sms_with_unreported_echo_only_evidence_keep_their_transport() {
        let (client, path) = client();
        for (transport, expected) in [(Transport::Mms, "mms"), (Transport::Sms, "sms")] {
            let message = message(transport);
            seed(&path, &message, 0, false);

            let view = message_view(&client, message).unwrap();

            assert_eq!(view.transport, expected);
            assert_eq!(view.rcs_status, None);
            assert!(!view.rcs_reported);
        }
        drop(client);
        remove_database(&path);
    }

    #[test]
    fn outgoing_mms_with_unreported_failed_evidence_stays_failed() {
        let (client, path) = client();
        let message = message(Transport::Mms);
        seed(&path, &message, 3, false);

        let view = message_view(&client, message).unwrap();

        assert_eq!(view.transport, "rcs");
        assert_eq!(view.rcs_status.as_deref(), Some("failed"));
        assert!(!view.rcs_reported);
        drop(client);
        remove_database(&path);
    }
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
        if self.alternate_message_ids.len() > MAX_ALTERNATE_MESSAGE_IDS {
            return Err(MobileBindingsError::InvalidRequest);
        }
        for alternate in &self.alternate_message_ids {
            bounded_text(alternate, GO_ID_CAP)?;
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
        let group_sender = group_sender(
            direction,
            self.sender_address,
            self.sender_unknown,
            self.self_sent,
        )?;
        if self.attachments.len() > 10 {
            return Err(MobileBindingsError::InvalidRequest);
        }
        let attachments = self
            .attachments
            .into_iter()
            .enumerate()
            .map(|(position, attachment)| {
                if attachment.index != position as u32
                    || attachment.mime.is_empty()
                    || attachment.mime.len() > 255
                    || attachment.name.is_empty()
                    || attachment.name.len() > 1024
                    || (!attachment.too_large && attachment.media_ref.is_empty())
                    || attachment.media_ref.len() > 16 * 1024
                    || (!attachment.too_large && attachment.size > 32 * 1024 * 1024)
                {
                    return Err(MobileBindingsError::InvalidRequest);
                }
                Ok(RcsIncomingAttachment {
                    original_index: attachment.index,
                    media_type: attachment.mime,
                    display_name: attachment.name,
                    plaintext_bytes: attachment.size,
                    too_large: attachment.too_large || attachment.size > 32 * 1024 * 1024,
                    media_ref: attachment.media_ref,
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
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
                alternate_message_ids: self.alternate_message_ids,
                attachments,
            },
            group_sender,
            status: status.map(|status| RcsStatusApplication {
                google_message_id: self.google_message_id,
                google_conversation_id: self.google_conversation_id,
                tmp_id: self.tmp_id,
                status,
            }),
        })
    }
}

/// Closed record and status outcomes. Recovery progress is recorded only with
/// `gateway_record_rcs_recovery_progress`.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct NativeRcsIngestOutcome {
    pub record_outcome: String,
    pub status_outcome: Option<String>,
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
    Defer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum NativeRcsProviderRouteReason {
    NotRestrictedText,
    NotLinkOwned,
    PreProof,
    GuardBand,
    LinkLate,
    Unidentified,
    Media,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct NativeRcsProviderRoute {
    pub route: NativeRcsProviderRowRoute,
    pub reason: Option<NativeRcsProviderRouteReason>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ProviderRowJson {
    provider_row_id: i64,
    provider_thread_id: i64,
    provider_type: i64,
    observed_at_ms: i64,
    addresses: Vec<String>,
    #[serde(default)]
    restricted: bool,
    #[serde(default)]
    text_only: bool,
    #[serde(default)]
    transaction_id: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct LinkOwnershipClaimJson {
    conversation_id: String,
    google_conversation_id: String,
    mapping_revision: u64,
    provider_floor: i64,
    expected_lifecycle_generation: u64,
    expected_link_generation: u64,
    expected_session_generation: u64,
    expected_context_revision: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum NativeRcsLinkOwnershipClaimOutcome {
    Claimed,
    Refused,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct NativeRcsLinkOwnershipClaim {
    pub outcome: NativeRcsLinkOwnershipClaimOutcome,
    pub ownership_generation: u64,
    pub proof_time_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct NativeRcsCutoverAlerts {
    pub count: u64,
    pub conversation_ids: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum NativeRcsReceiveOwner {
    Link,
    Provider,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct NativeRcsResumeHint {
    pub target_ms: i64,
    pub cursor: String,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct NativeRcsReceiveContext {
    pub conversation_id: String,
    pub google_conversation_id: String,
    pub ownership_generation: u64,
    pub lifecycle_generation: u64,
    pub owner: NativeRcsReceiveOwner,
    /// Newest sort time fully traversed by historical catch-up (ms).
    pub watermark_ms: i64,
    /// Advisory resume point for an interrupted traversal; correctness never depends on it.
    pub resume_hint: Option<NativeRcsResumeHint>,
    /// True while a catch-up gap is open (a later trigger retries).
    pub incomplete: bool,
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
        RcsProviderRowRoute::Defer => NativeRcsProviderRowRoute::Defer,
    }
}

fn provider_route_reason(value: RcsProviderRouteReason) -> NativeRcsProviderRouteReason {
    match value {
        RcsProviderRouteReason::NotRestrictedText => {
            NativeRcsProviderRouteReason::NotRestrictedText
        }
        RcsProviderRouteReason::NotLinkOwned => NativeRcsProviderRouteReason::NotLinkOwned,
        RcsProviderRouteReason::PreProof => NativeRcsProviderRouteReason::PreProof,
        RcsProviderRouteReason::GuardBand => NativeRcsProviderRouteReason::GuardBand,
        RcsProviderRouteReason::LinkLate => NativeRcsProviderRouteReason::LinkLate,
        RcsProviderRouteReason::Unidentified => NativeRcsProviderRouteReason::Unidentified,
        RcsProviderRouteReason::Media => NativeRcsProviderRouteReason::Media,
    }
}

fn provider_route_view(value: RcsProviderRowRouteResult) -> NativeRcsProviderRoute {
    NativeRcsProviderRoute {
        route: provider_row_route(value.route),
        reason: value.reason.map(provider_route_reason),
    }
}

fn ownership_claim_view(value: RcsLinkOwnershipClaimOutcome) -> NativeRcsLinkOwnershipClaim {
    match value {
        RcsLinkOwnershipClaimOutcome::Claimed(ownership) => NativeRcsLinkOwnershipClaim {
            outcome: NativeRcsLinkOwnershipClaimOutcome::Claimed,
            ownership_generation: ownership.ownership_generation,
            proof_time_ms: ownership.proof_time_ms,
        },
        RcsLinkOwnershipClaimOutcome::Refused => NativeRcsLinkOwnershipClaim {
            outcome: NativeRcsLinkOwnershipClaimOutcome::Refused,
            ownership_generation: 0,
            proof_time_ms: 0,
        },
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
        watermark_ms: value.recovery.watermark_ms,
        resume_hint: value.recovery.resume_hint.map(|hint| NativeRcsResumeHint {
            target_ms: hint.target_ms,
            cursor: hint.cursor,
        }),
        incomplete: value.recovery.incomplete,
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
        Option<RcsGroupSender>,
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
        group_sender,
        status,
    } = go_record.into_parts()?;
    with_client(&owner.client, |client| {
        let record_outcome = ingest(
            client,
            conversation,
            ownership_generation,
            record,
            group_sender,
        )?;
        if matches!(record_outcome, RcsReceiveOutcome::Rejected(_)) {
            return Ok(NativeRcsIngestOutcome {
                record_outcome: receive_outcome(record_outcome),
                status_outcome: None,
            });
        }
        let status_outcome = status
            .map(|status| {
                client.apply_rcs_projected_status(conversation, ownership_generation, status)
            })
            .transpose()?;
        Ok(NativeRcsIngestOutcome {
            record_outcome: receive_outcome(record_outcome),
            status_outcome: status_outcome.map(receive_outcome),
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

fn recovery_outcome(value: RcsRecoveryOutcome) -> String {
    match value {
        RcsRecoveryOutcome::Recorded => "recorded".into(),
        RcsRecoveryOutcome::Rejected(code) => format!("rejected:{}", receive_code(code)),
    }
}

#[uniffi::export]
impl NativeClient {
    /// Trusted Android gateway work only. The returned opaque reference must be supplied solely to
    /// Go's private download API and never to a renderer.
    pub fn gateway_pending_rcs_media_receive_tasks(
        &self,
    ) -> Result<Vec<NativePendingRcsMediaReceive>, MobileBindingsError> {
        with_client(&self.client, Client::pending_rcs_media_receive_tasks)
            .map(|tasks| tasks.into_iter().map(pending_receive_view).collect())
    }

    /// Performs only durable local accounting. `available_bytes` must be an authenticated host
    /// capacity observation; absent capacity is intentionally retryable rather than quota-full.
    pub fn gateway_admit_rcs_media_download(
        &self,
        message_id: String,
        attachment_index: u32,
        available_bytes: Option<u64>,
    ) -> Result<NativeRcsMediaDownloadAdmission, MobileBindingsError> {
        let message_id = message_id
            .parse()
            .map_err(|_| MobileBindingsError::InvalidRequest)?;
        with_client(&self.client, |client| {
            client.admit_rcs_media_download(message_id, attachment_index, available_bytes)
        })
        .map(receive_admission_view)
    }

    /// Encrypts a trusted private download after a held admission. The source must have exactly
    /// the manifest's declared length; the token cannot be reused for another attachment.
    pub fn gateway_import_rcs_media_download(
        &self,
        message_id: String,
        attachment_index: u32,
        admission_token: String,
        plaintext_path: String,
    ) -> Result<crate::NativeAttachmentInfo, MobileBindingsError> {
        let message_id = message_id
            .parse()
            .map_err(|_| MobileBindingsError::InvalidRequest)?;
        with_client(&self.client, |client| {
            client.import_rcs_media_download(
                message_id,
                attachment_index,
                &admission_token,
                std::path::Path::new(&plaintext_path),
            )
        })
        .map(attachment_view)
    }

    /// Emits `Stored` only after the normal hosted upload path set a real remote descriptor.
    pub fn gateway_finalize_rcs_media_download(
        &self,
        message_id: String,
        attachment_index: u32,
    ) -> Result<(), MobileBindingsError> {
        let message_id = message_id
            .parse()
            .map_err(|_| MobileBindingsError::InvalidRequest)?;
        with_client(&self.client, |client| {
            client.finalize_rcs_media_download(message_id, attachment_index)
        })
    }

    /// Marks a trusted host download as retryable/terminal without allowing a caller to fabricate
    /// `Stored`; stored requires a real hosted descriptor through `gateway_finalize_rcs_media_download`.
    pub fn gateway_mark_rcs_media_download_state(
        &self,
        message_id: String,
        attachment_index: u32,
        state: String,
    ) -> Result<(), MobileBindingsError> {
        let message_id = message_id
            .parse()
            .map_err(|_| MobileBindingsError::InvalidRequest)?;
        let state = match state.as_str() {
            "downloading" => peppy_client_core::RcsMediaSyncState::Downloading,
            "failed" => peppy_client_core::RcsMediaSyncState::Failed,
            "quota_full" => peppy_client_core::RcsMediaSyncState::QuotaFull,
            "expired" => peppy_client_core::RcsMediaSyncState::Expired,
            _ => return Err(MobileBindingsError::InvalidRequest),
        };
        with_client(&self.client, |client| {
            client.mark_rcs_media_download_state(message_id, attachment_index, state)
        })
    }

    /// Desktop and UI callers address an original attachment index; core derives the authenticated
    /// source gateway and rejects states that cannot be retried.
    pub fn demand_rcs_media_retry_for_attachment(
        &self,
        message_id: String,
        attachment_index: u32,
    ) -> Result<(), MobileBindingsError> {
        let message_id = message_id
            .parse()
            .map_err(|_| MobileBindingsError::InvalidRequest)?;
        with_client(&self.client, |client| {
            client.demand_rcs_media_retry_for_attachment(message_id, attachment_index)
        })
    }

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
            |client, conversation, generation, record, sender| {
                client.ingest_rcs_receive_observation(
                    conversation,
                    generation,
                    record,
                    sender,
                    RcsObservationProvenance::Live,
                )
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
            |client, conversation, generation, record, sender| {
                client.ingest_rcs_receive_observation(
                    conversation,
                    generation,
                    record,
                    sender,
                    provenance,
                )
            },
        )
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
    pub fn gateway_record_rcs_recovery_progress(
        &self,
        conversation_id: String,
        ownership_generation: u64,
        target_ms: i64,
        cursor: String,
    ) -> Result<String, MobileBindingsError> {
        if cursor.len() > GO_CURSOR_CAP {
            return Err(MobileBindingsError::InvalidRequest);
        }
        with_client(&self.client, |client| {
            client.record_rcs_recovery_progress(
                conversation_id.parse().map_err(|_| {
                    peppy_client_core::Error::InvalidRequest("invalid conversation")
                })?,
                ownership_generation,
                target_ms,
                &cursor,
            )
        })
        .map(recovery_outcome)
    }
    pub fn gateway_complete_rcs_recovery(
        &self,
        conversation_id: String,
        ownership_generation: u64,
        target_ms: i64,
    ) -> Result<String, MobileBindingsError> {
        with_client(&self.client, |client| {
            client.complete_rcs_recovery(
                conversation_id.parse().map_err(|_| {
                    peppy_client_core::Error::InvalidRequest("invalid conversation")
                })?,
                ownership_generation,
                target_ms,
            )
        })
        .map(recovery_outcome)
    }
    pub fn gateway_clear_rcs_recovery_hint(
        &self,
        conversation_id: String,
        ownership_generation: u64,
    ) -> Result<String, MobileBindingsError> {
        with_client(&self.client, |client| {
            client.clear_rcs_recovery_hint(
                conversation_id.parse().map_err(|_| {
                    peppy_client_core::Error::InvalidRequest("invalid conversation")
                })?,
                ownership_generation,
            )
        })
        .map(recovery_outcome)
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
    ) -> Result<NativeRcsProviderRoute, MobileBindingsError> {
        if provider_row_json.len() > GO_RECORD_JSON_CAP {
            return Err(MobileBindingsError::InvalidRequest);
        }
        let row: ProviderRowJson = serde_json::from_str(&provider_row_json)
            .map_err(|_| MobileBindingsError::InvalidRequest)?;
        with_client(&self.client, |client| {
            client.route_rcs_provider_row_result(RcsProviderRow {
                provider_row_id: row.provider_row_id,
                provider_thread_id: row.provider_thread_id,
                provider_type: row.provider_type,
                observed_at_ms: row.observed_at_ms,
                transaction_id: row.transaction_id,
                addresses: row.addresses,
                restricted: row.restricted,
                text_only: row.text_only,
            })
        })
        .map(provider_route_view)
    }

    pub fn gateway_claim_rcs_link_ownership_json(
        &self,
        claim_json: String,
    ) -> Result<NativeRcsLinkOwnershipClaim, MobileBindingsError> {
        if claim_json.len() > GO_RECORD_JSON_CAP {
            return Err(MobileBindingsError::InvalidRequest);
        }
        let claim: LinkOwnershipClaimJson =
            serde_json::from_str(&claim_json).map_err(|_| MobileBindingsError::InvalidRequest)?;
        if claim.conversation_id.len() > GO_ID_CAP || claim.google_conversation_id.len() > GO_ID_CAP
        {
            return Err(MobileBindingsError::InvalidRequest);
        }
        let conversation_id = claim
            .conversation_id
            .parse()
            .map_err(|_| MobileBindingsError::InvalidRequest)?;
        with_client(&self.client, |client| {
            client.claim_rcs_link_ownership(RcsLinkOwnershipClaim {
                conversation_id,
                google_conversation_id: claim.google_conversation_id,
                mapping_revision: claim.mapping_revision,
                provider_floor: claim.provider_floor,
                expected_lifecycle_generation: claim.expected_lifecycle_generation,
                expected_link_generation: claim.expected_link_generation,
                expected_session_generation: claim.expected_session_generation,
                expected_context_revision: claim.expected_context_revision,
            })
        })
        .map(ownership_claim_view)
    }

    pub fn gateway_rcs_cutover_alerts(
        &self,
    ) -> Result<NativeRcsCutoverAlerts, MobileBindingsError> {
        with_client(&self.client, |client| client.rcs_cutover_alerts()).map(|alerts| {
            NativeRcsCutoverAlerts {
                count: alerts.count,
                conversation_ids: alerts
                    .conversation_ids
                    .into_iter()
                    .map(|conversation_id| conversation_id.to_string())
                    .collect(),
            }
        })
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

    pub fn gateway_rcs_link_lifecycle_generation(
        &self,
    ) -> Result<Option<u64>, MobileBindingsError> {
        with_client(&self.client, |client| {
            client.rcs_link_lifecycle_generation()
        })
    }
}
