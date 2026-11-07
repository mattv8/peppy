use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post, put},
};
use peppy_hosted_api::mcp::{
    McpWorkerWebhookOp, McpWorkerWebhookRequest, WebhookCreated, WebhookInput, WebhookList,
    WebhookSecret, WebhookTestResult, WebhookView,
};
use sqlx::Row;
use uuid::Uuid;

use super::super::{
    ApiError, ApiResult, ApiState, Operation, authenticated, database_unavailable, owner, scope,
};
use super::webhook_worker_request;

const HOSTED_APP_ORIGIN: &str = "https://app.peppy.pro";

pub(super) fn routes() -> Router<ApiState> {
    Router::new()
        .route("/v1/mcp/webhooks", get(list).post(create))
        .route("/v1/mcp/webhooks/{id}", put(update).delete(remove))
        .route("/v1/mcp/webhooks/{id}/secret", post(reveal_secret))
        .route("/v1/mcp/webhooks/{id}/rotate-secret", post(rotate_secret))
        .route("/v1/mcp/webhooks/{id}/test", post(test))
}

async fn list(State(state): State<ApiState>, headers: HeaderMap) -> ApiResult<Response> {
    let principal = authenticated(&state, &headers, Operation::Read).await?;
    owner(&principal)?;
    if state.mcp_worker.is_none() {
        return Ok(Json(WebhookList {
            available: false,
            webhooks: Vec::new(),
        })
        .into_response());
    }
    let Some(device_id) = live_device_id(&state, principal.vault).await? else {
        return Ok(Json(WebhookList {
            available: false,
            webhooks: Vec::new(),
        })
        .into_response());
    };
    let response: WebhookList = forward(
        &state,
        principal.vault,
        device_id,
        McpWorkerWebhookOp::List,
        None,
        None,
    )
    .await?;
    Ok(Json(response).into_response())
}

async fn create(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Json(input): Json<WebhookInput>,
) -> ApiResult<Response> {
    let principal = owner_principal(&state, &headers).await?;
    let response: WebhookCreated = mutation(
        &state,
        principal.vault,
        McpWorkerWebhookOp::Create,
        None,
        Some(input),
    )
    .await?;
    Ok(no_store(
        (StatusCode::CREATED, Json(response)).into_response(),
    ))
}

async fn update(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(input): Json<WebhookInput>,
) -> ApiResult<Response> {
    let principal = owner_principal(&state, &headers).await?;
    let response: WebhookView = mutation(
        &state,
        principal.vault,
        McpWorkerWebhookOp::Update,
        Some(id),
        Some(input),
    )
    .await?;
    Ok(Json(response).into_response())
}

async fn remove(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let principal = owner_principal(&state, &headers).await?;
    let _: serde_json::Value = mutation(
        &state,
        principal.vault,
        McpWorkerWebhookOp::Delete,
        Some(id),
        None,
    )
    .await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

async fn reveal_secret(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let principal = owner_principal(&state, &headers).await?;
    let response: WebhookSecret = mutation(
        &state,
        principal.vault,
        McpWorkerWebhookOp::RevealSecret,
        Some(id),
        None,
    )
    .await?;
    Ok(no_store(Json(response).into_response()))
}

fn no_store(mut response: Response) -> Response {
    response
        .headers_mut()
        .insert("cache-control", HeaderValue::from_static("no-store"));
    response
}

async fn rotate_secret(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let principal = owner_principal(&state, &headers).await?;
    let response: WebhookSecret = mutation(
        &state,
        principal.vault,
        McpWorkerWebhookOp::RotateSecret,
        Some(id),
        None,
    )
    .await?;
    Ok(no_store(Json(response).into_response()))
}

async fn test(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let principal = owner_principal(&state, &headers).await?;
    let response: WebhookTestResult = mutation(
        &state,
        principal.vault,
        McpWorkerWebhookOp::Test,
        Some(id),
        None,
    )
    .await?;
    Ok(Json(response).into_response())
}

async fn owner_principal(
    state: &ApiState,
    headers: &HeaderMap,
) -> ApiResult<super::super::Principal> {
    let principal = authenticated(state, headers, Operation::Read).await?;
    owner(&principal)?;
    Ok(principal)
}

async fn mutation<T: serde::de::DeserializeOwned>(
    state: &ApiState,
    vault_id: Uuid,
    op: McpWorkerWebhookOp,
    id: Option<Uuid>,
    input: Option<WebhookInput>,
) -> ApiResult<T> {
    if state.mcp_worker.is_none() {
        return Err(ApiError(StatusCode::CONFLICT, "mcp_device_required"));
    }
    let device_id = live_device_id(state, vault_id)
        .await?
        .ok_or(ApiError(StatusCode::CONFLICT, "mcp_device_required"))?;
    forward(state, vault_id, device_id, op, id, input).await
}

async fn forward<T: serde::de::DeserializeOwned>(
    state: &ApiState,
    vault_id: Uuid,
    device_id: Uuid,
    op: McpWorkerWebhookOp,
    id: Option<Uuid>,
    input: Option<WebhookInput>,
) -> ApiResult<T> {
    let body = serde_json::to_value(McpWorkerWebhookRequest {
        vault_id,
        device_id,
        source_origin: source_origin(state),
        op,
        id,
        input,
    })
    .expect("DTO serializes");
    let response = webhook_worker_request(state, &body).await?;
    serde_json::from_value(response)
        .map_err(|_| ApiError(StatusCode::BAD_GATEWAY, "mcp_worker_invalid"))
}

fn source_origin(state: &ApiState) -> String {
    if state.row_security {
        return HOSTED_APP_ORIGIN.into();
    }
    state
        .mcp_consent_origin
        .clone()
        .or_else(|| {
            state
                .instance_origin
                .as_ref()
                .map(|origin| origin.origin().ascii_serialization())
        })
        .unwrap_or_default()
}

async fn live_device_id(state: &ApiState, vault_id: Uuid) -> ApiResult<Option<Uuid>> {
    let mut tx = scope::begin(&state.db, vault_id)
        .await
        .map_err(|error| database_unavailable(&error, "mcp_webhooks_device_begin"))?;
    let row = sqlx::query(
        "SELECT m.device_id FROM mcp_devices m JOIN devices d ON d.vault_id=m.vault_id AND d.device_id=m.device_id \
         WHERE m.vault_id=$1 AND m.cleanup_pending=FALSE AND d.revoked_at IS NULL ORDER BY m.created_at DESC LIMIT 1",
    )
    .bind(vault_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(|error| database_unavailable(&error, "mcp_webhooks_device_query"))?;
    tx.commit()
        .await
        .map_err(|error| database_unavailable(&error, "mcp_webhooks_device_commit"))?;
    Ok(row.map(|row| row.get("device_id")))
}
