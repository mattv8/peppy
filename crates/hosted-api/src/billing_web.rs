//! Public wire contracts for the hosted billing web client.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Required for billing mutations after the account's first binding retirement.
pub const BILLING_BINDING_HEADER: &str = "X-Peppy-Billing-Binding";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WebProviderLane {
    Primary,
    Sandbox,
}

#[derive(Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WebBillingContextResponse {
    pub binding_id: Uuid,
    pub provider_lane: WebProviderLane,
    pub config: crate::auth_web::WebConfigResponse,
    pub summary: BillingSummary,
}

pub mod routes {
    pub const CONTEXT: &str = "/hosted/v1/web/billing/context";
    pub const BILLING: &str = "/hosted/v1/web/billing";
    pub const PLANS: &str = "/hosted/v1/web/billing/plans";
    pub const SUBSCRIBE: &str = "/hosted/v1/web/billing/subscribe";
    pub const PAYMENT_ACTION: &str = "/hosted/v1/web/billing/payment-action";
    pub const RECONCILE: &str = "/hosted/v1/web/billing/reconcile";
    pub const PAYMENT_METHOD: &str = "/hosted/v1/web/billing/payment-method";
    pub const PAYMENT_METHOD_CONFIRM: &str = "/hosted/v1/web/billing/payment-method/confirm";
    pub const CANCEL: &str = "/hosted/v1/web/billing/cancel";
    pub const RESUME: &str = "/hosted/v1/web/billing/resume";
    pub const INVOICES: &str = "/hosted/v1/web/billing/invoices";
    pub const STORAGE_PACK: &str = "/hosted/v1/web/billing/storage-pack";
    pub const ATTACHMENT_STORAGE_STATUS: &str =
        crate::attachments::routes::ATTACHMENT_STORAGE_STATUS;
    pub const RETENTION_NOTICE_ACKNOWLEDGEMENT: &str =
        crate::attachments::routes::RETENTION_NOTICE_ACKNOWLEDGEMENT;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[derive(schemars::JsonSchema)]
pub struct BillingOperationRequest {
    pub operation_id: Uuid,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[derive(schemars::JsonSchema)]
pub struct SubscribeRequest {
    pub operation_id: Uuid,
    /// The selected, configured quote; stale or unconfigured quotes are rejected.
    pub expected_price_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[derive(schemars::JsonSchema)]
pub struct PaymentMethodConfirmRequest {
    pub operation_id: Uuid,
    pub setup_intent_id: String,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
#[derive(schemars::JsonSchema)]
pub enum SubscriptionAction {
    Payment { client_secret: String },
    Setup { client_secret: String },
    None,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[derive(schemars::JsonSchema)]
pub struct SubscribeResponse {
    pub subscription_id: String,
    pub next_action: SubscriptionAction,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[derive(schemars::JsonSchema)]
pub struct StoragePackRequest {
    pub operation_id: Uuid,
    /// Reconciled quantity displayed to the user when this edit was made.
    pub expected_quantity: i64,
    pub quantity: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[derive(schemars::JsonSchema)]
pub struct InvoiceSummary {
    pub id: String,
    pub status: String,
    pub amount_due: i64,
    pub amount_paid: i64,
    pub currency: String,
    pub created_at: i64,
    pub hosted_invoice_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[derive(schemars::JsonSchema)]
pub struct BillingSummary {
    pub subscription_id: Option<String>,
    pub subscription_status: String,
    pub paid_through: i64,
    pub cancel_at_period_end: bool,
    pub access: String,
    pub price_id: String,
    pub plan: PlanSummary,
    pub invoices: Vec<InvoiceSummary>,
    pub storage: StorageSummary,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StorageSummary {
    pub used_bytes: i64,
    pub limit_bytes: i64,
    pub base_bytes: i64,
    pub pack_quantity: i64,
    pub pack_bytes: i64,
    pub pack_max_quantity: Option<i64>,
    pub pack_purchase_available: bool,
    pub pack_pending: bool,
    pub pack_mutation_blocked_reason: Option<String>,
    pub pack: Option<PlanSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[derive(schemars::JsonSchema)]
pub struct PlanSummary {
    pub price_id: String,
    pub product_name: String,
    pub unit_amount: i64,
    pub currency: String,
    pub interval: String,
    pub interval_count: i64,
    pub discount_percent: Option<i64>,
    pub comped: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[derive(schemars::JsonSchema)]
pub struct BillingPlans {
    pub default_price_id: String,
    pub plans: Vec<PlanSummary>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[derive(schemars::JsonSchema)]
pub struct PaymentMethodSetupResponse {
    pub setup_intent_id: String,
    pub client_secret: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binding_context_adds_no_required_fields_to_legacy_mutations() {
        let request: BillingOperationRequest =
            serde_json::from_str(r#"{"operation_id":"aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"}"#)
                .unwrap();
        assert_eq!(
            serde_json::to_value(request)
                .unwrap()
                .as_object()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(BILLING_BINDING_HEADER, "X-Peppy-Billing-Binding");
        assert_eq!(routes::CONTEXT, "/hosted/v1/web/billing/context");
        assert_eq!(
            serde_json::to_value(WebProviderLane::Primary).unwrap(),
            "primary"
        );
        assert_eq!(
            serde_json::to_value(WebProviderLane::Sandbox).unwrap(),
            "sandbox"
        );
        assert!(serde_json::from_str::<WebProviderLane>("\"other\"").is_err());
    }

    #[test]
    fn context_roundtrip_bundles_binding_lane_configuration_and_summary() {
        let plan = serde_json::json!({"price_id":"price_test","product_name":"Test",
            "unit_amount":100,"currency":"usd","interval":"month","interval_count":1,
            "discount_percent":null,"comped":false});
        let input = serde_json::json!({
            "binding_id":"aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa","provider_lane":"sandbox",
            "config":{"stripe_publishable_key":"pk_test","available_providers":["google"]},
            "summary":{"subscription_id":null,"subscription_status":"none","paid_through":0,
                "cancel_at_period_end":false,"access":"read_only","price_id":"price_test",
                "plan":plan,"invoices":[],"storage":{"used_bytes":0,"limit_bytes":100,
                    "base_bytes":100,"pack_quantity":0,"pack_bytes":100,"pack_max_quantity":null,
                    "pack_purchase_available":false,"pack_pending":false,
                    "pack_mutation_blocked_reason":null,"pack":null}}
        });
        let parsed: WebBillingContextResponse = serde_json::from_value(input.clone()).unwrap();
        assert_eq!(serde_json::to_value(parsed).unwrap(), input);
    }

    #[test]
    fn subscription_action_keeps_secrets_out_of_zero_charge_response() {
        let json = serde_json::to_string(&SubscriptionAction::None).unwrap();
        assert_eq!(json, r#"{"kind":"none"}"#);
        let parsed: SubscriptionAction = serde_json::from_str(&json).unwrap();
        assert!(matches!(parsed, SubscriptionAction::None));
    }

    #[test]
    fn web_billing_routes_are_distinct_from_legacy_checkout() {
        assert_eq!(routes::SUBSCRIBE, "/hosted/v1/web/billing/subscribe");
        assert_eq!(routes::PLANS, "/hosted/v1/web/billing/plans");
        assert_eq!(
            routes::PAYMENT_ACTION,
            "/hosted/v1/web/billing/payment-action"
        );
        assert_eq!(
            routes::PAYMENT_METHOD_CONFIRM,
            "/hosted/v1/web/billing/payment-method/confirm"
        );
    }
}
