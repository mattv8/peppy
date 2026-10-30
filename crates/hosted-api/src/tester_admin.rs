//! Canonical tester administration contracts. Plaintext input emails are transient.

use serde::{Deserialize, Deserializer, Serialize, de::Error};
use uuid::Uuid;

/// Apply before parsing admin request bodies; unrelated routes retain their limits.
pub const MAX_REQUEST_BODY_BYTES: usize = 256 * 1024;
/// Registry capacity, including retained revoked entries; never silently truncate.
pub const MAX_ENTRIES: usize = 100;
/// ASCII input bytes. Envelope fields use canonical unpadded base64url.
pub const MAX_EMAIL_BYTES: usize = 254;
/// Decoded ciphertext bytes, including the 16-byte AES-GCM authentication tag.
pub const MAX_CIPHERTEXT_BYTES: usize = 512;
pub const NONCE_BYTES: usize = 12;
pub const SALT_BYTES: usize = 16;
pub const KEY_ID_BYTES: usize = 36;
pub const RESET_CONFIRMATION: &str = "reset testers";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct KdfV1 {
    #[serde(deserialize_with = "deserialize_version")]
    #[schemars(schema_with = "version_schema")]
    pub version: u8,
    #[serde(deserialize_with = "deserialize_kdf_algorithm")]
    #[schemars(schema_with = "kdf_algorithm_schema")]
    pub algorithm: String,
    #[serde(deserialize_with = "deserialize_iterations")]
    #[schemars(schema_with = "iterations_schema")]
    pub iterations: u32,
    pub salt: String,
    #[serde(rename = "keyId")]
    pub key_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CiphertextV1 {
    #[serde(deserialize_with = "deserialize_version")]
    #[schemars(schema_with = "version_schema")]
    pub version: u8,
    #[serde(deserialize_with = "deserialize_cipher_algorithm")]
    #[schemars(schema_with = "cipher_algorithm_schema")]
    pub algorithm: String,
    #[serde(rename = "keyId")]
    pub key_id: String,
    pub nonce: String,
    pub ciphertext: String,
}

fn deserialize_version<'de, D: Deserializer<'de>>(deserializer: D) -> Result<u8, D::Error> {
    let value = u8::deserialize(deserializer)?;
    if value != 1 {
        return Err(D::Error::custom("unsupported envelope version"));
    }
    Ok(value)
}

fn deserialize_iterations<'de, D: Deserializer<'de>>(deserializer: D) -> Result<u32, D::Error> {
    let value = u32::deserialize(deserializer)?;
    if value != 600_000 {
        return Err(D::Error::custom("unsupported KDF cost"));
    }
    Ok(value)
}

fn deserialize_kdf_algorithm<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<String, D::Error> {
    deserialize_algorithm(deserializer, "pbkdf2-sha256+hkdf-sha256")
}

fn deserialize_cipher_algorithm<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<String, D::Error> {
    deserialize_algorithm(deserializer, "aes-256-gcm")
}

fn deserialize_algorithm<'de, D: Deserializer<'de>>(
    deserializer: D,
    expected: &str,
) -> Result<String, D::Error> {
    let value = String::deserialize(deserializer)?;
    if value != expected {
        return Err(D::Error::custom("unsupported algorithm"));
    }
    Ok(value)
}

fn version_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({"type":"integer", "const":1})
}
fn iterations_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({"type":"integer", "const":600000})
}
fn kdf_algorithm_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({"type":"string", "const":"pbkdf2-sha256+hkdf-sha256"})
}
fn cipher_algorithm_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({"type":"string", "const":"aes-256-gcm"})
}

/// Registry revisions fit PostgreSQL bigint and use canonical unsigned decimals.
pub fn valid_revision(value: &str) -> bool {
    !value.is_empty()
        && (value == "0" || !value.starts_with('0'))
        && value.bytes().all(|byte| byte.is_ascii_digit())
        && value.parse::<i64>().is_ok()
}

fn deserialize_revision<'de, D: Deserializer<'de>>(deserializer: D) -> Result<String, D::Error> {
    let value = String::deserialize(deserializer)?;
    if !valid_revision(&value) {
        return Err(D::Error::custom("invalid registry revision"));
    }
    Ok(value)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TesterStatus {
    PendingVerification,
    Active,
    Blocked,
    Revoked,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TesterEntry {
    pub entry_id: Uuid,
    pub ciphertext: CiphertextV1,
    pub status: TesterStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TesterRegistryResponse {
    pub registry_id: Uuid,
    #[serde(deserialize_with = "deserialize_revision")]
    pub revision: String,
    pub initialized: bool,
    pub kdf: Option<KdfV1>,
    pub check: Option<CiphertextV1>,
    pub entries: Vec<TesterEntry>,
    pub legacy_accounts: Vec<Uuid>,
}

// No Debug: this input contains a transient plaintext email.
#[derive(Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TesterInput {
    pub entry_id: Uuid,
    pub email: String,
    pub ciphertext: CiphertextV1,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LegacyTesterMapping {
    pub account_id: Uuid,
    pub entry_id: Uuid,
}

#[derive(Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct InitializeTestersRequest {
    #[serde(deserialize_with = "deserialize_revision")]
    pub expected_revision: String,
    pub kdf: KdfV1,
    pub check: CiphertextV1,
    pub entries: Vec<TesterInput>,
    pub legacy_mappings: Vec<LegacyTesterMapping>,
    pub retire_legacy_accounts: Vec<Uuid>,
    pub confirm_legacy_retirement: bool,
}

#[derive(Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AddTesterRequest {
    #[serde(deserialize_with = "deserialize_revision")]
    pub expected_revision: String,
    pub entry: TesterInput,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RemoveTesterRequest {
    #[serde(deserialize_with = "deserialize_revision")]
    pub expected_revision: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReencryptedTesterEntry {
    pub entry_id: Uuid,
    pub ciphertext: CiphertextV1,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RekeyTestersRequest {
    #[serde(deserialize_with = "deserialize_revision")]
    pub expected_revision: String,
    pub kdf: KdfV1,
    pub check: CiphertextV1,
    pub entries: Vec<ReencryptedTesterEntry>,
}

#[derive(Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ResetTestersRequest {
    #[serde(deserialize_with = "deserialize_revision")]
    pub expected_revision: String,
    pub confirmation: String,
    pub kdf: KdfV1,
    pub check: CiphertextV1,
    pub entries: Vec<TesterInput>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TesterMutationResponse {
    #[serde(deserialize_with = "deserialize_revision")]
    pub revision: String,
    pub entry: Option<TesterEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TesterErrorResponse {
    pub code: String,
    pub message: String,
}

pub mod routes {
    pub const REGISTRY: &str = "/hosted/v1/web/admin/testers";
    pub const INITIALIZE: &str = "/hosted/v1/web/admin/testers/initialize";
    pub const ENTRIES: &str = "/hosted/v1/web/admin/testers/entries";
    pub const REMOVE: &str = "/hosted/v1/web/admin/testers/entries/{entry_id}";
    pub const REKEY: &str = "/hosted/v1/web/admin/testers/rekey";
    pub const RESET: &str = "/hosted/v1/web/admin/testers/reset";
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn kdf() -> serde_json::Value {
        json!({"version":1,"algorithm":"pbkdf2-sha256+hkdf-sha256","iterations":600000,
            "salt":"AAAAAAAAAAAAAAAAAAAAAA","keyId":"aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"})
    }

    fn ciphertext() -> serde_json::Value {
        json!({"version":1,"algorithm":"aes-256-gcm",
            "keyId":"aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
            "nonce":"AAAAAAAAAAAAAAAA","ciphertext":"AAAAAAAAAAAAAAAAAAAAAA"})
    }

    #[test]
    fn synthetic_profiles_roundtrip_exact_wire_names() {
        let parsed: KdfV1 = serde_json::from_value(kdf()).unwrap();
        assert_eq!(serde_json::to_value(parsed).unwrap(), kdf());
        let parsed: CiphertextV1 = serde_json::from_value(ciphertext()).unwrap();
        assert_eq!(serde_json::to_value(parsed).unwrap(), ciphertext());
    }

    #[test]
    fn unsupported_profiles_and_unknown_fields_are_rejected() {
        for (field, value) in [
            ("version", json!(2)),
            ("algorithm", json!("other")),
            ("iterations", json!(1)),
            ("key_id", json!("unexpected")),
        ] {
            let mut input = kdf();
            input[field] = value;
            assert!(serde_json::from_value::<KdfV1>(input).is_err(), "{field}");
        }
        for (field, value) in [
            ("version", json!(2)),
            ("algorithm", json!("other")),
            ("extra", json!(true)),
        ] {
            let mut input = ciphertext();
            input[field] = value;
            assert!(
                serde_json::from_value::<CiphertextV1>(input).is_err(),
                "{field}"
            );
        }
    }

    #[test]
    fn revisions_require_canonical_nonnegative_bigint_decimal_strings() {
        for revision in [json!("0"), json!("1"), json!("9223372036854775807")] {
            assert!(
                serde_json::from_value::<RemoveTesterRequest>(
                    json!({"expected_revision":revision})
                )
                .is_ok()
            );
        }
        for revision in [
            json!(""),
            json!("01"),
            json!("-1"),
            json!("+1"),
            json!(" 1"),
            json!("9223372036854775808"),
            json!(1),
        ] {
            assert!(
                serde_json::from_value::<RemoveTesterRequest>(
                    json!({"expected_revision":revision})
                )
                .is_err()
            );
        }
    }

    #[test]
    fn registry_roundtrip_preserves_nullable_initial_state_and_canonical_uuids() {
        let input = json!({"registry_id":"AAAAAAAA-AAAA-4AAA-8AAA-AAAAAAAAAAAA",
            "revision":"0","initialized":false,"kdf":null,"check":null,"entries":[],"legacy_accounts":[]});
        let parsed: TesterRegistryResponse = serde_json::from_value(input).unwrap();
        let output = serde_json::to_value(parsed).unwrap();
        assert_eq!(
            output["registry_id"],
            "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"
        );
        assert_eq!(output["revision"], "0");
        assert!(output["kdf"].is_null());
    }

    #[test]
    fn tester_routes_are_stable() {
        assert_eq!(routes::REGISTRY, "/hosted/v1/web/admin/testers");
        assert_eq!(
            routes::INITIALIZE,
            format!("{}/initialize", routes::REGISTRY)
        );
        assert_eq!(routes::ENTRIES, format!("{}/entries", routes::REGISTRY));
        assert_eq!(
            routes::REMOVE,
            format!("{}/entries/{{entry_id}}", routes::REGISTRY)
        );
        assert_eq!(routes::REKEY, format!("{}/rekey", routes::REGISTRY));
        assert_eq!(routes::RESET, format!("{}/reset", routes::REGISTRY));
    }

    #[test]
    fn mutations_roundtrip_only_the_frozen_dictionary() {
        fn roundtrip<T: serde::de::DeserializeOwned + serde::Serialize>(value: serde_json::Value) {
            let parsed: T = serde_json::from_value(value.clone()).unwrap();
            assert_eq!(serde_json::to_value(parsed).unwrap(), value);
            let mut extra = value;
            extra["unexpected"] = json!(true);
            assert!(serde_json::from_value::<T>(extra).is_err());
        }
        let id = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
        let entry = json!({"entry_id":id,"email":"tester@example.test","ciphertext":ciphertext()});
        roundtrip::<InitializeTestersRequest>(json!({"expected_revision":"0","kdf":kdf(),
            "check":ciphertext(),"entries":[entry.clone()],"legacy_mappings":[{"account_id":id,"entry_id":id}],
            "retire_legacy_accounts":[],"confirm_legacy_retirement":false}));
        roundtrip::<AddTesterRequest>(json!({"expected_revision":"1","entry":entry.clone()}));
        roundtrip::<RemoveTesterRequest>(json!({"expected_revision":"2"}));
        roundtrip::<RekeyTestersRequest>(json!({"expected_revision":"3","kdf":kdf(),
            "check":ciphertext(),"entries":[{"entry_id":id,"ciphertext":ciphertext()}]}));
        roundtrip::<ResetTestersRequest>(
            json!({"expected_revision":"4","confirmation":RESET_CONFIRMATION,
            "kdf":kdf(),"check":ciphertext(),"entries":[entry]}),
        );
        for status in ["pending_verification", "active", "blocked", "revoked"] {
            roundtrip::<TesterMutationResponse>(json!({"revision":"5","entry":{
                "entry_id":id,"ciphertext":ciphertext(),"status":status}}));
        }
        roundtrip::<TesterMutationResponse>(json!({"revision":"6","entry":null}));
        roundtrip::<TesterErrorResponse>(json!({"code":"conflict","message":"Reload registry"}));
    }
}
