use super::*;
use peppy_desktop_api::{
    AttachmentRetentionAttachmentInput, AttachmentRetentionBeginInput,
    AttachmentRetentionCapabilityInput, AttachmentRetentionNoticeRequestInput,
    AttachmentRetentionOperationsDto, AttachmentRetentionPreparationPendingDto,
    AttachmentRetentionPrepareInput, AttachmentRetentionProtocolConfigurationInput,
    AttachmentRetentionProtocolStatusDto, AttachmentRetentionReadyDto,
    AttachmentRetentionReleaseInput, AttachmentRetentionResponseBodyInput, AttachmentUseKindDto,
    AttachmentUseOperationKindDto, PendingAttachmentUseOperationDto,
    attachment_retention_request_dto, attachment_retention_state_dto,
    attachment_storage_status_dto, attachment_upload_retention_dto, attachment_use_response,
    attachment_use_response_dto,
    client_supported_attachment_retention_notice_acknowledgement_request_dto,
    pending_attachment_use_operation_dto,
};
use peppy_hosted_api::attachments::{
    RegisterAttachmentUseRequest, ReleaseAttachmentUseRequest, RetentionCapabilityRequest,
};
use peppy_hosted_client::attachment_retention as codec;
use serde::Deserialize;
use serde_json::{Value, json};

pub(super) fn handles(command: &str) -> bool {
    matches!(
        command,
        "_worker_attachment_retention_begin"
            | "_worker_attachment_retention_prepare"
            | "_worker_attachment_retention_pending"
            | "_worker_attachment_retention_preparation_pending"
            | "_worker_attachment_retention_capability_request"
            | "_worker_attachment_retention_configure_protocol"
            | "_worker_attachment_retention_protocol_status"
            | "_worker_attachment_retention_upload_classification"
            | "_worker_attachment_retention_status_request"
            | "_worker_attachment_retention_parse_status"
            | "_worker_attachment_retention_notice_acknowledgement_request"
            | "_worker_attachment_retention_operation_request"
            | "_worker_attachment_retention_parse_operation_response"
            | "_worker_attachment_retention_apply_response"
            | "_worker_attachment_retention_request_release"
            | "_worker_attachment_retention_report_removed"
            | "attachment_retention_state"
            | "_worker_attachment_retention_ready"
            | "_worker_attachment_retention_mark_reconciled"
            | "_worker_attachment_retention_invalidate_reconciliation"
    )
}

impl BrowserCore {
    pub(super) fn attachment_retention_command(
        &self,
        command: &str,
        args: Value,
    ) -> Result<Value, Failure> {
        match command {
            "_worker_attachment_retention_capability_request" => {
                let input: AttachmentRetentionCapabilityInput =
                    serde_json::from_value(args).map_err(|_| invalid())?;
                let request =
                    codec::build_retention_capability_request(RetentionCapabilityRequest {
                        protocol_version: RetentionCapabilityRequest::VERSION,
                        cleanup_ready: input.cleanup_ready,
                    })
                    .map_err(|_| invalid())?;
                request_json(request)
            }
            "_worker_attachment_retention_configure_protocol" => {
                let input: AttachmentRetentionProtocolConfigurationInput =
                    serde_json::from_value(args).map_err(|_| invalid())?;
                self.client()?
                    .configure_attachment_retention_protocol(input.enabled)
                    .map_err(core)?;
                Ok(json!({}))
            }
            "_worker_attachment_retention_protocol_status" => {
                require_empty(args)?;
                serialize(AttachmentRetentionProtocolStatusDto {
                    enabled: self
                        .client()?
                        .attachment_retention_protocol_enabled()
                        .map_err(core)?,
                })
            }
            "_worker_attachment_retention_upload_classification" => {
                let input: AttachmentRetentionAttachmentInput =
                    serde_json::from_value(args).map_err(|_| invalid())?;
                let classification = self
                    .client()?
                    .attachment_upload_retention(parse_attachment_id(&input.attachment_id)?)
                    .map_err(core)?;
                serde_json::to_value(attachment_upload_retention_dto(classification))
                    .map_err(|_| core(CoreError::Database))
            }
            "_worker_attachment_retention_status_request" => {
                require_empty(args)?;
                request_json(codec::build_device_attachment_storage_status_request())
            }
            "_worker_attachment_retention_parse_status" => {
                let input: AttachmentRetentionResponseBodyInput =
                    serde_json::from_value(args).map_err(|_| invalid())?;
                let status = codec::parse_attachment_storage_status_response(&input.body)
                    .map_err(|_| invalid())?;
                serde_json::to_value(attachment_storage_status_dto(status))
                    .map_err(|_| core(CoreError::Database))
            }
            "_worker_attachment_retention_notice_acknowledgement_request" => {
                let input: AttachmentRetentionNoticeRequestInput =
                    serde_json::from_value(args).map_err(|_| invalid())?;
                if input.policy_version
                    != peppy_hosted_api::attachments::CLIENT_SUPPORTED_ATTACHMENT_RETENTION_NOTICE_POLICY_VERSION
                {
                    return Err(invalid());
                }
                let request =
                    client_supported_attachment_retention_notice_acknowledgement_request_dto()
                        .map_err(|_| invalid())?;
                serialize(request)
            }
            "_worker_attachment_retention_operation_request" => {
                let input: PendingAttachmentUseOperationDto =
                    serde_json::from_value(args).map_err(|_| invalid())?;
                let request = operation_request(input).map_err(|_| invalid())?;
                request_json(request)
            }
            "_worker_attachment_retention_parse_operation_response" => {
                let input: AttachmentRetentionResponseBodyInput =
                    serde_json::from_value(args).map_err(|_| invalid())?;
                let response =
                    codec::parse_attachment_use_response(&input.body).map_err(|_| invalid())?;
                serialize(attachment_use_response_dto(response))
            }
            "_worker_attachment_retention_begin" => {
                let input: AttachmentRetentionBeginInput =
                    serde_json::from_value(args).map_err(|_| invalid())?;
                let operation = self
                    .client()?
                    .begin_attachment_retention_use(
                        parse_attachment_id(&input.attachment_id)?,
                        input.kind.into(),
                    )
                    .map_err(core)?;
                serialize(pending_attachment_use_operation_dto(operation))
            }
            "_worker_attachment_retention_prepare" => {
                let input: AttachmentRetentionPrepareInput =
                    serde_json::from_value(args).map_err(|_| invalid())?;
                let operations = self
                    .client()?
                    .prepare_attachment_retention_operations(input.limit)
                    .map_err(core)?
                    .into_iter()
                    .map(pending_attachment_use_operation_dto)
                    .collect::<Vec<_>>();
                serialize(AttachmentRetentionOperationsDto { operations })
            }
            "_worker_attachment_retention_pending" => {
                require_empty(args)?;
                let operations = self
                    .client()?
                    .pending_attachment_retention_operations()
                    .map_err(core)?
                    .into_iter()
                    .map(pending_attachment_use_operation_dto)
                    .collect::<Vec<_>>();
                serialize(AttachmentRetentionOperationsDto { operations })
            }
            "_worker_attachment_retention_preparation_pending" => {
                require_empty(args)?;
                serialize(AttachmentRetentionPreparationPendingDto {
                    pending: self
                        .client()?
                        .attachment_retention_preparation_pending()
                        .map_err(core)?,
                })
            }
            "_worker_attachment_retention_apply_response" => {
                let input: peppy_desktop_api::AttachmentUseResponseDto =
                    serde_json::from_value(args).map_err(|_| invalid())?;
                let response = attachment_use_response(input).ok_or_else(invalid)?;
                self.client()?
                    .apply_attachment_retention_response(&response)
                    .map_err(core)?;
                Ok(json!({}))
            }
            "_worker_attachment_retention_request_release" => {
                let input: AttachmentRetentionReleaseInput =
                    serde_json::from_value(args).map_err(|_| invalid())?;
                let operation = self
                    .client()?
                    .request_attachment_retention_release(
                        uuid::Uuid::parse_str(&input.use_id).map_err(|_| invalid())?,
                    )
                    .map_err(core)?;
                serialize(pending_attachment_use_operation_dto(operation))
            }
            "_worker_attachment_retention_report_removed" => {
                let input: AttachmentRetentionAttachmentInput =
                    serde_json::from_value(args).map_err(|_| invalid())?;
                self.client()?
                    .report_attachment_removed_by_storage_policy(parse_attachment_id(
                        &input.attachment_id,
                    )?)
                    .map_err(core)?;
                Ok(json!({}))
            }
            "attachment_retention_state" => {
                let input: AttachmentRetentionAttachmentInput =
                    serde_json::from_value(args).map_err(|_| invalid())?;
                let state = self
                    .client()?
                    .attachment_retention_state(parse_attachment_id(&input.attachment_id)?)
                    .map_err(core)?;
                serde_json::to_value(attachment_retention_state_dto(state))
                    .map_err(|_| core(CoreError::Database))
            }
            "_worker_attachment_retention_ready" => {
                require_empty(args)?;
                serialize(AttachmentRetentionReadyDto {
                    ready: self.client()?.attachment_retention_ready().map_err(core)?,
                })
            }
            "_worker_attachment_retention_mark_reconciled" => {
                require_empty(args)?;
                self.client()?
                    .mark_attachment_retention_reconciled()
                    .map_err(core)?;
                Ok(json!({}))
            }
            "_worker_attachment_retention_invalidate_reconciliation" => {
                require_empty(args)?;
                self.client()?
                    .invalidate_attachment_retention_reconciliation()
                    .map_err(core)?;
                Ok(json!({}))
            }
            _ => Err(unknown_command()),
        }
    }
}

fn request_json(request: codec::AttachmentRetentionRequest) -> Result<Value, Failure> {
    serialize(attachment_retention_request_dto(request))
}

fn serialize(value: impl serde::Serialize) -> Result<Value, Failure> {
    serde_json::to_value(value).map_err(|_| core(CoreError::Database))
}

fn operation_request(
    input: PendingAttachmentUseOperationDto,
) -> Result<codec::AttachmentRetentionRequest, codec::AttachmentRetentionCodecError> {
    let use_id = uuid::Uuid::parse_str(&input.use_id)
        .map_err(|_| codec::AttachmentRetentionCodecError::InvalidRequest)?;
    let attachment_id = uuid::Uuid::parse_str(&input.attachment_id)
        .map_err(|_| codec::AttachmentRetentionCodecError::InvalidRequest)?;
    let device_id = uuid::Uuid::parse_str(&input.device_id)
        .map_err(|_| codec::AttachmentRetentionCodecError::InvalidRequest)?;
    match input.operation {
        AttachmentUseOperationKindDto::Register => {
            codec::build_register_attachment_use_request(RegisterAttachmentUseRequest {
                use_id,
                attachment_id,
                device_id,
                kind: hosted_attachment_use_kind(input.kind),
            })
        }
        AttachmentUseOperationKindDto::Release => {
            codec::build_release_attachment_use_request(ReleaseAttachmentUseRequest {
                use_id,
                attachment_id,
                device_id,
            })
        }
    }
}

fn hosted_attachment_use_kind(
    value: AttachmentUseKindDto,
) -> peppy_hosted_api::attachments::AttachmentUseKind {
    match value {
        AttachmentUseKindDto::MessageHistory => {
            peppy_hosted_api::attachments::AttachmentUseKind::MessageHistory
        }
        AttachmentUseKindDto::PendingSend => {
            peppy_hosted_api::attachments::AttachmentUseKind::PendingSend
        }
        AttachmentUseKindDto::ContactPhoto => {
            peppy_hosted_api::attachments::AttachmentUseKind::ContactPhoto
        }
        AttachmentUseKindDto::RestoreCritical => {
            peppy_hosted_api::attachments::AttachmentUseKind::RestoreCritical
        }
    }
}

fn parse_attachment_id(value: &str) -> Result<AttachmentId, Failure> {
    value.parse().map_err(|_| invalid())
}

fn require_empty(args: Value) -> Result<(), Failure> {
    serde_json::from_value::<EmptyArgs>(args)
        .map(|_| ())
        .map_err(|_| invalid())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EmptyArgs {}

#[cfg(test)]
mod tests {
    use super::*;
    use peppy_client_core::{ClientConfig, DatabaseKey, VaultId};

    fn dispatch(core: &mut BrowserCore, command: &str, args: Value) -> Value {
        serde_json::from_str(&core.dispatch(&json!({"command": command, "args": args}).to_string()))
            .unwrap()
    }

    #[test]
    fn retention_allowlist_uses_sanitized_dtos_and_checkpoint_invalidation() {
        let root = tempfile::tempdir().unwrap();
        let mut browser = BrowserCore::new(root.path().to_owned());
        let device_id = DeviceId::new();
        browser.client = Some(
            Client::open(
                ClientConfig {
                    database_path: root.path().join("client.db"),
                    vault_id: VaultId::new(),
                    device_id,
                },
                DatabaseKey::new(&[4; 32]).unwrap(),
            )
            .unwrap(),
        );
        browser.device_id = Some(device_id);
        let id = AttachmentId::new();
        assert_eq!(
            dispatch(
                &mut browser,
                "_worker_attachment_retention_protocol_status",
                json!({}),
            )["value"]["enabled"],
            false
        );
        dispatch(
            &mut browser,
            "_worker_attachment_retention_configure_protocol",
            json!({"enabled": true}),
        );
        assert_eq!(
            dispatch(
                &mut browser,
                "_worker_attachment_retention_protocol_status",
                json!({}),
            )["value"]["enabled"],
            true
        );

        let begun = dispatch(
            &mut browser,
            "_worker_attachment_retention_begin",
            json!({"attachmentId": id, "kind": "pending_send"}),
        );
        assert_eq!(begun["value"]["attachmentId"], id.to_string());
        assert_eq!(begun["value"]["deviceId"], device_id.to_string());
        assert!(begun.to_string().len() < 1024);
        let prepared = dispatch(
            &mut browser,
            "_worker_attachment_retention_prepare",
            json!({"limit": 10}),
        );
        assert_eq!(
            prepared["value"]["operations"],
            json!([begun["value"].clone()])
        );
        assert_eq!(
            dispatch(
                &mut browser,
                "_worker_attachment_retention_begin",
                json!({"attachmentId": id, "kind": "pending_send", "extra": true}),
            )["error"]["code"],
            "invalid-request"
        );

        dispatch(
            &mut browser,
            "_worker_attachment_retention_apply_response",
            json!({
                "useId": begun["value"]["useId"],
                "attachmentId": begun["value"]["attachmentId"],
                "deviceId": begun["value"]["deviceId"],
                "kind": begun["value"]["kind"],
                "state": "registered",
            }),
        );
        let reconciled = dispatch(
            &mut browser,
            "_worker_attachment_retention_mark_reconciled",
            json!({}),
        );
        assert_eq!(reconciled["value"], json!({}));
        assert_eq!(
            dispatch(
                &mut browser,
                "_worker_attachment_retention_ready",
                json!({})
            )["value"]["ready"],
            true
        );
        dispatch(
            &mut browser,
            "_worker_attachment_retention_invalidate_reconciliation",
            json!({}),
        );
        assert_eq!(
            dispatch(
                &mut browser,
                "_worker_attachment_retention_ready",
                json!({})
            )["value"]["ready"],
            false
        );
    }

    #[test]
    fn preparation_pending_tracks_queued_and_exhausted_due_work() {
        let root = tempfile::tempdir().unwrap();
        let mut browser = BrowserCore::new(root.path().to_owned());
        let device_id = DeviceId::new();
        browser.client = Some(
            Client::open(
                ClientConfig {
                    database_path: root.path().join("client.db"),
                    vault_id: VaultId::new(),
                    device_id,
                },
                DatabaseKey::new(&[9; 32]).unwrap(),
            )
            .unwrap(),
        );
        let preparation_pending = |browser: &mut BrowserCore| {
            dispatch(
                browser,
                "_worker_attachment_retention_preparation_pending",
                json!({}),
            )["value"]["pending"]
                .as_bool()
                .unwrap()
        };
        assert!(!preparation_pending(&mut browser));

        dispatch(
            &mut browser,
            "_worker_attachment_retention_configure_protocol",
            json!({"enabled": true}),
        );
        let begun = dispatch(
            &mut browser,
            "_worker_attachment_retention_begin",
            json!({
                "attachmentId": AttachmentId::new(),
                "kind": "pending_send",
            }),
        )["value"]
            .clone();
        assert!(preparation_pending(&mut browser));
        dispatch(
            &mut browser,
            "_worker_attachment_retention_prepare",
            json!({"limit": 10}),
        );
        dispatch(
            &mut browser,
            "_worker_attachment_retention_apply_response",
            json!({
                "useId": begun["useId"],
                "attachmentId": begun["attachmentId"],
                "deviceId": begun["deviceId"],
                "kind": begun["kind"],
                "state": "registered",
            }),
        );
        let prepared = dispatch(
            &mut browser,
            "_worker_attachment_retention_prepare",
            json!({"limit": 10}),
        );
        assert_eq!(prepared["value"]["operations"], json!([]));
        assert!(!preparation_pending(&mut browser));
    }

    #[test]
    fn worker_rejects_future_notice_ack_and_response_identity_mismatch() {
        let root = tempfile::tempdir().unwrap();
        let mut browser = BrowserCore::new(root.path().to_owned());
        let device_id = DeviceId::new();
        browser.client = Some(
            Client::open(
                ClientConfig {
                    database_path: root.path().join("client.db"),
                    vault_id: VaultId::new(),
                    device_id,
                },
                DatabaseKey::new(&[8; 32]).unwrap(),
            )
            .unwrap(),
        );
        let future = dispatch(
            &mut browser,
            "_worker_attachment_retention_notice_acknowledgement_request",
            json!({"policyVersion": "hosted_attachment_retention_v2"}),
        );
        assert_eq!(future["error"]["code"], "invalid-request");

        browser
            .client()
            .unwrap()
            .configure_attachment_retention_protocol(true)
            .unwrap();
        let pending = browser
            .client()
            .unwrap()
            .begin_attachment_retention_use(
                AttachmentId::new(),
                peppy_client_core::AttachmentUseKind::PendingSend,
            )
            .unwrap();
        let mismatched = dispatch(
            &mut browser,
            "_worker_attachment_retention_apply_response",
            json!({
                "useId": pending.use_id,
                "attachmentId": AttachmentId::new(),
                "deviceId": pending.device_id,
                "kind": "pending_send",
                "state": "registered",
            }),
        );
        assert_eq!(mismatched["error"]["code"], "core");
    }

    #[test]
    fn worker_operation_request_and_response_use_generated_canonical_dtos() {
        let root = tempfile::tempdir().unwrap();
        let mut browser = BrowserCore::new(root.path().to_owned());
        let use_id = uuid::Uuid::new_v4();
        let attachment_id = AttachmentId::new();
        let device_id = DeviceId::new();
        let request = dispatch(
            &mut browser,
            "_worker_attachment_retention_operation_request",
            json!({
                "operation": "register",
                "useId": use_id,
                "attachmentId": attachment_id,
                "deviceId": device_id,
                "kind": "contact_photo",
            }),
        );
        assert_eq!(request["value"]["method"], "POST");
        assert_eq!(
            request["value"]["relativePath"],
            format!("/v1/attachments/{attachment_id}/retention-uses")
        );
        assert_eq!(request["value"]["expectedSuccessStatus"], 200);
        assert_eq!(
            request["value"]["protocolHeader"],
            json!({"name": "Peppy-Attachment-Retention-Protocol", "value": "1"})
        );
        let body: Value =
            serde_json::from_str(request["value"]["jsonBody"].as_str().unwrap()).unwrap();
        assert_eq!(body["use_id"], use_id.to_string());

        let release = dispatch(
            &mut browser,
            "_worker_attachment_retention_operation_request",
            json!({
                "operation": "release",
                "useId": use_id,
                "attachmentId": attachment_id,
                "deviceId": device_id,
                "kind": "contact_photo",
            }),
        );
        assert_eq!(release["value"]["method"], "POST");
        assert_eq!(
            release["value"]["relativePath"],
            format!("/v1/attachments/{attachment_id}/retention-uses/{use_id}/release")
        );
        assert_eq!(release["value"]["expectedSuccessStatus"], 200);
        let release_body: Value =
            serde_json::from_str(release["value"]["jsonBody"].as_str().unwrap()).unwrap();
        assert_eq!(release_body["use_id"], use_id.to_string());
        assert!(release_body.get("kind").is_none());

        let parsed = dispatch(
            &mut browser,
            "_worker_attachment_retention_parse_operation_response",
            json!({
                "body": json!({
                    "use_id": use_id,
                    "attachment_id": attachment_id,
                    "device_id": device_id,
                    "kind": "contact_photo",
                    "state": "registered",
                }).to_string(),
            }),
        );
        assert_eq!(parsed["value"]["useId"], use_id.to_string());
        assert_eq!(parsed["value"]["state"], "registered");
    }

    #[test]
    fn private_parse_status_dispatch_returns_the_generated_presentation_shape() {
        let root = tempfile::tempdir().unwrap();
        let mut browser = BrowserCore::new(root.path().to_owned());
        let parsed = dispatch(
            &mut browser,
            "_worker_attachment_retention_parse_status",
            json!({
                "body": json!({
                    "used_bytes": 12345,
                    "limit_bytes": 23456,
                    "base_bytes": 10000,
                    "pack_bytes": 2345,
                    "protected_bytes": 8765,
                    "removed_bytes": 1234,
                    "cleanup_state": "blocked",
                    "cleanup_unavailable_reason": "device_not_ready",
                    "notice": {
                        "state": "grace_period",
                        "policy_version": "hosted_attachment_retention_v1",
                        "presented_at": 1700000000,
                        "acknowledged_at": null,
                        "activation_at": 1702592000,
                    },
                }).to_string(),
            }),
        );
        assert_eq!(parsed["value"]["usedBytes"], 12345);
        assert_eq!(parsed["value"]["cleanupState"], "blocked");
        assert_eq!(
            parsed["value"]["notice"]["presentedAtSeconds"],
            1700000000_i64
        );
        assert_eq!(
            parsed["value"]["notice"]["activationAtSeconds"],
            1702592000_i64
        );
    }

    #[test]
    fn private_capability_request_dispatch_uses_the_canonical_contract() {
        let root = tempfile::tempdir().unwrap();
        let mut browser = BrowserCore::new(root.path().to_owned());
        let request = dispatch(
            &mut browser,
            "_worker_attachment_retention_capability_request",
            json!({"cleanupReady": false}),
        );
        assert_eq!(request["value"]["method"], "PUT");
        assert_eq!(
            request["value"]["relativePath"],
            "/v1/devices/self/retention-capability"
        );
        assert_eq!(request["value"]["expectedSuccessStatus"], 204);
        assert!(request["value"]["protocolHeader"].is_null());
        assert_eq!(
            serde_json::from_str::<Value>(request["value"]["jsonBody"].as_str().unwrap()).unwrap(),
            json!({"protocol_version": 1, "cleanup_ready": false})
        );

        let extra = dispatch(
            &mut browser,
            "_worker_attachment_retention_capability_request",
            json!({"cleanupReady": false, "extra": true}),
        );
        assert_eq!(extra["error"]["code"], "invalid-request");
    }

    #[test]
    fn private_status_request_uses_the_canonical_relative_route() {
        let root = tempfile::tempdir().unwrap();
        let mut browser = BrowserCore::new(root.path().to_owned());
        let request = dispatch(
            &mut browser,
            "_worker_attachment_retention_status_request",
            json!({}),
        );
        assert_eq!(request["value"]["method"], "GET");
        assert_eq!(
            request["value"]["relativePath"],
            "/v1/attachments/storage/status"
        );
        assert!(request["value"]["protocolHeader"].is_null());
        assert_eq!(request["value"]["expectedSuccessStatus"], 200);
    }
}
