//! Public wire contracts for the hosted billing web client.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub mod routes {
    pub const BILLING: &str = "/hosted/v1/web/billing";
    pub const SUBSCRIBE: &str = "/hosted/v1/web/billing/subscribe";
    pub const PAYMENT_ACTION: &str = "/hosted/v1/web/billing/payment-action";
    pub const RECONCILE: &str = "/hosted/v1/web/billing/reconcile";
    pub const PAYMENT_METHOD: &str = "/hosted/v1/web/billing/payment-method";
    pub const PAYMENT_METHOD_CONFIRM: &str = "/hosted/v1/web/billing/payment-method/confirm";
    pub const CANCEL: &str = "/hosted/v1/web/billing/cancel";
    pub const RESUME: &str = "/hosted/v1/web/billing/resume";
    pub const INVOICES: &str = "/hosted/v1/web/billing/invoices";
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
    /// A stale quote is rejected; this never selects the server-side price.
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
    fn subscription_action_keeps_secrets_out_of_zero_charge_response() {
        let json = serde_json::to_string(&SubscriptionAction::None).unwrap();
        assert_eq!(json, r#"{"kind":"none"}"#);
        let parsed: SubscriptionAction = serde_json::from_str(&json).unwrap();
        assert!(matches!(parsed, SubscriptionAction::None));
    }

    #[test]
    fn web_billing_routes_are_distinct_from_legacy_checkout() {
        assert_eq!(routes::SUBSCRIBE, "/hosted/v1/web/billing/subscribe");
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
