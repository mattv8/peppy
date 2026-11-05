use base64::{Engine as _, engine::general_purpose::STANDARD};
use peppy_domain::{DeviceId, VaultId};
use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Deserializer, Serialize, de};
use thiserror::Error;

pub const EPHEMERAL_PROTOCOL_VERSION: u16 = 1;
pub const MAX_EPHEMERAL_TTL_MS: u64 = 15_000;
pub const POSTGRES_NOTIFY_PAYLOAD_BYTES: usize = 8_000;
pub const MAX_EPHEMERAL_CIPHERTEXT_BYTES: usize = 5_700;
const AAD_DOMAIN_SEPARATOR: &[u8] = b"peppy-ephemeral-typing-v1\0";

/// Live application frames use the existing signal JSON and AAD unchanged.
pub type LiveFrameV1 = EphemeralSignalV1;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "LiveHelloWire")]
pub struct LiveHelloV1 {
    pub r#type: String,
    pub protocol_version: u16,
    pub max_ttl_ms: u64,
    pub max_ciphertext_bytes: usize,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LiveHelloWire {
    r#type: String,
    protocol_version: u16,
    max_ttl_ms: u64,
    max_ciphertext_bytes: usize,
}

impl TryFrom<LiveHelloWire> for LiveHelloV1 {
    type Error = &'static str;

    fn try_from(wire: LiveHelloWire) -> Result<Self, Self::Error> {
        if wire.r#type != "hello" || wire.protocol_version != EPHEMERAL_PROTOCOL_VERSION {
            return Err("unsupported live hello type or protocol version");
        }
        if !(1..=MAX_EPHEMERAL_TTL_MS).contains(&wire.max_ttl_ms)
            || !(1..=MAX_EPHEMERAL_CIPHERTEXT_BYTES).contains(&wire.max_ciphertext_bytes)
        {
            return Err("invalid live hello limits");
        }
        Ok(Self {
            r#type: wire.r#type,
            protocol_version: wire.protocol_version,
            max_ttl_ms: wire.max_ttl_ms,
            max_ciphertext_bytes: wire.max_ciphertext_bytes,
        })
    }
}

impl LiveHelloV1 {
    pub fn new() -> Self {
        Self {
            r#type: "hello".into(),
            protocol_version: EPHEMERAL_PROTOCOL_VERSION,
            max_ttl_ms: MAX_EPHEMERAL_TTL_MS,
            max_ciphertext_bytes: MAX_EPHEMERAL_CIPHERTEXT_BYTES,
        }
    }
}

impl Default for LiveHelloV1 {
    fn default() -> Self {
        Self::new()
    }
}

impl JsonSchema for LiveHelloV1 {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "LiveHelloV1".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "type": { "const": "hello" },
                "protocol_version": { "const": EPHEMERAL_PROTOCOL_VERSION },
                "max_ttl_ms": { "type": "integer", "minimum": 1, "maximum": MAX_EPHEMERAL_TTL_MS },
                "max_ciphertext_bytes": { "type": "integer", "minimum": 1, "maximum": MAX_EPHEMERAL_CIPHERTEXT_BYTES }
            },
            "required": ["type", "protocol_version", "max_ttl_ms", "max_ciphertext_bytes"]
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct EphemeralSignalV1 {
    pub protocol_version: u16,
    pub kind: String,
    pub sender_device_id: DeviceId,
    pub key_epoch: u32,
    pub sent_at_ms: String,
    pub expires_at_ms: String,
    pub sequence: String,
    pub ciphertext_b64: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EphemeralSignalWire {
    protocol_version: u16,
    kind: String,
    sender_device_id: DeviceId,
    key_epoch: u32,
    sent_at_ms: String,
    expires_at_ms: String,
    sequence: String,
    ciphertext_b64: String,
}

impl<'de> Deserialize<'de> for EphemeralSignalV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = EphemeralSignalWire::deserialize(deserializer)?;
        let signal = Self {
            protocol_version: wire.protocol_version,
            kind: wire.kind,
            sender_device_id: wire.sender_device_id,
            key_epoch: wire.key_epoch,
            sent_at_ms: wire.sent_at_ms,
            expires_at_ms: wire.expires_at_ms,
            sequence: wire.sequence,
            ciphertext_b64: wire.ciphertext_b64,
        };
        signal.validate().map_err(de::Error::custom)?;
        Ok(signal)
    }
}

impl JsonSchema for EphemeralSignalV1 {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "EphemeralSignalV1".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "protocol_version": { "const": EPHEMERAL_PROTOCOL_VERSION },
                "kind": { "const": "typing" },
                "sender_device_id": { "type": "string", "format": "uuid" },
                "key_epoch": { "type": "integer", "format": "uint32", "minimum": 1 },
                "sent_at_ms": { "type": "string", "pattern": "^(0|[1-9][0-9]*)$" },
                "expires_at_ms": { "type": "string", "pattern": "^(0|[1-9][0-9]*)$" },
                "sequence": { "type": "string", "pattern": "^(0|[1-9][0-9]*)$" },
                "ciphertext_b64": { "type": "string", "maxLength": 7600 }
            },
            "required": ["protocol_version", "kind", "sender_device_id", "key_epoch", "sent_at_ms", "expires_at_ms", "sequence", "ciphertext_b64"]
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum EphemeralSignalError {
    #[error("unsupported ephemeral protocol version")]
    UnsupportedVersion,
    #[error("unsupported ephemeral signal kind")]
    UnsupportedKind,
    #[error("key epoch must start at one")]
    InvalidKeyEpoch,
    #[error("timestamp or sequence is not a canonical unsigned decimal string")]
    InvalidDecimal,
    #[error("ephemeral signal TTL must be between one and 15,000 milliseconds")]
    InvalidTtl,
    #[error("ephemeral ciphertext is empty, malformed, or too large")]
    CiphertextTooLarge,
}

impl EphemeralSignalV1 {
    pub fn validate(&self) -> Result<(), EphemeralSignalError> {
        if self.protocol_version != EPHEMERAL_PROTOCOL_VERSION {
            return Err(EphemeralSignalError::UnsupportedVersion);
        }
        if self.kind != "typing" {
            return Err(EphemeralSignalError::UnsupportedKind);
        }
        if self.key_epoch == 0 {
            return Err(EphemeralSignalError::InvalidKeyEpoch);
        }
        let sent_at_ms = parse_decimal(&self.sent_at_ms)?;
        let expires_at_ms = parse_decimal(&self.expires_at_ms)?;
        parse_decimal(&self.sequence)?;
        if !(1..=MAX_EPHEMERAL_TTL_MS).contains(&expires_at_ms.saturating_sub(sent_at_ms))
            || expires_at_ms <= sent_at_ms
        {
            return Err(EphemeralSignalError::InvalidTtl);
        }
        let ciphertext = STANDARD
            .decode(&self.ciphertext_b64)
            .map_err(|_| EphemeralSignalError::CiphertextTooLarge)?;
        if ciphertext.is_empty() || ciphertext.len() > MAX_EPHEMERAL_CIPHERTEXT_BYTES {
            return Err(EphemeralSignalError::CiphertextTooLarge);
        }
        Ok(())
    }

    pub fn aad_bytes(&self, vault_id: VaultId) -> Result<Vec<u8>, EphemeralSignalError> {
        self.validate()?;
        let mut aad = Vec::with_capacity(AAD_DOMAIN_SEPARATOR.len() + 16 + 2 + 1 + 6 + 16 + 4 + 24);
        aad.extend_from_slice(AAD_DOMAIN_SEPARATOR);
        aad.extend_from_slice(vault_id.0.as_bytes());
        aad.extend_from_slice(&self.protocol_version.to_be_bytes());
        aad.push(self.kind.len() as u8);
        aad.extend_from_slice(self.kind.as_bytes());
        aad.extend_from_slice(self.sender_device_id.0.as_bytes());
        aad.extend_from_slice(&self.key_epoch.to_be_bytes());
        aad.extend_from_slice(&parse_decimal(&self.sent_at_ms)?.to_be_bytes());
        aad.extend_from_slice(&parse_decimal(&self.expires_at_ms)?.to_be_bytes());
        aad.extend_from_slice(&parse_decimal(&self.sequence)?.to_be_bytes());
        Ok(aad)
    }
}

fn parse_decimal(value: &str) -> Result<u64, EphemeralSignalError> {
    if value.is_empty()
        || !value.bytes().all(|byte| byte.is_ascii_digit())
        || (value.len() > 1 && value.starts_with('0'))
    {
        return Err(EphemeralSignalError::InvalidDecimal);
    }
    value
        .parse()
        .map_err(|_| EphemeralSignalError::InvalidDecimal)
}
