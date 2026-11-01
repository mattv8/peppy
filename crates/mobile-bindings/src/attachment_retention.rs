//! Native-safe wrappers for pure attachment-retention transport codecs.
//!
//! These functions only build relative requests and parse bounded responses.
//! Native hosts retain bearer credentials, network I/O, cancellation, and
//! checkpoint ownership.

use super::{
    MobileBindingsError, NativeAttachmentUseKind, NativeAttachmentUseResponse,
    NativeAttachmentUseState, NativePendingAttachmentRetentionOperation,
};
use peppy_hosted_api::attachments::{
    ATTACHMENT_RETENTION_PROTOCOL_HEADER, ATTACHMENT_RETENTION_PROTOCOL_VERSION, AttachmentUseKind,
    CLIENT_SUPPORTED_ATTACHMENT_RETENTION_NOTICE_POLICY_VERSION, RegisterAttachmentUseRequest,
    ReleaseAttachmentUseRequest, RetentionCapabilityRequest,
};
use peppy_hosted_client::attachment_retention as codec;

#[uniffi::export]
pub fn attachment_retention_response_max_bytes() -> u64 {
    codec::MAX_ATTACHMENT_RETENTION_RESPONSE_JSON as u64
}

#[uniffi::export]
pub fn attachment_retention_response_read_limit_bytes() -> u64 {
    (codec::MAX_ATTACHMENT_RETENTION_RESPONSE_JSON + 1) as u64
}

#[uniffi::export]
pub fn attachment_retention_prepare_limit_max() -> u64 {
    peppy_client_core::MAX_PREPARED_ATTACHMENT_RETENTION_OPERATIONS as u64
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct NativeAttachmentRetentionProtocolHeader {
    pub name: String,
    pub value: String,
}

#[uniffi::export]
pub fn attachment_retention_protocol_header() -> NativeAttachmentRetentionProtocolHeader {
    NativeAttachmentRetentionProtocolHeader {
        name: ATTACHMENT_RETENTION_PROTOCOL_HEADER.into(),
        value: ATTACHMENT_RETENTION_PROTOCOL_VERSION.into(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct NativeAttachmentRetentionRequest {
    pub method: String,
    pub relative_path: String,
    pub json_body: Option<String>,
    pub protocol_header: Option<NativeAttachmentRetentionProtocolHeader>,
    pub expected_success_status: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum NativeAttachmentRetentionErrorClassification {
    RemovedByStoragePolicy,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum NativeDeviceRetentionAvailability {
    Unsupported,
    TemporarilyUnavailable,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum NativeRetentionNoticeState {
    NotPresented,
    GracePeriod,
    Eligible,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum NativeStorageCleanupState {
    Disabled,
    Blocked,
    Pending,
    Available,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum NativeStorageCleanupUnavailableReason {
    NoticeNotEligible,
    DeviceNotReady,
    ReadOnly,
    EntitlementUnavailable,
    ProviderReconciliationPending,
    BillingMutationPending,
    CleanupDisabled,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct NativeRetentionNotice {
    pub state: NativeRetentionNoticeState,
    pub policy_version: Option<String>,
    /// Unix seconds supplied by the server.
    pub presented_at_seconds: Option<i64>,
    /// Unix seconds supplied by the server.
    pub acknowledged_at_seconds: Option<i64>,
    /// Unix seconds supplied by the server.
    pub activation_at_seconds: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct NativeAttachmentStorageStatus {
    /// All size fields are bytes.
    pub used_bytes: i64,
    pub limit_bytes: i64,
    pub base_bytes: i64,
    pub pack_bytes: i64,
    pub protected_bytes: i64,
    pub removed_bytes: i64,
    pub cleanup_state: NativeStorageCleanupState,
    pub cleanup_unavailable_reason: Option<NativeStorageCleanupUnavailableReason>,
    pub notice: NativeRetentionNotice,
}

#[uniffi::export]
pub fn attachment_retention_register_request(
    operation: NativePendingAttachmentRetentionOperation,
) -> Result<NativeAttachmentRetentionRequest, MobileBindingsError> {
    let request = RegisterAttachmentUseRequest {
        use_id: parse_uuid(&operation.use_id)?,
        attachment_id: parse_uuid(&operation.attachment_id)?,
        device_id: parse_uuid(&operation.device_id)?,
        kind: attachment_use_kind(operation.kind),
    };
    codec::build_register_attachment_use_request(request)
        .map(request_view)
        .map_err(codec_error)
}

#[uniffi::export]
pub fn attachment_retention_release_request(
    operation: NativePendingAttachmentRetentionOperation,
) -> Result<NativeAttachmentRetentionRequest, MobileBindingsError> {
    let request = ReleaseAttachmentUseRequest {
        use_id: parse_uuid(&operation.use_id)?,
        attachment_id: parse_uuid(&operation.attachment_id)?,
        device_id: parse_uuid(&operation.device_id)?,
    };
    codec::build_release_attachment_use_request(request)
        .map(request_view)
        .map_err(codec_error)
}

#[uniffi::export]
pub fn attachment_retention_capability_request(
    cleanup_ready: bool,
) -> Result<NativeAttachmentRetentionRequest, MobileBindingsError> {
    codec::build_retention_capability_request(RetentionCapabilityRequest {
        protocol_version: RetentionCapabilityRequest::VERSION,
        cleanup_ready,
    })
    .map(request_view)
    .map_err(codec_error)
}

#[uniffi::export]
pub fn attachment_retention_status_request() -> NativeAttachmentRetentionRequest {
    request_view(codec::build_device_attachment_storage_status_request())
}

#[uniffi::export]
pub fn attachment_retention_notice_acknowledgement_request(
    policy_version: String,
) -> Result<NativeAttachmentRetentionRequest, MobileBindingsError> {
    if !notice_copy_supported(Some(&policy_version)) {
        return Err(MobileBindingsError::InvalidRequest);
    }
    codec::build_client_supported_device_retention_notice_acknowledgement_request()
        .map(request_view)
        .map_err(codec_error)
}

#[uniffi::export]
pub fn attachment_retention_supported_notice_policy_version() -> String {
    CLIENT_SUPPORTED_ATTACHMENT_RETENTION_NOTICE_POLICY_VERSION.into()
}

#[uniffi::export]
pub fn attachment_retention_notice_copy_supported(policy_version: Option<String>) -> bool {
    notice_copy_supported(policy_version.as_deref())
}

#[uniffi::export]
pub fn parse_attachment_retention_response(
    response_json: String,
) -> Result<NativeAttachmentUseResponse, MobileBindingsError> {
    let response = codec::parse_attachment_use_response(&response_json).map_err(codec_error)?;
    Ok(NativeAttachmentUseResponse {
        use_id: response.use_id.to_string(),
        attachment_id: response.attachment_id.to_string(),
        device_id: response.device_id.to_string(),
        kind: native_attachment_use_kind(response.kind),
        state: match response.state {
            peppy_hosted_api::attachments::AttachmentUseState::Registered => {
                NativeAttachmentUseState::Registered
            }
            peppy_hosted_api::attachments::AttachmentUseState::Released => {
                NativeAttachmentUseState::Released
            }
        },
    })
}

#[uniffi::export]
pub fn parse_attachment_retention_status(
    response_json: String,
) -> Result<NativeAttachmentStorageStatus, MobileBindingsError> {
    codec::parse_attachment_storage_status_response(&response_json)
        .map(status_view)
        .map_err(codec_error)
}

#[uniffi::export]
pub fn classify_attachment_retention_error(
    status: u16,
    response_json: String,
) -> NativeAttachmentRetentionErrorClassification {
    match codec::classify_attachment_retention_error(status, &response_json) {
        codec::AttachmentRetentionErrorClassification::RemovedByStoragePolicy => {
            NativeAttachmentRetentionErrorClassification::RemovedByStoragePolicy
        }
        codec::AttachmentRetentionErrorClassification::Other => {
            NativeAttachmentRetentionErrorClassification::Other
        }
    }
}

#[uniffi::export]
pub fn classify_device_retention_availability(
    status: u16,
    response_json: String,
) -> NativeDeviceRetentionAvailability {
    match codec::classify_device_retention_availability(status, &response_json) {
        codec::DeviceRetentionAvailability::Unsupported => {
            NativeDeviceRetentionAvailability::Unsupported
        }
        codec::DeviceRetentionAvailability::TemporarilyUnavailable => {
            NativeDeviceRetentionAvailability::TemporarilyUnavailable
        }
        codec::DeviceRetentionAvailability::Other => NativeDeviceRetentionAvailability::Other,
    }
}

fn parse_uuid(value: &str) -> Result<uuid::Uuid, MobileBindingsError> {
    uuid::Uuid::parse_str(value).map_err(|_| MobileBindingsError::InvalidRequest)
}

fn notice_copy_supported(policy_version: Option<&str>) -> bool {
    policy_version == Some(CLIENT_SUPPORTED_ATTACHMENT_RETENTION_NOTICE_POLICY_VERSION)
}

fn attachment_use_kind(value: NativeAttachmentUseKind) -> AttachmentUseKind {
    match value {
        NativeAttachmentUseKind::MessageHistory => AttachmentUseKind::MessageHistory,
        NativeAttachmentUseKind::PendingSend => AttachmentUseKind::PendingSend,
        NativeAttachmentUseKind::ContactPhoto => AttachmentUseKind::ContactPhoto,
        NativeAttachmentUseKind::RestoreCritical => AttachmentUseKind::RestoreCritical,
    }
}

fn native_attachment_use_kind(value: AttachmentUseKind) -> NativeAttachmentUseKind {
    match value {
        AttachmentUseKind::MessageHistory => NativeAttachmentUseKind::MessageHistory,
        AttachmentUseKind::PendingSend => NativeAttachmentUseKind::PendingSend,
        AttachmentUseKind::ContactPhoto => NativeAttachmentUseKind::ContactPhoto,
        AttachmentUseKind::RestoreCritical => NativeAttachmentUseKind::RestoreCritical,
    }
}

pub(crate) fn request_view(
    value: codec::AttachmentRetentionRequest,
) -> NativeAttachmentRetentionRequest {
    NativeAttachmentRetentionRequest {
        method: value.method.into(),
        relative_path: value.relative_path,
        json_body: value.json_body,
        protocol_header: value.protocol_header.map(|header| {
            NativeAttachmentRetentionProtocolHeader {
                name: header.name.into(),
                value: header.value.into(),
            }
        }),
        expected_success_status: value.expected_success_status,
    }
}

fn status_view(
    value: peppy_hosted_api::attachments::AttachmentStorageStatusResponse,
) -> NativeAttachmentStorageStatus {
    NativeAttachmentStorageStatus {
        used_bytes: value.used_bytes,
        limit_bytes: value.limit_bytes,
        base_bytes: value.base_bytes,
        pack_bytes: value.pack_bytes,
        protected_bytes: value.protected_bytes,
        removed_bytes: value.removed_bytes,
        cleanup_state: native_cleanup_state(value.cleanup_state),
        cleanup_unavailable_reason: value
            .cleanup_unavailable_reason
            .map(native_cleanup_unavailable_reason),
        notice: NativeRetentionNotice {
            state: match value.notice.state {
                peppy_hosted_api::attachments::RetentionNoticeState::NotPresented => {
                    NativeRetentionNoticeState::NotPresented
                }
                peppy_hosted_api::attachments::RetentionNoticeState::GracePeriod => {
                    NativeRetentionNoticeState::GracePeriod
                }
                peppy_hosted_api::attachments::RetentionNoticeState::Eligible => {
                    NativeRetentionNoticeState::Eligible
                }
            },
            policy_version: value.notice.policy_version,
            presented_at_seconds: value.notice.presented_at,
            acknowledged_at_seconds: value.notice.acknowledged_at,
            activation_at_seconds: value.notice.activation_at,
        },
    }
}

fn native_cleanup_state(
    value: peppy_hosted_api::attachments::StorageCleanupState,
) -> NativeStorageCleanupState {
    match value {
        peppy_hosted_api::attachments::StorageCleanupState::Disabled => {
            NativeStorageCleanupState::Disabled
        }
        peppy_hosted_api::attachments::StorageCleanupState::Blocked => {
            NativeStorageCleanupState::Blocked
        }
        peppy_hosted_api::attachments::StorageCleanupState::Pending => {
            NativeStorageCleanupState::Pending
        }
        peppy_hosted_api::attachments::StorageCleanupState::Available => {
            NativeStorageCleanupState::Available
        }
    }
}

fn native_cleanup_unavailable_reason(
    value: peppy_hosted_api::attachments::StorageCleanupUnavailableReason,
) -> NativeStorageCleanupUnavailableReason {
    match value {
        peppy_hosted_api::attachments::StorageCleanupUnavailableReason::NoticeNotEligible => NativeStorageCleanupUnavailableReason::NoticeNotEligible,
        peppy_hosted_api::attachments::StorageCleanupUnavailableReason::DeviceNotReady => NativeStorageCleanupUnavailableReason::DeviceNotReady,
        peppy_hosted_api::attachments::StorageCleanupUnavailableReason::ReadOnly => NativeStorageCleanupUnavailableReason::ReadOnly,
        peppy_hosted_api::attachments::StorageCleanupUnavailableReason::EntitlementUnavailable => NativeStorageCleanupUnavailableReason::EntitlementUnavailable,
        peppy_hosted_api::attachments::StorageCleanupUnavailableReason::ProviderReconciliationPending => NativeStorageCleanupUnavailableReason::ProviderReconciliationPending,
        peppy_hosted_api::attachments::StorageCleanupUnavailableReason::BillingMutationPending => NativeStorageCleanupUnavailableReason::BillingMutationPending,
        peppy_hosted_api::attachments::StorageCleanupUnavailableReason::CleanupDisabled => NativeStorageCleanupUnavailableReason::CleanupDisabled,
    }
}

fn codec_error(_: codec::AttachmentRetentionCodecError) -> MobileBindingsError {
    MobileBindingsError::InvalidRequest
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_protocol_header_helper_matches_the_canonical_contract() {
        assert_eq!(
            attachment_retention_protocol_header(),
            NativeAttachmentRetentionProtocolHeader {
                name: "Peppy-Attachment-Retention-Protocol".into(),
                value: "1".into(),
            }
        );
    }

    #[test]
    fn native_limits_match_the_canonical_codec_and_core_bounds() {
        assert_eq!(attachment_retention_response_max_bytes(), 16 * 1024);
        assert_eq!(
            attachment_retention_response_read_limit_bytes(),
            attachment_retention_response_max_bytes() + 1
        );
        assert_eq!(attachment_retention_prepare_limit_max(), 256);
    }

    #[test]
    fn parses_status_with_typed_cleanup_and_epoch_second_notice_fields() {
        const ACTIVATION_EPOCH_SECONDS: i64 = 1_700_000_000;
        const THIRTY_DAYS_SECONDS: i64 = 30 * 24 * 60 * 60;
        let status = parse_attachment_retention_status(
            format!(r#"{{"used_bytes":12345,"limit_bytes":23456,"base_bytes":10000,"pack_bytes":2345,"protected_bytes":8765,"removed_bytes":1234,"cleanup_state":"blocked","cleanup_unavailable_reason":"device_not_ready","notice":{{"state":"grace_period","policy_version":"hosted_attachment_retention_v1","presented_at":{ACTIVATION_EPOCH_SECONDS},"acknowledged_at":null,"activation_at":{}}}}}"#, ACTIVATION_EPOCH_SECONDS + THIRTY_DAYS_SECONDS),
        )
        .unwrap();
        assert_eq!(status.used_bytes, 12_345);
        assert_eq!(
            status.notice.presented_at_seconds,
            Some(ACTIVATION_EPOCH_SECONDS)
        );
        assert_eq!(
            status.notice.activation_at_seconds,
            Some(ACTIVATION_EPOCH_SECONDS + THIRTY_DAYS_SECONDS)
        );
        assert_eq!(
            status.notice.policy_version.as_deref(),
            Some("hosted_attachment_retention_v1")
        );
    }

    #[test]
    fn native_builders_preserve_canonical_body_header_status_and_validation() {
        let operation = NativePendingAttachmentRetentionOperation {
            operation: super::super::NativeAttachmentUseOperationKind::Register,
            use_id: uuid::Uuid::new_v4().to_string(),
            attachment_id: uuid::Uuid::new_v4().to_string(),
            device_id: uuid::Uuid::new_v4().to_string(),
            kind: NativeAttachmentUseKind::PendingSend,
        };
        let register = attachment_retention_register_request(operation.clone()).unwrap();
        assert_eq!(register.expected_success_status, 200);
        assert_eq!(
            register.protocol_header.as_ref().unwrap().name,
            "Peppy-Attachment-Retention-Protocol"
        );
        assert_eq!(register.protocol_header.as_ref().unwrap().value, "1");
        let body: serde_json::Value =
            serde_json::from_str(register.json_body.as_deref().unwrap()).unwrap();
        assert_eq!(body["use_id"], operation.use_id);
        assert_eq!(body["kind"], "pending_send");

        let release = attachment_retention_release_request(operation.clone()).unwrap();
        assert_eq!(release.expected_success_status, 200);
        assert!(
            release
                .relative_path
                .ends_with(&format!("/{}/release", operation.use_id))
        );
        assert!(release.protocol_header.is_some());

        let capability = attachment_retention_capability_request(false).unwrap();
        assert_eq!(capability.method, "PUT");
        assert_eq!(capability.expected_success_status, 204);
        assert!(capability.protocol_header.is_none());

        let status = attachment_retention_status_request();
        assert_eq!(status.method, "GET");
        assert_eq!(status.expected_success_status, 200);
        assert!(status.json_body.is_none());
        assert!(status.protocol_header.is_none());

        let mut invalid = operation;
        invalid.use_id = "not-a-uuid".into();
        assert!(matches!(
            attachment_retention_register_request(invalid),
            Err(MobileBindingsError::InvalidRequest)
        ));

        let notice = attachment_retention_notice_acknowledgement_request(
            CLIENT_SUPPORTED_ATTACHMENT_RETENTION_NOTICE_POLICY_VERSION.into(),
        )
        .unwrap();
        assert_eq!(notice.expected_success_status, 204);
        assert!(notice.protocol_header.is_none());
        assert!(
            attachment_retention_notice_acknowledgement_request(
                "hosted_attachment_retention_v2".into()
            )
            .is_err()
        );
    }

    #[test]
    fn native_classifiers_preserve_terminal_and_availability_distinctions() {
        assert_eq!(
            classify_attachment_retention_error(
                410,
                r#"{"code":"attachment_removed_by_storage_policy"}"#.into(),
            ),
            NativeAttachmentRetentionErrorClassification::RemovedByStoragePolicy
        );
        assert_eq!(
            classify_attachment_retention_error(
                404,
                r#"{"code":"attachment_removed_by_storage_policy"}"#.into()
            ),
            NativeAttachmentRetentionErrorClassification::Other
        );
        assert_eq!(
            classify_device_retention_availability(
                501,
                r#"{"code":"attachment_retention_unsupported"}"#.into()
            ),
            NativeDeviceRetentionAvailability::Unsupported
        );
        assert_eq!(
            classify_device_retention_availability(403, String::new()),
            NativeDeviceRetentionAvailability::TemporarilyUnavailable
        );
    }

    #[test]
    fn future_server_notice_is_not_supported_for_acknowledgement() {
        assert!(!attachment_retention_notice_copy_supported(Some(
            "hosted_attachment_retention_v2".into()
        )));
    }
}
