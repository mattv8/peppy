//! Sanitized UI view models (the Rust side of `src/bridge.ts`). Only display data is
//! serialized: no credentials, tokens, keys, file keys, raw paths or core records.
use peppy_client_core::{
    AppFilter, AttachmentId, AttachmentInfo, AttachmentState, Client, ComposeDraft, Direction,
    Error, MediaPage, MediaPageAround, Message, MessageId, MirroredNotification, SendState,
    Transport,
};
use serde::{Deserialize, Serialize};

pub mod avatar;
pub mod compose;
pub mod gateways;
pub mod images;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NotificationPreferences {
    pub message_banners: bool,
    pub mirrored_banners: bool,
    pub preview: Preview,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Preview {
    Full,
    Hidden,
}

impl Default for NotificationPreferences {
    fn default() -> Self {
        Self {
            message_banners: true,
            mirrored_banners: true,
            preview: Preview::Full,
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub version: &'static str,
    pub mode: &'static str,
    pub connection: Connection,
    pub encryption: Encryption,
    pub gateways: Vec<GatewayView>,
    pub conversations: Vec<ConversationView>,
    pub notifications: Vec<MirroredNotification>,
    pub app_filters: Vec<AppFilter>,
    pub notification_preferences: NotificationPreferences,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_conversation_id: Option<String>,
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
    pub contact_books: Option<Vec<serde_json::Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub contacts_pending_count: Option<u64>,
    /// `{repairRequired, projection?: {state, reason?}}`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub contact_sync: Option<serde_json::Value>,
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_seen_age_seconds: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub presence: Option<GatewayPresence>,
    #[serde(default)]
    pub wake_route: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mms_content_version: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mms_max_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mms_limit_source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mms_max_recipients: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capability_note: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GatewayAvailability {
    #[default]
    Unknown,
    Connected,
    Delayed,
    Offline,
    NeverConnected,
    NoSmsSim,
    SmsPermissionNeeded,
    SmsUnavailable,
    CapabilitiesUnknown,
    UnsupportedPlatform,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct GatewayPresence {
    pub state: GatewayPresenceState,
    pub last_seen_age_seconds: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GatewayPresenceState {
    Connected,
    Seen,
    Never,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reply_blocked_reason: Option<String>,
    /// This row is the reusable recipientless composition, not an empty history.
    #[serde(default)]
    pub recipientless_draft: bool,
    /// This row exists only for local compose state and has no established history.
    #[serde(default)]
    pub draft_only: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryPage {
    pub conversation_id: String,
    pub messages: Vec<MessageView>,
    pub has_more: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageView {
    pub id: String,
    pub revision: String,
    pub sender: &'static str,
    /// Actual incoming sender, retained separately from the self/other display category.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sender_address: Option<String>,
    pub body: String,
    /// Empty only when the provider did not supply a timestamp; never synthesized from arrival.
    pub timestamp: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<&'static str>,
    /// Gateway-recorded time of `status`, as positive decimal epoch milliseconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status_at: Option<String>,
    pub attachments: Vec<AttachmentView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transport: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub participants: Option<Vec<String>>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentView {
    pub id: String,
    pub name: String,
    pub media_type: String,
    pub byte_size: u64,
    pub state: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Native-generated, re-encoded PNG thumbnail data URL only (never a path or SVG/HTML).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preview_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transfer: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retryable: Option<bool>,
    /// Producer metadata links a ready thumbnail independently from the original's transfer state.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub media_meta: Option<Box<MediaMetadataView>>,
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
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaMetadataView {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thumbnail: Option<AttachmentView>,
    /// `thumbnail_unavailable` is producer-authenticated v3 completion evidence, not a host guess.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preview_unavailable: Option<&'static str>,
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
        preview_unavailable: meta.thumbnail.is_none().then_some("thumbnail_unavailable"),
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

#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DraftView {
    pub id: String,
    pub conversation_id: String,
    pub text: String,
    pub recipient_ids: Vec<String>,
    pub attachment_ids: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gateway_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sim_id: Option<String>,
    pub revision: String,
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
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PublicCopyView {
    pub url: String,
    pub expires_in_seconds: u64,
}

pub fn send_state_label(state: SendState) -> &'static str {
    match state {
        SendState::QueuedLocal => "queued-local",
        SendState::AcceptedServer => "server-accepted",
        SendState::PersistedGateway => "gateway-persisted",
        SendState::AttemptRecorded => "preparing",
        SendState::SubmittedToOs => "submitted",
        SendState::Sent => "sent",
        SendState::Delivered => "delivery-confirmed",
        SendState::FailedBeforeSubmission => "failed-before-submit",
        SendState::FailedConfirmed => "failed-confirmed",
        SendState::OutcomeUnknown => "unknown",
    }
}

pub fn attachment_state_label(state: AttachmentState) -> &'static str {
    match state {
        AttachmentState::PendingUpload => "uploading",
        AttachmentState::Uploaded | AttachmentState::Available => "ready",
        AttachmentState::PendingDownload => "pending",
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
            "failed"
        } else {
            attachment_state_label(info.state)
        },
        error,
        preview_url,
        transfer: match info.state {
            AttachmentState::PendingUpload => Some("upload"),
            AttachmentState::PendingDownload => Some("download"),
            AttachmentState::Uploaded | AttachmentState::Available => None,
        },
        retryable,
        media_meta: None,
    }
}

pub fn media_page_view(page: MediaPage) -> MediaPageView {
    MediaPageView {
        items: page
            .items
            .into_iter()
            .map(|item| MediaPageItemView {
                message_id: item.message.record.message_id.to_string(),
                sender_address: (item.message.direction == Direction::Incoming)
                    .then_some(item.message.sender_address)
                    .flatten(),
                timestamp: positive_ms_to_decimal(item.message.sent_at_ms).unwrap_or_default(),
                attachment: attachment_view(&item.attachment, None, None),
                media_meta: item.media_meta.map(media_metadata_view),
                cursor: cursor_view(item.cursor),
            })
            .collect(),
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
            "self"
        } else {
            "other"
        },
        sender_address: (message.payload.direction == Direction::Incoming)
            .then(|| message.payload.sender_address.clone())
            .flatten(),
        body: message.payload.body.clone(),
        timestamp: positive_ms_to_decimal(message.payload.sent_at_ms).unwrap_or_default(),
        status: message.send_state.map(send_state_label),
        status_at: positive_ms_to_decimal(message.status_at_ms),
        attachments,
        transport: match message.payload.transport {
            Transport::Sms => Some("sms"),
            Transport::Mms => Some("mms"),
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
        gateway_id: draft
            .route
            .as_ref()
            .map(|route| route.gateway_device_id.to_string()),
        sim_id: draft
            .route
            .as_ref()
            .map(|route| route.subscription_id.clone()),
        revision: draft.revision.to_string(),
    }
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
    fn draft_conversion_keeps_camel_case_omissions_and_decimal_revision() {
        let draft = ComposeDraft {
            draft_id: DraftId::new(),
            conversation_id: ConversationId::new(),
            text: "Hello".into(),
            recipients: vec!["+15555550123".into()],
            attachment_ids: vec![AttachmentId::new()],
            route: None,
            revision: 18_446_744_073_709_551_615,
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
    fn status_labels_remain_bridge_compatible() {
        assert_eq!(send_state_label(SendState::QueuedLocal), "queued-local");
        assert_eq!(send_state_label(SendState::Delivered), "delivery-confirmed");
        assert_eq!(send_state_label(SendState::OutcomeUnknown), "unknown");
        assert_eq!(
            attachment_state_label(AttachmentState::PendingDownload),
            "pending"
        );
        assert_eq!(attachment_state_label(AttachmentState::Available), "ready");
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
                imported: false,
                sent_at_ms: Some(2),
                ordered_at_ms: None,
                mms_context: None,
            },
            seen: true,
            send_state: Some(SendState::Sent),
            status_at_ms: Some(1_725_363_180_000),
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

    #[test]
    fn notification_preferences_match_the_existing_json_contract() {
        assert_eq!(
            serde_json::to_value(NotificationPreferences::default()).unwrap(),
            json!({
                "messageBanners": true,
                "mirroredBanners": true,
                "preview": "full",
            })
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
