//! Vault-bound tester administration v2 wire contracts.

use crate::tester_admin::{LegacyTesterMapping, TesterStatus, valid_revision};
use peppy_crypto::{EncryptedEnvelope, KeyProfile, VaultCheckHeader};
use serde::{Deserialize, Deserializer, Serialize, de::Error};
use uuid::Uuid;

pub const VERSION: u8 = 2;
pub const ALGORITHM: &str = "xchacha20poly1305-ietf";
pub const NONCE_BYTES: usize = 24;
pub const MAX_ENTRIES: usize = 100;

/// Schema-capable public representation of `peppy_crypto::KeyProfile`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct KeyProfileV2 {
    pub crypto_suite: u16,
    pub salt: [u8; 16],
    pub vault_id: Uuid,
    pub key_epoch: u32,
}
impl From<KeyProfile> for KeyProfileV2 {
    fn from(value: KeyProfile) -> Self {
        Self {
            crypto_suite: value.crypto_suite,
            salt: value.salt,
            vault_id: value.vault_id,
            key_epoch: value.key_epoch,
        }
    }
}
impl From<KeyProfileV2> for KeyProfile {
    fn from(value: KeyProfileV2) -> Self {
        Self {
            crypto_suite: value.crypto_suite,
            salt: value.salt,
            vault_id: value.vault_id,
            key_epoch: value.key_epoch,
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EncryptedEnvelopeV2 {
    pub nonce: [u8; 24],
    pub ciphertext: Vec<u8>,
}
impl From<EncryptedEnvelope> for EncryptedEnvelopeV2 {
    fn from(value: EncryptedEnvelope) -> Self {
        Self {
            nonce: value.nonce,
            ciphertext: value.ciphertext,
        }
    }
}
impl From<EncryptedEnvelopeV2> for EncryptedEnvelope {
    fn from(value: EncryptedEnvelopeV2) -> Self {
        Self {
            nonce: value.nonce,
            ciphertext: value.ciphertext,
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct VaultCheckHeaderV2 {
    pub profile: KeyProfileV2,
    pub check: EncryptedEnvelopeV2,
}
impl From<VaultCheckHeader> for VaultCheckHeaderV2 {
    fn from(value: VaultCheckHeader) -> Self {
        Self {
            profile: value.profile.into(),
            check: value.check.into(),
        }
    }
}
impl From<VaultCheckHeaderV2> for VaultCheckHeader {
    fn from(value: VaultCheckHeaderV2) -> Self {
        Self {
            profile: value.profile.into(),
            check: value.check.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct VaultBindingV2 {
    pub account_id: Uuid,
    pub vault_id: Uuid,
    pub profile: KeyProfileV2,
    pub profile_fingerprint: String,
    pub key_epoch: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CiphertextV2 {
    #[serde(deserialize_with = "version")]
    #[schemars(schema_with = "version_schema")]
    pub version: u8,
    #[serde(deserialize_with = "algorithm")]
    #[schemars(schema_with = "algorithm_schema")]
    pub algorithm: String,
    #[serde(rename = "keyId")]
    pub key_id: Uuid,
    pub nonce: String,
    pub ciphertext: String,
}

fn version<'de, D: Deserializer<'de>>(deserializer: D) -> Result<u8, D::Error> {
    let value = u8::deserialize(deserializer)?;
    (value == VERSION)
        .then_some(value)
        .ok_or_else(|| D::Error::custom("unsupported envelope version"))
}
fn algorithm<'de, D: Deserializer<'de>>(deserializer: D) -> Result<String, D::Error> {
    let value = String::deserialize(deserializer)?;
    (value == ALGORITHM)
        .then_some(value)
        .ok_or_else(|| D::Error::custom("unsupported envelope algorithm"))
}
fn revision<'de, D: Deserializer<'de>>(deserializer: D) -> Result<String, D::Error> {
    let value = String::deserialize(deserializer)?;
    valid_revision(&value)
        .then_some(value)
        .ok_or_else(|| D::Error::custom("invalid registry revision"))
}
fn encryption_version<'de, D: Deserializer<'de>>(deserializer: D) -> Result<u8, D::Error> {
    let value = u8::deserialize(deserializer)?;
    (value <= VERSION)
        .then_some(value)
        .ok_or_else(|| D::Error::custom("unsupported tester encryption version"))
}
fn version_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({"type":"integer", "const":2})
}
fn algorithm_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({"type":"string", "const":"xchacha20poly1305-ietf"})
}
fn encryption_version_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({"type":"integer", "enum":[0,1,2]})
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TesterEntryV2 {
    pub entry_id: Uuid,
    pub ciphertext: CiphertextV2,
    pub status: TesterStatus,
}
#[derive(Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TesterInputV2 {
    pub entry_id: Uuid,
    pub email: String,
    pub ciphertext: CiphertextV2,
}
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TesterUnlockContextResponseV2 {
    pub registry_id: Uuid,
    pub binding: VaultBindingV2,
    pub vault_check_header: VaultCheckHeaderV2,
}
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TesterRegistryResponseV2 {
    pub registry_id: Uuid,
    #[serde(deserialize_with = "revision")]
    pub revision: String,
    pub initialized: bool,
    /// 0 is absent, 1 requires a legacy registry fetch, 2 is vault-bound.
    #[serde(deserialize_with = "encryption_version")]
    #[schemars(schema_with = "encryption_version_schema")]
    pub encryption_version: u8,
    pub binding: Option<VaultBindingV2>,
    pub key_id: Option<Uuid>,
    /// Historical server-verified header recorded with the binding, if v2 initialized.
    pub vault_check_header: Option<VaultCheckHeaderV2>,
    pub entries: Vec<TesterEntryV2>,
    pub legacy_accounts: Vec<Uuid>,
}
#[derive(Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct InitializeTestersRequestV2 {
    #[serde(deserialize_with = "revision")]
    pub expected_revision: String,
    pub binding: VaultBindingV2,
    pub key_id: Uuid,
    pub entries: Vec<TesterInputV2>,
    pub legacy_mappings: Vec<LegacyTesterMapping>,
    pub retire_legacy_accounts: Vec<Uuid>,
    pub confirm_legacy_retirement: bool,
}
#[derive(Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AddTesterRequestV2 {
    #[serde(deserialize_with = "revision")]
    pub expected_revision: String,
    pub binding: VaultBindingV2,
    pub entry: TesterInputV2,
}
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RemoveTesterRequestV2 {
    #[serde(deserialize_with = "revision")]
    pub expected_revision: String,
    pub binding: VaultBindingV2,
}
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RekeyTestersRequestV2 {
    #[serde(deserialize_with = "revision")]
    pub expected_revision: String,
    pub binding: VaultBindingV2,
    pub key_id: Uuid,
    pub entries: Vec<ReencryptedTesterEntryV2>,
}
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MigrateTestersRequestV2 {
    #[serde(deserialize_with = "revision")]
    pub expected_revision: String,
    pub binding: VaultBindingV2,
    pub key_id: Uuid,
    pub entries: Vec<ReencryptedTesterEntryV2>,
}
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReencryptedTesterEntryV2 {
    pub entry_id: Uuid,
    pub ciphertext: CiphertextV2,
}
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TesterMutationResponseV2 {
    #[serde(deserialize_with = "revision")]
    pub revision: String,
    pub entry: Option<TesterEntryV2>,
}

pub mod routes {
    pub const REGISTRY: &str = "/hosted/v2/web/admin/testers";
    pub const UNLOCK_CONTEXT: &str = "/hosted/v2/web/admin/testers/unlock-context";
    pub const INITIALIZE: &str = "/hosted/v2/web/admin/testers/initialize";
    pub const ENTRIES: &str = "/hosted/v2/web/admin/testers/entries";
    pub const REMOVE: &str = "/hosted/v2/web/admin/testers/entries/{entry_id}";
    pub const REKEY: &str = "/hosted/v2/web/admin/testers/rekey";
    pub const MIGRATE: &str = "/hosted/v2/web/admin/testers/migrate";
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn v2_constants_are_fixed() {
        assert_eq!(VERSION, 2);
        assert_eq!(ALGORITHM, "xchacha20poly1305-ietf");
        assert_eq!(NONCE_BYTES, 24);
    }
    #[test]
    fn v2_routes_are_separate_from_v1() {
        assert_eq!(
            routes::UNLOCK_CONTEXT,
            format!("{}/unlock-context", routes::REGISTRY)
        );
        assert_eq!(routes::MIGRATE, format!("{}/migrate", routes::REGISTRY));
    }
}
