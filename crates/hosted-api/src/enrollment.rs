//! Public enrollment wire contracts shared by community and hosted services.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const ENROLLMENT_KEY_BYTES: usize = 32;
pub const ENROLLMENT_SIGNATURE_BYTES: usize = 64;
pub const ENROLLMENT_TRANSFER_NONCE_BYTES: usize = 24;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("invalid base64url enrollment value")]
pub struct EnrollmentEncodingError;

/// Decodes an unpadded base64url enrollment value and rejects every other decoded length.
pub fn decode_base64url<const N: usize>(value: &str) -> Result<[u8; N], EnrollmentEncodingError> {
    use base64::Engine as _;
    let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|_| EnrollmentEncodingError)?;
    decoded.try_into().map_err(|_| EnrollmentEncodingError)
}

pub fn encode_base64url(bytes: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DeploymentMode {
    Community,
    Hosted,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ClientKind {
    Browser,
    Desktop,
    Android,
    Ios,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EnrollmentMethod {
    Transfer,
    SharedSecret,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EnrollmentState {
    Waiting,
    Claimed,
    Approved,
    Confirmed,
    Consumed,
    Expired,
    Denied,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EnrollmentRole {
    Owner,
    Device,
    Gateway,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PhonePairingQr {
    pub version: u8,
    pub purpose: String,
    pub origin: String,
    pub invitation_token: String,
    pub transfer_public_key: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct InstanceInfo {
    pub mode: DeploymentMode,
    pub claimed: bool,
    pub setup_required: bool,
    pub password_auth: bool,
    pub qr_pairing: bool,
    pub shared_secret_pairing: bool,
}

#[derive(Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct VaultMaterial {
    pub profile: serde_json::Value,
    pub check_header: String,
    pub fingerprint: String,
    pub key_epoch: u32,
    pub admission_public_key: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProposedDevice {
    pub device_id: Uuid,
    pub kind: ClientKind,
    pub name: String,
    pub signing_public_key: String,
}

#[derive(Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DeviceEnrollment {
    pub vault_id: Uuid,
    pub device_id: Uuid,
    pub role: EnrollmentRole,
    pub credential: String,
    pub material: VaultMaterial,
}

#[derive(Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ClaimInstanceRequest {
    pub operation_id: Uuid,
    pub setup_token: String,
    pub admin_password: String,
    pub device: ProposedDevice,
    pub material: VaultMaterial,
    pub credential_digest: String,
}

#[derive(Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct InstanceSession {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_token: Option<String>,
    pub csrf_token: Option<String>,
    pub expires_in_seconds: u32,
}

#[derive(Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ClaimInstanceResponse {
    pub vault_id: Uuid,
    pub device_id: Uuid,
    pub replayed: bool,
    pub session: InstanceSession,
}

#[derive(Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct InstanceLoginRequest {
    pub admin_password: String,
    pub client: ClientKind,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EnrollmentDevice {
    pub device_id: Uuid,
    pub name: String,
    pub kind: ClientKind,
    pub role: EnrollmentRole,
    pub revoked: bool,
    pub gateway_online: bool,
}

#[derive(Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct InstanceAccount {
    pub vault_id: Uuid,
    pub material: VaultMaterial,
    pub devices: Vec<EnrollmentDevice>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct InstanceManagement {
    pub vault_id: Uuid,
    pub devices: Vec<EnrollmentDevice>,
    pub storage_bytes: u64,
    pub quota_bytes: u64,
    pub replay_retention_days: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AdmissionChallengeRequest {
    pub device: ProposedDevice,
    pub vault_id: Option<Uuid>,
    pub credential_digest: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AdmissionChallenge {
    pub enrollment_id: Uuid,
    pub origin: String,
    pub vault_id: Uuid,
    pub profile: serde_json::Value,
    pub fingerprint: String,
    pub key_epoch: u32,
    pub device: ProposedDevice,
    pub requested_role: EnrollmentRole,
    pub nonce: String,
    pub credential_digest: String,
    pub expires_in_seconds: u32,
}

#[derive(Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CompleteAdmissionRequest {
    pub admission_signature: String,
    pub device_signature: String,
    pub credential_digest: String,
}

#[derive(Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AdmissionComplete {
    pub vault_id: Uuid,
    pub device_id: Uuid,
    pub role: EnrollmentRole,
    pub material: VaultMaterial,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateInvitationRequest {
    pub method: EnrollmentMethod,
    pub transfer_public_key: String,
    pub invitation_token_digest: String,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct InvitationCreated {
    pub invitation_id: Uuid,
    pub expires_in_seconds: u32,
}

#[derive(Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ClaimInvitationRequest {
    pub invitation_token_digest: String,
    pub claim_authenticator: String,
    pub device: ProposedDevice,
    pub transfer_public_key: String,
    pub credential_digest: String,
}

#[derive(Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct InvitationClaim {
    pub invitation_id: Uuid,
    pub claim_secret: String,
    pub origin: String,
    pub vault_id: Uuid,
    pub fingerprint: String,
    pub key_epoch: u32,
    pub method: EnrollmentMethod,
    pub owner_transfer_public_key: String,
    pub expires_in_seconds: u32,
}

/// Immutable phone enrollment context supplied only after authenticated invitation lookup.
#[derive(Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EnrollmentTranscript {
    pub origin: String,
    pub vault_id: Uuid,
    pub fingerprint: String,
    pub key_epoch: u32,
    pub enrollment_id: Uuid,
    pub nonce: String,
    pub owner_transfer_public_key: String,
    pub recipient_transfer_public_key: String,
    pub signing_public_key: String,
    pub device_id: Uuid,
    pub credential_digest: String,
    pub role: EnrollmentRole,
}
impl EnrollmentTranscript {
    /// Checks the exact decoded sizes before a binding derives a SAS or opens a handoff.
    pub fn validate_phone_fields(&self) -> Result<(), EnrollmentEncodingError> {
        if self.origin.is_empty()
            || !lowercase_hex(&self.fingerprint, ENROLLMENT_KEY_BYTES)
            || !lowercase_hex(&self.credential_digest, ENROLLMENT_KEY_BYTES)
        {
            return Err(EnrollmentEncodingError);
        }
        decode_base64url::<ENROLLMENT_KEY_BYTES>(&self.nonce)?;
        decode_base64url::<ENROLLMENT_KEY_BYTES>(&self.owner_transfer_public_key)?;
        decode_base64url::<ENROLLMENT_KEY_BYTES>(&self.recipient_transfer_public_key)?;
        decode_base64url::<ENROLLMENT_KEY_BYTES>(&self.signing_public_key)?;
        Ok(())
    }
}

fn lowercase_hex(value: &str, decoded_bytes: usize) -> bool {
    value.len() == decoded_bytes * 2
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
        && hex::decode(value).is_ok_and(|decoded| decoded.len() == decoded_bytes)
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct InvitationPublicClaim {
    pub device: ProposedDevice,
    pub transfer_public_key: String,
    pub credential_digest: String,
    pub claim_authenticator: String,
}

#[derive(Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct InvitationStatus {
    pub state: EnrollmentState,
    pub expires_in_seconds: u32,
    pub claim: Option<InvitationPublicClaim>,
    pub transcript: Option<EnrollmentTranscript>,
    pub encrypted_key_package: Option<String>,
    pub transfer_nonce: Option<String>,
    pub profile: Option<serde_json::Value>,
}

#[derive(Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ApproveInvitationRequest {
    pub transcript_hash: String,
    pub encrypted_key_package: Option<String>,
    pub transfer_nonce: Option<String>,
}

#[derive(Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ConfirmInvitationRequest {
    pub claim_secret: String,
    pub transcript_hash: String,
    pub device_signature: String,
    pub admission_signature: String,
}

pub mod routes {
    pub const INSTANCE: &str = "/v1/instance";
    pub const INSTANCE_CLAIM: &str = "/v1/instance/claim";
    pub const INSTANCE_SESSION: &str = "/v1/instance/session";
    pub const INSTANCE_ACCOUNT: &str = "/v1/instance/account";
    pub const INSTANCE_MANAGEMENT: &str = "/v1/instance/management";
    pub const ENROLLMENT_CHALLENGES: &str = "/v1/enrollment/challenges";
    pub const ENROLLMENT_INVITATIONS: &str = "/v1/enrollment/invitations";
    pub const HOSTED_ENROLLMENT_CONFIG: &str = "/hosted/v1/enrollment/config";
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phone_qr_has_the_exact_contract_keys() {
        let value = serde_json::to_value(PhonePairingQr {
            version: 1,
            purpose: "connect_phone".into(),
            origin: "https://example.test".into(),
            invitation_token: "token".into(),
            transfer_public_key: "key".into(),
        })
        .unwrap();
        assert_eq!(value.as_object().unwrap().len(), 5);
        assert!(serde_json::from_value::<PhonePairingQr>(serde_json::json!({"version":1,"purpose":"connect_phone","origin":"https://example.test","invitation_token":"token","transfer_public_key":"key","extra":true})).is_err());
    }

    #[test]
    fn enrollment_enums_are_snake_case() {
        assert_eq!(
            serde_json::to_string(&EnrollmentMethod::SharedSecret).unwrap(),
            "\"shared_secret\""
        );
        assert_eq!(
            serde_json::to_string(&EnrollmentRole::Gateway).unwrap(),
            "\"gateway\""
        );
    }

    #[test]
    fn binary_contract_values_require_exact_unpadded_base64url_lengths() {
        let encoded = encode_base64url(&[7; ENROLLMENT_KEY_BYTES]);
        assert_eq!(
            decode_base64url::<ENROLLMENT_KEY_BYTES>(&encoded).unwrap(),
            [7; 32]
        );
        assert!(decode_base64url::<ENROLLMENT_KEY_BYTES>("Bw==").is_err());
        assert!(decode_base64url::<ENROLLMENT_SIGNATURE_BYTES>(&encoded).is_err());
    }

    #[test]
    fn invitation_status_carries_the_complete_authenticated_transcript() {
        let transcript = EnrollmentTranscript {
            origin: "https://example.test".into(),
            vault_id: Uuid::nil(),
            fingerprint: "a".repeat(64),
            key_epoch: 1,
            enrollment_id: Uuid::from_u128(1),
            nonce: encode_base64url(&[1; ENROLLMENT_KEY_BYTES]),
            owner_transfer_public_key: encode_base64url(&[2; ENROLLMENT_KEY_BYTES]),
            recipient_transfer_public_key: encode_base64url(&[3; ENROLLMENT_KEY_BYTES]),
            signing_public_key: encode_base64url(&[4; ENROLLMENT_KEY_BYTES]),
            device_id: Uuid::from_u128(2),
            credential_digest: "b".repeat(64),
            role: EnrollmentRole::Gateway,
        };
        let status = InvitationStatus {
            state: EnrollmentState::Approved,
            expires_in_seconds: 300,
            claim: None,
            transcript: Some(transcript),
            encrypted_key_package: Some("ciphertext".into()),
            transfer_nonce: Some(encode_base64url(&[5; ENROLLMENT_TRANSFER_NONCE_BYTES])),
            profile: Some(serde_json::json!({"crypto_suite": 1})),
        };
        let value = serde_json::to_value(status).unwrap();
        assert!(value["transcript"].is_object());
        assert_eq!(value["transfer_nonce"].as_str().unwrap().len(), 32);
        let transcript: EnrollmentTranscript =
            serde_json::from_value(value["transcript"].clone()).unwrap();
        assert!(transcript.validate_phone_fields().is_ok());
    }

    #[test]
    fn browser_instance_session_omits_and_accepts_no_bearer_token() {
        let session: InstanceSession = serde_json::from_value(serde_json::json!({
            "csrf_token": "csrf",
            "expires_in_seconds": 900,
        }))
        .unwrap();
        assert_eq!(session.session_token, None);
        let serialized = serde_json::to_value(session).unwrap();
        assert!(serialized.get("session_token").is_none());
    }
}
