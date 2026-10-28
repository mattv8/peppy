//! Versioned wire contracts for owner-approved computer enrollment.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::enrollment::{
    decode_base64url, EnrollmentEncodingError, EnrollmentRole, ProposedDevice, ENROLLMENT_KEY_BYTES,
};

pub const COMPUTER_ENROLLMENT_VERSION: u8 = 1;
pub const COMPUTER_ENROLLMENT_PURPOSE: &str = "connect_computer";

/// Recipient identity committed by the computer before its QR is rendered.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ComputerEnrollmentRecipient {
    pub device: ProposedDevice,
    pub credential_digest: String,
    pub requested_role: EnrollmentRole,
}

/// Owner-authenticated QR-first creation request. The computer generated every
/// public field locally and included it in the scanned QR. `claim_token_digest`
/// is a digest of an HKDF-derived bearer, never of the QR secret itself.
#[derive(Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateComputerEnrollmentRequest {
    pub invitation_id: Uuid,
    pub recipient: ComputerEnrollmentRecipient,
    pub recipient_transfer_public_key: String,
    pub recipient_commitment: String,
    pub claim_token_digest: String,
}

#[derive(Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ComputerEnrollmentCreated {
    pub server_nonce: String,
    pub expires_in_seconds: u32,
}

/// QR payload shown by the unconnected computer and scanned by an existing owner phone.
/// The invitation secret must not be logged or copied into ordinary application state.
#[derive(Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ComputerPairingQr {
    pub version: u8,
    pub purpose: String,
    pub origin: String,
    pub invitation_id: Uuid,
    pub invitation_secret: String,
    pub recipient: ComputerEnrollmentRecipient,
    pub recipient_transfer_public_key: String,
    pub recipient_commitment: String,
}

/// The exact transcript MACed by the owner proof and sealed into the handoff context.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ComputerEnrollmentTranscript {
    pub origin: String,
    pub vault_id: Uuid,
    pub fingerprint: String,
    pub key_epoch: u32,
    pub invitation_id: Uuid,
    pub server_nonce: String,
    pub owner_transfer_public_key: String,
    pub owner_device_id: Uuid,
    pub recipient: ComputerEnrollmentRecipient,
    pub recipient_transfer_public_key: String,
    pub granted_role: EnrollmentRole,
}

impl ComputerEnrollmentTranscript {
    pub fn validate_fields(&self) -> Result<(), EnrollmentEncodingError> {
        if self.origin.is_empty()
            || self.key_epoch == 0
            || !lowercase_hex(&self.fingerprint)
            || !lowercase_hex(&self.recipient.credential_digest)
        {
            return Err(EnrollmentEncodingError);
        }
        decode_base64url::<ENROLLMENT_KEY_BYTES>(&self.server_nonce)?;
        decode_base64url::<ENROLLMENT_KEY_BYTES>(&self.owner_transfer_public_key)?;
        decode_base64url::<ENROLLMENT_KEY_BYTES>(&self.recipient_transfer_public_key)?;
        decode_base64url::<ENROLLMENT_KEY_BYTES>(&self.recipient.device.signing_public_key)?;
        Ok(())
    }
}

/// Owner approval. The proof is opaque to the server and verified by the computer with the
/// QR-derived proof key; server authorization remains enforced by its enrolled-device policy.
#[derive(Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ApproveComputerEnrollmentRequest {
    pub transcript: ComputerEnrollmentTranscript,
    pub encrypted_key_package: String,
    pub transfer_nonce: String,
    pub owner_proof: String,
}

/// Claimant confirmation proofs over the exact verified computer transcript.
#[derive(Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ComputerEnrollmentConfirmRequest {
    pub claim_token: String,
    pub device_signature: String,
    pub admission_signature: String,
}

/// Claimant-authenticated result. Verify its local fields and `owner_proof` before opening.
#[derive(Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ComputerEnrollmentStatus {
    pub state: crate::enrollment::EnrollmentState,
    pub expires_in_seconds: u32,
    pub transcript: Option<ComputerEnrollmentTranscript>,
    pub encrypted_key_package: Option<String>,
    pub transfer_nonce: Option<String>,
    pub owner_proof: Option<String>,
}

fn lowercase_hex(value: &str) -> bool {
    value.len() == ENROLLMENT_KEY_BYTES * 2
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
        && hex::decode(value).is_ok_and(|decoded| decoded.len() == ENROLLMENT_KEY_BYTES)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::enrollment::{
        encode_base64url, ClientKind, EnrollmentState, ENROLLMENT_TRANSFER_NONCE_BYTES,
    };

    fn recipient() -> ComputerEnrollmentRecipient {
        ComputerEnrollmentRecipient {
            device: ProposedDevice {
                device_id: Uuid::from_u128(1),
                kind: ClientKind::Desktop,
                name: "Computer".into(),
                signing_public_key: encode_base64url(&[1; 32]),
            },
            credential_digest: "a".repeat(64),
            requested_role: EnrollmentRole::Device,
        }
    }
    fn transcript() -> ComputerEnrollmentTranscript {
        ComputerEnrollmentTranscript {
            origin: "https://example.test".into(),
            vault_id: Uuid::from_u128(2),
            fingerprint: "b".repeat(64),
            key_epoch: 1,
            invitation_id: Uuid::from_u128(3),
            server_nonce: encode_base64url(&[2; 32]),
            owner_transfer_public_key: encode_base64url(&[3; 32]),
            owner_device_id: Uuid::from_u128(4),
            recipient: recipient(),
            recipient_transfer_public_key: encode_base64url(&[4; 32]),
            granted_role: EnrollmentRole::Device,
        }
    }
    #[test]
    fn computer_qr_carries_all_owner_creation_data() {
        let qr = ComputerPairingQr {
            version: COMPUTER_ENROLLMENT_VERSION,
            purpose: COMPUTER_ENROLLMENT_PURPOSE.into(),
            origin: "https://example.test".into(),
            invitation_id: Uuid::nil(),
            invitation_secret: encode_base64url(&[1; 32]),
            recipient: recipient(),
            recipient_transfer_public_key: encode_base64url(&[2; 32]),
            recipient_commitment: encode_base64url(&[3; 32]),
        };
        let value = serde_json::to_value(qr).unwrap();
        assert_eq!(value.as_object().unwrap().len(), 8);
        let scanned: ComputerPairingQr = serde_json::from_value(value).unwrap();
        let create = CreateComputerEnrollmentRequest {
            invitation_id: scanned.invitation_id,
            recipient: scanned.recipient.clone(),
            recipient_transfer_public_key: scanned.recipient_transfer_public_key.clone(),
            recipient_commitment: scanned.recipient_commitment.clone(),
            claim_token_digest: encode_base64url(&[4; 32]),
        };
        assert_eq!(create.invitation_id, scanned.invitation_id);
        assert_eq!(create.recipient, scanned.recipient);
        assert_eq!(
            create.recipient_transfer_public_key,
            scanned.recipient_transfer_public_key
        );
        assert_eq!(create.recipient_commitment, scanned.recipient_commitment);
        assert!(serde_json::from_value::<ComputerPairingQr>(serde_json::json!({"version":1,"purpose":"connect_computer","origin":"https://example.test","invitation_id":Uuid::nil(),"invitation_secret":"secret","recipient":{},"recipient_transfer_public_key":"key","recipient_commitment":"commitment","extra":true})).is_err());
    }
    #[test]
    fn status_carries_the_mac_bound_handoff_fields() {
        let transcript = transcript();
        assert!(transcript.validate_fields().is_ok());
        let status = ComputerEnrollmentStatus {
            state: EnrollmentState::Approved,
            expires_in_seconds: 60,
            transcript: Some(transcript),
            encrypted_key_package: Some(encode_base64url(b"ciphertext")),
            transfer_nonce: Some(encode_base64url(&[5; ENROLLMENT_TRANSFER_NONCE_BYTES])),
            owner_proof: Some(encode_base64url(&[6; 32])),
        };
        let value = serde_json::to_value(status).unwrap();
        assert!(value["transcript"].is_object());
        assert_eq!(value["transfer_nonce"].as_str().unwrap().len(), 32);
    }

    #[test]
    fn confirm_requires_both_context_signatures() {
        let request = ComputerEnrollmentConfirmRequest {
            claim_token: encode_base64url(&[1; 32]),
            device_signature: encode_base64url(&[2; 64]),
            admission_signature: encode_base64url(&[3; 64]),
        };
        let value = serde_json::to_value(&request).unwrap();
        assert_eq!(value.as_object().unwrap().len(), 3);
        assert!(
            serde_json::from_value::<ComputerEnrollmentConfirmRequest>(serde_json::json!({
                "claim_token": request.claim_token,
            }))
            .is_err()
        );
    }
}
