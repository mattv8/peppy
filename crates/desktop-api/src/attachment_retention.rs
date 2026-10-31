//! Sanitized attachment-retention DTOs shared by desktop and browser worker hosts.

use peppy_client_core::{
    AttachmentId, AttachmentRetentionClass, AttachmentRetentionState, AttachmentUploadRetention,
    AttachmentUseKind, AttachmentUseOperationKind, AttachmentUseResponse, AttachmentUseState,
    DeviceId, PendingAttachmentUseOperation, RemoteAttachmentState,
};
use peppy_hosted_api::attachments::{
    AttachmentStorageStatusResponse, RetentionNoticeState, StorageCleanupState,
    StorageCleanupUnavailableReason,
};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum AttachmentUseKindDto {
    MessageHistory,
    PendingSend,
    ContactPhoto,
    RestoreCritical,
}

impl From<AttachmentUseKind> for AttachmentUseKindDto {
    fn from(value: AttachmentUseKind) -> Self {
        match value {
            AttachmentUseKind::MessageHistory => Self::MessageHistory,
            AttachmentUseKind::PendingSend => Self::PendingSend,
            AttachmentUseKind::ContactPhoto => Self::ContactPhoto,
            AttachmentUseKind::RestoreCritical => Self::RestoreCritical,
        }
    }
}

impl From<AttachmentUseKindDto> for AttachmentUseKind {
    fn from(value: AttachmentUseKindDto) -> Self {
        match value {
            AttachmentUseKindDto::MessageHistory => Self::MessageHistory,
            AttachmentUseKindDto::PendingSend => Self::PendingSend,
            AttachmentUseKindDto::ContactPhoto => Self::ContactPhoto,
            AttachmentUseKindDto::RestoreCritical => Self::RestoreCritical,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum AttachmentUseOperationKindDto {
    Register,
    Release,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PendingAttachmentUseOperationDto {
    pub operation: AttachmentUseOperationKindDto,
    pub use_id: String,
    pub attachment_id: String,
    pub device_id: String,
    pub kind: AttachmentUseKindDto,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum AttachmentUseStateDto {
    Registered,
    Released,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AttachmentUseResponseDto {
    pub use_id: String,
    pub attachment_id: String,
    pub device_id: String,
    pub kind: AttachmentUseKindDto,
    pub state: AttachmentUseStateDto,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentRetentionProtocolHeaderDto {
    pub name: String,
    pub value: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentRetentionRequestDto {
    pub method: String,
    pub relative_path: String,
    pub json_body: Option<String>,
    pub protocol_header: Option<AttachmentRetentionProtocolHeaderDto>,
    pub expected_success_status: u16,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AttachmentRetentionProtocolConfigurationInput {
    pub enabled: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AttachmentRetentionCapabilityInput {
    pub cleanup_ready: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AttachmentRetentionAttachmentInput {
    pub attachment_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AttachmentRetentionBeginInput {
    pub attachment_id: String,
    pub kind: AttachmentUseKindDto,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AttachmentRetentionPrepareInput {
    pub limit: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AttachmentRetentionResponseBodyInput {
    pub body: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AttachmentRetentionNoticeRequestInput {
    pub policy_version: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AttachmentRetentionReleaseInput {
    pub use_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentRetentionOperationsDto {
    pub operations: Vec<PendingAttachmentUseOperationDto>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentRetentionProtocolStatusDto {
    pub enabled: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentRetentionReadyDto {
    pub ready: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentRetentionPreparationPendingDto {
    pub pending: bool,
}

pub fn client_supported_attachment_retention_notice_acknowledgement_request_dto() -> Result<
    AttachmentRetentionRequestDto,
    peppy_hosted_client::attachment_retention::AttachmentRetentionCodecError,
> {
    peppy_hosted_client::attachment_retention::build_client_supported_device_retention_notice_acknowledgement_request()
        .map(attachment_retention_request_dto)
}

pub fn attachment_retention_request_dto(
    value: peppy_hosted_client::attachment_retention::AttachmentRetentionRequest,
) -> AttachmentRetentionRequestDto {
    AttachmentRetentionRequestDto {
        method: value.method.into(),
        relative_path: value.relative_path,
        json_body: value.json_body,
        protocol_header: value
            .protocol_header
            .map(|header| AttachmentRetentionProtocolHeaderDto {
                name: header.name.into(),
                value: header.value.into(),
            }),
        expected_success_status: value.expected_success_status,
    }
}

pub fn attachment_use_response_dto(
    value: peppy_hosted_api::attachments::AttachmentUseResponse,
) -> AttachmentUseResponseDto {
    AttachmentUseResponseDto {
        use_id: value.use_id.to_string(),
        attachment_id: value.attachment_id.to_string(),
        device_id: value.device_id.to_string(),
        kind: match value.kind {
            peppy_hosted_api::attachments::AttachmentUseKind::MessageHistory => {
                AttachmentUseKindDto::MessageHistory
            }
            peppy_hosted_api::attachments::AttachmentUseKind::PendingSend => {
                AttachmentUseKindDto::PendingSend
            }
            peppy_hosted_api::attachments::AttachmentUseKind::ContactPhoto => {
                AttachmentUseKindDto::ContactPhoto
            }
            peppy_hosted_api::attachments::AttachmentUseKind::RestoreCritical => {
                AttachmentUseKindDto::RestoreCritical
            }
        },
        state: match value.state {
            peppy_hosted_api::attachments::AttachmentUseState::Registered => {
                AttachmentUseStateDto::Registered
            }
            peppy_hosted_api::attachments::AttachmentUseState::Released => {
                AttachmentUseStateDto::Released
            }
        },
    }
}

pub fn attachment_use_response(value: AttachmentUseResponseDto) -> Option<AttachmentUseResponse> {
    Some(AttachmentUseResponse {
        use_id: uuid::Uuid::parse_str(&value.use_id).ok()?,
        attachment_id: value.attachment_id.parse::<AttachmentId>().ok()?,
        device_id: DeviceId(uuid::Uuid::parse_str(&value.device_id).ok()?),
        kind: value.kind.into(),
        state: match value.state {
            AttachmentUseStateDto::Registered => AttachmentUseState::Registered,
            AttachmentUseStateDto::Released => AttachmentUseState::Released,
        },
    })
}

pub fn pending_attachment_use_operation_dto(
    value: PendingAttachmentUseOperation,
) -> PendingAttachmentUseOperationDto {
    PendingAttachmentUseOperationDto {
        operation: match value.operation {
            AttachmentUseOperationKind::Register => AttachmentUseOperationKindDto::Register,
            AttachmentUseOperationKind::Release => AttachmentUseOperationKindDto::Release,
        },
        use_id: value.use_id.to_string(),
        attachment_id: value.attachment_id.to_string(),
        device_id: value.device_id.to_string(),
        kind: value.kind.into(),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum AttachmentRetentionClassDto {
    Protected,
    MessageMedia,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentUploadRetentionDto {
    pub retention_class: AttachmentRetentionClassDto,
    pub reference_tracking: bool,
}

pub fn attachment_upload_retention_dto(
    value: AttachmentUploadRetention,
) -> AttachmentUploadRetentionDto {
    AttachmentUploadRetentionDto {
        retention_class: match value.retention_class {
            AttachmentRetentionClass::Protected => AttachmentRetentionClassDto::Protected,
            AttachmentRetentionClass::MessageMedia => AttachmentRetentionClassDto::MessageMedia,
        },
        reference_tracking: value.reference_tracking,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum RemoteAttachmentStateDto {
    Available,
    RemovedByStoragePolicy,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentRetentionStateDto {
    pub remote: RemoteAttachmentStateDto,
    pub local_available: bool,
}

pub fn attachment_retention_state_dto(
    value: AttachmentRetentionState,
) -> AttachmentRetentionStateDto {
    AttachmentRetentionStateDto {
        remote: match value.remote {
            RemoteAttachmentState::Available => RemoteAttachmentStateDto::Available,
            RemoteAttachmentState::RemovedByStoragePolicy => {
                RemoteAttachmentStateDto::RemovedByStoragePolicy
            }
        },
        local_available: value.local_available,
    }
}

/// Display-safe storage-policy status. Byte fields are bytes; timestamps are
/// Unix seconds. Hosts preserve these values and do not derive cleanup
/// readiness or notice eligibility themselves.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentStorageStatusDto {
    #[ts(type = "number")]
    pub used_bytes: i64,
    #[ts(type = "number")]
    pub limit_bytes: i64,
    #[ts(type = "number")]
    pub base_bytes: i64,
    #[ts(type = "number")]
    pub pack_bytes: i64,
    #[ts(type = "number")]
    pub protected_bytes: i64,
    #[ts(type = "number")]
    pub removed_bytes: i64,
    pub cleanup_state: StorageCleanupStateDto,
    pub cleanup_unavailable_reason: Option<StorageCleanupUnavailableReasonDto>,
    pub notice: RetentionNoticeDto,
}

/// Host-local status binding and typed availability. `scope_id` is opaque
/// presentation state for the active vault/device/origin session and is never
/// sent to the server.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[ts(tag = "kind", rename_all = "snake_case")]
pub enum AttachmentRetentionStatusResult {
    Available {
        #[serde(rename = "scopeId")]
        #[ts(rename = "scopeId")]
        scope_id: String,
        status: AttachmentStorageStatusDto,
    },
    Unsupported,
    TemporarilyUnavailable,
}

/// Renderer acknowledgement input. Hosts compare `scope_id` with the active
/// captured binding before selecting credentials or issuing HTTP.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AttachmentRetentionNoticeAcknowledgementInput {
    pub scope_id: String,
    pub policy_version: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum AttachmentRetentionNoticeAcknowledgementResult {
    Acknowledged,
    Unsupported,
    TemporarilyUnavailable,
    StaleScope,
    UnsupportedPolicyVersion,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct RetentionNoticeDto {
    pub state: RetentionNoticeStateDto,
    pub policy_version: Option<String>,
    #[ts(type = "number | null")]
    pub presented_at_seconds: Option<i64>,
    #[ts(type = "number | null")]
    pub acknowledged_at_seconds: Option<i64>,
    #[ts(type = "number | null")]
    pub activation_at_seconds: Option<i64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum RetentionNoticeStateDto {
    NotPresented,
    GracePeriod,
    Eligible,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum StorageCleanupStateDto {
    Disabled,
    Blocked,
    Pending,
    Available,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum StorageCleanupUnavailableReasonDto {
    NoticeNotEligible,
    DeviceNotReady,
    ReadOnly,
    EntitlementUnavailable,
    ProviderReconciliationPending,
    BillingMutationPending,
    CleanupDisabled,
}

pub fn attachment_storage_status_dto(
    value: AttachmentStorageStatusResponse,
) -> AttachmentStorageStatusDto {
    AttachmentStorageStatusDto {
        used_bytes: value.used_bytes,
        limit_bytes: value.limit_bytes,
        base_bytes: value.base_bytes,
        pack_bytes: value.pack_bytes,
        protected_bytes: value.protected_bytes,
        removed_bytes: value.removed_bytes,
        cleanup_state: storage_cleanup_state_dto(value.cleanup_state),
        cleanup_unavailable_reason: value
            .cleanup_unavailable_reason
            .map(storage_cleanup_unavailable_reason_dto),
        notice: RetentionNoticeDto {
            state: retention_notice_state_dto(value.notice.state),
            policy_version: value.notice.policy_version,
            presented_at_seconds: value.notice.presented_at,
            acknowledged_at_seconds: value.notice.acknowledged_at,
            activation_at_seconds: value.notice.activation_at,
        },
    }
}

fn retention_notice_state_dto(value: RetentionNoticeState) -> RetentionNoticeStateDto {
    match value {
        RetentionNoticeState::NotPresented => RetentionNoticeStateDto::NotPresented,
        RetentionNoticeState::GracePeriod => RetentionNoticeStateDto::GracePeriod,
        RetentionNoticeState::Eligible => RetentionNoticeStateDto::Eligible,
    }
}

fn storage_cleanup_state_dto(value: StorageCleanupState) -> StorageCleanupStateDto {
    match value {
        StorageCleanupState::Disabled => StorageCleanupStateDto::Disabled,
        StorageCleanupState::Blocked => StorageCleanupStateDto::Blocked,
        StorageCleanupState::Pending => StorageCleanupStateDto::Pending,
        StorageCleanupState::Available => StorageCleanupStateDto::Available,
    }
}

fn storage_cleanup_unavailable_reason_dto(
    value: StorageCleanupUnavailableReason,
) -> StorageCleanupUnavailableReasonDto {
    match value {
        StorageCleanupUnavailableReason::NoticeNotEligible => {
            StorageCleanupUnavailableReasonDto::NoticeNotEligible
        }
        StorageCleanupUnavailableReason::DeviceNotReady => {
            StorageCleanupUnavailableReasonDto::DeviceNotReady
        }
        StorageCleanupUnavailableReason::ReadOnly => StorageCleanupUnavailableReasonDto::ReadOnly,
        StorageCleanupUnavailableReason::EntitlementUnavailable => {
            StorageCleanupUnavailableReasonDto::EntitlementUnavailable
        }
        StorageCleanupUnavailableReason::ProviderReconciliationPending => {
            StorageCleanupUnavailableReasonDto::ProviderReconciliationPending
        }
        StorageCleanupUnavailableReason::BillingMutationPending => {
            StorageCleanupUnavailableReasonDto::BillingMutationPending
        }
        StorageCleanupUnavailableReason::CleanupDisabled => {
            StorageCleanupUnavailableReasonDto::CleanupDisabled
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use peppy_client_core::{AttachmentId, DeviceId};
    use peppy_hosted_api::attachments::RetentionNoticeResponse;

    #[test]
    fn strict_transport_inputs_reject_extra_fields_on_otherwise_valid_payloads() {
        let operation = serde_json::json!({
            "operation": "register",
            "useId": uuid::Uuid::new_v4(),
            "attachmentId": AttachmentId::new(),
            "deviceId": DeviceId::new(),
            "kind": "pending_send",
            "extra": true,
        });
        assert!(serde_json::from_value::<PendingAttachmentUseOperationDto>(operation).is_err());

        let response = serde_json::json!({
            "useId": uuid::Uuid::new_v4(),
            "attachmentId": AttachmentId::new(),
            "deviceId": DeviceId::new(),
            "kind": "pending_send",
            "state": "registered",
            "extra": true,
        });
        assert!(serde_json::from_value::<AttachmentUseResponseDto>(response).is_err());

        let acknowledgement = serde_json::json!({
            "scopeId": "vault-device-origin-generation",
            "policyVersion": "hosted_attachment_retention_v1",
            "extra": true,
        });
        assert!(
            serde_json::from_value::<AttachmentRetentionNoticeAcknowledgementInput>(
                acknowledgement,
            )
            .is_err()
        );
    }

    #[test]
    fn pending_operation_dto_contains_only_canonical_transport_identity() {
        let dto = pending_attachment_use_operation_dto(PendingAttachmentUseOperation {
            operation: AttachmentUseOperationKind::Register,
            use_id: uuid::Uuid::new_v4(),
            attachment_id: AttachmentId::new(),
            device_id: DeviceId::new(),
            kind: AttachmentUseKind::PendingSend,
        });
        let value = serde_json::to_value(dto).unwrap();
        assert_eq!(value["operation"], "register");
        assert_eq!(value["kind"], "pending_send");
        assert_eq!(value.as_object().unwrap().len(), 5);

        let release = pending_attachment_use_operation_dto(PendingAttachmentUseOperation {
            operation: AttachmentUseOperationKind::Release,
            use_id: uuid::Uuid::new_v4(),
            attachment_id: AttachmentId::new(),
            device_id: DeviceId::new(),
            kind: AttachmentUseKind::ContactPhoto,
        });
        let release = serde_json::to_value(release).unwrap();
        assert_eq!(release["operation"], "release");
        assert_eq!(release["kind"], "contact_photo");
        assert_eq!(release.as_object().unwrap().len(), 5);
    }

    #[test]
    fn generated_worker_transport_dtos_have_one_canonical_json_shape() {
        let request =
            client_supported_attachment_retention_notice_acknowledgement_request_dto().unwrap();
        assert_eq!(
            serde_json::to_value(request).unwrap(),
            serde_json::json!({
                "method": "POST",
                "relativePath": "/v1/attachments/storage/notice/acknowledgement",
                "jsonBody": "{\"policy_version\":\"hosted_attachment_retention_v1\"}",
                "protocolHeader": null,
                "expectedSuccessStatus": 204,
            })
        );

        let response = AttachmentUseResponseDto {
            use_id: uuid::Uuid::new_v4().to_string(),
            attachment_id: AttachmentId::new().to_string(),
            device_id: DeviceId::new().to_string(),
            kind: AttachmentUseKindDto::PendingSend,
            state: AttachmentUseStateDto::Registered,
        };
        assert_eq!(
            serde_json::to_value(response).unwrap()["state"],
            "registered"
        );
    }

    #[test]
    fn scoped_status_result_distinguishes_transport_availability() {
        let available = AttachmentRetentionStatusResult::Available {
            scope_id: "vault-device-origin-generation".into(),
            status: attachment_storage_status_dto(AttachmentStorageStatusResponse {
                used_bytes: 1,
                limit_bytes: 2,
                base_bytes: 2,
                pack_bytes: 0,
                protected_bytes: 1,
                removed_bytes: 0,
                cleanup_state: StorageCleanupState::Blocked,
                cleanup_unavailable_reason: Some(StorageCleanupUnavailableReason::DeviceNotReady),
                notice: RetentionNoticeResponse {
                    state: RetentionNoticeState::NotPresented,
                    policy_version: None,
                    presented_at: None,
                    acknowledged_at: None,
                    activation_at: None,
                },
            }),
        };
        let available = serde_json::to_value(available).unwrap();
        assert_eq!(available["kind"], "available");
        assert_eq!(available["scopeId"], "vault-device-origin-generation");
        assert!(available.get("scope_id").is_none());

        assert_eq!(
            serde_json::to_value(AttachmentRetentionStatusResult::Unsupported).unwrap(),
            serde_json::json!({"kind": "unsupported"})
        );
        assert_eq!(
            serde_json::to_value(AttachmentRetentionStatusResult::TemporarilyUnavailable).unwrap(),
            serde_json::json!({"kind": "temporarily_unavailable"})
        );
    }

    #[test]
    fn status_dto_serializes_epoch_seconds_and_thirty_day_notice_interval() {
        const ACTIVATION_EPOCH_SECONDS: i64 = 1_700_000_000;
        const NOTICE_INTERVAL_SECONDS: i64 = 30 * 24 * 60 * 60;
        let status = AttachmentStorageStatusResponse {
            used_bytes: 12_345,
            limit_bytes: 23_456,
            base_bytes: 10_000,
            pack_bytes: 2_345,
            protected_bytes: 8_765,
            removed_bytes: 1_234,
            cleanup_state: StorageCleanupState::Blocked,
            cleanup_unavailable_reason: Some(StorageCleanupUnavailableReason::DeviceNotReady),
            notice: RetentionNoticeResponse {
                state: RetentionNoticeState::GracePeriod,
                policy_version: Some("hosted_attachment_retention_v1".into()),
                presented_at: Some(ACTIVATION_EPOCH_SECONDS),
                acknowledged_at: None,
                activation_at: Some(ACTIVATION_EPOCH_SECONDS + NOTICE_INTERVAL_SECONDS),
            },
        };

        let dto = attachment_storage_status_dto(status);
        let serialized = serde_json::to_value(dto).unwrap();
        assert_eq!(serialized["usedBytes"], 12_345);
        assert_eq!(
            serialized["notice"]["presentedAtSeconds"],
            ACTIVATION_EPOCH_SECONDS
        );
        assert_eq!(
            serialized["notice"]["activationAtSeconds"],
            ACTIVATION_EPOCH_SECONDS + NOTICE_INTERVAL_SECONDS
        );
    }
}
