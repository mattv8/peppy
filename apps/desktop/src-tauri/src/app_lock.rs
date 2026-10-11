//! Local application-open password verifier. The verifier is opaque, versioned data in the OS
//! secure store; plaintext passwords never leave the native prompt.
use crate::{
    error::{BridgeError, BridgeResult},
    secure_store::SecretStore,
};
use peppy_crypto::{
    create_vault_check_header, derive_root_key, verify_vault_check_header, KeyProfile,
    VaultCheckHeader,
};
use serde::{Deserialize, Serialize};
use std::{
    sync::Mutex,
    time::{Duration, Instant},
};
use uuid::Uuid;
use zeroize::Zeroizing;

const ACCOUNT: &str = "app-lock:v1";
const VERSION: u8 = 1;
const MAX_FAILURES: u32 = 5;
const BACKOFF: Duration = Duration::from_millis(500);

#[derive(Serialize, Deserialize)]
struct Record {
    version: u8,
    state: String,
    verifier: Option<VaultCheckHeader>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Disabled,
    Enabled,
}

#[derive(Default)]
pub struct Attempts(Mutex<(u32, Option<Instant>)>);

fn invalid_record() -> BridgeError {
    BridgeError::new(
        "app-lock-corrupt",
        "The local app-lock record is invalid; Peppy cannot safely continue.",
    )
}

fn decode(bytes: &[u8]) -> BridgeResult<Record> {
    let record: Record = serde_json::from_slice(bytes).map_err(|_| invalid_record())?;
    if record.version != VERSION
        || !matches!(record.state.as_str(), "enabled" | "disabled")
        || (record.state == "enabled") != record.verifier.is_some()
    {
        return Err(invalid_record());
    }
    Ok(record)
}

pub fn status(store: &dyn SecretStore) -> BridgeResult<Status> {
    match store.get(ACCOUNT)? {
        None => Ok(Status::Disabled),
        Some(bytes) => Ok(if decode(&bytes)?.state == "enabled" {
            Status::Enabled
        } else {
            Status::Disabled
        }),
    }
}

pub fn enable(store: &dyn SecretStore, password: &str) -> BridgeResult<()> {
    let profile = KeyProfile::new(Uuid::new_v4(), 1).map_err(|_| invalid_record())?;
    let root = derive_root_key(password, &profile)
        .map_err(|_| BridgeError::new("app-lock-password", "The password could not be used."))?;
    let verifier = create_vault_check_header(&root, profile).map_err(|_| invalid_record())?;
    let bytes = serde_json::to_vec(&Record {
        version: VERSION,
        state: "enabled".into(),
        verifier: Some(verifier),
    })
    .map_err(|_| invalid_record())?;
    store.set(ACCOUNT, &bytes)
}

pub fn disable(store: &dyn SecretStore) -> BridgeResult<()> {
    let bytes = serde_json::to_vec(&Record {
        version: VERSION,
        state: "disabled".into(),
        verifier: None,
    })
    .map_err(|_| invalid_record())?;
    store.set(ACCOUNT, &bytes)
}

pub fn verify(
    store: &dyn SecretStore,
    password: Zeroizing<String>,
    attempts: &Attempts,
) -> BridgeResult<bool> {
    let mut failures = attempts.0.lock().map_err(|_| BridgeError::host_state())?;
    if let Some(until) = failures.1 {
        if until > Instant::now() {
            return Ok(false);
        }
    }
    let Some(bytes) = store.get(ACCOUNT)? else {
        return Ok(false);
    };
    let record = decode(&bytes)?;
    let Some(verifier) = record.verifier else {
        return Ok(false);
    };
    let root = derive_root_key(&password, &verifier.profile)
        .map_err(|_| BridgeError::new("app-lock-password", "The password could not be used."))?;
    let valid = verify_vault_check_header(&root, &verifier.profile, &verifier).is_ok();
    if valid {
        *failures = (0, None);
    } else {
        failures.0 = failures.0.saturating_add(1);
        if failures.0 >= MAX_FAILURES {
            failures.1 = Some(Instant::now() + BACKOFF);
            failures.0 = 0;
        }
    }
    Ok(valid)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secure_store::MemoryStore;
    #[test]
    fn verifier_rejects_wrong_password_and_corruption() {
        let store = MemoryStore::default();
        enable(&store, "correct horse").unwrap();
        assert!(!verify(&store, Zeroizing::new("wrong".into()), &Attempts::default()).unwrap());
        store.set(ACCOUNT, b"bad").unwrap();
        assert!(verify(
            &store,
            Zeroizing::new("correct horse".into()),
            &Attempts::default()
        )
        .is_err());
    }
}
