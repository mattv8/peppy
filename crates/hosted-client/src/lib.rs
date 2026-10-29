//! Pure hosted provisioning and enrollment primitives.

use base64::{Engine as _, engine::general_purpose::STANDARD};
use peppy_crypto::enrollment::derive_admission_keypair;
use peppy_crypto::{
    KeyProfile, VaultCheckHeader, create_vault_check_header, derive_root_key,
    verify_vault_check_header,
};
use peppy_hosted_api::enrollment::ClientKind;
use peppy_hosted_api::{
    AccountResponse, CompleteProvisioningRequest, CompleteProvisioningResponse, IdentityProvider,
    LoginAttemptRequest, LoginAttemptResponse, ProvisioningGrantResponse, ProvisioningRequest,
    PushConfig, SESSION_LIFETIME_SECONDS, SessionExchangeRequest, SessionRenewalResponse,
    SessionResponse,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{fmt, sync::Mutex};
use url::Url;
use uuid::Uuid;
use zeroize::{Zeroize, Zeroizing};

pub mod device_credentials;
pub mod newenrollment;

const MAX_JSON: usize = 16 * 1024;
const MAX_CHECKPOINT: usize = 64 * 1024;
const HOSTED_ORIGIN: &str = "https://peppy.pro";
const HOSTED_WAKE_RELAY_URL: &str = "https://relay.peppy.pro";

#[derive(Debug, thiserror::Error)]
pub enum HostedClientError {
    #[error("invalid hosted-client request")]
    InvalidRequest,
    #[error("invalid profile")]
    InvalidProfile,
    #[error("verified handoff keys are unavailable")]
    KeysUnavailable,
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
    client_kind: ClientKind,
    device_name: String,
    signing_seed: Vec<u8>,
    admission_public_key: String,
}

impl Checkpoint {
    /// Clears every secret the checkpoint carries; Drop shares this path.
    fn zeroize_secrets(&mut self) {
        self.token.zeroize();
        if let Some(ref mut grant) = self.grant {
            grant.zeroize();
        }
        self.signing_seed.zeroize();
    }
}

impl Drop for Checkpoint {
    fn drop(&mut self) {
        self.zeroize_secrets();
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
    client_kind: ClientKind,
    device_name: String,
    signing_seed: Zeroizing<[u8; 32]>,
    admission_public_key: String,
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
        || !(1..=SESSION_LIFETIME_SECONDS).contains(&value.expires_in_seconds)
    {
        return invalid();
    }
    Ok(HostedSession {
        account_id: value.account_id.to_string(),
        bearer_token: Zeroizing::new(value.session_token),
        expires_in_seconds: value.expires_in_seconds,
    })
}
pub fn parse_hosted_session_renewal(
    json: String,
    expected_account_id: String,
) -> Result<u32, HostedClientError> {
    bounded(&json, MAX_JSON)?;
    let expected = uuid(&expected_account_id)?;
    let value: SessionRenewalResponse =
        serde_json::from_str(&json).map_err(|_| HostedClientError::InvalidRequest)?;
    if value.account_id != expected
        || !(1..=SESSION_LIFETIME_SECONDS).contains(&value.expires_in_seconds)
    {
        return invalid();
    }
    Ok(value.expires_in_seconds)
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

/// Returns the fixed hosted wake relay only for the canonical hosted origin.
pub fn hosted_wake_relay_url(origin: String) -> Option<String> {
    (origin == HOSTED_ORIGIN).then(|| HOSTED_WAKE_RELAY_URL.into())
}

/// Parses the versioned hosted push capability response without widening the
/// canonical wire contract to untrusted provider or environment values.
pub fn parse_push_config(json: String) -> Result<PushConfig, HostedClientError> {
    bounded(&json, MAX_JSON)?;
    let config: PushConfig =
        serde_json::from_str(&json).map_err(|_| HostedClientError::InvalidRequest)?;
    if !config.validate() {
        return invalid();
    }
    Ok(config)
}
pub fn hosted_passphrase_acceptable(mut passphrase: String) -> bool {
    let acceptable = peppy_crypto::passphrase::passphrase_acceptable(&passphrase);
    passphrase.zeroize();
    acceptable
}

pub fn vault_profile_fingerprint(profile_json: String) -> Result<String, HostedClientError> {
    bounded(&profile_json, MAX_JSON)?;
    let profile: KeyProfile =
        serde_json::from_str(&profile_json).map_err(|_| HostedClientError::InvalidProfile)?;
    profile
        .fingerprint()
        .map_err(|_| HostedClientError::InvalidProfile)
}

pub fn prepare(
    origin: impl AsRef<str>,
    account_id: impl AsRef<str>,
    passphrase: impl AsRef<str>,
    client_kind: ClientKind,
    device_name: impl AsRef<str>,
) -> Result<HostedProvisioning, HostedClientError> {
    let origin = canonical_origin(origin.as_ref())?;
    let account = uuid(account_id.as_ref())?;
    let passphrase = Zeroizing::new(passphrase.as_ref().to_owned());
    if !peppy_crypto::passphrase::passphrase_acceptable(&passphrase) {
        return invalid();
    }
    let device_name = device_name.as_ref().to_owned();
    if device_name.is_empty()
        || device_name.len() > 128
        || device_name.chars().any(char::is_control)
    {
        return invalid();
    }
    let vault_id = Uuid::new_v4();
    let device_id = Uuid::new_v4();
    let operation_id = Uuid::new_v4();
    let profile = KeyProfile::new(vault_id, 1).map_err(|_| HostedClientError::Crypto)?;
    let root = derive_root_key(&passphrase, &profile).map_err(|_| HostedClientError::Crypto)?;
    let header =
        create_vault_check_header(&root, profile.clone()).map_err(|_| HostedClientError::Crypto)?;
    let admission_public_key = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(
        derive_admission_keypair(&root)
            .map_err(|_| HostedClientError::Crypto)?
            .public_key(),
    );
    let mut bytes = [0u8; 48];
    let mut signing_seed = [0u8; 32];
    libsodium_rs::ensure_init().map_err(|_| HostedClientError::Crypto)?;
    libsodium_rs::random::fill_bytes(&mut bytes);
    libsodium_rs::random::fill_bytes(&mut signing_seed);
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
        client_kind,
        device_name,
        signing_seed: Zeroizing::new(signing_seed),
        admission_public_key,
        grant: Mutex::new(None),
    })
}

fn checkpoint(value: &HostedProvisioning) -> Result<Vec<u8>, HostedClientError> {
    let grant = value.grant.lock().map_err(|_| HostedClientError::Crypto)?;
    serde_json::to_vec(&Checkpoint {
        version: 2,
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
        client_kind: value.client_kind,
        device_name: value.device_name.clone(),
        signing_seed: value.signing_seed.to_vec(),
        admission_public_key: value.admission_public_key.clone(),
    })
    .map_err(|_| HostedClientError::Crypto)
}
pub fn restore(
    checkpoint_bytes: Vec<u8>,
    expected_origin: impl AsRef<str>,
    expected_account_id: impl AsRef<str>,
) -> Result<HostedProvisioning, HostedClientError> {
    let checkpoint_bytes = Zeroizing::new(checkpoint_bytes);
    restore_validated(
        &checkpoint_bytes,
        expected_origin.as_ref(),
        expected_account_id.as_ref(),
    )
}

/// Restores protected local checkpoint material only for an exact canonical
/// origin, vault, and device identity tuple.
///
/// This is structural checkpoint validation, not cryptographic authentication.
/// Callers must read the checkpoint from protected local storage and verify any
/// recovered secret against the current server-verified vault header.
pub fn restore_for_identity(
    checkpoint_bytes: Vec<u8>,
    expected_origin: impl AsRef<str>,
    expected_vault_id: impl AsRef<str>,
    expected_device_id: impl AsRef<str>,
) -> Result<HostedProvisioning, HostedClientError> {
    let checkpoint_bytes = Zeroizing::new(checkpoint_bytes);
    let origin = canonical_origin(expected_origin.as_ref())?;
    if expected_origin.as_ref() != origin {
        return invalid();
    }
    let vault_id = canonical_non_nil_uuid(expected_vault_id.as_ref())?;
    let device_id = canonical_non_nil_uuid(expected_device_id.as_ref())?;
    let account_id = checkpoint_account_id(&checkpoint_bytes)?;
    let provisioning = restore_validated(&checkpoint_bytes, &origin, &account_id)?;
    if provisioning.vault_id != vault_id || provisioning.device_id != device_id {
        return invalid();
    }
    Ok(provisioning)
}

fn checkpoint_account_id(checkpoint_bytes: &[u8]) -> Result<String, HostedClientError> {
    if checkpoint_bytes.is_empty() || checkpoint_bytes.len() > MAX_CHECKPOINT {
        return invalid();
    }
    let value: Checkpoint =
        serde_json::from_slice(checkpoint_bytes).map_err(|_| HostedClientError::InvalidRequest)?;
    Ok(value.account_id.clone())
}

fn canonical_non_nil_uuid(value: &str) -> Result<Uuid, HostedClientError> {
    let id = uuid(value)?;
    if id.is_nil() || value != id.to_string() {
        return invalid();
    }
    Ok(id)
}

fn restore_validated(
    checkpoint_bytes: &[u8],
    expected_origin: &str,
    expected_account_id: &str,
) -> Result<HostedProvisioning, HostedClientError> {
    if checkpoint_bytes.is_empty() || checkpoint_bytes.len() > MAX_CHECKPOINT {
        return invalid();
    }
    let origin = canonical_origin(expected_origin)?;
    let account = uuid(expected_account_id)?;
    let mut value: Checkpoint =
        serde_json::from_slice(checkpoint_bytes).map_err(|_| HostedClientError::InvalidRequest)?;
    if value.version != 2
        || value.origin != origin
        || value.account_id != account.to_string()
        || !hex(&value.token, 96, None)
        || value.profile.key_epoch != 1
        || value.profile.vault_id.to_string() != value.vault_id
        || value.device_name.is_empty()
        || value.device_name.len() > 128
        || value.signing_seed.len() != 32
        || base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(&value.admission_public_key)
            .ok()
            .is_none_or(|bytes| bytes.len() != 32)
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
    let signing_seed: [u8; 32] = value
        .signing_seed
        .as_slice()
        .try_into()
        .map_err(|_| HostedClientError::InvalidRequest)?;
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
        client_kind: value.client_kind,
        device_name: value.device_name.clone(),
        signing_seed: Zeroizing::new(signing_seed),
        admission_public_key: value.admission_public_key.clone(),
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
    pub fn profile(&self) -> KeyProfile {
        self.profile.clone()
    }
    pub fn header(&self) -> VaultCheckHeader {
        self.header.clone()
    }
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
            admission_public_key: self.admission_public_key.clone(),
            signing_public_key: self.signing_public_key()?,
            client_kind: self.client_kind,
            device_name: self.device_name.clone(),
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
        let credential = device_credentials::PortableDeviceCredential::new(
            self.origin.clone(),
            self.vault_id.to_string(),
            self.device_id.to_string(),
            self.token.to_string(),
        )
        .map_err(|_| HostedClientError::InvalidRequest)?;
        let bytes = device_credentials::serialize_portable_credential(&credential)
            .map_err(|_| HostedClientError::Crypto)?;
        String::from_utf8(bytes.to_vec()).map_err(|_| HostedClientError::Crypto)
    }
    pub fn passphrase_matches(&self, passphrase: &str) -> Result<bool, HostedClientError> {
        let passphrase = Zeroizing::new(passphrase.to_owned());
        let root = derive_root_key(&passphrase, &self.profile);
        match root {
            Ok(root) => Ok(verify_vault_check_header(&root, &self.profile, &self.header).is_ok()),
            Err(_) => Ok(false),
        }
    }
    fn signing_public_key(&self) -> Result<String, HostedClientError> {
        let key = libsodium_rs::crypto_sign::KeyPair::from_seed(&self.signing_seed[..])
            .map_err(|_| HostedClientError::Crypto)?;
        Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(key.public_key.as_bytes()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const PASSPHRASE: &str = "alpha bravo charlie delta echo foxtrot";
    const ORIGIN: &str = "https://peppy.example";

    #[test]
    fn checkpoint_secret_wipe_clears_signing_seed_with_token_and_grant() {
        let prepared = prepare(
            ORIGIN,
            Uuid::new_v4().to_string(),
            PASSPHRASE,
            ClientKind::Desktop,
            "test device",
        )
        .expect("valid local preparation");
        let bytes = checkpoint(&prepared).expect("checkpoint serializes");
        let mut restored: Checkpoint = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(restored.signing_seed.len(), 32);
        assert!(!restored.token.is_empty());
        restored.zeroize_secrets();
        assert!(restored.token.is_empty());
        assert!(restored.signing_seed.is_empty());
    }

    #[test]
    fn provisioning_material_unlocks_serializes_and_restores_without_plaintext() {
        let account = Uuid::new_v4().to_string();
        let prepared = prepare(
            ORIGIN,
            account.clone(),
            PASSPHRASE,
            ClientKind::Desktop,
            "test device",
        )
        .expect("valid local preparation");
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
        let complete_value: serde_json::Value = serde_json::from_str(&complete).unwrap();
        assert_eq!(complete_value["client_kind"], "desktop");
        assert_eq!(complete_value["device_name"], "test device");
        assert_eq!(
            complete_value["admission_public_key"]
                .as_str()
                .unwrap()
                .len(),
            43
        );
        assert_eq!(
            complete_value["signing_public_key"].as_str().unwrap().len(),
            43
        );
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
    fn session_renewal_requires_the_expected_account_and_canonical_lifetime() {
        let account = Uuid::new_v4();
        let renewal = serde_json::json!({
            "account_id": account,
            "expires_in_seconds": 2_592_000,
        });
        assert_eq!(
            parse_hosted_session_renewal(renewal.to_string(), account.to_string()).unwrap(),
            2_592_000
        );
        assert!(
            parse_hosted_session_renewal(renewal.to_string(), Uuid::new_v4().to_string()).is_err()
        );
        for expires_in_seconds in [0, 2_592_001] {
            assert!(
                parse_hosted_session_renewal(
                    serde_json::json!({
                        "account_id": account,
                        "expires_in_seconds": expires_in_seconds,
                    })
                    .to_string(),
                    account.to_string()
                )
                .is_err()
            );
        }
        assert!(
            parse_hosted_session_renewal(
                serde_json::json!({
                    "account_id": account,
                    "expires_in_seconds": 2_592_000,
                    "session_token": "must_not_be_returned",
                })
                .to_string(),
                account.to_string()
            )
            .is_err()
        );
    }

    #[test]
    fn session_parser_accepts_the_canonical_thirty_day_lifetime() {
        let account = Uuid::new_v4();
        assert_eq!(
            parse_hosted_session(
                serde_json::json!({
                    "account_id": account,
                    "session_token": format!("pst_{}", "a".repeat(64)),
                    "expires_in_seconds": 2_592_000,
                })
                .to_string()
            )
            .unwrap()
            .expires_in_seconds(),
            2_592_000
        );
    }

    #[test]
    fn initial_session_parser_rejects_lifetimes_outside_the_canonical_range() {
        let account = Uuid::new_v4();
        for expires_in_seconds in [0, SESSION_LIFETIME_SECONDS + 1] {
            assert!(
                parse_hosted_session(
                    serde_json::json!({
                        "account_id": account,
                        "session_token": format!("pst_{}", "a".repeat(64)),
                        "expires_in_seconds": expires_in_seconds,
                    })
                    .to_string()
                )
                .is_err()
            );
        }
    }

    #[test]
    fn hosted_wake_relay_requires_the_exact_hosted_origin() {
        assert_eq!(
            hosted_wake_relay_url("https://peppy.pro".into()),
            Some("https://relay.peppy.pro".into())
        );
        for origin in [
            "https://peppy.pro/",
            "https://peppy.pro.evil.example",
            "https://relay.peppy.pro",
            "http://peppy.pro",
        ] {
            assert_eq!(hosted_wake_relay_url(origin.into()), None, "{origin}");
        }
    }

    #[test]
    fn push_config_fails_closed_on_incompatible_values() {
        let valid = r#"{"version":1,"available":true,"providers":["fcm","apns"],"apns_environment":"production"}"#;
        assert!(parse_push_config(valid.into()).is_ok());
        for invalid in [
            r#"{"version":2,"available":true,"providers":["fcm"],"apns_environment":null}"#,
            r#"{"version":1,"available":true,"providers":["apns"],"apns_environment":null}"#,
            r#"{"version":1,"available":false,"providers":["fcm"],"apns_environment":null}"#,
            r#"{"version":1,"available":true,"providers":["fcm","fcm"],"apns_environment":null}"#,
            r#"{"version":1,"available":true,"providers":["webpush"],"apns_environment":null}"#,
        ] {
            assert!(parse_push_config(invalid.into()).is_err(), "{invalid}");
        }
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
    fn rejects_complete_response_with_wrong_operation_vault_device() {
        let account = Uuid::new_v4().to_string();
        let prepared = prepare(
            ORIGIN,
            account.clone(),
            PASSPHRASE,
            ClientKind::Desktop,
            "test device",
        )
        .expect("valid local preparation");
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
        let prepared = prepare(
            ORIGIN,
            account.clone(),
            PASSPHRASE,
            ClientKind::Desktop,
            "test device",
        )
        .expect("valid local preparation");
        let checkpoint = prepared.checkpoint().unwrap();

        // Wrong origin
        assert!(restore(checkpoint.clone(), "https://wrong.example", account.clone()).is_err());

        // Wrong account
        assert!(restore(checkpoint, ORIGIN, Uuid::new_v4().to_string()).is_err());
    }

    #[test]
    fn restore_for_identity_requires_the_exact_canonical_identity_tuple() {
        let account = Uuid::new_v4().to_string();
        let prepared = prepare(
            ORIGIN,
            account.clone(),
            PASSPHRASE,
            ClientKind::Desktop,
            "test device",
        )
        .expect("valid local preparation");
        let view = prepared.view();
        let checkpoint = prepared.checkpoint().unwrap();

        let restored =
            restore_for_identity(checkpoint.clone(), ORIGIN, &view.vault_id, &view.device_id)
                .expect("checkpoint restores without an account session");
        assert_eq!(restored.view(), view);

        for (origin, vault_id, device_id) in [
            (
                "https://wrong.example".to_owned(),
                view.vault_id.clone(),
                view.device_id.clone(),
            ),
            (
                ORIGIN.to_owned(),
                Uuid::new_v4().to_string(),
                view.device_id.clone(),
            ),
            (
                ORIGIN.to_owned(),
                view.vault_id.clone(),
                Uuid::new_v4().to_string(),
            ),
        ] {
            assert!(restore_for_identity(checkpoint.clone(), origin, vault_id, device_id).is_err());
        }

        for (origin, vault_id, device_id) in [
            (
                format!("{ORIGIN}/"),
                view.vault_id.clone(),
                view.device_id.clone(),
            ),
            (
                ORIGIN.to_owned(),
                view.vault_id.to_uppercase(),
                view.device_id.clone(),
            ),
            (
                ORIGIN.to_owned(),
                view.vault_id.clone(),
                view.device_id.to_uppercase(),
            ),
            (
                ORIGIN.to_owned(),
                view.vault_id.clone(),
                Uuid::nil().to_string(),
            ),
            (
                ORIGIN.to_owned(),
                Uuid::nil().to_string(),
                view.device_id.clone(),
            ),
        ] {
            assert!(restore_for_identity(checkpoint.clone(), origin, vault_id, device_id).is_err());
        }

        assert!(restore_for_identity(vec![b'{'], ORIGIN, &view.vault_id, &view.device_id).is_err());
        assert!(
            restore_for_identity(
                vec![b'{'; MAX_CHECKPOINT + 1],
                ORIGIN,
                &view.vault_id,
                &view.device_id,
            )
            .is_err()
        );

        let mut inconsistent: Checkpoint = serde_json::from_slice(&checkpoint).unwrap();
        inconsistent.profile.key_epoch = 2;
        assert!(
            restore_for_identity(
                serde_json::to_vec(&inconsistent).unwrap(),
                ORIGIN,
                &view.vault_id,
                &view.device_id,
            )
            .is_err()
        );
    }

    #[test]
    fn rejects_noncanonical_device_operation_vault_uuids() {
        let account = Uuid::new_v4().to_string();
        let prepared = prepare(
            ORIGIN,
            account.clone(),
            PASSPHRASE,
            ClientKind::Desktop,
            "test device",
        )
        .expect("valid local preparation");
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
        let prepared = prepare(
            ORIGIN,
            account.clone(),
            PASSPHRASE,
            ClientKind::Desktop,
            "test device",
        )
        .expect("valid local preparation");
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
}
