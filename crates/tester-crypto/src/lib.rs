//! Small browser-worker crypto boundary for vault-bound tester envelopes.

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use peppy_crypto::{
    CryptoError, EncryptedEnvelope, KeyPurpose, PurposeKey, decrypt, derive_purpose_key,
    derive_root_key, encrypt, profile_fingerprint_bytes, verify_vault_check_header,
};
use peppy_hosted_api::tester_admin_v2::{ALGORITHM, CiphertextV2, VERSION, VaultBindingV2};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{Mutex, OnceLock},
};
use uuid::Uuid;
use zeroize::{Zeroize, Zeroizing};

const MAX_REQUEST_BYTES: usize = 64 * 1024;
const MAX_PLAINTEXT_BYTES: usize = 4096;
const MAX_SESSIONS: usize = 8;
const AAD_PREFIX: &[u8] = b"peppy-tester-vault-v2\0";

struct Session {
    binding: VaultBindingV2,
    key: Box<PurposeKey>,
}
impl Drop for Session {
    fn drop(&mut self) {
        self.binding.profile_fingerprint.zeroize();
    }
}
#[derive(Default)]
pub struct TesterCryptoWorker {
    sessions: HashMap<Uuid, Session>,
}

#[derive(Deserialize)]
struct Request<'a> {
    command: String,
    #[serde(default, borrow)]
    args: Option<&'a serde_json::value::RawValue>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct UnlockRequest {
    passphrase: SecretString,
    binding: VaultBindingV2,
    vault_check_header: peppy_hosted_api::tester_admin_v2::VaultCheckHeaderV2,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct EncryptRequest {
    handle: Uuid,
    registry_id: Uuid,
    entry_id: Uuid,
    key_id: Uuid,
    plaintext: SecretString,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DecryptRequest {
    handle: Uuid,
    registry_id: Uuid,
    entry_id: Uuid,
    key_id: Uuid,
    ciphertext: CiphertextV2,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CloseRequest {
    handle: Uuid,
}
#[derive(Serialize)]
struct Failure {
    code: &'static str,
    message: &'static str,
}
struct SecretString(Zeroizing<String>);
impl<'de> Deserialize<'de> for SecretString {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer).map(|value| Self(Zeroizing::new(value)))
    }
}
impl SecretString {
    fn as_str(&self) -> &str {
        &self.0
    }
}

fn failure(code: &'static str, message: &'static str) -> String {
    json!({"ok":false,"error":Failure { code, message }}).to_string()
}
fn crypto_failure(error: CryptoError) -> String {
    match error {
        CryptoError::AuthenticationFailed => failure(
            "authentication-failed",
            "The encrypted tester entry does not match this vault context.",
        ),
        CryptoError::SurroundingWhitespace | CryptoError::InvalidProfile => failure(
            "unlock-failed",
            "The vault phrase did not unlock this vault.",
        ),
        _ => failure(
            "unlock-failed",
            "The vault phrase did not unlock this vault.",
        ),
    }
}

impl TesterCryptoWorker {
    pub fn dispatch(&mut self, input: &str) -> String {
        if input.len() > MAX_REQUEST_BYTES {
            return failure("invalid-request", "The request is invalid.");
        }
        let request: Request<'_> = match serde_json::from_str(input) {
            Ok(value) => value,
            Err(_) => return failure("invalid-request", "The request is invalid."),
        };
        let value = match request.command.as_str() {
            "unlock" => self.unlock(request.args),
            "encrypt" => self.encrypt(request.args),
            "decrypt" => self.decrypt(request.args),
            "close" => self.close(request.args),
            _ => Err(failure("invalid-request", "The request is invalid.")),
        };
        match value {
            Ok(value) => json!({"ok":true,"value":value}).to_string(),
            Err(error) => error,
        }
    }
    fn unlock(&mut self, args: Option<&serde_json::value::RawValue>) -> Result<Value, String> {
        let request: UnlockRequest = serde_json::from_str(
            args.ok_or_else(|| failure("invalid-request", "The request is invalid."))?
                .get(),
        )
        .map_err(|_| failure("invalid-request", "The request is invalid."))?;
        let profile: peppy_crypto::KeyProfile = request.binding.profile.clone().into();
        let header: peppy_crypto::VaultCheckHeader = request.vault_check_header.into();
        if request.binding.vault_id != profile.vault_id
            || request.binding.key_epoch != profile.key_epoch
            || request.binding.profile_fingerprint
                != profile.fingerprint().map_err(crypto_failure)?
            || header.profile != profile
        {
            return Err(failure(
                "unlock-failed",
                "The vault context is inconsistent.",
            ));
        }
        let root =
            derive_root_key(request.passphrase.as_str(), &profile).map_err(crypto_failure)?;
        verify_vault_check_header(&root, &profile, &header).map_err(|_| {
            failure(
                "unlock-failed",
                "The vault phrase did not unlock this vault.",
            )
        })?;
        if self.sessions.len() >= MAX_SESSIONS {
            return Err(failure(
                "locked",
                "Close an existing vault session before unlocking another.",
            ));
        }
        let key = Box::new(
            derive_purpose_key(&root, &profile, KeyPurpose::TesterRegistry)
                .map_err(crypto_failure)?,
        );
        let handle = Uuid::new_v4();
        self.sessions.insert(
            handle,
            Session {
                binding: request.binding,
                key,
            },
        );
        Ok(json!({"handle":handle}))
    }
    fn encrypt(&self, args: Option<&serde_json::value::RawValue>) -> Result<Value, String> {
        let request: EncryptRequest = serde_json::from_str(
            args.ok_or_else(|| failure("invalid-request", "The request is invalid."))?
                .get(),
        )
        .map_err(|_| failure("invalid-request", "The request is invalid."))?;
        let session = self
            .sessions
            .get(&request.handle)
            .ok_or_else(|| failure("locked", "Unlock the vault before encrypting testers."))?;
        if request.plaintext.as_str().len() > MAX_PLAINTEXT_BYTES {
            return Err(failure("invalid-request", "The tester entry is too large."));
        }
        let envelope = encrypt(
            &session.key,
            &aad(
                &session.binding,
                request.registry_id,
                request.entry_id,
                request.key_id,
            )?,
            request.plaintext.as_str().as_bytes(),
        )
        .map_err(crypto_failure)?;
        Ok(serde_json::to_value(ciphertext(request.key_id, envelope))
            .expect("ciphertext serializes"))
    }
    fn decrypt(&self, args: Option<&serde_json::value::RawValue>) -> Result<Value, String> {
        let request: DecryptRequest = serde_json::from_str(
            args.ok_or_else(|| failure("invalid-request", "The request is invalid."))?
                .get(),
        )
        .map_err(|_| failure("invalid-request", "The request is invalid."))?;
        if request.ciphertext.key_id != request.key_id {
            return Err(failure(
                "authentication-failed",
                "The encrypted tester entry does not match this registry.",
            ));
        }
        let session = self
            .sessions
            .get(&request.handle)
            .ok_or_else(|| failure("locked", "Unlock the vault before decrypting testers."))?;
        let plaintext = decrypt(
            &session.key,
            &aad(
                &session.binding,
                request.registry_id,
                request.entry_id,
                request.key_id,
            )?,
            &envelope(&request.ciphertext)?,
        )
        .map_err(crypto_failure)?;
        let plaintext = String::from_utf8(plaintext).map_err(|_| {
            failure(
                "authentication-failed",
                "The encrypted tester entry is invalid.",
            )
        })?;
        Ok(json!({"plaintext":plaintext}))
    }
    fn close(&mut self, args: Option<&serde_json::value::RawValue>) -> Result<Value, String> {
        let request: CloseRequest = serde_json::from_str(
            args.ok_or_else(|| failure("invalid-request", "The request is invalid."))?
                .get(),
        )
        .map_err(|_| failure("invalid-request", "The request is invalid."))?;
        if let Some(mut session) = self.sessions.remove(&request.handle) {
            session.key.zeroize();
        }
        Ok(json!({}))
    }
}
fn aad(
    binding: &VaultBindingV2,
    registry_id: Uuid,
    entry_id: Uuid,
    key_id: Uuid,
) -> Result<Vec<u8>, String> {
    let profile: peppy_crypto::KeyProfile = binding.profile.clone().into();
    let fingerprint = profile_fingerprint_bytes(&profile).map_err(crypto_failure)?;
    let mut result = Vec::with_capacity(AAD_PREFIX.len() + 16 * 4 + 32 + 4);
    result.extend_from_slice(AAD_PREFIX);
    result.extend_from_slice(binding.account_id.as_bytes());
    result.extend_from_slice(registry_id.as_bytes());
    result.extend_from_slice(entry_id.as_bytes());
    result.extend_from_slice(key_id.as_bytes());
    result.extend_from_slice(&fingerprint);
    result.extend_from_slice(&binding.key_epoch.to_be_bytes());
    Ok(result)
}
fn ciphertext(key_id: Uuid, value: EncryptedEnvelope) -> CiphertextV2 {
    CiphertextV2 {
        version: VERSION,
        algorithm: ALGORITHM.into(),
        key_id,
        nonce: URL_SAFE_NO_PAD.encode(value.nonce),
        ciphertext: URL_SAFE_NO_PAD.encode(value.ciphertext),
    }
}
fn envelope(value: &CiphertextV2) -> Result<EncryptedEnvelope, String> {
    if value.version != VERSION || value.algorithm != ALGORITHM {
        return Err(failure(
            "invalid-request",
            "The encrypted tester entry is invalid.",
        ));
    }
    let nonce = URL_SAFE_NO_PAD
        .decode(&value.nonce)
        .map_err(|_| failure("invalid-request", "The encrypted tester entry is invalid."))?;
    let ciphertext = URL_SAFE_NO_PAD
        .decode(&value.ciphertext)
        .map_err(|_| failure("invalid-request", "The encrypted tester entry is invalid."))?;
    let nonce: [u8; 24] = nonce
        .try_into()
        .map_err(|_| failure("invalid-request", "The encrypted tester entry is invalid."))?;
    (ciphertext.len() >= 16 && ciphertext.len() <= MAX_PLAINTEXT_BYTES + 16)
        .then_some(EncryptedEnvelope { nonce, ciphertext })
        .ok_or_else(|| failure("invalid-request", "The encrypted tester entry is invalid."))
}

static WORKER: OnceLock<Mutex<TesterCryptoWorker>> = OnceLock::new();
#[unsafe(no_mangle)]
pub extern "C" fn peppy_tester_crypto_alloc(length: usize) -> *mut u8 {
    if length > MAX_REQUEST_BYTES {
        return std::ptr::null_mut();
    }
    let mut bytes = vec![0; length];
    let pointer = bytes.as_mut_ptr();
    std::mem::forget(bytes);
    pointer
}
#[unsafe(no_mangle)]
/// Dispatches a UTF-8 request and returns an owned NUL-terminated response.
///
/// # Safety
/// `request` must point to a live, readable allocation of exactly `length` bytes
/// for this call, with no mutable alias while it is read. It may originate from
/// `peppy_tester_crypto_alloc`; callers retain ownership and must release such a
/// request with `peppy_tester_crypto_free_request` exactly once. The returned
/// pointer is owned by this function and must be released exactly once with
/// `peppy_tester_crypto_free_response`.
pub unsafe extern "C" fn peppy_tester_crypto_dispatch(
    request: *const u8,
    length: usize,
) -> *mut u8 {
    let output = std::panic::catch_unwind(|| {
        if request.is_null() || length > MAX_REQUEST_BYTES {
            return failure("invalid-request", "The request is invalid.");
        }
        let bytes = unsafe { std::slice::from_raw_parts(request, length) };
        let request = std::str::from_utf8(bytes).unwrap_or("");
        WORKER
            .get_or_init(|| Mutex::new(TesterCryptoWorker::default()))
            .lock()
            .map_or_else(
                |_| failure("locked", "The worker is unavailable."),
                |mut worker| worker.dispatch(request),
            )
    })
    .unwrap_or_else(|_| failure("locked", "The worker is unavailable."));
    std::ffi::CString::new(output)
        .map_or(std::ptr::null_mut(), std::ffi::CString::into_raw)
        .cast()
}
#[unsafe(no_mangle)]
/// Zeroizes and releases a request allocation created by `peppy_tester_crypto_alloc`.
///
/// # Safety
/// `request` must be the live, uniquely owned allocation returned by
/// `peppy_tester_crypto_alloc` for exactly `length` bytes. It must not be used
/// during or after this call and must not have been freed previously.
pub unsafe extern "C" fn peppy_tester_crypto_free_request(request: *mut u8, length: usize) {
    if !request.is_null() && length <= MAX_REQUEST_BYTES {
        let mut bytes = unsafe { Vec::from_raw_parts(request, length, length) };
        bytes.zeroize();
    }
}
#[unsafe(no_mangle)]
/// Zeroizes and releases a response returned by `peppy_tester_crypto_dispatch`.
///
/// # Safety
/// `response` must be a live pointer returned by `peppy_tester_crypto_dispatch`,
/// still uniquely owned by the caller. It must not be used during or after this
/// call and must not have been freed previously.
pub unsafe extern "C" fn peppy_tester_crypto_free_response(response: *mut u8) {
    if !response.is_null() {
        let mut bytes =
            unsafe { std::ffi::CString::from_raw(response.cast()) }.into_bytes_with_nul();
        bytes.zeroize();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use peppy_crypto::{KeyProfile, create_vault_check_header};
    fn fixture() -> (
        VaultBindingV2,
        peppy_hosted_api::tester_admin_v2::VaultCheckHeaderV2,
    ) {
        let profile = KeyProfile::new(Uuid::new_v4(), 7).unwrap();
        let root = derive_root_key("café", &profile).unwrap();
        let header = create_vault_check_header(&root, profile.clone()).unwrap();
        let binding = VaultBindingV2 {
            account_id: Uuid::new_v4(),
            vault_id: profile.vault_id,
            profile_fingerprint: profile.fingerprint().unwrap(),
            key_epoch: profile.key_epoch,
            profile: profile.into(),
        };
        (binding, header.into())
    }
    fn unlock(
        worker: &mut TesterCryptoWorker,
        binding: &VaultBindingV2,
        header: &peppy_hosted_api::tester_admin_v2::VaultCheckHeaderV2,
    ) -> Uuid {
        let response: Value = serde_json::from_str(&worker.dispatch(&json!({"command":"unlock","args":{"passphrase":"café","binding":binding,"vaultCheckHeader":header}}).to_string())).unwrap();
        serde_json::from_value(response["value"]["handle"].clone()).unwrap()
    }
    #[test]
    fn unicode_roundtrip_and_wrong_phrase() {
        let (binding, header) = fixture();
        let mut worker = TesterCryptoWorker::default();
        let bad: Value = serde_json::from_str(&worker.dispatch(&json!({"command":"unlock","args":{"passphrase":"wrong","binding":binding,"vaultCheckHeader":header}}).to_string())).unwrap();
        assert!(!bad["ok"].as_bool().unwrap());
        let handle = unlock(&mut worker, &binding, &header);
        let registry = Uuid::new_v4();
        let entry = Uuid::new_v4();
        let key_id = Uuid::new_v4();
        let encrypted: Value = serde_json::from_str(&worker.dispatch(&json!({"command":"encrypt","args":{"handle":handle,"registryId":registry,"entryId":entry,"keyId":key_id,"plaintext":"a@éxample.test"}}).to_string())).unwrap();
        let decrypted: Value = serde_json::from_str(&worker.dispatch(&json!({"command":"decrypt","args":{"handle":handle,"registryId":registry,"entryId":entry,"keyId":key_id,"ciphertext":encrypted["value"]}}).to_string())).unwrap();
        assert_eq!(decrypted["value"]["plaintext"], "a@éxample.test");
    }
    #[test]
    fn ciphertext_cannot_replay_across_entries_or_accounts() {
        let (binding, header) = fixture();
        let mut worker = TesterCryptoWorker::default();
        let handle = unlock(&mut worker, &binding, &header);
        let registry = Uuid::new_v4();
        let entry = Uuid::new_v4();
        let key_id = Uuid::new_v4();
        let encrypted: Value = serde_json::from_str(&worker.dispatch(&json!({"command":"encrypt","args":{"handle":handle,"registryId":registry,"entryId":entry,"keyId":key_id,"plaintext":"tester@example.test"}}).to_string())).unwrap();
        for changed_entry in [Uuid::new_v4(), entry] {
            let mut other = binding.clone();
            if changed_entry == entry {
                other.account_id = Uuid::new_v4();
            }
            let other_profile: KeyProfile = other.profile.clone().into();
            let root = derive_root_key("café", &other_profile).unwrap();
            let other_header = create_vault_check_header(&root, other_profile)
                .unwrap()
                .into();
            let other_handle = unlock(&mut worker, &other, &other_header);
            let result: Value = serde_json::from_str(&worker.dispatch(&json!({"command":"decrypt","args":{"handle":other_handle,"registryId":registry,"entryId":changed_entry,"keyId":key_id,"ciphertext":encrypted["value"]}}).to_string())).unwrap();
            assert!(!result["ok"].as_bool().unwrap());
        }
    }

    #[test]
    fn aad_matches_the_canonical_context_vector() {
        let profile = KeyProfile {
            crypto_suite: 1,
            salt: [7; 16],
            vault_id: Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap(),
            key_epoch: 9,
        };
        let binding = VaultBindingV2 {
            account_id: Uuid::parse_str("22222222-2222-2222-2222-222222222222").unwrap(),
            vault_id: profile.vault_id,
            profile_fingerprint: profile.fingerprint().unwrap(),
            key_epoch: profile.key_epoch,
            profile: profile.into(),
        };
        let actual = aad(
            &binding,
            Uuid::parse_str("33333333-3333-3333-3333-333333333333").unwrap(),
            Uuid::parse_str("44444444-4444-4444-4444-444444444444").unwrap(),
            Uuid::parse_str("55555555-5555-5555-5555-555555555555").unwrap(),
        )
        .unwrap();
        assert_eq!(
            hex::encode(actual),
            "70657070792d7465737465722d7661756c742d763200222222222222222222222222222222223333333333333333333333333333333344444444444444444444444444444444555555555555555555555555555555551c0196d5cf53ba1d8abb470de26c1a91a280cbff8250d826ad5b6bd98511263c00000009"
        );
    }

    #[test]
    fn close_is_idempotent_and_oversized_requests_fail_closed() {
        let (binding, header) = fixture();
        let mut worker = TesterCryptoWorker::default();
        let handle = unlock(&mut worker, &binding, &header);
        for _ in 0..2 {
            let response: Value = serde_json::from_str(
                &worker.dispatch(&json!({"command":"close","args":{"handle":handle}}).to_string()),
            )
            .unwrap();
            assert!(response["ok"].as_bool().unwrap());
        }
        let response: Value =
            serde_json::from_str(&worker.dispatch(&"x".repeat(MAX_REQUEST_BYTES + 1))).unwrap();
        assert!(!response["ok"].as_bool().unwrap());
    }
}
