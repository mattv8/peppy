//! Portable device credential parsing and serialization. Credentials contain a sensitive
//! plaintext device-authentication token that must be zeroized on drop and never exposed to
//! unauthoritative parsers or debug formatting.

use serde::{Deserialize, Serialize};
use url::{Host, Url};
use uuid::Uuid;
use zeroize::{Zeroize, Zeroizing};

// Constants
pub const MAX_CREDENTIAL_BYTES: usize = 16 * 1024;
pub const CREDENTIAL_EXPORT_FILENAME: &str = "peppy-credentials.json";

// Error types
#[derive(Debug, PartialEq, Eq)]
pub enum PortableCredentialError {
    TooLarge,
    InvalidJson,
    UnsupportedVersion,
    InvalidToken,
    InvalidVaultId,
    InvalidDeviceId,
    InvalidOrigin(OriginError),
    Serialization,
}

#[derive(Debug, PartialEq, Eq)]
pub enum OriginError {
    Empty,
    InvalidUrl,
    Credentials,
    PathQueryOrFragment,
    Insecure,
}

// Wire type: deserialization only, serialization via CredentialWireRef
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CredentialWire {
    version: u8,
    origin: String,
    vault_id: String,
    device_id: String,
    device_token: String,
}

impl Drop for CredentialWire {
    fn drop(&mut self) {
        self.device_token.zeroize();
    }
}

/// A portable device credential. Its token is zeroized when the value is dropped and cannot be
/// cloned or formatted by accidental diagnostics.
pub struct PortableDeviceCredential {
    version: u8,
    origin: String,
    vault_id: String,
    device_id: String,
    device_token: String,
}

impl Drop for PortableDeviceCredential {
    fn drop(&mut self) {
        self.device_token.zeroize();
    }
}

impl PortableDeviceCredential {
    pub fn new(
        origin: String,
        vault_id: String,
        device_id: String,
        device_token: String,
    ) -> Result<Self, PortableCredentialError> {
        let mut device_token = Zeroizing::new(device_token);
        if !valid_token(&device_token) {
            return Err(PortableCredentialError::InvalidToken);
        }
        let origin =
            canonical_credential_origin(&origin).map_err(PortableCredentialError::InvalidOrigin)?;
        let vault_id = parse_id(&vault_id, PortableCredentialError::InvalidVaultId)?;
        let device_id = parse_id(&device_id, PortableCredentialError::InvalidDeviceId)?;
        Ok(Self {
            version: 1,
            origin,
            vault_id,
            device_id,
            device_token: std::mem::take(&mut *device_token),
        })
    }

    pub fn origin(&self) -> &str {
        &self.origin
    }

    pub fn vault_id(&self) -> &str {
        &self.vault_id
    }

    pub fn device_id(&self) -> &str {
        &self.device_id
    }

    pub fn device_token(&self) -> &str {
        &self.device_token
    }
}

/// Normalizes an origin using the existing desktop credential policy.
pub fn canonical_credential_origin(value: &str) -> Result<String, OriginError> {
    let value = value.trim();
    if value.is_empty() || value.len() > 2048 {
        return Err(OriginError::Empty);
    }
    let url = Url::parse(value).map_err(|_| OriginError::InvalidUrl)?;
    if !url.username().is_empty() || url.password().is_some() {
        return Err(OriginError::Credentials);
    }
    if url.query().is_some() || url.fragment().is_some() || !matches!(url.path(), "" | "/") {
        return Err(OriginError::PathQueryOrFragment);
    }
    let loopback = matches!(url.host(), Some(Host::Ipv4(ip)) if ip.is_loopback())
        || matches!(url.host(), Some(Host::Ipv6(ip)) if ip.is_loopback())
        || matches!(url.host(), Some(Host::Domain(domain)) if domain.eq_ignore_ascii_case("localhost"));
    if !matches!(url.scheme(), "https") && !(url.scheme() == "http" && loopback) {
        return Err(OriginError::Insecure);
    }
    if url.host().is_none() {
        return Err(OriginError::InvalidUrl);
    }
    Ok(url.origin().ascii_serialization())
}

pub fn parse_portable_credential(
    bytes: &[u8],
) -> Result<PortableDeviceCredential, PortableCredentialError> {
    if bytes.len() > MAX_CREDENTIAL_BYTES {
        return Err(PortableCredentialError::TooLarge);
    }
    let mut wire: CredentialWire =
        serde_json::from_slice(bytes).map_err(|_| PortableCredentialError::InvalidJson)?;
    if wire.version != 1 {
        return Err(PortableCredentialError::UnsupportedVersion);
    }
    PortableDeviceCredential::new(
        std::mem::take(&mut wire.origin),
        std::mem::take(&mut wire.vault_id),
        std::mem::take(&mut wire.device_id),
        std::mem::take(&mut wire.device_token),
    )
}

pub fn serialize_portable_credential(
    credential: &PortableDeviceCredential,
) -> Result<Zeroizing<Vec<u8>>, PortableCredentialError> {
    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct CredentialWireRef<'a> {
        version: u8,
        origin: &'a str,
        vault_id: &'a str,
        device_id: &'a str,
        device_token: &'a str,
    }

    serde_json::to_vec(&CredentialWireRef {
        version: credential.version,
        origin: &credential.origin,
        vault_id: &credential.vault_id,
        device_id: &credential.device_id,
        device_token: &credential.device_token,
    })
    .map(Zeroizing::new)
    .map_err(|_| PortableCredentialError::Serialization)
}

fn parse_id(
    value: &str,
    error: PortableCredentialError,
) -> Result<String, PortableCredentialError> {
    Uuid::parse_str(value)
        .map(|id| id.to_string())
        .map_err(|_| error)
}

fn valid_token(value: &str) -> bool {
    value.len() == 96 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOKEN: &str = "ABCDEF0123456789ABCDEF0123456789ABCDEF0123456789ABCDEF0123456789ABCDEF0123456789ABCDEF0123456789";

    #[test]
    fn portable_credentials_normalize_ids_and_preserve_token_bytes() {
        let credential = parse_portable_credential(
            format!(
                r#"{{"version":1,"origin":" HTTPS://Example.TEST:443/ ","vaultId":"{vault}","deviceId":"{device}","deviceToken":"{TOKEN}"}}"#,
                vault = "A0A0A0A0-0000-0000-0000-000000000001",
                device = "B0B0B0B0-0000-0000-0000-000000000002",
            )
            .as_bytes(),
        )
        .unwrap();

        assert_eq!(credential.origin(), "https://example.test");
        assert_eq!(
            credential.vault_id(),
            "a0a0a0a0-0000-0000-0000-000000000001"
        );
        assert_eq!(
            credential.device_id(),
            "b0b0b0b0-0000-0000-0000-000000000002"
        );
        assert_eq!(credential.device_token(), TOKEN);
        assert!(
            String::from_utf8(serialize_portable_credential(&credential).unwrap().to_vec())
                .unwrap()
                .contains(TOKEN)
        );
    }

    #[test]
    fn portable_credentials_reject_invalid_contract_fields() {
        let valid = format!(
            r#"{{"version":1,"origin":"https://example.test","vaultId":"{}","deviceId":"{}","deviceToken":"{}"}}"#,
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            "a".repeat(96),
        );
        for (payload, expected) in [
            (
                valid.replace("\"version\":1", "\"version\":2"),
                PortableCredentialError::UnsupportedVersion,
            ),
            (
                valid.replace("\"deviceToken\":\"", "\"extra\":true,\"deviceToken\":\""),
                PortableCredentialError::InvalidJson,
            ),
            (
                valid.replace("\"deviceToken\":\"", "\"deviceToken\":\"x"),
                PortableCredentialError::InvalidToken,
            ),
        ] {
            assert_eq!(parse_error(payload.as_bytes()), expected);
        }
        assert_eq!(
            parse_error(&vec![b' '; MAX_CREDENTIAL_BYTES + 1]),
            PortableCredentialError::TooLarge
        );
    }

    fn parse_error(bytes: &[u8]) -> PortableCredentialError {
        parse_portable_credential(bytes)
            .err()
            .expect("invalid credential")
    }

    #[test]
    fn canonical_origin_allows_only_https_or_loopback_http() {
        assert_eq!(
            canonical_credential_origin(" HTTP://127.0.0.2:8080/ ").unwrap(),
            "http://127.0.0.2:8080"
        );
        assert_eq!(
            canonical_credential_origin("http://example.test").unwrap_err(),
            OriginError::Insecure
        );
    }

    #[test]
    fn canonical_origin_matches_desktop_loopback_and_rejection_policy() {
        for (input, expected) in [
            (" HTTPS://Example.TEST:443/ ", "https://example.test"),
            ("https://example.test:8443/", "https://example.test:8443"),
            ("http://127.0.0.1", "http://127.0.0.1"),
            (
                "http://127.255.255.255:8080/",
                "http://127.255.255.255:8080",
            ),
            ("http://[::1]:8080", "http://[::1]:8080"),
            ("http://localhost:1420/", "http://localhost:1420"),
        ] {
            assert_eq!(
                canonical_credential_origin(input).unwrap(),
                expected,
                "{input}"
            );
        }
        for input in [
            "http://127.0.0.1.evil.test",
            "http://localhost.evil.test",
            "http://example.test",
            "ws://127.0.0.1",
        ] {
            assert_eq!(
                canonical_credential_origin(input),
                Err(OriginError::Insecure),
                "{input}"
            );
        }
        for input in [
            "https://example.test/path",
            "https://example.test?query=value",
            "https://example.test#fragment",
            "javascript:alert(1)",
            "file:///etc/passwd",
        ] {
            assert_eq!(
                canonical_credential_origin(input),
                Err(OriginError::PathQueryOrFragment),
                "{input}"
            );
        }
        assert_eq!(
            canonical_credential_origin("https://user@example.test"),
            Err(OriginError::Credentials)
        );
        assert_eq!(canonical_credential_origin(""), Err(OriginError::Empty));
        assert_eq!(
            canonical_credential_origin(&"x".repeat(2049)),
            Err(OriginError::Empty)
        );
    }

    #[test]
    fn portable_validation_checks_token_before_origin_and_identifiers() {
        assert_eq!(
            PortableDeviceCredential::new(
                "not an origin".into(),
                "not a vault".into(),
                "not a device".into(),
                "short".into(),
            )
            .err(),
            Some(PortableCredentialError::InvalidToken)
        );
        assert_eq!(
            PortableDeviceCredential::new(
                "not an origin".into(),
                "not a vault".into(),
                "not a device".into(),
                "a".repeat(96),
            )
            .err(),
            Some(PortableCredentialError::InvalidOrigin(
                OriginError::InvalidUrl
            ))
        );
    }
}
