//! Sanitized UI view models (the Rust side of `src/bridge/bridge.ts`). Only display data is
//! serialized: no credentials, tokens, keys, file keys, raw paths or core records.
use peppy_client_core::{
    AppFilter, AttachmentId, AttachmentInfo, AttachmentState, Client, ComposeDraft,
    ConversationFolder, ConversationFolderReason, DeferredMediaStatus, Direction, Error,
    GatewayRoute, MediaPage, MediaPageAround, Message, MessageId, MirroredNotification,
    SendRefusalReason, SendState, SenderUnblockStatus, Transport,
};
pub use peppy_client_core::{NotificationPreferences, NotificationPreview as Preview};
use serde::{Deserialize, Serialize};
pub mod rcs_receive;
#[cfg(test)]
mod rcs_receive_privacy_tests;
use std::collections::BTreeMap;

pub mod account_access;
pub mod attachment_retention;
pub use attachment_retention::*;
pub mod avatar;
pub mod compose;
pub mod gateways;
pub mod group;
pub use group::*;
pub mod images;
pub mod mms_plan;
pub mod notification_activation;
pub use notification_activation::*;
pub mod notification_dismissal;
pub use notification_dismissal::{NotificationDismissalBlock, NotificationDismissalBlockReason};
pub mod launch_progress;
pub use launch_progress::*;
pub mod quick_reply;
pub mod reactions;
pub use reactions::{
    ConversationReactions, MessageReactionView, OwnReactionRefusal, OwnReactionRequestState,
    OwnReactionRequestView, ReactionActorView, ReactionRouteDto, ReactionSubmitView,
};
pub mod rcs_status;
pub use rcs_status::*;
pub mod search;
pub use search::*;
pub mod search_history;
pub use search_history::*;
pub mod send_recovery;
pub use send_recovery::{SendRecoveryKind, SendRecoveryView, SentAgainView, send_recovery_view};
#[cfg(test)]
mod send_again_flow_tests;
#[cfg(test)]
mod send_again_tests;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub version: &'static str,
    pub mode: &'static str,
    pub writes_paused: bool,
    /// Opaque native device-session invalidation hint, never a credential or acknowledgement scope.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_scope_id: Option<String>,
    pub connection: Connection,
    pub encryption: Encryption,
    pub gateways: Vec<GatewayView>,
    pub conversations: Vec<ConversationView>,
    pub notifications: Vec<MirroredNotification>,
    pub app_filters: Vec<AppFilter>,
    /// Phones that cannot currently record dismissals. Empty while locked.
    pub notification_dismissal_blocks: Vec<NotificationDismissalBlock>,
    pub notification_preferences: NotificationPreferences,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_conversation_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requested_redirect: Option<RequestedRedirect>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub draft: Option<DraftView>,
    pub head: Head,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub desktop: Option<Desktop>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device_role: Option<String>,
    pub pending_count: u64,
    pub quarantine_count: u64,
    /// Display-only: phone address -> resolved contact name/avatar (see `contacts`). Stored
    /// conversation names, addresses and draft recipients are never rewritten.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub contact_resolution: Option<serde_json::Map<String, serde_json::Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub contact_name_conflicts: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub contact_books: Option<Vec<serde_json::Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub contacts_pending_count: Option<u64>,
    /// `{repairRequired, projection?: {state, reason?}}`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub contact_sync: Option<serde_json::Value>,
    /// Active conversation tone presets. Omitted until vault keys are unlocked.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tone_overrides: Option<BTreeMap<String, u8>>,
    /// Durable replay frontiers only. Hosts omit this until their sync lane can provide it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sync_progress: Option<SyncProgress>,
    /// Bounded, read-only RCS display state. Omitted while locked or unsupported.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rcs_status: Option<RcsStatusSnapshot>,
}

#[derive(Clone, Debug, Serialize)]
pub struct RequestedRedirect {
    pub from: String,
    pub to: String,
}

/// Sanitized, exact replay status. Cursors and snapshot counts remain decimal strings so JS never
/// loses precision. `target_high_water` is a transient authenticated server cut, not a total.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncProgress {
    pub phase: SyncProgressPhase,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub update_completed: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub update_total: Option<String>,
    pub received_through: String,
    pub processed_through: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_high_water: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snapshot_received: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snapshot_expected: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snapshot_remaining: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snapshot_total: Option<String>,
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum SyncProgressPhase {
    Updating,
    Receiving,
    Applying,
    WaitingForKeys,
    Repairing,
    Blocked,
    Current,
}

#[cfg(test)]
mod sync_progress_shape_tests {
    use super::*;

    #[test]
    fn sync_progress_serializes_snapshot_total_as_decimal_string() {
        let progress = SyncProgress {
            phase: SyncProgressPhase::Applying,
            update_completed: None,
            update_total: None,
            received_through: "9".into(),
            processed_through: "8".into(),
            target_high_water: None,
            snapshot_received: None,
            snapshot_expected: None,
            snapshot_remaining: Some("4".into()),
            snapshot_total: Some("10".into()),
        };
        let value = serde_json::to_value(progress).unwrap();
        assert_eq!(value["snapshotRemaining"], "4");
        assert_eq!(value["snapshotTotal"], "10");
        let progress = SyncProgress {
            phase: SyncProgressPhase::Current,
            update_completed: None,
            update_total: None,
            received_through: "0".into(),
            processed_through: "0".into(),
            target_high_water: None,
            snapshot_received: None,
            snapshot_expected: None,
            snapshot_remaining: None,
            snapshot_total: None,
        };
        assert!(
            serde_json::to_value(progress)
                .unwrap()
                .get("snapshotTotal")
                .is_none()
        );
        assert_eq!(
            serde_json::to_value(SyncProgressPhase::Repairing).unwrap(),
            "repairing"
        );
        assert_eq!(
            serde_json::to_value(SyncProgressPhase::Blocked).unwrap(),
            "blocked"
        );
    }
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Connection {
    pub state: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_code: Option<&'static str>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Encryption {
    pub state: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile_fingerprint: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Head {
    pub enabled: bool,
    pub capability: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pinned_conversation_ids: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub panel: Option<bool>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Desktop {
    pub tray_available: bool,
    pub start_at_login: bool,
    pub startup_supported: bool,
    pub background: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct GatewayView {
    pub id: String,
    pub name: String,
    pub sim_id: String,
    pub online: bool,
    pub simulated: bool,
    pub supports_sms: bool,
    pub supports_mms: bool,
    /// A truthful presence/capability projection. Missing from legacy cached views means unknown.
    #[serde(default)]
    pub availability: GatewayAvailability,
    /// True only while the gateway reports an active history import.
    #[serde(default)]
    pub setting_up: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "number")]
    pub last_seen_age_seconds: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub presence: Option<GatewayPresence>,
    #[serde(default)]
    pub wake_route: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub mms_content_version: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "number")]
    pub mms_max_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "\"carrier\" | \"fallback\"")]
    pub mms_limit_source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "number")]
    pub mms_max_recipients: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub capability_note: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum GatewayAvailability {
    #[default]
    Unknown,
    Connected,
    Delayed,
    SettingUp,
    Offline,
    NeverConnected,
    NoSmsSim,
    SmsPermissionNeeded,
    SmsUnavailable,
    CapabilitiesUnknown,
    UnsupportedPlatform,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub struct GatewayPresence {
    pub state: GatewayPresenceState,
    #[ts(type = "number | null")]
    pub last_seen_age_seconds: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum GatewayPresenceState {
    Connected,
    Seen,
    Never,
}

#[cfg(test)]
mod gateway_wire_tests {
    use super::*;

    #[test]
    fn gateway_json_is_unchanged() {
        let minimal = GatewayView {
            id: "id".into(),
            name: "name".into(),
            sim_id: "sim".into(),
            online: false,
            simulated: false,
            supports_sms: false,
            supports_mms: false,
            availability: GatewayAvailability::Unknown,
            setting_up: false,
            last_seen_age_seconds: None,
            presence: None,
            wake_route: false,
            mms_content_version: None,
            mms_max_bytes: None,
            mms_limit_source: None,
            mms_max_recipients: None,
            capability_note: None,
        };
        let full = GatewayView {
            id: "id".into(),
            name: "name".into(),
            sim_id: "sim".into(),
            online: true,
            simulated: true,
            supports_sms: true,
            supports_mms: true,
            availability: GatewayAvailability::Connected,
            setting_up: true,
            last_seen_age_seconds: Some(12),
            presence: Some(GatewayPresence {
                state: GatewayPresenceState::Seen,
                last_seen_age_seconds: Some(13),
            }),
            wake_route: true,
            mms_content_version: Some(2),
            mms_max_bytes: Some(1024),
            mms_limit_source: Some("carrier".into()),
            mms_max_recipients: Some(5),
            capability_note: Some("note".into()),
        };
        let cases = [
            (
                minimal,
                r#"{"id":"id","name":"name","simId":"sim","online":false,"simulated":false,"supportsSms":false,"supportsMms":false,"availability":"unknown","settingUp":false,"wakeRoute":false}"#,
            ),
            (
                full,
                r#"{"id":"id","name":"name","simId":"sim","online":true,"simulated":true,"supportsSms":true,"supportsMms":true,"availability":"connected","settingUp":true,"lastSeenAgeSeconds":12,"presence":{"state":"seen","last_seen_age_seconds":13},"wakeRoute":true,"mmsContentVersion":2,"mmsMaxBytes":1024,"mmsLimitSource":"carrier","mmsMaxRecipients":5,"capabilityNote":"note"}"#,
            ),
        ];
        for (view, json) in cases {
            assert_eq!(serde_json::to_string(&view).unwrap(), json);
            assert_eq!(serde_json::from_str::<GatewayView>(json).unwrap(), view);
        }
    }

    #[test]
    fn legacy_gateway_defaults_presence_fields() {
        let json = r#"{"id":"id","name":"name","simId":"sim","online":false,"simulated":false,"supportsSms":false,"supportsMms":false}"#;
        let view: GatewayView = serde_json::from_str(json).unwrap();
        assert_eq!(view.availability, GatewayAvailability::Unknown);
        assert!(!view.setting_up);
        assert!(!view.wake_route);
    }

    #[test]
    fn gateway_presence_missing_age_serializes_as_null() {
        let view = GatewayPresence {
            state: GatewayPresenceState::Never,
            last_seen_age_seconds: None,
        };
        let json = r#"{"state":"never","last_seen_age_seconds":null}"#;
        assert_eq!(serde_json::to_string(&view).unwrap(), json);
        assert_eq!(serde_json::from_str::<GatewayPresence>(json).unwrap(), view);
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationView {
    pub id: String,
    pub name: String,
    pub preview: String,
    pub unread: u64,
    pub messages: Vec<MessageView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub has_older_messages: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub participants: Option<Vec<String>>,
    /// This row is the reusable recipientless composition, not an empty history.
    #[serde(default)]
    pub recipientless_draft: bool,
    /// This row exists only for local compose state and has no established history.
    #[serde(default)]
    pub draft_only: bool,
    pub pinned: bool,
    pub folder: ConversationFolder,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub folder_reason: Option<ConversationFolderReason>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unblock: Option<SenderUnblockStatus>,
    pub can_unblock: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rcs_encryption: Option<bool>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub rcs_history_gap: bool,
    /// Present only for a group conversation (a roster or an RCS group mapping).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group: Option<GroupView>,
    /// Reaction route the conversation offers; absent for draft-only rows.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reaction_support: Option<&'static str>,
    /// Gateway the reaction route uses; the UI reads its presence for the offline gate.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reaction_gateway_id: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryPage {
    pub conversation_id: String,
    pub messages: Vec<MessageView>,
    pub has_more: bool,
}

/// Sender side of a message view: `self` for outgoing, `other` for incoming.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, ts_rs::TS)]
#[serde(rename_all = "lowercase")]
pub enum MessageSender {
    #[serde(rename = "self")]
    SelfSent,
    Other,
}

/// Send or receipt status of a message. Serialized values are the UI contract's `MessageStatus`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, ts_rs::TS)]
#[serde(rename_all = "kebab-case")]
pub enum MessageStatus {
    QueuedLocal,
    ServerAccepted,
    GatewayPersisted,
    Preparing,
    Submitted,
    Sent,
    DeliveryConfirmed,
    FailedBeforeSubmit,
    FailedConfirmed,
    Unknown,
    Read,
}

/// Group refusal or outcome shown on a message.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum GroupHint {
    MembersChanged,
    UpdateRequired,
    Withdrawn,
    CreateUnknown,
    CreateNotRcs,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, ts_rs::TS)]
#[serde(rename_all = "lowercase")]
pub enum MessageTransport {
    Sms,
    Mms,
    Rcs,
}

/// Cancellation state of a send. Serialized as the UI contract's `SendCancellationState`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, ts_rs::TS)]
#[serde(rename_all = "kebab-case")]
#[ts(rename = "SendCancellationState")]
pub enum SendCancellationStateDto {
    None,
    Requested,
    Confirmed,
    TooLate,
    Unknown,
}

/// Attachment state. Serialized as the UI contract's `AttachmentState`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, ts_rs::TS)]
#[serde(rename_all = "lowercase")]
#[ts(rename = "AttachmentState")]
pub enum AttachmentStateDto {
    Pending,
    Uploading,
    Failed,
    Ready,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, ts_rs::TS)]
#[serde(rename_all = "lowercase")]
pub enum AttachmentTransfer {
    Upload,
    Download,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, ts_rs::TS)]
#[serde(rename_all = "lowercase")]
pub enum MediaHold {
    Held,
    Requested,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, ts_rs::TS)]
#[serde(rename_all = "kebab-case")]
pub enum DraftAttachmentState {
    Saving,
    Local,
    Waiting,
    Uploading,
    Uploaded,
    UploadFailed,
    Downloading,
    DownloadFailed,
    Unavailable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum RcsAttachmentState {
    Downloading,
    Stored,
    TooLarge,
    QuotaFull,
    Failed,
    Expired,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum RcsSendStatus {
    Sending,
    Sent,
    NotSent,
    SendFailed,
    OutcomeUnknown,
    MembersChanged,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum MediaPreviewUnavailable {
    ThumbnailUnavailable,
}

#[derive(Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct MessageView {
    pub id: String,
    pub revision: String,
    pub sender: MessageSender,
    /// Actual incoming sender, retained separately from the self/other display category.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub sender_address: Option<String>,
    pub body: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub one_time_code: Option<String>,
    /// Empty only when the provider did not supply a timestamp; never synthesized from arrival.
    pub timestamp: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub status: Option<MessageStatus>,
    /// Gateway-recorded time of `status`, as positive decimal epoch milliseconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub status_at: Option<String>,
    /// Group refusal or outcome the message view shows (see `group_hint`).
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub group_hint: Option<GroupHint>,
    pub attachments: Vec<AttachmentView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub transport: Option<MessageTransport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub subject: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub participants: Option<Vec<String>>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    #[ts(as = "Option<bool>", optional)]
    pub rcs_reported: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub send_cancellation: Option<SendCancellationView>,
    /// Recovery state of a carrier send that may not have reached the carrier (see `send_recovery`).
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub send_recovery: Option<SendRecoveryView>,
    /// This device's explicit "Send again" copy of this message, independent of the recovery state.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub sent_again: Option<SentAgainView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub reactions: Option<Vec<MessageReactionView>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub own_reaction_request: Option<OwnReactionRequestView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub reactable: Option<ReactionRouteDto>,
}

#[derive(Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct SendCancellationView {
    pub state: SendCancellationStateDto,
    pub can_cancel: bool,
    pub requires_gateway_update: bool,
}

#[derive(Clone, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentView {
    pub id: String,
    pub name: String,
    pub media_type: String,
    #[ts(type = "number")]
    pub byte_size: u64,
    pub state: AttachmentStateDto,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub error: Option<String>,
    /// Native-generated, re-encoded PNG thumbnail data URL only (never a path or SVG/HTML).
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub preview_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub transfer: Option<AttachmentTransfer>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub retryable: Option<bool>,
    /// A deferred original is waiting for the gateway's upload policy or an accepted manual sync.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub media_hold: Option<MediaHold>,
    /// Producer metadata links a ready thumbnail independently from the original's transfer state.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub media_meta: Option<Box<MediaMetadataView>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    #[ts(inline)]
    pub remote_state: Option<RemoteAttachmentStateDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub local_available: Option<bool>,
    /// Draft-transfer state, present only for attachments displayed in a draft.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub draft_state: Option<DraftAttachmentState>,
    /// Original position in an RCS message; used to address a retry demand safely.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub index: Option<u32>,
    /// Sanitized incoming RCS media state for a known receive placeholder.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub rcs_state: Option<RcsAttachmentState>,
    /// Sanitized outgoing RCS media state, never an internal ledger state.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub rcs_send_status: Option<RcsSendStatus>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaCursorView {
    pub sort_at_ms: String,
    pub local_order: String,
    pub message_id: String,
    pub attachment_position: u32,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaPageItemView {
    pub message_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sender_address: Option<String>,
    pub timestamp: String,
    pub attachment: AttachmentView,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub media_meta: Option<MediaMetadataView>,
    pub cursor: MediaCursorView,
}
#[derive(Clone, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct MediaMetadataView {
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub width: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub height: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub duration_ms: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub thumbnail: Option<AttachmentView>,
    /// `thumbnail_unavailable` is producer-authenticated v3 completion evidence, not a host guess.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub preview_unavailable: Option<MediaPreviewUnavailable>,
}

pub fn media_metadata_view(meta: peppy_client_core::MediaMetadata) -> MediaMetadataView {
    MediaMetadataView {
        width: meta.width,
        height: meta.height,
        duration_ms: meta.duration_ms.map(|value| value.to_string()),
        thumbnail: meta
            .thumbnail
            .as_ref()
            .map(|item| attachment_view(item, None, None)),
        preview_unavailable: meta
            .thumbnail
            .is_none()
            .then_some(MediaPreviewUnavailable::ThumbnailUnavailable),
    }
}

/// Snapshot/history callers can enrich each visible attachment without relying on attachment order.
pub fn message_attachment_media_metadata_view(
    client: &Client,
    message_id: MessageId,
    attachment_id: AttachmentId,
) -> Result<Option<MediaMetadataView>, Error> {
    client
        .message_attachment_media_metadata(message_id, attachment_id)
        .map(|metadata| metadata.map(media_metadata_view))
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaPageView {
    pub items: Vec<MediaPageItemView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<MediaCursorView>,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaPageAroundView {
    pub target: MediaPageItemView,
    pub newer: Vec<MediaPageItemView>,
    pub older: Vec<MediaPageItemView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub newer_cursor: Option<MediaCursorView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub older_cursor: Option<MediaCursorView>,
}

#[derive(Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct DraftView {
    pub id: String,
    pub conversation_id: String,
    pub text: String,
    pub recipient_ids: Vec<String>,
    pub attachment_ids: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub attachments: Option<Vec<AttachmentView>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub gateway_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub sim_id: Option<String>,
    pub revision: String,
    /// Group name for a draft to two or more members.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub group_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub sync: Option<DraftSyncView>,
}

impl std::fmt::Debug for DraftView {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DraftView")
            .field("id", &self.id)
            .field("conversation_id", &self.conversation_id)
            .field("revision", &self.revision)
            .field(
                "attachment_count",
                &self.attachments.as_ref().map_or(0, Vec::len),
            )
            .finish()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, ts_rs::TS)]
#[serde(rename_all = "kebab-case")]
pub enum DraftSyncStateView {
    LocalOnly,
    SavedLocal,
    Syncing,
    Synced,
    Paused,
    Failed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, ts_rs::TS)]
#[serde(rename_all = "kebab-case")]
pub enum DraftNoticeView {
    RestoredAfterExpiry,
    KeptAfterSent,
    UpdatedFromOtherDevice,
}

#[derive(Debug, PartialEq, Eq, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct DraftSyncView {
    pub state: DraftSyncStateView,
    pub expires_at: Option<String>,
    pub conflicts: Vec<DraftConflictView>,
    pub notice: Option<DraftNoticeView>,
    pub send_requires_connection: bool,
}

#[derive(Debug, PartialEq, Eq, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct DraftConflictView {
    pub branch_id: String,
    pub text: String,
    pub attachment_ids: Vec<String>,
    pub recipient_ids: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub gateway_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub sim_id: Option<String>,
}

#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SendResultView {
    /// True only when the send was durably committed to the local encrypted outbox.
    pub accepted: bool,
    pub status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Stored draft revision after the send (the cleared draft), for the next CAS save.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parts: Option<u32>,
    /// Current members and revision, only for a `members_changed` group refusal.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group: Option<GroupRefusalView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refused_attachment_ids: Option<Vec<String>>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PublicCopyView {
    pub url: String,
    pub expires_in_seconds: u64,
}

pub fn send_state_label(state: SendState) -> MessageStatus {
    match state {
        SendState::QueuedLocal => MessageStatus::QueuedLocal,
        SendState::AcceptedServer => MessageStatus::ServerAccepted,
        SendState::PersistedGateway => MessageStatus::GatewayPersisted,
        SendState::AttemptRecorded => MessageStatus::Preparing,
        SendState::SubmittedToOs => MessageStatus::Submitted,
        SendState::Sent => MessageStatus::Sent,
        SendState::Delivered => MessageStatus::DeliveryConfirmed,
        SendState::FailedBeforeSubmission => MessageStatus::FailedBeforeSubmit,
        SendState::FailedConfirmed => MessageStatus::FailedConfirmed,
        SendState::OutcomeUnknown => MessageStatus::Unknown,
    }
}

pub fn attachment_state_label(state: AttachmentState) -> AttachmentStateDto {
    match state {
        AttachmentState::PendingUpload => AttachmentStateDto::Uploading,
        AttachmentState::Uploaded | AttachmentState::Available => AttachmentStateDto::Ready,
        AttachmentState::PendingDownload => AttachmentStateDto::Pending,
    }
}

pub fn attachment_view(
    info: &AttachmentInfo,
    error: Option<String>,
    preview_url: Option<String>,
) -> AttachmentView {
    let retryable = error.as_ref().map(|_| true);
    AttachmentView {
        id: info.attachment_id.to_string(),
        name: info.display_name.clone(),
        media_type: info.media_type.clone(),
        byte_size: info.plaintext_bytes,
        state: if error.is_some() {
            AttachmentStateDto::Failed
        } else {
            attachment_state_label(info.state)
        },
        error,
        preview_url,
        transfer: match info.state {
            AttachmentState::PendingUpload => Some(AttachmentTransfer::Upload),
            AttachmentState::PendingDownload => Some(AttachmentTransfer::Download),
            AttachmentState::Uploaded | AttachmentState::Available => None,
        },
        retryable,
        media_hold: None,
        media_meta: None,
        remote_state: None,
        local_available: None,
        draft_state: None,
        index: None,
        rcs_state: None,
        rcs_send_status: None,
    }
}

/// Adds the durable deferred-original state without exposing its selection or transfer internals.
pub fn attachment_view_with_media_status(
    info: &AttachmentInfo,
    error: Option<String>,
    preview_url: Option<String>,
    status: Option<DeferredMediaStatus>,
) -> AttachmentView {
    let mut view = attachment_view(info, error, preview_url);
    view.media_hold = match status {
        Some(DeferredMediaStatus::Retryable { reason }) => {
            view.error = Some(reason.to_owned());
            Some(MediaHold::Held)
        }
        Some(DeferredMediaStatus::Held) => Some(MediaHold::Held),
        Some(DeferredMediaStatus::UploadQueued) => Some(MediaHold::Requested),
        Some(DeferredMediaStatus::Available) | None => None,
    };
    view
}

/// A failed snapshot lookup must neither expose an original nor invent a deferred-sync action.
pub fn snapshot_attachment_view(
    info: &AttachmentInfo,
    error: Option<String>,
    status: Result<Option<DeferredMediaStatus>, Error>,
) -> AttachmentView {
    match status {
        Ok(status) => attachment_view_with_media_status(info, error, None, status),
        Err(_) => {
            let mut view = attachment_view(
                info,
                Some("Media availability could not be verified. Refresh to try again.".into()),
                None,
            );
            view.state = AttachmentStateDto::Failed;
            view.retryable = Some(false);
            view
        }
    }
}

/// Converts media pages with the same deferred-original view used by message snapshots.
pub fn media_page_view_with_media_status(
    client: &Client,
    page: MediaPage,
) -> Result<MediaPageView, Error> {
    let items = page
        .items
        .into_iter()
        .map(|item| media_page_item_view_with_media_status(client, item))
        .collect::<Result<Vec<_>, Error>>()?;
    Ok(MediaPageView {
        items,
        next_cursor: page.next_cursor.map(cursor_view),
    })
}

fn media_page_item_view_with_media_status(
    client: &Client,
    item: peppy_client_core::MediaPageItem,
) -> Result<MediaPageItemView, Error> {
    Ok(MediaPageItemView {
        message_id: item.message.record.message_id.to_string(),
        sender_address: (item.message.direction == Direction::Incoming)
            .then_some(item.message.sender_address)
            .flatten(),
        timestamp: positive_ms_to_decimal(item.message.sent_at_ms).unwrap_or_default(),
        attachment: attachment_view_with_media_status(
            &item.attachment,
            None,
            None,
            client.deferred_media_status(item.attachment.attachment_id)?,
        ),
        media_meta: item.media_meta.map(media_metadata_view),
        cursor: cursor_view(item.cursor),
    })
}

/// Converts media-page neighbors with deferred-original status.
pub fn media_page_around_view_with_media_status(
    client: &Client,
    page: MediaPageAround,
) -> Result<MediaPageAroundView, Error> {
    Ok(MediaPageAroundView {
        target: media_page_item_view_with_media_status(client, page.target)?,
        newer: page
            .newer
            .into_iter()
            .map(|item| media_page_item_view_with_media_status(client, item))
            .collect::<Result<Vec<_>, Error>>()?,
        older: page
            .older
            .into_iter()
            .map(|item| media_page_item_view_with_media_status(client, item))
            .collect::<Result<Vec<_>, Error>>()?,
        newer_cursor: page.newer_cursor.map(cursor_view),
        older_cursor: page.older_cursor.map(cursor_view),
    })
}

pub fn media_page_view(page: MediaPage) -> MediaPageView {
    MediaPageView {
        items: page.items.into_iter().map(media_page_item_view).collect(),
        next_cursor: page.next_cursor.map(cursor_view),
    }
}

pub fn media_page_around_view(page: MediaPageAround) -> MediaPageAroundView {
    MediaPageAroundView {
        target: media_page_item_view(page.target),
        newer: page.newer.into_iter().map(media_page_item_view).collect(),
        older: page.older.into_iter().map(media_page_item_view).collect(),
        newer_cursor: page.newer_cursor.map(cursor_view),
        older_cursor: page.older_cursor.map(cursor_view),
    }
}

fn cursor_view(cursor: peppy_client_core::MediaCursor) -> MediaCursorView {
    MediaCursorView {
        sort_at_ms: cursor.sort_at_ms.to_string(),
        local_order: cursor.local_order.to_string(),
        message_id: cursor.message_id.to_string(),
        attachment_position: cursor.attachment_position,
    }
}

fn media_page_item_view(item: peppy_client_core::MediaPageItem) -> MediaPageItemView {
    MediaPageItemView {
        message_id: item.message.record.message_id.to_string(),
        sender_address: (item.message.direction == Direction::Incoming)
            .then_some(item.message.sender_address)
            .flatten(),
        timestamp: positive_ms_to_decimal(item.message.sent_at_ms).unwrap_or_default(),
        attachment: attachment_view(&item.attachment, None, None),
        media_meta: item.media_meta.map(media_metadata_view),
        cursor: cursor_view(item.cursor),
    }
}

/// Display address of the other party.
pub fn counterpart(message: &Message) -> String {
    match message.payload.direction {
        Direction::Incoming => message
            .payload
            .sender_address
            .clone()
            .unwrap_or_else(|| "Unknown sender".into()),
        Direction::Outgoing if message.payload.recipients.is_empty() => "Unknown recipient".into(),
        Direction::Outgoing => message.payload.recipients.join(", "),
    }
}

pub fn message_view(message: &Message, attachments: Vec<AttachmentView>) -> MessageView {
    MessageView {
        id: message.payload.record.message_id.to_string(),
        revision: message.payload.record.source_sequence.0.to_string(),
        sender: if message.payload.direction == Direction::Outgoing {
            MessageSender::SelfSent
        } else {
            MessageSender::Other
        },
        sender_address: (message.payload.direction == Direction::Incoming)
            .then(|| message.payload.sender_address.clone())
            .flatten(),
        body: message.payload.body.clone(),
        one_time_code: (message.payload.direction == Direction::Incoming)
            .then(|| peppy_client_core::one_time_code::detect(&message.payload.body))
            .flatten()
            .map(|code| code.code),
        timestamp: positive_ms_to_decimal(message.payload.sent_at_ms).unwrap_or_default(),
        status: message.send_state.map(send_state_label),
        status_at: positive_ms_to_decimal(message.status_at_ms),
        group_hint: message.send_refusal.and_then(group_hint),
        attachments,
        transport: match message.payload.transport {
            Transport::Sms => Some(MessageTransport::Sms),
            Transport::Mms => Some(MessageTransport::Mms),
            Transport::Rcs => None,
        },
        subject: message.payload.subject.clone(),
        participants: {
            let mut values = message.payload.recipients.clone();
            if let Some(sender) = &message.payload.sender_address
                && !values.contains(sender)
            {
                values.push(sender.clone());
            }
            (!values.is_empty()).then_some(values)
        },
        rcs_reported: false,
        send_cancellation: None,
        send_recovery: None,
        sent_again: None,
        reactions: reactions::reactions_view(&message.reactions),
        own_reaction_request: message
            .own_reaction_request
            .as_ref()
            .map(reactions::own_reaction_request_view),
        reactable: reactions::reactable_wire(message.reactable),
    }
}

/// The message-level hint for a group refusal, one wire value per core reason. The composer
/// shows the same reasons on the send result; the message keeps them so they survive a restart
/// and reach every device.
fn group_hint(reason: SendRefusalReason) -> Option<GroupHint> {
    match reason {
        SendRefusalReason::MembersChanged => Some(GroupHint::MembersChanged),
        SendRefusalReason::UpdateRequired => Some(GroupHint::UpdateRequired),
        SendRefusalReason::Withdrawn => Some(GroupHint::Withdrawn),
        SendRefusalReason::CreateUnconfirmed => Some(GroupHint::CreateUnknown),
        SendRefusalReason::CreateNotRcs => Some(GroupHint::CreateNotRcs),
    }
}

fn positive_ms_to_decimal(value: Option<i64>) -> Option<String> {
    value
        .filter(|value| *value > 0)
        .map(|value| value.to_string())
}

pub fn draft_view(draft: &ComposeDraft) -> DraftView {
    DraftView {
        id: draft.draft_id.to_string(),
        conversation_id: draft.conversation_id.to_string(),
        text: draft.text.clone(),
        recipient_ids: draft.recipients.clone(),
        attachment_ids: draft
            .attachment_ids
            .iter()
            .map(ToString::to_string)
            .collect(),
        gateway_id: draft.route.as_ref().map(|route| match route {
            GatewayRoute::Carrier {
                gateway_device_id, ..
            }
            | GatewayRoute::RcsLink { gateway_device_id } => gateway_device_id.to_string(),
        }),
        sim_id: draft.route.as_ref().and_then(|route| match route {
            GatewayRoute::Carrier {
                subscription_id, ..
            } => Some(subscription_id.clone()),
            GatewayRoute::RcsLink { .. } => None,
        }),
        attachments: None,
        revision: draft.revision.to_string(),
        group_name: draft.group_name.clone(),
        sync: None,
    }
}

pub fn draft_view_with_sync(client: &Client, draft: &ComposeDraft) -> Result<DraftView, Error> {
    let mut view = draft_view(draft);
    let status = client.draft_sync_status(draft.draft_id)?;
    let state = match status.state {
        peppy_client_core::DraftSyncState::LocalOnly => DraftSyncStateView::LocalOnly,
        peppy_client_core::DraftSyncState::SavedLocal => DraftSyncStateView::SavedLocal,
        peppy_client_core::DraftSyncState::Syncing => DraftSyncStateView::Syncing,
        peppy_client_core::DraftSyncState::Synced => DraftSyncStateView::Synced,
        peppy_client_core::DraftSyncState::Paused => DraftSyncStateView::Paused,
        peppy_client_core::DraftSyncState::Failed => DraftSyncStateView::Failed,
    };
    view.sync = Some(DraftSyncView {
        state,
        expires_at: status
            .expires_at_ms
            .and_then(|ms| time::OffsetDateTime::from_unix_timestamp(ms / 1000).ok())
            .and_then(|value| {
                value
                    .format(&time::format_description::well_known::Rfc3339)
                    .ok()
            }),
        conflicts: status
            .conflicts
            .into_iter()
            .map(|conflict| DraftConflictView {
                branch_id: conflict.branch_id.to_string(),
                text: conflict.text,
                attachment_ids: conflict
                    .attachment_ids
                    .into_iter()
                    .map(|id| id.to_string())
                    .collect(),
                recipient_ids: conflict.recipient_ids,
                gateway_id: conflict.route.as_ref().map(|route| match route {
                    GatewayRoute::Carrier {
                        gateway_device_id, ..
                    }
                    | GatewayRoute::RcsLink { gateway_device_id } => gateway_device_id.to_string(),
                }),
                sim_id: conflict.route.as_ref().and_then(|route| match route {
                    GatewayRoute::Carrier {
                        subscription_id, ..
                    } => Some(subscription_id.clone()),
                    GatewayRoute::RcsLink { .. } => None,
                }),
            })
            .collect(),
        notice: status.notice.map(|notice| match notice {
            peppy_client_core::DraftNotice::RestoredAfterExpiry => {
                DraftNoticeView::RestoredAfterExpiry
            }
            peppy_client_core::DraftNotice::KeptAfterSent => DraftNoticeView::KeptAfterSent,
            peppy_client_core::DraftNotice::UpdatedFromOtherDevice => {
                DraftNoticeView::UpdatedFromOtherDevice
            }
        }),
        send_requires_connection: status.send_requires_connection,
    });
    Ok(view)
}

#[cfg(test)]
mod tests {
    use super::*;
    use peppy_client_core::{
        AttachmentId, ConversationId, DeviceId, DraftId, MessageId, MessagePayload, MessageRecord,
        SourceSequence,
    };
    use serde_json::json;

    #[test]
    fn draft_json_goldens() {
        let conflict = DraftConflictView {
            branch_id: "branch".into(),
            text: "other".into(),
            attachment_ids: vec!["attachment".into()],
            recipient_ids: vec!["recipient".into()],
            gateway_id: Some("gateway".into()),
            sim_id: Some("sim".into()),
        };
        let draft = DraftView {
            id: "draft".into(),
            conversation_id: "conversation".into(),
            text: "hello".into(),
            recipient_ids: vec!["recipient".into()],
            attachment_ids: vec!["attachment".into()],
            attachments: Some(vec![AttachmentView {
                id: "attachment".into(),
                name: "photo".into(),
                media_type: "image/png".into(),
                byte_size: 5,
                state: AttachmentStateDto::Ready,
                error: None,
                preview_url: None,
                transfer: None,
                retryable: None,
                media_hold: None,
                media_meta: None,
                remote_state: None,
                local_available: None,
                draft_state: Some(DraftAttachmentState::Local),
                index: None,
                rcs_state: None,
                rcs_send_status: None,
            }]),
            gateway_id: Some("gateway".into()),
            sim_id: Some("sim".into()),
            revision: "42".into(),
            group_name: Some("group".into()),
            sync: Some(DraftSyncView {
                state: DraftSyncStateView::Synced,
                expires_at: Some("2030-01-01T00:00:00Z".into()),
                conflicts: vec![conflict],
                notice: Some(DraftNoticeView::KeptAfterSent),
                send_requires_connection: true,
            }),
        };
        assert_eq!(
            serde_json::to_string(&draft).unwrap(),
            r#"{"id":"draft","conversationId":"conversation","text":"hello","recipientIds":["recipient"],"attachmentIds":["attachment"],"attachments":[{"id":"attachment","name":"photo","mediaType":"image/png","byteSize":5,"state":"ready","draftState":"local"}],"gatewayId":"gateway","simId":"sim","revision":"42","groupName":"group","sync":{"state":"synced","expiresAt":"2030-01-01T00:00:00Z","conflicts":[{"branchId":"branch","text":"other","attachmentIds":["attachment"],"recipientIds":["recipient"],"gatewayId":"gateway","simId":"sim"}],"notice":"kept-after-sent","sendRequiresConnection":true}}"#
        );
        let minimal = DraftView {
            id: "draft".into(),
            conversation_id: "conversation".into(),
            text: String::new(),
            recipient_ids: vec![],
            attachment_ids: vec![],
            attachments: None,
            gateway_id: None,
            sim_id: None,
            revision: "0".into(),
            group_name: None,
            sync: Some(DraftSyncView {
                state: DraftSyncStateView::LocalOnly,
                expires_at: None,
                conflicts: vec![],
                notice: None,
                send_requires_connection: false,
            }),
        };
        assert_eq!(
            serde_json::to_string(&minimal).unwrap(),
            r#"{"id":"draft","conversationId":"conversation","text":"","recipientIds":[],"attachmentIds":[],"revision":"0","sync":{"state":"local-only","expiresAt":null,"conflicts":[],"notice":null,"sendRequiresConnection":false}}"#
        );
        let without_sync = DraftView {
            sync: None,
            ..minimal
        };
        assert_eq!(
            serde_json::to_string(&without_sync).unwrap(),
            r#"{"id":"draft","conversationId":"conversation","text":"","recipientIds":[],"attachmentIds":[],"revision":"0"}"#
        );
        let conflict = DraftConflictView {
            branch_id: "branch".into(),
            text: String::new(),
            attachment_ids: vec![],
            recipient_ids: vec![],
            gateway_id: None,
            sim_id: None,
        };
        assert_eq!(
            serde_json::to_string(&conflict).unwrap(),
            r#"{"branchId":"branch","text":"","attachmentIds":[],"recipientIds":[]}"#
        );
    }

    #[test]
    fn draft_sync_string_goldens() {
        for (state, expected) in [
            (DraftSyncStateView::LocalOnly, "local-only"),
            (DraftSyncStateView::SavedLocal, "saved-local"),
            (DraftSyncStateView::Syncing, "syncing"),
            (DraftSyncStateView::Synced, "synced"),
            (DraftSyncStateView::Paused, "paused"),
            (DraftSyncStateView::Failed, "failed"),
        ] {
            let sync = DraftSyncView {
                state,
                expires_at: None,
                conflicts: vec![],
                notice: None,
                send_requires_connection: false,
            };
            assert_eq!(
                serde_json::to_string(&sync).unwrap(),
                format!(
                    r#"{{"state":"{expected}","expiresAt":null,"conflicts":[],"notice":null,"sendRequiresConnection":false}}"#
                )
            );
        }
        for (notice, expected) in [
            (
                DraftNoticeView::RestoredAfterExpiry,
                "restored-after-expiry",
            ),
            (DraftNoticeView::KeptAfterSent, "kept-after-sent"),
            (
                DraftNoticeView::UpdatedFromOtherDevice,
                "updated-from-other-device",
            ),
        ] {
            let sync = DraftSyncView {
                state: DraftSyncStateView::Synced,
                expires_at: None,
                conflicts: vec![],
                notice: Some(notice),
                send_requires_connection: false,
            };
            assert_eq!(
                serde_json::to_string(&sync).unwrap(),
                format!(
                    r#"{{"state":"synced","expiresAt":null,"conflicts":[],"notice":"{expected}","sendRequiresConnection":false}}"#
                )
            );
        }
    }

    #[test]
    fn draft_conversion_keeps_camel_case_omissions_and_decimal_revision() {
        let draft = ComposeDraft {
            draft_id: DraftId::new(),
            conversation_id: ConversationId::new(),
            text: "Hello".into(),
            recipients: vec!["+15555550123".into()],
            attachment_ids: vec![AttachmentId::new()],
            route: None,
            revision: 18_446_744_073_709_551_615,
            group_name: None,
        };

        let value = serde_json::to_value(draft_view(&draft)).unwrap();

        assert_eq!(value["text"], "Hello");
        assert_eq!(value["recipientIds"], json!(["+15555550123"]));
        assert_eq!(value["revision"], "18446744073709551615");
        assert!(value.get("conversation_id").is_none());
        assert!(value.get("gatewayId").is_none());
        assert!(value.get("simId").is_none());
    }

    #[test]
    fn conversation_view_serializes_its_durable_conversation_state() {
        let view = ConversationView {
            id: "conversation".into(),
            name: "Conversation".into(),
            preview: "Preview".into(),
            unread: 0,
            messages: vec![],
            has_older_messages: None,
            participants: None,
            recipientless_draft: false,
            draft_only: false,
            pinned: true,
            folder: ConversationFolder::Archived,
            folder_reason: Some(ConversationFolderReason::Marked),
            unblock: Some(SenderUnblockStatus::Failed),
            can_unblock: true,
            rcs_encryption: Some(true),
            rcs_history_gap: true,
            group: None,
            reaction_support: Some("native"),
            reaction_gateway_id: Some("gateway".into()),
        };

        let value = serde_json::to_value(view).unwrap();
        assert!(value.get("group").is_none());
        assert_eq!(value["pinned"], true);
        assert_eq!(value["folder"], "archived");
        assert_eq!(value["folderReason"], "marked");
        assert_eq!(value["unblock"], "failed");
        assert_eq!(value["canUnblock"], true);
        assert_eq!(value["rcsEncryption"], true);
        assert_eq!(value["rcsHistoryGap"], true);
        let serialized = value.to_string();
        for forbidden in ["google", "cursor", "detector", "provider", "tmp"] {
            assert!(!serialized.contains(forbidden));
        }
    }

    #[test]
    fn media_page_projections_preserve_sender_timestamp_and_order() {
        use peppy_client_core::{ClientConfig, DatabaseKey, MediaCursor, MediaPageItem};
        let dir = tempfile::tempdir().unwrap();
        let client = Client::open(
            ClientConfig {
                database_path: dir.path().join("media.db"),
                vault_id: peppy_client_core::VaultId::new(),
                device_id: DeviceId::new(),
            },
            DatabaseKey::new(&[9; 32]).unwrap(),
        )
        .unwrap();
        let make_item = |direction, timestamp| {
            let mut message = message_with_body(direction, "image").payload;
            message.sent_at_ms = timestamp;
            message.sender_address = Some("+15555550100".into());
            MediaPageItem {
                cursor: MediaCursor {
                    sort_at_ms: 3,
                    local_order: 4,
                    message_id: message.record.message_id,
                    attachment_position: 0,
                },
                message,
                attachment: AttachmentInfo {
                    attachment_id: AttachmentId::new(),
                    media_type: "image/png".into(),
                    display_name: "photo.png".into(),
                    plaintext_bytes: 42,
                    ciphertext_bytes: 99,
                    ciphertext_sha256: "private".into(),
                    state: AttachmentState::PendingDownload,
                },
                media_meta: None,
            }
        };
        let incoming = make_item(Direction::Incoming, Some(12));
        let outgoing = make_item(Direction::Outgoing, None);
        let zero = make_item(Direction::Incoming, Some(0));
        let negative = make_item(Direction::Outgoing, Some(-1));
        let page = MediaPage {
            items: vec![
                incoming.clone(),
                outgoing.clone(),
                zero.clone(),
                negative.clone(),
            ],
            next_cursor: Some(incoming.cursor.clone()),
        };
        let normal = serde_json::to_value(media_page_view(page.clone())).unwrap();
        let status =
            serde_json::to_value(media_page_view_with_media_status(&client, page).unwrap())
                .unwrap();
        assert_eq!(normal, status);
        assert_eq!(normal["items"][0]["senderAddress"], "+15555550100");
        assert_eq!(normal["items"][0]["timestamp"], "12");
        for index in 1..4 {
            assert_eq!(normal["items"][index]["timestamp"], "");
        }
        assert!(normal["items"][1].get("senderAddress").is_none());
        assert!(normal["items"][0].get("mediaMeta").is_none());
        let around = MediaPageAround {
            target: incoming,
            newer: vec![outgoing, zero],
            older: vec![negative],
            newer_cursor: None,
            older_cursor: None,
        };
        let normal_around = serde_json::to_value(media_page_around_view(around.clone())).unwrap();
        let status_around = serde_json::to_value(
            media_page_around_view_with_media_status(&client, around).unwrap(),
        )
        .unwrap();
        assert_eq!(normal_around, status_around);
        assert_eq!(normal_around["target"], normal["items"][0]);
        assert_eq!(normal_around["newer"][0], normal["items"][1]);
        assert_eq!(normal_around["newer"][1], normal["items"][2]);
        assert_eq!(normal_around["older"][0], normal["items"][3]);

        let db = rusqlite::Connection::open(dir.path().join("media.db")).unwrap();
        db.execute_batch(
            "PRAGMA key = \"x'0909090909090909090909090909090909090909090909090909090909090909'\";\
             DROP TABLE mms_deferred_originals;",
        )
        .unwrap();
        let item = make_item(Direction::Incoming, None);
        assert!(matches!(
            media_page_view_with_media_status(
                &client,
                MediaPage {
                    items: vec![item.clone()],
                    next_cursor: None,
                }
            ),
            Err(Error::Database)
        ));
        assert!(matches!(
            media_page_around_view_with_media_status(
                &client,
                MediaPageAround {
                    target: item,
                    newer: vec![],
                    older: vec![],
                    newer_cursor: None,
                    older_cursor: None,
                }
            ),
            Err(Error::Database)
        ));
    }

    #[test]
    fn attachment_conversion_exposes_only_sanitized_display_fields() {
        let attachment = AttachmentInfo {
            attachment_id: AttachmentId::new(),
            media_type: "image/png".into(),
            display_name: "photo.png".into(),
            plaintext_bytes: 42,
            ciphertext_bytes: 99,
            ciphertext_sha256: "sensitive-ciphertext-digest".into(),
            state: AttachmentState::PendingUpload,
        };

        let value = serde_json::to_value(attachment_view(&attachment, None, None)).unwrap();

        assert_eq!(value["mediaType"], "image/png");
        assert_eq!(value["byteSize"], 42);
        assert_eq!(value["state"], "uploading");
        assert_eq!(value["transfer"], "upload");
        for forbidden in ["ciphertextBytes", "ciphertextSha256", "path", "key"] {
            assert!(value.get(forbidden).is_none(), "serialized {forbidden}");
        }
    }

    #[test]
    fn attachment_availability_serializes_typed_optional_camel_case_fields() {
        let attachment = AttachmentInfo {
            attachment_id: AttachmentId::new(),
            media_type: "image/png".into(),
            display_name: "photo.png".into(),
            plaintext_bytes: 42,
            ciphertext_bytes: 99,
            ciphertext_sha256: "sensitive-ciphertext-digest".into(),
            state: AttachmentState::Available,
        };
        let mut view = attachment_view(&attachment, None, None);
        let absent = serde_json::to_value(&view).unwrap();
        assert!(absent.get("remoteState").is_none());
        assert!(absent.get("localAvailable").is_none());

        view.remote_state = Some(RemoteAttachmentStateDto::RemovedByStoragePolicy);
        view.local_available = Some(true);
        let available = serde_json::to_value(view).unwrap();
        assert_eq!(available["remoteState"], "removed_by_storage_policy");
        assert_eq!(available["localAvailable"], true);
    }

    #[test]
    fn attachment_conversion_exposes_only_sanitized_deferred_hold_states() {
        let attachment = AttachmentInfo {
            attachment_id: AttachmentId::new(),
            media_type: "image/png".into(),
            display_name: "photo.png".into(),
            plaintext_bytes: 42,
            ciphertext_bytes: 99,
            ciphertext_sha256: "sensitive-ciphertext-digest".into(),
            state: AttachmentState::PendingUpload,
        };

        let held = serde_json::to_value(attachment_view_with_media_status(
            &attachment,
            None,
            None,
            Some(DeferredMediaStatus::Held),
        ))
        .unwrap();
        let requested = serde_json::to_value(attachment_view_with_media_status(
            &attachment,
            None,
            None,
            Some(DeferredMediaStatus::UploadQueued),
        ))
        .unwrap();
        let available = serde_json::to_value(attachment_view_with_media_status(
            &attachment,
            None,
            None,
            Some(DeferredMediaStatus::Available),
        ))
        .unwrap();

        assert_eq!(held["mediaHold"], "held");
        assert_eq!(requested["mediaHold"], "requested");
        assert!(available.get("mediaHold").is_none());
        assert!(held.get("selection").is_none());
        let failed_lookup = serde_json::to_value(snapshot_attachment_view(
            &attachment,
            None,
            Err(Error::Database),
        ))
        .unwrap();
        assert_eq!(failed_lookup["state"], "failed");
        assert_eq!(failed_lookup["retryable"], false);
        assert!(
            failed_lookup.get("mediaHold").is_none(),
            "a failed lookup must not offer an unknown deferred-media request"
        );
        assert!(failed_lookup.get("previewUrl").is_none());
    }

    #[test]
    fn status_labels_remain_bridge_compatible() {
        assert_eq!(
            send_state_label(SendState::QueuedLocal),
            MessageStatus::QueuedLocal
        );
        assert_eq!(
            send_state_label(SendState::Delivered),
            MessageStatus::DeliveryConfirmed
        );
        assert_eq!(
            send_state_label(SendState::OutcomeUnknown),
            MessageStatus::Unknown
        );
        assert_eq!(
            attachment_state_label(AttachmentState::PendingDownload),
            AttachmentStateDto::Pending
        );
        assert_eq!(
            attachment_state_label(AttachmentState::Available),
            AttachmentStateDto::Ready
        );
    }

    #[test]
    fn group_refusals_map_to_the_message_hint_the_composer_reads() {
        let mut message = message_with_body(Direction::Outgoing, "hello");
        message.send_refusal = Some(SendRefusalReason::MembersChanged);
        assert_eq!(
            message_view(&message, vec![]).group_hint,
            Some(GroupHint::MembersChanged)
        );
        message.send_refusal = Some(SendRefusalReason::CreateUnconfirmed);
        assert_eq!(
            message_view(&message, vec![]).group_hint,
            Some(GroupHint::CreateUnknown)
        );
        message.send_refusal = Some(SendRefusalReason::UpdateRequired);
        assert_eq!(
            message_view(&message, vec![]).group_hint,
            Some(GroupHint::UpdateRequired)
        );
        message.send_refusal = Some(SendRefusalReason::Withdrawn);
        assert_eq!(
            message_view(&message, vec![]).group_hint,
            Some(GroupHint::Withdrawn)
        );
        message.send_refusal = Some(SendRefusalReason::CreateNotRcs);
        assert_eq!(
            message_view(&message, vec![]).group_hint,
            Some(GroupHint::CreateNotRcs)
        );
        message.send_refusal = None;
        let json = serde_json::to_value(message_view(&message, vec![])).unwrap();
        assert!(json.get("groupHint").is_none());
    }

    #[test]
    fn message_status_time_serializes_as_optional_decimal_milliseconds() {
        let message = Message {
            payload: MessagePayload {
                record: MessageRecord {
                    message_id: MessageId::new(),
                    conversation_id: ConversationId::new(),
                    source_sequence: SourceSequence(1),
                    attachments: vec![],
                },
                source_device_id: DeviceId::new(),
                provider_message_id: None,
                sender_address: None,
                recipients: vec![],
                subject: None,
                body: "Hello".into(),
                transport: Transport::Sms,
                direction: Direction::Outgoing,
                rcs_capture: false,
                imported: false,
                sent_at_ms: Some(2),
                ordered_at_ms: None,
                mms_context: None,
            },
            seen: true,
            send_state: Some(SendState::Sent),
            status_at_ms: Some(1_725_363_180_000),
            send_refusal: None,
            target_gateway_id: None,
            send_cancel_requested: false,
            sent_again_message_id: None,
            send_route: None,
            reactions: Vec::new(),
            own_reaction_request: None,
            reactable: None,
        };

        let view = message_view(&message, vec![]);
        let with_status_time = serde_json::to_value(&view).unwrap();
        assert_eq!(with_status_time["statusAt"], "1725363180000");

        let without_status_time = MessageView {
            status_at: None,
            ..view
        };
        assert!(
            serde_json::to_value(without_status_time)
                .unwrap()
                .get("statusAt")
                .is_none()
        );
    }

    fn message_with_body(direction: Direction, body: &str) -> Message {
        Message {
            payload: MessagePayload {
                record: MessageRecord {
                    message_id: MessageId::new(),
                    conversation_id: ConversationId::new(),
                    source_sequence: SourceSequence(1),
                    attachments: vec![],
                },
                source_device_id: DeviceId::new(),
                provider_message_id: None,
                sender_address: None,
                recipients: vec![],
                subject: None,
                body: body.into(),
                transport: Transport::Sms,
                direction,
                rcs_capture: false,
                imported: false,
                sent_at_ms: Some(2),
                ordered_at_ms: None,
                mms_context: None,
            },
            seen: false,
            send_state: None,
            status_at_ms: None,
            send_refusal: None,
            target_gateway_id: None,
            send_cancel_requested: false,
            sent_again_message_id: None,
            send_route: None,
            reactions: Vec::new(),
            own_reaction_request: None,
            reactable: None,
        }
    }

    #[test]
    fn one_time_code_is_offered_only_for_incoming_messages() {
        let body = "Your verification code is 482913.";
        let incoming = message_view(&message_with_body(Direction::Incoming, body), vec![]);
        assert_eq!(incoming.one_time_code.as_deref(), Some("482913"));
        assert_eq!(
            serde_json::to_value(&incoming).unwrap()["oneTimeCode"],
            "482913"
        );

        let outgoing = message_view(&message_with_body(Direction::Outgoing, body), vec![]);
        assert_eq!(outgoing.one_time_code, None);
        assert!(
            serde_json::to_value(&outgoing)
                .unwrap()
                .get("oneTimeCode")
                .is_none()
        );
    }

    #[test]
    fn notification_preferences_match_the_existing_json_contract() {
        assert_eq!(
            serde_json::to_value(NotificationPreferences::default()).unwrap(),
            json!({
                "messageBanners": true,
                "mirroredBanners": true,
                "preview": "full",
                "muteWithHeads": true,
                "showSource": false,
            })
        );
    }

    #[test]
    fn legacy_notification_preferences_default_mute_with_heads_on() {
        let parsed: NotificationPreferences = serde_json::from_value(json!({
            "messageBanners": true,
            "mirroredBanners": false,
            "preview": "hidden",
        }))
        .unwrap();

        assert!(parsed.mute_with_heads);
        assert!(!parsed.show_source);
        assert!(!parsed.mirrored_banners);
        assert_eq!(parsed.preview, Preview::Hidden);
    }

    #[test]
    fn notification_preferences_round_trip_mute_with_heads() {
        let preferences = NotificationPreferences {
            mute_with_heads: false,
            ..NotificationPreferences::default()
        };
        let value = serde_json::to_value(&preferences).unwrap();

        assert_eq!(value["muteWithHeads"], json!(false));
        assert_eq!(
            serde_json::from_value::<NotificationPreferences>(value).unwrap(),
            preferences
        );
    }

    #[test]
    fn legacy_gateway_cache_defaults_new_presence_fields_to_unknown() {
        let gateway: GatewayView = serde_json::from_value(json!({
            "id": "gateway", "name": "Phone", "simId": "sim", "online": true,
            "simulated": false, "supportsSms": true, "supportsMms": false,
        }))
        .unwrap();

        assert_eq!(gateway.availability, GatewayAvailability::Unknown);
        assert!(!gateway.wake_route);
        assert!(gateway.presence.is_none());
        assert_eq!(
            serde_json::to_value(GatewayPresence {
                state: GatewayPresenceState::Seen,
                last_seen_age_seconds: None,
            })
            .unwrap(),
            json!({"state": "seen", "last_seen_age_seconds": null})
        );
    }
}

pub mod contact_error;
pub mod contacts;
pub use contact_error::{ContactError, ContactResult};

#[cfg(test)]
mod message_wire_tests {
    use super::*;
    use serde_json::{Value, json};

    fn wire<T: Serialize>(value: &T) -> Value {
        serde_json::to_value(value).unwrap()
    }

    fn with_field(mut value: Value, key: &str, field: Value) -> Value {
        value[key] = field;
        value
    }

    fn minimal_attachment_json() -> Value {
        json!({
            "id": "attachment-1",
            "name": "photo.jpg",
            "mediaType": "image/jpeg",
            "byteSize": 42,
            "state": "pending"
        })
    }

    fn attachment(state: AttachmentStateDto) -> AttachmentView {
        AttachmentView {
            id: "attachment-1".into(),
            name: "photo.jpg".into(),
            media_type: "image/jpeg".into(),
            byte_size: 42,
            state,
            error: None,
            preview_url: None,
            transfer: None,
            retryable: None,
            media_hold: None,
            media_meta: None,
            remote_state: None,
            local_available: None,
            draft_state: None,
            index: None,
            rcs_state: None,
            rcs_send_status: None,
        }
    }

    fn minimal_message_json() -> Value {
        json!({
            "id": "message-1",
            "revision": "7",
            "sender": "other",
            "body": "",
            "timestamp": "",
            "attachments": []
        })
    }

    fn message(sender: MessageSender) -> MessageView {
        MessageView {
            id: "message-1".into(),
            revision: "7".into(),
            sender,
            sender_address: None,
            body: String::new(),
            one_time_code: None,
            timestamp: String::new(),
            status: None,
            status_at: None,
            group_hint: None,
            attachments: vec![],
            transport: None,
            subject: None,
            participants: None,
            rcs_reported: false,
            send_cancellation: None,
            send_recovery: None,
            sent_again: None,
            reactions: None,
            own_reaction_request: None,
            reactable: None,
        }
    }

    #[test]
    fn attachment_json_is_unchanged() {
        assert_eq!(
            wire(&attachment(AttachmentStateDto::Pending)),
            minimal_attachment_json()
        );

        let full = AttachmentView {
            error: Some("Upload failed".into()),
            preview_url: Some("data:image/png;base64,AAAA".into()),
            transfer: Some(AttachmentTransfer::Upload),
            retryable: Some(true),
            media_hold: Some(MediaHold::Held),
            media_meta: Some(Box::new(MediaMetadataView {
                width: Some(640),
                height: Some(480),
                duration_ms: Some("1500".into()),
                thumbnail: Some(attachment(AttachmentStateDto::Ready)),
                preview_unavailable: None,
            })),
            remote_state: Some(RemoteAttachmentStateDto::RemovedByStoragePolicy),
            local_available: Some(false),
            draft_state: Some(DraftAttachmentState::UploadFailed),
            index: Some(2),
            rcs_state: Some(RcsAttachmentState::TooLarge),
            rcs_send_status: Some(RcsSendStatus::SendFailed),
            ..attachment(AttachmentStateDto::Uploading)
        };
        assert_eq!(
            wire(&full),
            json!({
                "id": "attachment-1",
                "name": "photo.jpg",
                "mediaType": "image/jpeg",
                "byteSize": 42,
                "state": "uploading",
                "error": "Upload failed",
                "previewUrl": "data:image/png;base64,AAAA",
                "transfer": "upload",
                "retryable": true,
                "mediaHold": "held",
                "mediaMeta": {
                    "width": 640,
                    "height": 480,
                    "durationMs": "1500",
                    "thumbnail": {
                        "id": "attachment-1",
                        "name": "photo.jpg",
                        "mediaType": "image/jpeg",
                        "byteSize": 42,
                        "state": "ready"
                    }
                },
                "remoteState": "removed_by_storage_policy",
                "localAvailable": false,
                "draftState": "upload-failed",
                "index": 2,
                "rcsState": "too_large",
                "rcsSendStatus": "send_failed"
            })
        );
    }

    #[test]
    fn attachment_union_values_are_unchanged() {
        for (state, wire_value) in [
            (AttachmentStateDto::Pending, "pending"),
            (AttachmentStateDto::Uploading, "uploading"),
            (AttachmentStateDto::Failed, "failed"),
            (AttachmentStateDto::Ready, "ready"),
        ] {
            assert_eq!(
                wire(&attachment(state)),
                with_field(minimal_attachment_json(), "state", json!(wire_value))
            );
        }
        for (transfer, wire_value) in [
            (AttachmentTransfer::Upload, "upload"),
            (AttachmentTransfer::Download, "download"),
        ] {
            let view = AttachmentView {
                transfer: Some(transfer),
                ..attachment(AttachmentStateDto::Pending)
            };
            assert_eq!(
                wire(&view),
                with_field(minimal_attachment_json(), "transfer", json!(wire_value))
            );
        }
        for (hold, wire_value) in [
            (MediaHold::Held, "held"),
            (MediaHold::Requested, "requested"),
        ] {
            let view = AttachmentView {
                media_hold: Some(hold),
                ..attachment(AttachmentStateDto::Pending)
            };
            assert_eq!(
                wire(&view),
                with_field(minimal_attachment_json(), "mediaHold", json!(wire_value))
            );
        }
        for (draft_state, wire_value) in [
            (DraftAttachmentState::Saving, "saving"),
            (DraftAttachmentState::Local, "local"),
            (DraftAttachmentState::Waiting, "waiting"),
            (DraftAttachmentState::Uploading, "uploading"),
            (DraftAttachmentState::Uploaded, "uploaded"),
            (DraftAttachmentState::UploadFailed, "upload-failed"),
            (DraftAttachmentState::Downloading, "downloading"),
            (DraftAttachmentState::DownloadFailed, "download-failed"),
            (DraftAttachmentState::Unavailable, "unavailable"),
        ] {
            let view = AttachmentView {
                draft_state: Some(draft_state),
                ..attachment(AttachmentStateDto::Pending)
            };
            assert_eq!(
                wire(&view),
                with_field(minimal_attachment_json(), "draftState", json!(wire_value))
            );
        }
        for (rcs_state, wire_value) in [
            (RcsAttachmentState::Downloading, "downloading"),
            (RcsAttachmentState::Stored, "stored"),
            (RcsAttachmentState::TooLarge, "too_large"),
            (RcsAttachmentState::QuotaFull, "quota_full"),
            (RcsAttachmentState::Failed, "failed"),
            (RcsAttachmentState::Expired, "expired"),
        ] {
            let view = AttachmentView {
                rcs_state: Some(rcs_state),
                ..attachment(AttachmentStateDto::Pending)
            };
            assert_eq!(
                wire(&view),
                with_field(minimal_attachment_json(), "rcsState", json!(wire_value))
            );
        }
        for (rcs_send_status, wire_value) in [
            (RcsSendStatus::Sending, "sending"),
            (RcsSendStatus::Sent, "sent"),
            (RcsSendStatus::NotSent, "not_sent"),
            (RcsSendStatus::SendFailed, "send_failed"),
            (RcsSendStatus::OutcomeUnknown, "outcome_unknown"),
            (RcsSendStatus::MembersChanged, "members_changed"),
        ] {
            let view = AttachmentView {
                rcs_send_status: Some(rcs_send_status),
                ..attachment(AttachmentStateDto::Pending)
            };
            assert_eq!(
                wire(&view),
                with_field(
                    minimal_attachment_json(),
                    "rcsSendStatus",
                    json!(wire_value)
                )
            );
        }
        for (remote, wire_value) in [
            (RemoteAttachmentStateDto::Available, "available"),
            (
                RemoteAttachmentStateDto::RemovedByStoragePolicy,
                "removed_by_storage_policy",
            ),
        ] {
            let view = AttachmentView {
                remote_state: Some(remote),
                ..attachment(AttachmentStateDto::Pending)
            };
            assert_eq!(
                wire(&view),
                with_field(minimal_attachment_json(), "remoteState", json!(wire_value))
            );
        }
    }

    #[test]
    fn media_metadata_json_is_unchanged() {
        assert_eq!(
            wire(&MediaMetadataView {
                width: None,
                height: None,
                duration_ms: None,
                thumbnail: None,
                preview_unavailable: None,
            }),
            json!({})
        );
        assert_eq!(
            wire(&MediaMetadataView {
                width: None,
                height: None,
                duration_ms: None,
                thumbnail: None,
                preview_unavailable: Some(MediaPreviewUnavailable::ThumbnailUnavailable),
            }),
            json!({"previewUnavailable": "thumbnail_unavailable"})
        );
    }

    #[test]
    fn send_cancellation_json_is_unchanged() {
        for (state, wire_value) in [
            (SendCancellationStateDto::None, "none"),
            (SendCancellationStateDto::Requested, "requested"),
            (SendCancellationStateDto::Confirmed, "confirmed"),
            (SendCancellationStateDto::TooLate, "too-late"),
            (SendCancellationStateDto::Unknown, "unknown"),
        ] {
            let view = SendCancellationView {
                state,
                can_cancel: true,
                requires_gateway_update: false,
            };
            assert_eq!(
                wire(&view),
                json!({"state": wire_value, "canCancel": true, "requiresGatewayUpdate": false})
            );
        }
    }

    #[test]
    fn send_recovery_json_is_unchanged() {
        for (kind, wire_value) in [
            (SendRecoveryKind::NotSent, "not-sent"),
            (SendRecoveryKind::UnconfirmedRemoved, "unconfirmed-removed"),
            (
                SendRecoveryKind::UnconfirmedRemovedCancel,
                "unconfirmed-removed-cancel",
            ),
        ] {
            let view = SendRecoveryView {
                kind,
                can_send_again: false,
                sent_again: None,
            };
            assert_eq!(
                wire(&view),
                json!({"kind": wire_value, "canSendAgain": false})
            );
        }
        let copy = SendRecoveryView {
            kind: SendRecoveryKind::UnconfirmedRemoved,
            can_send_again: false,
            sent_again: Some(SentAgainView {
                message_id: "copy-1".into(),
            }),
        };
        assert_eq!(
            wire(&copy),
            json!({
                "kind": "unconfirmed-removed",
                "canSendAgain": false,
                "sentAgain": {"messageId": "copy-1"}
            })
        );
        assert_eq!(
            wire(&SentAgainView {
                message_id: "copy-1".into()
            }),
            json!({"messageId": "copy-1"})
        );
    }

    #[test]
    fn reaction_json_is_unchanged() {
        assert_eq!(
            wire(&MessageReactionView {
                emoji: "👍".into(),
                actors: vec![
                    ReactionActorView {
                        is_self: true,
                        address: None,
                    },
                    ReactionActorView {
                        is_self: false,
                        address: Some("+15555550101".into()),
                    },
                ],
            }),
            json!({
                "emoji": "👍",
                "actors": [
                    {"self": true},
                    {"self": false, "address": "+15555550101"}
                ]
            })
        );
        assert_eq!(
            wire(&MessageReactionView {
                emoji: "❤️".into(),
                actors: vec![],
            }),
            json!({"emoji": "❤️", "actors": []})
        );
    }

    #[test]
    fn own_reaction_request_json_is_unchanged() {
        assert_eq!(
            wire(&OwnReactionRequestView {
                request_id: "req-1".into(),
                emoji: Some("👍".into()),
                previous_emoji: None,
                state: OwnReactionRequestState::Sending,
                refusal: None,
            }),
            json!({"requestId": "req-1", "emoji": "👍", "state": "sending"})
        );
        assert_eq!(
            wire(&OwnReactionRequestView {
                request_id: "req-1".into(),
                emoji: None,
                previous_emoji: Some("❤️".into()),
                state: OwnReactionRequestState::NotSent,
                refusal: Some(OwnReactionRefusal::InvalidEmoji),
            }),
            json!({
                "requestId": "req-1",
                "emoji": null,
                "previousEmoji": "❤️",
                "state": "not_sent",
                "refusal": "invalid_emoji"
            })
        );
        for (state, wire_value) in [
            (OwnReactionRequestState::Sending, "sending"),
            (OwnReactionRequestState::NotSent, "not_sent"),
            (OwnReactionRequestState::Unknown, "unknown"),
        ] {
            let view = OwnReactionRequestView {
                request_id: "req-1".into(),
                emoji: Some("👍".into()),
                previous_emoji: None,
                state,
                refusal: None,
            };
            assert_eq!(
                wire(&view),
                json!({"requestId": "req-1", "emoji": "👍", "state": wire_value})
            );
        }
        for (refusal, wire_value) in [
            (OwnReactionRefusal::Expired, "expired"),
            (
                OwnReactionRefusal::MessageUnavailable,
                "message_unavailable",
            ),
            (OwnReactionRefusal::NotRcs, "not_rcs"),
            (OwnReactionRefusal::LinkUnavailable, "link_unavailable"),
            (OwnReactionRefusal::Rejected, "rejected"),
            (OwnReactionRefusal::Busy, "busy"),
            (OwnReactionRefusal::InvalidEmoji, "invalid_emoji"),
        ] {
            let view = OwnReactionRequestView {
                request_id: "req-1".into(),
                emoji: Some("👍".into()),
                previous_emoji: None,
                state: OwnReactionRequestState::Sending,
                refusal: Some(refusal),
            };
            assert_eq!(
                wire(&view),
                json!({"requestId": "req-1", "emoji": "👍", "state": "sending", "refusal": wire_value})
            );
        }
    }

    #[test]
    fn message_json_is_unchanged() {
        assert_eq!(wire(&message(MessageSender::Other)), minimal_message_json());
        assert!(
            wire(&message(MessageSender::Other))
                .get("rcsReported")
                .is_none()
        );
        assert_eq!(
            wire(&message(MessageSender::SelfSent)),
            with_field(minimal_message_json(), "sender", json!("self"))
        );

        let full = MessageView {
            sender_address: Some("+15555550100".into()),
            one_time_code: Some("123456".into()),
            status: Some(MessageStatus::Read),
            status_at: Some("1700000000001".into()),
            group_hint: Some(GroupHint::MembersChanged),
            attachments: vec![attachment(AttachmentStateDto::Ready)],
            transport: Some(MessageTransport::Rcs),
            subject: Some("Subject".into()),
            participants: Some(vec!["+15555550100".into()]),
            rcs_reported: true,
            send_cancellation: Some(SendCancellationView {
                state: SendCancellationStateDto::TooLate,
                can_cancel: false,
                requires_gateway_update: true,
            }),
            send_recovery: Some(SendRecoveryView {
                kind: SendRecoveryKind::UnconfirmedRemovedCancel,
                can_send_again: false,
                sent_again: Some(SentAgainView {
                    message_id: "copy-1".into(),
                }),
            }),
            sent_again: Some(SentAgainView {
                message_id: "copy-2".into(),
            }),
            reactions: Some(vec![MessageReactionView {
                emoji: "👍".into(),
                actors: vec![
                    ReactionActorView {
                        is_self: true,
                        address: None,
                    },
                    ReactionActorView {
                        is_self: false,
                        address: Some("+15555550101".into()),
                    },
                ],
            }]),
            own_reaction_request: Some(OwnReactionRequestView {
                request_id: "req-1".into(),
                emoji: None,
                previous_emoji: Some("❤️".into()),
                state: OwnReactionRequestState::NotSent,
                refusal: Some(OwnReactionRefusal::InvalidEmoji),
            }),
            reactable: Some(ReactionRouteDto::Native),
            ..message(MessageSender::Other)
        };
        assert_eq!(
            wire(&full),
            json!({
                "id": "message-1",
                "revision": "7",
                "sender": "other",
                "senderAddress": "+15555550100",
                "body": "",
                "oneTimeCode": "123456",
                "timestamp": "",
                "status": "read",
                "statusAt": "1700000000001",
                "groupHint": "members_changed",
                "attachments": [{
                    "id": "attachment-1",
                    "name": "photo.jpg",
                    "mediaType": "image/jpeg",
                    "byteSize": 42,
                    "state": "ready"
                }],
                "transport": "rcs",
                "subject": "Subject",
                "participants": ["+15555550100"],
                "rcsReported": true,
                "sendCancellation": {"state": "too-late", "canCancel": false, "requiresGatewayUpdate": true},
                "sendRecovery": {
                    "kind": "unconfirmed-removed-cancel",
                    "canSendAgain": false,
                    "sentAgain": {"messageId": "copy-1"}
                },
                "sentAgain": {"messageId": "copy-2"},
                "reactions": [{
                    "emoji": "👍",
                    "actors": [{"self": true}, {"self": false, "address": "+15555550101"}]
                }],
                "ownReactionRequest": {
                    "requestId": "req-1",
                    "emoji": null,
                    "previousEmoji": "❤️",
                    "state": "not_sent",
                    "refusal": "invalid_emoji"
                },
                "reactable": "native"
            })
        );
    }

    #[test]
    fn message_union_values_are_unchanged() {
        for (sender, wire_value) in [
            (MessageSender::SelfSent, "self"),
            (MessageSender::Other, "other"),
        ] {
            assert_eq!(
                wire(&message(sender)),
                with_field(minimal_message_json(), "sender", json!(wire_value))
            );
        }
        for (status, wire_value) in [
            (MessageStatus::QueuedLocal, "queued-local"),
            (MessageStatus::ServerAccepted, "server-accepted"),
            (MessageStatus::GatewayPersisted, "gateway-persisted"),
            (MessageStatus::Preparing, "preparing"),
            (MessageStatus::Submitted, "submitted"),
            (MessageStatus::Sent, "sent"),
            (MessageStatus::DeliveryConfirmed, "delivery-confirmed"),
            (MessageStatus::FailedBeforeSubmit, "failed-before-submit"),
            (MessageStatus::FailedConfirmed, "failed-confirmed"),
            (MessageStatus::Unknown, "unknown"),
            (MessageStatus::Read, "read"),
        ] {
            let view = MessageView {
                status: Some(status),
                ..message(MessageSender::Other)
            };
            assert_eq!(
                wire(&view),
                with_field(minimal_message_json(), "status", json!(wire_value))
            );
        }
        for (group_hint, wire_value) in [
            (GroupHint::MembersChanged, "members_changed"),
            (GroupHint::UpdateRequired, "update_required"),
            (GroupHint::Withdrawn, "withdrawn"),
            (GroupHint::CreateUnknown, "create_unknown"),
            (GroupHint::CreateNotRcs, "create_not_rcs"),
        ] {
            let view = MessageView {
                group_hint: Some(group_hint),
                ..message(MessageSender::Other)
            };
            assert_eq!(
                wire(&view),
                with_field(minimal_message_json(), "groupHint", json!(wire_value))
            );
        }
        for (transport, wire_value) in [
            (MessageTransport::Sms, "sms"),
            (MessageTransport::Mms, "mms"),
            (MessageTransport::Rcs, "rcs"),
        ] {
            let view = MessageView {
                transport: Some(transport),
                ..message(MessageSender::Other)
            };
            assert_eq!(
                wire(&view),
                with_field(minimal_message_json(), "transport", json!(wire_value))
            );
        }
        for (reactable, wire_value) in [
            (ReactionRouteDto::Native, "native"),
            (ReactionRouteDto::Text, "text"),
        ] {
            let view = MessageView {
                reactable: Some(reactable),
                ..message(MessageSender::Other)
            };
            assert_eq!(
                wire(&view),
                with_field(minimal_message_json(), "reactable", json!(wire_value))
            );
        }
    }

    #[test]
    fn media_metadata_preview_is_nested_in_attachment_json() {
        let meta = MediaMetadataView {
            width: None,
            height: None,
            duration_ms: None,
            thumbnail: None,
            preview_unavailable: Some(MediaPreviewUnavailable::ThumbnailUnavailable),
        };
        let view = AttachmentView {
            media_meta: Some(Box::new(meta)),
            ..attachment(AttachmentStateDto::Ready)
        };
        assert_eq!(
            wire(&view),
            json!({
                "id": "attachment-1",
                "name": "photo.jpg",
                "mediaType": "image/jpeg",
                "byteSize": 42,
                "state": "ready",
                "mediaMeta": {"previewUnavailable": "thumbnail_unavailable"}
            })
        );
    }
}
