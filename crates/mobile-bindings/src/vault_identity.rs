use crate::{MobileBindingsError, NativeClient, with_client};
use peppy_client_core::{
    VaultIdentityAdoption, VaultIdentityReadiness, VaultIdentitySnapshotState,
};
use zeroize::{Zeroize, Zeroizing};

struct OwnedSecretInput(Zeroizing<Vec<u8>>);

impl OwnedSecretInput {
    fn new(bytes: Vec<u8>) -> Self {
        Self(Zeroizing::new(bytes))
    }

    fn take(&mut self) -> Vec<u8> {
        std::mem::take(&mut *self.0)
    }
}

impl Drop for OwnedSecretInput {
    fn drop(&mut self) {
        if self.0.is_empty() {
            return;
        }
        self.0.as_mut_slice().zeroize();
        #[cfg(test)]
        record_secret_input_wipe(self.0.iter().all(|byte| *byte == 0));
    }
}

#[derive(Clone, PartialEq, Eq, uniffi::Record)]
pub struct NativeVaultIdentitySnapshot {
    pub state: String,
    pub ready: bool,
    pub needs_fetch: bool,
    pub operation_id: Option<String>,
    pub key_epoch: Option<u32>,
    pub unavailable_code: Option<String>,
}
#[derive(uniffi::Record)]
pub struct NativeVaultIdentityPendingMaterial {
    pub operation_id: String,
    pub protected_pending_bytes: Vec<u8>,
    pub establish_body_json: String,
}
#[derive(uniffi::Record)]
pub struct NativeVaultIdentityAdoption {
    pub kind: String,
    pub operation_id: Option<String>,
    pub protected_accepted_bytes: Option<Vec<u8>>,
    pub pending_cleanup: bool,
    pub code: Option<String>,
}

#[uniffi::export]
impl NativeClient {
    pub fn gateway_vault_identity_snapshot(
        &self,
    ) -> Result<NativeVaultIdentitySnapshot, MobileBindingsError> {
        with_client(&self.client, |client| {
            let value = client.vault_identity_snapshot()?;
            Ok(NativeVaultIdentitySnapshot {
                state: match value.state {
                    VaultIdentitySnapshotState::Absent => "Absent",
                    VaultIdentitySnapshotState::Pending => "Pending",
                    VaultIdentitySnapshotState::Accepted => "Accepted",
                    VaultIdentitySnapshotState::Conflict => "Conflict",
                }
                .into(),
                ready: value.ready,
                needs_fetch: value.needs_fetch,
                operation_id: value.operation_id,
                key_epoch: value.key_epoch,
                unavailable_code: value.unavailable_code,
            })
        })
    }
    pub fn gateway_begin_vault_identity_establishment(
        &self,
    ) -> Result<NativeVaultIdentityPendingMaterial, MobileBindingsError> {
        with_client(&self.client, |client| {
            let value = client.begin_vault_identity_establishment()?;
            Ok(NativeVaultIdentityPendingMaterial {
                operation_id: value.operation_id,
                protected_pending_bytes: value.protected_pending_bytes.to_vec(),
                establish_body_json: value.establish_body_json,
            })
        })
    }
    pub fn gateway_confirm_vault_identity_pending_durable(
        &self,
        operation_id: String,
    ) -> Result<(), MobileBindingsError> {
        with_client(&self.client, |client| {
            client.confirm_vault_identity_pending_durable(operation_id)
        })
    }
    pub fn gateway_adopt_vault_identity(
        &self,
        status_json: String,
    ) -> Result<NativeVaultIdentityAdoption, MobileBindingsError> {
        with_client(&self.client, |client| {
            Ok(match client.adopt_vault_identity(status_json)? {
                VaultIdentityAdoption::AbsentEligible => NativeVaultIdentityAdoption {
                    kind: "AbsentEligible".into(),
                    operation_id: None,
                    protected_accepted_bytes: None,
                    pending_cleanup: false,
                    code: None,
                },
                VaultIdentityAdoption::AbsentNotEligible => NativeVaultIdentityAdoption {
                    kind: "AbsentNotEligible".into(),
                    operation_id: None,
                    protected_accepted_bytes: None,
                    pending_cleanup: false,
                    code: None,
                },
                VaultIdentityAdoption::Conflict => NativeVaultIdentityAdoption {
                    kind: "Conflict".into(),
                    operation_id: None,
                    protected_accepted_bytes: None,
                    pending_cleanup: false,
                    code: Some("identity_conflict".into()),
                },
                VaultIdentityAdoption::Unavailable { code } => NativeVaultIdentityAdoption {
                    kind: "Unavailable".into(),
                    operation_id: None,
                    protected_accepted_bytes: None,
                    pending_cleanup: false,
                    code: Some(code),
                },
                VaultIdentityAdoption::Adopted {
                    operation_id,
                    protected_accepted_bytes,
                    pending_cleanup,
                } => NativeVaultIdentityAdoption {
                    kind: "Adopted".into(),
                    operation_id: Some(operation_id),
                    protected_accepted_bytes: Some(protected_accepted_bytes.to_vec()),
                    pending_cleanup,
                    code: None,
                },
            })
        })
    }
    pub fn gateway_commit_vault_identity_acceptance(
        &self,
        operation_id: String,
    ) -> Result<(), MobileBindingsError> {
        with_client(&self.client, |client| {
            client.commit_vault_identity_acceptance(operation_id)
        })
    }
    pub fn gateway_restore_vault_identity_from_protected(
        &self,
        bytes: Vec<u8>,
    ) -> Result<(), MobileBindingsError> {
        let mut bytes = OwnedSecretInput::new(bytes);
        with_client(&self.client, move |client| {
            client.restore_vault_identity_from_protected(bytes.take())
        })
    }
    pub fn export_vault_identity_recovery_key_for_native_storage(
        &self,
        epoch: u32,
    ) -> Result<Vec<u8>, MobileBindingsError> {
        with_client(&self.client, |client| {
            client.export_vault_identity_recovery_key_for_native_storage(epoch)
        })
    }
    pub fn import_vault_identity_recovery_key_from_native_storage(
        &self,
        bytes: Vec<u8>,
    ) -> Result<(), MobileBindingsError> {
        let mut bytes = OwnedSecretInput::new(bytes);
        with_client(&self.client, move |client| {
            client.import_vault_identity_recovery_key_from_native_storage(bytes.take())
        })
    }
    pub fn gateway_vault_identity_readiness(&self) -> Result<String, MobileBindingsError> {
        with_client(&self.client, |client| {
            Ok(match client.vault_identity_readiness()? {
                VaultIdentityReadiness::Ready { .. } => "ready".into(),
                VaultIdentityReadiness::Unavailable { code } => code,
            })
        })
    }
}

#[cfg(test)]
thread_local! {
    static SECRET_INPUT_WIPED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
fn record_secret_input_wipe(wiped: bool) {
    SECRET_INPUT_WIPED.set(wiped);
}

#[cfg(test)]
fn reset_secret_input_wipe_observation() {
    SECRET_INPUT_WIPED.set(false);
}

#[cfg(test)]
fn assert_secret_input_was_wiped() {
    assert!(
        SECRET_INPUT_WIPED.get(),
        "owned secret input was not zeroized"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{MobileBindingsError, NativeOpenConfig, create_vault_material, open_native_client};
    use std::path::PathBuf;
    use uuid::Uuid;

    const PASSPHRASE: &str = "correct horse battery staple";

    fn native_client() -> (PathBuf, std::sync::Arc<NativeClient>) {
        let dir = std::env::temp_dir().join(format!("peppy-vault-identity-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let vault_id = Uuid::new_v4().to_string();
        let material = create_vault_material(vault_id.clone(), PASSPHRASE.into()).unwrap();
        let client = open_native_client(NativeOpenConfig {
            database_path: dir.join("native.db").to_string_lossy().into_owned(),
            vault_id,
            device_id: Uuid::new_v4().to_string(),
            database_key: vec![9; 32],
        })
        .unwrap();
        client
            .unlock(
                material.profile_json,
                material.header_json,
                PASSPHRASE.into(),
            )
            .unwrap();
        (dir, client)
    }

    #[test]
    fn identity_wrappers_return_closed_after_dispose() {
        let (dir, client) = native_client();
        client.dispose().unwrap();
        assert!(matches!(
            client.gateway_vault_identity_snapshot(),
            Err(MobileBindingsError::Closed)
        ));
        assert!(matches!(
            client.gateway_vault_identity_readiness(),
            Err(MobileBindingsError::Closed)
        ));
        drop(client);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn closed_client_zeroizes_every_owned_secret_input_before_returning() {
        let (dir, client) = native_client();
        client.dispose().unwrap();

        reset_secret_input_wipe_observation();
        assert!(matches!(
            client.gateway_restore_vault_identity_from_protected(vec![0x5a; 147]),
            Err(MobileBindingsError::Closed)
        ));
        assert_secret_input_was_wiped();

        reset_secret_input_wipe_observation();
        assert!(matches!(
            client.import_vault_identity_recovery_key_from_native_storage(vec![0xa5; 137]),
            Err(MobileBindingsError::Closed)
        ));
        assert_secret_input_was_wiped();

        drop(client);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn poisoned_client_mutex_zeroizes_every_owned_secret_input_before_returning() {
        let (dir, client) = native_client();
        let poisoning_client = client.clone();
        assert!(
            std::thread::spawn(move || {
                let _guard = poisoning_client.client.lock().unwrap();
                panic!("poison native client mutex");
            })
            .join()
            .is_err()
        );

        reset_secret_input_wipe_observation();
        assert!(matches!(
            client.gateway_restore_vault_identity_from_protected(vec![0x5a; 147]),
            Err(MobileBindingsError::Database)
        ));
        assert_secret_input_was_wiped();

        reset_secret_input_wipe_observation();
        assert!(matches!(
            client.import_vault_identity_recovery_key_from_native_storage(vec![0xa5; 137]),
            Err(MobileBindingsError::Database)
        ));
        assert_secret_input_was_wiped();

        drop(client);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn identity_wrappers_keep_fixed_error_codes_and_success_shape() {
        let (dir, client) = native_client();
        assert_eq!(
            client.gateway_vault_identity_readiness().unwrap(),
            "identity_unavailable"
        );
        let pending = client.gateway_begin_vault_identity_establishment().unwrap();
        assert_eq!(pending.protected_pending_bytes.len(), 147);
        assert!(matches!(
            client.gateway_confirm_vault_identity_pending_durable("bad".into()),
            Err(MobileBindingsError::InvalidRequest)
        ));
        client
            .gateway_confirm_vault_identity_pending_durable(pending.operation_id)
            .unwrap();
        let snapshot = client.gateway_vault_identity_snapshot().unwrap();
        assert_eq!(snapshot.state, "Pending");
        assert!(!snapshot.ready);
        assert_eq!(
            snapshot.unavailable_code.as_deref(),
            Some("identity_unavailable")
        );
        client.dispose().unwrap();
        drop(client);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn restore_wrapper_distinguishes_malformed_storage_from_valid_pin_conflict() {
        let (dir, client) = native_client();
        let rival = client.gateway_begin_vault_identity_establishment().unwrap();
        let accepted = client.gateway_begin_vault_identity_establishment().unwrap();
        client
            .gateway_confirm_vault_identity_pending_durable(accepted.operation_id.clone())
            .unwrap();
        let mut status: serde_json::Value =
            serde_json::from_str(&accepted.establish_body_json).unwrap();
        status["state"] = serde_json::json!("established");
        let adopted = client
            .gateway_adopt_vault_identity(status.to_string())
            .unwrap();
        client
            .gateway_commit_vault_identity_acceptance(adopted.operation_id.unwrap())
            .unwrap();
        assert!(matches!(
            client.gateway_restore_vault_identity_from_protected(vec![1, 2, 3]),
            Err(MobileBindingsError::InvalidKeyCache)
        ));
        assert!(matches!(
            client.gateway_restore_vault_identity_from_protected(rival.protected_pending_bytes),
            Err(MobileBindingsError::Conflict)
        ));
        client.dispose().unwrap();
        drop(client);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
