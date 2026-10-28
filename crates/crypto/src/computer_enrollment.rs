//! Purpose-separated computer QR enrollment proofs.

use crate::{
    CryptoError,
    enrollment::{EnrollmentContext, EnrollmentRole, TRANSFER_NONCE_BYTES},
};
use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use uuid::Uuid;
use zeroize::Zeroizing;

pub const COMPUTER_ENROLLMENT_QR_SECRET_BYTES: usize = 32;
pub const COMPUTER_ENROLLMENT_PROOF_BYTES: usize = 32;
const HKDF_SALT: &[u8] = b"peppy-computer-enrollment-v1";
const CLAIM_TOKEN_LABEL: &[u8] = b"server-claim-token";
const OWNER_PROOF_KEY_LABEL: &[u8] = b"owner-proof-key";
const RECIPIENT_COMMITMENT_DOMAIN: &[u8] = b"peppy-computer-recipient-v1";
const OWNER_PROOF_DOMAIN: &[u8] = b"peppy-computer-owner-proof-v1";
const MAX_TEXT_BYTES: usize = 2_048;

/// The computer-controlled recipient identity pinned into the QR commitment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ComputerRecipientBinding {
    pub transfer_public_key: [u8; 32],
    pub signing_public_key: [u8; 32],
    pub device_id: Uuid,
    pub credential_digest: [u8; 32],
    pub requested_role: EnrollmentRole,
    pub client_kind: String,
    pub device_name: String,
}

/// Full owner response authenticated with a key derived from the QR-only secret.
pub struct ComputerOwnerProofInput<'a> {
    pub context: &'a EnrollmentContext,
    pub recipient: &'a ComputerRecipientBinding,
    pub owner_device_id: Uuid,
    pub granted_role: EnrollmentRole,
    pub transfer_nonce: &'a [u8; TRANSFER_NONCE_BYTES],
    pub handoff_ciphertext_hash: &'a [u8; 32],
}

/// Generates a fresh secret suitable for rendering into a computer QR payload.
pub fn generate_computer_enrollment_qr_secret() -> Result<Zeroizing<[u8; 32]>, CryptoError> {
    crate::ensure_sodium()?;
    let mut secret = Zeroizing::new([0; COMPUTER_ENROLLMENT_QR_SECRET_BYTES]);
    libsodium_rs::random::fill_bytes(&mut *secret);
    Ok(secret)
}

/// Derives the server-only bearer token. It is distinct from the owner proof key.
pub fn computer_enrollment_claim_token(
    qr_secret: &[u8; COMPUTER_ENROLLMENT_QR_SECRET_BYTES],
) -> Result<[u8; 32], CryptoError> {
    derive_qr_key(qr_secret, CLAIM_TOKEN_LABEL)
}

/// Derives the local owner-proof MAC key. Never send this key to the server.
pub fn computer_enrollment_owner_proof_key(
    qr_secret: &[u8; COMPUTER_ENROLLMENT_QR_SECRET_BYTES],
) -> Result<Zeroizing<[u8; 32]>, CryptoError> {
    Ok(Zeroizing::new(derive_qr_key(
        qr_secret,
        OWNER_PROOF_KEY_LABEL,
    )?))
}

/// Commits to every claimant-owned identity value displayed or approved by the owner.
pub fn computer_recipient_commitment(
    recipient: &ComputerRecipientBinding,
) -> Result<[u8; 32], CryptoError> {
    let mut bytes =
        Vec::with_capacity(192 + recipient.client_kind.len() + recipient.device_name.len());
    framed(&mut bytes, RECIPIENT_COMMITMENT_DOMAIN)?;
    bytes.extend_from_slice(&recipient.transfer_public_key);
    bytes.extend_from_slice(&recipient.signing_public_key);
    bytes.extend_from_slice(recipient.device_id.as_bytes());
    bytes.extend_from_slice(&recipient.credential_digest);
    framed(&mut bytes, recipient.requested_role.bytes())?;
    framed_text(&mut bytes, &recipient.client_kind)?;
    framed_text(&mut bytes, &recipient.device_name)?;
    Ok(Sha256::digest(bytes).into())
}

/// MACs the complete owner response, including the ciphertext digest.
pub fn computer_owner_proof(
    qr_secret: &[u8; COMPUTER_ENROLLMENT_QR_SECRET_BYTES],
    input: &ComputerOwnerProofInput<'_>,
) -> Result<[u8; COMPUTER_ENROLLMENT_PROOF_BYTES], CryptoError> {
    Ok(owner_proof_mac(qr_secret, input)?
        .finalize()
        .into_bytes()
        .into())
}

/// Rejects a returned owner response unless it still matches the computer's local QR state.
pub fn verify_computer_owner_proof(
    qr_secret: &[u8; COMPUTER_ENROLLMENT_QR_SECRET_BYTES],
    expected_canonical_origin: &str,
    expected_invitation_id: Uuid,
    expected_recipient: &ComputerRecipientBinding,
    input: &ComputerOwnerProofInput<'_>,
    proof: &[u8; COMPUTER_ENROLLMENT_PROOF_BYTES],
) -> Result<bool, CryptoError> {
    if expected_recipient != input.recipient
        || expected_canonical_origin != input.context.canonical_origin
        || expected_invitation_id != input.context.enrollment_id
        || input.context.purpose != crate::enrollment::EnrollmentPurpose::Computer
        || input.context.recipient_transfer_public_key != input.recipient.transfer_public_key
        || input.context.recipient_signing_public_key != input.recipient.signing_public_key
        || input.context.device_id != input.recipient.device_id
        || input.context.credential_digest != input.recipient.credential_digest
        || input.context.role != input.granted_role
    {
        return Ok(false);
    }
    Ok(owner_proof_mac(qr_secret, input)?
        .verify_slice(proof)
        .is_ok())
}

fn owner_proof_mac(
    qr_secret: &[u8; COMPUTER_ENROLLMENT_QR_SECRET_BYTES],
    input: &ComputerOwnerProofInput<'_>,
) -> Result<Hmac<Sha256>, CryptoError> {
    validate_input(input)?;
    let key = computer_enrollment_owner_proof_key(qr_secret)?;
    let mut mac =
        Hmac::<Sha256>::new_from_slice(&*key).map_err(|_| CryptoError::OperationFailed)?;
    update_framed_mac(&mut mac, OWNER_PROOF_DOMAIN)?;
    update_framed_mac(&mut mac, &input.context.canonical_bytes()?)?;
    mac.update(&computer_recipient_commitment(input.recipient)?);
    mac.update(input.owner_device_id.as_bytes());
    update_framed_mac(&mut mac, input.granted_role.bytes())?;
    mac.update(input.transfer_nonce);
    mac.update(input.handoff_ciphertext_hash);
    Ok(mac)
}

fn derive_qr_key(qr_secret: &[u8; 32], label: &[u8]) -> Result<[u8; 32], CryptoError> {
    let hkdf = Hkdf::<Sha256>::new(Some(HKDF_SALT), qr_secret);
    let mut output = [0; 32];
    hkdf.expand(label, &mut output)
        .map_err(|_| CryptoError::OperationFailed)?;
    Ok(output)
}

fn validate_input(input: &ComputerOwnerProofInput<'_>) -> Result<(), CryptoError> {
    if input.context.purpose != crate::enrollment::EnrollmentPurpose::Computer
        || input.context.recipient_transfer_public_key != input.recipient.transfer_public_key
        || input.context.recipient_signing_public_key != input.recipient.signing_public_key
        || input.context.device_id != input.recipient.device_id
        || input.context.credential_digest != input.recipient.credential_digest
        || input.context.role != input.granted_role
    {
        return Err(CryptoError::InvalidEnrollment);
    }
    computer_recipient_commitment(input.recipient)?;
    input.context.canonical_bytes().map(|_| ())
}

fn framed_text(out: &mut Vec<u8>, value: &str) -> Result<(), CryptoError> {
    if value.is_empty() || value.len() > MAX_TEXT_BYTES {
        return Err(CryptoError::InvalidEnrollment);
    }
    framed(out, value.as_bytes())
}
fn framed(out: &mut Vec<u8>, bytes: &[u8]) -> Result<(), CryptoError> {
    let length = u32::try_from(bytes.len()).map_err(|_| CryptoError::InvalidEnrollment)?;
    out.extend_from_slice(&length.to_be_bytes());
    out.extend_from_slice(bytes);
    Ok(())
}
fn update_framed_mac(mac: &mut Hmac<Sha256>, bytes: &[u8]) -> Result<(), CryptoError> {
    let length = u32::try_from(bytes.len()).map_err(|_| CryptoError::InvalidEnrollment)?;
    mac.update(&length.to_be_bytes());
    mac.update(bytes);
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::enrollment::{EnrollmentContext, EnrollmentPurpose};

    fn recipient() -> ComputerRecipientBinding {
        ComputerRecipientBinding {
            transfer_public_key: [1; 32],
            signing_public_key: [2; 32],
            device_id: Uuid::from_u128(3),
            credential_digest: [4; 32],
            requested_role: EnrollmentRole::Device,
            client_kind: "desktop".into(),
            device_name: "Computer".into(),
        }
    }
    fn context(recipient: &ComputerRecipientBinding) -> EnrollmentContext {
        EnrollmentContext {
            purpose: EnrollmentPurpose::Computer,
            canonical_origin: "https://peppy.test".into(),
            vault_id: Uuid::from_u128(5),
            fingerprint: [6; 32],
            key_epoch: 7,
            enrollment_id: Uuid::from_u128(8),
            server_nonce: [9; 32],
            owner_transfer_public_key: [10; 32],
            recipient_transfer_public_key: recipient.transfer_public_key,
            recipient_signing_public_key: recipient.signing_public_key,
            device_id: recipient.device_id,
            credential_digest: recipient.credential_digest,
            role: EnrollmentRole::Device,
        }
    }
    #[test]
    fn separates_claim_token_from_owner_proof_key() {
        let secret = [11; 32];
        assert_ne!(
            computer_enrollment_claim_token(&secret).unwrap(),
            *computer_enrollment_owner_proof_key(&secret).unwrap()
        );
    }
    #[test]
    fn owner_proof_binds_every_response_field_and_ciphertext() {
        let secret = [11; 32];
        let recipient = recipient();
        let context = context(&recipient);
        let input = ComputerOwnerProofInput {
            context: &context,
            recipient: &recipient,
            owner_device_id: Uuid::from_u128(12),
            granted_role: EnrollmentRole::Device,
            transfer_nonce: &[13; TRANSFER_NONCE_BYTES],
            handoff_ciphertext_hash: &[13; 32],
        };
        let proof = computer_owner_proof(&secret, &input).unwrap();
        assert!(
            verify_computer_owner_proof(
                &secret,
                &context.canonical_origin,
                context.enrollment_id,
                &recipient,
                &input,
                &proof
            )
            .unwrap()
        );
        let mutations: [fn(&mut EnrollmentContext); 13] = [
            |v| v.purpose = EnrollmentPurpose::Phone,
            |v| v.canonical_origin.push('/'),
            |v| v.vault_id = Uuid::from_u128(14),
            |v| v.fingerprint[0] ^= 1,
            |v| v.key_epoch += 1,
            |v| v.enrollment_id = Uuid::from_u128(15),
            |v| v.server_nonce[0] ^= 1,
            |v| v.owner_transfer_public_key[0] ^= 1,
            |v| v.recipient_transfer_public_key[0] ^= 1,
            |v| v.recipient_signing_public_key[0] ^= 1,
            |v| v.device_id = Uuid::from_u128(16),
            |v| v.credential_digest[0] ^= 1,
            |v| v.role = EnrollmentRole::Owner,
        ];
        for mutate in mutations {
            let mut changed = context.clone();
            mutate(&mut changed);
            let changed_input = ComputerOwnerProofInput {
                context: &changed,
                ..input
            };
            assert!(
                !verify_computer_owner_proof(
                    &secret,
                    &context.canonical_origin,
                    context.enrollment_id,
                    &recipient,
                    &changed_input,
                    &proof
                )
                .unwrap()
            );
        }
        let changed = ComputerOwnerProofInput {
            handoff_ciphertext_hash: &[14; 32],
            ..input
        };
        assert!(
            !verify_computer_owner_proof(
                &secret,
                &context.canonical_origin,
                context.enrollment_id,
                &recipient,
                &changed,
                &proof
            )
            .unwrap()
        );
        let changed = ComputerOwnerProofInput {
            transfer_nonce: &[14; TRANSFER_NONCE_BYTES],
            ..input
        };
        assert!(
            !verify_computer_owner_proof(
                &secret,
                &context.canonical_origin,
                context.enrollment_id,
                &recipient,
                &changed,
                &proof
            )
            .unwrap()
        );
        let mut changed_recipient = recipient.clone();
        changed_recipient.requested_role = EnrollmentRole::Owner;
        let changed = ComputerOwnerProofInput {
            recipient: &changed_recipient,
            ..input
        };
        assert!(
            !verify_computer_owner_proof(
                &secret,
                &context.canonical_origin,
                context.enrollment_id,
                &recipient,
                &changed,
                &proof
            )
            .unwrap()
        );
        let mut changed_recipient = recipient.clone();
        changed_recipient.client_kind = "browser".into();
        let changed = ComputerOwnerProofInput {
            recipient: &changed_recipient,
            ..input
        };
        assert!(
            !verify_computer_owner_proof(
                &secret,
                &context.canonical_origin,
                context.enrollment_id,
                &recipient,
                &changed,
                &proof
            )
            .unwrap()
        );
        let mut changed_recipient = recipient.clone();
        changed_recipient.device_name = "Other computer".into();
        let changed = ComputerOwnerProofInput {
            recipient: &changed_recipient,
            ..input
        };
        assert!(
            !verify_computer_owner_proof(
                &secret,
                &context.canonical_origin,
                context.enrollment_id,
                &recipient,
                &changed,
                &proof
            )
            .unwrap()
        );
        let changed = ComputerOwnerProofInput {
            owner_device_id: Uuid::from_u128(17),
            ..input
        };
        assert!(
            !verify_computer_owner_proof(
                &secret,
                &context.canonical_origin,
                context.enrollment_id,
                &recipient,
                &changed,
                &proof
            )
            .unwrap()
        );
        let mut response_context = context.clone();
        response_context.canonical_origin = "https://other.test".into();
        let response = ComputerOwnerProofInput {
            context: &response_context,
            ..input
        };
        let response_proof = computer_owner_proof(&secret, &response).unwrap();
        assert!(
            !verify_computer_owner_proof(
                &secret,
                &context.canonical_origin,
                context.enrollment_id,
                &recipient,
                &response,
                &response_proof
            )
            .unwrap()
        );
        let mut response_context = context.clone();
        response_context.enrollment_id = Uuid::from_u128(18);
        let response = ComputerOwnerProofInput {
            context: &response_context,
            ..input
        };
        let response_proof = computer_owner_proof(&secret, &response).unwrap();
        assert!(
            !verify_computer_owner_proof(
                &secret,
                &context.canonical_origin,
                context.enrollment_id,
                &recipient,
                &response,
                &response_proof
            )
            .unwrap()
        );
    }
    #[test]
    fn rejects_wrong_key_and_cross_purpose_proof() {
        let secret = [11; 32];
        let recipient = recipient();
        let context = context(&recipient);
        let input = ComputerOwnerProofInput {
            context: &context,
            recipient: &recipient,
            owner_device_id: Uuid::from_u128(12),
            granted_role: EnrollmentRole::Device,
            transfer_nonce: &[13; TRANSFER_NONCE_BYTES],
            handoff_ciphertext_hash: &[13; 32],
        };
        let proof = computer_owner_proof(&secret, &input).unwrap();
        assert!(
            !verify_computer_owner_proof(
                &computer_enrollment_claim_token(&secret).unwrap(),
                &context.canonical_origin,
                context.enrollment_id,
                &recipient,
                &input,
                &proof
            )
            .unwrap()
        );
        let mut phone_context = context.clone();
        phone_context.purpose = EnrollmentPurpose::Phone;
        let phone = ComputerOwnerProofInput {
            context: &phone_context,
            ..input
        };
        assert!(
            !verify_computer_owner_proof(
                &secret,
                &context.canonical_origin,
                context.enrollment_id,
                &recipient,
                &phone,
                &proof
            )
            .unwrap()
        );
    }
}
