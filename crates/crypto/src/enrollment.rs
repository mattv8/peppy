//! Canonical enrollment proofs and authenticated root-key handoff.

use crate::{
    CryptoError, EncryptedEnvelope, KeyProfile, RootKey, VaultCheckHeader, ensure_sodium,
    verify_vault_check_header,
};
use hmac::{Hmac, Mac};
use libsodium_rs::{crypto_box, crypto_kdf, crypto_sign};
use sha2::{Digest, Sha256};
use uuid::Uuid;
use zeroize::Zeroizing;

pub const ENROLLMENT_VERSION: u8 = 1;
pub const TRANSFER_NONCE_BYTES: usize = 24;
pub const ENROLLMENT_CONTEXT_DOMAIN: &[u8] = b"peppy-enrollment-v1";
pub const DEVICE_PROOF_DOMAIN: &[u8] = b"peppy-device-enrollment-v1";
pub const PHONE_CLAIM_DOMAIN: &[u8] = b"peppy-phone-claim-v1";
const HANDOFF_VERSION: u8 = 1;
const HANDOFF_MAX_BYTES: usize = 16 * 1024;
const MAX_CANONICAL_ORIGIN_BYTES: usize = 2_048;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EnrollmentPurpose {
    Manual,
    Phone,
}
impl EnrollmentPurpose {
    fn bytes(self) -> &'static [u8] {
        match self {
            Self::Manual => b"manual",
            Self::Phone => b"phone",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EnrollmentRole {
    Owner,
    Device,
    Gateway,
}
impl EnrollmentRole {
    fn bytes(self) -> &'static [u8] {
        match self {
            Self::Owner => b"owner",
            Self::Device => b"device",
            Self::Gateway => b"gateway",
        }
    }
}

/// Every field which can vary in length is explicitly framed, so the signed transcript is unique.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EnrollmentContext {
    pub purpose: EnrollmentPurpose,
    pub canonical_origin: String,
    pub vault_id: Uuid,
    pub fingerprint: [u8; 32],
    pub key_epoch: u32,
    pub enrollment_id: Uuid,
    pub server_nonce: [u8; 32],
    pub owner_transfer_public_key: [u8; 32],
    pub recipient_transfer_public_key: [u8; 32],
    pub recipient_signing_public_key: [u8; 32],
    pub device_id: Uuid,
    pub credential_digest: [u8; 32],
    pub role: EnrollmentRole,
}
impl EnrollmentContext {
    pub fn canonical_bytes(&self) -> Result<Vec<u8>, CryptoError> {
        let origin = self.canonical_origin.as_bytes();
        if origin.is_empty() || origin.len() > MAX_CANONICAL_ORIGIN_BYTES {
            return Err(CryptoError::InvalidEnrollment);
        }
        let mut out = Vec::with_capacity(256 + origin.len());
        framed(&mut out, ENROLLMENT_CONTEXT_DOMAIN)?;
        out.push(ENROLLMENT_VERSION);
        framed(&mut out, self.purpose.bytes())?;
        framed(&mut out, origin)?;
        out.extend_from_slice(self.vault_id.as_bytes());
        out.extend_from_slice(&self.fingerprint);
        out.extend_from_slice(&self.key_epoch.to_be_bytes());
        out.extend_from_slice(self.enrollment_id.as_bytes());
        out.extend_from_slice(&self.server_nonce);
        out.extend_from_slice(&self.owner_transfer_public_key);
        out.extend_from_slice(&self.recipient_transfer_public_key);
        out.extend_from_slice(&self.recipient_signing_public_key);
        out.extend_from_slice(self.device_id.as_bytes());
        out.extend_from_slice(&self.credential_digest);
        framed(&mut out, self.role.bytes())?;
        Ok(out)
    }

    pub fn sas(&self) -> Result<String, CryptoError> {
        let digest = Sha256::digest(self.canonical_bytes()?);
        let number =
            u64::from_be_bytes(digest[..8].try_into().expect("SHA-256 has 32 bytes")) % 1_000_000;
        Ok(format!("{number:06}"))
    }
}

/// Constant-time comparison for the fixed-width user-confirmed SAS value.
pub fn sas_matches(expected: &str, provided: &str) -> bool {
    expected.len() == 6
        && provided.len() == 6
        && expected
            .bytes()
            .zip(provided.bytes())
            .fold(0u8, |difference, (left, right)| difference | (left ^ right))
            == 0
}

/// Immutable, recipient-supplied values authenticated with the QR-only invitation secret.
pub struct PhoneClaimMacInput<'a> {
    pub canonical_origin: &'a str,
    pub invitation_token_digest: &'a [u8; 32],
    pub owner_transfer_public_key: &'a [u8; 32],
    pub recipient_transfer_public_key: &'a [u8; 32],
    pub recipient_signing_public_key: &'a [u8; 32],
    pub device_id: Uuid,
    pub credential_digest: &'a [u8; 32],
    pub client_kind: &'a str,
    pub device_name: &'a str,
}

pub fn phone_claim_authenticator(
    invitation_secret: &[u8; 32],
    input: &PhoneClaimMacInput<'_>,
) -> Result<[u8; 32], CryptoError> {
    let origin = input.canonical_origin.as_bytes();
    if origin.is_empty()
        || origin.len() > MAX_CANONICAL_ORIGIN_BYTES
        || input.client_kind.is_empty()
        || input.device_name.is_empty()
    {
        return Err(CryptoError::InvalidEnrollment);
    }
    let mut mac = Hmac::<Sha256>::new_from_slice(invitation_secret)
        .map_err(|_| CryptoError::OperationFailed)?;
    update_framed_mac(&mut mac, PHONE_CLAIM_DOMAIN)?;
    update_framed_mac(&mut mac, origin)?;
    mac.update(input.invitation_token_digest);
    mac.update(input.owner_transfer_public_key);
    mac.update(input.recipient_transfer_public_key);
    mac.update(input.recipient_signing_public_key);
    mac.update(input.device_id.as_bytes());
    mac.update(input.credential_digest);
    update_framed_mac(&mut mac, input.client_kind.as_bytes())?;
    update_framed_mac(&mut mac, input.device_name.as_bytes())?;
    Ok(mac.finalize().into_bytes().into())
}

pub fn verify_phone_claim_authenticator(
    invitation_secret: &[u8; 32],
    input: &PhoneClaimMacInput<'_>,
    authenticator: &[u8; 32],
) -> Result<bool, CryptoError> {
    let expected = phone_claim_authenticator(invitation_secret, input)?;
    Ok(constant_time_equal(&expected, authenticator))
}

pub struct AdmissionKeypair(crypto_sign::KeyPair);
impl AdmissionKeypair {
    pub fn public_key(&self) -> [u8; 32] {
        *self.0.public_key.as_bytes()
    }
    pub fn sign_context(&self, context: &EnrollmentContext) -> Result<[u8; 64], CryptoError> {
        sign(&self.0.secret_key, &context.canonical_bytes()?)
    }
}

pub fn derive_admission_keypair(root: &RootKey) -> Result<AdmissionKeypair, CryptoError> {
    ensure_sodium()?;
    let master = crypto_kdf::Key::from_slice(&root.0).map_err(|_| CryptoError::OperationFailed)?;
    let seed = Zeroizing::new(
        crypto_kdf::derive_from_key(32, 1, b"PpyAdm01", &master)
            .map_err(|_| CryptoError::OperationFailed)?,
    );
    let keypair =
        crypto_sign::KeyPair::from_seed(&seed).map_err(|_| CryptoError::OperationFailed)?;
    Ok(AdmissionKeypair(keypair))
}

pub fn verify_admission_signature(
    public_key: &[u8; 32],
    context: &EnrollmentContext,
    signature: &[u8; 64],
) -> Result<bool, CryptoError> {
    verify(public_key, &context.canonical_bytes()?, signature)
}

pub fn sign_device_proof(
    device_secret_key: &[u8; 64],
    context: &EnrollmentContext,
) -> Result<[u8; 64], CryptoError> {
    let key = crypto_sign::SecretKey::from_bytes(device_secret_key)
        .map_err(|_| CryptoError::InvalidEnrollment)?;
    sign(&key, &device_proof_message(context)?)
}

pub fn verify_device_proof(
    device_public_key: &[u8; 32],
    context: &EnrollmentContext,
    signature: &[u8; 64],
) -> Result<bool, CryptoError> {
    verify(
        device_public_key,
        &device_proof_message(context)?,
        signature,
    )
}

fn sign(key: &crypto_sign::SecretKey, message: &[u8]) -> Result<[u8; 64], CryptoError> {
    ensure_sodium()?;
    crypto_sign::sign_detached(message, key).map_err(|_| CryptoError::OperationFailed)
}
fn verify(
    public_key: &[u8; 32],
    message: &[u8],
    signature: &[u8; 64],
) -> Result<bool, CryptoError> {
    ensure_sodium()?;
    let key = crypto_sign::PublicKey::from_bytes(public_key)
        .map_err(|_| CryptoError::InvalidEnrollment)?;
    Ok(crypto_sign::verify_detached(signature, message, &key))
}
fn device_proof_message(context: &EnrollmentContext) -> Result<Vec<u8>, CryptoError> {
    let context = context.canonical_bytes()?;
    let mut message = Vec::with_capacity(DEVICE_PROOF_DOMAIN.len() + 4 + context.len());
    framed(&mut message, DEVICE_PROOF_DOMAIN)?;
    framed(&mut message, &context)?;
    Ok(message)
}

pub struct TransferKeypair(crypto_box::KeyPair);
impl TransferKeypair {
    pub fn generate() -> Result<Self, CryptoError> {
        ensure_sodium()?;
        Ok(Self(crypto_box::KeyPair::generate()))
    }
    pub fn public_key(&self) -> [u8; 32] {
        *self.0.public_key.as_bytes()
    }
}

/// Root material that is only ever present in protected native/worker memory.
pub struct HandoffRootPackage {
    root: RootKey,
    profile: KeyProfile,
    header: VaultCheckHeader,
}
impl HandoffRootPackage {
    pub fn new(
        root: RootKey,
        profile: KeyProfile,
        header: VaultCheckHeader,
    ) -> Result<Self, CryptoError> {
        if header.profile != profile {
            return Err(CryptoError::InvalidProfile);
        }
        verify_vault_check_header(&root, &profile, &header)?;
        Ok(Self {
            root,
            profile,
            header,
        })
    }
    pub fn profile(&self) -> &KeyProfile {
        &self.profile
    }
    pub fn header(&self) -> &VaultCheckHeader {
        &self.header
    }
    pub fn into_parts(self) -> (RootKey, KeyProfile, VaultCheckHeader) {
        (self.root, self.profile, self.header)
    }
}

pub struct EncryptedHandoff {
    pub nonce: [u8; TRANSFER_NONCE_BYTES],
    pub ciphertext: Vec<u8>,
}

/// A handoff that was opened with authenticated peer keys and fully checked against its context.
pub struct VerifiedHandoff {
    package: HandoffRootPackage,
    context: EnrollmentContext,
}
impl VerifiedHandoff {
    pub fn into_parts(self) -> (HandoffRootPackage, EnrollmentContext) {
        (self.package, self.context)
    }

    /// Signs only the exact context authenticated while opening this handoff. The root never
    /// leaves the opaque handoff, allowing the caller to retain it for verified activation.
    pub fn sign_admission_context(
        &self,
        context: &EnrollmentContext,
    ) -> Result<[u8; 64], CryptoError> {
        if context != &self.context {
            return Err(CryptoError::AuthenticationFailed);
        }
        derive_admission_keypair(&self.package.root)?.sign_context(&self.context)
    }
}

pub fn seal_handoff(
    sender: &TransferKeypair,
    recipient_public_key: &[u8; 32],
    context: &EnrollmentContext,
    package: HandoffRootPackage,
) -> Result<EncryptedHandoff, CryptoError> {
    ensure_sodium()?;
    validate_phone_handoff_context(context)?;
    if sender.public_key() != context.owner_transfer_public_key
        || recipient_public_key != &context.recipient_transfer_public_key
    {
        return Err(CryptoError::AuthenticationFailed);
    }
    let recipient = crypto_box::PublicKey::from_bytes(recipient_public_key)
        .map_err(|_| CryptoError::InvalidEnrollment)?;
    validate_package_context(package.profile(), context, package.header())?;
    let context = context.canonical_bytes()?;
    let package = encode_package(package)?;
    let mut plaintext = Zeroizing::new(Vec::with_capacity(4 + context.len() + package.len()));
    framed(&mut plaintext, &context)?;
    plaintext.extend_from_slice(&package);
    if plaintext.len() > HANDOFF_MAX_BYTES {
        return Err(CryptoError::InvalidEnrollment);
    }
    let nonce = crypto_box::Nonce::generate();
    let ciphertext = crypto_box::seal(&plaintext, &nonce, &recipient, &sender.0.secret_key)
        .map_err(|_| CryptoError::OperationFailed)?;
    Ok(EncryptedHandoff {
        nonce: *nonce.as_bytes(),
        ciphertext,
    })
}

pub fn open_handoff(
    recipient: &TransferKeypair,
    sender_public_key: &[u8; 32],
    expected_context: &EnrollmentContext,
    encrypted: &EncryptedHandoff,
) -> Result<VerifiedHandoff, CryptoError> {
    ensure_sodium()?;
    validate_phone_handoff_context(expected_context)?;
    if recipient.public_key() != expected_context.recipient_transfer_public_key
        || sender_public_key != &expected_context.owner_transfer_public_key
    {
        return Err(CryptoError::AuthenticationFailed);
    }
    if encrypted.ciphertext.len() > HANDOFF_MAX_BYTES + crypto_box::MACBYTES {
        return Err(CryptoError::InvalidEnrollment);
    }
    let sender = crypto_box::PublicKey::from_bytes(sender_public_key)
        .map_err(|_| CryptoError::InvalidEnrollment)?;
    let nonce = crypto_box::Nonce::from_bytes_exact(encrypted.nonce);
    let plain = Zeroizing::new(
        crypto_box::open(
            &encrypted.ciphertext,
            &nonce,
            &sender,
            &recipient.0.secret_key,
        )
        .map_err(|_| CryptoError::AuthenticationFailed)?,
    );
    let (context, package) = split_framed(&plain)?;
    if plain.len() > HANDOFF_MAX_BYTES || context != expected_context.canonical_bytes()? {
        return Err(CryptoError::AuthenticationFailed);
    }
    let package = decode_package(package)?;
    validate_package_context(package.profile(), expected_context, package.header())?;
    Ok(VerifiedHandoff {
        package,
        context: expected_context.clone(),
    })
}

fn encode_package(package: HandoffRootPackage) -> Result<Zeroizing<Vec<u8>>, CryptoError> {
    let (root, profile, header) = package.into_parts();
    let mut out = Zeroizing::new(Vec::with_capacity(128 + header.check.ciphertext.len()));
    out.push(HANDOFF_VERSION);
    out.extend_from_slice(&root.0);
    encode_profile(&mut out, &profile);
    out.extend_from_slice(&header.check.nonce);
    framed(&mut out, &header.check.ciphertext)?;
    if out.len() > HANDOFF_MAX_BYTES {
        return Err(CryptoError::InvalidEnrollment);
    }
    Ok(out)
}
fn validate_phone_handoff_context(context: &EnrollmentContext) -> Result<(), CryptoError> {
    if context.purpose != EnrollmentPurpose::Phone
        || context.owner_transfer_public_key == [0; 32]
        || context.recipient_transfer_public_key == [0; 32]
    {
        return Err(CryptoError::AuthenticationFailed);
    }
    context.canonical_bytes().map(|_| ())
}
fn validate_package_context(
    profile: &KeyProfile,
    context: &EnrollmentContext,
    header: &VaultCheckHeader,
) -> Result<(), CryptoError> {
    if profile != &header.profile {
        return Err(CryptoError::AuthenticationFailed);
    }
    if profile.vault_id != context.vault_id
        || profile.key_epoch != context.key_epoch
        || crate::profile_fingerprint_bytes(profile)? != context.fingerprint
    {
        return Err(CryptoError::AuthenticationFailed);
    }
    Ok(())
}
fn decode_package(bytes: &[u8]) -> Result<HandoffRootPackage, CryptoError> {
    if bytes.len() < 1 + 32 + 2 + 16 + 16 + 4 + 24 + 4
        || bytes.len() > HANDOFF_MAX_BYTES
        || bytes[0] != HANDOFF_VERSION
    {
        return Err(CryptoError::InvalidEnrollment);
    }
    let root = RootKey::from_bytes(&bytes[1..33])?;
    let (profile, at) = decode_profile(bytes, 33)?;
    let nonce: [u8; 24] = bytes
        .get(at..at + 24)
        .ok_or(CryptoError::InvalidEnrollment)?
        .try_into()
        .map_err(|_| CryptoError::InvalidEnrollment)?;
    let (ciphertext, end) = read_framed(bytes, at + 24)?;
    if end != bytes.len() {
        return Err(CryptoError::InvalidEnrollment);
    }
    HandoffRootPackage::new(
        root,
        profile.clone(),
        VaultCheckHeader {
            profile,
            check: EncryptedEnvelope {
                nonce,
                ciphertext: ciphertext.to_vec(),
            },
        },
    )
}
fn encode_profile(out: &mut Vec<u8>, profile: &KeyProfile) {
    out.extend_from_slice(&profile.crypto_suite.to_be_bytes());
    out.extend_from_slice(&profile.salt);
    out.extend_from_slice(profile.vault_id.as_bytes());
    out.extend_from_slice(&profile.key_epoch.to_be_bytes());
}
fn decode_profile(bytes: &[u8], at: usize) -> Result<(KeyProfile, usize), CryptoError> {
    let end = at.checked_add(38).ok_or(CryptoError::InvalidEnrollment)?;
    let fields = bytes.get(at..end).ok_or(CryptoError::InvalidEnrollment)?;
    let crypto_suite = u16::from_be_bytes(
        fields[..2]
            .try_into()
            .map_err(|_| CryptoError::InvalidEnrollment)?,
    );
    let salt = fields[2..18]
        .try_into()
        .map_err(|_| CryptoError::InvalidEnrollment)?;
    let vault_id = Uuid::from_bytes(
        fields[18..34]
            .try_into()
            .map_err(|_| CryptoError::InvalidEnrollment)?,
    );
    let key_epoch = u32::from_be_bytes(
        fields[34..38]
            .try_into()
            .map_err(|_| CryptoError::InvalidEnrollment)?,
    );
    let profile = KeyProfile {
        crypto_suite,
        salt,
        vault_id,
        key_epoch,
    };
    profile.validate()?;
    Ok((profile, end))
}
fn framed(out: &mut Vec<u8>, bytes: &[u8]) -> Result<(), CryptoError> {
    let len = u32::try_from(bytes.len()).map_err(|_| CryptoError::InvalidEnrollment)?;
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(bytes);
    Ok(())
}
fn update_framed_mac(mac: &mut Hmac<Sha256>, bytes: &[u8]) -> Result<(), CryptoError> {
    let length = u32::try_from(bytes.len()).map_err(|_| CryptoError::InvalidEnrollment)?;
    mac.update(&length.to_be_bytes());
    mac.update(bytes);
    Ok(())
}
fn constant_time_equal(left: &[u8], right: &[u8]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .fold(0u8, |difference, (l, r)| difference | (l ^ r))
            == 0
}
fn split_framed(bytes: &[u8]) -> Result<(&[u8], &[u8]), CryptoError> {
    let (first, end) = read_framed(bytes, 0)?;
    Ok((
        first,
        bytes.get(end..).ok_or(CryptoError::InvalidEnrollment)?,
    ))
}
fn read_framed(bytes: &[u8], at: usize) -> Result<(&[u8], usize), CryptoError> {
    let length: [u8; 4] = bytes
        .get(at..at + 4)
        .ok_or(CryptoError::InvalidEnrollment)?
        .try_into()
        .map_err(|_| CryptoError::InvalidEnrollment)?;
    let end = at
        .checked_add(4 + u32::from_be_bytes(length) as usize)
        .ok_or(CryptoError::InvalidEnrollment)?;
    Ok((
        bytes
            .get(at + 4..end)
            .ok_or(CryptoError::InvalidEnrollment)?,
        end,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{create_vault_check_header, derive_root_key};

    fn context() -> EnrollmentContext {
        EnrollmentContext {
            purpose: EnrollmentPurpose::Phone,
            canonical_origin: "https://peppy.test".into(),
            vault_id: Uuid::from_u128(1),
            fingerprint: [2; 32],
            key_epoch: 3,
            enrollment_id: Uuid::from_u128(4),
            server_nonce: [5; 32],
            owner_transfer_public_key: [6; 32],
            recipient_transfer_public_key: [7; 32],
            recipient_signing_public_key: [8; 32],
            device_id: Uuid::from_u128(9),
            credential_digest: [10; 32],
            role: EnrollmentRole::Gateway,
        }
    }
    fn package(profile: &KeyProfile) -> HandoffRootPackage {
        let root = derive_root_key("correct horse battery staple", profile).unwrap();
        let header = create_vault_check_header(&root, profile.clone()).unwrap();
        HandoffRootPackage::new(root, profile.clone(), header).unwrap()
    }
    fn handoff_context(
        profile: &KeyProfile,
        sender: &TransferKeypair,
        recipient: &TransferKeypair,
    ) -> EnrollmentContext {
        EnrollmentContext {
            owner_transfer_public_key: sender.public_key(),
            recipient_transfer_public_key: recipient.public_key(),
            fingerprint: crate::profile_fingerprint_bytes(profile).unwrap(),
            ..context()
        }
    }
    #[test]
    fn signatures_bind_every_context_field() {
        let profile = KeyProfile::new(Uuid::from_u128(1), 3).unwrap();
        let root = derive_root_key("correct horse battery staple", &profile).unwrap();
        let key = derive_admission_keypair(&root).unwrap();
        let context = context();
        let signature = key.sign_context(&context).unwrap();
        assert!(verify_admission_signature(&key.public_key(), &context, &signature).unwrap());
        let mutations: [fn(&mut EnrollmentContext); 13] = [
            |value| value.purpose = EnrollmentPurpose::Manual,
            |value| value.canonical_origin.push('/'),
            |value| value.vault_id = Uuid::from_u128(11),
            |value| value.fingerprint[0] ^= 1,
            |value| value.key_epoch += 1,
            |value| value.enrollment_id = Uuid::from_u128(12),
            |value| value.server_nonce[0] ^= 1,
            |value| value.owner_transfer_public_key[0] ^= 1,
            |value| value.recipient_transfer_public_key[0] ^= 1,
            |value| value.recipient_signing_public_key[0] ^= 1,
            |value| value.device_id = Uuid::from_u128(13),
            |value| value.credential_digest[0] ^= 1,
            |value| value.role = EnrollmentRole::Device,
        ];
        for mutate in mutations {
            let mut changed = context.clone();
            mutate(&mut changed);
            assert!(!verify_admission_signature(&key.public_key(), &changed, &signature).unwrap());
        }
    }
    #[test]
    fn context_and_sas_vector_are_stable() {
        let context = context();
        assert_eq!(context.sas().unwrap(), "940871");
        assert!(sas_matches("940871", "940871"));
        assert!(!sas_matches("940871", "940870"));
        assert_eq!(
            Sha256::digest(context.canonical_bytes().unwrap()).as_slice(),
            &[
                0x85, 0x95, 0xce, 0xe3, 0x63, 0x76, 0xf2, 0x07, 0x17, 0x73, 0x4c, 0x9a, 0x74, 0xf1,
                0x2d, 0x94, 0xae, 0x89, 0x0e, 0xe7, 0xda, 0x62, 0x32, 0x3c, 0x00, 0x2e, 0x57, 0xc3,
                0x03, 0x2a, 0xe7, 0xbb,
            ]
        );
    }
    #[test]
    fn wrong_shared_secret_cannot_prove_admission() {
        let profile = KeyProfile::new(Uuid::from_u128(1), 3).unwrap();
        let correct = derive_root_key("correct horse battery staple", &profile).unwrap();
        let wrong = derive_root_key("another valid shared secret", &profile).unwrap();
        let correct = derive_admission_keypair(&correct).unwrap();
        let wrong = derive_admission_keypair(&wrong).unwrap();
        let signature = wrong.sign_context(&context()).unwrap();
        assert!(
            !verify_admission_signature(&correct.public_key(), &context(), &signature).unwrap()
        );
    }
    #[test]
    fn handoff_rejects_wrong_recipient_context_and_tampering() {
        let sender = TransferKeypair::generate().unwrap();
        let recipient = TransferKeypair::generate().unwrap();
        let profile = KeyProfile::new(Uuid::from_u128(1), 3).unwrap();
        let context = handoff_context(&profile, &sender, &recipient);
        let mut sealed = seal_handoff(
            &sender,
            &recipient.public_key(),
            &context,
            package(&profile),
        )
        .unwrap();
        assert!(open_handoff(&recipient, &sender.public_key(), &context, &sealed).is_ok());
        sealed.ciphertext[0] ^= 1;
        assert!(open_handoff(&recipient, &sender.public_key(), &context, &sealed).is_err());
        let sealed = seal_handoff(
            &sender,
            &recipient.public_key(),
            &context,
            package(&profile),
        )
        .unwrap();
        let other = TransferKeypair::generate().unwrap();
        assert!(open_handoff(&other, &sender.public_key(), &context, &sealed).is_err());
        let mut changed = context;
        changed.key_epoch += 1;
        assert!(open_handoff(&recipient, &sender.public_key(), &changed, &sealed).is_err());
    }

    #[test]
    fn handoff_rejects_non_phone_keys_and_package_anchor_mismatches() {
        let sender = TransferKeypair::generate().unwrap();
        let recipient = TransferKeypair::generate().unwrap();
        let profile = KeyProfile::new(Uuid::from_u128(1), 3).unwrap();
        let context = handoff_context(&profile, &sender, &recipient);
        let mut foreign_vault = context.clone();
        foreign_vault.vault_id = Uuid::from_u128(99);
        assert!(
            seal_handoff(
                &sender,
                &recipient.public_key(),
                &foreign_vault,
                package(&profile)
            )
            .is_err()
        );
        let mut foreign_epoch = context.clone();
        foreign_epoch.key_epoch += 1;
        assert!(
            seal_handoff(
                &sender,
                &recipient.public_key(),
                &foreign_epoch,
                package(&profile)
            )
            .is_err()
        );
        let mut foreign_fingerprint = context.clone();
        foreign_fingerprint.fingerprint[0] ^= 1;
        assert!(
            seal_handoff(
                &sender,
                &recipient.public_key(),
                &foreign_fingerprint,
                package(&profile)
            )
            .is_err()
        );
        let mut manual = context;
        manual.purpose = EnrollmentPurpose::Manual;
        assert!(
            seal_handoff(&sender, &recipient.public_key(), &manual, package(&profile)).is_err()
        );
    }

    #[test]
    fn qr_secret_mac_authenticates_all_claim_fields() {
        let input = PhoneClaimMacInput {
            canonical_origin: "https://peppy.test",
            invitation_token_digest: &[1; 32],
            owner_transfer_public_key: &[2; 32],
            recipient_transfer_public_key: &[3; 32],
            recipient_signing_public_key: &[4; 32],
            device_id: Uuid::from_u128(5),
            credential_digest: &[6; 32],
            client_kind: "android",
            device_name: "Phone",
        };
        let secret = [7; 32];
        let authenticator = phone_claim_authenticator(&secret, &input).unwrap();
        assert!(verify_phone_claim_authenticator(&secret, &input, &authenticator).unwrap());
        let changed = PhoneClaimMacInput {
            recipient_transfer_public_key: &[8; 32],
            ..input
        };
        assert!(!verify_phone_claim_authenticator(&secret, &changed, &authenticator).unwrap());
        let changed = PhoneClaimMacInput {
            credential_digest: &[9; 32],
            ..input
        };
        assert!(!verify_phone_claim_authenticator(&secret, &changed, &authenticator).unwrap());
    }
}
