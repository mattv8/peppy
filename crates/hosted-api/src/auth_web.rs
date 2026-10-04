//! Public wire contracts for hosted browser authentication.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[derive(schemars::JsonSchema)]
pub struct WebSessionResponse {
    pub authenticated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub account_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub csrf_token: Option<String>,
}

#[derive(Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WebConfigResponse {
    pub stripe_publishable_key: String,
}

#[derive(Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DeletionState {
    None,
    Pending,
}

#[derive(Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WebAccountResponse {
    pub account_id: Uuid,
    pub deletion_state: DeletionState,
}

pub mod routes {
    pub const SESSION: &str = "/hosted/v1/web/session";
    pub const AUTH_START: &str = "/hosted/v1/web/auth/{provider}/start";
    pub const GOOGLE_CALLBACK: &str = "/hosted/v1/web/auth/google/callback";
    pub const APPLE_CALLBACK: &str = "/hosted/v1/web/auth/apple/callback";
    pub const LOGOUT: &str = "/hosted/v1/web/logout";
    pub const ACCOUNT: &str = "/hosted/v1/web/account";
    pub const CONFIG: &str = "/hosted/v1/web/config";
}
