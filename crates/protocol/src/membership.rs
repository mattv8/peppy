use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use peppy_domain::DeviceId;
use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Deserializer, Serialize, de};
use std::collections::BTreeSet;
use thiserror::Error;

pub const MEMBERSHIP_PROTOCOL_VERSION: u8 = 1;
pub const MAX_MEMBERSHIP_CONTROL_BYTES: usize = 32 * 1024;
pub const MAX_MEMBERSHIP_ROSTER_ENTRIES: usize = 100;
const MAX_MEMBERSHIP_VERSION: u64 = i64::MAX as u64;
const MEMBERSHIP_ID_BYTES: usize = 16;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct MembershipVersionV1(String);

impl MembershipVersionV1 {
    pub fn parse(value: &str) -> Result<Self, MembershipControlError> {
        if value.is_empty()
            || !value.bytes().all(|byte| byte.is_ascii_digit())
            || (value.len() > 1 && value.starts_with('0'))
            || value
                .parse::<u64>()
                .ok()
                .filter(|value| (1..=MAX_MEMBERSHIP_VERSION).contains(value))
                .is_none()
        {
            return Err(MembershipControlError::InvalidMembershipVersion);
        }
        Ok(Self(value.into()))
    }
}

impl<'de> Deserialize<'de> for MembershipVersionV1 {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::parse(&String::deserialize(deserializer)?).map_err(de::Error::custom)
    }
}

impl JsonSchema for MembershipVersionV1 {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "MembershipVersionV1".into()
    }
    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        schema_from_value(membership_version_schema())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MembershipRoleV1 {
    Owner,
    Gateway,
    Device,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct MembershipDeviceV1 {
    pub device_id: DeviceId,
    pub revoked: bool,
    pub key_epoch: u32,
    pub role: MembershipRoleV1,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MembershipDeviceWire {
    device_id: String,
    revoked: bool,
    key_epoch: u32,
    role: MembershipRoleV1,
}

impl<'de> Deserialize<'de> for MembershipDeviceV1 {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let wire = MembershipDeviceWire::deserialize(deserializer)?;
        if wire.key_epoch == 0 {
            return Err(de::Error::custom(MembershipControlError::InvalidKeyEpoch));
        }
        let device_id = parse_device_id(&wire.device_id).map_err(de::Error::custom)?;
        Ok(Self {
            device_id,
            revoked: wire.revoked,
            key_epoch: wire.key_epoch,
            role: wire.role,
        })
    }
}

impl JsonSchema for MembershipDeviceV1 {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "MembershipDeviceV1".into()
    }
    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        schema_from_value(membership_device_schema())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct MembershipBootstrapV1 {
    #[serde(rename = "type")]
    pub r#type: String,
    pub version: u8,
    pub connection_id: String,
    pub membership_version: MembershipVersionV1,
    pub roster: Vec<MembershipDeviceV1>,
}

impl JsonSchema for MembershipBootstrapV1 {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "MembershipBootstrapV1".into()
    }
    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        membership_schema("membership_bootstrap", false, true, true)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct MembershipChallengeV1 {
    #[serde(rename = "type")]
    pub r#type: String,
    pub version: u8,
    pub connection_id: String,
    pub challenge_id: String,
}

impl JsonSchema for MembershipChallengeV1 {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "MembershipChallengeV1".into()
    }
    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        membership_schema("membership_challenge", true, false, false)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct MembershipResponseV1 {
    #[serde(rename = "type")]
    pub r#type: String,
    pub version: u8,
    pub connection_id: String,
    pub challenge_id: String,
    pub membership_version: MembershipVersionV1,
    pub roster: Vec<MembershipDeviceV1>,
}

impl JsonSchema for MembershipResponseV1 {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "MembershipResponseV1".into()
    }
    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        membership_schema("membership_response", true, true, true)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct MembershipHintV1 {
    #[serde(rename = "type")]
    pub r#type: String,
    pub version: u8,
    pub membership_version: MembershipVersionV1,
}

impl JsonSchema for MembershipHintV1 {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "MembershipHintV1".into()
    }
    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        membership_schema("membership_hint", false, true, false)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MembershipBootstrapWire {
    #[serde(rename = "type")]
    kind: String,
    version: u8,
    connection_id: String,
    membership_version: MembershipVersionV1,
    roster: Vec<MembershipDeviceV1>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MembershipChallengeWire {
    #[serde(rename = "type")]
    kind: String,
    version: u8,
    connection_id: String,
    challenge_id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MembershipResponseWire {
    #[serde(rename = "type")]
    kind: String,
    version: u8,
    connection_id: String,
    challenge_id: String,
    membership_version: MembershipVersionV1,
    roster: Vec<MembershipDeviceV1>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MembershipHintWire {
    #[serde(rename = "type")]
    kind: String,
    version: u8,
    membership_version: MembershipVersionV1,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub enum MembershipControlV1 {
    Bootstrap(MembershipBootstrapV1),
    Challenge(MembershipChallengeV1),
    Response(MembershipResponseV1),
    Hint(MembershipHintV1),
}

impl JsonSchema for MembershipControlV1 {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "MembershipControlV1".into()
    }
    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({"oneOf":[membership_schema_value("membership_bootstrap", false, true, true),membership_schema_value("membership_challenge", true, false, false),membership_schema_value("membership_response", true, true, true),membership_schema_value("membership_hint", false, true, false)]})
    }
}

impl<'de> Deserialize<'de> for MembershipControlV1 {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = StrictJson::deserialize(deserializer)?.0;
        let kind = value
            .get("type")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| de::Error::custom("missing membership control type"))?;
        let control = match kind {
            "membership_bootstrap" => {
                let wire: MembershipBootstrapWire =
                    serde_json::from_value(value).map_err(de::Error::custom)?;
                validate_bootstrap(&wire).map_err(de::Error::custom)?;
                Self::Bootstrap(MembershipBootstrapV1 {
                    r#type: wire.kind,
                    version: wire.version,
                    connection_id: wire.connection_id,
                    membership_version: wire.membership_version,
                    roster: wire.roster,
                })
            }
            "membership_challenge" => {
                let wire: MembershipChallengeWire =
                    serde_json::from_value(value).map_err(de::Error::custom)?;
                validate_challenge(&wire).map_err(de::Error::custom)?;
                Self::Challenge(MembershipChallengeV1 {
                    r#type: wire.kind,
                    version: wire.version,
                    connection_id: wire.connection_id,
                    challenge_id: wire.challenge_id,
                })
            }
            "membership_response" => {
                let wire: MembershipResponseWire =
                    serde_json::from_value(value).map_err(de::Error::custom)?;
                validate_response(&wire).map_err(de::Error::custom)?;
                Self::Response(MembershipResponseV1 {
                    r#type: wire.kind,
                    version: wire.version,
                    connection_id: wire.connection_id,
                    challenge_id: wire.challenge_id,
                    membership_version: wire.membership_version,
                    roster: wire.roster,
                })
            }
            "membership_hint" => {
                let wire: MembershipHintWire =
                    serde_json::from_value(value).map_err(de::Error::custom)?;
                if wire.version != MEMBERSHIP_PROTOCOL_VERSION {
                    return Err(de::Error::custom(MembershipControlError::InvalidVersion));
                }
                Self::Hint(MembershipHintV1 {
                    r#type: wire.kind,
                    version: wire.version,
                    membership_version: wire.membership_version,
                })
            }
            _ => {
                return Err(de::Error::custom(
                    MembershipControlError::InvalidControlType,
                ));
            }
        };
        Ok(control)
    }
}

struct StrictJson(serde_json::Value);

impl<'de> Deserialize<'de> for StrictJson {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(StrictJsonVisitor)
    }
}

struct StrictJsonVisitor;

impl<'de> de::Visitor<'de> for StrictJsonVisitor {
    type Value = StrictJson;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("JSON without duplicate object fields")
    }

    fn visit_bool<E: de::Error>(self, value: bool) -> Result<Self::Value, E> {
        Ok(StrictJson(value.into()))
    }
    fn visit_i64<E: de::Error>(self, value: i64) -> Result<Self::Value, E> {
        Ok(StrictJson(value.into()))
    }
    fn visit_u64<E: de::Error>(self, value: u64) -> Result<Self::Value, E> {
        Ok(StrictJson(value.into()))
    }
    fn visit_f64<E: de::Error>(self, value: f64) -> Result<Self::Value, E> {
        serde_json::Number::from_f64(value)
            .map(|number| StrictJson(number.into()))
            .ok_or_else(|| E::custom("invalid JSON number"))
    }
    fn visit_str<E: de::Error>(self, value: &str) -> Result<Self::Value, E> {
        Ok(StrictJson(value.into()))
    }
    fn visit_string<E: de::Error>(self, value: String) -> Result<Self::Value, E> {
        Ok(StrictJson(value.into()))
    }
    fn visit_none<E: de::Error>(self) -> Result<Self::Value, E> {
        Ok(StrictJson(serde_json::Value::Null))
    }
    fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
        Ok(StrictJson(serde_json::Value::Null))
    }
    fn visit_seq<A: de::SeqAccess<'de>>(self, mut sequence: A) -> Result<Self::Value, A::Error> {
        let mut values = Vec::new();
        while let Some(value) = sequence.next_element::<StrictJson>()? {
            values.push(value.0);
        }
        Ok(StrictJson(values.into()))
    }
    fn visit_map<A: de::MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        let mut fields = serde_json::Map::new();
        while let Some(field) = map.next_key::<String>()? {
            if fields.contains_key(&field) {
                return Err(de::Error::custom(format!("duplicate field {field}")));
            }
            fields.insert(field, map.next_value::<StrictJson>()?.0);
        }
        Ok(StrictJson(fields.into()))
    }
}

impl MembershipControlV1 {
    pub fn parse(raw: &[u8]) -> Result<Self, MembershipControlError> {
        if raw.len() > MAX_MEMBERSHIP_CONTROL_BYTES {
            return Err(MembershipControlError::TooLarge);
        }
        serde_json::from_slice(raw).map_err(|_| MembershipControlError::InvalidJson)
    }
}

fn validate_bootstrap(wire: &MembershipBootstrapWire) -> Result<(), MembershipControlError> {
    if wire.version != MEMBERSHIP_PROTOCOL_VERSION {
        return Err(MembershipControlError::InvalidVersion);
    }
    validate_id(&wire.connection_id)?;
    validate_roster(&wire.roster)
}

fn membership_schema(
    kind: &str,
    challenge: bool,
    membership_version: bool,
    roster: bool,
) -> Schema {
    schema_from_value(membership_schema_value(
        kind,
        challenge,
        membership_version,
        roster,
    ))
}

fn schema_from_value(value: serde_json::Value) -> Schema {
    serde_json::from_value(value).expect("valid membership schema")
}

fn membership_version_schema() -> serde_json::Value {
    let maximum = MAX_MEMBERSHIP_VERSION.to_string();
    serde_json::json!({
        "type":"string",
        "pattern":canonical_decimal_pattern(&maximum),
        "maxLength":maximum.len(),
        "x-peppy-canonical-decimal-maximum":maximum,
    })
}

fn membership_id_schema() -> serde_json::Value {
    serde_json::json!({
        "type":"string",
        "pattern":"^[A-Za-z0-9_-]{21}[AQgw]$",
        "x-peppy-canonical-base64url-bytes":MEMBERSHIP_ID_BYTES,
    })
}

fn membership_device_schema() -> serde_json::Value {
    serde_json::json!({"type":"object","additionalProperties":false,"properties":{"device_id":{"type":"string","pattern":"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$"},"revoked":{"type":"boolean"},"key_epoch":{"type":"integer","format":"uint32","minimum":1,"maximum":u32::MAX},"role":{"enum":["owner","gateway","device"]}},"required":["device_id","revoked","key_epoch","role"]})
}

fn canonical_decimal_pattern(maximum: &str) -> String {
    let mut alternatives = vec![format!("[1-9][0-9]{{0,{}}}", maximum.len() - 2)];
    for (index, digit) in maximum.bytes().enumerate() {
        let minimum = if index == 0 { b'1' } else { b'0' };
        if digit > minimum {
            alternatives.push(format!(
                "{}[{}-{}][0-9]{{{}}}",
                &maximum[..index],
                minimum as char,
                (digit - 1) as char,
                maximum.len() - index - 1
            ));
        }
    }
    alternatives.push(maximum.into());
    format!("^(?:{})$", alternatives.join("|"))
}

fn membership_schema_value(
    kind: &str,
    challenge: bool,
    membership_version: bool,
    roster: bool,
) -> serde_json::Value {
    let mut properties = serde_json::Map::from_iter([
        ("type".into(), serde_json::json!({"const":kind})),
        (
            "version".into(),
            serde_json::json!({"const":MEMBERSHIP_PROTOCOL_VERSION}),
        ),
    ]);
    let mut required = vec!["type", "version"];
    if challenge || roster {
        properties.insert("connection_id".into(), membership_id_schema());
        required.push("connection_id");
    }
    if challenge {
        properties.insert("challenge_id".into(), membership_id_schema());
        required.push("challenge_id");
    }
    if membership_version {
        properties.insert("membership_version".into(), membership_version_schema());
        required.push("membership_version");
    }
    if roster {
        properties.insert("roster".into(), serde_json::json!({"type":"array","maxItems":MAX_MEMBERSHIP_ROSTER_ENTRIES,"items":membership_device_schema()}));
        required.push("roster");
    }
    serde_json::json!({"type":"object","additionalProperties":false,"properties":properties,"required":required})
}

fn validate_challenge(wire: &MembershipChallengeWire) -> Result<(), MembershipControlError> {
    if wire.version != MEMBERSHIP_PROTOCOL_VERSION {
        return Err(MembershipControlError::InvalidVersion);
    }
    validate_id(&wire.connection_id)?;
    validate_id(&wire.challenge_id)
}

fn validate_response(wire: &MembershipResponseWire) -> Result<(), MembershipControlError> {
    validate_challenge(&MembershipChallengeWire {
        kind: wire.kind.clone(),
        version: wire.version,
        connection_id: wire.connection_id.clone(),
        challenge_id: wire.challenge_id.clone(),
    })?;
    validate_roster(&wire.roster)
}

fn validate_id(value: &str) -> Result<(), MembershipControlError> {
    let Ok(decoded) = URL_SAFE_NO_PAD.decode(value) else {
        return Err(MembershipControlError::InvalidId);
    };
    if decoded.len() != MEMBERSHIP_ID_BYTES || URL_SAFE_NO_PAD.encode(decoded) != value {
        return Err(MembershipControlError::InvalidId);
    }
    Ok(())
}

fn validate_roster(roster: &[MembershipDeviceV1]) -> Result<(), MembershipControlError> {
    if roster.len() > MAX_MEMBERSHIP_ROSTER_ENTRIES {
        return Err(MembershipControlError::RosterTooLarge);
    }
    let mut device_ids = BTreeSet::new();
    if roster
        .iter()
        .any(|device| !device_ids.insert(device.device_id.to_string()))
    {
        return Err(MembershipControlError::DuplicateDevice);
    }
    Ok(())
}

fn parse_device_id(value: &str) -> Result<DeviceId, MembershipControlError> {
    let device_id = value
        .parse::<DeviceId>()
        .map_err(|_| MembershipControlError::InvalidDeviceId)?;
    if device_id.to_string() != value {
        return Err(MembershipControlError::InvalidDeviceId);
    }
    Ok(device_id)
}

#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum MembershipControlError {
    #[error("membership control exceeds its size limit")]
    TooLarge,
    #[error("invalid membership control JSON")]
    InvalidJson,
    #[error("invalid membership control type")]
    InvalidControlType,
    #[error("invalid membership control version")]
    InvalidVersion,
    #[error("invalid membership version")]
    InvalidMembershipVersion,
    #[error("invalid connection or challenge ID")]
    InvalidId,
    #[error("invalid membership device ID")]
    InvalidDeviceId,
    #[error("invalid membership key epoch")]
    InvalidKeyEpoch,
    #[error("membership roster exceeds its size limit")]
    RosterTooLarge,
    #[error("membership roster has duplicate device IDs")]
    DuplicateDevice,
}
