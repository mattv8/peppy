use peppy_hosted_client::play_billing::{
    PlayBillingContextAvailability, PlayBillingLinkResult, build_play_billing_link_request,
    parse_play_billing_context, parse_play_billing_link_response,
};
use uuid::Uuid;

#[test]
fn parser_rejects_mismatched_accounts_and_unknown_or_inconsistent_contexts_cannot_purchase() {
    let account_id = Uuid::new_v4();
    let context = serde_json::json!({
        "account_id": account_id,
        "available": true,
        "product_id": "peppy_pro",
        "base_plan_ids": ["monthly"],
        "account_binding": "opaque-binding",
        "current_provider": "none"
    });
    assert!(matches!(
        parse_play_billing_context(&context.to_string(), account_id),
        Ok(context) if context.availability == PlayBillingContextAvailability::Available
    ));
    assert!(parse_play_billing_context(&context.to_string(), Uuid::new_v4()).is_err());

    let unknown_provider = context
        .as_object()
        .unwrap()
        .iter()
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect::<serde_json::Map<_, _>>();
    let mut unknown_provider = serde_json::Value::Object(unknown_provider);
    unknown_provider["current_provider"] = "future_provider".into();
    assert!(matches!(
        parse_play_billing_context(&unknown_provider.to_string(), account_id),
        Ok(context) if context.availability == PlayBillingContextAvailability::Unavailable
    ));

    let link = serde_json::json!({"account_id": account_id, "outcome": "linked"});
    assert_eq!(
        parse_play_billing_link_response(&link.to_string(), account_id).unwrap(),
        PlayBillingLinkResult::Linked
    );
    assert!(parse_play_billing_link_response(&link.to_string(), Uuid::new_v4()).is_err());

    let token = "play-token".repeat(32);
    let request = build_play_billing_link_request(Uuid::new_v4(), "peppy_pro", &token).unwrap();
    assert!(request.contains(&token));
}
