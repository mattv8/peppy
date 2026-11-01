//! Sanitized account-access notice DTOs shared by desktop and browser worker hosts.
//!
//! Hosts transport the decision from `peppy_hosted_client::account_access`; they never
//! derive notice kind, severity, dismissibility, or URL trust themselves.

use peppy_hosted_client::account_access::{
    AccountAccessOutcome, AccountNotice, AccountNoticeKind, NoticeSeverity, account_access_outcome,
};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum AccountNoticeKindDto {
    PaymentFailedGrace,
    EndingScheduled,
    ReadOnlyPastDue,
    ReadOnlyCanceled,
    ReadOnlyIncomplete,
    ReadOnlyNoSubscription,
    ReadOnlyTesterIneligible,
    ReadOnlyAccountDeleting,
    ReadOnlyOther,
}

impl From<AccountNoticeKind> for AccountNoticeKindDto {
    fn from(value: AccountNoticeKind) -> Self {
        match value {
            AccountNoticeKind::PaymentFailedGrace => Self::PaymentFailedGrace,
            AccountNoticeKind::EndingScheduled => Self::EndingScheduled,
            AccountNoticeKind::ReadOnlyPastDue => Self::ReadOnlyPastDue,
            AccountNoticeKind::ReadOnlyCanceled => Self::ReadOnlyCanceled,
            AccountNoticeKind::ReadOnlyIncomplete => Self::ReadOnlyIncomplete,
            AccountNoticeKind::ReadOnlyNoSubscription => Self::ReadOnlyNoSubscription,
            AccountNoticeKind::ReadOnlyTesterIneligible => Self::ReadOnlyTesterIneligible,
            AccountNoticeKind::ReadOnlyAccountDeleting => Self::ReadOnlyAccountDeleting,
            AccountNoticeKind::ReadOnlyOther => Self::ReadOnlyOther,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum AccountNoticeSeverityDto {
    Warning,
    Error,
}

impl From<NoticeSeverity> for AccountNoticeSeverityDto {
    fn from(value: NoticeSeverity) -> Self {
        match value {
            NoticeSeverity::Warning => Self::Warning,
            NoticeSeverity::Error => Self::Error,
        }
    }
}

/// Display decision for the global account ribbon. `deadline` is Unix seconds; clients
/// format it locally. `manage_url` is present only after trust validation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct AccountNoticeDto {
    pub kind: AccountNoticeKindDto,
    pub severity: AccountNoticeSeverityDto,
    #[ts(type = "number | null")]
    pub deadline: Option<i64>,
    pub manage_url: Option<String>,
    pub dismissible: bool,
    pub snooze_key: String,
}

impl From<AccountNotice> for AccountNoticeDto {
    fn from(value: AccountNotice) -> Self {
        Self {
            kind: value.kind.into(),
            severity: value.severity.into(),
            deadline: value.deadline,
            manage_url: value.manage_url,
            dismissible: value.dismissible,
            snooze_key: value.snooze_key,
        }
    }
}

/// Typed availability for the account notice. A `none` result means the server answered
/// and no notice applies; `unsupported` means the server has no account-access route.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[ts(tag = "kind", rename_all = "snake_case")]
pub enum AccountAccessResult {
    Notice { notice: AccountNoticeDto },
    None,
    Unsupported,
    TemporarilyUnavailable,
}

/// Maps one device-authenticated response into the UI result. The decision lives in
/// `peppy_hosted_client::account_access`; this only converts it to the DTO.
pub fn account_access_result_from_response(
    status: u16,
    body: &str,
    now_unix: i64,
    trusted_origins: &[String],
) -> AccountAccessResult {
    match account_access_outcome(status, body, now_unix, trusted_origins) {
        AccountAccessOutcome::Notice(notice) => AccountAccessResult::Notice {
            notice: notice.into(),
        },
        // A successful "no notice".
        AccountAccessOutcome::None => AccountAccessResult::None,
        AccountAccessOutcome::Unsupported => AccountAccessResult::Unsupported,
        AccountAccessOutcome::TemporarilyUnavailable => AccountAccessResult::TemporarilyUnavailable,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn notice(kind: AccountNoticeKind, severity: NoticeSeverity) -> AccountNotice {
        AccountNotice {
            kind,
            severity,
            deadline: Some(1_800_000_000),
            manage_url: Some("https://peppy.pro/account".into()),
            dismissible: true,
            snooze_key: format!("{}:1800000000", kind.snake_case()),
        }
    }

    #[test]
    fn notice_converts_every_field_into_camel_case_wire_shape() {
        let value = serde_json::to_value(AccountAccessResult::Notice {
            notice: notice(AccountNoticeKind::ReadOnlyPastDue, NoticeSeverity::Error).into(),
        })
        .unwrap();

        assert_eq!(
            value,
            json!({
                "kind": "notice",
                "notice": {
                    "kind": "read_only_past_due",
                    "severity": "error",
                    "deadline": 1_800_000_000,
                    "manageUrl": "https://peppy.pro/account",
                    "dismissible": true,
                    "snoozeKey": "read_only_past_due:1800000000",
                },
            })
        );
    }

    #[test]
    fn every_notice_kind_keeps_the_copy_key_stem() {
        let kinds = [
            AccountNoticeKind::PaymentFailedGrace,
            AccountNoticeKind::EndingScheduled,
            AccountNoticeKind::ReadOnlyPastDue,
            AccountNoticeKind::ReadOnlyCanceled,
            AccountNoticeKind::ReadOnlyIncomplete,
            AccountNoticeKind::ReadOnlyNoSubscription,
            AccountNoticeKind::ReadOnlyTesterIneligible,
            AccountNoticeKind::ReadOnlyAccountDeleting,
            AccountNoticeKind::ReadOnlyOther,
        ];
        for kind in kinds {
            let dto = AccountNoticeKindDto::from(kind);
            assert_eq!(
                serde_json::to_value(dto).unwrap(),
                json!(kind.snake_case()),
                "kind {kind:?}"
            );
        }
    }

    #[test]
    fn warning_notice_without_deadline_or_url_serializes_nulls() {
        let mut value = notice(
            AccountNoticeKind::PaymentFailedGrace,
            NoticeSeverity::Warning,
        );
        value.deadline = None;
        value.manage_url = None;
        let dto = serde_json::to_value(AccountNoticeDto::from(value)).unwrap();

        assert_eq!(dto["severity"], "warning");
        assert!(dto["deadline"].is_null());
        assert!(dto["manageUrl"].is_null());
    }

    #[test]
    fn response_mapping_converts_each_shared_outcome() {
        let origins = ["https://peppy.pro".to_owned()];
        let read_only = r#"{"access":"read_only","state":"read_only","reason":"canceled","deadline":null,"manage_url":"https://peppy.pro/account"}"#;
        let notice = account_access_result_from_response(200, read_only, 1_000, &origins);
        assert!(matches!(
            notice,
            AccountAccessResult::Notice { ref notice } if notice.kind == AccountNoticeKindDto::ReadOnlyCanceled
        ));
        let ok = r#"{"access":"read_write","state":"ok","reason":null,"deadline":null,"manage_url":null}"#;
        assert_eq!(
            account_access_result_from_response(200, ok, 1_000, &origins),
            AccountAccessResult::None
        );
        assert_eq!(
            account_access_result_from_response(404, "", 1_000, &origins),
            AccountAccessResult::Unsupported
        );
        assert_eq!(
            account_access_result_from_response(503, "{}", 1_000, &origins),
            AccountAccessResult::TemporarilyUnavailable
        );
    }

    #[test]
    fn absent_and_unsupported_results_have_only_a_kind_field() {
        assert_eq!(
            serde_json::to_value(AccountAccessResult::None).unwrap(),
            json!({"kind": "none"})
        );
        assert_eq!(
            serde_json::to_value(AccountAccessResult::Unsupported).unwrap(),
            json!({"kind": "unsupported"})
        );
        assert_eq!(
            serde_json::to_value(AccountAccessResult::TemporarilyUnavailable).unwrap(),
            json!({"kind": "temporarily_unavailable"})
        );
    }
}
