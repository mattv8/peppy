use axum::{
    Router,
    http::HeaderMap,
    routing::{get, post},
};
use peppy_hosted_api::enrollment::{ClientKind, InstanceSession};
use serde::Serialize;
use sha2::{Digest, Sha256};
use sqlx::{Postgres, Row, Transaction};
use uuid::Uuid;

use super::{ApiError, ApiResult, ApiState};

mod events;
mod invites;
mod requests;

pub(super) struct SessionCtx {
    pub vault_id: Uuid,
    pub is_admin: bool,
    pub username: String,
}

pub(super) fn routes() -> Router<ApiState> {
    Router::new()
        .route("/v1/instance", get(super::instance_info))
        .route("/v1/instance/claim", post(super::claim_instance))
        .route(
            "/v1/instance/session",
            get(restore_browser_session)
                .post(super::instance_login)
                .delete(super::instance_logout),
        )
        .route("/v1/instance/account", get(super::instance_account))
        .route("/v1/instance/management", get(super::instance_management))
        .route(
            "/v1/instance/devices/{device_id}/revoke",
            post(super::instance_revoke_device),
        )
        .merge(invites::routes())
        .merge(requests::routes())
        .merge(events::routes())
}

#[derive(Serialize)]
struct BrowserSessionResponse {
    csrf_token: String,
}

async fn restore_browser_session(
    axum::extract::State(state): axum::extract::State<ApiState>,
    headers: HeaderMap,
) -> ApiResult<impl axum::response::IntoResponse> {
    if headers.contains_key("origin") && !super::instance_origin_matches(&state, &headers) {
        return Err(ApiError(
            axum::http::StatusCode::FORBIDDEN,
            "invalid_origin",
        ));
    }
    let token = super::cookie_token(&headers).ok_or(ApiError(
        axum::http::StatusCode::UNAUTHORIZED,
        "invalid_session",
    ))?;
    let (_, expected_csrf_digest) = lookup_session(&state.db, token)
        .await
        .map_err(|error| super::database_unavailable(&error, "instance_session_restore"))?
        .ok_or(ApiError(
            axum::http::StatusCode::UNAUTHORIZED,
            "invalid_session",
        ))?;
    let csrf_token = crate::instance::browser_csrf_token(token);
    if expected_csrf_digest.as_deref()
        != Some(crate::instance::digest(csrf_token.as_bytes()).as_slice())
    {
        return Err(ApiError(
            axum::http::StatusCode::UNAUTHORIZED,
            "invalid_session",
        ));
    }
    Ok((
        [(axum::http::header::CACHE_CONTROL, "no-store")],
        axum::Json(BrowserSessionResponse { csrf_token }),
    ))
}

pub(super) async fn require_session(
    state: &ApiState,
    headers: &HeaderMap,
    mutating: bool,
) -> ApiResult<SessionCtx> {
    let (token, browser) = super::instance_session_token(headers).ok_or(ApiError(
        axum::http::StatusCode::UNAUTHORIZED,
        "invalid_session",
    ))?;
    let row = lookup_session(&state.db, token)
        .await
        .map_err(|error| super::database_unavailable(&error, "instance_session_lookup"))?
        .ok_or(ApiError(
            axum::http::StatusCode::UNAUTHORIZED,
            "invalid_session",
        ))?;
    if mutating && browser {
        if !super::instance_origin_matches(state, headers) {
            return Err(ApiError(
                axum::http::StatusCode::FORBIDDEN,
                "invalid_origin",
            ));
        }
        let csrf = headers
            .get("x-peppy-csrf-token")
            .and_then(|value| value.to_str().ok())
            .ok_or(ApiError(axum::http::StatusCode::FORBIDDEN, "invalid_csrf"))?;
        let expected = row.1;
        if expected.as_deref() != Some(crate::instance::digest(csrf.as_bytes()).as_slice()) {
            return Err(ApiError(axum::http::StatusCode::FORBIDDEN, "invalid_csrf"));
        }
    }
    Ok(SessionCtx {
        vault_id: row.0.vault_id,
        is_admin: row.0.is_admin,
        username: row.0.username,
    })
}

pub(super) async fn lookup_session(
    db: &sqlx::PgPool,
    token: &str,
) -> Result<Option<(SessionCtx, Option<Vec<u8>>)>, sqlx::Error> {
    sqlx::query("SELECT owners.vault_id, owners.is_admin, owners.username, sessions.csrf_digest FROM instance_sessions sessions JOIN vault_owners owners ON owners.vault_id=sessions.vault_id WHERE sessions.token_digest=$1 AND sessions.revoked_at IS NULL AND sessions.expires_at>now()")
        .bind(crate::instance::digest(token.as_bytes()).as_slice())
        .fetch_optional(db)
        .await
        .map(|row| row.map(|row| (SessionCtx { vault_id: row.get("vault_id"), is_admin: row.get("is_admin"), username: row.get("username") }, row.get("csrf_digest"))))
}

pub(super) async fn require_admin(
    state: &ApiState,
    headers: &HeaderMap,
    mutating: bool,
) -> ApiResult<SessionCtx> {
    let session = require_session(state, headers, mutating).await?;
    if session.is_admin {
        Ok(session)
    } else {
        Err(ApiError(
            axum::http::StatusCode::FORBIDDEN,
            "admin_required",
        ))
    }
}

pub(super) async fn create_instance_session(
    tx: &mut Transaction<'_, Postgres>,
    client: ClientKind,
    vault_id: Uuid,
) -> ApiResult<InstanceSession> {
    super::create_instance_session(tx, client, vault_id).await
}

pub(super) fn request_digest<T: Serialize>(request: &T, excluded: &[&str]) -> ApiResult<[u8; 32]> {
    let mut value = serde_json::to_value(request)
        .map_err(|_| ApiError(axum::http::StatusCode::BAD_REQUEST, "invalid_claim"))?;
    let fields = value.as_object_mut().ok_or(ApiError(
        axum::http::StatusCode::BAD_REQUEST,
        "invalid_claim",
    ))?;
    for field in excluded {
        fields.remove(*field);
    }
    Ok(Sha256::digest(
        serde_json::to_vec(&value)
            .map_err(|_| ApiError(axum::http::StatusCode::BAD_REQUEST, "invalid_claim"))?,
    )
    .into())
}

pub(super) fn normalize_username(raw: &str) -> ApiResult<String> {
    let value = raw.trim().to_ascii_lowercase();
    let valid = (1..=32).contains(&value.len())
        && value.bytes().enumerate().all(|(index, byte)| {
            byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || (index > 0 && matches!(byte, b'.' | b'_' | b'-'))
        });
    if valid {
        Ok(value)
    } else {
        Err(ApiError(
            axum::http::StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_username",
        ))
    }
}

/// Older first-claim requests omitted the username and were persisted with the
/// default `admin` identity. Keep that replay-compatible default isolated from
/// new account creation, where an empty username is invalid.
pub(super) fn normalize_claim_username(raw: &str) -> ApiResult<String> {
    if raw.trim().is_empty() {
        Ok("admin".into())
    } else {
        normalize_username(raw)
    }
}

pub(super) fn notify_instance_change(state: &ApiState) {
    let _ = state.instance_events.send(());
}

pub(super) fn instance_claim_response(
    vault_id: Uuid,
    device_id: Uuid,
    replayed: bool,
    session: InstanceSession,
    client: ClientKind,
) -> ApiResult<axum::response::Response> {
    super::instance_claim_response(vault_id, device_id, replayed, session, client)
}
