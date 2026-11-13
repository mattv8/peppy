use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    routing::{get, post},
};
use futures_util::StreamExt;
use peppy_hosted_api::mcp::{
    MCP_WORKER_COMPLETE_TIMEOUT_MS, McpDeviceState, McpDeviceStatus, McpDeviceView,
    McpEnrollmentPreparation, McpGrantView, McpWorkerActivateRequest, McpWorkerActivateResponse,
    McpWorkerCompleteRequest, McpWorkerPrepareRequest, McpWorkerPrepareResponse,
    McpWorkerStatusRequest, McpWorkerStatusResponse,
};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use sqlx::Row;
use std::time::Duration;
use uuid::Uuid;

use super::{
    ApiError, ApiResult, ApiState, EnrollmentAuthorizationRequest, EnrollmentOperation,
    McpWorkerClient, Operation, Principal, authenticated, database_unavailable, owner,
    revalidate_principal, scope, unique_conflict,
};

pub(super) fn spawn_cleanup_maintenance(pool: sqlx::PgPool, worker: Option<McpWorkerClient>) {
    let Some(worker) = worker else {
        return;
    };
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(30));
        loop {
            interval.tick().await;
            if cleanup_pending_once(&pool, &worker).await.is_err() {
                tracing::warn!(
                    category = "mcp_cleanup",
                    code = "mcp_cleanup_database",
                    "MCP cleanup maintenance failed"
                );
            }
        }
    });
}

pub(super) fn spawn_oauth_maintenance(pool: sqlx::PgPool) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(3_600));
        loop {
            interval.tick().await;
            if sqlx::query("SELECT mcp_sweep_oauth()")
                .execute(&pool)
                .await
                .is_err()
            {
                tracing::warn!(
                    category = "mcp_oauth",
                    code = "mcp_oauth_sweep_database",
                    "MCP OAuth retention sweep failed"
                );
            }
        }
    });
}

pub(super) async fn cleanup_pending_once(
    pool: &sqlx::PgPool,
    worker: &McpWorkerClient,
) -> Result<(), sqlx::Error> {
    let candidates = sqlx::query(
        "SELECT id,vault_id FROM mcp_operations WHERE (state IN ('prepared','activated') AND expires_at <= clock_timestamp()) OR cleanup_pending",
    )
    .fetch_all(pool)
    .await?;
    for candidate in candidates {
        cleanup_operation(pool, worker, candidate.get("vault_id"), candidate.get("id")).await?;
    }
    let devices = sqlx::query("SELECT vault_id,device_id FROM mcp_devices WHERE cleanup_pending=TRUE ORDER BY cleanup_last_attempt_at ASC NULLS FIRST LIMIT 32")
        .fetch_all(pool).await?;
    for device in devices {
        let vault_id: Uuid = device.get("vault_id");
        let device_id: Uuid = device.get("device_id");
        let mut tx = scope::begin(pool, vault_id).await?;
        mcp_enable_lock(&mut tx, vault_id).await?;
        sqlx::query("UPDATE mcp_devices SET cleanup_attempts=cleanup_attempts+1,cleanup_last_attempt_at=clock_timestamp() WHERE vault_id=$1 AND device_id=$2 AND cleanup_pending=TRUE")
            .bind(vault_id)
            .bind(device_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        if !revoke_worker_device(worker, vault_id, device_id).await {
            tracing::warn!(
                category = "mcp_cleanup",
                code = "mcp_worker_revoke_failed",
                "MCP device cleanup will retry"
            );
            continue;
        }
        let mut tx = scope::begin(pool, vault_id).await?;
        mcp_enable_lock(&mut tx, vault_id).await?;
        sqlx::query("UPDATE mcp_devices SET cleanup_pending=FALSE WHERE vault_id=$1 AND device_id=$2 AND cleanup_pending=TRUE")
            .bind(vault_id)
            .bind(device_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
    }
    let jobs = sqlx::query("SELECT vault_id,worker_device_id FROM mcp_cleanup_jobs ORDER BY last_attempt_at ASC NULLS FIRST LIMIT 32")
        .fetch_all(pool)
        .await?;
    for job in jobs {
        let vault_id: Uuid = job.get("vault_id");
        let device_id: Uuid = job.get("worker_device_id");
        // Keep deletion cleanup serialized with an in-flight completion for this vault. The
        // operation may already have cascaded, so this deliberately uses an unscoped transaction.
        let mut tx = pool.begin().await?;
        mcp_enable_lock(&mut tx, vault_id).await?;
        sqlx::query("UPDATE mcp_cleanup_jobs SET attempts=attempts+1,last_attempt_at=clock_timestamp() WHERE vault_id=$1 AND worker_device_id=$2")
            .bind(vault_id)
            .bind(device_id)
            .execute(&mut *tx)
            .await?;
        if !revoke_worker_device(worker, vault_id, device_id).await {
            tracing::warn!(
                category = "mcp_cleanup",
                code = "mcp_worker_revoke_failed",
                "MCP deletion cleanup will retry"
            );
            tx.commit().await?;
            continue;
        }
        sqlx::query("DELETE FROM mcp_cleanup_jobs WHERE vault_id=$1 AND worker_device_id=$2")
            .bind(vault_id)
            .bind(device_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
    }
    Ok(())
}

async fn cleanup_operation(
    pool: &sqlx::PgPool,
    worker: &McpWorkerClient,
    vault_id: Uuid,
    operation_id: Uuid,
) -> Result<(), sqlx::Error> {
    let mut tx = scope::begin(pool, vault_id).await?;
    mcp_enable_lock(&mut tx, vault_id).await?;
    let Some(operation) = sqlx::query(
        "SELECT owner_device_id,worker_device_id,state FROM mcp_operations WHERE id=$1 AND vault_id=$2 AND ((state IN ('prepared','activated') AND expires_at <= clock_timestamp()) OR cleanup_pending) FOR UPDATE",
    )
    .bind(operation_id)
    .bind(vault_id)
    .fetch_optional(&mut *tx)
    .await?
    else {
        tx.commit().await?;
        return Ok(());
    };
    let owner_device_id: Uuid = operation.get("owner_device_id");
    let worker_device_id: Uuid = operation.get("worker_device_id");
    // Only a prepared claimant is known to hold no installed device. Activated operations, and
    // operations revoked earlier with cleanup pending, may already have installed it.
    let may_be_installed = operation.get::<String, _>("state") != "prepared";
    sqlx::query("UPDATE mcp_operations SET state='revoked',cleanup_pending=TRUE WHERE id=$1 AND vault_id=$2")
        .bind(operation_id)
        .bind(vault_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;

    let cancelled = cancel_worker_claimant(worker, vault_id, owner_device_id, operation_id).await;
    let revoked =
        !may_be_installed || revoke_worker_device(worker, vault_id, worker_device_id).await;
    if !(cancelled && revoked) {
        tracing::warn!(
            category = "mcp_cleanup",
            code = "mcp_worker_cleanup_failed",
            "MCP operation cleanup will retry"
        );
        return Ok(());
    }
    let mut tx = scope::begin(pool, vault_id).await?;
    mcp_enable_lock(&mut tx, vault_id).await?;
    sqlx::query("UPDATE mcp_operations SET cleanup_pending=FALSE WHERE id=$1 AND vault_id=$2 AND state='revoked'")
        .bind(operation_id)
        .bind(vault_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

async fn cancel_worker_claimant(
    worker: &McpWorkerClient,
    vault_id: Uuid,
    owner_device_id: Uuid,
    invitation_id: Uuid,
) -> bool {
    let body = serde_json::to_value(peppy_hosted_api::mcp::McpWorkerCancelRequest {
        vault_id,
        owner_device_id,
        invitation_id,
    })
    .expect("DTO serializes");
    worker_request_with(worker, "/internal/mcp/cancel", &body)
        .await
        .is_ok()
}

async fn revoke_worker_device(worker: &McpWorkerClient, vault_id: Uuid, device_id: Uuid) -> bool {
    let body = serde_json::to_value(peppy_hosted_api::mcp::McpWorkerRevokeRequest {
        vault_id,
        device_id,
    })
    .expect("DTO serializes");
    worker_request_with(worker, "/internal/mcp/revoke", &body)
        .await
        .is_ok()
}

mod oauth;
mod protocol;
mod webhooks;

pub(super) fn routes() -> Router<ApiState> {
    Router::new()
        .route("/v1/mcp/device", get(status))
        .route("/v1/mcp/device/prepare", post(prepare))
        .route("/v1/mcp/device/activate", post(activate))
        .route("/v1/mcp/device/complete", post(complete))
        .route("/v1/mcp/grants", get(grants))
        .route("/v1/mcp/grants/{grant_id}/revoke", post(revoke_grant))
        .merge(webhooks::routes())
        .merge(oauth::routes())
        .merge(protocol::routes())
}

async fn status(
    State(state): State<ApiState>,
    headers: HeaderMap,
) -> ApiResult<Json<McpDeviceStatus>> {
    let principal = authenticated(&state, &headers, Operation::Read).await?;
    let mut tx = scope::begin(&state.db, principal.vault)
        .await
        .map_err(|error| database_unavailable(&error, "mcp_status_begin"))?;
    let row = sqlx::query(
        "SELECT d.device_id,d.name,d.revoked_at IS NOT NULL revoked,m.cleanup_pending \
         FROM mcp_devices m JOIN devices d ON d.vault_id=m.vault_id AND d.device_id=m.device_id \
         WHERE m.vault_id=$1 ORDER BY m.created_at DESC LIMIT 1",
    )
    .bind(principal.vault)
    .fetch_optional(&mut *tx)
    .await
    .map_err(|error| database_unavailable(&error, "mcp_status_query"))?;
    tx.commit()
        .await
        .map_err(|error| database_unavailable(&error, "mcp_status_commit"))?;

    let worker_state = match (state.mcp_worker.as_ref(), row.as_ref()) {
        (Some(_), Some(row)) => worker_request(
            &state,
            "/internal/mcp/status",
            &serde_json::to_value(McpWorkerStatusRequest {
                vault_id: principal.vault,
                device_id: row.get("device_id"),
            })
            .expect("DTO serializes"),
        )
        .await
        .ok()
        .and_then(|value| serde_json::from_value::<McpWorkerStatusResponse>(value).ok())
        .filter(|response| {
            response.vault_id == principal.vault
                && response.device_id == row.get::<Uuid, _>("device_id")
        }),
        _ => None,
    };
    let device = row.map(|row| McpDeviceView {
        device_id: row.get("device_id"),
        name: row.get("name"),
        state: if row.get("revoked") {
            McpDeviceState::Revoked
        } else {
            worker_state
                .as_ref()
                .map(|response| response.state)
                .unwrap_or(McpDeviceState::Offline)
        },
        last_synced_at: worker_state
            .as_ref()
            .and_then(|response| response.last_synced_at.clone()),
        cleanup_pending: row.get("cleanup_pending"),
    });
    Ok(Json(McpDeviceStatus {
        vault_id: principal.vault,
        available: mcp_available(&state),
        server_origin: state
            .instance_origin
            .as_ref()
            .map(|origin| origin.origin().ascii_serialization())
            .unwrap_or_default(),
        hosted: state.row_security,
        device,
    }))
}

async fn grants(
    State(state): State<ApiState>,
    headers: HeaderMap,
) -> ApiResult<Json<Vec<McpGrantView>>> {
    let principal = authenticated(&state, &headers, Operation::Read).await?;
    owner(&principal)?;
    let mut tx = scope::begin(&state.db, principal.vault)
        .await
        .map_err(|error| database_unavailable(&error, "mcp_grants_begin"))?;
    let rows = sqlx::query(
        "SELECT id,client_name,scopes,\
         to_char(created_at AT TIME ZONE 'UTC','YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"') created_at,\
         to_char(last_used_at AT TIME ZONE 'UTC','YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"') last_used_at \
         FROM mcp_grants WHERE vault_id=$1 AND revoked_at IS NULL ORDER BY created_at DESC",
    )
    .bind(principal.vault)
    .fetch_all(&mut *tx)
    .await
    .map_err(|error| database_unavailable(&error, "mcp_grants_query"))?;
    tx.commit()
        .await
        .map_err(|error| database_unavailable(&error, "mcp_grants_commit"))?;
    let grants = rows
        .into_iter()
        .map(|row| {
            let scopes = row
                .get::<Vec<String>, _>("scopes")
                .into_iter()
                .map(|scope| serde_json::from_value(serde_json::Value::String(scope)))
                .collect::<Result<_, _>>()
                .map_err(|_| ApiError(StatusCode::INTERNAL_SERVER_ERROR, "mcp_grant_invalid"))?;
            Ok(McpGrantView {
                id: row.get("id"),
                client_name: row.get("client_name"),
                scopes,
                created_at: row.get("created_at"),
                last_used_at: row.get("last_used_at"),
            })
        })
        .collect::<ApiResult<Vec<_>>>()?;
    Ok(Json(grants))
}

async fn revoke_grant(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Path(grant_id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    let principal = authenticated(&state, &headers, Operation::Revoke).await?;
    owner(&principal)?;
    let mut tx =
        mcp_advisory_locked_transaction(&state, &principal, "mcp_grant_revoke_begin").await?;
    lock_vault_row(&mut tx, principal.vault, "mcp_grant_revoke_vault_lock").await?;
    require_current_owner(&mut tx, &principal).await?;
    let result = sqlx::query(
        "UPDATE mcp_grants SET revoked_at=now() WHERE id=$1 AND vault_id=$2 AND revoked_at IS NULL",
    )
    .bind(grant_id)
    .bind(principal.vault)
    .execute(&mut *tx)
    .await
    .map_err(|error| database_unavailable(&error, "mcp_grant_revoke"))?;
    if result.rows_affected() != 1 {
        return Err(ApiError(StatusCode::NOT_FOUND, "mcp_grant_not_found"));
    }
    tx.commit()
        .await
        .map_err(|error| database_unavailable(&error, "mcp_grant_revoke_commit"))?;
    Ok(StatusCode::NO_CONTENT)
}

async fn prepare(
    State(state): State<ApiState>,
    headers: HeaderMap,
) -> ApiResult<Json<McpEnrollmentPreparation>> {
    let principal = authenticated(&state, &headers, Operation::Revoke).await?;
    owner(&principal)?;
    authorize_custody(
        &state,
        &headers,
        &principal,
        EnrollmentOperation::CreateComputerEnrollment,
    )
    .await?;
    let mut tx = mcp_advisory_locked_transaction(&state, &principal, "mcp_prepare_lock").await?;
    require_current_owner(&mut tx, &principal).await?;
    let live_device: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM mcp_devices m JOIN devices d ON d.vault_id=m.vault_id AND d.device_id=m.device_id WHERE m.vault_id=$1 AND m.cleanup_pending=FALSE AND d.revoked_at IS NULL)",
    )
    .bind(principal.vault)
    .fetch_one(&mut *tx)
    .await
    .map_err(|error| database_unavailable(&error, "mcp_prepare_device"))?;
    if live_device {
        return Err(ApiError(StatusCode::CONFLICT, "mcp_device_conflict"));
    }
    let active: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM mcp_operations WHERE vault_id=$1 AND owner_device_id=$2 AND state='activated' AND expires_at > clock_timestamp())",
    )
    .bind(principal.vault)
    .bind(principal.device)
    .fetch_one(&mut *tx)
    .await
    .map_err(|error| database_unavailable(&error, "mcp_prepare_active"))?;
    if active {
        return Err(ApiError(StatusCode::CONFLICT, "mcp_enable_in_progress"));
    }
    let response = worker_request(
        &state,
        "/internal/mcp/prepare",
        &serde_json::to_value(McpWorkerPrepareRequest {
            vault_id: principal.vault,
            owner_device_id: principal.device,
        })
        .expect("DTO serializes"),
    )
    .await?;
    let prepared: McpWorkerPrepareResponse = serde_json::from_value(response)
        .map_err(|_| ApiError(StatusCode::BAD_GATEWAY, "mcp_worker_invalid"))?;
    let device_id = prepared.pairing.recipient.device.device_id;
    // Operation rows are written only after the worker call, and only once the vault row is locked:
    // vault deletion locks the vault row and then cascades into these rows, so no operation row may
    // be locked while this transaction still waits for the vault row.
    if let Err(rejection) = confirm_owner_under_vault_lock(&mut tx, &principal).await {
        cancel_new_claimant(&state, &principal, prepared.operation_id).await;
        return Err(rejection);
    }
    let superseded = sqlx::query(
        "UPDATE mcp_operations SET state='revoked',cleanup_pending=(state='activated') WHERE vault_id=$1 AND owner_device_id=$2 AND state IN ('prepared','activated') AND (state='prepared' OR expires_at <= clock_timestamp()) RETURNING id",
    )
    .bind(principal.vault)
    .bind(principal.device)
    .fetch_all(&mut *tx)
    .await
    .map_err(|error| database_unavailable(&error, "mcp_prepare_supersede"))?;
    let result = sqlx::query("INSERT INTO mcp_operations(id,vault_id,owner_device_id,worker_device_id,state) VALUES($1,$2,$3,$4,'prepared')")
        .bind(prepared.operation_id).bind(principal.vault).bind(principal.device).bind(device_id).execute(&mut *tx).await
        .map_err(|error| unique_conflict(error, "mcp_enable_in_progress", "mcp_prepare_insert"));
    if let Err(error) = result {
        cancel_new_claimant(&state, &principal, prepared.operation_id).await;
        return Err(error);
    }
    tx.commit()
        .await
        .map_err(|error| database_unavailable(&error, "mcp_prepare_commit"))?;
    if let Some(worker) = &state.mcp_worker {
        for operation in superseded {
            let _ = cancel_worker_claimant(
                worker,
                principal.vault,
                principal.device,
                operation.get("id"),
            )
            .await;
        }
    }
    Ok(Json(McpEnrollmentPreparation {
        operation_id: prepared.operation_id,
        pairing: prepared.pairing,
    }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HostActivation {
    operation_id: Uuid,
    status: peppy_hosted_api::computer_enrollment::ComputerEnrollmentStatus,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HostCompletion {
    operation_id: Uuid,
    admission: peppy_hosted_api::enrollment::AdmissionComplete,
}

fn request_fingerprint<T: serde::Serialize>(request: &T) -> ApiResult<Vec<u8>> {
    let canonical = serde_json::to_vec(request)
        .map_err(|_| ApiError(StatusCode::BAD_REQUEST, "mcp_request_invalid"))?;
    Ok(Sha256::digest(canonical).to_vec())
}

async fn activate(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Json(request): Json<HostActivation>,
) -> ApiResult<Json<McpWorkerActivateResponse>> {
    let principal = authenticated(&state, &headers, Operation::Revoke).await?;
    owner(&principal)?;
    authorize_custody(
        &state,
        &headers,
        &principal,
        EnrollmentOperation::ApproveComputerEnrollment,
    )
    .await?;
    let fingerprint = request_fingerprint(&request.status)?;
    let mut tx = mcp_advisory_locked_transaction(&state, &principal, "mcp_activate_begin").await?;
    require_current_owner(&mut tx, &principal).await?;
    let operation = sqlx::query("SELECT worker_device_id,state,activation_fingerprint,expires_at <= clock_timestamp() expired FROM mcp_operations WHERE id=$1 AND vault_id=$2 AND owner_device_id=$3")
        .bind(request.operation_id).bind(principal.vault).bind(principal.device).fetch_optional(&mut *tx).await
        .map_err(|error| database_unavailable(&error, "mcp_activate_lookup"))?.ok_or(ApiError(StatusCode::NOT_FOUND, "mcp_operation_not_found"))?;
    let operation_state: String = operation.get("state");
    let expired: bool = operation.get("expired");
    if operation_state != "completed" && expired {
        return Err(ApiError(StatusCode::CONFLICT, "mcp_operation_invalid"));
    }
    if operation_state != "prepared"
        && (operation_state != "activated"
            || operation
                .get::<Option<Vec<u8>>, _>("activation_fingerprint")
                .as_deref()
                != Some(fingerprint.as_slice()))
    {
        return Err(ApiError(StatusCode::CONFLICT, "mcp_operation_invalid"));
    }
    let worker_device_id: Uuid = operation.get("worker_device_id");
    let response = worker_request(
        &state,
        "/internal/mcp/activate",
        &serde_json::to_value(McpWorkerActivateRequest {
            vault_id: principal.vault,
            operation_id: request.operation_id,
            status: request.status,
        })
        .map_err(|_| ApiError(StatusCode::BAD_REQUEST, "mcp_activation_invalid"))?,
    )
    .await?;
    let response: McpWorkerActivateResponse = serde_json::from_value(response)
        .map_err(|_| ApiError(StatusCode::BAD_GATEWAY, "mcp_worker_invalid"))?;
    // Finalize in lock order: vault row, then operation row. A vanished operation means the vault
    // or operation was deleted while the worker call ran; nothing is installed before completion.
    lock_vault_row(&mut tx, principal.vault, "mcp_activate_vault_lock").await?;
    let locked_state = locked_operation_state(
        &mut tx,
        principal.vault,
        principal.device,
        request.operation_id,
    )
    .await?
    .ok_or(ApiError(StatusCode::NOT_FOUND, "mcp_operation_not_found"))?;
    if locked_state != operation_state {
        return Err(ApiError(StatusCode::CONFLICT, "mcp_operation_invalid"));
    }
    require_current_owner(&mut tx, &principal).await?;
    if operation_state == "prepared" {
        let transition = sqlx::query("UPDATE mcp_operations SET state='activated',activation_fingerprint=$4 WHERE id=$1 AND vault_id=$2 AND worker_device_id=$3 AND state='prepared' AND expires_at > clock_timestamp()")
            .bind(request.operation_id).bind(principal.vault).bind(worker_device_id).bind(&fingerprint).execute(&mut *tx).await.map_err(|error| database_unavailable(&error, "mcp_activate_update"))?;
        if transition.rows_affected() != 1 {
            return Err(ApiError(StatusCode::CONFLICT, "mcp_operation_invalid"));
        }
    }
    tx.commit()
        .await
        .map_err(|error| database_unavailable(&error, "mcp_activate_update_commit"))?;
    Ok(Json(response))
}

async fn complete(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Json(request): Json<HostCompletion>,
) -> ApiResult<Json<McpDeviceStatus>> {
    let principal = authenticated(&state, &headers, Operation::Revoke).await?;
    owner(&principal)?;
    authorize_custody(
        &state,
        &headers,
        &principal,
        EnrollmentOperation::ConfirmComputerEnrollment,
    )
    .await?;
    let fingerprint = request_fingerprint(&request.admission)?;
    let mut tx = mcp_advisory_locked_transaction(&state, &principal, "mcp_complete_begin").await?;
    require_current_owner(&mut tx, &principal).await?;
    let operation = sqlx::query("SELECT worker_device_id,state,completion_fingerprint,expires_at <= clock_timestamp() expired FROM mcp_operations WHERE id=$1 AND vault_id=$2 AND owner_device_id=$3")
        .bind(request.operation_id).bind(principal.vault).bind(principal.device).fetch_optional(&mut *tx).await.map_err(|error| database_unavailable(&error, "mcp_complete_lookup"))?.ok_or(ApiError(StatusCode::NOT_FOUND, "mcp_operation_not_found"))?;
    let device_id: Uuid = operation.get("worker_device_id");
    let operation_state: String = operation.get("state");
    let expired: bool = operation.get("expired");
    if operation_state != "completed" && expired {
        return Err(ApiError(StatusCode::CONFLICT, "mcp_operation_invalid"));
    }
    if request.admission.vault_id != principal.vault || request.admission.device_id != device_id {
        return Err(ApiError(StatusCode::CONFLICT, "mcp_operation_invalid"));
    }
    if operation_state == "completed" {
        if operation
            .get::<Option<Vec<u8>>, _>("completion_fingerprint")
            .as_deref()
            != Some(fingerprint.as_slice())
        {
            return Err(ApiError(StatusCode::CONFLICT, "mcp_operation_invalid"));
        }
        tx.commit()
            .await
            .map_err(|error| database_unavailable(&error, "mcp_complete_replay_commit"))?;
        return device_status(state, principal, device_id).await;
    }
    if operation_state != "activated" {
        return Err(ApiError(StatusCode::CONFLICT, "mcp_operation_invalid"));
    }
    ensure_no_other_live_mcp_device(&mut tx, &principal, device_id).await?;
    worker_request(
        &state,
        "/internal/mcp/complete",
        &serde_json::to_value(McpWorkerCompleteRequest {
            vault_id: principal.vault,
            operation_id: request.operation_id,
            admission: request.admission,
        })
        .map_err(|_| ApiError(StatusCode::BAD_REQUEST, "mcp_completion_invalid"))?,
    )
    .await?;
    if let Err(rejection) =
        finalize_installed_completion(&mut tx, &principal, request.operation_id, device_id).await
    {
        cleanup_rejected_completion(
            &state,
            &mut tx,
            principal.vault,
            request.operation_id,
            device_id,
        )
        .await?;
        tx.commit()
            .await
            .map_err(|error| database_unavailable(&error, "mcp_complete_rejection_commit"))?;
        return Err(rejection);
    }
    let transition = sqlx::query("UPDATE mcp_operations SET state='completed',completion_fingerprint=$3 WHERE id=$1 AND vault_id=$2 AND owner_device_id=$4 AND state='activated' AND expires_at > clock_timestamp()")
        .bind(request.operation_id).bind(principal.vault).bind(&fingerprint).bind(principal.device).execute(&mut *tx).await.map_err(|error| database_unavailable(&error, "mcp_complete_update"))?;
    if transition.rows_affected() != 1 {
        cleanup_rejected_completion(
            &state,
            &mut tx,
            principal.vault,
            request.operation_id,
            device_id,
        )
        .await?;
        tx.commit()
            .await
            .map_err(|error| database_unavailable(&error, "mcp_complete_cleanup_commit"))?;
        return Err(ApiError(StatusCode::CONFLICT, "mcp_operation_invalid"));
    }
    sqlx::query("INSERT INTO mcp_devices(vault_id,device_id) VALUES($1,$2) ON CONFLICT (vault_id,device_id) DO NOTHING") .bind(principal.vault).bind(device_id).execute(&mut *tx).await.map_err(|error| database_unavailable(&error, "mcp_complete_register"))?;
    tx.commit()
        .await
        .map_err(|error| database_unavailable(&error, "mcp_complete_register_commit"))?;
    device_status(state, principal, device_id).await
}

/// Finalizes an installed completion in lock order: advisory lock (held by the caller), then the
/// vault row, then the operation row, then the owner and recipient checks. A vanished operation
/// means the vault or operation was deleted while the worker call ran.
async fn finalize_installed_completion(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    principal: &Principal,
    operation_id: Uuid,
    device_id: Uuid,
) -> ApiResult<()> {
    lock_vault_row(tx, principal.vault, "mcp_complete_vault_lock").await?;
    let state = locked_operation_state(tx, principal.vault, principal.device, operation_id)
        .await?
        .ok_or(ApiError(StatusCode::NOT_FOUND, "mcp_operation_not_found"))?;
    if state != "activated" {
        return Err(ApiError(StatusCode::CONFLICT, "mcp_operation_invalid"));
    }
    require_current_owner(tx, principal).await?;
    ensure_no_other_live_mcp_device(tx, principal, device_id).await?;
    let enrolled = sqlx::query_scalar::<_, bool>("SELECT EXISTS(SELECT 1 FROM computer_enrollments c JOIN devices d ON d.vault_id=c.vault_id AND d.device_id=c.recipient_device_id WHERE c.vault_id=$1 AND c.owner_device_id=$2 AND c.recipient_device_id=$3 AND c.state='consumed' AND d.role='device' AND d.revoked_at IS NULL)")
        .bind(principal.vault)
        .bind(principal.device)
        .bind(device_id)
        .fetch_one(&mut **tx)
        .await
        .map_err(|error| database_unavailable(&error, "mcp_complete_device"))?;
    if !enrolled {
        return Err(ApiError(StatusCode::CONFLICT, "mcp_device_not_enrolled"));
    }
    Ok(())
}

async fn ensure_no_other_live_mcp_device(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    principal: &Principal,
    device_id: Uuid,
) -> ApiResult<()> {
    let live_device: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM mcp_devices m JOIN devices d ON d.vault_id=m.vault_id AND d.device_id=m.device_id WHERE m.vault_id=$1 AND m.device_id<>$2 AND m.cleanup_pending=FALSE AND d.revoked_at IS NULL)",
    )
    .bind(principal.vault)
    .bind(device_id)
    .fetch_one(&mut **tx)
    .await
    .map_err(|error| database_unavailable(&error, "mcp_complete_device_conflict"))?;
    if live_device {
        return Err(ApiError(StatusCode::CONFLICT, "mcp_device_conflict"));
    }
    Ok(())
}

/// Locks one operation row. Callers must already hold the vault row lock.
async fn locked_operation_state(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    vault_id: Uuid,
    owner_device_id: Uuid,
    operation_id: Uuid,
) -> ApiResult<Option<String>> {
    sqlx::query_scalar::<_, String>("SELECT state FROM mcp_operations WHERE id=$1 AND vault_id=$2 AND owner_device_id=$3 FOR UPDATE")
        .bind(operation_id)
        .bind(vault_id)
        .bind(owner_device_id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(|error| database_unavailable(&error, "mcp_operation_lock"))
}

async fn confirm_owner_under_vault_lock(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    principal: &Principal,
) -> ApiResult<()> {
    lock_vault_row(tx, principal.vault, "mcp_owner_vault_lock").await?;
    require_current_owner(tx, principal).await
}

/// Best-effort cancel of a claimant minted by a prepare that could not be finalized.
async fn cancel_new_claimant(state: &ApiState, principal: &Principal, operation_id: Uuid) {
    if let Some(worker) = &state.mcp_worker {
        let _ =
            cancel_worker_claimant(worker, principal.vault, principal.device, operation_id).await;
    }
}

async fn lock_vault_row(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    vault_id: Uuid,
    operation: &'static str,
) -> ApiResult<()> {
    sqlx::query("SELECT 1 FROM vaults WHERE vault_id=$1 FOR UPDATE")
        .bind(vault_id)
        .execute(&mut **tx)
        .await
        .map_err(|error| database_unavailable(&error, operation))?;
    Ok(())
}

/// Durable cleanup for a worker install that completion will not finalize. The operation is
/// revoked and the runtime revoke is attempted while the enable lock and vault row are still held;
/// cleanup_pending clears only after the worker acknowledges the revoke. A failed or unreachable
/// revoke leaves cleanup_pending set for the sweep to retry.
async fn cleanup_rejected_completion(
    state: &ApiState,
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    vault_id: Uuid,
    operation_id: Uuid,
    device_id: Uuid,
) -> ApiResult<()> {
    sqlx::query("UPDATE mcp_operations SET state='revoked',cleanup_pending=TRUE WHERE id=$1 AND vault_id=$2 AND state='activated'")
        .bind(operation_id)
        .bind(vault_id)
        .execute(&mut **tx)
        .await
        .map_err(|error| database_unavailable(&error, "mcp_complete_cleanup"))?;
    if let Some(worker) = &state.mcp_worker
        && revoke_worker_device(worker, vault_id, device_id).await
    {
        sqlx::query("UPDATE mcp_operations SET cleanup_pending=FALSE WHERE id=$1 AND vault_id=$2 AND state='revoked'")
            .bind(operation_id)
            .bind(vault_id)
            .execute(&mut **tx)
            .await
            .map_err(|error| database_unavailable(&error, "mcp_complete_cleanup_clear"))?;
    }
    Ok(())
}

async fn device_status(
    state: ApiState,
    principal: Principal,
    device_id: Uuid,
) -> ApiResult<Json<McpDeviceStatus>> {
    let mut tx = scope::begin(&state.db, principal.vault)
        .await
        .map_err(|error| database_unavailable(&error, "mcp_device_status_begin"))?;
    let row = sqlx::query(
        "SELECT d.name,d.revoked_at IS NOT NULL revoked,m.cleanup_pending \
         FROM mcp_devices m JOIN devices d ON d.vault_id=m.vault_id AND d.device_id=m.device_id \
         WHERE m.vault_id=$1 AND m.device_id=$2",
    )
    .bind(principal.vault)
    .bind(device_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(|error| database_unavailable(&error, "mcp_device_status_query"))?;
    tx.commit()
        .await
        .map_err(|error| database_unavailable(&error, "mcp_device_status_commit"))?;
    let row = row.ok_or(ApiError(StatusCode::CONFLICT, "mcp_device_not_enrolled"))?;
    Ok(Json(McpDeviceStatus {
        vault_id: principal.vault,
        available: mcp_available(&state),
        server_origin: state
            .instance_origin
            .as_ref()
            .map(|origin| origin.origin().ascii_serialization())
            .unwrap_or_default(),
        hosted: state.row_security,
        device: Some(McpDeviceView {
            device_id,
            name: row.get("name"),
            state: if row.get("revoked") {
                McpDeviceState::Revoked
            } else {
                McpDeviceState::Offline
            },
            last_synced_at: None,
            cleanup_pending: row.get("cleanup_pending"),
        }),
    }))
}

fn mcp_available(state: &ApiState) -> bool {
    state.mcp_worker.is_some() && state.instance_origin.is_some()
}

async fn authorize_custody(
    state: &ApiState,
    headers: &HeaderMap,
    principal: &Principal,
    operation: EnrollmentOperation,
) -> ApiResult<()> {
    let context = state
        .enrollment_authorization
        .authorize_enrollment(EnrollmentAuthorizationRequest {
            headers: headers.clone(),
            method: axum::http::Method::POST,
            operation,
            vault_id: Some(principal.vault),
        })
        .await
        .map_err(|status| ApiError(status, "mcp_enrollment_unauthorized"))?;
    if context.vault_id != principal.vault {
        return Err(ApiError(StatusCode::FORBIDDEN, "foreign_vault"));
    }
    Ok(())
}

async fn mcp_advisory_locked_transaction<'a>(
    state: &'a ApiState,
    principal: &Principal,
    operation: &'static str,
) -> Result<sqlx::Transaction<'a, sqlx::Postgres>, ApiError> {
    let mut tx = scope::begin(&state.db, principal.vault)
        .await
        .map_err(|error| database_unavailable(&error, operation))?;
    mcp_enable_lock(&mut tx, principal.vault)
        .await
        .map_err(|error| database_unavailable(&error, operation))?;
    Ok(tx)
}

async fn mcp_enable_lock(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    vault_id: Uuid,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "SELECT pg_advisory_xact_lock(hashtextextended('peppy:mcp-enable:' || $1::text, 0))",
    )
    .bind(vault_id)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn require_current_owner(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    principal: &Principal,
) -> ApiResult<()> {
    if revalidate_principal(tx, principal).await? != "owner" {
        return Err(ApiError(StatusCode::FORBIDDEN, "owner_required"));
    }
    Ok(())
}

pub(super) async fn worker_request(
    state: &ApiState,
    path: &str,
    body: &serde_json::Value,
) -> ApiResult<serde_json::Value> {
    let worker = state
        .mcp_worker
        .as_ref()
        .ok_or(ApiError(StatusCode::SERVICE_UNAVAILABLE, "mcp_unavailable"))?;
    worker_request_with(worker, path, body).await
}

pub(super) async fn webhook_worker_request(
    state: &ApiState,
    body: &serde_json::Value,
) -> ApiResult<serde_json::Value> {
    let worker = state
        .mcp_worker
        .as_ref()
        .ok_or(ApiError(StatusCode::CONFLICT, "mcp_device_required"))?;
    worker_request_with_error_mapping(worker, "/internal/mcp/webhooks", body, true).await
}

async fn worker_request_with(
    worker: &McpWorkerClient,
    path: &str,
    body: &serde_json::Value,
) -> ApiResult<serde_json::Value> {
    worker_request_with_error_mapping(worker, path, body, false).await
}

async fn worker_request_with_error_mapping(
    worker: &McpWorkerClient,
    path: &str,
    body: &serde_json::Value,
    webhook_errors: bool,
) -> ApiResult<serde_json::Value> {
    let Some(max_body) = worker_request_limit(path) else {
        return Err(ApiError(
            StatusCode::BAD_REQUEST,
            "mcp_worker_request_invalid",
        ));
    };
    if serde_json::to_vec(body).map_or(true, |body| body.len() > max_body) {
        return Err(ApiError(
            StatusCode::BAD_REQUEST,
            "mcp_worker_request_invalid",
        ));
    }
    let token = tokio::fs::read_to_string(&worker.token_file)
        .await
        .map_err(|_| ApiError(StatusCode::SERVICE_UNAVAILABLE, "mcp_unavailable"))?;
    let token = token.trim();
    if token.is_empty() || token.len() > 4096 {
        return Err(ApiError(StatusCode::SERVICE_UNAVAILABLE, "mcp_unavailable"));
    }
    let url = worker
        .url
        .join(path.trim_start_matches('/'))
        .map_err(|_| ApiError(StatusCode::SERVICE_UNAVAILABLE, "mcp_unavailable"))?;
    let timeout = worker_request_timeout(path).expect("allowlisted path has a timeout");
    let response = worker
        .client
        .post(url)
        .bearer_auth(token)
        .json(body)
        .timeout(timeout)
        .send()
        .await
        .map_err(|_| ApiError(StatusCode::SERVICE_UNAVAILABLE, "mcp_worker_unavailable"))?;
    if !response.status().is_success() {
        if webhook_errors {
            let status = response.status();
            let body = bounded_response_body(response, 4 * 1024).await?;
            return Err(webhook_worker_error(status, &body));
        }
        return Err(ApiError(
            StatusCode::SERVICE_UNAVAILABLE,
            "mcp_worker_unavailable",
        ));
    }
    let response_limit = if webhook_errors {
        2 * 1024 * 1024
    } else {
        max_body
    };
    if response
        .content_length()
        .is_some_and(|length| length > response_limit as u64)
    {
        return Err(ApiError(StatusCode::BAD_GATEWAY, "mcp_worker_invalid"));
    }
    let status = response.status();
    let bytes = bounded_response_body(response, response_limit).await?;
    if (matches!(path, "/internal/mcp/revoke" | "/internal/mcp/cancel") || webhook_errors)
        && status == StatusCode::NO_CONTENT
        && bytes.is_empty()
    {
        return Ok(serde_json::Value::Null);
    }
    serde_json::from_slice(&bytes)
        .map_err(|_| ApiError(StatusCode::BAD_GATEWAY, "mcp_worker_invalid"))
}

fn worker_request_limit(path: &str) -> Option<usize> {
    match path {
        "/internal/mcp/call" => Some(4 * 1024 * 1024),
        "/internal/mcp/prepare"
        | "/internal/mcp/activate"
        | "/internal/mcp/complete"
        | "/internal/mcp/status"
        | "/internal/mcp/revoke"
        | "/internal/mcp/cancel"
        | "/internal/mcp/webhooks" => Some(64 * 1024),
        _ => None,
    }
}

fn worker_request_timeout(path: &str) -> Option<Duration> {
    match path {
        "/internal/mcp/call" => Some(Duration::from_secs(30)),
        "/internal/mcp/complete" => Some(Duration::from_millis(MCP_WORKER_COMPLETE_TIMEOUT_MS)),
        "/internal/mcp/webhooks" => Some(Duration::from_secs(20)),
        "/internal/mcp/prepare"
        | "/internal/mcp/activate"
        | "/internal/mcp/status"
        | "/internal/mcp/revoke"
        | "/internal/mcp/cancel" => Some(Duration::from_secs(5)),
        _ => None,
    }
}

async fn bounded_response_body(response: reqwest::Response, limit: usize) -> ApiResult<Vec<u8>> {
    let mut stream = response.bytes_stream();
    let mut bytes = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| ApiError(StatusCode::BAD_GATEWAY, "mcp_worker_invalid"))?;
        if bytes.len().saturating_add(chunk.len()) > limit {
            return Err(ApiError(StatusCode::BAD_GATEWAY, "mcp_worker_invalid"));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

fn webhook_worker_error(status: StatusCode, body: &[u8]) -> ApiError {
    #[derive(Deserialize)]
    struct WorkerError {
        code: String,
    }

    let Ok(WorkerError { code }) = serde_json::from_slice(body) else {
        return ApiError(StatusCode::BAD_GATEWAY, "mcp_worker_invalid");
    };
    let allowed_code = match code.as_str() {
        "webhook_url_invalid" => "webhook_url_invalid",
        "webhook_url_insecure" => "webhook_url_insecure",
        "webhook_url_blocked" => "webhook_url_blocked",
        "webhook_name_invalid" => "webhook_name_invalid",
        "webhook_filter_invalid" => "webhook_filter_invalid",
        "webhook_limit" => "webhook_limit",
        "webhook_not_found" => "webhook_not_found",
        "mcp_unavailable" => "mcp_unavailable",
        "mcp_device_required" => "mcp_device_required",
        _ => return ApiError(StatusCode::BAD_GATEWAY, "mcp_worker_invalid"),
    };
    if matches!(
        status,
        StatusCode::BAD_REQUEST
            | StatusCode::NOT_FOUND
            | StatusCode::CONFLICT
            | StatusCode::SERVICE_UNAVAILABLE
    ) {
        ApiError(status, allowed_code)
    } else {
        ApiError(StatusCode::BAD_GATEWAY, "mcp_worker_invalid")
    }
}

#[cfg(test)]
mod worker_wire_tests {
    use super::*;

    #[tokio::test]
    async fn worker_rejects_invalid_responses_and_maps_status_failures_to_unavailable() {
        let temp = tempfile::tempdir().unwrap();
        let token_file = temp.path().join("worker-token");
        std::fs::write(&token_file, "test-worker-control-token").unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap())
            .parse()
            .unwrap();
        let router = axum::Router::new().fallback(axum::routing::post(
            |headers: HeaderMap, Json(body): Json<serde_json::Value>| async move {
                assert_eq!(headers["authorization"], "Bearer test-worker-control-token");
                let status = match body["response"].as_str().unwrap() {
                    "empty204" => StatusCode::NO_CONTENT,
                    "empty200" => StatusCode::OK,
                    "error" => StatusCode::BAD_REQUEST,
                    "webhookError" => StatusCode::CONFLICT,
                    _ => StatusCode::OK,
                };
                let body = match body["response"].as_str().unwrap() {
                    "error" => "{\"error\":\"rejected\"}",
                    "webhookError" => "{\"code\":\"webhook_limit\"}",
                    "malformed" => "not json",
                    _ => "",
                };
                (status, body)
            },
        ));
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let worker = McpWorkerClient {
            url,
            token_file,
            client: reqwest::Client::new(),
        };
        let response = worker_request_with(
            &worker,
            "/internal/mcp/revoke",
            &serde_json::json!({"response":"empty204"}),
        )
        .await
        .unwrap();
        assert_eq!(response, serde_json::Value::Null);
        let error = worker_request_with_error_mapping(
            &worker,
            "/internal/mcp/webhooks",
            &serde_json::json!({"response":"webhookError"}),
            true,
        )
        .await
        .unwrap_err();
        assert_eq!(error.0, StatusCode::CONFLICT);
        assert_eq!(error.1, "webhook_limit");
        for (path, response, expected) in [
            ("/internal/mcp/status", "empty204", StatusCode::BAD_GATEWAY),
            ("/internal/mcp/revoke", "empty200", StatusCode::BAD_GATEWAY),
            ("/internal/mcp/status", "malformed", StatusCode::BAD_GATEWAY),
            ("/internal/mcp/revoke", "malformed", StatusCode::BAD_GATEWAY),
            (
                "/internal/mcp/status",
                "error",
                StatusCode::SERVICE_UNAVAILABLE,
            ),
            (
                "/internal/mcp/revoke",
                "error",
                StatusCode::SERVICE_UNAVAILABLE,
            ),
        ] {
            let error =
                worker_request_with(&worker, path, &serde_json::json!({"response":response}))
                    .await
                    .unwrap_err();
            assert_eq!(error.0, expected, "{path} {response}");
        }
        server.abort();
    }

    #[test]
    fn webhook_worker_errors_pass_through_only_allowlisted_codes() {
        for (code, status) in [
            ("webhook_url_invalid", StatusCode::BAD_REQUEST),
            ("webhook_url_insecure", StatusCode::BAD_REQUEST),
            ("webhook_url_blocked", StatusCode::BAD_REQUEST),
            ("webhook_name_invalid", StatusCode::BAD_REQUEST),
            ("webhook_filter_invalid", StatusCode::BAD_REQUEST),
            ("webhook_limit", StatusCode::CONFLICT),
            ("webhook_not_found", StatusCode::NOT_FOUND),
            ("mcp_unavailable", StatusCode::SERVICE_UNAVAILABLE),
            ("mcp_device_required", StatusCode::CONFLICT),
        ] {
            let error = webhook_worker_error(status, format!(r#"{{"code":"{code}"}}"#).as_bytes());
            assert_eq!(error.0, status, "{code}");
            assert_eq!(error.1, code);
        }

        for (status, body) in [
            (
                StatusCode::BAD_REQUEST,
                br#"{"code":"unexpected"}"#.as_slice(),
            ),
            (StatusCode::BAD_REQUEST, b"not json".as_slice()),
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                br#"{"code":"webhook_limit"}"#.as_slice(),
            ),
        ] {
            let error = webhook_worker_error(status, body);
            assert_eq!(error.0, StatusCode::BAD_GATEWAY);
            assert_eq!(error.1, "mcp_worker_invalid");
        }
    }

    #[test]
    fn worker_request_configuration_allows_webhooks_with_control_limits() {
        assert_eq!(
            worker_request_limit("/internal/mcp/webhooks"),
            Some(64 * 1024)
        );
        assert_eq!(
            worker_request_timeout("/internal/mcp/webhooks"),
            Some(Duration::from_secs(20))
        );
        assert_eq!(
            worker_request_limit("/internal/mcp/call"),
            Some(4 * 1024 * 1024)
        );
        assert_eq!(
            worker_request_timeout("/internal/mcp/status"),
            Some(Duration::from_secs(5))
        );
        assert!(
            worker_request_timeout("/internal/mcp/complete").unwrap()
                < Duration::from_millis(peppy_hosted_api::mcp::MCP_COMPLETE_REQUEST_TIMEOUT_MS)
        );
        assert_eq!(worker_request_limit("/internal/mcp/not-a-route"), None);
    }
}
