//! Public wire contracts for hosted browser authentication.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::IdentityProvider;

#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WebCapabilitiesResponse {
    #[serde(default)]
    pub manage_testers: bool,
}

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
    pub available_providers: Vec<IdentityProvider>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_url: Option<String>,
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
    pub const CAPABILITIES: &str = "/hosted/v1/web/capabilities";
    pub const SESSION: &str = "/hosted/v1/web/session";
    pub const SESSION_RENEW: &str = "/hosted/v1/web/session/renew";
    pub const APP_SESSION_RENEW: &str = "/hosted/v1/web/app-session/renew";
    pub const AUTH_START: &str = "/hosted/v1/web/auth/{provider}/start";
    pub const GOOGLE_CALLBACK: &str = "/hosted/v1/web/auth/google/callback";
    pub const APPLE_CALLBACK: &str = "/hosted/v1/web/auth/apple/callback";
    pub const LOGOUT: &str = "/hosted/v1/web/logout";
    pub const ACCOUNT: &str = "/hosted/v1/web/account";
    pub const CONFIG: &str = "/hosted/v1/web/config";
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::IdentityProvider;

    #[test]
    fn absent_capability_denies_tester_administration() {
        let capabilities: WebCapabilitiesResponse = serde_json::from_str("{}").unwrap();
        assert!(!capabilities.manage_testers);
        assert!(!WebCapabilitiesResponse::default().manage_testers);
        assert_eq!(routes::CAPABILITIES, "/hosted/v1/web/capabilities");
    }

    #[test]
    fn web_config_serializes_only_configured_identity_providers() {
        let config = WebConfigResponse {
            stripe_publishable_key: "pk_test".into(),
            available_providers: vec![IdentityProvider::Google],
            app_url: None,
        };

        assert_eq!(
            serde_json::to_value(config).unwrap(),
            serde_json::json!({
                "stripe_publishable_key": "pk_test",
                "available_providers": ["google"],
            })
        );
    }

    #[test]
    fn web_config_accepts_older_payloads_without_an_app_url() {
        let config: WebConfigResponse = serde_json::from_value(serde_json::json!({
            "stripe_publishable_key": "pk_test",
            "available_providers": ["google"],
        }))
        .unwrap();

        assert_eq!(config.app_url, None);
    }

    #[test]
    fn web_config_serializes_and_deserializes_an_app_url() {
        let config = WebConfigResponse {
            stripe_publishable_key: "pk_test".into(),
            available_providers: vec![IdentityProvider::Google],
            app_url: Some("https://app.example.test/".into()),
        };

        let value = serde_json::to_value(config).unwrap();
        assert_eq!(value["app_url"], "https://app.example.test/");
        let deserialized: WebConfigResponse = serde_json::from_value(value).unwrap();
        assert_eq!(
            deserialized.app_url.as_deref(),
            Some("https://app.example.test/")
        );
    }

    #[test]
    fn renewal_routes_use_the_web_and_app_session_audiences() {
        assert_eq!(routes::SESSION_RENEW, "/hosted/v1/web/session/renew");
        assert_eq!(
            routes::APP_SESSION_RENEW,
            "/hosted/v1/web/app-session/renew"
        );
    }
}
