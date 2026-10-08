//! Internal Worker-only local identity storage envelope.
//!
//! This is deliberately not a UI DTO. Callers supply the trusted Worker origin and persist the
//! resulting opaque JSON themselves. For a profile transition, callers must first seal and verify
//! the new passphrase/header envelope, then manually unlock and activate the epoch, then atomically
//! checkpoint the SQLCipher state and Rust-owned envelope bytes. They must reject an authenticated
//! wrapping epoch that differs from the persisted database active epoch; this module never resets
//! either state.

use peppy_crypto::{
    CryptoError, EncryptedEnvelope, KeyProfile, KeyPurpose, VaultCheckHeader, decrypt,
    derive_purpose_key, derive_root_key, encrypt, verify_vault_check_header,
};
use serde::{Deserialize, Serialize, de};
use serde_json::Value;
use std::collections::HashSet;
use uuid::Uuid;
use zeroize::Zeroizing;

const VERSION: u8 = 1;
const MAX_ENCODED_BYTES: usize = 64 * 1024;
const DATABASE_KEY_BYTES: usize = 32;
const DEVICE_TOKEN_HEX: usize = 96;
const PLAINTEXT_BYTES: usize = DATABASE_KEY_BYTES + DEVICE_TOKEN_HEX;
const CIPHERTEXT_BYTES: usize = PLAINTEXT_BYTES + 16;
const AAD_PREFIX: &[u8] = b"peppy-browser-local-identity-v1\0";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct IdentityMetadata {
    pub version: u8,
    pub origin: String,
    pub vault_id: Uuid,
    pub device_id: Uuid,
    pub role: DeviceRole,
    pub profile: KeyProfile,
    pub header: VaultCheckHeader,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DeviceRole {
    Owner,
    Device,
    Gateway,
}

/// Secrets retained only by this Rust boundary; it intentionally has no `Debug`, `Clone`, or
/// serialization implementation.
pub struct IdentitySecrets {
    database_key: Zeroizing<[u8; DATABASE_KEY_BYTES]>,
    device_token: Zeroizing<String>,
}

impl IdentitySecrets {
    /// Accepts a host RNG-created key by value. The input array may transiently exist in CPU stack
    /// locations before this boundary receives it; Rust cannot guarantee their erasure.
    pub fn new(
        database_key: [u8; DATABASE_KEY_BYTES],
        device_token: String,
    ) -> Result<Self, IdentityError> {
        let database_key = Zeroizing::new(database_key);
        let device_token = Zeroizing::new(device_token);
        Self::from_parts(database_key, device_token)
    }

    fn from_parts(
        database_key: Zeroizing<[u8; DATABASE_KEY_BYTES]>,
        device_token: Zeroizing<String>,
    ) -> Result<Self, IdentityError> {
        validate_token(&device_token)?;
        Ok(Self {
            database_key,
            device_token,
        })
    }

    pub fn with_database_key<T>(
        &self,
        operation: impl FnOnce(&[u8; DATABASE_KEY_BYTES]) -> T,
    ) -> T {
        operation(&self.database_key)
    }

    pub fn with_device_token<T>(&self, operation: impl FnOnce(&str) -> T) -> T {
        operation(&self.device_token)
    }
}

/// An unlocked identity is internal Rust state, not a serializable host response.
pub struct UnlockedIdentity {
    metadata: IdentityMetadata,
    secrets: IdentitySecrets,
}

impl UnlockedIdentity {
    pub fn metadata(&self) -> &IdentityMetadata {
        &self.metadata
    }

    pub fn secrets(&self) -> &IdentitySecrets {
        &self.secrets
    }

    /// Transfers secrets only to the Worker Rust caller for an explicit rewrap transaction.
    pub fn into_secrets(self) -> IdentitySecrets {
        self.secrets
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WrappedIdentity {
    pub metadata: IdentityMetadata,
    pub envelope: EncryptedEnvelope,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IdentityError {
    Invalid,
    InvalidPassphrase,
    AuthFailed,
    Unavailable,
}

/// Seals a host-RNG supplied database key and credential token. The returned value contains only
/// public metadata and ciphertext.
pub fn seal_identity(
    metadata: IdentityMetadata,
    secrets: &IdentitySecrets,
    passphrase: &str,
) -> Result<WrappedIdentity, IdentityError> {
    validate_metadata(&metadata)?;
    let root = derive_root_key(passphrase, &metadata.profile).map_err(map_crypto_error)?;
    verify_vault_check_header(&root, &metadata.profile, &metadata.header)
        .map_err(map_crypto_error)?;
    let key = derive_purpose_key(&root, &metadata.profile, KeyPurpose::LocalWrap)
        .map_err(map_crypto_error)?;
    let aad = metadata_aad(&metadata)?;
    let plaintext = encode_secrets(secrets);
    let envelope = encrypt(&key, &aad, &plaintext).map_err(map_crypto_error)?;
    if envelope.ciphertext.len() != CIPHERTEXT_BYTES
        || encoded_len(&metadata, &envelope)? > MAX_ENCODED_BYTES
    {
        return Err(IdentityError::Invalid);
    }
    Ok(WrappedIdentity { metadata, envelope })
}

/// Opens an envelope only for the exact trusted Worker location origin.
pub fn open_identity(
    serialized: &[u8],
    expected_origin: &str,
    passphrase: &str,
) -> Result<UnlockedIdentity, IdentityError> {
    if serialized.len() > MAX_ENCODED_BYTES {
        return Err(IdentityError::Invalid);
    }
    let expected_origin = canonical_origin(expected_origin)?;
    serde_json::from_slice::<NoDuplicateJson>(serialized).map_err(|_| IdentityError::Invalid)?;
    let value: Value = serde_json::from_slice(serialized).map_err(|_| IdentityError::Invalid)?;
    reject_unknown_fields(&value)?;
    let wrapped: WrappedIdentity =
        serde_json::from_slice(serialized).map_err(|_| IdentityError::Invalid)?;
    validate_wrapped(&wrapped)?;
    if wrapped.metadata.origin != expected_origin {
        return Err(IdentityError::Invalid);
    }
    let root = derive_root_key(passphrase, &wrapped.metadata.profile).map_err(map_crypto_error)?;
    verify_vault_check_header(&root, &wrapped.metadata.profile, &wrapped.metadata.header)
        .map_err(map_crypto_error)?;
    let key = derive_purpose_key(&root, &wrapped.metadata.profile, KeyPurpose::LocalWrap)
        .map_err(map_crypto_error)?;
    let aad = metadata_aad(&wrapped.metadata)?;
    let plaintext =
        Zeroizing::new(decrypt(&key, &aad, &wrapped.envelope).map_err(map_crypto_error)?);
    let secrets = decode_secrets(&plaintext)?;
    Ok(UnlockedIdentity {
        metadata: wrapped.metadata,
        secrets,
    })
}

fn validate_wrapped(wrapped: &WrappedIdentity) -> Result<(), IdentityError> {
    validate_metadata(&wrapped.metadata)?;
    if wrapped.envelope.ciphertext.len() != CIPHERTEXT_BYTES {
        return Err(IdentityError::Invalid);
    }
    Ok(())
}

pub(crate) fn validate_metadata(metadata: &IdentityMetadata) -> Result<(), IdentityError> {
    if metadata.version != VERSION
        || canonical_origin(&metadata.origin)? != metadata.origin
        || metadata.profile.vault_id != metadata.vault_id
        || metadata.header.profile != metadata.profile
    {
        return Err(IdentityError::Invalid);
    }
    metadata
        .profile
        .validate()
        .map_err(|_| IdentityError::Invalid)
}

fn canonical_origin(input: &str) -> Result<String, IdentityError> {
    crate::origin::canonical_origin(input).map_err(|_| IdentityError::Invalid)
}

fn metadata_aad(metadata: &IdentityMetadata) -> Result<Vec<u8>, IdentityError> {
    let encoded = serde_json::to_vec(metadata).map_err(|_| IdentityError::Invalid)?;
    let mut aad = Vec::with_capacity(AAD_PREFIX.len() + encoded.len());
    aad.extend_from_slice(AAD_PREFIX);
    aad.extend_from_slice(&encoded);
    Ok(aad)
}

fn encode_secrets(secrets: &IdentitySecrets) -> Zeroizing<Vec<u8>> {
    let mut plaintext = Zeroizing::new(Vec::with_capacity(PLAINTEXT_BYTES));
    plaintext.extend_from_slice(secrets.database_key.as_slice());
    plaintext.extend_from_slice(secrets.device_token.as_bytes());
    plaintext
}

fn decode_secrets(plaintext: &[u8]) -> Result<IdentitySecrets, IdentityError> {
    if plaintext.len() != PLAINTEXT_BYTES {
        return Err(IdentityError::Invalid);
    }
    let mut database_key = Zeroizing::new([0; DATABASE_KEY_BYTES]);
    database_key.copy_from_slice(&plaintext[..DATABASE_KEY_BYTES]);
    let token = Zeroizing::new(
        std::str::from_utf8(&plaintext[DATABASE_KEY_BYTES..])
            .map_err(|_| IdentityError::Invalid)?
            .to_owned(),
    );
    IdentitySecrets::from_parts(database_key, token)
}

fn map_crypto_error(error: CryptoError) -> IdentityError {
    match error {
        CryptoError::AuthenticationFailed => IdentityError::AuthFailed,
        CryptoError::SurroundingWhitespace => IdentityError::InvalidPassphrase,
        CryptoError::UnsupportedSuite
        | CryptoError::InvalidProfile
        | CryptoError::InvalidCiphertext => IdentityError::Invalid,
        CryptoError::OperationFailed | CryptoError::InvalidStream | CryptoError::Io => {
            IdentityError::Unavailable
        }
    }
}

pub(crate) fn validate_token(token: &str) -> Result<(), IdentityError> {
    if token.len() != DEVICE_TOKEN_HEX || !token.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(IdentityError::Invalid);
    }
    Ok(())
}

fn reject_unknown_fields(value: &Value) -> Result<(), IdentityError> {
    let wrapped = value.as_object().ok_or(IdentityError::Invalid)?;
    require_fields(wrapped, &["metadata", "envelope"])?;
    let metadata = wrapped
        .get("metadata")
        .and_then(Value::as_object)
        .ok_or(IdentityError::Invalid)?;
    require_fields(
        metadata,
        &[
            "version", "origin", "vaultId", "deviceId", "role", "profile", "header",
        ],
    )?;
    let profile = metadata
        .get("profile")
        .and_then(Value::as_object)
        .ok_or(IdentityError::Invalid)?;
    require_fields(profile, &["crypto_suite", "salt", "vault_id", "key_epoch"])?;
    let header = metadata
        .get("header")
        .and_then(Value::as_object)
        .ok_or(IdentityError::Invalid)?;
    require_fields(header, &["profile", "check"])?;
    let header_profile = header
        .get("profile")
        .and_then(Value::as_object)
        .ok_or(IdentityError::Invalid)?;
    require_fields(
        header_profile,
        &["crypto_suite", "salt", "vault_id", "key_epoch"],
    )?;
    for envelope in [wrapped.get("envelope"), header.get("check")] {
        let envelope = envelope
            .and_then(Value::as_object)
            .ok_or(IdentityError::Invalid)?;
        require_fields(envelope, &["nonce", "ciphertext"])?;
    }
    Ok(())
}

fn require_fields(
    object: &serde_json::Map<String, Value>,
    expected: &[&str],
) -> Result<(), IdentityError> {
    if object.len() != expected.len()
        || object
            .keys()
            .any(|field| !expected.contains(&field.as_str()))
    {
        return Err(IdentityError::Invalid);
    }
    Ok(())
}

struct NoDuplicateJson;

impl<'de> Deserialize<'de> for NoDuplicateJson {
    fn deserialize<D: de::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(NoDuplicateVisitor)
    }
}

struct NoDuplicateVisitor;

impl<'de> de::Visitor<'de> for NoDuplicateVisitor {
    type Value = NoDuplicateJson;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("JSON without duplicate object keys")
    }

    fn visit_bool<E: de::Error>(self, _: bool) -> Result<Self::Value, E> {
        Ok(NoDuplicateJson)
    }

    fn visit_i64<E: de::Error>(self, _: i64) -> Result<Self::Value, E> {
        Ok(NoDuplicateJson)
    }

    fn visit_u64<E: de::Error>(self, _: u64) -> Result<Self::Value, E> {
        Ok(NoDuplicateJson)
    }

    fn visit_f64<E: de::Error>(self, _: f64) -> Result<Self::Value, E> {
        Ok(NoDuplicateJson)
    }

    fn visit_str<E: de::Error>(self, _: &str) -> Result<Self::Value, E> {
        Ok(NoDuplicateJson)
    }

    fn visit_borrowed_str<E: de::Error>(self, _: &'de str) -> Result<Self::Value, E> {
        Ok(NoDuplicateJson)
    }

    fn visit_string<E: de::Error>(self, _: String) -> Result<Self::Value, E> {
        Ok(NoDuplicateJson)
    }

    fn visit_none<E: de::Error>(self) -> Result<Self::Value, E> {
        Ok(NoDuplicateJson)
    }

    fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
        Ok(NoDuplicateJson)
    }

    fn visit_some<D: de::Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> Result<Self::Value, D::Error> {
        Deserialize::deserialize(deserializer)
    }

    fn visit_seq<A: de::SeqAccess<'de>>(self, mut sequence: A) -> Result<Self::Value, A::Error> {
        while sequence.next_element::<NoDuplicateJson>()?.is_some() {}
        Ok(NoDuplicateJson)
    }

    fn visit_map<A: de::MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        let mut keys = HashSet::new();
        while let Some(key) = map.next_key::<String>()? {
            if !keys.insert(key) {
                return Err(de::Error::custom("duplicate JSON key"));
            }
            map.next_value::<NoDuplicateJson>()?;
        }
        Ok(NoDuplicateJson)
    }
}

fn encoded_len(
    metadata: &IdentityMetadata,
    envelope: &EncryptedEnvelope,
) -> Result<usize, IdentityError> {
    serde_json::to_vec(&WrappedIdentity {
        metadata: metadata.clone(),
        envelope: envelope.clone(),
    })
    .map(|encoded| encoded.len())
    .map_err(|_| IdentityError::Invalid)
}

#[cfg(test)]
mod tests {
    use super::*;
    use peppy_crypto::create_vault_check_header;
    use std::sync::OnceLock;

    const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    const PHRASE: &str = "correct horse battery staple";

    fn fixture() -> &'static (KeyProfile, VaultCheckHeader) {
        static FIXTURE: OnceLock<(KeyProfile, VaultCheckHeader)> = OnceLock::new();
        FIXTURE.get_or_init(|| {
            let profile = KeyProfile::new(Uuid::new_v4(), 1).unwrap();
            let root = derive_root_key(PHRASE, &profile).unwrap();
            let header = create_vault_check_header(&root, profile.clone()).unwrap();
            (profile, header)
        })
    }

    fn metadata() -> IdentityMetadata {
        metadata_for_origin("https://peppy.test")
    }

    fn metadata_for_origin(origin: &str) -> IdentityMetadata {
        let (profile, header) = fixture();
        IdentityMetadata {
            version: VERSION,
            origin: origin.into(),
            vault_id: profile.vault_id,
            device_id: Uuid::new_v4(),
            role: DeviceRole::Device,
            profile: profile.clone(),
            header: header.clone(),
        }
    }

    fn secrets() -> IdentitySecrets {
        IdentitySecrets::new([0xA5; DATABASE_KEY_BYTES], TOKEN.into()).unwrap()
    }

    fn sealed() -> (IdentityMetadata, Vec<u8>) {
        let metadata = metadata();
        let secrets = secrets();
        let wrapped = seal_identity(metadata.clone(), &secrets, PHRASE).unwrap();
        (metadata, serde_json::to_vec(&wrapped).unwrap())
    }

    #[test]
    fn seals_and_opens_a_bound_identity_without_plaintext_markers() {
        let (metadata, serialized) = sealed();
        assert!(
            !serialized
                .windows(TOKEN.len())
                .any(|bytes| bytes == TOKEN.as_bytes())
        );
        assert!(!serialized.windows(16).any(|bytes| bytes == [0xA5; 16]));
        assert!(
            !serialized
                .windows(PHRASE.len())
                .any(|bytes| bytes == PHRASE.as_bytes())
        );
        let unlocked = open_identity(&serialized, "https://peppy.test/", PHRASE).unwrap();
        assert_eq!(unlocked.metadata(), &metadata);
        unlocked
            .secrets()
            .with_database_key(|key| assert_eq!(key, &[0xA5; DATABASE_KEY_BYTES]));
        unlocked
            .secrets()
            .with_device_token(|token| assert_eq!(token, TOKEN));
    }

    #[test]
    fn seals_and_opens_loopback_http_identities_only() {
        for origin in [
            "http://localhost:7100",
            "http://127.0.0.1:7100",
            "http://[::1]:7100",
        ] {
            let metadata = metadata_for_origin(origin);
            let wrapped = seal_identity(metadata.clone(), &secrets(), PHRASE).unwrap();
            let serialized = serde_json::to_vec(&wrapped).unwrap();
            assert_eq!(
                open_identity(&serialized, origin, PHRASE)
                    .unwrap()
                    .metadata(),
                &metadata
            );
        }

        for origin in [
            "http://192.168.1.1",
            "http://example.test",
            "http://localhost.evil.test",
            "http://127.0.0.1.evil.test",
            "http://user:secret@localhost",
            "http://localhost/path",
            "http://localhost?query=value",
            "http://localhost#fragment",
        ] {
            assert!(matches!(
                seal_identity(metadata_for_origin(origin), &secrets(), PHRASE),
                Err(IdentityError::Invalid)
            ));
        }
    }

    #[test]
    fn metadata_aad_is_pinned_to_the_v1_golden_bytes() {
        let vault_id = Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap();
        let profile = KeyProfile {
            crypto_suite: 1,
            salt: [1; 16],
            vault_id,
            key_epoch: 7,
        };
        let metadata = IdentityMetadata {
            version: VERSION,
            origin: "https://example.test".into(),
            vault_id,
            device_id: Uuid::parse_str("22222222-2222-2222-2222-222222222222").unwrap(),
            role: DeviceRole::Owner,
            profile: profile.clone(),
            header: VaultCheckHeader {
                profile,
                check: EncryptedEnvelope {
                    nonce: [2; 24],
                    ciphertext: vec![3; 16],
                },
            },
        };
        assert_eq!(
            metadata_aad(&metadata).unwrap(),
            b"peppy-browser-local-identity-v1\0{\"version\":1,\"origin\":\"https://example.test\",\"vaultId\":\"11111111-1111-1111-1111-111111111111\",\"deviceId\":\"22222222-2222-2222-2222-222222222222\",\"role\":\"owner\",\"profile\":{\"crypto_suite\":1,\"salt\":[1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1],\"vault_id\":\"11111111-1111-1111-1111-111111111111\",\"key_epoch\":7},\"header\":{\"profile\":{\"crypto_suite\":1,\"salt\":[1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1],\"vault_id\":\"11111111-1111-1111-1111-111111111111\",\"key_epoch\":7},\"check\":{\"nonce\":[2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2],\"ciphertext\":[3,3,3,3,3,3,3,3,3,3,3,3,3,3,3,3]}}}".to_vec()
        );
    }

    #[test]
    fn crypto_failures_have_fixed_typed_classifications() {
        assert_eq!(
            map_crypto_error(CryptoError::AuthenticationFailed),
            IdentityError::AuthFailed
        );
        assert_eq!(
            map_crypto_error(CryptoError::SurroundingWhitespace),
            IdentityError::InvalidPassphrase
        );
        assert_eq!(
            map_crypto_error(CryptoError::InvalidProfile),
            IdentityError::Invalid
        );
        assert_eq!(
            map_crypto_error(CryptoError::OperationFailed),
            IdentityError::Unavailable
        );
        let (_, serialized) = sealed();
        assert!(matches!(
            open_identity(&serialized, "https://peppy.test", " bad phrase "),
            Err(IdentityError::InvalidPassphrase)
        ));
    }

    #[test]
    fn rejects_wrong_phrase_and_authenticated_metadata_or_ciphertext_changes() {
        let (_, serialized) = sealed();
        assert!(matches!(
            open_identity(&serialized, "https://peppy.test", "wrong"),
            Err(IdentityError::AuthFailed)
        ));
        let mut wrapped: WrappedIdentity = serde_json::from_slice(&serialized).unwrap();
        wrapped.metadata.device_id = Uuid::new_v4();
        assert!(matches!(
            open_identity(
                &serde_json::to_vec(&wrapped).unwrap(),
                "https://peppy.test",
                PHRASE
            ),
            Err(IdentityError::AuthFailed)
        ));
        let mut wrapped: WrappedIdentity = serde_json::from_slice(&serialized).unwrap();
        wrapped.metadata.vault_id = Uuid::new_v4();
        assert!(matches!(
            open_identity(
                &serde_json::to_vec(&wrapped).unwrap(),
                "https://peppy.test",
                PHRASE
            ),
            Err(IdentityError::Invalid)
        ));
        let mut wrapped: WrappedIdentity = serde_json::from_slice(&serialized).unwrap();
        wrapped.metadata.role = DeviceRole::Gateway;
        assert!(matches!(
            open_identity(
                &serde_json::to_vec(&wrapped).unwrap(),
                "https://peppy.test",
                PHRASE
            ),
            Err(IdentityError::AuthFailed)
        ));
        let mut wrapped: WrappedIdentity = serde_json::from_slice(&serialized).unwrap();
        wrapped.metadata.profile.key_epoch += 1;
        assert!(matches!(
            open_identity(
                &serde_json::to_vec(&wrapped).unwrap(),
                "https://peppy.test",
                PHRASE
            ),
            Err(IdentityError::Invalid)
        ));
        let mut wrapped: WrappedIdentity = serde_json::from_slice(&serialized).unwrap();
        wrapped.metadata.header.check.ciphertext[0] ^= 1;
        assert!(matches!(
            open_identity(
                &serde_json::to_vec(&wrapped).unwrap(),
                "https://peppy.test",
                PHRASE
            ),
            Err(IdentityError::AuthFailed)
        ));
        let mut wrapped: WrappedIdentity = serde_json::from_slice(&serialized).unwrap();
        let replacement_profile = KeyProfile::new(wrapped.metadata.vault_id, 2).unwrap();
        let replacement_root = derive_root_key(PHRASE, &replacement_profile).unwrap();
        wrapped.metadata.profile = replacement_profile.clone();
        wrapped.metadata.header =
            create_vault_check_header(&replacement_root, replacement_profile).unwrap();
        assert!(matches!(
            open_identity(
                &serde_json::to_vec(&wrapped).unwrap(),
                "https://peppy.test",
                PHRASE
            ),
            Err(IdentityError::AuthFailed)
        ));
        let mut wrapped: WrappedIdentity = serde_json::from_slice(&serialized).unwrap();
        wrapped.envelope.ciphertext[0] ^= 1;
        assert!(matches!(
            open_identity(
                &serde_json::to_vec(&wrapped).unwrap(),
                "https://peppy.test",
                PHRASE
            ),
            Err(IdentityError::AuthFailed)
        ));
    }

    #[test]
    fn rejects_wrong_origin_and_malformed_or_unrecognized_data_before_kdf() {
        let (_, serialized) = sealed();
        assert!(matches!(
            open_identity(&serialized, "https://other.test", PHRASE),
            Err(IdentityError::Invalid)
        ));
        let mut transplanted: WrappedIdentity = serde_json::from_slice(&serialized).unwrap();
        transplanted.metadata.origin = "https://other.test".into();
        assert!(matches!(
            open_identity(
                &serde_json::to_vec(&transplanted).unwrap(),
                "https://other.test",
                PHRASE
            ),
            Err(IdentityError::AuthFailed)
        ));
        let mut value: serde_json::Value = serde_json::from_slice(&serialized).unwrap();
        value["metadata"]["version"] = serde_json::json!(2);
        assert!(matches!(
            open_identity(
                &serde_json::to_vec(&value).unwrap(),
                "https://peppy.test",
                "wrong"
            ),
            Err(IdentityError::Invalid)
        ));
        value["unknown"] = serde_json::json!(true);
        assert!(matches!(
            open_identity(
                &serde_json::to_vec(&value).unwrap(),
                "https://peppy.test",
                "wrong"
            ),
            Err(IdentityError::Invalid)
        ));
        let mut nested: Value = serde_json::from_slice(&serialized).unwrap();
        nested["metadata"]["profile"]["unknown"] = serde_json::json!(true);
        assert!(matches!(
            open_identity(
                &serde_json::to_vec(&nested).unwrap(),
                "https://peppy.test",
                "wrong"
            ),
            Err(IdentityError::Invalid)
        ));
        let duplicate = String::from_utf8(serialized.clone()).unwrap().replacen(
            "\"metadata\":",
            "\"metadata\":{},\"metadata\":",
            1,
        );
        assert!(matches!(
            open_identity(duplicate.as_bytes(), "https://peppy.test", "wrong"),
            Err(IdentityError::Invalid)
        ));
        assert!(matches!(
            open_identity(
                &vec![b' '; MAX_ENCODED_BYTES + 1],
                "https://peppy.test",
                "wrong"
            ),
            Err(IdentityError::Invalid)
        ));
    }

    #[test]
    fn rejects_invalid_metadata_and_secret_inputs() {
        let secrets = secrets();
        assert!(matches!(
            IdentitySecrets::new([0; DATABASE_KEY_BYTES], "bad".into()),
            Err(IdentityError::Invalid)
        ));
        assert!(matches!(
            decode_secrets(&[0; PLAINTEXT_BYTES - 1]),
            Err(IdentityError::Invalid)
        ));
        let mut malformed = [b'0'; PLAINTEXT_BYTES];
        malformed[DATABASE_KEY_BYTES] = b'z';
        assert!(matches!(
            decode_secrets(&malformed),
            Err(IdentityError::Invalid)
        ));
        let mut invalid = metadata();
        invalid.origin = "http://peppy.test".into();
        assert!(matches!(
            seal_identity(invalid, &secrets, PHRASE),
            Err(IdentityError::Invalid)
        ));
        let mut invalid = metadata();
        invalid.profile.vault_id = Uuid::new_v4();
        assert!(matches!(
            seal_identity(invalid, &secrets, PHRASE),
            Err(IdentityError::Invalid)
        ));
    }

    #[test]
    fn explicit_rewrap_preserves_secrets_and_rejects_the_old_phrase() {
        let (mut metadata, serialized) = sealed();
        let unlocked = open_identity(&serialized, "https://peppy.test", PHRASE).unwrap();
        let new_profile = KeyProfile::new(metadata.vault_id, 2).unwrap();
        let new_root = derive_root_key("new correct horse battery staple", &new_profile).unwrap();
        metadata.profile = new_profile.clone();
        metadata.header = create_vault_check_header(&new_root, new_profile).unwrap();
        let active_secrets = unlocked.into_secrets();
        assert!(matches!(
            seal_identity(metadata.clone(), &active_secrets, "wrong new phrase"),
            Err(IdentityError::AuthFailed)
        ));
        active_secrets.with_database_key(|key| assert_eq!(key, &[0xA5; DATABASE_KEY_BYTES]));
        active_secrets.with_device_token(|token| assert_eq!(token, TOKEN));
        let rewrapped = seal_identity(
            metadata,
            &active_secrets,
            "new correct horse battery staple",
        )
        .unwrap();
        let reserialized = serde_json::to_vec(&rewrapped).unwrap();
        assert!(matches!(
            open_identity(&reserialized, "https://peppy.test", PHRASE),
            Err(IdentityError::AuthFailed)
        ));
        let reopened = open_identity(
            &reserialized,
            "https://peppy.test",
            "new correct horse battery staple",
        )
        .unwrap();
        reopened
            .secrets()
            .with_database_key(|key| assert_eq!(key, &[0xA5; DATABASE_KEY_BYTES]));
        reopened
            .secrets()
            .with_device_token(|token| assert_eq!(token, TOKEN));
    }
}
