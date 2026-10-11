//! Self-host instance setup and administrator session primitives.

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use libsodium_rs::crypto_pwhash::argon2id;
use sha2::{Digest, Sha256};
use std::sync::OnceLock;

const SESSION_TOKEN_BYTES: usize = 32;
const MIN_CONFIGURED_SETUP_TOKEN_BYTES: usize = 32;
static GENERATED_SETUP_TOKEN: OnceLock<String> = OnceLock::new();

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
    validate_password(password).is_ok() && argon2id::pwhash_str_verify(hash, password.as_bytes())
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

pub(crate) fn operator_setup_token_digest(token: &str) -> Result<[u8; 32], &'static str> {
    if token.len() < MIN_CONFIGURED_SETUP_TOKEN_BYTES {
        return Err("setup token must be at least 32 bytes");
    }
    Ok(digest(token.as_bytes()))
}

/// Parses the optional `PEPPY_SETUP_TOKEN` value. Absent and empty values both
/// mean "not configured", so Compose can forward an unset variable as `""`.
/// Empty input never disables the first-claim requirement; the caller falls
/// back to the generated first-claim token.
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

/// Returns a setup token digest. Call this only after proving that the instance
/// is unclaimed. The generated fallback is process-local and logged once so an
/// operator can perform the first claim.
pub fn setup_token_digest() -> Result<[u8; 32], &'static str> {
    if let Some(configured) = configured_setup_token_digest()? {
        return Ok(configured);
    }
    if let Some(token) = GENERATED_SETUP_TOKEN.get() {
        return Ok(digest(token.as_bytes()));
    }
    let candidate = random_token()?;
    if GENERATED_SETUP_TOKEN.set(candidate.clone()).is_ok() {
        tracing::warn!(
            setup_token = %candidate,
            "generated process-local setup token; configure PEPPY_SETUP_TOKEN for replicas"
        );
    }
    let token = GENERATED_SETUP_TOKEN
        .get()
        .expect("setup token is initialized before it is read");
    Ok(digest(token.as_bytes()))
}

/// An unreachable digest used when setup is disabled for an already-claimed or
/// hosted instance. No setup-shaped secret is generated or logged.
pub fn disabled_setup_token_digest() -> [u8; 32] {
    [0; 32]
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
