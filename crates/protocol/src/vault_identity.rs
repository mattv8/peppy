use crate::ephemeral::canonical_standard_base64_schema;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use schemars::{JsonSchema, Schema, SchemaGenerator};
use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use uuid::Uuid;

pub const VAULT_IDENTITY_PROTOCOL_VERSION: u8 = 1;
pub const VAULT_IDENTITY_RECOVERY_RECORD_BYTES: usize = 266;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VaultIdentityRecoveryRecordV1(Vec<u8>);

impl VaultIdentityRecoveryRecordV1 {
    pub fn from_bytes(bytes: Vec<u8>) -> Result<Self, VaultIdentityError> {
        if bytes.len() != VAULT_IDENTITY_RECOVERY_RECORD_BYTES {
            return Err(VaultIdentityError::InvalidRecoveryRecord);
        }
        Ok(Self(bytes))
    }
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

impl<'de> Deserialize<'de> for VaultIdentityRecoveryRecordV1 {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let encoded = String::deserialize(deserializer)?;
        if encoded.len() != 356 || !encoded.ends_with('=') {
            return Err(de::Error::custom(VaultIdentityError::InvalidRecoveryRecord));
        }
        let bytes = STANDARD
            .decode(&encoded)
            .map_err(|_| de::Error::custom(VaultIdentityError::InvalidRecoveryRecord))?;
        if STANDARD.encode(&bytes) != encoded {
            return Err(de::Error::custom(VaultIdentityError::InvalidRecoveryRecord));
        }
        Self::from_bytes(bytes).map_err(de::Error::custom)
    }
}

impl Serialize for VaultIdentityRecoveryRecordV1 {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&STANDARD.encode(&self.0))
    }
}

impl JsonSchema for VaultIdentityRecoveryRecordV1 {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "VaultIdentityRecoveryRecordV1".into()
    }
    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        let mut recovery_record = canonical_standard_base64_schema(356, true);
        recovery_record["contentEncoding"] = serde_json::json!("base64");
        recovery_record["minLength"] = serde_json::json!(356);
        recovery_record["x-peppy-canonical-base64-bytes"] = serde_json::json!(266);
        schema(recovery_record)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct EstablishVaultIdentityV1 {
    pub protocol_version: u8,
    pub operation_id: Uuid,
    pub key_epoch: u32,
    pub profile_fingerprint: String,
    pub recovery_record: VaultIdentityRecoveryRecordV1,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EstablishVaultIdentityWire {
    protocol_version: u8,
    operation_id: Uuid,
    key_epoch: u32,
    profile_fingerprint: String,
    recovery_record: VaultIdentityRecoveryRecordV1,
}

impl<'de> Deserialize<'de> for EstablishVaultIdentityV1 {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let wire = EstablishVaultIdentityWire::deserialize(deserializer)?;
        let request = Self {
            protocol_version: wire.protocol_version,
            operation_id: wire.operation_id,
            key_epoch: wire.key_epoch,
            profile_fingerprint: wire.profile_fingerprint,
            recovery_record: wire.recovery_record,
        };
        request.validate().map_err(de::Error::custom)?;
        Ok(request)
    }
}

impl JsonSchema for EstablishVaultIdentityV1 {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "EstablishVaultIdentityV1".into()
    }

    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        let recovery_record = generator.subschema_for::<VaultIdentityRecoveryRecordV1>();
        schema(serde_json::json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "protocol_version": {"const": 1},
                "operation_id": {
                    "type": "string",
                    "format": "uuid",
                    "not": {"const": "00000000-0000-0000-0000-000000000000"}
                },
                "key_epoch": {
                    "type": "integer",
                    "format": "uint32",
                    "minimum": 1,
                    "maximum": 4294967295_u64
                },
                "profile_fingerprint": {
                    "type": "string",
                    "minLength": 64,
                    "maxLength": 64,
                    "pattern": "^[0-9a-f]{64}$"
                },
                "recovery_record": recovery_record
            },
            "required": [
                "protocol_version",
                "operation_id",
                "key_epoch",
                "profile_fingerprint",
                "recovery_record"
            ]
        }))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum VaultIdentityStatusV1 {
    Absent {
        protocol_version: u8,
    },
    Established {
        protocol_version: u8,
        operation_id: Uuid,
        key_epoch: u32,
        profile_fingerprint: String,
        recovery_record: VaultIdentityRecoveryRecordV1,
    },
}

#[derive(Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
enum VaultIdentityStatusWire {
    Absent {
        protocol_version: u8,
    },
    Established {
        protocol_version: u8,
        operation_id: Uuid,
        key_epoch: u32,
        profile_fingerprint: String,
        recovery_record: VaultIdentityRecoveryRecordV1,
    },
}

impl<'de> Deserialize<'de> for VaultIdentityStatusV1 {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let status = match VaultIdentityStatusWire::deserialize(deserializer)? {
            VaultIdentityStatusWire::Absent { protocol_version } => {
                Self::Absent { protocol_version }
            }
            VaultIdentityStatusWire::Established {
                protocol_version,
                operation_id,
                key_epoch,
                profile_fingerprint,
                recovery_record,
            } => Self::Established {
                protocol_version,
                operation_id,
                key_epoch,
                profile_fingerprint,
                recovery_record,
            },
        };
        status.validate().map_err(de::Error::custom)?;
        Ok(status)
    }
}

impl VaultIdentityStatusV1 {
    fn validate(&self) -> Result<(), VaultIdentityError> {
        match self {
            Self::Absent { protocol_version } => {
                if *protocol_version != VAULT_IDENTITY_PROTOCOL_VERSION {
                    return Err(VaultIdentityError::InvalidRecoveryRecord);
                }
            }
            Self::Established {
                protocol_version,
                operation_id,
                key_epoch,
                profile_fingerprint,
                ..
            } => {
                if *protocol_version != VAULT_IDENTITY_PROTOCOL_VERSION
                    || operation_id.is_nil()
                    || *key_epoch == 0
                    || !is_fingerprint(profile_fingerprint)
                {
                    return Err(VaultIdentityError::InvalidRecoveryRecord);
                }
            }
        }
        Ok(())
    }
}

impl JsonSchema for VaultIdentityStatusV1 {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "VaultIdentityStatusV1".into()
    }
    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        let recovery_record = generator.subschema_for::<VaultIdentityRecoveryRecordV1>();
        schema(
            serde_json::json!({"oneOf":[{"type":"object","additionalProperties":false,"properties":{"state":{"const":"absent"},"protocol_version":{"const":1}},"required":["state","protocol_version"]},{"type":"object","additionalProperties":false,"properties":{"state":{"const":"established"},"protocol_version":{"const":1},"operation_id":{"type":"string","format":"uuid","not":{"const":"00000000-0000-0000-0000-000000000000"}},"key_epoch":{"type":"integer","format":"uint32","minimum":1,"maximum":4294967295_u64},"profile_fingerprint":{"type":"string","minLength":64,"maxLength":64,"pattern":"^[0-9a-f]{64}$"},"recovery_record":recovery_record},"required":["state","protocol_version","operation_id","key_epoch","profile_fingerprint","recovery_record"]}]}),
        )
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum VaultIdentityError {
    #[error("invalid vault identity request")]
    InvalidRecoveryRecord,
}

impl EstablishVaultIdentityV1 {
    pub fn validate(&self) -> Result<(), VaultIdentityError> {
        if self.protocol_version != VAULT_IDENTITY_PROTOCOL_VERSION
            || self.operation_id.is_nil()
            || self.key_epoch == 0
            || !is_fingerprint(&self.profile_fingerprint)
        {
            return Err(VaultIdentityError::InvalidRecoveryRecord);
        }
        Ok(())
    }
}

fn is_fingerprint(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}
fn schema(value: serde_json::Value) -> Schema {
    serde_json::from_value(value).expect("valid vault identity schema")
}

#[cfg(test)]
mod tests {
    use super::*;
    fn request() -> EstablishVaultIdentityV1 {
        EstablishVaultIdentityV1 {
            protocol_version: 1,
            operation_id: Uuid::from_u128(1),
            key_epoch: 1,
            profile_fingerprint: "a".repeat(64),
            recovery_record: VaultIdentityRecoveryRecordV1::from_bytes(vec![7; 266]).unwrap(),
        }
    }
    #[test]
    fn recovery_record_requires_canonical_fixed_size_base64() {
        let encoded = serde_json::to_string(&request()).unwrap();
        assert_eq!(
            serde_json::from_str::<EstablishVaultIdentityV1>(&encoded).unwrap(),
            request()
        );
        assert!(
            serde_json::from_str::<EstablishVaultIdentityV1>(&encoded.replacen('=', "A", 1))
                .is_err()
        );
    }
    #[test]
    fn recovery_record_schema_matches_runtime_canonical_pad_bits() {
        let openapi: serde_json::Value =
            serde_json::from_str(&crate::generate_contracts().openapi).unwrap();
        let schema = &openapi["components"]["schemas"]["VaultIdentityRecoveryRecordV1"];
        let validator = jsonschema::validator_for(schema).unwrap();
        let canonical = STANDARD.encode(vec![7_u8; VAULT_IDENTITY_RECOVERY_RECORD_BYTES]);
        assert!(canonical.ends_with("Bwc="));
        assert!(validator.is_valid(&serde_json::json!(canonical)));
        assert!(
            serde_json::from_value::<VaultIdentityRecoveryRecordV1>(serde_json::json!(canonical))
                .is_ok()
        );

        let mut noncanonical = canonical.into_bytes();
        let final_sextet = noncanonical.len() - 2;
        noncanonical[final_sextet] = b'd';
        let noncanonical = String::from_utf8(noncanonical).unwrap();
        assert!(
            !validator.is_valid(&serde_json::json!(noncanonical)),
            "schema accepted nonzero base64 pad bits"
        );
        assert!(
            serde_json::from_value::<VaultIdentityRecoveryRecordV1>(serde_json::json!(
                noncanonical
            ))
            .is_err()
        );
    }

    #[test]
    fn status_round_trips_absent_and_established() {
        let statuses = [
            VaultIdentityStatusV1::Absent {
                protocol_version: VAULT_IDENTITY_PROTOCOL_VERSION,
            },
            VaultIdentityStatusV1::Established {
                protocol_version: VAULT_IDENTITY_PROTOCOL_VERSION,
                operation_id: Uuid::from_u128(1),
                key_epoch: 1,
                profile_fingerprint: "a".repeat(64),
                recovery_record: VaultIdentityRecoveryRecordV1::from_bytes(vec![7; 266]).unwrap(),
            },
        ];
        for status in statuses {
            let encoded = serde_json::to_vec(&status).unwrap();
            let decoded: VaultIdentityStatusV1 = serde_json::from_slice(&encoded).unwrap();
            assert_eq!(decoded, status);
        }
    }

    #[test]
    fn status_deserialization_rejects_unknown_fields_and_invalid_context() {
        let absent = serde_json::json!({"state":"absent","protocol_version":1});
        let mut unknown = absent.clone();
        unknown["unexpected"] = true.into();
        assert!(serde_json::from_value::<VaultIdentityStatusV1>(unknown).is_err());
        let mut wrong_version = absent;
        wrong_version["protocol_version"] = 2.into();
        assert!(serde_json::from_value::<VaultIdentityStatusV1>(wrong_version).is_err());

        let established = serde_json::to_value(VaultIdentityStatusV1::Established {
            protocol_version: 1,
            operation_id: Uuid::from_u128(1),
            key_epoch: 1,
            profile_fingerprint: "a".repeat(64),
            recovery_record: VaultIdentityRecoveryRecordV1::from_bytes(vec![7; 266]).unwrap(),
        })
        .unwrap();
        for (field, value) in [
            ("protocol_version", serde_json::json!(2)),
            ("operation_id", serde_json::json!(Uuid::nil())),
            ("key_epoch", serde_json::json!(0)),
            ("profile_fingerprint", serde_json::json!("a".repeat(63))),
            ("recovery_record", serde_json::json!("AAAA")),
        ] {
            let mut invalid = established.clone();
            invalid[field] = value;
            assert!(serde_json::from_value::<VaultIdentityStatusV1>(invalid).is_err());
        }
    }

    #[test]
    fn establish_schema_matches_runtime_boundaries() {
        let openapi: serde_json::Value =
            serde_json::from_str(&crate::generate_contracts().openapi).unwrap();
        let schema = serde_json::json!({
            "$ref": "#/components/schemas/EstablishVaultIdentityV1",
            "components": openapi["components"].clone(),
        });
        let validator = jsonschema::validator_for(&schema).unwrap();
        let valid = serde_json::to_value(request()).unwrap();
        assert!(validator.is_valid(&valid));

        let mut max_epoch = valid.clone();
        max_epoch["key_epoch"] = serde_json::json!(u32::MAX);
        assert!(validator.is_valid(&max_epoch));
        assert!(serde_json::from_value::<EstablishVaultIdentityV1>(max_epoch).is_ok());

        let invalid = [
            ("protocol version", "protocol_version", serde_json::json!(2)),
            (
                "nil operation",
                "operation_id",
                serde_json::json!(Uuid::nil()),
            ),
            ("zero epoch", "key_epoch", serde_json::json!(0)),
            (
                "oversized epoch",
                "key_epoch",
                serde_json::json!(u64::from(u32::MAX) + 1),
            ),
            (
                "uppercase fingerprint",
                "profile_fingerprint",
                serde_json::json!("A".repeat(64)),
            ),
            (
                "short fingerprint",
                "profile_fingerprint",
                serde_json::json!("a".repeat(63)),
            ),
            (
                "short recovery",
                "recovery_record",
                serde_json::json!("AAAA"),
            ),
        ];
        for (case, field, value) in invalid {
            let mut candidate = valid.clone();
            candidate[field] = value;
            assert!(!validator.is_valid(&candidate), "schema accepted {case}");
            assert!(
                serde_json::from_value::<EstablishVaultIdentityV1>(candidate).is_err(),
                "runtime accepted {case}"
            );
        }
    }

    #[test]
    fn establish_rejects_unknown_fields_and_invalid_context() {
        let mut value = serde_json::to_value(request()).unwrap();
        value["unexpected"] = true.into();
        assert!(serde_json::from_value::<EstablishVaultIdentityV1>(value).is_err());
        let mut invalid = request();
        invalid.key_epoch = 0;
        assert!(invalid.validate().is_err());
    }
}
