//! Server origin validation. Credentials are bound to exactly one normalized origin; the host
//! never forwards them elsewhere (redirects are disabled for every request).
use crate::error::{BridgeError, BridgeResult};
use peppy_hosted_client::device_credentials::{canonical_credential_origin, OriginError};
use url::{Host, Url};

fn invalid(message: &'static str) -> BridgeError {
    BridgeError::new("invalid-origin", message)
}

pub fn shared_origin_error(error: OriginError) -> BridgeError {
    match error {
        OriginError::Empty => invalid("Enter a server origin such as https://messages.example."),
        OriginError::InvalidUrl => invalid("The server origin is not a valid URL."),
        OriginError::Credentials => invalid("Server origin must not include credentials."),
        OriginError::PathQueryOrFragment => {
            invalid("Server origin must not include a path, query, or fragment.")
        }
        OriginError::Insecure => {
            invalid("Use an HTTPS origin, or an explicit loopback HTTP origin for development.")
        }
    }
}

fn is_loopback(url: &Url) -> bool {
    match url.host() {
        Some(Host::Ipv4(ip)) => ip.is_loopback(),
        Some(Host::Ipv6(ip)) => ip.is_loopback(),
        Some(Host::Domain(domain)) => domain.eq_ignore_ascii_case("localhost"),
        None => false,
    }
}

/// Returns the normalized `scheme://host[:port]` origin. HTTPS is required except for explicit
/// loopback development origins (`127.0.0.0/8`, `::1`, `localhost`).
pub fn validate_origin(value: &str) -> BridgeResult<String> {
    canonical_credential_origin(value).map_err(shared_origin_error)
}

/// True when plaintext HTTP is permitted for this (already validated) origin.
pub fn is_loopback_http(origin: &str) -> bool {
    Url::parse(origin).is_ok_and(|url| url.scheme() == "http" && is_loopback(&url))
}

/// WebSocket endpoint for a validated origin (`wss` for HTTPS, `ws` for loopback HTTP only).
pub fn websocket_url(origin: &str) -> BridgeResult<String> {
    let origin = validate_origin(origin)?;
    if let Some(rest) = origin.strip_prefix("https://") {
        Ok(format!("wss://{rest}/v1/ws"))
    } else if let Some(rest) = origin.strip_prefix("http://") {
        Ok(format!("ws://{rest}/v1/ws"))
    } else {
        Err(invalid("Unsupported server origin."))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_non_origin_urls_and_allows_explicit_loopback() {
        assert!(validate_origin("https://example.test/v1").is_err());
        assert!(validate_origin("https://user@example.test").is_err());
        assert!(validate_origin("https://example.test/?x=1").is_err());
        assert!(validate_origin("https://example.test/#frag").is_err());
        assert!(validate_origin("http://example.test:8080").is_err());
        assert!(validate_origin("http://localhost.evil.test:8080").is_err());
        assert!(validate_origin("http://127.0.0.1.evil.test").is_err());
        assert!(validate_origin("ws://127.0.0.1:8080").is_err());
        assert!(validate_origin("javascript:alert(1)").is_err());
        assert!(validate_origin("file:///etc/passwd").is_err());
        assert_eq!(
            validate_origin("http://127.0.0.1:18333/").unwrap(),
            "http://127.0.0.1:18333"
        );
        assert_eq!(
            validate_origin(" HTTPS://Example.TEST:443/ ").unwrap(),
            "https://example.test"
        );
        assert_eq!(
            validate_origin("http://[::1]:8080").unwrap(),
            "http://[::1]:8080"
        );
        assert_eq!(
            validate_origin("http://localhost:1420").unwrap(),
            "http://localhost:1420"
        );
    }

    #[test]
    fn websocket_url_preserves_tls_requirement() {
        assert_eq!(
            websocket_url("https://example.test").unwrap(),
            "wss://example.test/v1/ws"
        );
        assert_eq!(
            websocket_url("http://127.0.0.1:8080").unwrap(),
            "ws://127.0.0.1:8080/v1/ws"
        );
        assert!(websocket_url("http://example.test").is_err());
        assert!(is_loopback_http("http://127.0.0.1:8080"));
        assert!(!is_loopback_http("https://example.test"));
    }

    #[test]
    fn shared_origin_errors_keep_native_codes_and_messages() {
        for (input, message) in [
            (
                "",
                "Enter a server origin such as https://messages.example.",
            ),
            (
                "https://user@example.test",
                "Server origin must not include credentials.",
            ),
            (
                "https://example.test/path",
                "Server origin must not include a path, query, or fragment.",
            ),
            (
                "http://example.test",
                "Use an HTTPS origin, or an explicit loopback HTTP origin for development.",
            ),
        ] {
            let error = validate_origin(input).unwrap_err();
            assert_eq!(error.code, "invalid-origin");
            assert_eq!(error.message, message);
        }
    }
}
