//! Self-host instance setup and administrator session primitives.

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use libsodium_rs::crypto_pwhash::{self, argon2id};
use sha2::{Digest, Sha256};

const SESSION_TOKEN_BYTES: usize = 32;
const MIN_CONFIGURED_SETUP_TOKEN_BYTES: usize = 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClaimPolicy {
    Open,
    TokenRequired([u8; 32]),
    Disabled,
}

impl ClaimPolicy {
    pub fn requires_setup_token(self) -> bool {
        matches!(self, Self::TokenRequired(_))
    }

    pub fn authorizes(self, token: &str) -> bool {
        match self {
            Self::Open => true,
            Self::TokenRequired(expected) => expected == digest(token.as_bytes()),
            Self::Disabled => false,
        }
    }
}

pub fn validate_password(password: &str) -> Result<(), &'static str> {
    if !(12..=1024).contains(&password.len()) {
        return Err("invalid_admin_password");
    }
    Ok(())
}

pub fn password_hash(password: &str) -> Result<String, &'static str> {
    validate_password(password)?;
    libsodium_rs::ensure_init().map_err(|_| "password_hash_failed")?;
    argon2id::pwhash_str(
        password.as_bytes(),
        argon2id::OPSLIMIT_INTERACTIVE,
        argon2id::MEMLIMIT_INTERACTIVE,
    )
    .map_err(|_| "password_hash_failed")
}

pub fn password_matches(hash: &str, password: &str) -> bool {
    validate_password(password).is_ok()
        && crypto_pwhash::pwhash_str_verify(hash, password.as_bytes()).unwrap_or(false)
}

pub fn digest(value: &[u8]) -> [u8; 32] {
    Sha256::digest(value).into()
}

pub fn random_token() -> Result<String, &'static str> {
    libsodium_rs::ensure_init().map_err(|_| "random_failed")?;
    let mut bytes = [0; SESSION_TOKEN_BYTES];
    libsodium_rs::random::fill_bytes(&mut bytes);
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

pub fn configured_setup_token_digest() -> Result<Option<[u8; 32]>, &'static str> {
    setup_token_digest_from_env(std::env::var("PEPPY_SETUP_TOKEN"))
}

/// Resolves the only first-claim policy used by community and hosted assembly.
/// Row-security runtimes never expose community instance setup, even if a token
/// was supplied by a shared environment.
pub fn claim_policy(
    row_security: bool,
    setup_token: Option<&str>,
) -> Result<ClaimPolicy, &'static str> {
    if row_security {
        return Ok(ClaimPolicy::Disabled);
    }
    setup_token
        .filter(|token| !token.is_empty())
        .map(operator_setup_token_digest)
        .transpose()
        .map(|digest| digest.map_or(ClaimPolicy::Open, ClaimPolicy::TokenRequired))
}

pub fn configured_claim_policy(row_security: bool) -> Result<ClaimPolicy, &'static str> {
    if row_security {
        return Ok(ClaimPolicy::Disabled);
    }
    configured_setup_token_digest()
        .map(|digest| digest.map_or(ClaimPolicy::Open, ClaimPolicy::TokenRequired))
}

pub(crate) fn operator_setup_token_digest(token: &str) -> Result<[u8; 32], &'static str> {
    if token.len() < MIN_CONFIGURED_SETUP_TOKEN_BYTES {
        return Err("setup token must be at least 32 bytes");
    }
    Ok(digest(token.as_bytes()))
}

/// Parses the optional `PEPPY_SETUP_TOKEN` value. Absent and empty values both
/// mean "not configured", so Compose can forward an unset variable as `""`.
fn setup_token_digest_from_env(
    value: Result<String, std::env::VarError>,
) -> Result<Option<[u8; 32]>, &'static str> {
    match value {
        Ok(token) if token.is_empty() => Ok(None),
        Ok(token) => operator_setup_token_digest(&token)
            .map(Some)
            .map_err(|_| "invalid_setup_token"),
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(std::env::VarError::NotUnicode(_)) => Err("invalid_setup_token"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env::VarError;

    #[test]
    fn absent_setup_token_is_unconfigured() {
        assert_eq!(
            setup_token_digest_from_env(Err(VarError::NotPresent)),
            Ok(None)
        );
    }

    #[test]
    fn empty_setup_token_is_unconfigured_not_an_error() {
        assert_eq!(setup_token_digest_from_env(Ok(String::new())), Ok(None));
    }

    #[test]
    fn short_configured_setup_token_is_rejected() {
        assert_eq!(
            setup_token_digest_from_env(Ok("a".repeat(31))),
            Err("invalid_setup_token")
        );
    }

    #[test]
    fn configured_setup_token_accepts_at_least_32_utf8_bytes() {
        let token = "é".repeat(16);
        assert_eq!(
            setup_token_digest_from_env(Ok(token.clone())),
            Ok(Some(digest(token.as_bytes())))
        );
    }

    #[test]
    fn claim_policy_is_open_without_a_token_and_disabled_for_hosted() {
        assert_eq!(claim_policy(false, None), Ok(ClaimPolicy::Open));
        assert_eq!(claim_policy(false, Some("")), Ok(ClaimPolicy::Open));
        assert_eq!(
            claim_policy(false, Some(&"a".repeat(32))),
            Ok(ClaimPolicy::TokenRequired(digest(
                b"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
            )))
        );
        assert_eq!(
            claim_policy(true, Some(&"a".repeat(32))),
            Ok(ClaimPolicy::Disabled)
        );
        assert_eq!(
            claim_policy(true, Some("too-short")),
            Ok(ClaimPolicy::Disabled)
        );
    }

    #[cfg(unix)]
    #[test]
    fn non_unicode_setup_token_is_rejected() {
        use std::os::unix::ffi::OsStringExt;

        let invalid = std::ffi::OsString::from_vec(vec![0xff]);
        assert_eq!(
            setup_token_digest_from_env(Err(VarError::NotUnicode(invalid))),
            Err("invalid_setup_token")
        );
    }
}
