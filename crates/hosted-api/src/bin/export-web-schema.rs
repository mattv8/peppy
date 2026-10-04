//! Emits the public browser contract; private frontend generation consumes this.
use peppy_hosted_api::{auth_web as auth, billing_web as billing};

#[derive(schemars::JsonSchema)]
#[allow(dead_code)]
struct HostedWebContracts {
    session: auth::WebSessionResponse,
    config: auth::WebConfigResponse,
    account: auth::WebAccountResponse,
    billing: billing::BillingSummary,
    plan_catalog: billing::BillingPlans,
    operation: billing::BillingOperationRequest,
    subscribe_request: billing::SubscribeRequest,
    subscribe_response: billing::SubscribeResponse,
    payment_method_confirm: billing::PaymentMethodConfirmRequest,
    payment_method_setup: billing::PaymentMethodSetupResponse,
}

fn main() {
    let mut schema = serde_json::to_value(schemars::schema_for!(HostedWebContracts)).unwrap();
    schema["x-peppy-routes"] = serde_json::json!({
        "session": auth::routes::SESSION, "authStart": auth::routes::AUTH_START,
        "logout": auth::routes::LOGOUT, "account": auth::routes::ACCOUNT,
        "config": auth::routes::CONFIG, "billing": billing::routes::BILLING,
        "plans": billing::routes::PLANS,
        "subscribe": billing::routes::SUBSCRIBE,
        "paymentAction": billing::routes::PAYMENT_ACTION,
        "reconcile": billing::routes::RECONCILE,
        "paymentMethod": billing::routes::PAYMENT_METHOD,
        "paymentMethodConfirm": billing::routes::PAYMENT_METHOD_CONFIRM,
        "cancel": billing::routes::CANCEL, "resume": billing::routes::RESUME,
        "invoices": billing::routes::INVOICES,
    });
    println!("{}", serde_json::to_string_pretty(&schema).unwrap());
}
