#![doc = include_str!("../PLAY_BILLING.md")]

use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub mod routes {
    pub const CONTEXT: &str = "/hosted/v1/play/billing/context";
    pub const LINK: &str = "/hosted/v1/play/billing/link";
}

/// Provider currently associated with the hosted account.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PlayBillingProvider {
    None,
    Stripe,
    Play,
    /// A future provider must not enable a purchase in older clients.
    #[serde(other)]
    Unknown,
}

/// Response to the hosted Play billing context endpoint.
///
/// `available` means that this account may start a *new* Play purchase. It is
/// false for active Stripe or Play subscriptions, but existing Play metadata is
/// retained for restore and management flows.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PlayBillingContextResponse {
    pub account_id: Uuid,
    pub available: bool,
    pub product_id: Option<String>,
    pub base_plan_ids: Vec<String>,
    /// Lifetime-stable opaque server value; never an account UUID or derivation.
    pub account_binding: Option<String>,
    pub current_provider: PlayBillingProvider,
}

/// Request to associate a verified Google Play purchase with the account.
///
/// `operation_id` only deduplicates this request attempt. The private platform
/// binds idempotency to `purchase_token`, so a same-token relink after reinstall
/// succeeds even with a fresh operation ID.
///
/// ```compile_fail
/// use peppy_hosted_api::billing_play::PlayBillingLinkRequest;
///
/// fn must_not_log(request: PlayBillingLinkRequest) {
///     let _ = format!("{request:?}");
/// }
/// ```
#[derive(Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PlayBillingLinkRequest {
    pub operation_id: Uuid,
    pub product_id: String,
    /// Deliberately does not implement `Debug`: it is provider credential material.
    pub purchase_token: String,
}

/// Result of a link attempt. Unknown values are safe only as a non-entitling outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PlayBillingLinkOutcome {
    Linked,
    Pending,
    BoundToOtherAccount,
    NotEntitling,
    #[serde(other)]
    Unknown,
}

/// Account-bound response to a Play purchase link attempt.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PlayBillingLinkResponse {
    pub account_id: Uuid,
    pub outcome: PlayBillingLinkOutcome,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn purchase_token_request_serializes_the_canonical_link_payload() {
        let request = PlayBillingLinkRequest {
            operation_id: Uuid::nil(),
            product_id: "peppy_pro".into(),
            purchase_token: "secret_purchase_token".into(),
        };
        assert_eq!(
            serde_json::to_value(request).unwrap(),
            json!({
                "operation_id": "00000000-0000-0000-0000-000000000000",
                "product_id": "peppy_pro",
                "purchase_token": "secret_purchase_token"
            })
        );
    }
}
