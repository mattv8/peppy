use base64::{
    Engine as _,
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
};
use peppy_domain::{DeviceId, VaultId};
use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Deserializer, Serialize, de};
use thiserror::Error;

pub const EPHEMERAL_PROTOCOL_VERSION: u16 = 1;
pub const MAX_EPHEMERAL_TTL_MS: u64 = 15_000;
pub const POSTGRES_NOTIFY_PAYLOAD_BYTES: usize = 8_000;
pub const MAX_EPHEMERAL_CIPHERTEXT_BYTES: usize = 5_700;
pub const EPHEMERAL_V2_PROTOCOL_VERSION: u16 = 2;
pub const MAX_EPHEMERAL_V2_TYPING_CIPHERTEXT_BYTES: usize = 5_400;
pub const MAX_EPHEMERAL_V2_CONTROL_CIPHERTEXT_BYTES: usize = 512;
const AAD_DOMAIN_SEPARATOR: &[u8] = b"peppy-ephemeral-typing-v1\0";
const AAD_V2_DOMAIN_SEPARATOR: &[u8] = b"peppy-ephemeral-typing-v2\0";

/// Live application frames use the existing signal JSON and AAD unchanged.
pub type LiveFrameV1 = EphemeralSignalV1;
pub type LiveFrameV2 = EphemeralSignalV2;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EphemeralSignalV2Kind {
    Typing,
    Challenge,
    SessionAck,
}

impl EphemeralSignalV2Kind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Typing => "typing",
            Self::Challenge => "challenge",
            Self::SessionAck => "session_ack",
        }
    }

    fn ciphertext_limit(self) -> usize {
        match self {
            Self::Typing => MAX_EPHEMERAL_V2_TYPING_CIPHERTEXT_BYTES,
            Self::Challenge | Self::SessionAck => MAX_EPHEMERAL_V2_CONTROL_CIPHERTEXT_BYTES,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "LiveHelloV2Wire")]
pub struct LiveHelloV2 {
    pub r#type: String,
    pub protocol_version: u16,
    pub max_ttl_ms: u64,
    pub max_ciphertext_bytes: usize,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LiveHelloV2Wire {
    r#type: String,
    protocol_version: u16,
    max_ttl_ms: u64,
    max_ciphertext_bytes: usize,
}

impl TryFrom<LiveHelloV2Wire> for LiveHelloV2 {
    type Error = &'static str;

    fn try_from(wire: LiveHelloV2Wire) -> Result<Self, Self::Error> {
        if wire.r#type != "hello"
            || wire.protocol_version != EPHEMERAL_V2_PROTOCOL_VERSION
            || wire.max_ttl_ms != MAX_EPHEMERAL_TTL_MS
            || wire.max_ciphertext_bytes != MAX_EPHEMERAL_V2_TYPING_CIPHERTEXT_BYTES
        {
            return Err("unsupported live hello v2");
        }
        Ok(Self {
            r#type: wire.r#type,
            protocol_version: wire.protocol_version,
            max_ttl_ms: wire.max_ttl_ms,
            max_ciphertext_bytes: wire.max_ciphertext_bytes,
        })
    }
}

impl LiveHelloV2 {
    pub fn new() -> Self {
        Self {
            r#type: "hello".into(),
            protocol_version: EPHEMERAL_V2_PROTOCOL_VERSION,
            max_ttl_ms: MAX_EPHEMERAL_TTL_MS,
            max_ciphertext_bytes: MAX_EPHEMERAL_V2_TYPING_CIPHERTEXT_BYTES,
        }
    }
}

impl Default for LiveHelloV2 {
    fn default() -> Self {
        Self::new()
    }
}

impl JsonSchema for LiveHelloV2 {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "LiveHelloV2".into()
    }
    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({"type":"object","additionalProperties":false,"properties":{"type":{"const":"hello"},"protocol_version":{"const":2},"max_ttl_ms":{"const":15000},"max_ciphertext_bytes":{"const":5400}},"required":["type","protocol_version","max_ttl_ms","max_ciphertext_bytes"]})
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct EphemeralSignalV2 {
    pub protocol_version: u16,
    pub kind: EphemeralSignalV2Kind,
    pub sender_device_id: DeviceId,
    pub key_epoch: u32,
    pub session_id: String,
    pub sent_at_ms: String,
    pub expires_at_ms: String,
    pub sequence: String,
    pub ciphertext_b64: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EphemeralSignalV2Wire {
    protocol_version: u16,
    kind: EphemeralSignalV2Kind,
    sender_device_id: String,
    key_epoch: u32,
    session_id: String,
    sent_at_ms: String,
    expires_at_ms: String,
    sequence: String,
    ciphertext_b64: String,
}

impl<'de> Deserialize<'de> for EphemeralSignalV2 {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let wire = EphemeralSignalV2Wire::deserialize(deserializer)?;
        let signal = Self {
            protocol_version: wire.protocol_version,
            kind: wire.kind,
            sender_device_id: parse_device_id(&wire.sender_device_id).map_err(de::Error::custom)?,
            key_epoch: wire.key_epoch,
            session_id: wire.session_id,
            sent_at_ms: wire.sent_at_ms,
            expires_at_ms: wire.expires_at_ms,
            sequence: wire.sequence,
            ciphertext_b64: wire.ciphertext_b64,
        };
        signal.validate().map_err(de::Error::custom)?;
        Ok(signal)
    }
}

impl EphemeralSignalV2 {
    pub fn validate(&self) -> Result<(), EphemeralSignalError> {
        if self.protocol_version != EPHEMERAL_V2_PROTOCOL_VERSION {
            return Err(EphemeralSignalError::UnsupportedVersion);
        }
        if self.key_epoch == 0 {
            return Err(EphemeralSignalError::InvalidKeyEpoch);
        }
        parse_session_id(&self.session_id)?;
        let sent = parse_decimal(&self.sent_at_ms)?;
        let expires = parse_decimal(&self.expires_at_ms)?;
        if parse_decimal(&self.sequence)? == 0 {
            return Err(EphemeralSignalError::InvalidSequence);
        }
        if expires <= sent || !(1..=MAX_EPHEMERAL_TTL_MS).contains(&(expires - sent)) {
            return Err(EphemeralSignalError::InvalidTtl);
        }
        let ciphertext = decode_standard(&self.ciphertext_b64)?;
        if ciphertext.is_empty() || ciphertext.len() > self.kind.ciphertext_limit() {
            return Err(EphemeralSignalError::CiphertextTooLarge);
        }
        Ok(())
    }

    pub fn aad_bytes(&self, vault_id: VaultId) -> Result<Vec<u8>, EphemeralSignalError> {
        self.validate()?;
        let mut aad = Vec::with_capacity(
            AAD_V2_DOMAIN_SEPARATOR.len()
                + 16
                + 2
                + 1
                + self.kind.as_str().len()
                + 16
                + 4
                + 16
                + 24,
        );
        aad.extend_from_slice(AAD_V2_DOMAIN_SEPARATOR);
        aad.extend_from_slice(vault_id.0.as_bytes());
        aad.extend_from_slice(&self.protocol_version.to_be_bytes());
        aad.push(self.kind.as_str().len() as u8);
        aad.extend_from_slice(self.kind.as_str().as_bytes());
        aad.extend_from_slice(self.sender_device_id.0.as_bytes());
        aad.extend_from_slice(&self.key_epoch.to_be_bytes());
        aad.extend_from_slice(&parse_session_id(&self.session_id)?);
        aad.extend_from_slice(&parse_decimal(&self.sent_at_ms)?.to_be_bytes());
        aad.extend_from_slice(&parse_decimal(&self.expires_at_ms)?.to_be_bytes());
        aad.extend_from_slice(&parse_decimal(&self.sequence)?.to_be_bytes());
        Ok(aad)
    }
}

impl JsonSchema for EphemeralSignalV2 {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "EphemeralSignalV2".into()
    }
    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        serde_json::from_value(serde_json::json!({"oneOf":[v2_header_schema("typing",7200,false),v2_header_schema("challenge",684,true),v2_header_schema("session_ack",684,true)]})).expect("valid v2 header schema")
    }
}

const CANONICAL_UUID_PATTERN: &str =
    "^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$";
const CANONICAL_SESSION_ID_PATTERN: &str = "^[A-Za-z0-9_-]{21}[AQgw]$";
const CANONICAL_NONCE_PATTERN: &str = "^[A-Za-z0-9+/]{21}[AQgw]==$";
const CANONICAL_STANDARD_BASE64_PATTERN: &str =
    "^(?:[A-Za-z0-9+/]{4})*(?:[A-Za-z0-9+/][AQgw]==|[A-Za-z0-9+/]{2}[AEIMQUYcgkosw048]=)?$";
const CANONICAL_U64_PATTERN: &str = "^(?:0|[1-9][0-9]{0,18}|1[0-7][0-9]{18}|18[0-3][0-9]{17}|184[0-3][0-9]{16}|1844[0-5][0-9]{15}|18446[0-6][0-9]{14}|184467[0-3][0-9]{13}|1844674[0-3][0-9]{12}|184467440[0-6][0-9]{10}|1844674407[0-2][0-9]{9}|18446744073[0-6][0-9]{8}|1844674407370[0-8][0-9]{6}|18446744073709[0-4][0-9]{5}|184467440737095[0-4][0-9]{4}|1844674407370955[0][0-9]{3}|18446744073709551[0-5][0-9]{2}|184467440737095516[0][0-9]|1844674407370955161[0-4]|18446744073709551615)$";

fn canonical_u64_schema(nonzero: bool) -> serde_json::Value {
    let mut schema = serde_json::json!({"type":"string","pattern":CANONICAL_U64_PATTERN});
    if nonzero {
        schema["not"] = serde_json::json!({"const":"0"});
    }
    schema
}

fn canonical_standard_base64_schema(
    max_length: usize,
    requires_padding_at_max_length: bool,
) -> serde_json::Value {
    let mut schema = serde_json::json!({"type":"string","minLength":4,"maxLength":max_length,"pattern":CANONICAL_STANDARD_BASE64_PATTERN});
    if requires_padding_at_max_length {
        schema["not"] = serde_json::json!({"pattern":format!("^[A-Za-z0-9+/]{{{max_length}}}$")});
    }
    schema
}

fn v2_header_schema(
    kind: &str,
    ciphertext_max_length: usize,
    requires_padding_at_max_length: bool,
) -> serde_json::Value {
    serde_json::json!({"type":"object","additionalProperties":false,"properties":{"protocol_version":{"const":2},"kind":{"const":kind},"sender_device_id":{"type":"string","pattern":CANONICAL_UUID_PATTERN},"key_epoch":{"type":"integer","format":"uint32","minimum":1,"maximum":u32::MAX},"session_id":{"type":"string","pattern":CANONICAL_SESSION_ID_PATTERN},"sent_at_ms":canonical_u64_schema(false),"expires_at_ms":canonical_u64_schema(false),"sequence":canonical_u64_schema(true),"ciphertext_b64":canonical_standard_base64_schema(ciphertext_max_length,requires_padding_at_max_length)},"required":["protocol_version","kind","sender_device_id","key_epoch","session_id","sent_at_ms","expires_at_ms","sequence","ciphertext_b64"]})
}

fn typing_plaintext_schema(origin: &str, includes_gateway: bool) -> serde_json::Value {
    let mut required = vec!["version", "origin", "conversation_id", "state"];
    if includes_gateway {
        required.push("gateway_device_id");
    }
    let mut properties = serde_json::Map::from_iter([
        ("version".into(), serde_json::json!({"const":2})),
        ("origin".into(), serde_json::json!({"const":origin})),
        (
            "conversation_id".into(),
            serde_json::json!({"type":"string","pattern":CANONICAL_UUID_PATTERN}),
        ),
        (
            "state".into(),
            serde_json::json!({"enum":["started","stopped"]}),
        ),
    ]);
    if includes_gateway {
        properties.insert(
            "gateway_device_id".into(),
            serde_json::json!({"type":"string","pattern":CANONICAL_UUID_PATTERN}),
        );
    }
    serde_json::json!({"type":"object","additionalProperties":false,"properties":properties,"required":required})
}

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
                "sent_at_ms": canonical_u64_schema(false),
                "expires_at_ms": canonical_u64_schema(false),
                "sequence": canonical_u64_schema(false),
                "ciphertext_b64": canonical_standard_base64_schema(7600, false)
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
    #[error("session identifier or nonce is not canonically encoded")]
    InvalidCanonicalEncoding,
    #[error("sequence must start at one")]
    InvalidSequence,
    #[error("ephemeral signal TTL must be between one and 15,000 milliseconds")]
    InvalidTtl,
    #[error("ephemeral ciphertext is empty, malformed, or too large")]
    CiphertextTooLarge,
}

fn decode_standard(value: &str) -> Result<Vec<u8>, EphemeralSignalError> {
    let decoded = STANDARD
        .decode(value)
        .map_err(|_| EphemeralSignalError::CiphertextTooLarge)?;
    if STANDARD.encode(&decoded) != value {
        return Err(EphemeralSignalError::CiphertextTooLarge);
    }
    Ok(decoded)
}

fn parse_session_id(value: &str) -> Result<[u8; 16], EphemeralSignalError> {
    if value.len() != 22 {
        return Err(EphemeralSignalError::InvalidCanonicalEncoding);
    }
    let decoded = URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|_| EphemeralSignalError::InvalidCanonicalEncoding)?;
    if decoded.len() != 16 || URL_SAFE_NO_PAD.encode(&decoded) != value {
        return Err(EphemeralSignalError::InvalidCanonicalEncoding);
    }
    decoded
        .try_into()
        .map_err(|_| EphemeralSignalError::InvalidCanonicalEncoding)
}

fn validate_nonce(value: &str) -> Result<(), EphemeralSignalError> {
    let decoded = decode_standard(value)?;
    if decoded.len() != 16 {
        return Err(EphemeralSignalError::InvalidCanonicalEncoding);
    }
    Ok(())
}

fn parse_uuid(value: &str) -> Result<uuid::Uuid, EphemeralSignalError> {
    let parsed: uuid::Uuid = value
        .parse()
        .map_err(|_| EphemeralSignalError::InvalidCanonicalEncoding)?;
    if parsed.to_string() != value {
        return Err(EphemeralSignalError::InvalidCanonicalEncoding);
    }
    Ok(parsed)
}

fn parse_device_id(value: &str) -> Result<DeviceId, EphemeralSignalError> {
    Ok(DeviceId(parse_uuid(value)?))
}

#[derive(Default)]
enum Presence<T> {
    #[default]
    Missing,
    Null,
    Value(T),
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for Presence<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor<T>(std::marker::PhantomData<T>);
        impl<'de, T: Deserialize<'de>> de::Visitor<'de> for Visitor<T> {
            type Value = Presence<T>;
            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("a nullable value")
            }
            fn visit_none<E: de::Error>(self) -> Result<Self::Value, E> {
                Ok(Presence::Null)
            }
            fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
                Ok(Presence::Null)
            }
            fn visit_some<D: Deserializer<'de>>(
                self,
                deserializer: D,
            ) -> Result<Self::Value, D::Error> {
                T::deserialize(deserializer).map(Presence::Value)
            }
        }
        deserializer.deserialize_option(Visitor(std::marker::PhantomData))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TypingOriginV2 {
    VaultDevice,
    GoogleContact,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TypingStateV2 {
    Started,
    Stopped,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct TypingPlaintextV2 {
    pub version: u16,
    pub origin: TypingOriginV2,
    pub conversation_id: uuid::Uuid,
    pub state: TypingStateV2,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gateway_device_id: Option<DeviceId>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TypingPlaintextV2Wire {
    version: u16,
    origin: TypingOriginV2,
    conversation_id: String,
    state: TypingStateV2,
    #[serde(default)]
    gateway_device_id: Presence<String>,
}

impl<'de> Deserialize<'de> for TypingPlaintextV2 {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let wire = TypingPlaintextV2Wire::deserialize(deserializer)?;
        let gateway_device_id = match (wire.origin, wire.gateway_device_id) {
            (TypingOriginV2::VaultDevice, Presence::Value(value)) => {
                Some(parse_device_id(&value).map_err(de::Error::custom)?)
            }
            (TypingOriginV2::GoogleContact, Presence::Missing) => None,
            _ => return Err(de::Error::custom("invalid v2 typing gateway_device_id")),
        };
        let value = Self {
            version: wire.version,
            origin: wire.origin,
            conversation_id: parse_uuid(&wire.conversation_id).map_err(de::Error::custom)?,
            state: wire.state,
            gateway_device_id,
        };
        value.validate().map_err(de::Error::custom)?;
        Ok(value)
    }
}

impl TypingPlaintextV2 {
    pub fn validate(&self) -> Result<(), EphemeralSignalError> {
        if self.version != EPHEMERAL_V2_PROTOCOL_VERSION
            || matches!(self.origin, TypingOriginV2::VaultDevice)
                != self.gateway_device_id.is_some()
        {
            return Err(EphemeralSignalError::InvalidCanonicalEncoding);
        }
        Ok(())
    }
}

impl JsonSchema for TypingPlaintextV2 {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "TypingPlaintextV2".into()
    }
    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        serde_json::from_value(serde_json::json!({"oneOf":[typing_plaintext_schema("vault_device",true),typing_plaintext_schema("google_contact",false)]})).expect("valid v2 typing schema")
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ChallengePlaintextV2 {
    pub version: u16,
    pub challenger_device_id: DeviceId,
    pub challenger_session_id: String,
    pub responder_device_id: DeviceId,
    pub nonce_b64: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ChallengePlaintextV2Wire {
    version: u16,
    challenger_device_id: String,
    challenger_session_id: String,
    responder_device_id: String,
    nonce_b64: String,
}

impl<'de> Deserialize<'de> for ChallengePlaintextV2 {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let wire = ChallengePlaintextV2Wire::deserialize(deserializer)?;
        let value = Self {
            version: wire.version,
            challenger_device_id: parse_device_id(&wire.challenger_device_id)
                .map_err(de::Error::custom)?,
            challenger_session_id: wire.challenger_session_id,
            responder_device_id: parse_device_id(&wire.responder_device_id)
                .map_err(de::Error::custom)?,
            nonce_b64: wire.nonce_b64,
        };
        value.validate().map_err(de::Error::custom)?;
        Ok(value)
    }
}

impl ChallengePlaintextV2 {
    pub fn validate(&self) -> Result<(), EphemeralSignalError> {
        if self.version != EPHEMERAL_V2_PROTOCOL_VERSION {
            return Err(EphemeralSignalError::UnsupportedVersion);
        }
        parse_session_id(&self.challenger_session_id)?;
        validate_nonce(&self.nonce_b64)
    }
}

impl JsonSchema for ChallengePlaintextV2 {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "ChallengePlaintextV2".into()
    }
    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({"type":"object","additionalProperties":false,"properties":{"version":{"const":2},"challenger_device_id":{"type":"string","pattern":CANONICAL_UUID_PATTERN},"challenger_session_id":{"type":"string","pattern":CANONICAL_SESSION_ID_PATTERN},"responder_device_id":{"type":"string","pattern":CANONICAL_UUID_PATTERN},"nonce_b64":{"type":"string","pattern":CANONICAL_NONCE_PATTERN}},"required":["version","challenger_device_id","challenger_session_id","responder_device_id","nonce_b64"]})
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SessionAckPlaintextV2 {
    pub version: u16,
    pub responder_device_id: DeviceId,
    pub responder_session_id: String,
    pub challenger_device_id: DeviceId,
    pub challenger_session_id: String,
    pub echoed_nonce_b64: String,
    pub sequence_floor: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SessionAckPlaintextV2Wire {
    version: u16,
    responder_device_id: String,
    responder_session_id: String,
    challenger_device_id: String,
    challenger_session_id: String,
    echoed_nonce_b64: String,
    sequence_floor: String,
}

impl<'de> Deserialize<'de> for SessionAckPlaintextV2 {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let wire = SessionAckPlaintextV2Wire::deserialize(deserializer)?;
        let value = Self {
            version: wire.version,
            responder_device_id: parse_device_id(&wire.responder_device_id)
                .map_err(de::Error::custom)?,
            responder_session_id: wire.responder_session_id,
            challenger_device_id: parse_device_id(&wire.challenger_device_id)
                .map_err(de::Error::custom)?,
            challenger_session_id: wire.challenger_session_id,
            echoed_nonce_b64: wire.echoed_nonce_b64,
            sequence_floor: wire.sequence_floor,
        };
        value.validate().map_err(de::Error::custom)?;
        Ok(value)
    }
}

impl SessionAckPlaintextV2 {
    pub fn validate(&self) -> Result<(), EphemeralSignalError> {
        if self.version != EPHEMERAL_V2_PROTOCOL_VERSION {
            return Err(EphemeralSignalError::UnsupportedVersion);
        }
        parse_session_id(&self.responder_session_id)?;
        parse_session_id(&self.challenger_session_id)?;
        validate_nonce(&self.echoed_nonce_b64)?;
        if parse_decimal(&self.sequence_floor)? == 0 {
            return Err(EphemeralSignalError::InvalidSequence);
        }
        Ok(())
    }
}

impl JsonSchema for SessionAckPlaintextV2 {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "SessionAckPlaintextV2".into()
    }
    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({"type":"object","additionalProperties":false,"properties":{"version":{"const":2},"responder_device_id":{"type":"string","pattern":CANONICAL_UUID_PATTERN},"responder_session_id":{"type":"string","pattern":CANONICAL_SESSION_ID_PATTERN},"challenger_device_id":{"type":"string","pattern":CANONICAL_UUID_PATTERN},"challenger_session_id":{"type":"string","pattern":CANONICAL_SESSION_ID_PATTERN},"echoed_nonce_b64":{"type":"string","pattern":CANONICAL_NONCE_PATTERN},"sequence_floor":canonical_u64_schema(true)},"required":["version","responder_device_id","responder_session_id","challenger_device_id","challenger_session_id","echoed_nonce_b64","sequence_floor"]})
    }
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
        let ciphertext = decode_standard(&self.ciphertext_b64)?;
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
