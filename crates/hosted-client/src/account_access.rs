//! Shared account-access decisions for every hosted client.
//!
//! Hosts own transport, caching, and presentation. This module owns the wire
//! codecs, status classification, and the single notice decision that UIs reuse.

use crate::attachment_retention::AttachmentRetentionRequest;
use peppy_hosted_api::account_access::{
    AccountAccessLevel, AccountAccessReason, AccountAccessResponse, AccountAccessState, routes,
};
use serde::Deserialize;
use url::{Host, Url};

/// Maximum accepted JSON body, in UTF-8 bytes, for both responses and denials.
pub const MAX_ACCOUNT_ACCESS_RESPONSE_JSON: usize = 4 * 1024;

const MAX_MANAGE_URL_BYTES: usize = 2048;
const OPERATION_NOT_PERMITTED: &str = "operation_not_permitted";
const SECONDS_PER_DAY: i64 = 24 * 60 * 60;
const ENDING_SOON_WINDOW_SECONDS: i64 = 7 * SECONDS_PER_DAY;
const DISMISSIBLE_MIN_REMAINING_SECONDS: i64 = 2 * SECONDS_PER_DAY;

pub type AccountAccessHttpRequest = AttachmentRetentionRequest;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountAccessAvailability {
    Available,
    Unsupported,
    TemporarilyUnavailable,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum AccountAccessCodecError {
    #[error("invalid account access response")]
    InvalidResponse,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountNoticeKind {
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

impl AccountNoticeKind {
    /// Stable snake_case name, used in snooze keys and copy keys (`account_<name>_title`).
    pub const fn snake_case(self) -> &'static str {
        match self {
            Self::PaymentFailedGrace => "payment_failed_grace",
            Self::EndingScheduled => "ending_scheduled",
            Self::ReadOnlyPastDue => "read_only_past_due",
            Self::ReadOnlyCanceled => "read_only_canceled",
            Self::ReadOnlyIncomplete => "read_only_incomplete",
            Self::ReadOnlyNoSubscription => "read_only_no_subscription",
            Self::ReadOnlyTesterIneligible => "read_only_tester_ineligible",
            Self::ReadOnlyAccountDeleting => "read_only_account_deleting",
            Self::ReadOnlyOther => "read_only_other",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoticeSeverity {
    Warning,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountNotice {
    pub kind: AccountNoticeKind,
    pub severity: NoticeSeverity,
    /// Unix seconds. Clients format it locally.
    pub deadline: Option<i64>,
    /// Present only when it passed trust validation; `None` means no action button.
    pub manage_url: Option<String>,
    pub dismissible: bool,
    /// `"<kind_snake>:<deadline|none>"`. Snoozing applies to this exact key.
    pub snooze_key: String,
}

pub fn build_device_account_access_request() -> AccountAccessHttpRequest {
    AttachmentRetentionRequest {
        method: "GET",
        relative_path: routes::DEVICE_ACCOUNT_ACCESS.into(),
        json_body: None,
        protocol_header: None,
        expected_success_status: 200,
    }
}

/// Parses the response body. Unknown fields are ignored; a negative deadline is rejected.
pub fn parse_account_access_response(
    body: &str,
) -> Result<AccountAccessResponse, AccountAccessCodecError> {
    if body.is_empty() || body.len() > MAX_ACCOUNT_ACCESS_RESPONSE_JSON {
        return Err(AccountAccessCodecError::InvalidResponse);
    }
    let response: AccountAccessResponse =
        serde_json::from_str(body).map_err(|_| AccountAccessCodecError::InvalidResponse)?;
    if response.deadline.is_some_and(|deadline| deadline < 0) {
        return Err(AccountAccessCodecError::InvalidResponse);
    }
    Ok(response)
}

/// Only a 200 carries a notice. Older servers answer 404, 405, or 501; any other
/// status is treated as a transient failure that must not clear the cached notice.
pub fn classify_account_access_status(status: u16) -> AccountAccessAvailability {
    match status {
        200 => AccountAccessAvailability::Available,
        404 | 405 | 501 => AccountAccessAvailability::Unsupported,
        _ => AccountAccessAvailability::TemporarilyUnavailable,
    }
}

/// A coded policy denial pauses writes; it is not an authentication failure.
pub fn is_policy_denial(status: u16, body: &str) -> bool {
    #[derive(Deserialize)]
    struct ErrorBody {
        code: Option<String>,
    }

    status == 403
        && !body.is_empty()
        && body.len() <= MAX_ACCOUNT_ACCESS_RESPONSE_JSON
        && serde_json::from_str::<ErrorBody>(body)
            .ok()
            .and_then(|error| error.code)
            .as_deref()
            == Some(OPERATION_NOT_PERMITTED)
}

/// The one place that decides what, if anything, the account notice shows.
///
/// `trusted_origins` are the device's API origin and the hosted account origin.
pub fn account_notice(
    response: &AccountAccessResponse,
    now_unix: i64,
    trusted_origins: &[String],
) -> Option<AccountNotice> {
    let (kind, severity, deadline) = notice_shape(response, now_unix)?;
    let dismissible = severity == NoticeSeverity::Warning
        && deadline.is_none_or(|deadline| {
            deadline.saturating_sub(now_unix) > DISMISSIBLE_MIN_REMAINING_SECONDS
        });
    Some(AccountNotice {
        kind,
        severity,
        deadline,
        manage_url: validated_manage_url(response.manage_url.as_deref(), trusted_origins),
        dismissible,
        snooze_key: snooze_key(kind, deadline),
    })
}

/// What one device-authenticated response means for the host UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccountAccessOutcome {
    Notice(AccountNotice),
    None,
    Unsupported,
    TemporarilyUnavailable,
}

/// Interprets one account-access response. A 200 whose body does not parse is
/// transient, so the host keeps its last good notice.
pub fn account_access_outcome(
    status: u16,
    body: &str,
    now_unix: i64,
    trusted_origins: &[String],
) -> AccountAccessOutcome {
    match classify_account_access_status(status) {
        AccountAccessAvailability::Unsupported => AccountAccessOutcome::Unsupported,
        AccountAccessAvailability::TemporarilyUnavailable => {
            AccountAccessOutcome::TemporarilyUnavailable
        }
        AccountAccessAvailability::Available => match parse_account_access_response(body) {
            Ok(response) => match account_notice(&response, now_unix, trusted_origins) {
                Some(notice) => AccountAccessOutcome::Notice(notice),
                None => AccountAccessOutcome::None,
            },
            Err(_) => AccountAccessOutcome::TemporarilyUnavailable,
        },
    }
}

/// `access` is authoritative: a read-only account always gets an error notice,
/// even when `state` says otherwise.
fn notice_shape(
    response: &AccountAccessResponse,
    now_unix: i64,
) -> Option<(AccountNoticeKind, NoticeSeverity, Option<i64>)> {
    match response.access {
        AccountAccessLevel::Unknown => None,
        AccountAccessLevel::ReadOnly => Some((
            read_only_kind(response.reason),
            NoticeSeverity::Error,
            response.deadline,
        )),
        AccountAccessLevel::ReadWrite => match response.state {
            AccountAccessState::PaymentFailedGrace => Some((
                AccountNoticeKind::PaymentFailedGrace,
                NoticeSeverity::Warning,
                response.deadline.filter(|&deadline| deadline > now_unix),
            )),
            AccountAccessState::EndingScheduled => {
                let deadline = response.deadline.filter(|&deadline| {
                    now_unix < deadline
                        && deadline <= now_unix.saturating_add(ENDING_SOON_WINDOW_SECONDS)
                })?;
                Some((
                    AccountNoticeKind::EndingScheduled,
                    NoticeSeverity::Warning,
                    Some(deadline),
                ))
            }
            AccountAccessState::Ok | AccountAccessState::ReadOnly | AccountAccessState::Unknown => {
                None
            }
        },
    }
}

fn read_only_kind(reason: Option<AccountAccessReason>) -> AccountNoticeKind {
    match reason {
        Some(AccountAccessReason::PastDue) => AccountNoticeKind::ReadOnlyPastDue,
        Some(AccountAccessReason::Canceled) => AccountNoticeKind::ReadOnlyCanceled,
        Some(AccountAccessReason::Incomplete) => AccountNoticeKind::ReadOnlyIncomplete,
        Some(AccountAccessReason::NoSubscription) => AccountNoticeKind::ReadOnlyNoSubscription,
        Some(AccountAccessReason::TesterIneligible) => AccountNoticeKind::ReadOnlyTesterIneligible,
        Some(AccountAccessReason::AccountDeleting) => AccountNoticeKind::ReadOnlyAccountDeleting,
        Some(AccountAccessReason::Unknown) | None => AccountNoticeKind::ReadOnlyOther,
    }
}

fn snooze_key(kind: AccountNoticeKind, deadline: Option<i64>) -> String {
    let kind = kind.snake_case();
    match deadline {
        Some(deadline) => format!("{kind}:{deadline}"),
        None => format!("{kind}:none"),
    }
}

/// Keeps the link only when it is a credential-free URL on a DNS host under a trusted origin.
fn validated_manage_url(raw: Option<&str>, trusted_origins: &[String]) -> Option<String> {
    let url = Url::parse(raw?)
        .ok()
        .filter(|url| url.as_str().len() <= MAX_MANAGE_URL_BYTES)?;
    let has_credentials = !url.username().is_empty() || url.password().is_some();
    let has_dns_host = matches!(url.host(), Some(Host::Domain(_)));
    let trusted = trusted_origins
        .iter()
        .any(|origin| origin_is_trusted(&url, origin));
    (!has_credentials && has_dns_host && trusted).then(|| url.to_string())
}

/// `https` must match a canonical trusted origin. Plain `http` is accepted only for
/// `localhost` against a trusted `localhost` origin on the same scheme, host and port.
fn origin_is_trusted(url: &Url, trusted: &str) -> bool {
    let origin = url.origin().ascii_serialization();
    match url.scheme() {
        "https" => crate::canonical_origin(trusted).is_ok_and(|canonical| canonical == origin),
        "http" => {
            is_localhost(url)
                && Url::parse(trusted).is_ok_and(|trusted| {
                    trusted.scheme() == "http"
                        && is_localhost(&trusted)
                        && trusted.origin().ascii_serialization() == origin
                })
        }
        _ => false,
    }
}

fn is_localhost(url: &Url) -> bool {
    url.host_str() == Some("localhost")
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_700_000_000;
    const API_ORIGIN: &str = "https://api.peppy.test";
    const ACCOUNT_ORIGIN: &str = "https://peppy.pro";
    const ACCOUNT_URL: &str = "https://peppy.pro/account";

    fn trusted() -> Vec<String> {
        vec![API_ORIGIN.into(), ACCOUNT_ORIGIN.into()]
    }

    fn response(
        access: AccountAccessLevel,
        state: AccountAccessState,
        reason: Option<AccountAccessReason>,
        deadline: Option<i64>,
    ) -> AccountAccessResponse {
        AccountAccessResponse {
            access,
            state,
            reason,
            deadline,
            manage_url: Some(ACCOUNT_URL.into()),
        }
    }

    fn read_write(state: AccountAccessState, deadline: Option<i64>) -> AccountAccessResponse {
        response(AccountAccessLevel::ReadWrite, state, None, deadline)
    }

    fn read_only(reason: Option<AccountAccessReason>) -> AccountAccessResponse {
        response(
            AccountAccessLevel::ReadOnly,
            AccountAccessState::ReadOnly,
            reason,
            None,
        )
    }

    fn notice(response: &AccountAccessResponse) -> AccountNotice {
        account_notice(response, NOW, &trusted()).expect("expected a notice")
    }

    // Request, codec, and status ------------------------------------------------

    #[test]
    fn request_is_a_bodyless_get_expecting_ok() {
        let request = build_device_account_access_request();
        assert_eq!(request.method, "GET");
        assert_eq!(request.relative_path, "/v1/account/access");
        assert_eq!(request.json_body, None);
        assert_eq!(request.protocol_header, None);
        assert_eq!(request.expected_success_status, 200);
    }

    #[test]
    fn parses_a_full_response() {
        let parsed = parse_account_access_response(
            r#"{"access":"read_only","state":"read_only","reason":"past_due","deadline":1700000000,"manage_url":"https://peppy.pro/account"}"#,
        )
        .unwrap();
        assert_eq!(
            parsed,
            response(
                AccountAccessLevel::ReadOnly,
                AccountAccessState::ReadOnly,
                Some(AccountAccessReason::PastDue),
                Some(NOW),
            )
        );
    }

    #[test]
    fn parser_tolerates_unknown_fields_and_absent_optionals() {
        let parsed = parse_account_access_response(
            r#"{"access":"read_write","state":"ok","added_later":[1,2]}"#,
        )
        .unwrap();
        assert_eq!(parsed.access, AccountAccessLevel::ReadWrite);
        assert_eq!(parsed.reason, None);
        assert_eq!(parsed.deadline, None);
        assert_eq!(parsed.manage_url, None);
    }

    #[test]
    fn parser_maps_unrecognised_values_to_unknown() {
        let parsed = parse_account_access_response(
            r#"{"access":"future","state":"future","reason":"future","deadline":null,"manage_url":null}"#,
        )
        .unwrap();
        assert_eq!(parsed.access, AccountAccessLevel::Unknown);
        assert_eq!(parsed.state, AccountAccessState::Unknown);
        assert_eq!(parsed.reason, Some(AccountAccessReason::Unknown));
    }

    #[test]
    fn parser_rejects_negative_deadlines() {
        assert_eq!(
            parse_account_access_response(
                r#"{"access":"read_write","state":"payment_failed_grace","deadline":-1}"#
            ),
            Err(AccountAccessCodecError::InvalidResponse)
        );
    }

    #[test]
    fn parser_rejects_empty_malformed_and_oversized_bodies() {
        assert_eq!(
            parse_account_access_response(""),
            Err(AccountAccessCodecError::InvalidResponse)
        );
        assert_eq!(
            parse_account_access_response("not json"),
            Err(AccountAccessCodecError::InvalidResponse)
        );
        assert_eq!(
            parse_account_access_response(r#"{"access":"read_write"}"#),
            Err(AccountAccessCodecError::InvalidResponse),
            "state is required"
        );

        let padding = "x".repeat(MAX_ACCOUNT_ACCESS_RESPONSE_JSON);
        let oversized = format!(r#"{{"access":"read_write","state":"ok","pad":"{padding}"}}"#);
        assert!(oversized.len() > MAX_ACCOUNT_ACCESS_RESPONSE_JSON);
        assert_eq!(
            parse_account_access_response(&oversized),
            Err(AccountAccessCodecError::InvalidResponse)
        );
    }

    #[test]
    fn parser_accepts_a_body_exactly_at_the_limit() {
        let head = r#"{"access":"read_write","state":"ok","pad":""}"#;
        let padding = "x".repeat(MAX_ACCOUNT_ACCESS_RESPONSE_JSON - head.len());
        let body = format!(r#"{{"access":"read_write","state":"ok","pad":"{padding}"}}"#);
        assert_eq!(body.len(), MAX_ACCOUNT_ACCESS_RESPONSE_JSON);
        assert!(parse_account_access_response(&body).is_ok());
    }

    #[test]
    fn status_200_is_available_and_legacy_codes_are_unsupported() {
        assert_eq!(
            classify_account_access_status(200),
            AccountAccessAvailability::Available
        );
        for status in [404, 405, 501] {
            assert_eq!(
                classify_account_access_status(status),
                AccountAccessAvailability::Unsupported,
                "{status}"
            );
        }
    }

    #[test]
    fn every_other_status_is_temporarily_unavailable() {
        for status in [0, 201, 204, 400, 401, 403, 409, 429, 500, 502, 503, 504] {
            assert_eq!(
                classify_account_access_status(status),
                AccountAccessAvailability::TemporarilyUnavailable,
                "{status}"
            );
        }
    }

    #[test]
    fn policy_denial_requires_a_403_with_the_coded_body() {
        assert!(is_policy_denial(
            403,
            r#"{"code":"operation_not_permitted"}"#
        ));
        assert!(is_policy_denial(
            403,
            r#"{"code":"operation_not_permitted","message":"read only"}"#
        ));
    }

    #[test]
    fn policy_denial_rejects_other_statuses_codes_and_bodies() {
        assert!(!is_policy_denial(
            401,
            r#"{"code":"operation_not_permitted"}"#
        ));
        assert!(!is_policy_denial(
            500,
            r#"{"code":"operation_not_permitted"}"#
        ));
        assert!(!is_policy_denial(403, r#"{"code":"missing_bearer"}"#));
        assert!(!is_policy_denial(403, r#"{"code":42}"#));
        assert!(!is_policy_denial(403, r#"{"message":"forbidden"}"#));
        assert!(!is_policy_denial(403, "operation_not_permitted"));
        assert!(!is_policy_denial(403, ""));
        let padding = "x".repeat(MAX_ACCOUNT_ACCESS_RESPONSE_JSON);
        assert!(!is_policy_denial(
            403,
            &format!(r#"{{"code":"operation_not_permitted","pad":"{padding}"}}"#)
        ));
    }

    // Notice decision -----------------------------------------------------------

    #[test]
    fn read_only_access_is_an_error_notice_and_never_dismissible() {
        let cases = [
            (
                Some(AccountAccessReason::PastDue),
                AccountNoticeKind::ReadOnlyPastDue,
                "read_only_past_due",
            ),
            (
                Some(AccountAccessReason::Canceled),
                AccountNoticeKind::ReadOnlyCanceled,
                "read_only_canceled",
            ),
            (
                Some(AccountAccessReason::Incomplete),
                AccountNoticeKind::ReadOnlyIncomplete,
                "read_only_incomplete",
            ),
            (
                Some(AccountAccessReason::NoSubscription),
                AccountNoticeKind::ReadOnlyNoSubscription,
                "read_only_no_subscription",
            ),
            (
                Some(AccountAccessReason::TesterIneligible),
                AccountNoticeKind::ReadOnlyTesterIneligible,
                "read_only_tester_ineligible",
            ),
            (
                Some(AccountAccessReason::AccountDeleting),
                AccountNoticeKind::ReadOnlyAccountDeleting,
                "read_only_account_deleting",
            ),
            (
                Some(AccountAccessReason::Unknown),
                AccountNoticeKind::ReadOnlyOther,
                "read_only_other",
            ),
            (None, AccountNoticeKind::ReadOnlyOther, "read_only_other"),
        ];
        for (reason, kind, snake) in cases {
            let notice = notice(&read_only(reason));
            assert_eq!(notice.kind, kind, "{reason:?}");
            assert_eq!(notice.severity, NoticeSeverity::Error);
            assert!(!notice.dismissible);
            assert_eq!(notice.snooze_key, format!("{snake}:none"));
        }
    }

    #[test]
    fn read_only_access_wins_over_a_contradictory_state() {
        let contradictory = response(
            AccountAccessLevel::ReadOnly,
            AccountAccessState::Ok,
            Some(AccountAccessReason::PastDue),
            None,
        );
        let notice = notice(&contradictory);
        assert_eq!(notice.kind, AccountNoticeKind::ReadOnlyPastDue);
        assert_eq!(notice.severity, NoticeSeverity::Error);
    }

    #[test]
    fn unknown_access_never_produces_a_notice() {
        for state in [
            AccountAccessState::PaymentFailedGrace,
            AccountAccessState::EndingScheduled,
            AccountAccessState::ReadOnly,
            AccountAccessState::Ok,
            AccountAccessState::Unknown,
        ] {
            let unknown = response(AccountAccessLevel::Unknown, state, None, Some(NOW + 60));
            assert_eq!(account_notice(&unknown, NOW, &trusted()), None, "{state:?}");
        }
    }

    #[test]
    fn read_write_ok_ready_or_unknown_states_have_no_notice() {
        for state in [
            AccountAccessState::Ok,
            AccountAccessState::Unknown,
            AccountAccessState::ReadOnly,
        ] {
            assert_eq!(
                account_notice(&read_write(state, Some(NOW + 60)), NOW, &trusted()),
                None,
                "{state:?}"
            );
        }
    }

    #[test]
    fn payment_grace_is_a_warning_carrying_its_future_deadline() {
        let deadline = NOW + 5 * SECONDS_PER_DAY;
        let notice = notice(&read_write(
            AccountAccessState::PaymentFailedGrace,
            Some(deadline),
        ));
        assert_eq!(notice.kind, AccountNoticeKind::PaymentFailedGrace);
        assert_eq!(notice.severity, NoticeSeverity::Warning);
        assert_eq!(notice.deadline, Some(deadline));
        assert!(notice.dismissible, "more than 48h remains");
        assert_eq!(
            notice.snooze_key,
            format!("payment_failed_grace:{deadline}")
        );
    }

    #[test]
    fn payment_grace_is_not_dismissible_inside_forty_eight_hours() {
        let at_boundary = notice(&read_write(
            AccountAccessState::PaymentFailedGrace,
            Some(NOW + DISMISSIBLE_MIN_REMAINING_SECONDS),
        ));
        assert!(!at_boundary.dismissible, "exactly 48h is not more than 48h");

        let just_over = notice(&read_write(
            AccountAccessState::PaymentFailedGrace,
            Some(NOW + DISMISSIBLE_MIN_REMAINING_SECONDS + 1),
        ));
        assert!(just_over.dismissible);

        let inside = notice(&read_write(
            AccountAccessState::PaymentFailedGrace,
            Some(NOW + 3_600),
        ));
        assert!(!inside.dismissible);
    }

    #[test]
    fn payment_grace_without_a_deadline_keeps_its_warning_and_is_dismissible() {
        let notice = notice(&read_write(AccountAccessState::PaymentFailedGrace, None));
        assert_eq!(notice.severity, NoticeSeverity::Warning);
        assert_eq!(notice.deadline, None);
        assert!(notice.dismissible);
        assert_eq!(notice.snooze_key, "payment_failed_grace:none");
    }

    #[test]
    fn payment_grace_with_a_past_deadline_drops_the_deadline() {
        let notice = notice(&read_write(
            AccountAccessState::PaymentFailedGrace,
            Some(NOW - 1),
        ));
        assert_eq!(notice.deadline, None);
        assert!(notice.dismissible);
        assert_eq!(notice.snooze_key, "payment_failed_grace:none");
    }

    #[test]
    fn payment_grace_deadline_equal_to_now_is_treated_as_past() {
        let notice = notice(&read_write(
            AccountAccessState::PaymentFailedGrace,
            Some(NOW),
        ));
        assert_eq!(notice.deadline, None);
    }

    #[test]
    fn ending_scheduled_shows_inside_the_seven_day_window() {
        let deadline = NOW + ENDING_SOON_WINDOW_SECONDS;
        let notice = notice(&read_write(
            AccountAccessState::EndingScheduled,
            Some(deadline),
        ));
        assert_eq!(notice.kind, AccountNoticeKind::EndingScheduled);
        assert_eq!(notice.severity, NoticeSeverity::Warning);
        assert_eq!(
            notice.deadline,
            Some(deadline),
            "seven days exactly is inside"
        );
        assert_eq!(notice.snooze_key, format!("ending_scheduled:{deadline}"));
    }

    #[test]
    fn ending_scheduled_is_hidden_beyond_seven_days_or_without_a_future_deadline() {
        for deadline in [
            Some(NOW + ENDING_SOON_WINDOW_SECONDS + 1),
            Some(NOW),
            Some(NOW - 1),
            None,
        ] {
            assert_eq!(
                account_notice(
                    &read_write(AccountAccessState::EndingScheduled, deadline),
                    NOW,
                    &trusted()
                ),
                None,
                "{deadline:?}"
            );
        }
    }

    #[test]
    fn ending_scheduled_dismissibility_follows_the_forty_eight_hour_rule() {
        let inside_two_days = notice(&read_write(
            AccountAccessState::EndingScheduled,
            Some(NOW + DISMISSIBLE_MIN_REMAINING_SECONDS),
        ));
        assert!(!inside_two_days.dismissible);

        let beyond_two_days = notice(&read_write(
            AccountAccessState::EndingScheduled,
            Some(NOW + DISMISSIBLE_MIN_REMAINING_SECONDS + 1),
        ));
        assert!(beyond_two_days.dismissible);
    }

    #[test]
    fn snooze_key_uses_none_for_read_only_without_deadline() {
        let notice = notice(&read_only(Some(AccountAccessReason::Canceled)));
        assert_eq!(notice.snooze_key, "read_only_canceled:none");
    }

    #[test]
    fn every_kind_has_a_unique_snake_case_snooze_stem() {
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
        let mut stems: Vec<&str> = kinds.iter().map(|kind| kind.snake_case()).collect();
        assert!(
            stems
                .iter()
                .all(|stem| stem.chars().all(|c| c.is_ascii_lowercase() || c == '_'))
        );
        stems.sort_unstable();
        stems.dedup();
        assert_eq!(stems.len(), kinds.len());
    }

    // manage_url trust ----------------------------------------------------------

    fn notice_with_url(manage_url: Option<&str>) -> Option<String> {
        let mut response = read_only(Some(AccountAccessReason::PastDue));
        response.manage_url = manage_url.map(str::to_owned);
        notice(&response).manage_url
    }

    #[test]
    fn keeps_https_urls_on_a_trusted_origin() {
        assert_eq!(
            notice_with_url(Some("https://peppy.pro/account")),
            Some("https://peppy.pro/account".into())
        );
        assert_eq!(
            notice_with_url(Some("https://api.peppy.test/billing?x=1")),
            Some("https://api.peppy.test/billing?x=1".into())
        );
    }

    #[test]
    fn drops_urls_whose_origin_is_not_trusted() {
        assert_eq!(notice_with_url(Some("https://evil.example/account")), None);
        assert_eq!(
            notice_with_url(Some("https://peppy.pro.evil.example/account")),
            None
        );
        assert_eq!(
            notice_with_url(Some("https://peppy.pro:8443/account")),
            None
        );
    }

    #[test]
    fn drops_plain_http_for_a_non_localhost_host() {
        assert_eq!(notice_with_url(Some("http://peppy.pro/account")), None);
    }

    #[test]
    fn drops_urls_with_credentials() {
        assert_eq!(
            notice_with_url(Some("https://user:secret@peppy.pro/account")),
            None
        );
        assert_eq!(
            notice_with_url(Some("https://user@peppy.pro/account")),
            None
        );
    }

    #[test]
    fn drops_ip_literal_hosts_even_when_trusted_spelling_matches() {
        let trusted_ip = vec!["https://203.0.113.7".to_owned()];
        let mut response = read_only(Some(AccountAccessReason::PastDue));
        response.manage_url = Some("https://203.0.113.7/account".into());
        assert_eq!(
            account_notice(&response, NOW, &trusted_ip)
                .unwrap()
                .manage_url,
            None
        );
    }

    #[test]
    fn drops_unparseable_and_oversized_urls() {
        assert_eq!(notice_with_url(Some("not a url")), None);
        assert_eq!(notice_with_url(Some("")), None);
        let long_path = "a".repeat(MAX_MANAGE_URL_BYTES);
        assert_eq!(
            notice_with_url(Some(&format!("https://peppy.pro/{long_path}"))),
            None
        );
    }

    #[test]
    fn absent_url_leaves_the_notice_without_an_action() {
        assert_eq!(notice_with_url(None), None);
        assert!(notice_with_url(None).is_none());
    }

    #[test]
    fn a_rejected_url_does_not_suppress_the_notice() {
        let mut response = read_only(Some(AccountAccessReason::PastDue));
        response.manage_url = Some("https://evil.example/account".into());
        let notice = notice(&response);
        assert_eq!(notice.kind, AccountNoticeKind::ReadOnlyPastDue);
        assert_eq!(notice.manage_url, None);
    }

    #[test]
    fn trusted_origins_accept_a_trailing_slash_in_their_canonical_form() {
        let mut response = read_only(Some(AccountAccessReason::PastDue));
        response.manage_url = Some("https://peppy.pro/account".into());
        let with_slash = vec!["https://peppy.pro/".to_owned()];
        assert_eq!(
            account_notice(&response, NOW, &with_slash)
                .unwrap()
                .manage_url,
            Some("https://peppy.pro/account".into())
        );
    }

    #[test]
    fn non_canonical_trusted_origins_are_ignored() {
        let mut response = read_only(Some(AccountAccessReason::PastDue));
        response.manage_url = Some("https://peppy.pro/account".into());
        for origin in ["http://peppy.pro", "https://peppy.pro/account", "peppy.pro"] {
            let trusted = vec![origin.to_owned()];
            assert_eq!(
                account_notice(&response, NOW, &trusted).unwrap().manage_url,
                None,
                "{origin}"
            );
        }
    }

    #[test]
    fn loopback_http_is_trusted_only_against_a_loopback_origin() {
        let mut response = read_only(Some(AccountAccessReason::PastDue));
        response.manage_url = Some("http://localhost:8080/account".into());

        let loopback = vec!["http://localhost:8080".to_owned()];
        assert_eq!(
            account_notice(&response, NOW, &loopback)
                .unwrap()
                .manage_url,
            Some("http://localhost:8080/account".into())
        );

        let other_port = vec!["http://localhost:9090".to_owned()];
        assert_eq!(
            account_notice(&response, NOW, &other_port)
                .unwrap()
                .manage_url,
            None
        );

        let secure_origin = vec!["https://localhost:8080".to_owned()];
        assert_eq!(
            account_notice(&response, NOW, &secure_origin)
                .unwrap()
                .manage_url,
            None
        );
    }

    #[test]
    fn loopback_http_ip_literal_is_never_trusted() {
        let mut response = read_only(Some(AccountAccessReason::PastDue));
        response.manage_url = Some("http://127.0.0.1:8080/account".into());
        let loopback = vec!["http://127.0.0.1:8080".to_owned()];
        assert_eq!(
            account_notice(&response, NOW, &loopback)
                .unwrap()
                .manage_url,
            None
        );
    }

    // Outcome -------------------------------------------------------------------

    #[test]
    fn outcome_200_with_a_notice_body_is_a_notice() {
        let outcome = account_access_outcome(
            200,
            r#"{"access":"read_only","state":"read_only","reason":"canceled","deadline":null,"manage_url":"https://peppy.pro/account"}"#,
            NOW,
            &trusted(),
        );
        assert_eq!(
            outcome,
            AccountAccessOutcome::Notice(notice(&read_only(Some(AccountAccessReason::Canceled))))
        );
    }

    #[test]
    fn outcome_200_without_a_notice_is_none() {
        let body = r#"{"access":"read_write","state":"ok","reason":null,"deadline":null,"manage_url":null}"#;
        assert_eq!(
            account_access_outcome(200, body, NOW, &trusted()),
            AccountAccessOutcome::None
        );
    }

    #[test]
    fn outcome_legacy_statuses_are_unsupported() {
        for status in [404, 405, 501] {
            assert_eq!(
                account_access_outcome(status, "", NOW, &trusted()),
                AccountAccessOutcome::Unsupported,
                "{status}"
            );
        }
    }

    #[test]
    fn outcome_other_statuses_are_temporarily_unavailable() {
        for status in [0, 401, 403, 500, 503] {
            assert_eq!(
                account_access_outcome(status, "{}", NOW, &trusted()),
                AccountAccessOutcome::TemporarilyUnavailable,
                "{status}"
            );
        }
    }

    #[test]
    fn outcome_unparseable_success_body_is_temporarily_unavailable() {
        for body in ["", "not json", r#"{"access":"read_only"}"#] {
            assert_eq!(
                account_access_outcome(200, body, NOW, &trusted()),
                AccountAccessOutcome::TemporarilyUnavailable,
                "body {body:?}"
            );
        }
    }

    #[test]
    fn decision_matches_the_spec_example_flow() {
        let response = parse_account_access_response(
            r#"{"access":"read_only","state":"read_only","reason":"past_due","deadline":null,"manage_url":"https://peppy.pro/account"}"#,
        )
        .unwrap();
        let notice = account_notice(&response, NOW, &trusted()).unwrap();
        assert_eq!(
            notice,
            AccountNotice {
                kind: AccountNoticeKind::ReadOnlyPastDue,
                severity: NoticeSeverity::Error,
                deadline: None,
                manage_url: Some(ACCOUNT_URL.into()),
                dismissible: false,
                snooze_key: "read_only_past_due:none".into(),
            }
        );
    }
}
