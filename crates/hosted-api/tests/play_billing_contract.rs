use peppy_hosted_api::billing_play::{
    PlayBillingContextResponse, PlayBillingLinkOutcome, PlayBillingLinkResponse,
    PlayBillingProvider, routes,
};
use serde_json::json;
use uuid::Uuid;

#[test]
fn play_billing_context_and_link_responses_are_account_bound_and_forward_compatible() {
    let account_id = Uuid::new_v4();
    let context: PlayBillingContextResponse = serde_json::from_value(json!({
        "account_id": account_id,
        "available": false,
        "product_id": "peppy_pro",
        "base_plan_ids": ["monthly", "annual"],
        "account_binding": "play_binding_opaque",
        "current_provider": "play",
        "future_field": true
    }))
    .unwrap();
    assert_eq!(context.account_id, account_id);
    assert_eq!(context.current_provider, PlayBillingProvider::Play);

    let link: PlayBillingLinkResponse = serde_json::from_value(json!({
        "account_id": account_id,
        "outcome": "future_outcome",
        "future_field": true
    }))
    .unwrap();
    assert_eq!(link.outcome, PlayBillingLinkOutcome::Unknown);
    assert_eq!(routes::CONTEXT, "/hosted/v1/play/billing/context");
    assert_eq!(routes::LINK, "/hosted/v1/play/billing/link");
}
