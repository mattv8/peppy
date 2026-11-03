//! Public wire contracts for native hosted billing and App Store reconciliation.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::billing_web::{BillingManageableIn, BillingProvider};

pub mod routes {
    pub const CATALOG: &str = "/hosted/v1/billing/catalog";
    pub const APPLE_ACCOUNT_TOKEN: &str = "/hosted/v1/billing/apple/account-token";
    pub const APPLE_TRANSACTIONS: &str = "/hosted/v1/billing/apple/transactions";
    pub const APPLE_NOTIFICATIONS: &str = "/hosted/v1/billing/apple/notifications";
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BillingCatalogResponse {
    #[serde(default)]
    pub summary: BillingAccountSummary,
    pub products: Vec<BillingCatalogProduct>,
    pub eligibility: BillingEligibility,
    #[serde(default)]
    pub storage_pack_bytes: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conflict_reason: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct BillingAccountSummary {
    pub provider: BillingProvider,
    pub manageable_in: BillingManageableIn,
    pub plan_active: bool,
    pub storage_packs: i64,
    pub storage_renews_without_plan: bool,
    pub deletion_pending: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BillingCatalogProduct {
    pub id: String,
    pub kind: BillingProductKind,
    pub provider: BillingProvider,
    pub interval: Option<String>,
    pub storage_pack_count: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum BillingProductKind {
    Plan,
    Storage,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum BillingEligibility {
    Eligible,
    Ineligible,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppleAccountTokenResponse {
    pub token: Uuid,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppleTransactionRequest {
    pub signed_transaction: String,
}

// Intentionally no Debug for AppleTransactionRequest (signed provider payload)

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AppleTransactionResult {
    Granted,
    Pending,
    Conflict,
    Rejected,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppleTransactionResponse {
    pub result: AppleTransactionResult,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppleNotificationRequest {
    pub signed_payload: String,
}

// Intentionally no Debug for AppleNotificationRequest (signed provider payload)

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routes_match_the_native_billing_contract() {
        assert_eq!(routes::CATALOG, "/hosted/v1/billing/catalog");
        assert_eq!(
            routes::APPLE_ACCOUNT_TOKEN,
            "/hosted/v1/billing/apple/account-token"
        );
        assert_eq!(
            routes::APPLE_TRANSACTIONS,
            "/hosted/v1/billing/apple/transactions"
        );
        assert_eq!(
            routes::APPLE_NOTIFICATIONS,
            "/hosted/v1/billing/apple/notifications"
        );
    }

    #[test]
    fn transaction_result_uses_the_documented_values() {
        assert_eq!(
            serde_json::to_value(AppleTransactionResult::Granted).unwrap(),
            "granted"
        );
        assert_eq!(
            serde_json::to_value(AppleTransactionResult::Pending).unwrap(),
            "pending"
        );
        assert_eq!(
            serde_json::to_value(AppleTransactionResult::Conflict).unwrap(),
            "conflict"
        );
        assert_eq!(
            serde_json::to_value(AppleTransactionResult::Rejected).unwrap(),
            "rejected"
        );
    }
}
