//! Wire contracts for hosted attachment retention.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum AttachmentRetentionClass {
    #[default]
    Protected,
    MessageMedia,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum HostedAttachmentState {
    Available,
    RemovedByStoragePolicy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RetentionNoticeState {
    NotPresented,
    GracePeriod,
    Eligible,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AttachmentUseKind {
    MessageHistory,
    PendingSend,
    ContactPhoto,
    RestoreCritical,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StorageCleanupState {
    Disabled,
    Blocked,
    Pending,
    Available,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StorageCleanupUnavailableReason {
    NoticeNotEligible,
    DeviceNotReady,
    ReadOnly,
    EntitlementUnavailable,
    ProviderReconciliationPending,
    BillingMutationPending,
    CleanupDisabled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AttachmentRetentionErrorCode {
    AttachmentRemovedByStoragePolicy,
    StorageCleanupPending,
    AttachmentUseConflict,
    RetentionProtocolRequired,
}

/// Header required on retention-sensitive mutations by a ready device.
pub const ATTACHMENT_RETENTION_PROTOCOL_HEADER: &str = "Peppy-Attachment-Retention-Protocol";
pub const ATTACHMENT_RETENTION_PROTOCOL_VERSION: &str = "1";
/// The only notice copy shipped by current clients. This intentionally does
/// not track the server's current policy version: an unknown server version
/// remains visible to the UI but cannot be acknowledged with older copy.
pub const CLIENT_SUPPORTED_ATTACHMENT_RETENTION_NOTICE_POLICY_VERSION: &str =
    "hosted_attachment_retention_v1";

/// Mutation families that must carry the current retention-protocol header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetentionProtocolMutation {
    AttachmentReserve,
    AttachmentUpload,
    AttachmentFinalize,
    AttachmentUseRegistration,
    AttachmentUseRelease,
    AttachmentPublish,
    EventWrite,
    Enrollment,
    Revocation,
}

/// Returns true only for the currently supported protocol signal. Server
/// handlers atomically withdraw device readiness before processing a mutation
/// with a missing or unknown signal.
pub fn retention_protocol_is_current(value: Option<&str>) -> bool {
    value == Some(ATTACHMENT_RETENTION_PROTOCOL_VERSION)
}

/// Canonical reservation request. A retry with the same attachment ID must
/// preserve every field, including `retention_class`; legacy retries default to
/// protected and cannot reclassify an existing object. Retry identity compares
/// the decoded SHA-256 bytes, so uppercase and lowercase hex spellings match.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReserveAttachmentRequest {
    pub attachment_id: Option<Uuid>,
    pub declared_ciphertext_bytes: i64,
    pub declared_ciphertext_sha256: String,
    #[serde(default)]
    pub reference_tracking: bool,
    #[serde(default)]
    pub retention_class: AttachmentRetentionClass,
}

impl ReserveAttachmentRequest {
    pub fn validate(&self) -> Result<(), AttachmentRetentionErrorCode> {
        if self
            .attachment_id
            .is_some_and(|attachment_id| attachment_id.is_nil())
            || self.declared_ciphertext_bytes <= 0
            || self.declared_ciphertext_sha256.len() != 64
            || !self
                .declared_ciphertext_sha256
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
            || matches!(self.retention_class, AttachmentRetentionClass::MessageMedia)
                && !self.reference_tracking
        {
            return Err(AttachmentRetentionErrorCode::RetentionProtocolRequired);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReservedAttachmentResponse {
    pub attachment_id: Uuid,
    pub expires_in_seconds: u16,
    #[serde(default)]
    pub retention_class: AttachmentRetentionClass,
}

/// Returned as HTTP 200 by both registration and release endpoints for fresh
/// and replayed success. `Registered` attests durable protection metadata, not
/// physical attachment existence; clients may register an exact future ID before
/// reservation. Re-registering a released UUID with identical fields returns
/// `Released`; it never re-protects the use or authorizes dependent work. A new
/// protection lifecycle requires a new `use_id`.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RegisterAttachmentUseRequest {
    pub use_id: Uuid,
    pub attachment_id: Uuid,
    pub device_id: Uuid,
    pub kind: AttachmentUseKind,
}

impl RegisterAttachmentUseRequest {
    pub fn validate(&self) -> Result<(), AttachmentRetentionErrorCode> {
        if self.use_id.is_nil() || self.attachment_id.is_nil() || self.device_id.is_nil() {
            return Err(AttachmentRetentionErrorCode::RetentionProtocolRequired);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReleaseAttachmentUseRequest {
    pub use_id: Uuid,
    pub attachment_id: Uuid,
    pub device_id: Uuid,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AttachmentUseState {
    Registered,
    Released,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AttachmentUseResponse {
    pub use_id: Uuid,
    pub attachment_id: Uuid,
    pub device_id: Uuid,
    pub kind: AttachmentUseKind,
    pub state: AttachmentUseState,
}

pub const ATTACHMENT_USE_SUCCESS_STATUS_CODE: u16 = 200;

impl ReleaseAttachmentUseRequest {
    pub fn validate(&self) -> Result<(), AttachmentRetentionErrorCode> {
        if self.use_id.is_nil() || self.attachment_id.is_nil() || self.device_id.is_nil() {
            return Err(AttachmentRetentionErrorCode::RetentionProtocolRequired);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RetentionCapabilityRequest {
    pub protocol_version: u16,
    pub cleanup_ready: bool,
}

impl RetentionCapabilityRequest {
    pub const VERSION: u16 = 1;
    pub fn validate(&self) -> Result<(), AttachmentRetentionErrorCode> {
        (self.protocol_version == Self::VERSION)
            .then_some(())
            .ok_or(AttachmentRetentionErrorCode::RetentionProtocolRequired)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AttachmentStorageStatusResponse {
    pub used_bytes: i64,
    pub limit_bytes: i64,
    pub base_bytes: i64,
    pub pack_bytes: i64,
    pub protected_bytes: i64,
    pub removed_bytes: i64,
    pub notice: RetentionNoticeResponse,
    pub cleanup_state: StorageCleanupState,
    pub cleanup_unavailable_reason: Option<StorageCleanupUnavailableReason>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RetentionNoticeResponse {
    pub state: RetentionNoticeState,
    pub policy_version: Option<String>,
    pub presented_at: Option<i64>,
    pub acknowledged_at: Option<i64>,
    pub activation_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AcknowledgeRetentionNoticeRequest {
    pub policy_version: String,
}

impl AcknowledgeRetentionNoticeRequest {
    pub fn validate(&self) -> Result<(), AttachmentRetentionErrorCode> {
        (!self.policy_version.is_empty() && self.policy_version.len() <= 128)
            .then_some(())
            .ok_or(AttachmentRetentionErrorCode::RetentionProtocolRequired)
    }
}

pub mod error_codes {
    pub const UNSUPPORTED: &str = "attachment_retention_unsupported";
    pub const FORBIDDEN: &str = "attachment_retention_forbidden";
    pub const UNAVAILABLE: &str = "attachment_retention_unavailable";
    pub const INVALID_POLICY_VERSION: &str = "invalid_retention_policy_version";
}

pub mod routes {
    pub const ATTACHMENT_USES: &str = "/v1/attachments/{attachment_id}/retention-uses";
    pub const ATTACHMENT_USE_RELEASE: &str =
        "/v1/attachments/{attachment_id}/retention-uses/{use_id}/release";
    pub const RETENTION_CAPABILITY: &str = "/v1/devices/self/retention-capability";
    pub const ATTACHMENT_STORAGE_STATUS: &str = "/hosted/v1/attachment-storage/status";
    /// Device-bearer policy status. Hosts fetch this on settings open, explicit
    /// refresh, or once when presenting the notice; it is not a sync-poll route.
    pub const DEVICE_ATTACHMENT_STORAGE_STATUS: &str = "/v1/attachments/storage/status";
    pub const RETENTION_NOTICE_ACKNOWLEDGEMENT: &str =
        "/hosted/v1/attachment-storage/notice/acknowledgement";
    /// Device-bearer display acknowledgement. This metadata-only write does
    /// not require the attachment-retention protocol header or device cleanup
    /// readiness and is intentionally outside `RetentionProtocolMutation`.
    pub const DEVICE_RETENTION_NOTICE_ACKNOWLEDGEMENT: &str =
        "/v1/attachments/storage/notice/acknowledgement";
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn omitted_legacy_retention_class_defaults_to_protected() {
        #[derive(Deserialize)]
        struct LegacyCompatible {
            #[serde(default)]
            retention_class: AttachmentRetentionClass,
        }
        let value: LegacyCompatible = serde_json::from_value(json!({})).unwrap();
        assert_eq!(value.retention_class, AttachmentRetentionClass::Protected);
    }

    #[test]
    fn retention_requests_reject_unknown_or_invalid_identity_fields() {
        assert!(serde_json::from_value::<RegisterAttachmentUseRequest>(json!({"use_id": Uuid::nil(), "attachment_id": Uuid::new_v4(), "device_id": Uuid::new_v4(), "kind": "message_history", "unexpected": true})).is_err());
        let request = RegisterAttachmentUseRequest {
            use_id: Uuid::nil(),
            attachment_id: Uuid::new_v4(),
            device_id: Uuid::new_v4(),
            kind: AttachmentUseKind::MessageHistory,
        };
        assert_eq!(
            request.validate(),
            Err(AttachmentRetentionErrorCode::RetentionProtocolRequired)
        );
    }

    #[test]
    fn reservation_defaults_to_protected_and_rejects_invalid_reclassification() {
        let request: ReserveAttachmentRequest = serde_json::from_value(json!({
            "attachment_id": null, "declared_ciphertext_bytes": 1,
            "declared_ciphertext_sha256": "a".repeat(64), "reference_tracking": false
        }))
        .unwrap();
        assert_eq!(request.retention_class, AttachmentRetentionClass::Protected);
        assert!(request.validate().is_ok());
        let message_media = ReserveAttachmentRequest {
            retention_class: AttachmentRetentionClass::MessageMedia,
            ..request
        };
        assert!(message_media.validate().is_err());
    }

    #[test]
    fn reservation_response_defaults_legacy_class_and_accepts_uppercase_hashes() {
        let response: ReservedAttachmentResponse = serde_json::from_value(json!({
            "attachment_id": Uuid::new_v4(), "expires_in_seconds": 60
        }))
        .unwrap();
        assert_eq!(
            response.retention_class,
            AttachmentRetentionClass::Protected
        );
        let lowercase = ReserveAttachmentRequest {
            attachment_id: Some(Uuid::new_v4()),
            declared_ciphertext_bytes: 1,
            declared_ciphertext_sha256: "ab".repeat(32),
            reference_tracking: false,
            retention_class: AttachmentRetentionClass::Protected,
        };
        let uppercase = ReserveAttachmentRequest {
            declared_ciphertext_sha256: lowercase.declared_ciphertext_sha256.to_uppercase(),
            ..lowercase.clone()
        };
        assert!(uppercase.validate().is_ok());
        assert_eq!(
            hex::decode(&lowercase.declared_ciphertext_sha256).unwrap(),
            hex::decode(&uppercase.declared_ciphertext_sha256).unwrap()
        );
    }

    #[test]
    fn notice_and_capability_validation_fail_closed() {
        assert!(
            RetentionCapabilityRequest {
                protocol_version: 2,
                cleanup_ready: true
            }
            .validate()
            .is_err()
        );
        assert!(
            RetentionCapabilityRequest {
                protocol_version: RetentionCapabilityRequest::VERSION,
                cleanup_ready: false
            }
            .validate()
            .is_ok()
        );
        assert!(
            AcknowledgeRetentionNoticeRequest {
                policy_version: String::new()
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn retention_protocol_header_accepts_only_the_current_version() {
        assert!(retention_protocol_is_current(Some(
            ATTACHMENT_RETENTION_PROTOCOL_VERSION
        )));
        assert!(!retention_protocol_is_current(None));
        assert!(!retention_protocol_is_current(Some("2")));
        assert_eq!(
            ATTACHMENT_RETENTION_PROTOCOL_HEADER,
            "Peppy-Attachment-Retention-Protocol"
        );
    }

    #[test]
    fn device_retention_http_error_codes_are_canonical() {
        assert_eq!(error_codes::UNSUPPORTED, "attachment_retention_unsupported");
        assert_eq!(error_codes::FORBIDDEN, "attachment_retention_forbidden");
        assert_eq!(error_codes::UNAVAILABLE, "attachment_retention_unavailable");
        assert_eq!(
            error_codes::INVALID_POLICY_VERSION,
            "invalid_retention_policy_version"
        );
    }

    #[test]
    fn device_and_account_notice_routes_are_distinct() {
        assert_eq!(
            routes::DEVICE_ATTACHMENT_STORAGE_STATUS,
            "/v1/attachments/storage/status"
        );
        assert_eq!(
            routes::DEVICE_RETENTION_NOTICE_ACKNOWLEDGEMENT,
            "/v1/attachments/storage/notice/acknowledgement"
        );
        assert_eq!(
            routes::ATTACHMENT_STORAGE_STATUS,
            "/hosted/v1/attachment-storage/status"
        );
        assert_eq!(
            routes::RETENTION_NOTICE_ACKNOWLEDGEMENT,
            "/hosted/v1/attachment-storage/notice/acknowledgement"
        );
    }

    #[test]
    fn attachment_use_success_contract_is_http_ok_for_replays() {
        assert_eq!(ATTACHMENT_USE_SUCCESS_STATUS_CODE, 200);
    }
}
