//! Parsing and request construction for hosted Google Play billing.
//!
//! Hosts own HTTP, session fencing, cancellation, secure persistence, and the
//! Play Billing client. This module never interprets a purchase token as an
//! entitlement; it only encodes the account-bound server contract.

use peppy_hosted_api::billing_play::{
    PlayBillingContextResponse, PlayBillingLinkOutcome, PlayBillingLinkRequest,
    PlayBillingLinkResponse, PlayBillingProvider, routes,
};
use uuid::Uuid;

pub const MAX_PLAY_BILLING_RESPONSE_JSON: usize = 8 * 1024;
pub const MAX_PLAY_PRODUCT_ID_BYTES: usize = 128;
pub const MAX_PLAY_BASE_PLAN_IDS: usize = 16;
pub const MAX_PLAY_ACCOUNT_BINDING_BYTES: usize = 64;
pub const MAX_PLAY_PURCHASE_TOKEN_BYTES: usize = 4096;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PlayBillingCodecError {
    #[error("invalid Play billing contract")]
    InvalidContract,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlayBillingContextAvailability {
    Available,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlayBillingContext {
    pub account_id: Uuid,
    /// Available means a new Play purchase may start; it is not a restore flag.
    pub availability: PlayBillingContextAvailability,
    pub product_id: Option<String>,
    pub base_plan_ids: Vec<String>,
    pub account_binding: Option<String>,
    pub current_provider: PlayBillingProvider,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlayBillingLinkResult {
    Linked,
    Pending,
    BoundToOtherAccount,
    NotEntitling,
    Unknown,
}

pub fn play_billing_context_path() -> &'static str {
    routes::CONTEXT
}

pub fn play_billing_link_path() -> &'static str {
    routes::LINK
}

/// Builds the request body. `operation_id` is request deduplication only;
/// private-platform idempotency must be bound to the purchase token.
pub fn build_play_billing_link_request(
    operation_id: Uuid,
    product_id: &str,
    purchase_token: &str,
) -> Result<String, PlayBillingCodecError> {
    if !valid_product_id(product_id) || !valid_purchase_token(purchase_token) {
        return Err(PlayBillingCodecError::InvalidContract);
    }
    serde_json::to_string(&PlayBillingLinkRequest {
        operation_id,
        product_id: product_id.into(),
        purchase_token: purchase_token.into(),
    })
    .map_err(|_| PlayBillingCodecError::InvalidContract)
}

pub fn parse_play_billing_context(
    response_json: &str,
    expected_account_id: Uuid,
) -> Result<PlayBillingContext, PlayBillingCodecError> {
    let response: PlayBillingContextResponse = parse_response(response_json)?;
    if response.account_id != expected_account_id
        || response.base_plan_ids.len() > MAX_PLAY_BASE_PLAN_IDS
        || !response.product_id.as_deref().is_none_or(valid_product_id)
        || !response.base_plan_ids.iter().all(|id| valid_product_id(id))
        || !response
            .account_binding
            .as_deref()
            .is_none_or(valid_account_binding)
    {
        return Err(PlayBillingCodecError::InvalidContract);
    }

    let is_purchase_ready = response.available
        && response.current_provider == PlayBillingProvider::None
        && response.product_id.is_some()
        && !response.base_plan_ids.is_empty()
        && response.account_binding.is_some();
    Ok(PlayBillingContext {
        account_id: response.account_id,
        availability: if is_purchase_ready {
            PlayBillingContextAvailability::Available
        } else {
            PlayBillingContextAvailability::Unavailable
        },
        product_id: response.product_id,
        base_plan_ids: response.base_plan_ids,
        account_binding: response.account_binding,
        current_provider: response.current_provider,
    })
}

pub fn parse_play_billing_link_response(
    response_json: &str,
    expected_account_id: Uuid,
) -> Result<PlayBillingLinkResult, PlayBillingCodecError> {
    let response: PlayBillingLinkResponse = parse_response(response_json)?;
    if response.account_id != expected_account_id {
        return Err(PlayBillingCodecError::InvalidContract);
    }
    Ok(match response.outcome {
        PlayBillingLinkOutcome::Linked => PlayBillingLinkResult::Linked,
        PlayBillingLinkOutcome::Pending => PlayBillingLinkResult::Pending,
        PlayBillingLinkOutcome::BoundToOtherAccount => PlayBillingLinkResult::BoundToOtherAccount,
        PlayBillingLinkOutcome::NotEntitling => PlayBillingLinkResult::NotEntitling,
        PlayBillingLinkOutcome::Unknown => PlayBillingLinkResult::Unknown,
    })
}

fn parse_response<T: for<'de> serde::Deserialize<'de>>(
    response_json: &str,
) -> Result<T, PlayBillingCodecError> {
    if response_json.is_empty() || response_json.len() > MAX_PLAY_BILLING_RESPONSE_JSON {
        return Err(PlayBillingCodecError::InvalidContract);
    }
    serde_json::from_str(response_json).map_err(|_| PlayBillingCodecError::InvalidContract)
}

fn valid_product_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_PLAY_PRODUCT_ID_BYTES
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

fn valid_account_binding(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_PLAY_ACCOUNT_BINDING_BYTES
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

fn valid_purchase_token(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_PLAY_PURCHASE_TOKEN_BYTES
        && value.bytes().all(|byte| byte.is_ascii_graphic())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn context(account_id: Uuid) -> serde_json::Value {
        json!({
            "account_id": account_id,
            "available": true,
            "product_id": "peppy_pro",
            "base_plan_ids": ["monthly"],
            "account_binding": "stable_opaque_binding",
            "current_provider": "none"
        })
    }

    #[test]
    fn context_only_allows_new_purchases_when_every_safe_precondition_holds() {
        let account_id = Uuid::new_v4();
        assert_eq!(
            parse_play_billing_context(&context(account_id).to_string(), account_id)
                .unwrap()
                .availability,
            PlayBillingContextAvailability::Available
        );
        for (field, value) in [
            ("current_provider", json!("stripe")),
            ("current_provider", json!("future_provider")),
            ("account_binding", serde_json::Value::Null),
            ("product_id", serde_json::Value::Null),
        ] {
            let mut invalid = context(account_id);
            invalid[field] = value;
            assert_eq!(
                parse_play_billing_context(&invalid.to_string(), account_id)
                    .unwrap()
                    .availability,
                PlayBillingContextAvailability::Unavailable
            );
        }
    }

    #[test]
    fn unavailable_context_retains_play_restore_metadata() {
        let account_id = Uuid::new_v4();
        let mut response = context(account_id);
        response["available"] = false.into();
        response["current_provider"] = "play".into();
        let parsed = parse_play_billing_context(&response.to_string(), account_id).unwrap();
        assert_eq!(
            parsed.availability,
            PlayBillingContextAvailability::Unavailable
        );
        assert_eq!(parsed.product_id.as_deref(), Some("peppy_pro"));
        assert_eq!(parsed.current_provider, PlayBillingProvider::Play);
    }

    #[test]
    fn parsers_reject_empty_and_oversized_response_bodies() {
        let account_id = Uuid::new_v4();
        let oversized = "x".repeat(MAX_PLAY_BILLING_RESPONSE_JSON + 1);
        assert!(parse_play_billing_context("", account_id).is_err());
        assert!(parse_play_billing_context(&oversized, account_id).is_err());
        assert!(parse_play_billing_link_response("", account_id).is_err());
        assert!(parse_play_billing_link_response(&oversized, account_id).is_err());
    }

    #[test]
    fn available_context_with_play_provider_or_no_base_plan_is_not_purchasable() {
        let account_id = Uuid::new_v4();
        for (field, value) in [
            ("current_provider", json!("play")),
            ("base_plan_ids", json!([])),
        ] {
            let mut response = context(account_id);
            response[field] = value;
            assert_eq!(
                parse_play_billing_context(&response.to_string(), account_id)
                    .unwrap()
                    .availability,
                PlayBillingContextAvailability::Unavailable
            );
        }
    }

    #[test]
    fn parser_rejects_invalid_play_context_metadata() {
        let account_id = Uuid::new_v4();
        let oversized_binding = "a".repeat(65);
        for binding in ["", oversized_binding.as_str(), "binding/invalid", "é"] {
            let mut response = context(account_id);
            response["account_binding"] = binding.into();
            assert!(parse_play_billing_context(&response.to_string(), account_id).is_err());
        }

        for (field, value) in [
            ("product_id", json!("invalid product")),
            ("base_plan_ids", json!(["invalid plan"])),
            (
                "base_plan_ids",
                json!(vec!["monthly"; MAX_PLAY_BASE_PLAN_IDS + 1]),
            ),
        ] {
            let mut response = context(account_id);
            response[field] = value;
            assert!(parse_play_billing_context(&response.to_string(), account_id).is_err());
        }
    }

    #[test]
    fn link_request_enforces_product_and_purchase_token_boundaries() {
        assert!(
            build_play_billing_link_request(Uuid::new_v4(), "invalid product", "token").is_err()
        );
        for token in ["", "has space", "é"] {
            assert!(build_play_billing_link_request(Uuid::new_v4(), "peppy_pro", token).is_err());
        }
        assert!(
            build_play_billing_link_request(
                Uuid::new_v4(),
                "peppy_pro",
                &"x".repeat(MAX_PLAY_PURCHASE_TOKEN_BYTES)
            )
            .is_ok()
        );
        assert!(
            build_play_billing_link_request(
                Uuid::new_v4(),
                "peppy_pro",
                &"x".repeat(MAX_PLAY_PURCHASE_TOKEN_BYTES + 1)
            )
            .is_err()
        );
    }

    #[test]
    fn unknown_link_outcomes_fail_closed() {
        let account_id = Uuid::new_v4();
        let response = json!({"account_id": account_id, "outcome": "later"});
        assert_eq!(
            parse_play_billing_link_response(&response.to_string(), account_id).unwrap(),
            PlayBillingLinkResult::Unknown
        );
    }
}
