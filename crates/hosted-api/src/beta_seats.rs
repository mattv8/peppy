//! Public beta-seat wire contracts.

use crate::{tester_admin::valid_revision, tester_admin_v2::VaultBindingV2};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Deserializer, Serialize, de::Error};
use std::collections::HashSet;
use std::str::FromStr;
use uuid::Uuid;

pub const FEEDBACK_URL: &str =
    "https://github.com/mattv8/peppy/issues/new?template=beta-feedback.yml";
pub const PLAY_OPT_IN_URL: &str = "https://play.google.com/apps/testing/dev.peppy.mobile";
pub const PLAY_INSTALL_URL: &str = "https://play.google.com/store/apps/details?id=dev.peppy.mobile";
pub const DEFAULT_SEAT_LIMIT: u32 = 15;
pub const MAX_SEAT_LIMIT: u32 = 100;
pub const LINK_CODE_BYTES: usize = 32;
pub const MAX_GROUP_URL_BYTES: usize = 512;
pub const MAX_EMAIL_BYTES: usize = 320;
pub const SEALED_ALGORITHM: &str = "x25519-xsalsa20poly1305-sealedbox";
pub const CRYPTO_BOX_SEAL_BYTES: usize = 48;
pub const SEALED_EMAIL_PREFIX_BYTES: usize = 19;
pub const SEALED_EMAIL_CONTEXT_BYTES: usize = 32;
pub const MIN_SEALED_EMAIL_BYTES: usize =
    CRYPTO_BOX_SEAL_BYTES + SEALED_EMAIL_PREFIX_BYTES + SEALED_EMAIL_CONTEXT_BYTES + 1;
pub const MAX_SEALED_EMAIL_BYTES: usize = CRYPTO_BOX_SEAL_BYTES
    + SEALED_EMAIL_PREFIX_BYTES
    + SEALED_EMAIL_CONTEXT_BYTES
    + MAX_EMAIL_BYTES;
pub const CLAIM_RETURN_PATH: &str = "/beta/claim";
pub const REQUEST_RETURN_PATH: &str = "/beta/request";
pub const TESTFLIGHT_JOIN_PREFIX: &str = "https://testflight.apple.com/join/";
pub const MAX_TESTFLIGHT_CODE_BYTES: usize = 32;
pub const MAX_PENDING_REQUESTS: u32 = 500;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BetaClaimRequest {
    #[serde(deserialize_with = "link_code")]
    pub code: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum BetaClaimOutcome {
    Claimed,
    AlreadyClaimed,
    LinkUnavailable,
    SeatsFull,
    PaidSubscription,
    AlreadyTester,
    VerifiedGoogleRequired,
    DeletionPending,
    Unavailable,
}

impl BetaClaimOutcome {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Claimed => "claimed",
            Self::AlreadyClaimed => "already_claimed",
            Self::LinkUnavailable => "link_unavailable",
            Self::SeatsFull => "seats_full",
            Self::PaidSubscription => "paid_subscription",
            Self::AlreadyTester => "already_tester",
            Self::VerifiedGoogleRequired => "verified_google_required",
            Self::DeletionPending => "deletion_pending",
            Self::Unavailable => "unavailable",
        }
    }
}

impl FromStr for BetaClaimOutcome {
    type Err = ();

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "claimed" => Ok(Self::Claimed),
            "already_claimed" => Ok(Self::AlreadyClaimed),
            "link_unavailable" => Ok(Self::LinkUnavailable),
            "seats_full" => Ok(Self::SeatsFull),
            "paid_subscription" => Ok(Self::PaidSubscription),
            "already_tester" => Ok(Self::AlreadyTester),
            "verified_google_required" => Ok(Self::VerifiedGoogleRequired),
            "deletion_pending" => Ok(Self::DeletionPending),
            "unavailable" => Ok(Self::Unavailable),
            _ => Err(()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum BetaRequestOutcome {
    Requested,
    AlreadyRequested,
    Declined,
    HasAccess,
    AlreadyTester,
    PaidSubscription,
    VerifiedGoogleRequired,
    DeletionPending,
    NotOpen,
    SeatEnded,
    Unavailable,
}

impl BetaRequestOutcome {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Requested => "requested",
            Self::AlreadyRequested => "already_requested",
            Self::Declined => "declined",
            Self::HasAccess => "has_access",
            Self::AlreadyTester => "already_tester",
            Self::PaidSubscription => "paid_subscription",
            Self::VerifiedGoogleRequired => "verified_google_required",
            Self::DeletionPending => "deletion_pending",
            Self::NotOpen => "not_open",
            Self::SeatEnded => "seat_ended",
            Self::Unavailable => "unavailable",
        }
    }
}

impl FromStr for BetaRequestOutcome {
    type Err = ();

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "requested" => Ok(Self::Requested),
            "already_requested" => Ok(Self::AlreadyRequested),
            "declined" => Ok(Self::Declined),
            "has_access" => Ok(Self::HasAccess),
            "already_tester" => Ok(Self::AlreadyTester),
            "paid_subscription" => Ok(Self::PaidSubscription),
            "verified_google_required" => Ok(Self::VerifiedGoogleRequired),
            "deletion_pending" => Ok(Self::DeletionPending),
            "not_open" => Ok(Self::NotOpen),
            "seat_ended" => Ok(Self::SeatEnded),
            "unavailable" => Ok(Self::Unavailable),
            _ => Err(()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum BetaRequestStatus {
    Pending,
    Approved,
    Declined,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BetaRequestSummary {
    pub status: BetaRequestStatus,
    pub requested_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BetaLinks {
    pub feedback_url: String,
    pub play_opt_in_url: String,
    pub install_url: String,
    #[serde(deserialize_with = "group_url")]
    pub group_url: Option<String>,
    #[serde(deserialize_with = "testflight_url")]
    pub testflight_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BetaMembershipResponse {
    pub membership: BetaMembership,
    pub links: Option<BetaLinks>,
    #[serde(deserialize_with = "required_nullable")]
    pub request: Option<BetaRequestSummary>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum BetaMembership {
    None,
    Seat,
    Tester,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SealedEmailV1 {
    #[serde(deserialize_with = "sealed_email_version")]
    #[schemars(schema_with = "sealed_email_version_schema")]
    pub version: u8,
    #[serde(deserialize_with = "sealed_email_algorithm")]
    #[schemars(schema_with = "sealed_email_algorithm_schema")]
    pub algorithm: String,
    #[serde(deserialize_with = "sealed_email")]
    pub sealed: String,
}

impl SealedEmailV1 {
    pub fn validate(&self) -> bool {
        self.version == 1
            && self.algorithm == SEALED_ALGORITHM
            && valid_base64url(&self.sealed).is_some_and(|bytes| {
                (MIN_SEALED_EMAIL_BYTES..=MAX_SEALED_EMAIL_BYTES).contains(&bytes.len())
            })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum BetaSeatStatus {
    Active,
    Revoked,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BetaSeatEntry {
    pub seat_id: Uuid,
    pub status: BetaSeatStatus,
    pub claimed_at: i64,
    pub revoked_at: Option<i64>,
    pub sealed_email: SealedEmailV1,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BetaProgramState {
    pub program_id: Uuid,
    #[serde(deserialize_with = "revision")]
    pub revision: String,
    pub enabled: bool,
    #[serde(deserialize_with = "seat_limit")]
    pub seat_limit: u32,
    pub claimed: u32,
    #[serde(deserialize_with = "group_url")]
    pub group_url: Option<String>,
    #[serde(deserialize_with = "testflight_url")]
    pub testflight_url: Option<String>,
    #[serde(deserialize_with = "public_key")]
    pub public_key: String,
    pub binding: VaultBindingV2,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BetaAdminResponse {
    pub program: Option<BetaProgramState>,
    pub seats: Vec<BetaSeatEntry>,
    pub requests: Vec<BetaRequestEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BetaSetupRequest {
    pub binding: VaultBindingV2,
    #[serde(deserialize_with = "public_key")]
    pub public_key: String,
    #[serde(deserialize_with = "seat_limit")]
    pub seat_limit: u32,
    #[serde(deserialize_with = "group_url")]
    pub group_url: Option<String>,
    #[serde(deserialize_with = "testflight_url")]
    pub testflight_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BetaLinkRequest {
    #[serde(deserialize_with = "revision")]
    pub expected_revision: String,
    pub action: BetaLinkAction,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum BetaLinkAction {
    Rotate,
    Disable,
    Enable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BetaSettingsRequest {
    #[serde(deserialize_with = "revision")]
    pub expected_revision: String,
    #[serde(deserialize_with = "seat_limit")]
    pub seat_limit: u32,
    #[serde(deserialize_with = "group_url")]
    pub group_url: Option<String>,
    #[serde(deserialize_with = "testflight_url")]
    pub testflight_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BetaRevokeRequest {
    #[serde(deserialize_with = "revision")]
    pub expected_revision: String,
    pub seat_id: Uuid,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BetaResealRequest {
    #[serde(deserialize_with = "revision")]
    pub expected_revision: String,
    pub binding: VaultBindingV2,
    #[serde(deserialize_with = "public_key")]
    pub public_key: String,
    pub seats: Vec<BetaResealSeat>,
    pub requests: Vec<BetaResealRequestEntry>,
}

impl BetaResealRequest {
    pub fn covers_seats(&self, seat_ids: &[Uuid]) -> bool {
        valid_reseal_seats(&self.seats, seat_ids)
    }

    pub fn covers_requests(&self, request_ids: &[Uuid]) -> bool {
        valid_reseal_entries(
            self.requests
                .iter()
                .map(|request| (request.request_id, &request.sealed_email)),
            request_ids,
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BetaResealSeat {
    pub seat_id: Uuid,
    pub sealed_email: SealedEmailV1,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BetaResealRequestEntry {
    pub request_id: Uuid,
    pub sealed_email: SealedEmailV1,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BetaRequestEntry {
    pub request_id: Uuid,
    pub status: BetaRequestStatus,
    pub requested_at: i64,
    pub decided_at: Option<i64>,
    pub withdrawn: bool,
    pub sealed_email: Option<SealedEmailV1>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum BetaDecision {
    Approve,
    Decline,
}

/// Carries a plaintext requester address for one decision, so it deliberately has no `Debug`.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BetaDecisionRequest {
    pub request_id: Uuid,
    pub decision: BetaDecision,
    #[serde(deserialize_with = "decision_email")]
    pub email: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BetaMutationResponse {
    pub program: BetaProgramState,
    pub seats: Vec<BetaSeatEntry>,
    pub requests: Vec<BetaRequestEntry>,
    pub link_code: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum BetaErrorCode {
    Forbidden,
    Conflict,
    Invalid,
    LimitBelowClaimed,
    SeatsFull,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BetaErrorResponse {
    pub code: BetaErrorCode,
    pub message: String,
}

pub fn valid_seat_limit(value: u32) -> bool {
    (1..=MAX_SEAT_LIMIT).contains(&value)
}

pub fn validate_seat_limit(limit: u32, active: u32) -> Result<(), BetaErrorCode> {
    if !valid_seat_limit(limit) {
        Err(BetaErrorCode::Invalid)
    } else if limit < active {
        Err(BetaErrorCode::LimitBelowClaimed)
    } else {
        Ok(())
    }
}

pub fn valid_group_url(value: Option<&str>) -> bool {
    value.is_none_or(|url| {
        url.starts_with("https://groups.google.com/")
            && url.len() <= MAX_GROUP_URL_BYTES
            && !url.chars().any(char::is_whitespace)
            && !url.chars().any(char::is_control)
    })
}

pub fn valid_link_code(value: &str) -> bool {
    value.len() == 43 && valid_base64url(value).is_some_and(|bytes| bytes.len() == LINK_CODE_BYTES)
}

pub fn valid_public_key(value: &str) -> bool {
    valid_base64url(value).is_some_and(|bytes| bytes.len() == 32)
}

pub fn valid_testflight_url(value: Option<&str>) -> bool {
    value.is_none_or(|url| {
        url.strip_prefix(TESTFLIGHT_JOIN_PREFIX)
            .is_some_and(|code| {
                (1..=MAX_TESTFLIGHT_CODE_BYTES).contains(&code.len())
                    && code.bytes().all(|byte| byte.is_ascii_alphanumeric())
            })
    })
}

pub fn valid_decision_email(value: &str) -> bool {
    (1..=MAX_EMAIL_BYTES).contains(&value.len())
        && !value
            .chars()
            .any(|character| character.is_whitespace() || character.is_control())
}

pub fn valid_reseal_seats(seats: &[BetaResealSeat], seat_ids: &[Uuid]) -> bool {
    valid_reseal_entries(
        seats.iter().map(|seat| (seat.seat_id, &seat.sealed_email)),
        seat_ids,
    )
}

fn valid_reseal_entries<'a>(
    entries: impl Iterator<Item = (Uuid, &'a SealedEmailV1)>,
    expected_ids: &[Uuid],
) -> bool {
    let entries = entries.collect::<Vec<_>>();
    let ids = entries.iter().map(|(id, _)| *id).collect::<HashSet<_>>();
    entries.len() == expected_ids.len()
        && ids.len() == entries.len()
        && entries.iter().all(|(_, sealed)| sealed.validate())
        && ids == expected_ids.iter().copied().collect()
}

fn valid_base64url(value: &str) -> Option<Vec<u8>> {
    URL_SAFE_NO_PAD
        .decode(value)
        .ok()
        .filter(|decoded| URL_SAFE_NO_PAD.encode(decoded) == value)
}

fn revision<'de, D: Deserializer<'de>>(deserializer: D) -> Result<String, D::Error> {
    let value = String::deserialize(deserializer)?;
    valid_revision(&value)
        .then_some(value)
        .ok_or_else(|| D::Error::custom("invalid registry revision"))
}

fn seat_limit<'de, D: Deserializer<'de>>(deserializer: D) -> Result<u32, D::Error> {
    let value = u32::deserialize(deserializer)?;
    valid_seat_limit(value)
        .then_some(value)
        .ok_or_else(|| D::Error::custom("invalid beta seat limit"))
}

fn group_url<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<String>, D::Error> {
    let value = Option::<String>::deserialize(deserializer)?;
    valid_group_url(value.as_deref())
        .then_some(value)
        .ok_or_else(|| D::Error::custom("invalid Google group URL"))
}

fn link_code<'de, D: Deserializer<'de>>(deserializer: D) -> Result<String, D::Error> {
    let value = String::deserialize(deserializer)?;
    valid_link_code(&value)
        .then_some(value)
        .ok_or_else(|| D::Error::custom("invalid beta link code"))
}

fn testflight_url<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<String>, D::Error> {
    let value = Option::<String>::deserialize(deserializer)?;
    valid_testflight_url(value.as_deref())
        .then_some(value)
        .ok_or_else(|| D::Error::custom("invalid TestFlight link"))
}

fn decision_email<'de, D: Deserializer<'de>>(deserializer: D) -> Result<String, D::Error> {
    let value = String::deserialize(deserializer)?;
    valid_decision_email(&value)
        .then_some(value)
        .ok_or_else(|| D::Error::custom("invalid decision email"))
}

/// Requires the key to be present while still allowing `null` (for `Option` fields).
fn required_nullable<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer)
}

fn public_key<'de, D: Deserializer<'de>>(deserializer: D) -> Result<String, D::Error> {
    let value = String::deserialize(deserializer)?;
    valid_public_key(&value)
        .then_some(value)
        .ok_or_else(|| D::Error::custom("invalid beta public key"))
}

fn sealed_email_version<'de, D: Deserializer<'de>>(deserializer: D) -> Result<u8, D::Error> {
    let value = u8::deserialize(deserializer)?;
    (value == 1)
        .then_some(value)
        .ok_or_else(|| D::Error::custom("unsupported sealed email version"))
}

fn sealed_email_algorithm<'de, D: Deserializer<'de>>(deserializer: D) -> Result<String, D::Error> {
    let value = String::deserialize(deserializer)?;
    (value == SEALED_ALGORITHM)
        .then_some(value)
        .ok_or_else(|| D::Error::custom("unsupported sealed email algorithm"))
}

fn sealed_email<'de, D: Deserializer<'de>>(deserializer: D) -> Result<String, D::Error> {
    let value = String::deserialize(deserializer)?;
    valid_base64url(&value)
        .filter(|bytes| (MIN_SEALED_EMAIL_BYTES..=MAX_SEALED_EMAIL_BYTES).contains(&bytes.len()))
        .map(|_| value)
        .ok_or_else(|| D::Error::custom("invalid sealed email"))
}

fn sealed_email_version_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({"type":"integer", "const":1})
}

fn sealed_email_algorithm_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({"type":"string", "const":"x25519-xsalsa20poly1305-sealedbox"})
}

pub mod routes {
    pub const CLAIM_START: &str = "/hosted/v1/web/beta/claim/start";
    pub const MEMBERSHIP: &str = "/hosted/v1/web/beta/membership";
    pub const ADMIN: &str = "/hosted/v2/web/admin/beta";
    pub const ADMIN_SETUP: &str = "/hosted/v2/web/admin/beta/setup";
    pub const ADMIN_LINK: &str = "/hosted/v2/web/admin/beta/link";
    pub const ADMIN_SETTINGS: &str = "/hosted/v2/web/admin/beta/settings";
    pub const ADMIN_REVOKE: &str = "/hosted/v2/web/admin/beta/revoke";
    pub const ADMIN_RESEAL: &str = "/hosted/v2/web/admin/beta/reseal";
    pub const REQUEST_START: &str = "/hosted/v1/web/beta/request/start";
    pub const ADMIN_DECIDE: &str = "/hosted/v2/web/admin/beta/requests/decide";
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sealed() -> String {
        URL_SAFE_NO_PAD.encode(vec![0; MIN_SEALED_EMAIL_BYTES])
    }

    #[test]
    fn testflight_links_are_strict() {
        assert!(valid_testflight_url(None));
        assert!(valid_testflight_url(Some(
            "https://testflight.apple.com/join/aZ09"
        )));
        assert!(valid_testflight_url(Some(&format!(
            "{}{}",
            TESTFLIGHT_JOIN_PREFIX,
            "a".repeat(32)
        ))));
        for value in [
            "https://testflight.apple.com/join/",
            "https://testflight.apple.com/join/a/",
            "https://testflight.apple.com/join/a?x=1",
            "http://testflight.apple.com/join/a",
            "https://example.com/join/a",
            "https://testflight.apple.com/join/abcdefghijklmnopqrstuvwxyzABCDEFG",
        ] {
            assert!(!valid_testflight_url(Some(value)), "{value}");
        }
    }

    #[test]
    fn sealed_email_rejects_noncanonical_envelopes() {
        let valid = SealedEmailV1 {
            version: 1,
            algorithm: SEALED_ALGORITHM.to_owned(),
            sealed: sealed(),
        };
        assert!(valid.validate());

        for (field, value) in [
            ("version", json!(2)),
            ("algorithm", json!("other")),
            ("sealed", json!("=")),
        ] {
            let mut input = serde_json::to_value(&valid).unwrap();
            input[field] = value;
            assert!(
                serde_json::from_value::<SealedEmailV1>(input).is_err(),
                "{field}"
            );
        }
    }

    #[test]
    fn validation_helpers_reject_invalid_public_inputs() {
        assert!(valid_seat_limit(1));
        assert!(valid_seat_limit(MAX_SEAT_LIMIT));
        assert!(!valid_seat_limit(0));
        assert!(!valid_seat_limit(MAX_SEAT_LIMIT + 1));
        assert!(valid_group_url(Some("https://groups.google.com/g/peppy")));
        assert!(!valid_group_url(Some("https://example.com/group")));
        assert!(!valid_group_url(Some(
            "https://groups.google.com/g/with space"
        )));
        assert!(valid_link_code(
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"
        ));
        assert!(!valid_link_code(
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA="
        ));
        assert!(valid_public_key(
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"
        ));
        assert!(!valid_public_key("not-base64url"));
    }

    #[test]
    fn seat_limit_cannot_drop_below_active_claims() {
        assert!(validate_seat_limit(15, 15).is_ok());
        assert!(validate_seat_limit(15, 16).is_err());
        assert_eq!(BetaLinkAction::Enable, BetaLinkAction::Enable);
        assert_eq!(
            BetaErrorCode::LimitBelowClaimed,
            BetaErrorCode::LimitBelowClaimed
        );
    }

    #[test]
    fn claim_outcomes_roundtrip_query_values() {
        for outcome in [
            BetaClaimOutcome::Claimed,
            BetaClaimOutcome::AlreadyClaimed,
            BetaClaimOutcome::LinkUnavailable,
            BetaClaimOutcome::SeatsFull,
            BetaClaimOutcome::PaidSubscription,
            BetaClaimOutcome::AlreadyTester,
            BetaClaimOutcome::VerifiedGoogleRequired,
            BetaClaimOutcome::DeletionPending,
            BetaClaimOutcome::Unavailable,
        ] {
            assert_eq!(outcome.as_str().parse::<BetaClaimOutcome>(), Ok(outcome));
        }
        assert!("invalid".parse::<BetaClaimOutcome>().is_err());
    }

    #[test]
    fn reseal_request_requires_each_seat_once() {
        let seat_id = Uuid::nil();
        let entry = BetaResealSeat {
            seat_id,
            sealed_email: SealedEmailV1 {
                version: 1,
                algorithm: SEALED_ALGORITHM.to_owned(),
                sealed: sealed(),
            },
        };
        assert!(valid_reseal_seats(std::slice::from_ref(&entry), &[seat_id]));
        assert!(!valid_reseal_seats(&[entry.clone(), entry], &[seat_id]));
    }

    #[test]
    fn public_requests_roundtrip_and_reject_unknown_fields() {
        let request = json!({
            "binding": {
                "account_id": "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
                "vault_id": "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb",
                "profile": {
                    "crypto_suite": 1,
                    "salt": [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
                    "vault_id": "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb",
                    "key_epoch": 1
                },
                "profile_fingerprint": "fingerprint",
                "key_epoch": 1
            },
            "public_key": "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
            "seat_limit": 15,
            "group_url": "https://groups.google.com/g/peppy",
            "testflight_url": "https://testflight.apple.com/join/AbC123"
        });
        let parsed: BetaSetupRequest = serde_json::from_value(request.clone()).unwrap();
        assert_eq!(serde_json::to_value(parsed).unwrap(), request);

        let mut unknown = request;
        unknown["unexpected"] = json!(true);
        assert!(serde_json::from_value::<BetaSetupRequest>(unknown).is_err());
    }

    fn setup_request_json() -> serde_json::Value {
        json!({
            "binding": {
                "account_id": "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
                "vault_id": "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb",
                "profile": {
                    "crypto_suite": 1,
                    "salt": [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
                    "vault_id": "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb",
                    "key_epoch": 1
                },
                "profile_fingerprint": "fingerprint",
                "key_epoch": 1
            },
            "public_key": "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
            "seat_limit": 15,
            "group_url": null,
            "testflight_url": null
        })
    }

    #[test]
    fn testflight_url_accepts_only_public_join_links() {
        assert!(valid_testflight_url(None));
        assert!(valid_testflight_url(Some(
            "https://testflight.apple.com/join/AbC123"
        )));
        assert!(valid_testflight_url(Some(&format!(
            "{TESTFLIGHT_JOIN_PREFIX}{}",
            "a".repeat(MAX_TESTFLIGHT_CODE_BYTES)
        ))));
        for invalid in [
            "http://testflight.apple.com/join/AbC123",
            "https://example.com/join/AbC123",
            "https://testflight.apple.com/join/AbC123?x=1",
            "https://testflight.apple.com/join/AbC123#frag",
            "https://testflight.apple.com/join/AbC123/",
            "https://testflight.apple.com/join/",
            "https://testflight.apple.com/join/AbC-123",
            "https://testflight.apple.com/join/AbC 123",
            "",
        ] {
            assert!(!valid_testflight_url(Some(invalid)), "{invalid}");
        }
        assert!(!valid_testflight_url(Some(&format!(
            "{TESTFLIGHT_JOIN_PREFIX}{}",
            "a".repeat(MAX_TESTFLIGHT_CODE_BYTES + 1)
        ))));
    }

    #[test]
    fn testflight_url_is_required_nullable_and_validated() {
        let mut missing = setup_request_json();
        missing.as_object_mut().unwrap().remove("testflight_url");
        assert!(serde_json::from_value::<BetaSetupRequest>(missing).is_err());

        let mut invalid = setup_request_json();
        invalid["testflight_url"] = json!("https://example.com/join/AbC123");
        assert!(serde_json::from_value::<BetaSetupRequest>(invalid).is_err());

        let valid = setup_request_json();
        let parsed: BetaSetupRequest = serde_json::from_value(valid).unwrap();
        assert_eq!(parsed.testflight_url, None);

        let settings = json!({
            "expected_revision": "1",
            "seat_limit": 15,
            "group_url": null
        });
        assert!(serde_json::from_value::<BetaSettingsRequest>(settings).is_err());
    }

    #[test]
    fn group_url_key_stays_required_and_nullable() {
        let mut missing = setup_request_json();
        missing.as_object_mut().unwrap().remove("group_url");
        assert!(serde_json::from_value::<BetaSetupRequest>(missing).is_err());
    }

    #[test]
    fn request_outcomes_roundtrip_query_values() {
        for outcome in [
            BetaRequestOutcome::Requested,
            BetaRequestOutcome::AlreadyRequested,
            BetaRequestOutcome::Declined,
            BetaRequestOutcome::HasAccess,
            BetaRequestOutcome::AlreadyTester,
            BetaRequestOutcome::PaidSubscription,
            BetaRequestOutcome::VerifiedGoogleRequired,
            BetaRequestOutcome::DeletionPending,
            BetaRequestOutcome::NotOpen,
            BetaRequestOutcome::SeatEnded,
            BetaRequestOutcome::Unavailable,
        ] {
            assert_eq!(outcome.as_str().parse::<BetaRequestOutcome>(), Ok(outcome));
            let json = serde_json::to_value(outcome).unwrap();
            assert_eq!(json, json!(outcome.as_str()));
            assert_eq!(
                serde_json::from_value::<BetaRequestOutcome>(json).unwrap(),
                outcome
            );
        }
        assert!("invalid".parse::<BetaRequestOutcome>().is_err());
    }

    #[test]
    fn seats_full_error_code_is_snake_case() {
        assert_eq!(
            serde_json::to_value(BetaErrorCode::SeatsFull).unwrap(),
            json!("seats_full")
        );
    }

    #[test]
    fn membership_requires_request_key_and_rejects_unknown_fields() {
        let base = json!({
            "membership": "none",
            "links": null,
            "request": null
        });
        let parsed: BetaMembershipResponse = serde_json::from_value(base.clone()).unwrap();
        assert_eq!(parsed.request, None);

        let mut missing = base.clone();
        missing.as_object_mut().unwrap().remove("request");
        assert!(serde_json::from_value::<BetaMembershipResponse>(missing).is_err());

        let pending = json!({
            "membership": "none",
            "links": null,
            "request": {"status": "pending", "requested_at": 1}
        });
        let parsed: BetaMembershipResponse = serde_json::from_value(pending.clone()).unwrap();
        assert_eq!(
            parsed.request,
            Some(BetaRequestSummary {
                status: BetaRequestStatus::Pending,
                requested_at: 1,
            })
        );
        assert_eq!(serde_json::to_value(parsed).unwrap(), pending);

        let mut unknown = base;
        unknown["unexpected"] = json!(true);
        assert!(serde_json::from_value::<BetaMembershipResponse>(unknown).is_err());

        let mut summary_unknown = pending;
        summary_unknown["request"]["unexpected"] = json!(true);
        assert!(serde_json::from_value::<BetaMembershipResponse>(summary_unknown).is_err());
    }

    #[test]
    fn request_entries_reject_unknown_fields_and_keep_nullable_rows() {
        let entry = json!({
            "request_id": "cccccccc-cccc-4ccc-8ccc-cccccccccccc",
            "status": "declined",
            "requested_at": 10,
            "decided_at": 20,
            "withdrawn": false,
            "sealed_email": null
        });
        let parsed: BetaRequestEntry = serde_json::from_value(entry.clone()).unwrap();
        assert_eq!(parsed.status, BetaRequestStatus::Declined);
        assert_eq!(parsed.sealed_email, None);
        assert_eq!(serde_json::to_value(parsed).unwrap(), entry);

        let mut unknown = entry;
        unknown["unexpected"] = json!(true);
        assert!(serde_json::from_value::<BetaRequestEntry>(unknown).is_err());
    }

    fn decision_json(email: &str) -> serde_json::Value {
        json!({
            "request_id": "cccccccc-cccc-4ccc-8ccc-cccccccccccc",
            "decision": "approve",
            "email": email
        })
    }

    #[test]
    fn decision_email_enforces_byte_bounds_and_no_whitespace_or_control() {
        assert!(valid_decision_email("a@example.com"));
        assert!(valid_decision_email(&"a".repeat(MAX_EMAIL_BYTES)));
        assert!(!valid_decision_email(""));
        assert!(!valid_decision_email(&"a".repeat(MAX_EMAIL_BYTES + 1)));
        assert!(!valid_decision_email("a b@example.com"));
        assert!(!valid_decision_email("a\t@example.com"));
        assert!(!valid_decision_email("a\u{7f}@example.com"));

        let parsed: BetaDecisionRequest =
            serde_json::from_value(decision_json("a@example.com")).unwrap();
        assert_eq!(parsed.decision, BetaDecision::Approve);
        assert!(serde_json::from_value::<BetaDecisionRequest>(decision_json("")).is_err());
        assert!(
            serde_json::from_value::<BetaDecisionRequest>(decision_json(
                &"a".repeat(MAX_EMAIL_BYTES + 1)
            ))
            .is_err()
        );
        assert!(
            serde_json::from_value::<BetaDecisionRequest>(decision_json("a @example.com")).is_err()
        );

        let mut unknown = decision_json("a@example.com");
        unknown["expected_revision"] = json!("1");
        assert!(serde_json::from_value::<BetaDecisionRequest>(unknown).is_err());
    }

    #[test]
    fn reseal_requests_cover_each_request_exactly_once() {
        let request_id = Uuid::nil();
        let sealed_email = SealedEmailV1 {
            version: 1,
            algorithm: SEALED_ALGORITHM.to_owned(),
            sealed: sealed(),
        };
        let entry = BetaResealRequestEntry {
            request_id,
            sealed_email: sealed_email.clone(),
        };
        let request = |requests: Vec<BetaResealRequestEntry>| BetaResealRequest {
            expected_revision: "1".to_owned(),
            binding: serde_json::from_value(setup_request_json()["binding"].clone()).unwrap(),
            public_key: "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA".to_owned(),
            seats: vec![],
            requests,
        };

        assert!(request(vec![entry.clone()]).covers_requests(&[request_id]));
        assert!(request(vec![]).covers_requests(&[]));
        assert!(!request(vec![]).covers_requests(&[request_id]));
        assert!(!request(vec![entry.clone()]).covers_requests(&[]));
        assert!(!request(vec![entry.clone(), entry.clone()]).covers_requests(&[request_id]));
        assert!(!request(vec![entry]).covers_requests(&[Uuid::from_u128(1)]));

        let mut invalid = BetaResealRequestEntry {
            request_id,
            sealed_email,
        };
        invalid.sealed_email.version = 2;
        assert!(!request(vec![invalid]).covers_requests(&[request_id]));
    }

    #[test]
    fn admin_and_mutation_responses_require_requests_key() {
        let admin = json!({"program": null, "seats": []});
        assert!(serde_json::from_value::<BetaAdminResponse>(admin).is_err());
        let admin = json!({"program": null, "seats": [], "requests": []});
        assert!(serde_json::from_value::<BetaAdminResponse>(admin).is_ok());
    }
}
