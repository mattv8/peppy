use peppy_domain::{DeviceId, VaultId};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct JoinRequestCreated {
    pub join_request_id: Uuid,
    pub poll_secret: String,
    pub expires_in_seconds: i64,
}

impl std::fmt::Debug for JoinRequestCreated {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("JoinRequestCreated")
            .field("join_request_id", &self.join_request_id)
            .field("poll_secret", &"[REDACTED]")
            .field("expires_in_seconds", &self.expires_in_seconds)
            .finish()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct JoinRequestQr {
    pub https_origin: String,
    pub join_request_id: Uuid,
    pub join_key: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct JoinRequestOffer {
    pub intent_digest: String,
    pub sealed_intent_token: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum JoinRequestState {
    Waiting,
    Offered,
    Expired,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct JoinRequestStatus {
    pub state: JoinRequestState,
    pub sealed_intent_token: Option<String>,
    pub intent_digest: Option<String>,
    pub expires_in_seconds: i64,
}

/// Lowercase SHA-256 digest of the 32-byte Ed25519 public key pinned by an intent.
pub fn pairing_key_digest(public_key: &[u8; 32]) -> String {
    Sha256::digest(public_key)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Six-digit SAS shared by the phone and owner after a pairing claim.
pub fn pairing_sas(intent_token: &str, key_digest: &str, device_id: DeviceId) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"peppy-pairing-sas-v1\0");
    hasher.update(intent_token.as_bytes());
    hasher.update(key_digest.as_bytes());
    hasher.update(device_id.0.as_bytes());
    let bytes = hasher.finalize();
    format!(
        "{:06}",
        u32::from_be_bytes(bytes[..4].try_into().expect("SHA-256 length")) % 1_000_000
    )
}

/// Canonical bytes that an enrolling device signs to prove possession of its
/// pinned Ed25519 private key. The challenge is the decoded 32-byte QR token;
/// role is the owner-approved role, never a consumer-supplied value.
pub fn pairing_proof_message(
    challenge: &[u8; 32],
    vault_id: VaultId,
    device_id: DeviceId,
    profile_fingerprint: &str,
    key_epoch: u32,
    approved_role: &str,
) -> Vec<u8> {
    let mut message = Vec::with_capacity(160);
    message.extend_from_slice(b"peppy-pairing-proof-v1\0");
    message.extend_from_slice(challenge);
    message.extend_from_slice(vault_id.0.as_bytes());
    message.extend_from_slice(device_id.0.as_bytes());
    message.extend_from_slice(profile_fingerprint.as_bytes());
    message.extend_from_slice(&key_epoch.to_be_bytes());
    message.extend_from_slice(approved_role.as_bytes());
    message
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn framing_is_stable() {
        let bytes = pairing_proof_message(
            &[7; 32],
            "00000000-0000-0000-0000-000000000000".parse().unwrap(),
            "00000000-0000-0000-0000-000000000000".parse().unwrap(),
            &"a".repeat(64),
            1,
            "gateway",
        );
        let prefix = b"peppy-pairing-proof-v1\0";
        assert!(bytes.starts_with(prefix));
        assert_eq!(bytes.len(), prefix.len() + 32 + 16 + 16 + 64 + 4 + 7);
    }

    #[test]
    fn join_request_created_debug_redacts_poll_secret() {
        let created = JoinRequestCreated {
            join_request_id: Uuid::nil(),
            poll_secret: "sensitive-poll-secret".into(),
            expires_in_seconds: 300,
        };

        let debug = format!("{created:?}");
        assert!(debug.contains("[REDACTED]"));
        assert!(!debug.contains(&created.poll_secret));
    }

    #[test]
    fn sas_vector_is_stable() {
        let key = [7; 32];
        let digest = pairing_key_digest(&key);
        assert_eq!(
            digest,
            "4bb06f8e4e3a7715d201d573d0aa423762e55dabd61a2c02278fa56cc6d294e0"
        );
        assert_eq!(
            pairing_sas(
                "intent-1",
                &digest,
                "00000000-0000-0000-0000-000000000002".parse().unwrap()
            ),
            "122032"
        );
    }
}
