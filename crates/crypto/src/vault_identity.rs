use crate::{
    CryptoError, EncryptedEnvelope, KeyProfile, KeyPurpose, PurposeKey, RootKey, decrypt,
    derive_purpose_key, encrypt, profile_fingerprint_bytes,
};
use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use uuid::Uuid;
use zeroize::{Zeroize, Zeroizing};

const ANCHOR_DOMAIN: &[u8] = b"peppy-vault-identity-anchor-v1\0";
const COMMITMENT_DOMAIN: &[u8] = b"peppy-vault-identity-commitment-v1\0";
const PIN_DOMAIN: &[u8] = b"peppy-vault-identity-anchor-pin-v1\0";
const RECOVERY_DOMAIN: &[u8] = b"peppy-vault-identity-recovery-v1\0";
const RCS_SALT_DOMAIN: &[u8] = b"peppy-vault-identity-keys-v1\0";
const RCS_INFO: &[u8] = b"peppy-rcs-message-id-v1";
const MESSAGE_DOMAIN: &[u8] = b"peppy-compaction-hmac-v1\0";
const MESSAGE_LABEL: &[u8] = b"rcs-message-id-v2";
const ANCHOR_BYTES: usize = 147;
const RECOVERY_BYTES: usize = 266;
const RECOVERY_CIPHERTEXT_BYTES: usize = ANCHOR_BYTES + 16;
const MAX_GOOGLE_ID_BYTES: usize = 512;

/// ```compile_fail
/// use peppy_crypto::vault_identity::VaultIdentityAnchor;
///
/// fn leaks(anchor: &VaultIdentityAnchor) {
///     println!("{anchor:?}");
/// }
/// ```
///
/// ```compile_fail
/// use peppy_crypto::vault_identity::VaultIdentityAnchor;
/// use serde::Serialize;
///
/// fn serializes<T: Serialize>() {}
/// fn leaks(_: &VaultIdentityAnchor) {
///     serializes::<VaultIdentityAnchor>();
/// }
/// ```
pub struct VaultIdentityAnchor {
    bytes: Zeroizing<Vec<u8>>,
}

impl VaultIdentityAnchor {
    pub fn generate(vault: Uuid, operation: Uuid, owner: Uuid) -> Result<Self, CryptoError> {
        validate_uuid(vault)?;
        validate_uuid(operation)?;
        validate_uuid(owner)?;
        crate::ensure_sodium()?;
        let mut seed = Zeroizing::new(vec![0; 32]);
        libsodium_rs::random::fill_bytes(&mut seed);
        let anchor = Self::from_seed(vault, operation, owner, &seed)?;
        seed.zeroize();
        Ok(anchor)
    }

    /// Imports protected bytes after validating their shape, vault binding, and commitment.
    /// This validates storage integrity only; it is not enrollment, bootstrap, or root authority.
    pub fn from_protected_bytes(bytes: &[u8], expected_vault: Uuid) -> Result<Self, CryptoError> {
        validate_uuid(expected_vault)?;
        if bytes.len() != ANCHOR_BYTES || !bytes.starts_with(ANCHOR_DOMAIN) {
            return Err(CryptoError::InvalidCiphertext);
        }
        let mut offset = ANCHOR_DOMAIN.len();
        if take_u16(bytes, &mut offset)? != 1 || take_u16(bytes, &mut offset)? != 1 {
            return Err(CryptoError::UnsupportedSuite);
        }
        let vault = take_uuid(bytes, &mut offset)?;
        let operation = take_uuid(bytes, &mut offset)?;
        let owner = take_uuid(bytes, &mut offset)?;
        validate_uuid(vault)?;
        validate_uuid(operation)?;
        validate_uuid(owner)?;
        if vault != expected_vault {
            return Err(CryptoError::InvalidProfile);
        }
        let seed = &bytes[offset..offset + 32];
        offset += 32;
        let commitment = &bytes[offset..offset + 32];
        if commitment != commitment_for(vault, seed) || offset + 32 != bytes.len() {
            return Err(CryptoError::AuthenticationFailed);
        }
        Ok(Self {
            bytes: Zeroizing::new(bytes.to_vec()),
        })
    }

    pub fn with_protected_bytes<T>(&self, operation: impl FnOnce(&[u8]) -> T) -> T {
        operation(&self.bytes)
    }

    pub fn pin(&self) -> VaultIdentityPin {
        let mut digest = Sha256::new();
        digest.update(PIN_DOMAIN);
        digest.update(&self.bytes);
        VaultIdentityPin(digest.finalize().into())
    }

    pub fn derive_rcs_key(&self) -> Result<RcsIdentityKey, CryptoError> {
        let vault = self.vault()?;
        let seed = self.seed()?;
        let mut salt = Vec::with_capacity(RCS_SALT_DOMAIN.len() + 16);
        salt.extend_from_slice(RCS_SALT_DOMAIN);
        salt.extend_from_slice(vault.as_bytes());
        let hkdf = Hkdf::<Sha256>::new(Some(&salt), seed);
        let mut bytes = [0; 32];
        hkdf.expand(RCS_INFO, &mut bytes)
            .map_err(|_| CryptoError::OperationFailed)?;
        Ok(RcsIdentityKey(bytes))
    }

    fn from_seed(
        vault: Uuid,
        operation: Uuid,
        owner: Uuid,
        seed: &[u8],
    ) -> Result<Self, CryptoError> {
        if seed.len() != 32 {
            return Err(CryptoError::OperationFailed);
        }
        let commitment = commitment_for(vault, seed);
        let mut bytes = Zeroizing::new(Vec::with_capacity(ANCHOR_BYTES));
        bytes.extend_from_slice(ANCHOR_DOMAIN);
        bytes.extend_from_slice(&1u16.to_be_bytes());
        bytes.extend_from_slice(&1u16.to_be_bytes());
        bytes.extend_from_slice(vault.as_bytes());
        bytes.extend_from_slice(operation.as_bytes());
        bytes.extend_from_slice(owner.as_bytes());
        bytes.extend_from_slice(seed);
        bytes.extend_from_slice(&commitment);
        Ok(Self { bytes })
    }

    fn vault(&self) -> Result<Uuid, CryptoError> {
        Uuid::from_slice(&self.bytes[ANCHOR_DOMAIN.len() + 4..ANCHOR_DOMAIN.len() + 20])
            .map_err(|_| CryptoError::InvalidCiphertext)
    }

    fn seed(&self) -> Result<&[u8], CryptoError> {
        self.bytes
            .get(ANCHOR_DOMAIN.len() + 52..ANCHOR_DOMAIN.len() + 84)
            .ok_or(CryptoError::InvalidCiphertext)
    }
}

pub struct VaultIdentityPin([u8; 32]);

impl VaultIdentityPin {
    pub fn with_bytes<T>(&self, operation: impl FnOnce(&[u8; 32]) -> T) -> T {
        operation(&self.0)
    }
}

impl Zeroize for VaultIdentityPin {
    fn zeroize(&mut self) {
        self.0.zeroize();
    }
}

impl Drop for VaultIdentityPin {
    fn drop(&mut self) {
        self.zeroize();
    }
}

pub struct VaultIdentityRecoveryRecord {
    profile: KeyProfile,
    envelope: EncryptedEnvelope,
}

impl VaultIdentityRecoveryRecord {
    pub fn wrap(
        anchor: &VaultIdentityAnchor,
        key: &PurposeKey,
        profile: &KeyProfile,
    ) -> Result<Self, CryptoError> {
        validate_profile(profile)?;
        validate_recovery_key(key, profile)?;
        if anchor.vault()? != profile.vault_id {
            return Err(CryptoError::InvalidProfile);
        }
        let envelope = encrypt(key, &recovery_aad(profile)?, &anchor.bytes)?;
        Ok(Self {
            profile: profile.clone(),
            envelope,
        })
    }

    pub fn open(
        &self,
        key: &PurposeKey,
        expected_profile: &KeyProfile,
        expected_vault: Uuid,
    ) -> Result<VaultIdentityAnchor, CryptoError> {
        validate_uuid(expected_vault)?;
        validate_profile(expected_profile)?;
        if &self.profile != expected_profile || expected_profile.vault_id != expected_vault {
            return Err(CryptoError::InvalidProfile);
        }
        validate_recovery_key(key, expected_profile)?;
        let plaintext = Zeroizing::new(decrypt(
            key,
            &recovery_aad(expected_profile)?,
            &self.envelope,
        )?);
        VaultIdentityAnchor::from_protected_bytes(&plaintext, expected_vault)
    }

    /// Validates public recovery framing only. Callers must authenticate with [`Self::open`]
    /// under a verified root/profile before treating a decoded record as authoritative.
    pub fn decode(bytes: &[u8]) -> Result<Self, CryptoError> {
        if bytes.len() != RECOVERY_BYTES || !bytes.starts_with(RECOVERY_DOMAIN) {
            return Err(CryptoError::InvalidCiphertext);
        }
        let mut offset = RECOVERY_DOMAIN.len();
        if take_u16(bytes, &mut offset)? != 1 || take_u16(bytes, &mut offset)? != 1 {
            return Err(CryptoError::UnsupportedSuite);
        }
        let profile = decode_profile(bytes, &mut offset)?;
        validate_profile(&profile)?;
        let nonce: [u8; 24] = bytes[offset..offset + 24]
            .try_into()
            .map_err(|_| CryptoError::InvalidCiphertext)?;
        offset += 24;
        let ciphertext_length = take_u32(bytes, &mut offset)? as usize;
        if ciphertext_length != RECOVERY_CIPHERTEXT_BYTES
            || offset + ciphertext_length != bytes.len()
        {
            return Err(CryptoError::InvalidCiphertext);
        }
        Ok(Self {
            profile,
            envelope: EncryptedEnvelope {
                nonce,
                ciphertext: bytes[offset..].to_vec(),
            },
        })
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(RECOVERY_BYTES);
        bytes.extend_from_slice(RECOVERY_DOMAIN);
        bytes.extend_from_slice(&1u16.to_be_bytes());
        bytes.extend_from_slice(&1u16.to_be_bytes());
        encode_profile(&self.profile, &mut bytes);
        bytes.extend_from_slice(&self.envelope.nonce);
        bytes.extend_from_slice(&(self.envelope.ciphertext.len() as u32).to_be_bytes());
        bytes.extend_from_slice(&self.envelope.ciphertext);
        bytes
    }

    pub fn profile(&self) -> &KeyProfile {
        &self.profile
    }
}

pub fn derive_vault_identity_recovery_key(
    root: &RootKey,
    profile: &KeyProfile,
) -> Result<PurposeKey, CryptoError> {
    validate_profile(profile)?;
    derive_purpose_key(root, profile, KeyPurpose::VaultIdentityRecovery)
}

pub struct RcsIdentityKey([u8; 32]);

impl RcsIdentityKey {
    pub fn message_identity(
        &self,
        scope: Uuid,
        conversation_id: &str,
        message_id: &str,
    ) -> Result<RcsMessageIdentity, CryptoError> {
        validate_uuid(scope)?;
        validate_google_id(conversation_id)?;
        validate_google_id(message_id)?;
        let scope = scope.hyphenated().to_string();
        let mut mac =
            Hmac::<Sha256>::new_from_slice(&self.0).map_err(|_| CryptoError::OperationFailed)?;
        mac.update(MESSAGE_DOMAIN);
        update_frame(&mut mac, MESSAGE_LABEL)?;
        update_frame(&mut mac, scope.as_bytes())?;
        update_frame(&mut mac, conversation_id.as_bytes())?;
        update_frame(&mut mac, message_id.as_bytes())?;
        let digest: [u8; 32] = mac.finalize().into_bytes().into();
        let mut uuid_bytes: [u8; 16] = digest[..16].try_into().expect("digest has 32 bytes");
        uuid_bytes[6] = (uuid_bytes[6] & 0x0f) | 0x80;
        uuid_bytes[8] = (uuid_bytes[8] & 0x3f) | 0x80;
        Ok(RcsMessageIdentity {
            digest,
            uuid: Uuid::from_bytes(uuid_bytes),
        })
    }
}

impl Zeroize for RcsIdentityKey {
    fn zeroize(&mut self) {
        self.0.zeroize();
    }
}

impl Drop for RcsIdentityKey {
    fn drop(&mut self) {
        self.zeroize();
    }
}

pub struct RcsMessageIdentity {
    digest: [u8; 32],
    uuid: Uuid,
}

impl RcsMessageIdentity {
    pub fn digest(&self) -> &[u8; 32] {
        &self.digest
    }

    pub fn uuid(&self) -> Uuid {
        self.uuid
    }
}

fn commitment_for(vault: Uuid, seed: &[u8]) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(COMMITMENT_DOMAIN);
    digest.update(1u16.to_be_bytes());
    digest.update(vault.as_bytes());
    digest.update(seed);
    digest.finalize().into()
}

fn validate_recovery_key(key: &PurposeKey, profile: &KeyProfile) -> Result<(), CryptoError> {
    if key.purpose() != KeyPurpose::VaultIdentityRecovery
        || key.profile_fingerprint != profile_fingerprint_bytes(profile)?
    {
        return Err(CryptoError::AuthenticationFailed);
    }
    Ok(())
}

fn recovery_aad(profile: &KeyProfile) -> Result<Vec<u8>, CryptoError> {
    validate_profile(profile)?;
    let mut aad = Vec::with_capacity(RECOVERY_DOMAIN.len() + 42);
    aad.extend_from_slice(RECOVERY_DOMAIN);
    aad.extend_from_slice(&1u16.to_be_bytes());
    aad.extend_from_slice(&1u16.to_be_bytes());
    encode_profile(profile, &mut aad);
    Ok(aad)
}

fn validate_profile(profile: &KeyProfile) -> Result<(), CryptoError> {
    profile.validate()?;
    validate_uuid(profile.vault_id)?;
    if profile.key_epoch == 0 {
        return Err(CryptoError::InvalidProfile);
    }
    Ok(())
}

fn validate_uuid(value: Uuid) -> Result<(), CryptoError> {
    if value.is_nil() {
        Err(CryptoError::InvalidProfile)
    } else {
        Ok(())
    }
}

fn validate_google_id(value: &str) -> Result<(), CryptoError> {
    if value.is_empty() || value.len() > MAX_GOOGLE_ID_BYTES {
        Err(CryptoError::InvalidCiphertext)
    } else {
        Ok(())
    }
}

fn update_frame(mac: &mut Hmac<Sha256>, value: &[u8]) -> Result<(), CryptoError> {
    let length = u32::try_from(value.len()).map_err(|_| CryptoError::InvalidCiphertext)?;
    mac.update(&length.to_be_bytes());
    mac.update(value);
    Ok(())
}

fn encode_profile(profile: &KeyProfile, output: &mut Vec<u8>) {
    output.extend_from_slice(&profile.crypto_suite.to_be_bytes());
    output.extend_from_slice(&profile.salt);
    output.extend_from_slice(profile.vault_id.as_bytes());
    output.extend_from_slice(&profile.key_epoch.to_be_bytes());
}

fn decode_profile(bytes: &[u8], offset: &mut usize) -> Result<KeyProfile, CryptoError> {
    let crypto_suite = take_u16(bytes, offset)?;
    let salt = take(bytes, offset, 16)?
        .try_into()
        .map_err(|_| CryptoError::InvalidCiphertext)?;
    let vault_id = take_uuid(bytes, offset)?;
    let key_epoch = take_u32(bytes, offset)?;
    Ok(KeyProfile {
        crypto_suite,
        salt,
        vault_id,
        key_epoch,
    })
}

fn take_uuid(bytes: &[u8], offset: &mut usize) -> Result<Uuid, CryptoError> {
    Uuid::from_slice(take(bytes, offset, 16)?).map_err(|_| CryptoError::InvalidCiphertext)
}

fn take_u16(bytes: &[u8], offset: &mut usize) -> Result<u16, CryptoError> {
    Ok(u16::from_be_bytes(
        take(bytes, offset, 2)?
            .try_into()
            .map_err(|_| CryptoError::InvalidCiphertext)?,
    ))
}

fn take_u32(bytes: &[u8], offset: &mut usize) -> Result<u32, CryptoError> {
    Ok(u32::from_be_bytes(
        take(bytes, offset, 4)?
            .try_into()
            .map_err(|_| CryptoError::InvalidCiphertext)?,
    ))
}

fn take<'a>(bytes: &'a [u8], offset: &mut usize, length: usize) -> Result<&'a [u8], CryptoError> {
    let end = offset
        .checked_add(length)
        .ok_or(CryptoError::InvalidCiphertext)?;
    let value = bytes
        .get(*offset..end)
        .ok_or(CryptoError::InvalidCiphertext)?;
    *offset = end;
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    const VAULT: &str = "00112233-4455-6677-8899-aabbccddeeff";
    const ANCHOR: &str = "70657070792d7661756c742d6964656e746974792d616e63686f722d7631000001000100112233445566778899aabbccddeeff102132435465768798a9bacbdcedfe0fffeeddccbbaa99887766554433221100000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f918a360d58325ba711dc0f6f14c31c939ba441c0dfd05a33a8e44cbd7fe7a490";
    const RECOVERY_RECORD: &str = "70657070792d7661756c742d6964656e746974792d7265636f766572792d763100000100010001101112131415161718191a1b1c1d1e1f00112233445566778899aabbccddeeff01020304202122232425262728292a2b2c2d2e2f3031323334353637000000a3404142434445464748494a4b4c4d4e4f505152535455565758595a5b5c5d5e5f606162636465666768696a6b6c6d6e6f707172737475767778797a7b7c7d7e7f808182838485868788898a8b8c8d8e8f909192939495969798999a9b9c9d9e9fa0a1a2a3a4a5a6a7a8a9aaabacadaeafb0b1b2b3b4b5b6b7b8b9babbbcbdbebfc0c1c2c3c4c5c6c7c8c9cacbcccdcecfd0d1d2d3d4d5d6d7d8d9dadbdcdddedfe0e1e2";

    fn hex(value: &str) -> Vec<u8> {
        (0..value.len())
            .step_by(2)
            .map(|index| u8::from_str_radix(&value[index..index + 2], 16).unwrap())
            .collect()
    }

    #[test]
    fn protected_anchor_and_rcs_key_match_independent_vectors() {
        let vault = Uuid::parse_str(VAULT).unwrap();
        let anchor = VaultIdentityAnchor::from_protected_bytes(&hex(ANCHOR), vault).unwrap();
        let mut protected = Vec::new();
        anchor.with_protected_bytes(|bytes| protected.extend_from_slice(bytes));
        assert_eq!(protected, hex(ANCHOR));
        anchor.pin().with_bytes(|pin| {
            assert_eq!(
                pin,
                hex("1ddc83d60497c51dbc09920b44f29a133666b3562a8e9bbb05ce698f2691b0a7").as_slice()
            );
        });

        let key = anchor.derive_rcs_key().unwrap();
        assert_eq!(
            key.0,
            hex("bf5682db09c34abba5c7f006f584190e6c1b0bfde552d0211d2817e083a44e45").as_slice()
        );
        let identity = key
            .message_identity(
                Uuid::parse_str("12345678-1234-5678-9abc-def012345678").unwrap(),
                "conversation/α",
                "msg-42",
            )
            .unwrap();
        assert_eq!(
            identity.digest(),
            hex("f19ab0ba7d10dfea6890deef44d160fcb8fa7b46d25c71d8584543f815f637ce").as_slice()
        );
        assert_eq!(
            identity.uuid(),
            Uuid::parse_str("f19ab0ba-7d10-8fea-a890-deef44d160fc").unwrap()
        );
    }

    #[test]
    fn independent_recovery_record_codec_vector_round_trips_exactly() {
        let bytes = hex(RECOVERY_RECORD);
        let record = VaultIdentityRecoveryRecord::decode(&bytes).unwrap();

        assert_eq!(bytes.len(), RECOVERY_BYTES);
        assert_eq!(
            record.profile(),
            &KeyProfile {
                crypto_suite: 1,
                salt: [
                    0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x1b, 0x1c,
                    0x1d, 0x1e, 0x1f,
                ],
                vault_id: Uuid::parse_str(VAULT).unwrap(),
                key_epoch: 0x0102_0304,
            }
        );
        assert_eq!(record.encode(), bytes);
    }

    fn profile(epoch: u32) -> KeyProfile {
        KeyProfile {
            crypto_suite: 1,
            salt: [7; 16],
            vault_id: Uuid::parse_str(VAULT).unwrap(),
            key_epoch: epoch,
        }
    }

    #[test]
    fn authenticated_recovery_record_rewraps_the_same_anchor() {
        let first_profile = profile(1);
        let root = crate::derive_root_key("identity test phrase", &first_profile).unwrap();
        let anchor =
            VaultIdentityAnchor::from_protected_bytes(&hex(ANCHOR), first_profile.vault_id)
                .unwrap();
        let first_key = derive_vault_identity_recovery_key(&root, &first_profile).unwrap();
        let record =
            VaultIdentityRecoveryRecord::wrap(&anchor, &first_key, &first_profile).unwrap();
        let decoded = VaultIdentityRecoveryRecord::decode(&record.encode()).unwrap();
        let opened = decoded
            .open(&first_key, &first_profile, first_profile.vault_id)
            .unwrap();

        let second_profile = profile(2);
        let second_key = derive_vault_identity_recovery_key(&root, &second_profile).unwrap();
        let rewrapped =
            VaultIdentityRecoveryRecord::wrap(&opened, &second_key, &second_profile).unwrap();
        let rewrapped_anchor = rewrapped
            .open(&second_key, &second_profile, second_profile.vault_id)
            .unwrap();
        let mut original = Vec::new();
        let mut recovered = Vec::new();
        anchor.with_protected_bytes(|bytes| original.extend_from_slice(bytes));
        rewrapped_anchor.with_protected_bytes(|bytes| recovered.extend_from_slice(bytes));
        assert_eq!(recovered, original);
    }

    #[test]
    fn recovery_rejects_wrong_key_context_and_malformed_records() {
        let profile = profile(1);
        let root = crate::derive_root_key("identity test phrase", &profile).unwrap();
        let anchor =
            VaultIdentityAnchor::from_protected_bytes(&hex(ANCHOR), profile.vault_id).unwrap();
        let recovery_key = derive_vault_identity_recovery_key(&root, &profile).unwrap();
        let record = VaultIdentityRecoveryRecord::wrap(&anchor, &recovery_key, &profile).unwrap();
        let header_key = crate::derive_purpose_key(&root, &profile, KeyPurpose::Header).unwrap();
        assert!(VaultIdentityRecoveryRecord::wrap(&anchor, &header_key, &profile).is_err());
        assert!(
            record
                .open(&header_key, &profile, profile.vault_id)
                .is_err()
        );
        assert!(
            record
                .open(&recovery_key, &profile, Uuid::new_v4())
                .is_err()
        );

        let encoded = record.encode();
        for length in 0..encoded.len() {
            assert!(VaultIdentityRecoveryRecord::decode(&encoded[..length]).is_err());
        }
        let mut trailing = encoded.clone();
        trailing.push(0);
        assert!(VaultIdentityRecoveryRecord::decode(&trailing).is_err());
        let mut forged_length = encoded;
        let length_offset = RECOVERY_DOMAIN.len() + 4 + 38 + 24;
        forged_length[length_offset..length_offset + 4].copy_from_slice(&1u32.to_be_bytes());
        assert!(VaultIdentityRecoveryRecord::decode(&forged_length).is_err());
    }

    #[test]
    fn commitment_and_message_scope_boundaries_fail_closed() {
        let vault = Uuid::parse_str(VAULT).unwrap();
        let mut corrupted = hex(ANCHOR);
        corrupted[ANCHOR_BYTES - 1] ^= 1;
        assert!(VaultIdentityAnchor::from_protected_bytes(&corrupted, vault).is_err());

        let anchor = VaultIdentityAnchor::from_protected_bytes(&hex(ANCHOR), vault).unwrap();
        let key = anchor.derive_rcs_key().unwrap();
        let scope = Uuid::parse_str("12345678-1234-5678-9abc-def012345678").unwrap();
        let first = key
            .message_identity(scope, "conversation", "message")
            .unwrap();
        let same = key
            .message_identity(scope, "conversation", "message")
            .unwrap();
        let other_scope = key
            .message_identity(Uuid::new_v4(), "conversation", "message")
            .unwrap();
        assert_eq!(first.digest(), same.digest());
        assert_ne!(first.digest(), other_scope.digest());
        assert!(
            key.message_identity(Uuid::nil(), "conversation", "message")
                .is_err()
        );
        assert!(key.message_identity(scope, "", "message").is_err());
        assert!(
            key.message_identity(scope, &"x".repeat(513), "message")
                .is_err()
        );
        assert!(
            key.message_identity(scope, &"x".repeat(512), "message")
                .is_ok()
        );
        assert!(key.message_identity(scope, "conversation", "").is_err());
    }

    #[test]
    fn independent_recovery_context_and_wire_lengths_are_pinned() {
        let profile = profile(1);
        let root = crate::derive_root_key("identity test phrase", &profile).unwrap();
        let key = derive_vault_identity_recovery_key(&root, &profile).unwrap();
        assert_eq!(key.purpose(), KeyPurpose::VaultIdentityRecovery);
        assert_eq!(
            recovery_aad(&profile).unwrap(),
            hex(
                "70657070792d7661756c742d6964656e746974792d7265636f766572792d7631000001000100010707070707070707070707070707070700112233445566778899aabbccddeeff00000001"
            )
        );
        assert_eq!(
            crate::purpose_aad(&key, &recovery_aad(&profile).unwrap()),
            hex(
                "70657070792d616561642d763100070235e6f432e9d240a13eb5635ab4508cd8e7d077f042424c7f11262037bab4eb70657070792d7661756c742d6964656e746974792d7265636f766572792d7631000001000100010707070707070707070707070707070700112233445566778899aabbccddeeff00000001"
            )
        );
        let anchor =
            VaultIdentityAnchor::from_protected_bytes(&hex(ANCHOR), profile.vault_id).unwrap();
        assert_eq!(hex(ANCHOR).len(), ANCHOR_BYTES);
        assert_eq!(
            VaultIdentityRecoveryRecord::wrap(&anchor, &key, &profile)
                .unwrap()
                .encode()
                .len(),
            RECOVERY_BYTES
        );
    }

    #[test]
    fn authenticated_decrypt_still_rejects_bad_anchor_context() {
        let profile = profile(1);
        let root = crate::derive_root_key("identity test phrase", &profile).unwrap();
        let key = derive_vault_identity_recovery_key(&root, &profile).unwrap();
        let mut malformed_commitment = hex(ANCHOR);
        malformed_commitment[ANCHOR_BYTES - 1] ^= 1;
        let record = VaultIdentityRecoveryRecord {
            profile: profile.clone(),
            envelope: encrypt(
                &key,
                &recovery_aad(&profile).unwrap(),
                &malformed_commitment,
            )
            .unwrap(),
        };
        assert!(record.open(&key, &profile, profile.vault_id).is_err());

        let mut wrong_vault = hex(ANCHOR);
        let different_vault = Uuid::parse_str("11112233-4455-6677-8899-aabbccddeeff").unwrap();
        let vault_offset = ANCHOR_DOMAIN.len() + 4;
        wrong_vault[vault_offset..vault_offset + 16].copy_from_slice(different_vault.as_bytes());
        let seed_offset = vault_offset + 48;
        let commitment_offset = seed_offset + 32;
        let commitment = commitment_for(
            different_vault,
            &wrong_vault[seed_offset..commitment_offset],
        );
        wrong_vault[commitment_offset..].copy_from_slice(&commitment);
        let record = VaultIdentityRecoveryRecord {
            profile: profile.clone(),
            envelope: encrypt(&key, &recovery_aad(&profile).unwrap(), &wrong_vault).unwrap(),
        };
        assert!(record.open(&key, &profile, profile.vault_id).is_err());
    }

    #[test]
    fn recovery_rejects_versions_profiles_and_aead_tampering() {
        let profile = profile(1);
        let root = crate::derive_root_key("identity test phrase", &profile).unwrap();
        let anchor =
            VaultIdentityAnchor::from_protected_bytes(&hex(ANCHOR), profile.vault_id).unwrap();
        let key = derive_vault_identity_recovery_key(&root, &profile).unwrap();
        let record = VaultIdentityRecoveryRecord::wrap(&anchor, &key, &profile).unwrap();
        let encoded = record.encode();
        for offset in [RECOVERY_DOMAIN.len(), RECOVERY_DOMAIN.len() + 2] {
            let mut unknown = encoded.clone();
            unknown[offset + 1] = 2;
            assert!(VaultIdentityRecoveryRecord::decode(&unknown).is_err());
        }
        let profile_offset = RECOVERY_DOMAIN.len() + 4;
        let mut unsupported_profile_suite = encoded.clone();
        unsupported_profile_suite[profile_offset + 1] = 2;
        assert!(VaultIdentityRecoveryRecord::decode(&unsupported_profile_suite).is_err());
        let mut nil_vault = encoded.clone();
        nil_vault[profile_offset + 18..profile_offset + 34].fill(0);
        assert!(VaultIdentityRecoveryRecord::decode(&nil_vault).is_err());
        let mut zero_epoch = encoded.clone();
        zero_epoch[profile_offset + 34..profile_offset + 38].fill(0);
        assert!(VaultIdentityRecoveryRecord::decode(&zero_epoch).is_err());
        let mut nonce_tampered = record.encode();
        nonce_tampered[RECOVERY_DOMAIN.len() + 4 + 38] ^= 1;
        let nonce_tampered = VaultIdentityRecoveryRecord::decode(&nonce_tampered).unwrap();
        assert!(
            nonce_tampered
                .open(&key, &profile, profile.vault_id)
                .is_err()
        );
        let mut cipher_tampered = record.encode();
        *cipher_tampered.last_mut().unwrap() ^= 1;
        let cipher_tampered = VaultIdentityRecoveryRecord::decode(&cipher_tampered).unwrap();
        assert!(
            cipher_tampered
                .open(&key, &profile, profile.vault_id)
                .is_err()
        );
        let mut profile_tampered = record.encode();
        profile_tampered[profile_offset + 2] ^= 1;
        let profile_tampered = VaultIdentityRecoveryRecord::decode(&profile_tampered).unwrap();
        assert!(
            profile_tampered
                .open(&key, profile_tampered.profile(), profile.vault_id)
                .is_err()
        );

        let mut other_profile = profile.clone();
        other_profile.salt[0] ^= 1;
        let wrong_key = derive_vault_identity_recovery_key(&root, &other_profile).unwrap();
        assert!(VaultIdentityRecoveryRecord::wrap(&anchor, &wrong_key, &profile).is_err());
        assert!(record.open(&wrong_key, &profile, profile.vault_id).is_err());
        let mut wrong_epoch = profile.clone();
        wrong_epoch.key_epoch += 1;
        assert!(record.open(&key, &wrong_epoch, profile.vault_id).is_err());
    }

    #[test]
    fn anchor_and_message_boundaries_reject_ambiguous_or_invalid_inputs() {
        let vault = Uuid::parse_str(VAULT).unwrap();
        let mut unsupported_anchor = hex(ANCHOR);
        unsupported_anchor[ANCHOR_DOMAIN.len() + 1] = 2;
        assert!(VaultIdentityAnchor::from_protected_bytes(&unsupported_anchor, vault).is_err());
        let mut unknown_suite_anchor = hex(ANCHOR);
        unknown_suite_anchor[ANCHOR_DOMAIN.len() + 3] = 2;
        assert!(VaultIdentityAnchor::from_protected_bytes(&unknown_suite_anchor, vault).is_err());
        assert!(VaultIdentityAnchor::from_protected_bytes(&hex(ANCHOR), Uuid::nil()).is_err());
        assert!(VaultIdentityAnchor::generate(vault, Uuid::nil(), Uuid::new_v4()).is_err());
        assert!(VaultIdentityAnchor::generate(vault, Uuid::new_v4(), Uuid::nil()).is_err());

        let key = VaultIdentityAnchor::from_protected_bytes(&hex(ANCHOR), vault)
            .unwrap()
            .derive_rcs_key()
            .unwrap();
        let scope = Uuid::parse_str("12345678-1234-5678-9abc-def012345678").unwrap();
        assert_ne!(
            key.message_identity(scope, "ab", "c").unwrap().digest(),
            key.message_identity(scope, "a", "bc").unwrap().digest()
        );
        let unicode_512 = "é".repeat(256);
        let unicode_513 = format!("{}x", unicode_512);
        assert!(
            key.message_identity(scope, &unicode_512, &unicode_512)
                .is_ok()
        );
        assert!(
            key.message_identity(scope, &unicode_513, "message")
                .is_err()
        );
        assert!(
            key.message_identity(scope, "conversation", &unicode_513)
                .is_err()
        );
    }
}
