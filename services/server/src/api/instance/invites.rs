use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    routing::{delete, post},
};
use peppy_hosted_api::enrollment::{CreateVaultInviteResponse, VaultInviteSummary};
use sqlx::Row;
use uuid::Uuid;

use super::{ApiError, ApiResult, ApiState, notify_instance_change, require_admin};

const INVITE_TTL_SECONDS: i64 = 24 * 60 * 60;
const MAX_ACTIVE_INVITES: i64 = 5;

pub(super) fn routes() -> Router<ApiState> {
    Router::new()
        .route("/v1/instance/invites", post(create).get(list))
        .route("/v1/instance/invites/{invite_id}", delete(revoke))
}

async fn create(
    State(state): State<ApiState>,
    headers: HeaderMap,
) -> ApiResult<Json<CreateVaultInviteResponse>> {
    let admin = require_admin(&state, &headers, true).await?;
    let mut tx = state
        .db
        .begin()
        .await
        .map_err(|error| super::super::database_unavailable(&error, "invite_begin"))?;
    // A transaction-scoped lock serializes the global active-invite quota.
    // Row locks alone cannot protect a count when the table has fewer than five rows.
    sqlx::query("SELECT pg_advisory_xact_lock(hashtext('peppy_active_vault_invites'))")
        .execute(&mut *tx)
        .await
        .map_err(|error| super::super::database_unavailable(&error, "invite_limit_lock"))?;
    let active: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM vault_invites WHERE state='active' AND expires_at > now()",
    )
    .fetch_one(&mut *tx)
    .await
    .map_err(|error| super::super::database_unavailable(&error, "invite_limit"))?;
    if active >= MAX_ACTIVE_INVITES {
        return Err(ApiError(StatusCode::CONFLICT, "invite_limit"));
    }
    let token = crate::instance::random_token()
        .map_err(|_| ApiError(StatusCode::SERVICE_UNAVAILABLE, "invite_unavailable"))?;
    let invite_id = Uuid::new_v4();
    sqlx::query("INSERT INTO vault_invites(id, token_digest, created_by, state, expires_at) VALUES($1,$2,$3,'active',now() + interval '24 hours')")
        .bind(invite_id).bind(crate::instance::digest(token.as_bytes()).as_slice()).bind(admin.vault_id)
        .execute(&mut *tx).await.map_err(|error| super::super::unique_conflict(error, "invite_limit", "invite_insert"))?;
    tx.commit()
        .await
        .map_err(|error| super::super::database_unavailable(&error, "invite_commit"))?;
    notify_instance_change(&state);
    Ok(Json(CreateVaultInviteResponse {
        invite_id,
        invite_token: token,
        expires_in_seconds: INVITE_TTL_SECONDS as u32,
    }))
}

async fn list(
    State(state): State<ApiState>,
    headers: HeaderMap,
) -> ApiResult<Json<Vec<VaultInviteSummary>>> {
    require_admin(&state, &headers, false).await?;
    let rows = sqlx::query("SELECT id, state, GREATEST(0, EXTRACT(EPOCH FROM expires_at-now()))::bigint seconds, to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"') created_at FROM vault_invites WHERE state='active' AND expires_at > now() ORDER BY created_at DESC")
        .fetch_all(&state.db).await.map_err(|error| super::super::database_unavailable(&error, "invite_list"))?;
    Ok(Json(
        rows.into_iter()
            .map(|row| VaultInviteSummary {
                invite_id: row.get("id"),
                state: row.get("state"),
                expires_in_seconds: row.get::<i64, _>("seconds") as u32,
                created_at: row.get("created_at"),
            })
            .collect(),
    ))
}

async fn revoke(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Path(invite_id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    require_admin(&state, &headers, true).await?;
    let mut tx = state
        .db
        .begin()
        .await
        .map_err(|error| super::super::database_unavailable(&error, "invite_revoke_begin"))?;
    let invite = sqlx::query("SELECT id FROM vault_invites WHERE id=$1 FOR UPDATE")
        .bind(invite_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|error| super::super::database_unavailable(&error, "invite_revoke_lock"))?;
    if invite.is_none() {
        return Err(ApiError(StatusCode::NOT_FOUND, "invite_invalid"));
    }
    // Lock the request after the invite, matching approve and create-vault. This
    // makes revocation authoritative for an approved request that has not completed.
    sqlx::query("SELECT id FROM vault_requests WHERE invite_id=$1 FOR UPDATE")
        .bind(invite_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|error| {
            super::super::database_unavailable(&error, "invite_revoke_request_lock")
        })?;
    sqlx::query("UPDATE vault_invites SET state='revoked' WHERE id=$1 AND state='active'")
        .bind(invite_id)
        .execute(&mut *tx)
        .await
        .map_err(|error| super::super::database_unavailable(&error, "invite_revoke"))?;
    sqlx::query("UPDATE vault_requests SET state='denied', decided_at=clock_timestamp() WHERE invite_id=$1 AND state IN ('pending','approved')").bind(invite_id).execute(&mut *tx).await.map_err(|error| super::super::database_unavailable(&error, "invite_revoke_request"))?;
    tx.commit()
        .await
        .map_err(|error| super::super::database_unavailable(&error, "invite_revoke_commit"))?;
    notify_instance_change(&state);
    Ok(StatusCode::NO_CONTENT)
}
