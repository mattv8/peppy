//! Rust-owned hosted enrollment contracts. Native wrappers around peppy_hosted_client.
//! Hosts own HTTP and encrypted secure storage.

use super::MobileBindingsError;
use peppy_hosted_client as hc;
use std::{fmt, sync::Mutex};
use zeroize::Zeroizing;

/// Convert HostedClientError to MobileBindingsError.
fn convert_error(e: hc::HostedClientError) -> MobileBindingsError {
    match e {
        hc::HostedClientError::InvalidRequest => MobileBindingsError::InvalidRequest,
        hc::HostedClientError::InvalidProfile => MobileBindingsError::InvalidProfile,
        hc::HostedClientError::Crypto => MobileBindingsError::Crypto,
    }
}

// Re-export native-facing types with minimal wrappers.

#[derive(uniffi::Object)]
pub struct NativeHostedLoginAttempt {
    inner: hc::HostedLoginAttempt,
}

#[derive(uniffi::Object)]
pub struct NativeHostedSession {
    account_id: String,
    bearer_token: Zeroizing<String>,
    expires_in_seconds: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct NativeHostedAccount {
    pub account_id: String,
    pub classification: String,
    pub entitlement: String,
    pub access: String,
    pub vault_id: Option<String>,
    pub operation_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct NativeHostedProvisioningView {
    pub origin: String,
    pub account_id: String,
    pub operation_id: String,
    pub vault_id: String,
    pub device_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct NativeJoinRequestQr {
    pub https_origin: String,
    pub join_request_id: String,
    pub join_key: String,
}

#[derive(uniffi::Object)]
pub struct NativeHostedProvisioning {
    inner: Mutex<hc::HostedProvisioning>,
}

impl fmt::Debug for NativeHostedProvisioning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NativeHostedProvisioning").finish()
    }
}

// Join request functions (these already delegate to hc)

#[uniffi::export]
pub fn parse_join_request_qr(
    payload: String,
    allow_loopback_http: bool,
) -> Result<NativeJoinRequestQr, MobileBindingsError> {
    let qr = hc::parse_join_request_qr(&payload, allow_loopback_http).map_err(convert_error)?;
    Ok(NativeJoinRequestQr {
        https_origin: qr.https_origin,
        join_request_id: qr.join_request_id.to_string(),
        join_key: qr.join_key,
    })
}

#[uniffi::export]
pub fn seal_intent_token(
    join_key_b64url: String,
    intent_token: String,
) -> Result<String, MobileBindingsError> {
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};

    let join_key: [u8; 32] = URL_SAFE_NO_PAD
        .decode(join_key_b64url)
        .map_err(|_| MobileBindingsError::InvalidRequest)?
        .try_into()
        .map_err(|_| MobileBindingsError::InvalidRequest)?;
    let sealed = hc::seal_intent_token(&join_key, &intent_token).map_err(convert_error)?;
    Ok(URL_SAFE_NO_PAD.encode(sealed))
}

#[uniffi::export]
pub fn intent_digest_hex(intent_token: String) -> String {
    hc::intent_digest_hex(&intent_token)
}

// Login and session request/parse functions

#[uniffi::export]
pub fn hosted_login_request(provider: String) -> Result<String, MobileBindingsError> {
    hc::hosted_login_request(provider).map_err(convert_error)
}

#[uniffi::export]
pub fn hosted_session_request(
    attempt_id: String,
    id_token: String,
) -> Result<String, MobileBindingsError> {
    hc::hosted_session_request(attempt_id, id_token).map_err(convert_error)
}

#[uniffi::export]
pub fn parse_hosted_login_attempt(
    json: String,
) -> Result<std::sync::Arc<NativeHostedLoginAttempt>, MobileBindingsError> {
    let attempt = hc::parse_hosted_login_attempt(json).map_err(convert_error)?;
    Ok(std::sync::Arc::new(NativeHostedLoginAttempt {
        inner: attempt,
    }))
}

#[uniffi::export]
pub fn parse_hosted_session(
    json: String,
) -> Result<std::sync::Arc<NativeHostedSession>, MobileBindingsError> {
    let session = hc::parse_hosted_session(json).map_err(convert_error)?;
    Ok(std::sync::Arc::new(NativeHostedSession {
        account_id: session.account_id(),
        bearer_token: Zeroizing::new(session.bearer_token()),
        expires_in_seconds: session.expires_in_seconds(),
    }))
}

#[uniffi::export]
pub fn parse_hosted_account(
    json: String,
    expected_account_id: String,
) -> Result<NativeHostedAccount, MobileBindingsError> {
    let account = hc::parse_hosted_account(json, expected_account_id).map_err(convert_error)?;
    Ok(NativeHostedAccount {
        account_id: account.account_id,
        classification: account.classification,
        entitlement: account.entitlement,
        access: account.access,
        vault_id: account.vault_id,
        operation_id: account.operation_id,
    })
}

#[uniffi::export]
pub fn generate_hosted_passphrase() -> String {
    hc::generate_hosted_passphrase()
}

#[uniffi::export]
pub fn hosted_passphrase_acceptable(passphrase: String) -> bool {
    hc::hosted_passphrase_acceptable(passphrase)
}

#[uniffi::export]
pub fn vault_profile_fingerprint(profile_json: String) -> Result<String, MobileBindingsError> {
    hc::vault_profile_fingerprint(profile_json).map_err(convert_error)
}

#[uniffi::export]
pub fn pairing_intent_sas(
    intent_token: String,
    key_digest: String,
    device_id: String,
) -> Result<String, MobileBindingsError> {
    hc::pairing_intent_sas(intent_token, key_digest, device_id).map_err(convert_error)
}

// Provisioning functions

#[uniffi::export]
pub fn prepare_hosted_provisioning(
    origin: String,
    account_id: String,
    passphrase: String,
) -> Result<std::sync::Arc<NativeHostedProvisioning>, MobileBindingsError> {
    let provisioning = hc::prepare(&origin, &account_id, &passphrase).map_err(convert_error)?;
    Ok(std::sync::Arc::new(NativeHostedProvisioning {
        inner: Mutex::new(provisioning),
    }))
}

#[uniffi::export]
pub fn restore_hosted_provisioning(
    checkpoint_bytes: Vec<u8>,
    expected_origin: String,
    expected_account_id: String,
) -> Result<std::sync::Arc<NativeHostedProvisioning>, MobileBindingsError> {
    let provisioning = hc::restore(checkpoint_bytes, &expected_origin, &expected_account_id)
        .map_err(convert_error)?;
    Ok(std::sync::Arc::new(NativeHostedProvisioning {
        inner: Mutex::new(provisioning),
    }))
}

// Method implementations

#[uniffi::export]
impl NativeHostedLoginAttempt {
    pub fn attempt_id(&self) -> String {
        self.inner.attempt_id()
    }
    pub fn nonce(&self) -> String {
        self.inner.nonce()
    }
    pub fn expires_in_seconds(&self) -> u32 {
        self.inner.expires_in_seconds()
    }
}

#[uniffi::export]
impl NativeHostedSession {
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

#[uniffi::export]
impl NativeHostedProvisioning {
    pub fn view(&self) -> NativeHostedProvisioningView {
        let inner = self.inner.lock().expect("mutex poisoned");
        let v = inner.view();
        NativeHostedProvisioningView {
            origin: v.origin,
            account_id: v.account_id,
            operation_id: v.operation_id,
            vault_id: v.vault_id,
            device_id: v.device_id,
        }
    }

    pub fn checkpoint(&self) -> Result<Vec<u8>, MobileBindingsError> {
        let inner = self.inner.lock().map_err(|_| MobileBindingsError::Crypto)?;
        inner.checkpoint().map_err(convert_error)
    }

    pub fn grant_request(&self) -> Result<String, MobileBindingsError> {
        let inner = self.inner.lock().map_err(|_| MobileBindingsError::Crypto)?;
        inner.grant_request().map_err(convert_error)
    }

    pub fn accept_grant(&self, response_json: String) -> Result<(), MobileBindingsError> {
        let inner = self.inner.lock().map_err(|_| MobileBindingsError::Crypto)?;
        inner.accept_grant(response_json).map_err(convert_error)
    }

    pub fn has_grant(&self) -> bool {
        self.inner.lock().is_ok_and(|inner| inner.has_grant())
    }

    pub fn complete_request(&self) -> Result<String, MobileBindingsError> {
        let inner = self.inner.lock().map_err(|_| MobileBindingsError::Crypto)?;
        inner.complete_request().map_err(convert_error)
    }

    pub fn credential_json(
        &self,
        complete_response_json: String,
    ) -> Result<String, MobileBindingsError> {
        let inner = self.inner.lock().map_err(|_| MobileBindingsError::Crypto)?;
        inner
            .credential_json(complete_response_json)
            .map_err(convert_error)
    }

    pub fn passphrase_matches(&self, passphrase: String) -> Result<bool, MobileBindingsError> {
        let inner = self.inner.lock().map_err(|_| MobileBindingsError::Crypto)?;
        inner.passphrase_matches(&passphrase).map_err(convert_error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PASSPHRASE: &str = "alpha bravo charlie delta echo foxtrot";
    const ORIGIN: &str = "https://peppy.example";

    #[test]
    fn provisioning_material_unlocks_serializes_and_restores_without_plaintext() {
        let account = uuid::Uuid::new_v4().to_string();
        let prepared =
            prepare_hosted_provisioning(ORIGIN.into(), account.clone(), PASSPHRASE.into())
                .expect("valid local preparation");
        assert!(prepared.passphrase_matches(PASSPHRASE.into()).unwrap());
        assert!(
            !prepared
                .passphrase_matches("wrong phrase with sufficient length here".into())
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
        let restored = restore_hosted_provisioning(checkpoint, ORIGIN.into(), account).unwrap();
        assert_eq!(complete, restored.complete_request().unwrap());
        assert!(
            restore_hosted_provisioning(
                vec![b'{'; 64 * 1024 + 1],
                ORIGIN.into(),
                uuid::Uuid::new_v4().to_string()
            )
            .is_err()
        );
    }

    #[test]
    fn session_and_account_contracts_fail_closed() {
        let account = uuid::Uuid::new_v4();
        assert!(parse_hosted_session(
            serde_json::json!({"account_id": account, "session_token": "missing", "expires_in_seconds": 60}).to_string()
        ).is_err());
        assert!(parse_hosted_account(
            serde_json::json!({"account_id": account, "classification": "new", "entitlement": "none", "access": "read_write"}).to_string(),
            uuid::Uuid::new_v4().to_string()
        ).is_err());
    }

    #[test]
    fn vault_profile_fingerprint_validates_and_matches_crypto_vector() {
        let profile = peppy_client_core::KeyProfile {
            crypto_suite: peppy_crypto::CRYPTO_SUITE_1,
            salt: [7; 16],
            vault_id: uuid::Uuid::nil(),
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
            peppy_protocol::pairing_sas(
                token,
                digest,
                peppy_client_core::DeviceId(uuid::Uuid::parse_str(device).unwrap())
            )
        );
        assert!(pairing_intent_sas("not-a-token".into(), digest.into(), device.into()).is_err());
        assert!(pairing_intent_sas(token.into(), "A".repeat(64), device.into()).is_err());
        assert!(pairing_intent_sas(token.into(), digest.into(), format!("{{{device}}}")).is_err());
    }

    #[test]
    fn rejects_complete_response_with_wrong_operation_vault_device() {
        let account = uuid::Uuid::new_v4().to_string();
        let prepared =
            prepare_hosted_provisioning(ORIGIN.into(), account.clone(), PASSPHRASE.into())
                .expect("valid local preparation");
        let grant = format!("pgr_{}", "a".repeat(64));
        prepared
            .accept_grant(serde_json::json!({"grant": grant, "expires_in_seconds": 60}).to_string())
            .unwrap();
        let complete = prepared.complete_request().unwrap();

        let view = prepared.view();

        // Test wrong operation_id
        let wrong_op = serde_json::json!({
            "operation_id": uuid::Uuid::new_v4().to_string(),
            "vault_id": view.vault_id,
            "device_id": view.device_id,
        });
        assert!(prepared.credential_json(wrong_op.to_string()).is_err());

        // Test wrong vault_id
        let wrong_vault = serde_json::json!({
            "operation_id": view.operation_id,
            "vault_id": uuid::Uuid::new_v4().to_string(),
            "device_id": view.device_id,
        });
        assert!(prepared.credential_json(wrong_vault.to_string()).is_err());

        // Test wrong device_id
        let wrong_device = serde_json::json!({
            "operation_id": view.operation_id,
            "vault_id": view.vault_id,
            "device_id": uuid::Uuid::new_v4().to_string(),
        });
        assert!(prepared.credential_json(wrong_device.to_string()).is_err());

        // Verify material still intact after failures
        assert_eq!(complete, prepared.complete_request().unwrap());
    }

    #[test]
    fn rejects_checkpoint_origin_and_account_mismatch() {
        let account = uuid::Uuid::new_v4().to_string();
        let prepared =
            prepare_hosted_provisioning(ORIGIN.into(), account.clone(), PASSPHRASE.into())
                .expect("valid local preparation");
        let checkpoint = prepared.checkpoint().unwrap();

        // Wrong origin
        assert!(
            restore_hosted_provisioning(
                checkpoint.clone(),
                "https://wrong.example".into(),
                account.clone()
            )
            .is_err()
        );

        // Wrong account
        assert!(
            restore_hosted_provisioning(
                checkpoint,
                ORIGIN.into(),
                uuid::Uuid::new_v4().to_string()
            )
            .is_err()
        );
    }

    #[test]
    fn rejects_noncanonical_device_operation_vault_uuids() {
        let account = uuid::Uuid::new_v4().to_string();
        let prepared =
            prepare_hosted_provisioning(ORIGIN.into(), account.clone(), PASSPHRASE.into())
                .expect("valid local preparation");
        let checkpoint = prepared.checkpoint().unwrap();

        #[derive(serde::Deserialize, serde::Serialize)]
        #[serde(deny_unknown_fields)]
        struct Checkpoint {
            version: u8,
            origin: String,
            account_id: String,
            operation_id: String,
            vault_id: String,
            device_id: String,
            token: String,
            profile: peppy_client_core::KeyProfile,
            header: peppy_crypto::VaultCheckHeader,
            profile_fingerprint: String,
            grant: Option<String>,
        }

        let mut value: Checkpoint = serde_json::from_slice(&checkpoint).unwrap();
        let original_op = value.operation_id.clone();

        // Non-canonical operation UUID (uppercase)
        value.operation_id = original_op.to_uppercase();
        let tampered_json = serde_json::to_vec(&value).unwrap();
        assert!(
            restore_hosted_provisioning(tampered_json, ORIGIN.into(), account.clone()).is_err(),
            "non-canonical operation UUID should be rejected"
        );

        // Restore original for next test
        value.operation_id = original_op.clone();

        // Nil UUID in vault field
        value.vault_id = "00000000-0000-0000-0000-000000000000".to_string();
        let nil_json = serde_json::to_vec(&value).unwrap();
        assert!(
            restore_hosted_provisioning(nil_json, ORIGIN.into(), account.clone()).is_err(),
            "nil vault UUID should be rejected"
        );
    }

    #[test]
    fn rejects_tampered_profile_and_header() {
        let account = uuid::Uuid::new_v4().to_string();
        let prepared =
            prepare_hosted_provisioning(ORIGIN.into(), account.clone(), PASSPHRASE.into())
                .expect("valid local preparation");
        let checkpoint = prepared.checkpoint().unwrap();

        #[derive(serde::Deserialize, serde::Serialize)]
        #[serde(deny_unknown_fields)]
        struct Checkpoint {
            version: u8,
            origin: String,
            account_id: String,
            operation_id: String,
            vault_id: String,
            device_id: String,
            token: String,
            profile: peppy_client_core::KeyProfile,
            header: peppy_crypto::VaultCheckHeader,
            profile_fingerprint: String,
            grant: Option<String>,
        }

        let mut value: Checkpoint = serde_json::from_slice(&checkpoint).unwrap();
        let original_profile = value.profile.clone();

        // Tamper with profile key_epoch
        value.profile.key_epoch = 2;
        let tampered_json = serde_json::to_vec(&value).unwrap();
        assert!(
            restore_hosted_provisioning(tampered_json, ORIGIN.into(), account.clone()).is_err(),
            "tampered key_epoch should be rejected"
        );

        // Restore original and test header mismatch
        value.profile = original_profile.clone();
        value.header.profile =
            peppy_client_core::KeyProfile::new(uuid::Uuid::new_v4(), 1).expect("valid profile");
        let header_mismatch = serde_json::to_vec(&value).unwrap();
        assert!(
            restore_hosted_provisioning(header_mismatch, ORIGIN.into(), account.clone()).is_err(),
            "mismatched header profile should be rejected"
        );
    }
}
