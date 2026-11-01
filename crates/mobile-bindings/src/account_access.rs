//! Native-safe wrappers for the account-access notice codec.
//!
//! Hosts own the device-authenticated GET, its scheduling, the cache, and the
//! presentation. These functions only build the relative request and turn a
//! raw response into the shared notice decision.

use crate::attachment_retention::{NativeAttachmentRetentionRequest, request_view};
use peppy_hosted_client::account_access::{
    self as codec, AccountAccessOutcome, AccountNotice, AccountNoticeKind, NoticeSeverity,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum NativeAccountNoticeKind {
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum NativeNoticeSeverity {
    Warning,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct NativeAccountNotice {
    pub kind: NativeAccountNoticeKind,
    pub severity: NativeNoticeSeverity,
    /// Unix seconds supplied by the server.
    pub deadline: Option<i64>,
    /// Present only when the URL passed the shared trust check; `None` means no action.
    pub manage_url: Option<String>,
    pub dismissible: bool,
    pub snooze_key: String,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Enum)]
pub enum NativeAccountAccessResult {
    Notice { notice: NativeAccountNotice },
    None,
    Unsupported,
    TemporarilyUnavailable,
}

#[uniffi::export]
pub fn account_access_request() -> NativeAttachmentRetentionRequest {
    request_view(codec::build_device_account_access_request())
}

/// Interprets one account-access response. A 200 whose body does not parse is
/// transient, so the host keeps its last good notice.
#[uniffi::export]
pub fn account_access_from_response(
    status: u16,
    body: String,
    now_unix: i64,
    trusted_origins: Vec<String>,
) -> NativeAccountAccessResult {
    match codec::account_access_outcome(status, &body, now_unix, &trusted_origins) {
        AccountAccessOutcome::Notice(notice) => NativeAccountAccessResult::Notice {
            notice: notice_view(notice),
        },
        AccountAccessOutcome::None => NativeAccountAccessResult::None,
        AccountAccessOutcome::Unsupported => NativeAccountAccessResult::Unsupported,
        AccountAccessOutcome::TemporarilyUnavailable => {
            NativeAccountAccessResult::TemporarilyUnavailable
        }
    }
}

#[uniffi::export]
pub fn is_account_policy_denial(status: u16, body: String) -> bool {
    codec::is_policy_denial(status, &body)
}

fn notice_view(notice: AccountNotice) -> NativeAccountNotice {
    NativeAccountNotice {
        kind: native_kind(notice.kind),
        severity: match notice.severity {
            NoticeSeverity::Warning => NativeNoticeSeverity::Warning,
            NoticeSeverity::Error => NativeNoticeSeverity::Error,
        },
        deadline: notice.deadline,
        manage_url: notice.manage_url,
        dismissible: notice.dismissible,
        snooze_key: notice.snooze_key,
    }
}

fn native_kind(kind: AccountNoticeKind) -> NativeAccountNoticeKind {
    match kind {
        AccountNoticeKind::PaymentFailedGrace => NativeAccountNoticeKind::PaymentFailedGrace,
        AccountNoticeKind::EndingScheduled => NativeAccountNoticeKind::EndingScheduled,
        AccountNoticeKind::ReadOnlyPastDue => NativeAccountNoticeKind::ReadOnlyPastDue,
        AccountNoticeKind::ReadOnlyCanceled => NativeAccountNoticeKind::ReadOnlyCanceled,
        AccountNoticeKind::ReadOnlyIncomplete => NativeAccountNoticeKind::ReadOnlyIncomplete,
        AccountNoticeKind::ReadOnlyNoSubscription => {
            NativeAccountNoticeKind::ReadOnlyNoSubscription
        }
        AccountNoticeKind::ReadOnlyTesterIneligible => {
            NativeAccountNoticeKind::ReadOnlyTesterIneligible
        }
        AccountNoticeKind::ReadOnlyAccountDeleting => {
            NativeAccountNoticeKind::ReadOnlyAccountDeleting
        }
        AccountNoticeKind::ReadOnlyOther => NativeAccountNoticeKind::ReadOnlyOther,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW_UNIX: i64 = 1_800_000_000;
    const TRUSTED_ORIGIN: &str = "https://peppy.pro";
    const READ_ONLY_PAST_DUE: &str = r#"{"access":"read_only","state":"read_only","reason":"past_due","deadline":null,"manage_url":"https://peppy.pro/account"}"#;
    const READ_WRITE_OK: &str =
        r#"{"access":"read_write","state":"ok","reason":null,"deadline":null,"manage_url":null}"#;

    fn trusted() -> Vec<String> {
        vec![TRUSTED_ORIGIN.into()]
    }

    #[test]
    fn request_is_a_bodyless_get_of_the_device_route() {
        let request = account_access_request();
        assert_eq!(request.method, "GET");
        assert_eq!(request.relative_path, "/v1/account/access");
        assert_eq!(request.json_body, None);
        assert_eq!(request.protocol_header, None);
        assert_eq!(request.expected_success_status, 200);
    }

    #[test]
    fn read_only_response_yields_an_error_notice_with_a_trusted_action() {
        let result =
            account_access_from_response(200, READ_ONLY_PAST_DUE.into(), NOW_UNIX, trusted());
        assert_eq!(
            result,
            NativeAccountAccessResult::Notice {
                notice: NativeAccountNotice {
                    kind: NativeAccountNoticeKind::ReadOnlyPastDue,
                    severity: NativeNoticeSeverity::Error,
                    deadline: None,
                    manage_url: Some("https://peppy.pro/account".into()),
                    dismissible: false,
                    snooze_key: "read_only_past_due:none".into(),
                },
            }
        );
    }

    #[test]
    fn every_native_kind_maps_from_the_shared_kind() {
        let kinds = [
            (
                AccountNoticeKind::PaymentFailedGrace,
                NativeAccountNoticeKind::PaymentFailedGrace,
            ),
            (
                AccountNoticeKind::EndingScheduled,
                NativeAccountNoticeKind::EndingScheduled,
            ),
            (
                AccountNoticeKind::ReadOnlyPastDue,
                NativeAccountNoticeKind::ReadOnlyPastDue,
            ),
            (
                AccountNoticeKind::ReadOnlyCanceled,
                NativeAccountNoticeKind::ReadOnlyCanceled,
            ),
            (
                AccountNoticeKind::ReadOnlyIncomplete,
                NativeAccountNoticeKind::ReadOnlyIncomplete,
            ),
            (
                AccountNoticeKind::ReadOnlyNoSubscription,
                NativeAccountNoticeKind::ReadOnlyNoSubscription,
            ),
            (
                AccountNoticeKind::ReadOnlyTesterIneligible,
                NativeAccountNoticeKind::ReadOnlyTesterIneligible,
            ),
            (
                AccountNoticeKind::ReadOnlyAccountDeleting,
                NativeAccountNoticeKind::ReadOnlyAccountDeleting,
            ),
            (
                AccountNoticeKind::ReadOnlyOther,
                NativeAccountNoticeKind::ReadOnlyOther,
            ),
        ];
        for (shared, native) in kinds {
            assert_eq!(native_kind(shared), native, "{shared:?}");
        }
    }

    #[test]
    fn warning_severity_and_deadline_are_mapped() {
        let deadline = NOW_UNIX + 10 * 24 * 60 * 60;
        let body = format!(
            r#"{{"access":"read_write","state":"payment_failed_grace","reason":"past_due","deadline":{deadline},"manage_url":null}}"#
        );
        let NativeAccountAccessResult::Notice { notice } =
            account_access_from_response(200, body, NOW_UNIX, trusted())
        else {
            panic!("expected a notice");
        };
        assert_eq!(notice.severity, NativeNoticeSeverity::Warning);
        assert_eq!(notice.deadline, Some(deadline));
    }

    #[test]
    fn mapping_passes_none_legacy_and_transient_outcomes_through() {
        assert_eq!(
            account_access_from_response(200, READ_WRITE_OK.into(), NOW_UNIX, trusted()),
            NativeAccountAccessResult::None
        );
        assert_eq!(
            account_access_from_response(404, String::new(), NOW_UNIX, trusted()),
            NativeAccountAccessResult::Unsupported
        );
        assert_eq!(
            account_access_from_response(503, String::new(), NOW_UNIX, trusted()),
            NativeAccountAccessResult::TemporarilyUnavailable
        );
        assert_eq!(
            account_access_from_response(200, "not json".into(), NOW_UNIX, trusted()),
            NativeAccountAccessResult::TemporarilyUnavailable
        );
    }

    #[test]
    fn coded_operation_not_permitted_is_a_policy_denial() {
        assert!(is_account_policy_denial(
            403,
            r#"{"code":"operation_not_permitted"}"#.into()
        ));
    }

    #[test]
    fn other_denials_are_not_policy_denials() {
        assert!(!is_account_policy_denial(403, String::new()));
        assert!(!is_account_policy_denial(
            403,
            r#"{"code":"attachment_retention_forbidden"}"#.into()
        ));
        assert!(!is_account_policy_denial(
            401,
            r#"{"code":"operation_not_permitted"}"#.into()
        ));
    }
}
