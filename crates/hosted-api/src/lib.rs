//! Public DTO crate for hosted infra wire contracts.
//!
//! This crate defines serialization contracts for the hosted authentication,
//! billing, and provisioning APIs. All types are designed for clean JSON
//! roundtripping with explicit error handling for unknown enum variants.

use serde::{Deserialize, Serialize};
use std::fmt;
use uuid::Uuid;

// ============================================================================
// Enums
// ============================================================================

/// Identity provider for authentication.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum IdentityProvider {
    Apple,
    Google,
}

impl fmt::Display for IdentityProvider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Apple => write!(f, "apple"),
            Self::Google => write!(f, "google"),
        }
    }
}

/// Account classification status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccountClassification {
    New,
    Existing,
    Incomplete,
    Provisioning,
    Pending,
    Lapsed,
    Unavailable,
}

impl fmt::Display for AccountClassification {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::New => write!(f, "new"),
            Self::Existing => write!(f, "existing"),
            Self::Incomplete => write!(f, "incomplete"),
            Self::Provisioning => write!(f, "provisioning"),
            Self::Pending => write!(f, "pending"),
            Self::Lapsed => write!(f, "lapsed"),
            Self::Unavailable => write!(f, "unavailable"),
        }
    }
}

/// Entitlement state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntitlementState {
    None,
    Active,
    Grace,
    BillingRetry,
    Pending,
    Expired,
    Revoked,
}

impl fmt::Display for EntitlementState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::None => write!(f, "none"),
            Self::Active => write!(f, "active"),
            Self::Grace => write!(f, "grace"),
            Self::BillingRetry => write!(f, "billing_retry"),
            Self::Pending => write!(f, "pending"),
            Self::Expired => write!(f, "expired"),
            Self::Revoked => write!(f, "revoked"),
        }
    }
}

/// Access mode for vault operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccessMode {
    ReadWrite,
    ReadOnly,
}

impl fmt::Display for AccessMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ReadWrite => write!(f, "read_write"),
            Self::ReadOnly => write!(f, "read_only"),
        }
    }
}

/// Error code for API responses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    Unavailable,
    Unauthorized,
    Conflict,
    InvalidRequest,
    EntitlementRequired,
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unavailable => write!(f, "unavailable"),
            Self::Unauthorized => write!(f, "unauthorized"),
            Self::Conflict => write!(f, "conflict"),
            Self::InvalidRequest => write!(f, "invalid_request"),
            Self::EntitlementRequired => write!(f, "entitlement_required"),
        }
    }
}

// ============================================================================
// Auth DTOs
// ============================================================================

/// Request to initiate a login attempt.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LoginAttemptRequest {
    pub provider: IdentityProvider,
}

/// Response containing login attempt details.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LoginAttemptResponse {
    pub attempt_id: Uuid,
    pub nonce: String,
    pub expires_in_seconds: u32,
}

/// Providers that are currently available for native sign-in.
#[derive(Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NativeAuthConfigResponse {
    pub available_providers: Vec<IdentityProvider>,
}

// Intentionally no Debug for LoginAttemptResponse (sensitive)

/// Request to exchange an identity token for a session.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionExchangeRequest {
    pub attempt_id: Uuid,
    pub id_token: String,
}

// Intentionally no Debug for SessionExchangeRequest (sensitive)

/// Response containing the authenticated session token.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionResponse {
    pub session_token: String,
    pub expires_in_seconds: u32,
    pub account_id: Uuid,
}

// Intentionally no Debug for SessionResponse (sensitive)

// ============================================================================
// Account DTOs
// ============================================================================

/// Response with account details.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccountResponse {
    pub account_id: Uuid,
    pub classification: AccountClassification,
    pub entitlement: EntitlementState,
    pub access: AccessMode,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vault_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub operation_id: Option<Uuid>,
}

// ============================================================================
// Billing DTOs
// ============================================================================

/// Request to initiate a billing session checkout.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BillingSessionRequest {
    pub operation_id: Uuid,
}

/// Response with billing session URL.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BillingSessionResponse {
    pub url: String,
}

// Intentionally no Debug for BillingSessionResponse (contains sensitive URL)

// ============================================================================
// Provisioning DTOs
// ============================================================================

/// Request to initiate device provisioning.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProvisioningRequest {
    pub operation_id: Uuid,
}

/// Response granting provisioning access.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProvisioningGrantResponse {
    pub grant: String,
    pub expires_in_seconds: u32,
}

// Intentionally no Debug for ProvisioningGrantResponse (sensitive)

/// Request to complete device provisioning.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompleteProvisioningRequest {
    pub operation_id: Uuid,
    pub grant: String,
    pub device_id: Uuid,
    /// SHA256 of client-held 96-hex token (64 lowercase hex digits).
    pub credential_digest: String,
    pub public_key_profile: serde_json::Value,
    /// Base64-encoded encrypted vault check header.
    pub encrypted_vault_check_header: String,
    pub profile_fingerprint: String,
    pub key_epoch: u32,
}

// Intentionally no Debug for CompleteProvisioningRequest (sensitive)

/// Response confirming provisioning completion.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompleteProvisioningResponse {
    pub operation_id: Uuid,
    pub vault_id: Uuid,
    pub device_id: Uuid,
    pub already_provisioned: bool,
}

// ============================================================================
// Route Constants
// ============================================================================

pub mod routes {
    pub const AUTH_CONFIG: &str = "/hosted/v1/auth/config";
    pub const AUTH_ATTEMPTS: &str = "/hosted/v1/auth/attempts";
    pub const AUTH_SESSION: &str = "/hosted/v1/auth/session";
    pub const ACCOUNT: &str = "/hosted/v1/account";
    pub const BILLING_CHECKOUT: &str = "/hosted/v1/billing/checkout";
    pub const BILLING_PORTAL: &str = "/hosted/v1/billing/portal";
    pub const BILLING_WEBHOOK: &str = "/hosted/v1/billing/webhook";
    pub const PROVISIONING: &str = "/hosted/v1/provisioning";
    pub const PROVISIONING_COMPLETE: &str = "/hosted/v1/provisioning/complete";
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ========================================================================
    // Enum Tests
    // ========================================================================

    #[test]
    fn test_identity_provider_snake_case() {
        let provider = IdentityProvider::Apple;
        let json = serde_json::to_string(&provider).unwrap();
        assert_eq!(json, "\"apple\"");

        let provider = IdentityProvider::Google;
        let json = serde_json::to_string(&provider).unwrap();
        assert_eq!(json, "\"google\"");
    }

    #[test]
    fn test_identity_provider_roundtrip() {
        let original = json!("apple");
        let provider: IdentityProvider = serde_json::from_value(original.clone()).unwrap();
        let serialized = serde_json::to_value(provider).unwrap();
        assert_eq!(original, serialized);

        let original = json!("google");
        let provider: IdentityProvider = serde_json::from_value(original.clone()).unwrap();
        let serialized = serde_json::to_value(provider).unwrap();
        assert_eq!(original, serialized);
    }

    #[test]
    fn test_identity_provider_unknown_rejects() {
        let json = json!("unknown_provider");
        let result: Result<IdentityProvider, _> = serde_json::from_value(json);
        assert!(result.is_err());
    }

    #[test]
    fn test_account_classification_snake_case() {
        assert_eq!(
            serde_json::to_string(&AccountClassification::New).unwrap(),
            "\"new\""
        );
        assert_eq!(
            serde_json::to_string(&AccountClassification::Existing).unwrap(),
            "\"existing\""
        );
        assert_eq!(
            serde_json::to_string(&AccountClassification::Incomplete).unwrap(),
            "\"incomplete\""
        );
        assert_eq!(
            serde_json::to_string(&AccountClassification::Provisioning).unwrap(),
            "\"provisioning\""
        );
        assert_eq!(
            serde_json::to_string(&AccountClassification::Pending).unwrap(),
            "\"pending\""
        );
        assert_eq!(
            serde_json::to_string(&AccountClassification::Lapsed).unwrap(),
            "\"lapsed\""
        );
        assert_eq!(
            serde_json::to_string(&AccountClassification::Unavailable).unwrap(),
            "\"unavailable\""
        );
    }

    #[test]
    fn test_account_classification_roundtrip() {
        for variant in &[
            AccountClassification::New,
            AccountClassification::Existing,
            AccountClassification::Incomplete,
            AccountClassification::Provisioning,
            AccountClassification::Pending,
            AccountClassification::Lapsed,
            AccountClassification::Unavailable,
        ] {
            let serialized = serde_json::to_value(variant).unwrap();
            let deserialized: AccountClassification =
                serde_json::from_value(serialized.clone()).unwrap();
            assert_eq!(*variant, deserialized);
        }
    }

    #[test]
    fn test_account_classification_unknown_rejects() {
        let json = json!("unknown_classification");
        let result: Result<AccountClassification, _> = serde_json::from_value(json);
        assert!(result.is_err());
    }

    #[test]
    fn test_entitlement_state_snake_case() {
        assert_eq!(
            serde_json::to_string(&EntitlementState::None).unwrap(),
            "\"none\""
        );
        assert_eq!(
            serde_json::to_string(&EntitlementState::Active).unwrap(),
            "\"active\""
        );
        assert_eq!(
            serde_json::to_string(&EntitlementState::Grace).unwrap(),
            "\"grace\""
        );
        assert_eq!(
            serde_json::to_string(&EntitlementState::BillingRetry).unwrap(),
            "\"billing_retry\""
        );
        assert_eq!(
            serde_json::to_string(&EntitlementState::Pending).unwrap(),
            "\"pending\""
        );
        assert_eq!(
            serde_json::to_string(&EntitlementState::Expired).unwrap(),
            "\"expired\""
        );
        assert_eq!(
            serde_json::to_string(&EntitlementState::Revoked).unwrap(),
            "\"revoked\""
        );
    }

    #[test]
    fn test_entitlement_state_roundtrip() {
        for variant in &[
            EntitlementState::None,
            EntitlementState::Active,
            EntitlementState::Grace,
            EntitlementState::BillingRetry,
            EntitlementState::Pending,
            EntitlementState::Expired,
            EntitlementState::Revoked,
        ] {
            let serialized = serde_json::to_value(variant).unwrap();
            let deserialized: EntitlementState =
                serde_json::from_value(serialized.clone()).unwrap();
            assert_eq!(*variant, deserialized);
        }
    }

    #[test]
    fn test_entitlement_state_unknown_rejects() {
        let json = json!("unknown_state");
        let result: Result<EntitlementState, _> = serde_json::from_value(json);
        assert!(result.is_err());
    }

    #[test]
    fn test_access_mode_snake_case() {
        assert_eq!(
            serde_json::to_string(&AccessMode::ReadWrite).unwrap(),
            "\"read_write\""
        );
        assert_eq!(
            serde_json::to_string(&AccessMode::ReadOnly).unwrap(),
            "\"read_only\""
        );
    }

    #[test]
    fn test_access_mode_roundtrip() {
        for variant in &[AccessMode::ReadWrite, AccessMode::ReadOnly] {
            let serialized = serde_json::to_value(variant).unwrap();
            let deserialized: AccessMode = serde_json::from_value(serialized.clone()).unwrap();
            assert_eq!(*variant, deserialized);
        }
    }

    #[test]
    fn test_access_mode_unknown_rejects() {
        let json = json!("unknown_mode");
        let result: Result<AccessMode, _> = serde_json::from_value(json);
        assert!(result.is_err());
    }

    #[test]
    fn test_error_code_snake_case() {
        assert_eq!(
            serde_json::to_string(&ErrorCode::Unavailable).unwrap(),
            "\"unavailable\""
        );
        assert_eq!(
            serde_json::to_string(&ErrorCode::Unauthorized).unwrap(),
            "\"unauthorized\""
        );
        assert_eq!(
            serde_json::to_string(&ErrorCode::Conflict).unwrap(),
            "\"conflict\""
        );
        assert_eq!(
            serde_json::to_string(&ErrorCode::InvalidRequest).unwrap(),
            "\"invalid_request\""
        );
        assert_eq!(
            serde_json::to_string(&ErrorCode::EntitlementRequired).unwrap(),
            "\"entitlement_required\""
        );
    }

    #[test]
    fn test_error_code_roundtrip() {
        for variant in &[
            ErrorCode::Unavailable,
            ErrorCode::Unauthorized,
            ErrorCode::Conflict,
            ErrorCode::InvalidRequest,
            ErrorCode::EntitlementRequired,
        ] {
            let serialized = serde_json::to_value(variant).unwrap();
            let deserialized: ErrorCode = serde_json::from_value(serialized.clone()).unwrap();
            assert_eq!(*variant, deserialized);
        }
    }

    #[test]
    fn test_error_code_unknown_rejects() {
        let json = json!("unknown_error");
        let result: Result<ErrorCode, _> = serde_json::from_value(json);
        assert!(result.is_err());
    }

    // ========================================================================
    // Auth DTO Tests
    // ========================================================================

    #[test]
    fn test_login_attempt_request_roundtrip() {
        let request = LoginAttemptRequest {
            provider: IdentityProvider::Google,
        };
        let json = serde_json::to_string(&request).unwrap();
        let deserialized: LoginAttemptRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.provider, IdentityProvider::Google);
    }

    #[test]
    fn test_login_attempt_request_unknown_fields_rejected() {
        let json_str = r#"{"provider":"google","extra":"field"}"#;
        let result: Result<LoginAttemptRequest, _> = serde_json::from_str(json_str);
        assert!(result.is_err());
    }

    #[test]
    fn test_login_attempt_response_roundtrip() {
        let attempt_id = Uuid::new_v4();
        let response = LoginAttemptResponse {
            attempt_id,
            nonce: "test_nonce".to_string(),
            expires_in_seconds: 300,
        };
        let json = serde_json::to_string(&response).unwrap();
        let deserialized: LoginAttemptResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.attempt_id, attempt_id);
        assert_eq!(deserialized.nonce, "test_nonce");
        assert_eq!(deserialized.expires_in_seconds, 300);
    }

    #[test]
    fn test_session_exchange_request_roundtrip() {
        let attempt_id = Uuid::new_v4();
        let request = SessionExchangeRequest {
            attempt_id,
            id_token: "eyJhbGc...".to_string(),
        };
        let json = serde_json::to_string(&request).unwrap();
        let deserialized: SessionExchangeRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.attempt_id, attempt_id);
        assert_eq!(deserialized.id_token, "eyJhbGc...");
    }

    #[test]
    fn test_session_exchange_request_unknown_fields_rejected() {
        let json_str = r#"{"attempt_id":"00000000-0000-0000-0000-000000000000","id_token":"token","extra":"field"}"#;
        let result: Result<SessionExchangeRequest, _> = serde_json::from_str(json_str);
        assert!(result.is_err());
    }

    #[test]
    fn test_session_response_roundtrip() {
        let account_id = Uuid::new_v4();
        let response = SessionResponse {
            session_token: "token_xyz".to_string(),
            expires_in_seconds: 3600,
            account_id,
        };
        let json = serde_json::to_string(&response).unwrap();
        let deserialized: SessionResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.session_token, "token_xyz");
        assert_eq!(deserialized.expires_in_seconds, 3600);
        assert_eq!(deserialized.account_id, account_id);
    }

    // ========================================================================
    // Account DTO Tests
    // ========================================================================

    #[test]
    fn test_account_response_full() {
        let account_id = Uuid::new_v4();
        let vault_id = Uuid::new_v4();
        let operation_id = Uuid::new_v4();

        let response = AccountResponse {
            account_id,
            classification: AccountClassification::Existing,
            entitlement: EntitlementState::Active,
            access: AccessMode::ReadWrite,
            vault_id: Some(vault_id),
            operation_id: Some(operation_id),
        };

        let json = serde_json::to_string(&response).unwrap();
        let deserialized: AccountResponse = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.account_id, account_id);
        assert_eq!(deserialized.classification, AccountClassification::Existing);
        assert_eq!(deserialized.entitlement, EntitlementState::Active);
        assert_eq!(deserialized.access, AccessMode::ReadWrite);
        assert_eq!(deserialized.vault_id, Some(vault_id));
        assert_eq!(deserialized.operation_id, Some(operation_id));
    }

    #[test]
    fn test_account_response_minimal() {
        let account_id = Uuid::new_v4();
        let response = AccountResponse {
            account_id,
            classification: AccountClassification::New,
            entitlement: EntitlementState::None,
            access: AccessMode::ReadOnly,
            vault_id: None,
            operation_id: None,
        };

        let json = serde_json::to_string(&response).unwrap();

        // Verify optional fields are omitted
        assert!(!json.contains("vault_id"));
        assert!(!json.contains("operation_id"));

        let deserialized: AccountResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.vault_id, None);
        assert_eq!(deserialized.operation_id, None);
    }

    #[test]
    fn test_account_response_unknown_classification_rejects() {
        let json_str = r#"{"account_id":"00000000-0000-0000-0000-000000000000","classification":"unknown","entitlement":"active","access":"read_write"}"#;
        let result: Result<AccountResponse, _> = serde_json::from_str(json_str);
        assert!(result.is_err());
    }

    #[test]
    fn test_account_response_unknown_entitlement_rejects() {
        let json_str = r#"{"account_id":"00000000-0000-0000-0000-000000000000","classification":"new","entitlement":"unknown","access":"read_write"}"#;
        let result: Result<AccountResponse, _> = serde_json::from_str(json_str);
        assert!(result.is_err());
    }

    #[test]
    fn test_account_response_unknown_access_rejects() {
        let json_str = r#"{"account_id":"00000000-0000-0000-0000-000000000000","classification":"new","entitlement":"active","access":"unknown"}"#;
        let result: Result<AccountResponse, _> = serde_json::from_str(json_str);
        assert!(result.is_err());
    }

    #[test]
    fn test_account_response_extra_fields_rejects() {
        let json_str = r#"{"account_id":"00000000-0000-0000-0000-000000000000","classification":"new","entitlement":"active","access":"read_write","extra_field":"should_fail"}"#;
        let result: Result<AccountResponse, _> = serde_json::from_str(json_str);
        assert!(result.is_err());
    }

    // ========================================================================
    // Billing DTO Tests
    // ========================================================================

    #[test]
    fn test_billing_session_request_roundtrip() {
        let operation_id = Uuid::new_v4();
        let request = BillingSessionRequest { operation_id };
        let json = serde_json::to_string(&request).unwrap();
        let deserialized: BillingSessionRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.operation_id, operation_id);
    }

    #[test]
    fn test_billing_session_request_unknown_fields_rejected() {
        let json_str = r#"{"operation_id":"00000000-0000-0000-0000-000000000000","extra":"field"}"#;
        let result: Result<BillingSessionRequest, _> = serde_json::from_str(json_str);
        assert!(result.is_err());
    }

    #[test]
    fn test_billing_session_response_roundtrip() {
        let response = BillingSessionResponse {
            url: "https://checkout.stripe.com/pay/session_xyz".to_string(),
        };
        let json = serde_json::to_string(&response).unwrap();
        let deserialized: BillingSessionResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(
            deserialized.url,
            "https://checkout.stripe.com/pay/session_xyz"
        );
    }

    // ========================================================================
    // Provisioning DTO Tests
    // ========================================================================

    #[test]
    fn test_provisioning_request_roundtrip() {
        let operation_id = Uuid::new_v4();
        let request = ProvisioningRequest { operation_id };
        let json = serde_json::to_string(&request).unwrap();
        let deserialized: ProvisioningRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.operation_id, operation_id);
    }

    #[test]
    fn test_provisioning_request_unknown_fields_rejected() {
        let json_str = r#"{"operation_id":"00000000-0000-0000-0000-000000000000","extra":"field"}"#;
        let result: Result<ProvisioningRequest, _> = serde_json::from_str(json_str);
        assert!(result.is_err());
    }

    #[test]
    fn test_provisioning_grant_response_roundtrip() {
        let response = ProvisioningGrantResponse {
            grant: "grant_token_123".to_string(),
            expires_in_seconds: 600,
        };
        let json = serde_json::to_string(&response).unwrap();
        let deserialized: ProvisioningGrantResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.grant, "grant_token_123");
        assert_eq!(deserialized.expires_in_seconds, 600);
    }

    #[test]
    fn test_complete_provisioning_request_roundtrip() {
        let operation_id = Uuid::new_v4();
        let device_id = Uuid::new_v4();
        let credential_digest = "a".repeat(64);
        let public_key_profile = json!({"curve": "Ed25519", "public_key": "..."});
        let encrypted_vault_check_header = "aGVsbG8gd29ybGQ=".to_string(); // base64
        let profile_fingerprint = "fingerprint_123".to_string();

        let request = CompleteProvisioningRequest {
            operation_id,
            grant: "grant_xyz".to_string(),
            device_id,
            credential_digest: credential_digest.clone(),
            public_key_profile,
            encrypted_vault_check_header: encrypted_vault_check_header.clone(),
            profile_fingerprint: profile_fingerprint.clone(),
            key_epoch: 1,
        };

        let json = serde_json::to_string(&request).unwrap();
        let deserialized: CompleteProvisioningRequest = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.operation_id, operation_id);
        assert_eq!(deserialized.device_id, device_id);
        assert_eq!(deserialized.credential_digest, credential_digest);
        assert_eq!(
            deserialized.encrypted_vault_check_header,
            encrypted_vault_check_header
        );
        assert_eq!(deserialized.profile_fingerprint, profile_fingerprint);
        assert_eq!(deserialized.key_epoch, 1);
    }

    #[test]
    fn test_complete_provisioning_request_unknown_fields_rejected() {
        let operation_id = Uuid::new_v4();
        let device_id = Uuid::new_v4();
        let request = CompleteProvisioningRequest {
            operation_id,
            grant: "grant_xyz".to_string(),
            device_id,
            credential_digest: "a".repeat(64),
            public_key_profile: json!({"curve": "Ed25519"}),
            encrypted_vault_check_header: "aGVsbG8gd29ybGQ=".to_string(),
            profile_fingerprint: "fp".to_string(),
            key_epoch: 1,
        };
        let mut json_obj = serde_json::to_value(&request)
            .unwrap()
            .as_object()
            .unwrap()
            .clone();
        json_obj.insert("extra".to_string(), json!("field"));
        let result: Result<CompleteProvisioningRequest, _> =
            serde_json::from_value(serde_json::Value::Object(json_obj));
        assert!(result.is_err());
    }

    #[test]
    fn test_complete_provisioning_response_roundtrip() {
        let operation_id = Uuid::new_v4();
        let vault_id = Uuid::new_v4();
        let device_id = Uuid::new_v4();

        let response = CompleteProvisioningResponse {
            operation_id,
            vault_id,
            device_id,
            already_provisioned: false,
        };

        let json = serde_json::to_string(&response).unwrap();
        let deserialized: CompleteProvisioningResponse = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.operation_id, operation_id);
        assert_eq!(deserialized.vault_id, vault_id);
        assert_eq!(deserialized.device_id, device_id);
        assert!(!deserialized.already_provisioned);
    }

    // ========================================================================
    // Route Constants Tests
    // ========================================================================

    #[test]
    fn test_route_constants() {
        assert_eq!(routes::AUTH_ATTEMPTS, "/hosted/v1/auth/attempts");
        assert_eq!(routes::AUTH_SESSION, "/hosted/v1/auth/session");
        assert_eq!(routes::ACCOUNT, "/hosted/v1/account");
        assert_eq!(routes::BILLING_CHECKOUT, "/hosted/v1/billing/checkout");
        assert_eq!(routes::BILLING_PORTAL, "/hosted/v1/billing/portal");
        assert_eq!(routes::BILLING_WEBHOOK, "/hosted/v1/billing/webhook");
        assert_eq!(routes::PROVISIONING, "/hosted/v1/provisioning");
        assert_eq!(
            routes::PROVISIONING_COMPLETE,
            "/hosted/v1/provisioning/complete"
        );
    }
}
pub mod auth_web;
pub mod billing_web;
