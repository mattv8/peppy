//! Pure device-bearer attachment-retention transport codecs.
//!
//! Hosts own authentication, network I/O, cancellation, and scheduling. In
//! particular, the status route is provider-backed and must not be polled as
//! part of normal sync.

use peppy_hosted_api::attachments::{
    ATTACHMENT_RETENTION_PROTOCOL_HEADER, ATTACHMENT_RETENTION_PROTOCOL_VERSION,
    AcknowledgeRetentionNoticeRequest, AttachmentRetentionErrorCode,
    AttachmentStorageStatusResponse, AttachmentUseResponse,
    CLIENT_SUPPORTED_ATTACHMENT_RETENTION_NOTICE_POLICY_VERSION, RegisterAttachmentUseRequest,
    ReleaseAttachmentUseRequest, RetentionCapabilityRequest, error_codes, routes,
};
use serde::Deserialize;

/// Maximum accepted JSON response size, in UTF-8 bytes.
pub const MAX_ATTACHMENT_RETENTION_RESPONSE_JSON: usize = 16 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachmentRetentionRequest {
    pub method: &'static str,
    pub relative_path: String,
    pub json_body: Option<String>,
    pub protocol_header: Option<AttachmentRetentionProtocolHeader>,
    pub expected_success_status: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AttachmentRetentionProtocolHeader {
    pub name: &'static str,
    pub value: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttachmentRetentionErrorClassification {
    RemovedByStoragePolicy,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceRetentionAvailability {
    Unsupported,
    TemporarilyUnavailable,
    Other,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum AttachmentRetentionCodecError {
    #[error("invalid attachment retention request")]
    InvalidRequest,
    #[error("invalid attachment retention response")]
    InvalidResponse,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ErrorResponse {
    code: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AttachmentRetentionErrorResponse {
    code: AttachmentRetentionErrorCode,
}

pub fn build_register_attachment_use_request(
    request: RegisterAttachmentUseRequest,
) -> Result<AttachmentRetentionRequest, AttachmentRetentionCodecError> {
    request
        .validate()
        .map_err(|_| AttachmentRetentionCodecError::InvalidRequest)?;
    let relative_path =
        routes::ATTACHMENT_USES.replace("{attachment_id}", &request.attachment_id.to_string());
    json_request("POST", relative_path, request, true, 200)
}

pub fn build_release_attachment_use_request(
    request: ReleaseAttachmentUseRequest,
) -> Result<AttachmentRetentionRequest, AttachmentRetentionCodecError> {
    request
        .validate()
        .map_err(|_| AttachmentRetentionCodecError::InvalidRequest)?;
    let relative_path = routes::ATTACHMENT_USE_RELEASE
        .replace("{attachment_id}", &request.attachment_id.to_string())
        .replace("{use_id}", &request.use_id.to_string());
    json_request("POST", relative_path, request, true, 200)
}

pub fn build_retention_capability_request(
    request: RetentionCapabilityRequest,
) -> Result<AttachmentRetentionRequest, AttachmentRetentionCodecError> {
    request
        .validate()
        .map_err(|_| AttachmentRetentionCodecError::InvalidRequest)?;
    json_request(
        "PUT",
        routes::RETENTION_CAPABILITY.into(),
        request,
        false,
        204,
    )
}

/// Builds the provider-backed device status GET. Hosts fetch it only on settings
/// open, explicit refresh, or once while presenting a notice—not during sync.
pub fn build_device_attachment_storage_status_request() -> AttachmentRetentionRequest {
    AttachmentRetentionRequest {
        method: "GET",
        relative_path: routes::DEVICE_ATTACHMENT_STORAGE_STATUS.into(),
        json_body: None,
        protocol_header: None,
        expected_success_status: 200,
    }
}

/// Builds the device display acknowledgement. This metadata write intentionally
/// has no retention-protocol header and does not depend on cleanup readiness.
pub fn build_device_retention_notice_acknowledgement_request(
    request: AcknowledgeRetentionNoticeRequest,
) -> Result<AttachmentRetentionRequest, AttachmentRetentionCodecError> {
    request
        .validate()
        .map_err(|_| AttachmentRetentionCodecError::InvalidRequest)?;
    json_request(
        "POST",
        routes::DEVICE_RETENTION_NOTICE_ACKNOWLEDGEMENT.into(),
        request,
        false,
        204,
    )
}

/// Builds an acknowledgement only for the reviewed copy bundled with this client.
/// Server-advertised future versions must remain visible but cannot be acknowledged.
pub fn build_client_supported_device_retention_notice_acknowledgement_request()
-> Result<AttachmentRetentionRequest, AttachmentRetentionCodecError> {
    build_device_retention_notice_acknowledgement_request(AcknowledgeRetentionNoticeRequest {
        policy_version: CLIENT_SUPPORTED_ATTACHMENT_RETENTION_NOTICE_POLICY_VERSION.into(),
    })
}

pub fn parse_attachment_use_response(
    response_json: &str,
) -> Result<AttachmentUseResponse, AttachmentRetentionCodecError> {
    let response: AttachmentUseResponse = parse_response(response_json)?;
    if response.use_id.is_nil() || response.attachment_id.is_nil() || response.device_id.is_nil() {
        return Err(AttachmentRetentionCodecError::InvalidResponse);
    }
    Ok(response)
}

/// Parses the canonical device status without deciding whether returned IDs or
/// state match a local pending operation; client-core owns that policy.
pub fn parse_attachment_storage_status_response(
    response_json: &str,
) -> Result<AttachmentStorageStatusResponse, AttachmentRetentionCodecError> {
    let response: AttachmentStorageStatusResponse = parse_response(response_json)?;
    if [
        response.used_bytes,
        response.limit_bytes,
        response.base_bytes,
        response.pack_bytes,
        response.protected_bytes,
        response.removed_bytes,
    ]
    .into_iter()
    .any(|bytes| bytes < 0)
        || [
            response.notice.presented_at,
            response.notice.acknowledged_at,
            response.notice.activation_at,
        ]
        .into_iter()
        .flatten()
        .any(|timestamp| timestamp < 0)
    {
        return Err(AttachmentRetentionCodecError::InvalidResponse);
    }
    Ok(response)
}

/// Only the canonical HTTP 410 code is a terminal removed-object result.
pub fn classify_attachment_retention_error(
    status: u16,
    response_json: &str,
) -> AttachmentRetentionErrorClassification {
    if status == 410
        && matches!(
            attachment_retention_error_code(response_json),
            Some(AttachmentRetentionErrorCode::AttachmentRemovedByStoragePolicy)
        )
    {
        AttachmentRetentionErrorClassification::RemovedByStoragePolicy
    } else {
        AttachmentRetentionErrorClassification::Other
    }
}

/// Classifies device bridge availability without making any vault mutation.
/// Any 403 is temporary, including an authorization denial not carrying the
/// device-specific error code.
pub fn classify_device_retention_availability(
    status: u16,
    response_json: &str,
) -> DeviceRetentionAvailability {
    if status == 501 && error_code(response_json).as_deref() == Some(error_codes::UNSUPPORTED) {
        DeviceRetentionAvailability::Unsupported
    } else if status == 403
        || (status == 503 && error_code(response_json).as_deref() == Some(error_codes::UNAVAILABLE))
    {
        DeviceRetentionAvailability::TemporarilyUnavailable
    } else {
        DeviceRetentionAvailability::Other
    }
}

fn json_request<T: serde::Serialize>(
    method: &'static str,
    relative_path: String,
    body: T,
    requires_protocol_header: bool,
    expected_success_status: u16,
) -> Result<AttachmentRetentionRequest, AttachmentRetentionCodecError> {
    let json_body =
        serde_json::to_string(&body).map_err(|_| AttachmentRetentionCodecError::InvalidRequest)?;
    Ok(AttachmentRetentionRequest {
        method,
        relative_path,
        json_body: Some(json_body),
        protocol_header: requires_protocol_header.then_some(AttachmentRetentionProtocolHeader {
            name: ATTACHMENT_RETENTION_PROTOCOL_HEADER,
            value: ATTACHMENT_RETENTION_PROTOCOL_VERSION,
        }),
        expected_success_status,
    })
}

fn parse_response<T: for<'de> Deserialize<'de>>(
    response_json: &str,
) -> Result<T, AttachmentRetentionCodecError> {
    if response_json.is_empty() || response_json.len() > MAX_ATTACHMENT_RETENTION_RESPONSE_JSON {
        return Err(AttachmentRetentionCodecError::InvalidResponse);
    }
    serde_json::from_str(response_json).map_err(|_| AttachmentRetentionCodecError::InvalidResponse)
}

fn error_code(response_json: &str) -> Option<String> {
    parse_response::<ErrorResponse>(response_json)
        .ok()
        .map(|error| error.code)
}

fn attachment_retention_error_code(response_json: &str) -> Option<AttachmentRetentionErrorCode> {
    parse_response::<AttachmentRetentionErrorResponse>(response_json)
        .ok()
        .map(|error| error.code)
}

#[cfg(test)]
mod tests {
    use super::*;
    use peppy_hosted_api::attachments::{
        AttachmentUseKind, AttachmentUseState, RetentionNoticeState, StorageCleanupState,
    };
    use serde_json::json;
    use uuid::Uuid;

    fn valid_status() -> serde_json::Value {
        json!({
            "used_bytes": 1, "limit_bytes": 2, "base_bytes": 2, "pack_bytes": 0,
            "protected_bytes": 1, "removed_bytes": 0, "cleanup_state": "blocked",
            "cleanup_unavailable_reason": "device_not_ready",
            "notice": {"state": "grace_period", "policy_version": "future_policy_v9", "presented_at": 1, "acknowledged_at": null, "activation_at": 2}
        })
    }

    fn valid_use_response() -> serde_json::Value {
        json!({
            "use_id": Uuid::new_v4(), "attachment_id": Uuid::new_v4(), "device_id": Uuid::new_v4(),
            "kind": "contact_photo", "state": "registered"
        })
    }

    #[test]
    fn mutation_builders_emit_canonical_routes_bodies_and_headers() {
        let attachment_id = Uuid::new_v4();
        let use_id = Uuid::new_v4();
        let device_id = Uuid::new_v4();
        let register = build_register_attachment_use_request(RegisterAttachmentUseRequest {
            use_id,
            attachment_id,
            device_id,
            kind: AttachmentUseKind::MessageHistory,
        })
        .unwrap();
        assert_eq!(register.method, "POST");
        assert_eq!(
            register.relative_path,
            format!("/v1/attachments/{attachment_id}/retention-uses")
        );
        assert_eq!(
            register.protocol_header,
            Some(AttachmentRetentionProtocolHeader {
                name: ATTACHMENT_RETENTION_PROTOCOL_HEADER,
                value: ATTACHMENT_RETENTION_PROTOCOL_VERSION,
            })
        );
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(register.json_body.as_deref().unwrap())
                .unwrap(),
            json!({"use_id": use_id, "attachment_id": attachment_id, "device_id": device_id, "kind": "message_history"})
        );
        assert!(!register.json_body.unwrap().contains("token"));

        let release = build_release_attachment_use_request(ReleaseAttachmentUseRequest {
            use_id,
            attachment_id,
            device_id,
        })
        .unwrap();
        assert_eq!(
            release.relative_path,
            format!("/v1/attachments/{attachment_id}/retention-uses/{use_id}/release")
        );
        assert_eq!(
            release.protocol_header,
            Some(AttachmentRetentionProtocolHeader {
                name: ATTACHMENT_RETENTION_PROTOCOL_HEADER,
                value: ATTACHMENT_RETENTION_PROTOCOL_VERSION,
            })
        );
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(release.json_body.as_deref().unwrap())
                .unwrap(),
            json!({"use_id": use_id, "attachment_id": attachment_id, "device_id": device_id})
        );
    }

    #[test]
    fn capability_status_and_notice_follow_the_device_contract() {
        let capability = build_retention_capability_request(RetentionCapabilityRequest {
            protocol_version: 1,
            cleanup_ready: false,
        })
        .unwrap();
        assert_eq!(capability.method, "PUT");
        assert_eq!(
            capability.relative_path,
            "/v1/devices/self/retention-capability"
        );
        assert!(capability.protocol_header.is_none());
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(capability.json_body.as_deref().unwrap())
                .unwrap(),
            json!({"protocol_version": 1, "cleanup_ready": false})
        );
        let status = build_device_attachment_storage_status_request();
        assert_eq!(status.method, "GET");
        assert_eq!(status.relative_path, "/v1/attachments/storage/status");
        assert!(status.json_body.is_none());
        assert!(status.protocol_header.is_none());
        let notice = build_device_retention_notice_acknowledgement_request(
            AcknowledgeRetentionNoticeRequest {
                policy_version: "hosted_attachment_retention_v1".into(),
            },
        )
        .unwrap();
        assert_eq!(
            notice.relative_path,
            "/v1/attachments/storage/notice/acknowledgement"
        );
        assert!(notice.protocol_header.is_none());
        assert_eq!(notice.method, "POST");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(notice.json_body.as_deref().unwrap())
                .unwrap(),
            json!({"policy_version": "hosted_attachment_retention_v1"})
        );
    }

    #[test]
    fn client_supported_notice_builder_uses_only_the_reviewed_copy_version() {
        let notice =
            build_client_supported_device_retention_notice_acknowledgement_request().unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(notice.json_body.as_deref().unwrap())
                .unwrap(),
            json!({"policy_version": "hosted_attachment_retention_v1"})
        );
        assert!(notice.protocol_header.is_none());
    }

    #[test]
    fn builders_reject_nil_ids_unknown_versions_and_invalid_notice() {
        assert!(
            build_register_attachment_use_request(RegisterAttachmentUseRequest {
                use_id: Uuid::nil(),
                attachment_id: Uuid::new_v4(),
                device_id: Uuid::new_v4(),
                kind: AttachmentUseKind::PendingSend,
            })
            .is_err()
        );
        assert!(
            build_retention_capability_request(RetentionCapabilityRequest {
                protocol_version: 2,
                cleanup_ready: true
            })
            .is_err()
        );
        assert!(
            build_device_retention_notice_acknowledgement_request(
                AcknowledgeRetentionNoticeRequest {
                    policy_version: String::new()
                }
            )
            .is_err()
        );
    }

    #[test]
    fn response_parsers_are_bounded_strict_and_preserve_unknown_policy_versions() {
        let status = valid_status();
        let parsed = parse_attachment_storage_status_response(&status.to_string()).unwrap();
        assert_eq!(
            parsed.notice.policy_version.as_deref(),
            Some("future_policy_v9")
        );
        assert_eq!(parsed.notice.state, RetentionNoticeState::GracePeriod);
        assert_eq!(parsed.cleanup_state, StorageCleanupState::Blocked);
        for field in [
            "used_bytes",
            "limit_bytes",
            "base_bytes",
            "pack_bytes",
            "protected_bytes",
            "removed_bytes",
        ] {
            let mut invalid = valid_status();
            invalid[field] = json!(-1);
            assert!(parse_attachment_storage_status_response(&invalid.to_string()).is_err());
        }
        for field in ["presented_at", "acknowledged_at", "activation_at"] {
            let mut invalid = valid_status();
            invalid["notice"][field] = json!(-1);
            assert!(parse_attachment_storage_status_response(&invalid.to_string()).is_err());
        }
        let mut unknown_root = valid_status();
        unknown_root["extra"] = json!(true);
        assert!(parse_attachment_storage_status_response(&unknown_root.to_string()).is_err());
        let mut unknown_notice = valid_status();
        unknown_notice["notice"]["extra"] = json!(true);
        assert!(parse_attachment_storage_status_response(&unknown_notice.to_string()).is_err());

        let mut padded = status.to_string();
        padded.push_str(&" ".repeat(MAX_ATTACHMENT_RETENTION_RESPONSE_JSON - padded.len()));
        assert_eq!(padded.len(), MAX_ATTACHMENT_RETENTION_RESPONSE_JSON);
        assert!(parse_attachment_storage_status_response(&padded).is_ok());
        padded.push(' ');
        assert_eq!(padded.len(), MAX_ATTACHMENT_RETENTION_RESPONSE_JSON + 1);
        assert!(parse_attachment_storage_status_response(&padded).is_err());
    }

    #[test]
    fn use_response_rejects_nil_and_unknown_fields_without_matching_local_state() {
        let response = valid_use_response();
        assert_eq!(
            parse_attachment_use_response(&response.to_string())
                .unwrap()
                .state,
            AttachmentUseState::Registered
        );
        let nil = json!({"use_id": Uuid::nil(), "attachment_id": Uuid::new_v4(), "device_id": Uuid::new_v4(), "kind": "contact_photo", "state": "registered"});
        assert!(parse_attachment_use_response(&nil.to_string()).is_err());
        let mut unknown = valid_use_response();
        unknown["unexpected"] = json!(true);
        assert!(parse_attachment_use_response(&unknown.to_string()).is_err());
    }

    #[test]
    fn only_the_exact_removed_error_is_terminal() {
        assert_eq!(
            classify_attachment_retention_error(
                410,
                r#"{"code":"attachment_removed_by_storage_policy"}"#
            ),
            AttachmentRetentionErrorClassification::RemovedByStoragePolicy
        );
        for (status, body) in [
            (404, r#"{"code":"attachment_removed_by_storage_policy"}"#),
            (410, r#"{"code":"storage_cleanup_pending"}"#),
            (401, r#"{"code":"attachment_removed_by_storage_policy"}"#),
        ] {
            assert_eq!(
                classify_attachment_retention_error(status, body),
                AttachmentRetentionErrorClassification::Other
            );
        }
        let mut oversized = r#"{"code":"attachment_removed_by_storage_policy"}"#.to_owned();
        oversized
            .push_str(&" ".repeat(MAX_ATTACHMENT_RETENTION_RESPONSE_JSON + 1 - oversized.len()));
        assert_eq!(
            classify_attachment_retention_error(410, &oversized),
            AttachmentRetentionErrorClassification::Other
        );
    }

    #[test]
    fn device_availability_never_turns_forbidden_into_a_permanent_result() {
        assert_eq!(
            classify_device_retention_availability(
                501,
                r#"{"code":"attachment_retention_unsupported"}"#
            ),
            DeviceRetentionAvailability::Unsupported
        );
        assert_eq!(
            classify_device_retention_availability(501, r#"{"code":"other"}"#),
            DeviceRetentionAvailability::Other
        );
        assert_eq!(
            classify_device_retention_availability(501, ""),
            DeviceRetentionAvailability::Other
        );
        assert_eq!(
            classify_device_retention_availability(403, r#"{"code":"operation_not_permitted"}"#),
            DeviceRetentionAvailability::TemporarilyUnavailable
        );
        for body in ["", "not json"] {
            assert_eq!(
                classify_device_retention_availability(403, body),
                DeviceRetentionAvailability::TemporarilyUnavailable
            );
        }
        assert_eq!(
            classify_device_retention_availability(
                503,
                r#"{"code":"attachment_retention_unavailable"}"#
            ),
            DeviceRetentionAvailability::TemporarilyUnavailable
        );
        assert_eq!(
            classify_device_retention_availability(503, r#"{"code":"storage_cleanup_pending"}"#),
            DeviceRetentionAvailability::Other
        );
    }
}
