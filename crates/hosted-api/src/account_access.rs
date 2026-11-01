//! Wire contract for the device-authenticated account-access notice.
//!
//! The server reports local billing and write-access state without provider I/O.
//! Clients turn it into user-facing notices through the shared decision function in
//! `peppy-hosted-client`; this module only defines the bytes on the wire.

use serde::{Deserialize, Serialize};

pub mod error_codes {
    pub const UNSUPPORTED: &str = "account_access_unsupported";
    pub const FORBIDDEN: &str = "account_access_forbidden";
    pub const UNAVAILABLE: &str = "account_access_unavailable";
}

pub mod routes {
    pub const DEVICE_ACCOUNT_ACCESS: &str = "/v1/account/access";
}

/// Write access the server enforces for the account.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccountAccessLevel {
    ReadWrite,
    ReadOnly,
    /// Lets older clients tolerate values added by newer servers. Never emitted by the server.
    #[serde(other)]
    Unknown,
}

/// Billing or lifecycle state the account is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccountAccessState {
    Ok,
    PaymentFailedGrace,
    EndingScheduled,
    ReadOnly,
    /// Lets older clients tolerate values added by newer servers. Never emitted by the server.
    #[serde(other)]
    Unknown,
}

/// Why the account is read-only, when the server knows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccountAccessReason {
    PastDue,
    Canceled,
    Incomplete,
    NoSubscription,
    TesterIneligible,
    AccountDeleting,
    /// Lets older clients tolerate values added by newer servers. Never emitted by the server.
    #[serde(other)]
    Unknown,
}

/// Response body for `GET /v1/account/access`.
///
/// Unknown fields are ignored so the response can grow without breaking older clients.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountAccessResponse {
    pub access: AccountAccessLevel,
    pub state: AccountAccessState,
    pub reason: Option<AccountAccessReason>,
    /// Unix seconds, matching the other hosted timestamps such as `presented_at`.
    pub deadline: Option<i64>,
    pub manage_url: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn device_account_access_route_is_stable() {
        assert_eq!(routes::DEVICE_ACCOUNT_ACCESS, "/v1/account/access");
    }

    #[test]
    fn known_values_serialize_as_snake_case() {
        let response = AccountAccessResponse {
            access: AccountAccessLevel::ReadOnly,
            state: AccountAccessState::PaymentFailedGrace,
            reason: Some(AccountAccessReason::PastDue),
            deadline: Some(1_700_000_000),
            manage_url: Some("https://peppy.pro/account".into()),
        };
        assert_eq!(
            serde_json::to_value(&response).unwrap(),
            json!({
                "access": "read_only",
                "state": "payment_failed_grace",
                "reason": "past_due",
                "deadline": 1_700_000_000,
                "manage_url": "https://peppy.pro/account"
            })
        );
    }

    #[test]
    fn every_known_reason_round_trips_with_its_wire_name() {
        for (reason, wire) in [
            (AccountAccessReason::PastDue, "past_due"),
            (AccountAccessReason::Canceled, "canceled"),
            (AccountAccessReason::Incomplete, "incomplete"),
            (AccountAccessReason::NoSubscription, "no_subscription"),
            (AccountAccessReason::TesterIneligible, "tester_ineligible"),
            (AccountAccessReason::AccountDeleting, "account_deleting"),
        ] {
            assert_eq!(serde_json::to_value(reason).unwrap(), json!(wire));
            assert_eq!(
                serde_json::from_value::<AccountAccessReason>(json!(wire)).unwrap(),
                reason
            );
        }
    }

    #[test]
    fn unrecognised_enum_values_decode_as_unknown() {
        let response: AccountAccessResponse = serde_json::from_value(json!({
            "access": "future_level",
            "state": "future_state",
            "reason": "future_reason",
            "deadline": null,
            "manage_url": null
        }))
        .unwrap();

        assert_eq!(response.access, AccountAccessLevel::Unknown);
        assert_eq!(response.state, AccountAccessState::Unknown);
        assert_eq!(response.reason, Some(AccountAccessReason::Unknown));
    }

    #[test]
    fn unknown_fields_and_absent_optional_fields_are_tolerated() {
        let response: AccountAccessResponse = serde_json::from_value(json!({
            "access": "read_write",
            "state": "ok",
            "future_field": {"nested": true}
        }))
        .unwrap();

        assert_eq!(response.access, AccountAccessLevel::ReadWrite);
        assert_eq!(response.state, AccountAccessState::Ok);
        assert_eq!(response.reason, None);
        assert_eq!(response.deadline, None);
        assert_eq!(response.manage_url, None);
    }
}
