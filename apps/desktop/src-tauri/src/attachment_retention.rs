//! Native transport for core-owned attachment-retention state.
use crate::{
    error::{core_error, BridgeError, BridgeResult},
    session::Session,
    sync::blocking,
};
use peppy_desktop_api::attachment_retention::{
    attachment_storage_status_dto,
    client_supported_attachment_retention_notice_acknowledgement_request_dto,
    AttachmentRetentionNoticeAcknowledgementInput, AttachmentRetentionNoticeAcknowledgementResult,
    AttachmentRetentionStatusResult,
};
use peppy_hosted_api::attachments::{
    AttachmentUseKind, RegisterAttachmentUseRequest, ReleaseAttachmentUseRequest,
    RetentionCapabilityRequest,
};
use peppy_hosted_client::attachment_retention::{
    self as codec, AttachmentRetentionErrorClassification, DeviceRetentionAvailability,
    MAX_ATTACHMENT_RETENTION_RESPONSE_JSON,
};
use std::sync::{atomic::Ordering, Arc};

const MAX_PREPARATION_PASSES: usize = 4;

fn operation_request(
    operation: peppy_client_core::PendingAttachmentUseOperation,
) -> BridgeResult<codec::AttachmentRetentionRequest> {
    let operation =
        peppy_desktop_api::attachment_retention::pending_attachment_use_operation_dto(operation);
    let attachment_id = operation
        .attachment_id
        .parse()
        .map_err(|_| BridgeError::host_state())?;
    let use_id = uuid::Uuid::parse_str(&operation.use_id).map_err(|_| BridgeError::host_state())?;
    let device_id =
        uuid::Uuid::parse_str(&operation.device_id).map_err(|_| BridgeError::host_state())?;
    match operation.operation {
        peppy_desktop_api::attachment_retention::AttachmentUseOperationKindDto::Register => {
            let kind = match operation.kind {
                peppy_desktop_api::attachment_retention::AttachmentUseKindDto::MessageHistory => {
                    AttachmentUseKind::MessageHistory
                }
                peppy_desktop_api::attachment_retention::AttachmentUseKindDto::PendingSend => {
                    AttachmentUseKind::PendingSend
                }
                peppy_desktop_api::attachment_retention::AttachmentUseKindDto::ContactPhoto => {
                    AttachmentUseKind::ContactPhoto
                }
                peppy_desktop_api::attachment_retention::AttachmentUseKindDto::RestoreCritical => {
                    AttachmentUseKind::RestoreCritical
                }
            };
            codec::build_register_attachment_use_request(RegisterAttachmentUseRequest {
                use_id,
                attachment_id,
                device_id,
                kind,
            })
        }
        peppy_desktop_api::attachment_retention::AttachmentUseOperationKindDto::Release => {
            codec::build_release_attachment_use_request(ReleaseAttachmentUseRequest {
                use_id,
                attachment_id,
                device_id,
            })
        }
    }
    .map_err(|_| BridgeError::host_state())
}

async fn execute(
    session: &Session,
    request: codec::AttachmentRetentionRequest,
) -> BridgeResult<(u16, String)> {
    let header = request
        .protocol_header
        .map(|header| (header.name, header.value));
    let (status, body) = session
        .api
        .retention_request(
            request.method,
            &request.relative_path,
            request.json_body.as_deref(),
            header,
            MAX_ATTACHMENT_RETENTION_RESPONSE_JSON + 1,
        )
        .await?;
    if status == 401 {
        return Err(BridgeError::new(
            "revoked",
            "The server rejected this device credential (revoked or invalid).",
        ));
    }
    if body.len() > MAX_ATTACHMENT_RETENTION_RESPONSE_JSON {
        return Err(BridgeError::new(
            "response-too-large",
            "A server response exceeded the native safety limit.",
        ));
    }
    String::from_utf8(body)
        .map(|body| (status, body))
        .map_err(|_| {
            BridgeError::new(
                "server-response",
                "The server returned an invalid response.",
            )
        })
}

fn active(session: &Session, generation: &str, epoch: u64) -> bool {
    !session.cancel.is_cancelled()
        && session.generation == generation
        && session.attachment_retention_epoch.load(Ordering::Acquire) == epoch
}

fn classify_capability_response(status: u16, body: &str) -> BridgeResult<bool> {
    match status {
        204 if body.is_empty() => Ok(true),
        404 | 405 => Ok(false),
        _ => Err(BridgeError::new(
            "server-response",
            "The server returned an invalid attachment-retention capability response.",
        )),
    }
}

async fn publish_capability(session: &Session, cleanup_ready: bool) -> BridgeResult<bool> {
    let request = codec::build_retention_capability_request(RetentionCapabilityRequest {
        protocol_version: 1,
        cleanup_ready,
    })
    .map_err(|_| BridgeError::host_state())?;
    let (status, body) = execute(session, request).await?;
    classify_capability_response(status, &body)
}

/// Negotiates before the normal sync worker can create or transmit producer work.
pub async fn negotiate(session: &Arc<Session>) -> BridgeResult<()> {
    let generation = session.generation.clone();
    let supported = publish_capability(session, false).await?;
    if session.cancel.is_cancelled() || session.generation != generation {
        return Err(BridgeError::new(
            "attachment-retention-stale",
            "Attachment retention negotiation will be retried for the active session.",
        ));
    }
    let client = session.client.clone();
    blocking(move || {
        client
            .configure_attachment_retention_protocol(supported)
            .map_err(core_error)
    })
    .await?;
    if session.cancel.is_cancelled() || session.generation != generation {
        return Err(BridgeError::new(
            "attachment-retention-stale",
            "Attachment retention negotiation will be retried for the active session.",
        ));
    }
    session
        .api
        .set_attachment_retention_protocol_enabled(supported);
    session
        .attachment_retention_ready_advertised
        .store(false, Ordering::Release);
    session
        .attachment_retention_supported
        .store(supported, Ordering::Release);
    session
        .attachment_retention_negotiated
        .store(true, Ordering::Release);
    if supported
        && session
            .attachment_retention_replay_observed
            .load(Ordering::Acquire)
    {
        if let Err(error) = reconcile_after_replay(session, &generation).await {
            if error.code == "revoked" {
                return Err(error);
            }
            session.set_status(|status| status.work_error = Some("attachment-retention-deferred"));
        }
    }
    Ok(())
}

pub fn ensure_negotiated(session: &Session) -> BridgeResult<()> {
    if session
        .attachment_retention_negotiated
        .load(Ordering::Acquire)
    {
        Ok(())
    } else {
        Err(BridgeError::new(
            "attachment-retention-negotiating",
            "Attachment storage is still preparing for this account. Try again shortly.",
        ))
    }
}

/// Core persists every preparation and response mutation; this loop only provides bounded I/O.
pub async fn drain_due_operations(session: &Arc<Session>, generation: &str) -> BridgeResult<()> {
    let _maintenance = session.attachment_retention_maintenance.lock().await;
    drain_due_operations_serial(session, generation, false).await
}

/// Reconciliation is only valid after replay has observed the current server state.
pub async fn reconcile_after_replay(session: &Arc<Session>, generation: &str) -> BridgeResult<()> {
    let _maintenance = session.attachment_retention_maintenance.lock().await;
    drain_due_operations_serial(session, generation, true).await
}

async fn drain_due_operations_serial(
    session: &Arc<Session>,
    generation: &str,
    reconcile: bool,
) -> BridgeResult<()> {
    let epoch = session.attachment_retention_epoch.load(Ordering::Acquire);
    if session
        .attachment_retention_ready_advertised
        .load(Ordering::Acquire)
    {
        let client = session.client.clone();
        let ready =
            blocking(move || client.attachment_retention_ready().map_err(core_error)).await?;
        if !ready && active(session, generation, epoch) {
            let published = publish_capability(session, false).await?;
            if !published {
                return Err(BridgeError::new(
                    "attachment-retention-deferred",
                    "Attachment retention readiness withdrawal will be retried.",
                ));
            }
            if !active(session, generation, epoch) {
                return Ok(());
            }
            session
                .attachment_retention_ready_advertised
                .store(false, Ordering::Release);
        }
    }
    for _ in 0..MAX_PREPARATION_PASSES {
        if !active(session, generation, epoch) {
            return Ok(());
        }
        let client = session.client.clone();
        let operations = blocking(move || {
            client
                .prepare_attachment_retention_operations(
                    peppy_client_core::MAX_PREPARED_ATTACHMENT_RETENTION_OPERATIONS,
                )
                .map_err(core_error)
        })
        .await?;
        if operations.is_empty() {
            let client = session.client.clone();
            let pending = blocking(move || {
                client
                    .attachment_retention_preparation_pending()
                    .map_err(core_error)
            })
            .await?;
            if !pending {
                break;
            }
            continue;
        }
        for operation in operations {
            if !active(session, generation, epoch) {
                return Ok(());
            }
            let attachment_id = operation.attachment_id;
            let request = operation_request(operation)?;
            let expected_success_status = request.expected_success_status;
            let (status, body) = execute(session, request).await?;
            if status != expected_success_status {
                if codec::classify_attachment_retention_error(status, &body)
                    == AttachmentRetentionErrorClassification::RemovedByStoragePolicy
                    && active(session, generation, epoch)
                {
                    crate::sync::record_storage_policy_removal(session, attachment_id)
                        .await
                        .map_err(|failure| match failure {
                            crate::sync::Failure::Net(error) => BridgeError::from(error),
                            crate::sync::Failure::Permanent(error) => error,
                        })?;
                    continue;
                }
                return Err(BridgeError::new(
                    "attachment-retention-deferred",
                    "Attachment retention reconciliation will be retried.",
                ));
            }
            let response = codec::parse_attachment_use_response(&body).map_err(|_| {
                BridgeError::new(
                    "server-response",
                    "The server returned an invalid response.",
                )
            })?;
            if !active(session, generation, epoch) {
                return Ok(());
            }
            let response =
                peppy_desktop_api::attachment_retention::attachment_use_response_dto(response);
            let response =
                peppy_desktop_api::attachment_retention::attachment_use_response(response)
                    .ok_or_else(BridgeError::host_state)?;
            let client = session.client.clone();
            blocking(move || {
                client
                    .apply_attachment_retention_response(&response)
                    .map_err(core_error)
            })
            .await?;
        }
    }
    let client = session.client.clone();
    let pending = blocking(move || {
        client
            .attachment_retention_preparation_pending()
            .map_err(core_error)
    })
    .await?;
    if pending {
        return Err(BridgeError::new(
            "attachment-retention-deferred",
            "Attachment retention has more due work to reconcile.",
        ));
    }
    if !reconcile || !active(session, generation, epoch) {
        return Ok(());
    }
    let client = session.client.clone();
    blocking(move || {
        client
            .mark_attachment_retention_reconciled()
            .map_err(core_error)
    })
    .await?;
    let client = session.client.clone();
    let ready = blocking(move || client.attachment_retention_ready().map_err(core_error)).await?;
    if ready
        && !session
            .attachment_retention_ready_advertised
            .load(Ordering::Acquire)
        && active(session, generation, epoch)
    {
        let published = publish_capability(session, true).await?;
        if !published {
            return Err(BridgeError::new(
                "attachment-retention-deferred",
                "Attachment retention readiness will be retried.",
            ));
        }
        if !active(session, generation, epoch) {
            return Ok(());
        }
        session
            .attachment_retention_ready_advertised
            .store(true, Ordering::Release);
    }
    Ok(())
}

pub async fn status(session: Arc<Session>) -> BridgeResult<AttachmentRetentionStatusResult> {
    let generation = session.generation.clone();
    let epoch = session.attachment_retention_epoch.load(Ordering::Acquire);
    let (status, body) = execute(
        &session,
        codec::build_device_attachment_storage_status_request(),
    )
    .await?;
    if !active(&session, &generation, epoch) {
        return Ok(AttachmentRetentionStatusResult::TemporarilyUnavailable);
    }
    if status != 200 {
        return Ok(
            match codec::classify_device_retention_availability(status, &body) {
                DeviceRetentionAvailability::Unsupported => {
                    AttachmentRetentionStatusResult::Unsupported
                }
                DeviceRetentionAvailability::TemporarilyUnavailable => {
                    AttachmentRetentionStatusResult::TemporarilyUnavailable
                }
                DeviceRetentionAvailability::Other => {
                    return Err(BridgeError::new(
                        "server-rejected",
                        "The server rejected the attachment storage status request.",
                    ));
                }
            },
        );
    }
    let status = codec::parse_attachment_storage_status_response(&body).map_err(|_| {
        BridgeError::new(
            "server-response",
            "The server returned an invalid response.",
        )
    })?;
    Ok(AttachmentRetentionStatusResult::Available {
        scope_id: generation,
        status: attachment_storage_status_dto(status),
    })
}

pub async fn acknowledge(
    session: Arc<Session>,
    input: AttachmentRetentionNoticeAcknowledgementInput,
) -> BridgeResult<AttachmentRetentionNoticeAcknowledgementResult> {
    let epoch = session.attachment_retention_epoch.load(Ordering::Acquire);
    if input.scope_id != session.generation || session.cancel.is_cancelled() {
        return Ok(AttachmentRetentionNoticeAcknowledgementResult::StaleScope);
    }
    if input.policy_version
        != peppy_hosted_api::attachments::CLIENT_SUPPORTED_ATTACHMENT_RETENTION_NOTICE_POLICY_VERSION
    {
        return Ok(AttachmentRetentionNoticeAcknowledgementResult::UnsupportedPolicyVersion);
    }
    let request = client_supported_attachment_retention_notice_acknowledgement_request_dto()
        .map_err(|_| BridgeError::host_state())?;
    let (status, body) = session
        .api
        .retention_request(
            &request.method,
            &request.relative_path,
            request.json_body.as_deref(),
            request
                .protocol_header
                .as_ref()
                .map(|header| (header.name.as_str(), header.value.as_str())),
            MAX_ATTACHMENT_RETENTION_RESPONSE_JSON + 1,
        )
        .await?;
    if status == 401 {
        return Err(BridgeError::new(
            "revoked",
            "The server rejected this device credential (revoked or invalid).",
        ));
    }
    if body.len() > MAX_ATTACHMENT_RETENTION_RESPONSE_JSON {
        return Err(BridgeError::new(
            "response-too-large",
            "A server response exceeded the native safety limit.",
        ));
    }
    let body = String::from_utf8(body).map_err(|_| {
        BridgeError::new(
            "server-response",
            "The server returned an invalid response.",
        )
    })?;
    if !active(&session, &input.scope_id, epoch) {
        return Ok(AttachmentRetentionNoticeAcknowledgementResult::StaleScope);
    }
    Ok(
        if status == request.expected_success_status && body.is_empty() {
            AttachmentRetentionNoticeAcknowledgementResult::Acknowledged
        } else {
            match codec::classify_device_retention_availability(status, &body) {
                DeviceRetentionAvailability::Unsupported => {
                    AttachmentRetentionNoticeAcknowledgementResult::Unsupported
                }
                DeviceRetentionAvailability::TemporarilyUnavailable => {
                    AttachmentRetentionNoticeAcknowledgementResult::TemporarilyUnavailable
                }
                DeviceRetentionAvailability::Other => {
                    return Err(BridgeError::new(
                        "server-rejected",
                        "The server rejected the notice acknowledgement.",
                    ));
                }
            }
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        contacts::tests::mock_server,
        tests::{fixture_at, open},
    };
    use std::{
        fs,
        sync::atomic::{AtomicUsize, Ordering},
        time::Duration,
    };

    #[test]
    fn capability_accepts_only_empty_204_and_old_server_fallbacks() {
        assert!(classify_capability_response(204, "").unwrap());
        assert!(!classify_capability_response(404, "{}").unwrap());
        assert!(!classify_capability_response(405, "").unwrap());
        assert!(classify_capability_response(204, "unexpected").is_err());
        assert!(classify_capability_response(500, "{}").is_err());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn old_server_fallback_completes_open_before_producers_can_run() {
        let (origin, seen) = mock_server(|method, path, _| {
            assert_eq!(
                (method, path),
                ("PUT", "/v1/devices/self/retention-capability")
            );
            (404, "{}".into())
        })
        .await;
        let fixture = fixture_at(&origin);
        let session = Arc::new(open(&fixture, &fixture.binding, &[]));

        negotiate(&session).await.unwrap();

        assert!(session
            .attachment_retention_negotiated
            .load(Ordering::Acquire));
        assert!(!session
            .client
            .attachment_retention_protocol_enabled()
            .unwrap());
        assert_eq!(seen.lock().unwrap().len(), 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn start_retries_a_transient_capability_failure_before_opening_producers() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let route_attempts = attempts.clone();
        let (origin, _seen) = mock_server(move |method, path, _| {
            assert_eq!(
                (method, path),
                ("PUT", "/v1/devices/self/retention-capability")
            );
            if route_attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                (503, "{}".into())
            } else {
                (204, String::new())
            }
        })
        .await;
        let fixture = fixture_at(&origin);
        let session = Arc::new(open(&fixture, &fixture.binding, &[]));

        crate::sync::start(&session);

        tokio::time::timeout(Duration::from_secs(3), async {
            while !session
                .attachment_retention_negotiated
                .load(Ordering::Acquire)
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("capability negotiation should retry");
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
        assert!(session
            .attachment_retention_supported
            .load(Ordering::Acquire));
        assert!(crate::sync::drain(&session, Duration::from_secs(1)).await);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn start_revokes_the_session_when_capability_negotiation_is_unauthorized() {
        let (origin, _seen) = mock_server(|method, path, _| {
            assert_eq!(
                (method, path),
                ("PUT", "/v1/devices/self/retention-capability")
            );
            (401, "{}".into())
        })
        .await;
        let fixture = fixture_at(&origin);
        let session = Arc::new(open(&fixture, &fixture.binding, &[]));

        crate::sync::start(&session);

        tokio::time::timeout(Duration::from_secs(1), session.cancel.cancelled())
            .await
            .expect("unauthorized capability negotiation should revoke the session");
        assert!(session.status.lock().unwrap().revoked);
        assert!(crate::sync::drain(&session, Duration::from_secs(1)).await);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn stale_notice_acknowledgement_never_reaches_the_transport() {
        let (origin, seen) = mock_server(|_, _, _| (500, "unexpected".into())).await;
        let fixture = fixture_at(&origin);
        let session = Arc::new(open(&fixture, &fixture.binding, &[]));

        let result = acknowledge(
            session,
            AttachmentRetentionNoticeAcknowledgementInput {
                scope_id: "different-open".into(),
                policy_version:
                    peppy_hosted_api::attachments::CLIENT_SUPPORTED_ATTACHMENT_RETENTION_NOTICE_POLICY_VERSION.into(),
            },
        )
        .await
        .unwrap();

        assert_eq!(
            result,
            AttachmentRetentionNoticeAcknowledgementResult::StaleScope
        );
        assert!(seen.lock().unwrap().is_empty());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn readiness_is_published_once_and_only_after_replay_reconciliation() {
        let (origin, seen) = mock_server(|method, path, body| {
            assert_eq!(
                (method, path),
                ("PUT", "/v1/devices/self/retention-capability")
            );
            assert!(body.contains("cleanup_ready"));
            (204, String::new())
        })
        .await;
        let fixture = fixture_at(&origin);
        let session = Arc::new(open(&fixture, &fixture.binding, &[]));
        negotiate(&session).await.unwrap();

        drain_due_operations(&session, &session.generation)
            .await
            .unwrap();
        assert_eq!(seen.lock().unwrap().len(), 1);

        reconcile_after_replay(&session, &session.generation)
            .await
            .unwrap();
        reconcile_after_replay(&session, &session.generation)
            .await
            .unwrap();

        let requests = seen.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert!(requests[1].body.contains("cleanup_ready\":true"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn invalidated_readiness_is_withdrawn_before_a_later_replay_republishes_it() {
        let (origin, seen) = mock_server(|method, path, _| {
            assert_eq!(
                (method, path),
                ("PUT", "/v1/devices/self/retention-capability")
            );
            (204, String::new())
        })
        .await;
        let fixture = fixture_at(&origin);
        let session = Arc::new(open(&fixture, &fixture.binding, &[]));
        negotiate(&session).await.unwrap();
        reconcile_after_replay(&session, &session.generation)
            .await
            .unwrap();
        session
            .client
            .invalidate_attachment_retention_reconciliation()
            .unwrap();

        drain_due_operations(&session, &session.generation)
            .await
            .unwrap();
        reconcile_after_replay(&session, &session.generation)
            .await
            .unwrap();

        let requests = seen.lock().unwrap();
        assert_eq!(requests.len(), 4);
        assert!(requests[1].body.contains("cleanup_ready\":true"));
        assert!(requests[2].body.contains("cleanup_ready\":false"));
        assert!(requests[3].body.contains("cleanup_ready\":true"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn maintenance_drains_more_than_one_preparation_batch_before_producers_continue() {
        let (origin, seen) = mock_server(|method, path, body| {
            if (method, path) == ("PUT", "/v1/devices/self/retention-capability") {
                return (204, String::new());
            }
            assert_eq!(method, "POST");
            assert!(path.starts_with("/v1/attachments/"));
            let request: serde_json::Value = serde_json::from_str(body).unwrap();
            (
                200,
                serde_json::json!({
                    "use_id": request["use_id"],
                    "attachment_id": request["attachment_id"],
                    "device_id": request["device_id"],
                    "kind": request["kind"],
                    "state": "registered",
                })
                .to_string(),
            )
        })
        .await;
        let fixture = fixture_at(&origin);
        let session = Arc::new(open(&fixture, &fixture.binding, &[]));
        negotiate(&session).await.unwrap();
        let count = peppy_client_core::MAX_PREPARED_ATTACHMENT_RETENTION_OPERATIONS + 1;
        for index in 0..count {
            let source = fixture.dir.path().join(format!("retention-{index}.txt"));
            fs::write(&source, b"attachment").unwrap();
            let attachment = session
                .client
                .prepare_attachment(&source, "text/plain", "retention.txt")
                .unwrap();
            session
                .client
                .begin_attachment_retention_use(
                    attachment.attachment_id,
                    peppy_client_core::AttachmentUseKind::PendingSend,
                )
                .unwrap();
        }

        drain_due_operations(&session, &session.generation)
            .await
            .unwrap();

        assert!(session
            .client
            .pending_attachment_retention_operations()
            .unwrap()
            .is_empty());
        assert_eq!(seen.lock().unwrap().len(), count + 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn work_round_defers_more_than_the_preparation_budget_without_reserving_uploads() {
        let (origin, seen) = mock_server(|method, path, body| {
            if (method, path) == ("PUT", "/v1/devices/self/retention-capability") {
                return (204, String::new());
            }
            assert_eq!(method, "POST");
            assert!(path.starts_with("/v1/attachments/"));
            assert_ne!(path, "/v1/attachments/reserve");
            let request: serde_json::Value = serde_json::from_str(body).unwrap();
            (
                200,
                serde_json::json!({
                    "use_id": request["use_id"],
                    "attachment_id": request["attachment_id"],
                    "device_id": request["device_id"],
                    "kind": request["kind"],
                    "state": "registered",
                })
                .to_string(),
            )
        })
        .await;
        let fixture = fixture_at(&origin);
        let session = Arc::new(open(&fixture, &fixture.binding, &[]));
        negotiate(&session).await.unwrap();
        let count = MAX_PREPARATION_PASSES
            * peppy_client_core::MAX_PREPARED_ATTACHMENT_RETENTION_OPERATIONS
            + 1;
        for index in 0..count {
            let source = fixture.dir.path().join(format!("budget-{index}.txt"));
            fs::write(&source, b"attachment").unwrap();
            let attachment = session
                .client
                .prepare_attachment(&source, "text/plain", "budget.txt")
                .unwrap();
            session
                .client
                .begin_attachment_retention_use(
                    attachment.attachment_id,
                    peppy_client_core::AttachmentUseKind::PendingSend,
                )
                .unwrap();
        }

        crate::sync::work_round(&session).await.unwrap();

        assert_eq!(
            session
                .client
                .pending_attachment_retention_operations()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            session.status.lock().unwrap().work_error,
            Some("attachment-retention-deferred")
        );
        assert!(seen
            .lock()
            .unwrap()
            .iter()
            .all(|request| request.path != "/v1/attachments/reserve"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn work_round_registers_new_use_and_restores_readiness_after_replay() {
        let (origin, seen) = mock_server(|method, path, body| {
            if (method, path) == ("PUT", "/v1/devices/self/retention-capability") {
                return (204, String::new());
            }
            assert_eq!(method, "POST");
            assert!(path.starts_with("/v1/attachments/"));
            let request: serde_json::Value = serde_json::from_str(body).unwrap();
            (
                200,
                serde_json::json!({
                    "use_id": request["use_id"],
                    "attachment_id": request["attachment_id"],
                    "device_id": request["device_id"],
                    "kind": request["kind"],
                    "state": "registered",
                })
                .to_string(),
            )
        })
        .await;
        let fixture = fixture_at(&origin);
        let session = Arc::new(open(&fixture, &fixture.binding, &[]));
        negotiate(&session).await.unwrap();
        session
            .attachment_retention_replay_observed
            .store(true, Ordering::Release);
        reconcile_after_replay(&session, &session.generation)
            .await
            .unwrap();
        let source = fixture.dir.path().join("post-replay-use.txt");
        fs::write(&source, b"attachment").unwrap();
        let attachment = session
            .client
            .prepare_attachment(&source, "text/plain", "post-replay-use.txt")
            .unwrap();
        session
            .client
            .begin_attachment_retention_use(
                attachment.attachment_id,
                peppy_client_core::AttachmentUseKind::PendingSend,
            )
            .unwrap();

        crate::sync::work_round(&session).await.unwrap();

        assert!(session
            .client
            .pending_attachment_retention_operations()
            .unwrap()
            .is_empty());
        assert!(session
            .attachment_retention_ready_advertised
            .load(Ordering::Acquire));
        let requests = seen.lock().unwrap();
        let readiness = requests
            .iter()
            .filter(|request| request.path == "/v1/devices/self/retention-capability")
            .map(|request| request.body.clone())
            .collect::<Vec<_>>();
        assert_eq!(readiness.len(), 4);
        assert!(readiness[0].contains("cleanup_ready\":false"));
        assert!(readiness[1].contains("cleanup_ready\":true"));
        assert!(readiness[2].contains("cleanup_ready\":false"));
        assert!(readiness[3].contains("cleanup_ready\":true"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn restore_epoch_discards_a_held_pre_restore_response_and_allows_a_later_pass() {
        let (entered_sender, entered) = std::sync::mpsc::channel();
        let (release_sender, release) = std::sync::mpsc::channel();
        let release = Arc::new(std::sync::Mutex::new(Some(release)));
        let first_response = Arc::new(AtomicUsize::new(0));
        let route_release = release.clone();
        let route_first_response = first_response.clone();
        let (origin, _seen) = mock_server(move |method, path, body| {
            if (method, path) == ("PUT", "/v1/devices/self/retention-capability") {
                return (204, String::new());
            }
            if route_first_response.fetch_add(1, Ordering::SeqCst) == 0 {
                entered_sender.send(()).unwrap();
                route_release
                    .lock()
                    .unwrap()
                    .take()
                    .unwrap()
                    .recv()
                    .unwrap();
            }
            let request: serde_json::Value = serde_json::from_str(body).unwrap();
            (
                200,
                serde_json::json!({
                    "use_id": request["use_id"],
                    "attachment_id": request["attachment_id"],
                    "device_id": request["device_id"],
                    "kind": request["kind"],
                    "state": "registered",
                })
                .to_string(),
            )
        })
        .await;
        let fixture = fixture_at(&origin);
        let session = Arc::new(open(&fixture, &fixture.binding, &[]));
        negotiate(&session).await.unwrap();
        let source = fixture.dir.path().join("held-response.txt");
        fs::write(&source, b"attachment").unwrap();
        let attachment = session
            .client
            .prepare_attachment(&source, "text/plain", "held-response.txt")
            .unwrap();
        session
            .client
            .begin_attachment_retention_use(
                attachment.attachment_id,
                peppy_client_core::AttachmentUseKind::PendingSend,
            )
            .unwrap();

        let draining = tokio::spawn({
            let session = session.clone();
            async move { drain_due_operations(&session, &session.generation).await }
        });
        tokio::time::timeout(Duration::from_secs(1), async {
            while entered.try_recv().is_err() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("retention request should be held");
        session.begin_attachment_retention_restore();
        release_sender.send(()).unwrap();
        draining.await.unwrap().unwrap();

        assert_eq!(
            session
                .client
                .pending_attachment_retention_operations()
                .unwrap()
                .len(),
            1,
            "a response from the pre-restore epoch must not be applied"
        );
        assert!(!session
            .attachment_retention_ready_advertised
            .load(Ordering::Acquire));

        drain_due_operations(&session, &session.generation)
            .await
            .unwrap();
        assert!(session
            .client
            .pending_attachment_retention_operations()
            .unwrap()
            .is_empty());
        reconcile_after_replay(&session, &session.generation)
            .await
            .unwrap();
        assert!(session
            .attachment_retention_ready_advertised
            .load(Ordering::Acquire));
    }
}
