use base64::{Engine as _, engine::general_purpose::STANDARD};
use hmac::{Hmac, Mac};
use sha2::Sha256;
use thiserror::Error;

const SECRET_PREFIX: &str = "whsec_";

#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum SigningError {
    #[error("invalid webhook signing secret")]
    InvalidSecret,
}

/// Generates a Standard Webhooks signing secret.
pub fn generate_secret() -> String {
    let _ = libsodium_rs::ensure_init();
    let mut bytes = [0_u8; 32];
    libsodium_rs::random::fill_bytes(&mut bytes);
    format!("{SECRET_PREFIX}{}", STANDARD.encode(bytes))
}

/// Signs an exact webhook body without exposing the secret in diagnostics.
pub fn sign(
    secret: &str,
    msg_id: &str,
    timestamp: i64,
    body: &[u8],
) -> Result<String, SigningError> {
    let key = decode_secret(secret)?;
    let mut mac = Hmac::<Sha256>::new_from_slice(&key).map_err(|_| SigningError::InvalidSecret)?;
    mac.update(msg_id.as_bytes());
    mac.update(b".");
    mac.update(timestamp.to_string().as_bytes());
    mac.update(b".");
    mac.update(body);
    Ok(format!(
        "v1,{}",
        STANDARD.encode(mac.finalize().into_bytes())
    ))
}

fn decode_secret(secret: &str) -> Result<Vec<u8>, SigningError> {
    let encoded = secret
        .strip_prefix(SECRET_PREFIX)
        .filter(|value| !value.is_empty())
        .ok_or(SigningError::InvalidSecret)?;
    let decoded = STANDARD
        .decode(encoded)
        .map_err(|_| SigningError::InvalidSecret)?;
    if decoded.is_empty() {
        return Err(SigningError::InvalidSecret);
    }
    Ok(decoded)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signs_standard_webhooks_reference_vector() {
        let signature = sign(
            "whsec_MfKQ9r8GKYqrTwjUPD8ILPZIo2LaLaSw",
            "msg_p5jXN8AQM9LWM0D4loKWxJek",
            1_614_265_330,
            br#"{"test": 2432232314}"#,
        )
        .expect("reference secret is valid");
        assert_eq!(signature, "v1,g0hM9SsE+OTPJTGt/tmIKtSyZlE3uFJELVlNIOLJ1OE=");
    }

    #[test]
    fn rejects_malformed_secrets() {
        for secret in ["", "secret", "whsec_not-base64", "whsec_"] {
            assert_eq!(
                sign(secret, "msg", 0, b"{}"),
                Err(SigningError::InvalidSecret)
            );
        }
    }

    #[test]
    fn generated_secret_has_expected_shape() {
        let secret = generate_secret();
        assert!(secret.starts_with(SECRET_PREFIX));
        assert_eq!(
            STANDARD
                .decode(&secret[SECRET_PREFIX.len()..])
                .expect("base64")
                .len(),
            32
        );
    }
}
