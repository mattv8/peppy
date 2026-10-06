//! Pure hosted enrollment, claimant, and desktop join-request primitives.

use base64::{Engine as _, engine::general_purpose::STANDARD};
use peppy_client_core::DeviceId;
use peppy_crypto::{
    KeyProfile, VaultCheckHeader, create_vault_check_header, derive_root_key,
    verify_vault_check_header,
};
use peppy_hosted_api::{
    AccountResponse, CompleteProvisioningRequest, CompleteProvisioningResponse, IdentityProvider,
    LoginAttemptRequest, LoginAttemptResponse, ProvisioningGrantResponse, ProvisioningRequest,
    SessionExchangeRequest, SessionResponse,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{fmt, sync::Mutex};
use url::Url;
use uuid::Uuid;
use zeroize::{Zeroize, Zeroizing};

const MAX_JSON: usize = 16 * 1024;
const MAX_CHECKPOINT: usize = 64 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum HostedClientError {
    #[error("invalid hosted-client request")]
    InvalidRequest,
    #[error("invalid profile")]
    InvalidProfile,
    #[error("cryptographic operation failed")]
    Crypto,
}

fn invalid<T>() -> Result<T, HostedClientError> {
    Err(HostedClientError::InvalidRequest)
}
fn bounded(value: &str, max: usize) -> Result<(), HostedClientError> {
    if value.is_empty() || value.len() > max {
        return invalid();
    }
    Ok(())
}
fn uuid(value: &str) -> Result<Uuid, HostedClientError> {
    Uuid::parse_str(value).map_err(|_| HostedClientError::InvalidRequest)
}
fn hex(value: &str, len: usize, prefix: Option<&str>) -> bool {
    let digits = match prefix {
        Some(prefix) if value.starts_with(prefix) => &value[prefix.len()..],
        Some(_) => return false,
        None => value,
    };
    value.len() == len
        && digits
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn canonical_origin(value: &str) -> Result<String, HostedClientError> {
    bounded(value, 2048)?;
    let url = Url::parse(value).map_err(|_| HostedClientError::InvalidRequest)?;
    if url.scheme() != "https"
        || url.username() != ""
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.host_str().is_none()
        || url.path() != "/" && !url.path().is_empty()
    {
        return invalid();
    }
    let canonical = url.origin().ascii_serialization();
    if value != canonical && value != format!("{canonical}/") {
        return invalid();
    }
    Ok(canonical)
}
fn token_digest(token: &str) -> String {
    format!("{:x}", Sha256::digest(token.as_bytes()))
}

pub struct HostedLoginAttempt {
    attempt_id: String,
    nonce: String,
    expires_in_seconds: u32,
}
pub struct HostedSession {
    account_id: String,
    bearer_token: Zeroizing<String>,
    expires_in_seconds: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostedAccount {
    pub account_id: String,
    pub classification: String,
    pub entitlement: String,
    pub access: String,
    pub vault_id: Option<String>,
    pub operation_id: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostedProvisioningView {
    pub origin: String,
    pub account_id: String,
    pub operation_id: String,
    pub vault_id: String,
    pub device_id: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Checkpoint {
    version: u8,
    origin: String,
    account_id: String,
    operation_id: String,
    vault_id: String,
    device_id: String,
    token: String,
    profile: KeyProfile,
    header: VaultCheckHeader,
    profile_fingerprint: String,
    grant: Option<String>,
}

impl Drop for Checkpoint {
    fn drop(&mut self) {
        self.token.zeroize();
        if let Some(ref mut grant) = self.grant {
            grant.zeroize();
        }
    }
}

pub struct HostedProvisioning {
    origin: String,
    account_id: String,
    operation_id: Uuid,
    vault_id: Uuid,
    device_id: Uuid,
    token: Zeroizing<String>,
    profile: KeyProfile,
    header: VaultCheckHeader,
    grant: Mutex<Option<Zeroizing<String>>>,
}
impl fmt::Debug for HostedProvisioning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HostedProvisioning")
            .field("origin", &self.origin)
            .field("account_id", &self.account_id)
            .field("operation_id", &self.operation_id)
            .field("vault_id", &self.vault_id)
            .field("device_id", &self.device_id)
            .finish()
    }
}

pub fn hosted_login_request(provider: String) -> Result<String, HostedClientError> {
    let provider = match provider.as_str() {
        "google" => IdentityProvider::Google,
        "apple" => IdentityProvider::Apple,
        _ => return invalid(),
    };
    serde_json::to_string(&LoginAttemptRequest { provider })
        .map_err(|_| HostedClientError::InvalidRequest)
}
pub fn hosted_session_request(
    attempt_id: String,
    id_token: String,
) -> Result<String, HostedClientError> {
    bounded(&id_token, MAX_JSON)?;
    let attempt_id = uuid(&attempt_id)?;
    serde_json::to_string(&SessionExchangeRequest {
        attempt_id,
        id_token,
    })
    .map_err(|_| HostedClientError::InvalidRequest)
}
pub fn parse_hosted_login_attempt(json: String) -> Result<HostedLoginAttempt, HostedClientError> {
    bounded(&json, MAX_JSON)?;
    let value: LoginAttemptResponse =
        serde_json::from_str(&json).map_err(|_| HostedClientError::InvalidRequest)?;
    if !hex(&value.nonce, 68, Some("pst_")) || !(1..=600).contains(&value.expires_in_seconds) {
        return invalid();
    }
    Ok(HostedLoginAttempt {
        attempt_id: value.attempt_id.to_string(),
        nonce: value.nonce,
        expires_in_seconds: value.expires_in_seconds,
    })
}
pub fn parse_hosted_session(json: String) -> Result<HostedSession, HostedClientError> {
    bounded(&json, MAX_JSON)?;
    let value: SessionResponse =
        serde_json::from_str(&json).map_err(|_| HostedClientError::InvalidRequest)?;
    if !hex(&value.session_token, 68, Some("pst_"))
        || !(1..=86_400).contains(&value.expires_in_seconds)
    {
        return invalid();
    }
    Ok(HostedSession {
        account_id: value.account_id.to_string(),
        bearer_token: Zeroizing::new(value.session_token),
        expires_in_seconds: value.expires_in_seconds,
    })
}
pub fn parse_hosted_account(
    json: String,
    expected_account_id: String,
) -> Result<HostedAccount, HostedClientError> {
    bounded(&json, MAX_JSON)?;
    let expected = uuid(&expected_account_id)?;
    let value: AccountResponse =
        serde_json::from_str(&json).map_err(|_| HostedClientError::InvalidRequest)?;
    if value.account_id != expected {
        return invalid();
    }
    Ok(HostedAccount {
        account_id: value.account_id.to_string(),
        classification: value.classification.to_string(),
        entitlement: value.entitlement.to_string(),
        access: value.access.to_string(),
        vault_id: value.vault_id.map(|x| x.to_string()),
        operation_id: value.operation_id.map(|x| x.to_string()),
    })
}
pub fn generate_hosted_passphrase() -> String {
    peppy_crypto::passphrase::generate_passphrase()
}
pub fn hosted_passphrase_acceptable(mut passphrase: String) -> bool {
    let acceptable = peppy_crypto::passphrase::passphrase_acceptable(&passphrase);
    passphrase.zeroize();
    acceptable
}

/// Computes the owner-visible SAS from the claimant's pinned intent facts.
/// Validates a serialized vault key profile and returns its canonical fingerprint.
/// Native hosts use this shared primitive when comparing local profile metadata.
pub fn vault_profile_fingerprint(profile_json: String) -> Result<String, HostedClientError> {
    bounded(&profile_json, MAX_JSON)?;
    let profile: KeyProfile =
        serde_json::from_str(&profile_json).map_err(|_| HostedClientError::InvalidProfile)?;
    profile
        .fingerprint()
        .map_err(|_| HostedClientError::InvalidProfile)
}

pub fn pairing_intent_sas(
    intent_token: String,
    key_digest: String,
    device_id: String,
) -> Result<String, HostedClientError> {
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;

    if intent_token.len() != 43
        || URL_SAFE_NO_PAD
            .decode(&intent_token)
            .ok()
            .is_none_or(|bytes| bytes.len() != 32)
        || !hex(&key_digest, 64, None)
    {
        return invalid();
    }
    let parsed = uuid(&device_id)?;
    if parsed.to_string() != device_id {
        return invalid();
    }
    Ok(peppy_protocol::pairing_sas(
        &intent_token,
        &key_digest,
        DeviceId(parsed),
    ))
}

pub fn prepare(
    origin: impl AsRef<str>,
    account_id: impl AsRef<str>,
    passphrase: impl AsRef<str>,
) -> Result<HostedProvisioning, HostedClientError> {
    let origin = canonical_origin(origin.as_ref())?;
    let account = uuid(account_id.as_ref())?;
    let passphrase = Zeroizing::new(passphrase.as_ref().to_owned());
    if !peppy_crypto::passphrase::passphrase_acceptable(&passphrase) {
        return invalid();
    }
    let vault_id = Uuid::new_v4();
    let device_id = Uuid::new_v4();
    let operation_id = Uuid::new_v4();
    let profile = KeyProfile::new(vault_id, 1).map_err(|_| HostedClientError::Crypto)?;
    let root = derive_root_key(&passphrase, &profile).map_err(|_| HostedClientError::Crypto)?;
    let header =
        create_vault_check_header(&root, profile.clone()).map_err(|_| HostedClientError::Crypto)?;
    let mut bytes = [0u8; 48];
    libsodium_rs::ensure_init().map_err(|_| HostedClientError::Crypto)?;
    libsodium_rs::random::fill_bytes(&mut bytes);
    let token = Zeroizing::new(bytes.iter().map(|x| format!("{x:02x}")).collect());
    bytes.zeroize();
    Ok(HostedProvisioning {
        origin,
        account_id: account.to_string(),
        operation_id,
        vault_id,
        device_id,
        token,
        profile,
        header,
        grant: Mutex::new(None),
    })
}

fn checkpoint(value: &HostedProvisioning) -> Result<Vec<u8>, HostedClientError> {
    let grant = value.grant.lock().map_err(|_| HostedClientError::Crypto)?;
    serde_json::to_vec(&Checkpoint {
        version: 1,
        origin: value.origin.clone(),
        account_id: value.account_id.clone(),
        operation_id: value.operation_id.to_string(),
        vault_id: value.vault_id.to_string(),
        device_id: value.device_id.to_string(),
        token: value.token.to_string(),
        profile: value.profile.clone(),
        header: value.header.clone(),
        profile_fingerprint: value
            .profile
            .fingerprint()
            .map_err(|_| HostedClientError::Crypto)?,
        grant: grant.as_ref().map(|x| x.to_string()),
    })
    .map_err(|_| HostedClientError::Crypto)
}
pub fn restore(
    checkpoint_bytes: Vec<u8>,
    expected_origin: impl AsRef<str>,
    expected_account_id: impl AsRef<str>,
) -> Result<HostedProvisioning, HostedClientError> {
    let checkpoint_bytes = Zeroizing::new(checkpoint_bytes);
    if checkpoint_bytes.is_empty() || checkpoint_bytes.len() > MAX_CHECKPOINT {
        return invalid();
    }
    let origin = canonical_origin(expected_origin.as_ref())?;
    let account = uuid(expected_account_id.as_ref())?;
    let mut value: Checkpoint =
        serde_json::from_slice(&checkpoint_bytes).map_err(|_| HostedClientError::InvalidRequest)?;
    if value.version != 1
        || value.origin != origin
        || value.account_id != account.to_string()
        || !hex(&value.token, 96, None)
        || value.profile.key_epoch != 1
        || value.profile.vault_id.to_string() != value.vault_id
        || value
            .profile
            .fingerprint()
            .map_err(|_| HostedClientError::InvalidRequest)?
            != value.profile_fingerprint
    {
        return invalid();
    }
    let operation_id = uuid(&value.operation_id)?;
    let vault_id = uuid(&value.vault_id)?;
    let device_id = uuid(&value.device_id)?;
    // Validate canonical UUID strings match parsed UUIDs (no nil UUIDs)
    if operation_id.to_string() != value.operation_id
        || vault_id.to_string() != value.vault_id
        || device_id.to_string() != value.device_id
        || operation_id.is_nil()
        || vault_id.is_nil()
        || device_id.is_nil()
    {
        return invalid();
    }
    if value.header.profile != value.profile
        || value
            .grant
            .as_ref()
            .is_some_and(|g| !hex(g, 68, Some("pgr_")))
    {
        return invalid();
    }
    Ok(HostedProvisioning {
        origin,
        account_id: account.to_string(),
        operation_id,
        vault_id,
        device_id,
        token: Zeroizing::new(std::mem::take(&mut value.token)),
        profile: value.profile.clone(),
        header: value.header.clone(),
        grant: Mutex::new(value.grant.take().map(Zeroizing::new)),
    })
}

impl HostedLoginAttempt {
    pub fn attempt_id(&self) -> String {
        self.attempt_id.clone()
    }
    pub fn nonce(&self) -> String {
        self.nonce.clone()
    }
    pub fn expires_in_seconds(&self) -> u32 {
        self.expires_in_seconds
    }
}
impl HostedSession {
    pub fn account_id(&self) -> String {
        self.account_id.clone()
    }
    pub fn bearer_token(&self) -> String {
        self.bearer_token.to_string()
    }
    pub fn expires_in_seconds(&self) -> u32 {
        self.expires_in_seconds
    }
}
impl HostedProvisioning {
    pub fn view(&self) -> HostedProvisioningView {
        HostedProvisioningView {
            origin: self.origin.clone(),
            account_id: self.account_id.clone(),
            operation_id: self.operation_id.to_string(),
            vault_id: self.vault_id.to_string(),
            device_id: self.device_id.to_string(),
        }
    }
    pub fn checkpoint(&self) -> Result<Vec<u8>, HostedClientError> {
        checkpoint(self)
    }
    pub fn grant_request(&self) -> Result<String, HostedClientError> {
        serde_json::to_string(&ProvisioningRequest {
            operation_id: self.operation_id,
        })
        .map_err(|_| HostedClientError::InvalidRequest)
    }
    pub fn accept_grant(&self, response_json: String) -> Result<(), HostedClientError> {
        bounded(&response_json, MAX_JSON)?;
        let value: ProvisioningGrantResponse =
            serde_json::from_str(&response_json).map_err(|_| HostedClientError::InvalidRequest)?;
        if !hex(&value.grant, 68, Some("pgr_")) || !(1..=600).contains(&value.expires_in_seconds) {
            return invalid();
        }
        *self.grant.lock().map_err(|_| HostedClientError::Crypto)? =
            Some(Zeroizing::new(value.grant));
        Ok(())
    }
    pub fn has_grant(&self) -> bool {
        self.grant.lock().is_ok_and(|g| g.is_some())
    }
    /// Forgets an expired or rejected grant so the same operation and material can request a new
    /// one; everything that identifies the vault and device stays unchanged.
    pub fn clear_grant(&self) -> Result<(), HostedClientError> {
        *self.grant.lock().map_err(|_| HostedClientError::Crypto)? = None;
        Ok(())
    }
    pub fn complete_request(&self) -> Result<String, HostedClientError> {
        let grant = self.grant.lock().map_err(|_| HostedClientError::Crypto)?;
        let grant = grant.as_ref().ok_or(HostedClientError::InvalidRequest)?;
        let request = CompleteProvisioningRequest {
            operation_id: self.operation_id,
            grant: grant.to_string(),
            device_id: self.device_id,
            credential_digest: token_digest(&self.token),
            public_key_profile: serde_json::to_value(&self.profile)
                .map_err(|_| HostedClientError::Crypto)?,
            encrypted_vault_check_header: STANDARD
                .encode(serde_json::to_vec(&self.header).map_err(|_| HostedClientError::Crypto)?),
            profile_fingerprint: self
                .profile
                .fingerprint()
                .map_err(|_| HostedClientError::Crypto)?,
            key_epoch: 1,
        };
        serde_json::to_string(&request).map_err(|_| HostedClientError::Crypto)
    }
    pub fn credential_json(
        &self,
        complete_response_json: String,
    ) -> Result<String, HostedClientError> {
        bounded(&complete_response_json, MAX_JSON)?;
        let response: CompleteProvisioningResponse = serde_json::from_str(&complete_response_json)
            .map_err(|_| HostedClientError::InvalidRequest)?;
        if response.operation_id != self.operation_id
            || response.vault_id != self.vault_id
            || response.device_id != self.device_id
        {
            return invalid();
        }
        serde_json::to_string(&serde_json::json!({"version": 1, "origin": self.origin, "vaultId": self.vault_id.to_string(), "deviceId": self.device_id.to_string(), "deviceToken": self.token.to_string()})).map_err(|_| HostedClientError::Crypto)
    }
    pub fn passphrase_matches(&self, passphrase: &str) -> Result<bool, HostedClientError> {
        let passphrase = Zeroizing::new(passphrase.to_owned());
        let root = derive_root_key(&passphrase, &self.profile);
        match root {
            Ok(root) => Ok(verify_vault_check_header(&root, &self.profile, &self.header).is_ok()),
            Err(_) => Ok(false),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;

    const PASSPHRASE: &str = "alpha bravo charlie delta echo foxtrot";
    const ORIGIN: &str = "https://peppy.example";

    #[test]
    fn provisioning_material_unlocks_serializes_and_restores_without_plaintext() {
        let account = Uuid::new_v4().to_string();
        let prepared =
            prepare(ORIGIN, account.clone(), PASSPHRASE).expect("valid local preparation");
        assert!(prepared.passphrase_matches(PASSPHRASE).unwrap());
        assert!(
            !prepared
                .passphrase_matches("wrong phrase with sufficient length here")
                .unwrap()
        );
        let grant = format!("pgr_{}", "a".repeat(64));
        prepared
            .accept_grant(serde_json::json!({"grant": grant, "expires_in_seconds": 60}).to_string())
            .unwrap();
        let complete = prepared.complete_request().unwrap();
        assert!(!complete.contains(PASSPHRASE));
        assert!(!complete.contains("pst_"));
        let checkpoint = prepared.checkpoint().unwrap();
        let restored = restore(checkpoint, ORIGIN, account).unwrap();
        assert_eq!(complete, restored.complete_request().unwrap());
        assert!(
            restore(
                vec![b'{'; MAX_CHECKPOINT + 1],
                ORIGIN,
                Uuid::new_v4().to_string()
            )
            .is_err()
        );
    }

    #[test]
    fn session_and_account_contracts_fail_closed() {
        let account = Uuid::new_v4();
        assert!(parse_hosted_session(
            serde_json::json!({"account_id": account, "session_token": "missing", "expires_in_seconds": 60}).to_string()
        ).is_err());
        assert!(parse_hosted_account(
            serde_json::json!({"account_id": account, "classification": "new", "entitlement": "none", "access": "read_write"}).to_string(),
            Uuid::new_v4().to_string()
        ).is_err());
    }

    #[test]
    pub fn vault_profile_fingerprint_validates_and_matches_crypto_vector() {
        let profile = KeyProfile {
            crypto_suite: peppy_crypto::CRYPTO_SUITE_1,
            salt: [7; 16],
            vault_id: Uuid::nil(),
            key_epoch: 9,
        };
        let json = serde_json::to_string(&profile).unwrap();
        assert_eq!(
            vault_profile_fingerprint(json).unwrap(),
            "5ce360c831ed3dc1c6616e8da679df33bebfa29f98227adf707b2f2fa2abf134"
        );
        assert!(vault_profile_fingerprint("not json".into()).is_err());
        assert!(vault_profile_fingerprint("{}".into()).is_err());
    }

    #[test]
    fn owner_pairing_sas_validates_and_matches_protocol_vector() {
        let token = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
        let digest = "4bb06f8e4e3a7715d201d573d0aa423762e55dabd61a2c02278fa56cc6d294e0";
        let device = "00000000-0000-0000-0000-000000000002";
        assert_eq!(
            pairing_intent_sas(token.into(), digest.into(), device.into()).unwrap(),
            peppy_protocol::pairing_sas(token, digest, DeviceId(uuid(device).unwrap()))
        );
        assert!(pairing_intent_sas("not-a-token".into(), digest.into(), device.into()).is_err());
        assert!(pairing_intent_sas(token.into(), "A".repeat(64), device.into()).is_err());
        assert!(pairing_intent_sas(token.into(), digest.into(), format!("{{{device}}}")).is_err());
    }

    #[test]
    fn rejects_complete_response_with_wrong_operation_vault_device() {
        let account = Uuid::new_v4().to_string();
        let prepared =
            prepare(ORIGIN, account.clone(), PASSPHRASE).expect("valid local preparation");
        let grant = format!("pgr_{}", "a".repeat(64));
        prepared
            .accept_grant(serde_json::json!({"grant": grant, "expires_in_seconds": 60}).to_string())
            .unwrap();
        let complete = prepared.complete_request().unwrap();

        // Test wrong operation_id
        let wrong_op = serde_json::json!({
            "operation_id": Uuid::new_v4().to_string(),
            "vault_id": prepared.vault_id.to_string(),
            "device_id": prepared.device_id.to_string(),
        });
        assert!(prepared.credential_json(wrong_op.to_string()).is_err());

        // Test wrong vault_id
        let wrong_vault = serde_json::json!({
            "operation_id": prepared.operation_id.to_string(),
            "vault_id": Uuid::new_v4().to_string(),
            "device_id": prepared.device_id.to_string(),
        });
        assert!(prepared.credential_json(wrong_vault.to_string()).is_err());

        // Test wrong device_id
        let wrong_device = serde_json::json!({
            "operation_id": prepared.operation_id.to_string(),
            "vault_id": prepared.vault_id.to_string(),
            "device_id": Uuid::new_v4().to_string(),
        });
        assert!(prepared.credential_json(wrong_device.to_string()).is_err());

        // Verify material still intact after failures
        assert_eq!(complete, prepared.complete_request().unwrap());
    }

    #[test]
    fn rejects_checkpoint_origin_and_account_mismatch() {
        let account = Uuid::new_v4().to_string();
        let prepared =
            prepare(ORIGIN, account.clone(), PASSPHRASE).expect("valid local preparation");
        let checkpoint = prepared.checkpoint().unwrap();

        // Wrong origin
        assert!(restore(checkpoint.clone(), "https://wrong.example", account.clone()).is_err());

        // Wrong account
        assert!(restore(checkpoint, ORIGIN, Uuid::new_v4().to_string()).is_err());
    }

    #[test]
    fn rejects_noncanonical_device_operation_vault_uuids() {
        let account = Uuid::new_v4().to_string();
        let prepared =
            prepare(ORIGIN, account.clone(), PASSPHRASE).expect("valid local preparation");
        let checkpoint = prepared.checkpoint().unwrap();

        let mut value: Checkpoint = serde_json::from_slice(&checkpoint).unwrap();
        let original_op = value.operation_id.clone();

        // Non-canonical operation UUID (uppercase)
        value.operation_id = original_op.to_uppercase();
        let tampered_json = serde_json::to_vec(&value).unwrap();
        assert!(
            restore(tampered_json, ORIGIN, account.clone()).is_err(),
            "non-canonical operation UUID should be rejected"
        );

        // Restore original for next test
        value.operation_id = original_op.clone();

        // Nil UUID in vault field
        value.vault_id = "00000000-0000-0000-0000-000000000000".to_string();
        let nil_json = serde_json::to_vec(&value).unwrap();
        assert!(
            restore(nil_json, ORIGIN, account.clone()).is_err(),
            "nil vault UUID should be rejected"
        );
    }

    #[test]
    fn rejects_tampered_profile_and_header() {
        let account = Uuid::new_v4().to_string();
        let prepared =
            prepare(ORIGIN, account.clone(), PASSPHRASE).expect("valid local preparation");
        let checkpoint = prepared.checkpoint().unwrap();

        let mut value: Checkpoint = serde_json::from_slice(&checkpoint).unwrap();
        let original_profile = value.profile.clone();

        // Tamper with profile key_epoch
        value.profile.key_epoch = 2;
        let tampered_json = serde_json::to_vec(&value).unwrap();
        assert!(
            restore(tampered_json, ORIGIN, account.clone()).is_err(),
            "tampered key_epoch should be rejected"
        );

        // Restore original and test header mismatch
        value.profile = original_profile.clone();
        value.header.profile = KeyProfile::new(Uuid::new_v4(), 1).expect("valid profile");
        let header_mismatch = serde_json::to_vec(&value).unwrap();
        assert!(
            restore(header_mismatch, ORIGIN, account.clone()).is_err(),
            "mismatched header profile should be rejected"
        );
    }

    #[test]
    fn join_qr_roundtrip_and_validation() {
        let (_, key) = generate_join_key().unwrap();
        let qr = JoinRequestQr {
            https_origin: ORIGIN.into(),
            join_request_id: Uuid::new_v4(),
            join_key: URL_SAFE_NO_PAD.encode(key),
        };
        let encoded = encode_join_request_qr(&qr);
        assert_eq!(parse_join_request_qr(&encoded, false).unwrap(), qr);
        for payload in [
            encoded.replace(ORIGIN, "http://peppy.example"),
            encoded.replace(ORIGIN, "https://peppy.example/path"),
            encoded.replace('}', ",\"extra\":true}"),
        ] {
            assert!(parse_join_request_qr(&payload, false).is_err());
        }
        for bytes in [[0u8; 31].as_slice(), [0u8; 33].as_slice()] {
            let invalid = JoinRequestQr {
                join_key: URL_SAFE_NO_PAD.encode(bytes),
                ..qr.clone()
            };
            assert!(parse_join_request_qr(&encode_join_request_qr(&invalid), false).is_err());
        }
    }

    #[test]
    fn join_seal_open_and_digest_contract() {
        let (secret, public) = generate_join_key().unwrap();
        let token = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
        let sealed = seal_intent_token(&public, token).unwrap();
        assert_eq!(
            &*open_intent_token(&secret, &public, &sealed).unwrap(),
            token
        );
        let (wrong, wrong_public) = generate_join_key().unwrap();
        assert!(open_intent_token(&wrong, &wrong_public, &sealed).is_err());
        let bad = seal_intent_token(&public, "not-43-characters").unwrap();
        assert!(open_intent_token(&secret, &public, &bad).is_err());
        assert_eq!(
            intent_digest_hex("hello"),
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
    }
}

pub mod claim {
    use super::HostedClientError;
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    use peppy_client_core::{DeviceId, VaultId};
    use zeroize::Zeroizing;

    pub fn pairing_proof_bytes(
        challenge_token: &str,
        vault_id: &str,
        device_id: &str,
        profile_fingerprint: &str,
        key_epoch: u32,
        approved_role: &str,
    ) -> Result<Vec<u8>, HostedClientError> {
        let challenge: [u8; 32] = URL_SAFE_NO_PAD
            .decode(challenge_token)
            .map_err(|_| HostedClientError::InvalidRequest)?
            .try_into()
            .map_err(|_| HostedClientError::InvalidRequest)?;
        let vault_id = VaultId(
            uuid::Uuid::parse_str(vault_id).map_err(|_| HostedClientError::InvalidRequest)?,
        );
        let device_id = DeviceId(
            uuid::Uuid::parse_str(device_id).map_err(|_| HostedClientError::InvalidRequest)?,
        );
        if key_epoch == 0 || profile_fingerprint.len() != 64 || approved_role.is_empty() {
            return Err(HostedClientError::InvalidRequest);
        }
        Ok(peppy_protocol::pairing_proof_message(
            &challenge,
            vault_id,
            device_id,
            profile_fingerprint,
            key_epoch,
            approved_role,
        ))
    }

    pub struct EnrollmentKey {
        seed: Zeroizing<[u8; 32]>,
    }
    impl EnrollmentKey {
        pub fn generate() -> Result<Self, HostedClientError> {
            libsodium_rs::ensure_init().map_err(|_| HostedClientError::Crypto)?;
            let mut seed = [0; 32];
            libsodium_rs::random::fill_bytes(&mut seed);
            Ok(Self {
                seed: Zeroizing::new(seed),
            })
        }
        pub fn from_seed(seed: &[u8]) -> Result<Self, HostedClientError> {
            let seed: [u8; 32] = seed
                .try_into()
                .map_err(|_| HostedClientError::InvalidRequest)?;
            key(&seed)?;
            Ok(Self {
                seed: Zeroizing::new(seed),
            })
        }
        pub fn export_seed_for_native_secure_storage(&self) -> Vec<u8> {
            self.seed.to_vec()
        }
        pub fn public_key_base64url(&self) -> Result<String, HostedClientError> {
            Ok(URL_SAFE_NO_PAD.encode(key(&self.seed[..])?.public_key.as_bytes()))
        }
        pub fn sign_pairing_proof(&self, proof: &[u8]) -> Result<String, HostedClientError> {
            let key = key(&self.seed[..])?;
            let signature = libsodium_rs::crypto_sign::sign_detached(proof, &key.secret_key)
                .map_err(|_| HostedClientError::Crypto)?;
            Ok(URL_SAFE_NO_PAD.encode(signature))
        }
        pub fn pairing_sas(
            &self,
            intent_token: &str,
            device_id: &str,
            server_key_digest: &str,
        ) -> Result<String, HostedClientError> {
            if intent_token.is_empty() {
                return Err(HostedClientError::InvalidRequest);
            }
            let device_id = DeviceId(
                uuid::Uuid::parse_str(device_id).map_err(|_| HostedClientError::InvalidRequest)?,
            );
            let key = key(&self.seed[..])?;
            let digest = peppy_protocol::pairing_key_digest(key.public_key.as_bytes());
            if digest != server_key_digest {
                return Err(HostedClientError::InvalidRequest);
            }
            Ok(peppy_protocol::pairing_sas(
                intent_token,
                &digest,
                device_id,
            ))
        }
    }
    fn key(seed: &[u8]) -> Result<libsodium_rs::crypto_sign::KeyPair, HostedClientError> {
        let seed: [u8; 32] = seed
            .try_into()
            .map_err(|_| HostedClientError::InvalidRequest)?;
        libsodium_rs::ensure_init().map_err(|_| HostedClientError::Crypto)?;
        libsodium_rs::crypto_sign::KeyPair::from_seed(&seed).map_err(|_| HostedClientError::Crypto)
    }
}

pub mod join {
    use super::HostedClientError;
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    use serde::{Deserialize, Serialize};
    use sha2::{Digest, Sha256};
    use std::fmt;
    use url::Url;
    use uuid::Uuid;
    use zeroize::Zeroizing;

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct JoinRequestQr {
        pub https_origin: String,
        pub join_request_id: Uuid,
        pub join_key: String,
    }
    pub struct JoinKeySecret(Zeroizing<[u8; 32]>);
    impl fmt::Debug for JoinKeySecret {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("JoinKeySecret(<redacted>)")
        }
    }
    pub fn encode_join_request_qr(qr: &JoinRequestQr) -> String {
        serde_json::to_string(qr).expect("JoinRequestQr serializes")
    }
    pub fn parse_join_request_qr(
        payload: &str,
        allow_loopback_http: bool,
    ) -> Result<JoinRequestQr, HostedClientError> {
        let qr: JoinRequestQr =
            serde_json::from_str(payload).map_err(|_| HostedClientError::InvalidRequest)?;
        canonical_origin(&qr.https_origin, allow_loopback_http)?;
        let key = URL_SAFE_NO_PAD
            .decode(&qr.join_key)
            .map_err(|_| HostedClientError::InvalidRequest)?;
        if key.len() != 32 {
            return Err(HostedClientError::InvalidRequest);
        }
        Ok(qr)
    }
    pub fn generate_join_key() -> Result<(JoinKeySecret, [u8; 32]), HostedClientError> {
        libsodium_rs::ensure_init().map_err(|_| HostedClientError::Crypto)?;
        let key = libsodium_rs::crypto_box::KeyPair::generate();
        Ok((
            JoinKeySecret(Zeroizing::new(*key.secret_key.as_bytes())),
            *key.public_key.as_bytes(),
        ))
    }
    pub fn seal_intent_token(
        join_key: &[u8; 32],
        intent_token: &str,
    ) -> Result<Vec<u8>, HostedClientError> {
        let public = libsodium_rs::crypto_box::PublicKey::from_bytes(join_key)
            .map_err(|_| HostedClientError::InvalidRequest)?;
        libsodium_rs::crypto_box::seal_box(intent_token.as_bytes(), &public)
            .map_err(|_| HostedClientError::Crypto)
    }
    pub fn open_intent_token(
        secret: &JoinKeySecret,
        public: &[u8; 32],
        sealed: &[u8],
    ) -> Result<Zeroizing<String>, HostedClientError> {
        let public = libsodium_rs::crypto_box::PublicKey::from_bytes(public)
            .map_err(|_| HostedClientError::InvalidRequest)?;
        let private = libsodium_rs::crypto_box::SecretKey::from_bytes(&secret.0[..])
            .map_err(|_| HostedClientError::Crypto)?;
        let opened = libsodium_rs::crypto_box::open_sealed_box(sealed, &public, &private)
            .map_err(|_| HostedClientError::Crypto)?;
        let opened = String::from_utf8(opened).map_err(|_| HostedClientError::InvalidRequest)?;
        if opened.len() != 43
            || URL_SAFE_NO_PAD
                .decode(&opened)
                .ok()
                .is_none_or(|b| b.len() != 32)
        {
            return Err(HostedClientError::InvalidRequest);
        }
        Ok(Zeroizing::new(opened))
    }
    pub fn intent_digest_hex(intent_token: &str) -> String {
        format!("{:x}", Sha256::digest(intent_token.as_bytes()))
    }
    fn canonical_origin(value: &str, allow_loopback_http: bool) -> Result<(), HostedClientError> {
        let url = Url::parse(value).map_err(|_| HostedClientError::InvalidRequest)?;
        let loopback = matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"));
        if !(url.scheme() == "https" || allow_loopback_http && url.scheme() == "http" && loopback)
            || url.username() != ""
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || url.host_str().is_none()
            || !(url.path().is_empty() || url.path() == "/")
        {
            return Err(HostedClientError::InvalidRequest);
        }
        let canonical = url.origin().ascii_serialization();
        if value != canonical && value != format!("{canonical}/") {
            return Err(HostedClientError::InvalidRequest);
        }
        Ok(())
    }
}
pub use join::{
    JoinKeySecret, JoinRequestQr, encode_join_request_qr, generate_join_key, intent_digest_hex,
    open_intent_token, parse_join_request_qr, seal_intent_token,
};
