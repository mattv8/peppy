//! Device-authenticated account-access notice.
//!
//! The handler checks only the device credential (which also rejects revoked
//! credentials). Authorization belongs to the policy, which may legitimately
//! answer while the account is being deleted, so `authorize` is deliberately not
//! called here. Reading this route neither touches presence nor extends sessions.

use axum::{
    Json,
    extract::State,
    http::{HeaderMap, StatusCode},
};
use peppy_hosted_api::account_access::{AccountAccessResponse, error_codes};

use super::{
    ApiError, ApiResult, ApiState, DeviceAccountAccessError, access_principal, auth_with_mode,
};

const ROUTE: &str = peppy_hosted_api::account_access::routes::DEVICE_ACCOUNT_ACCESS;

fn device_account_access_error(error: DeviceAccountAccessError) -> ApiError {
    match error {
        DeviceAccountAccessError::Unsupported => {
            ApiError(StatusCode::NOT_IMPLEMENTED, error_codes::UNSUPPORTED)
        }
        DeviceAccountAccessError::Forbidden => {
            ApiError(StatusCode::FORBIDDEN, error_codes::FORBIDDEN)
        }
        DeviceAccountAccessError::Unavailable => {
            tracing::warn!(
                route = ROUTE,
                error_class = "device_account_access_callback_unavailable",
                "device account access callback failed"
            );
            ApiError(StatusCode::SERVICE_UNAVAILABLE, error_codes::UNAVAILABLE)
        }
    }
}

pub(super) async fn device_account_access(
    State(state): State<ApiState>,
    headers: HeaderMap,
) -> ApiResult<Json<AccountAccessResponse>> {
    let principal = auth_with_mode(&state, &headers).await?;
    state
        .policy
        .device_account_access(access_principal(&principal))
        .await
        .map(Json)
        .map_err(device_account_access_error)
}
