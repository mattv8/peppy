use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};

use peppy_client_core::{Client, ClientConfig, DatabaseKey};
use peppy_domain::{DeviceId, VaultId};
use peppy_hosted_api::mcp::{
    McpWorkerActivateRequest, McpWorkerActivateResponse, McpWorkerCancelRequest,
    McpWorkerCompleteRequest, McpWorkerPrepareRequest, McpWorkerPrepareResponse,
};
use peppy_hosted_client::{
    HostedClientError,
    newenrollment::{ClientKind, EnrollmentClient, NativeEnrollmentIdentity},
};
use thiserror::Error;
use zeroize::Zeroizing;

use crate::{
    WorkerConfig,
    registry::vault_state_dir,
    runtime::{ApprovedBinding, CompletionBinding, Runtime, RuntimeError},
};

#[derive(Debug, Error)]
pub enum EnrollmentError {
    #[error("enrollment request is invalid or no longer pending")]
    Invalid,
    #[error("enrollment cryptographic verification failed")]
    Enrollment(#[from] HostedClientError),
    #[error("worker local state could not be installed")]
    Runtime(#[from] RuntimeError),
}

/// Lifetime of an unactivated claimant; matches computer enrollment expiry.
const CLAIM_TTL: Duration = Duration::from_secs(300);

#[cfg(test)]
thread_local! {
    static INSTALL_FAULT_AFTER_RUNTIME_RECORD: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Ephemeral claimant state. It intentionally cannot survive process restart:
/// computer transfer private keys are non-exportable and restarting requires a
/// fresh explicit owner approval rather than serializing pairing secrets.
pub struct EnrollmentManager {
    config: WorkerConfig,
    pending: BTreeMap<uuid::Uuid, PendingEnrollment>,
}

enum PendingEnrollment {
    Claimant(Box<PendingClaim>),
    Installing(Box<InstallingEnrollment>),
    Completed(CompletedEnrollment),
}

struct PendingClaim {
    vault_id: uuid::Uuid,
    owner_device_id: uuid::Uuid,
    created_at: Instant,
    client: EnrollmentClient,
}

impl PendingClaim {
    fn expired_at(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.created_at) > CLAIM_TTL
    }
}

impl PendingEnrollment {
    fn is_claim_for(&self, vault_id: uuid::Uuid, owner_device_id: uuid::Uuid) -> bool {
        match self {
            PendingEnrollment::Claimant(claim) => {
                claim.vault_id == vault_id && claim.owner_device_id == owner_device_id
            }
            PendingEnrollment::Installing(installing) => {
                installing.vault_id == vault_id && installing.owner_device_id == owner_device_id
            }
            PendingEnrollment::Completed(_) => false,
        }
    }
}

struct InstallingEnrollment {
    identity: NativeEnrollmentIdentity,
    database_key: Zeroizing<[u8; 32]>,
    operation_id: uuid::Uuid,
    admission: [u8; 32],
    vault_id: uuid::Uuid,
    owner_device_id: uuid::Uuid,
    created_at: Instant,
}

impl InstallingEnrollment {
    fn expired_at(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.created_at) > CLAIM_TTL
    }
}

struct CompletedEnrollment {
    vault_id: VaultId,
    device_id: DeviceId,
    admission: [u8; 32],
}

impl EnrollmentManager {
    pub fn new(config: WorkerConfig) -> Self {
        Self {
            config,
            pending: BTreeMap::new(),
        }
    }

    /// Creates the single claimant for this vault and owner, replacing any
    /// earlier unactivated one so an abandoned preparation cannot linger.
    pub fn prepare(
        &mut self,
        request: McpWorkerPrepareRequest,
    ) -> Result<McpWorkerPrepareResponse, EnrollmentError> {
        let now = Instant::now();
        self.sweep_expired(now);
        let mut client = EnrollmentClient::new(
            self.config.public_api_url.clone(),
            ClientKind::Desktop,
            "Peppy MCP".into(),
        )?;
        let pairing: peppy_hosted_api::computer_enrollment::ComputerPairingQr =
            serde_json::from_str(&client.begin_computer_claimant()?)
                .map_err(|_| EnrollmentError::Invalid)?;
        self.pending
            .retain(|_, pending| !pending.is_claim_for(request.vault_id, request.owner_device_id));
        let claim = PendingClaim {
            vault_id: request.vault_id,
            owner_device_id: request.owner_device_id,
            created_at: now,
            client,
        };
        if self
            .pending
            .insert(
                pairing.invitation_id,
                PendingEnrollment::Claimant(Box::new(claim)),
            )
            .is_some()
        {
            return Err(EnrollmentError::Invalid);
        }
        Ok(McpWorkerPrepareResponse {
            operation_id: pairing.invitation_id,
            pairing,
        })
    }

    /// Removes the claimant only when both its vault and owner match. Unknown,
    /// mismatched, and already-removed claimants are no-ops, so the call is
    /// idempotent and never touches a completed enrollment.
    pub fn cancel(&mut self, request: McpWorkerCancelRequest) {
        self.sweep_expired(Instant::now());
        let matches = self
            .pending
            .get(&request.invitation_id)
            .is_some_and(|pending| pending.is_claim_for(request.vault_id, request.owner_device_id));
        if matches {
            self.pending.remove(&request.invitation_id);
        }
    }

    /// Drops incomplete enrollment state older than `CLAIM_TTL`; completed
    /// enrollments remain available for replay.
    pub fn sweep_expired(&mut self, now: Instant) {
        self.pending.retain(|_, pending| match pending {
            PendingEnrollment::Claimant(claim) => !claim.expired_at(now),
            PendingEnrollment::Installing(installing) => !installing.expired_at(now),
            PendingEnrollment::Completed(_) => true,
        });
    }

    /// The host acquires `status` using its current owner/account authorization;
    /// account credentials never cross this process boundary.
    pub fn activate(
        &mut self,
        request: McpWorkerActivateRequest,
    ) -> Result<McpWorkerActivateResponse, EnrollmentError> {
        self.sweep_expired(Instant::now());
        let pending = self
            .pending
            .get_mut(&request.operation_id)
            .ok_or(EnrollmentError::Invalid)?;
        let PendingEnrollment::Claimant(claim) = pending else {
            return Err(EnrollmentError::Invalid);
        };
        let transcript = request
            .status
            .transcript
            .as_ref()
            .ok_or(EnrollmentError::Invalid)?;
        if claim.vault_id != request.vault_id
            || transcript.vault_id != claim.vault_id
            || transcript.owner_device_id != claim.owner_device_id
        {
            return Err(EnrollmentError::Invalid);
        }
        let status =
            serde_json::to_string(&request.status).map_err(|_| EnrollmentError::Invalid)?;
        let confirm = claim.client.confirm_computer_enrollment(status)?;
        let confirm = serde_json::from_str(&confirm).map_err(|_| EnrollmentError::Invalid)?;
        Ok(McpWorkerActivateResponse { confirm })
    }

    /// Imports the verified handoff into the actual SQLCipher client, exports a
    /// purpose-key cache only after that import, and persists every restart
    /// component in the independent encrypted custody record.
    pub fn complete(
        &mut self,
        request: McpWorkerCompleteRequest,
    ) -> Result<Runtime, EnrollmentError> {
        self.sweep_expired(Instant::now());
        let admission = admission_fingerprint(&request.admission)?;
        let Some(pending) = self.pending.remove(&request.operation_id) else {
            return self.restore_completed(&request, admission);
        };
        let (result, pending) = match pending {
            PendingEnrollment::Claimant(mut claim) => {
                if claim.vault_id != request.vault_id {
                    (
                        Err(EnrollmentError::Invalid),
                        PendingEnrollment::Claimant(claim),
                    )
                } else {
                    let identity = claim.client.finish_computer_admission(
                        serde_json::to_string(&request.admission)
                            .map_err(|_| EnrollmentError::Invalid)?,
                    );
                    match identity {
                        Ok(identity) => {
                            let mut database_key = [0_u8; 32];
                            libsodium_rs::random::fill_bytes(&mut database_key);
                            let installing = InstallingEnrollment {
                                identity,
                                database_key: Zeroizing::new(database_key),
                                operation_id: request.operation_id,
                                admission,
                                vault_id: claim.vault_id,
                                owner_device_id: claim.owner_device_id,
                                created_at: claim.created_at,
                            };
                            if installing.identity.metadata().vault_id == request.vault_id {
                                self.install(installing)
                            } else {
                                (
                                    Err(EnrollmentError::Invalid),
                                    PendingEnrollment::Installing(Box::new(installing)),
                                )
                            }
                        }
                        Err(error) => (Err(error.into()), PendingEnrollment::Claimant(claim)),
                    }
                }
            }
            PendingEnrollment::Installing(installing) if installing.admission == admission => {
                if installing.identity.metadata().vault_id == request.vault_id {
                    self.install(*installing)
                } else {
                    (
                        Err(EnrollmentError::Invalid),
                        PendingEnrollment::Installing(installing),
                    )
                }
            }
            PendingEnrollment::Installing(installing) => (
                Err(EnrollmentError::Invalid),
                PendingEnrollment::Installing(installing),
            ),
            PendingEnrollment::Completed(completed)
                if completed.admission == admission && completed.vault_id.0 == request.vault_id =>
            {
                let runtime = Runtime::open_existing(
                    vault_state_dir(&self.config.state_dir, completed.vault_id.0),
                    self.config.key_file.clone(),
                    self.config.api_url.clone(),
                    completed.vault_id,
                    completed.device_id,
                )
                .map_err(Into::into);
                (runtime, PendingEnrollment::Completed(completed))
            }
            PendingEnrollment::Completed(completed) => (
                Err(EnrollmentError::Invalid),
                PendingEnrollment::Completed(completed),
            ),
        };
        self.pending.insert(request.operation_id, pending);
        result
    }

    fn install(
        &self,
        mut installing: InstallingEnrollment,
    ) -> (Result<Runtime, EnrollmentError>, PendingEnrollment) {
        let metadata = installing.identity.metadata().clone();
        let vault_id = VaultId(metadata.vault_id);
        let device_id = DeviceId(metadata.device_id);
        let state_dir = vault_state_dir(&self.config.state_dir, metadata.vault_id);
        let result = (|| {
            let client = Client::open(
                ClientConfig {
                    database_path: state_dir.join("vault.sqlite"),
                    vault_id,
                    device_id,
                },
                DatabaseKey::new(installing.database_key.as_ref())
                    .map_err(|_| EnrollmentError::Invalid)?,
            )
            .map_err(|_| EnrollmentError::Invalid)?;
            installing
                .identity
                .prepare_verified_handoff_activation(&client)?;
            let cache = client
                .export_native_key_cache(metadata.profile.key_epoch)
                .map_err(|_| EnrollmentError::Invalid)?;
            Runtime::persist_approved(
                state_dir.clone(),
                self.config.key_file.clone(),
                ApprovedBinding {
                    vault_id,
                    device_id,
                    database_key: *installing.database_key,
                    device_credential: installing
                        .identity
                        .device_credential_for_native_secure_store(),
                    key_cache: cache,
                    completion: CompletionBinding {
                        operation_id: installing.operation_id,
                        admission: installing.admission,
                    },
                },
            )?;
            #[cfg(test)]
            if INSTALL_FAULT_AFTER_RUNTIME_RECORD.with(|fault| fault.replace(false)) {
                return Err(EnrollmentError::Invalid);
            }
            installing.identity.commit_verified_handoff_activation();
            Runtime::open_existing(
                state_dir,
                self.config.key_file.clone(),
                self.config.api_url.clone(),
                vault_id,
                device_id,
            )
            .map_err(Into::into)
        })();
        match result {
            Ok(runtime) => (
                Ok(runtime),
                PendingEnrollment::Completed(CompletedEnrollment {
                    vault_id,
                    device_id,
                    admission: installing.admission,
                }),
            ),
            Err(error) => (
                Err(error),
                PendingEnrollment::Installing(Box::new(installing)),
            ),
        }
    }

    fn restore_completed(
        &self,
        request: &McpWorkerCompleteRequest,
        admission: [u8; 32],
    ) -> Result<Runtime, EnrollmentError> {
        let state_dir = vault_state_dir(&self.config.state_dir, request.vault_id);
        if !state_dir.join("vault.sqlite").is_file() {
            return Err(EnrollmentError::Invalid);
        }
        let device_id = DeviceId(request.admission.device_id);
        Runtime::open_completed_existing(
            state_dir,
            self.config.key_file.clone(),
            self.config.api_url.clone(),
            VaultId(request.vault_id),
            device_id,
            CompletionBinding {
                operation_id: request.operation_id,
                admission,
            },
        )
        .map_err(Into::into)
    }
}

fn admission_fingerprint(
    admission: &peppy_hosted_api::enrollment::AdmissionComplete,
) -> Result<[u8; 32], EnrollmentError> {
    use sha2::{Digest, Sha256};

    Ok(Sha256::digest(serde_json::to_vec(admission).map_err(|_| EnrollmentError::Invalid)?).into())
}

#[cfg(test)]
mod tests {
    use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
    use peppy_crypto::{KeyProfile, create_vault_check_header, derive_root_key};
    use peppy_hosted_api::{
        computer_enrollment::{
            ApproveComputerEnrollmentRequest, ComputerEnrollmentCreated, ComputerEnrollmentStatus,
            ComputerPairingQr,
        },
        enrollment::{AdmissionComplete, EnrollmentRole, EnrollmentState, VaultMaterial},
        mcp::{McpWorkerActivateRequest, McpWorkerCompleteRequest, McpWorkerPrepareRequest},
    };
    use peppy_hosted_client::newenrollment::{ClientKind, EnrollmentClient};

    use crate::registry::RuntimeRegistry;

    use super::*;

    fn config(root: &tempfile::TempDir) -> WorkerConfig {
        let config = WorkerConfig {
            api_url: "https://api.example".into(),
            public_api_url: "https://enroll.example".into(),
            state_dir: root.path().join("state"),
            key_file: root.path().join("storage-key"),
            worker_token_file: root.path().join("worker-token"),
            bind_addr: "127.0.0.1:8091".parse().unwrap(),
            webhooks_enabled: false,
            webhooks_allow_private: false,
            app_origin: "https://enroll.example".into(),
        };
        RuntimeRegistry::open(config.clone()).unwrap();
        config
    }

    fn state_bytes(path: &std::path::Path) -> Vec<u8> {
        let mut bytes = Vec::new();
        for entry in std::fs::read_dir(path).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                bytes.extend(state_bytes(&path));
            } else {
                bytes.extend(std::fs::read(path).unwrap());
            }
        }
        bytes
    }

    /// Runs the actual claimant and owner handoff exchange, stopping at the
    /// host-owned server admission boundary.
    fn approved_admission(manager: &mut EnrollmentManager) -> McpWorkerCompleteRequest {
        let vault_id = uuid::Uuid::new_v4();
        let owner_device_id = uuid::Uuid::new_v4();
        let prepared = manager
            .prepare(McpWorkerPrepareRequest {
                vault_id,
                owner_device_id,
            })
            .unwrap();
        let (status, material) = approved_status(&prepared.pairing, vault_id, owner_device_id);
        let device_id = status
            .transcript
            .as_ref()
            .unwrap()
            .recipient
            .device
            .device_id;
        let replay_status = serde_json::to_value(&status).unwrap();
        let confirmation = manager
            .activate(McpWorkerActivateRequest {
                vault_id,
                operation_id: prepared.operation_id,
                status,
            })
            .unwrap();
        let replay = manager
            .activate(McpWorkerActivateRequest {
                vault_id,
                operation_id: prepared.operation_id,
                status: serde_json::from_value(replay_status).unwrap(),
            })
            .unwrap();
        assert_eq!(
            serde_json::to_value(confirmation).unwrap(),
            serde_json::to_value(replay).unwrap()
        );
        McpWorkerCompleteRequest {
            vault_id,
            operation_id: prepared.operation_id,
            admission: AdmissionComplete {
                vault_id,
                device_id,
                role: EnrollmentRole::Device,
                material,
            },
        }
    }

    fn approved_status(
        pairing: &ComputerPairingQr,
        vault_id: uuid::Uuid,
        owner_device_id: uuid::Uuid,
    ) -> (ComputerEnrollmentStatus, VaultMaterial) {
        let profile = KeyProfile::new(vault_id, 3).unwrap();
        let root = derive_root_key("correct horse battery staple", &profile).unwrap();
        let header = create_vault_check_header(&root, profile.clone()).unwrap();
        let mut owner = EnrollmentClient::for_existing_owner(
            "https://enroll.example".into(),
            ClientKind::Desktop,
            "Owner".into(),
            profile.clone(),
            header.clone(),
        )
        .unwrap();
        owner
            .configure_computer_owner_device(owner_device_id)
            .unwrap();
        owner
            .scan_computer_qr(serde_json::to_string(pairing).unwrap())
            .unwrap();
        owner
            .accept_computer_created(
                serde_json::to_string(&ComputerEnrollmentCreated {
                    server_nonce: URL_SAFE_NO_PAD.encode([7; 32]),
                    expires_in_seconds: 60,
                })
                .unwrap(),
            )
            .unwrap();
        let owner_store = tempfile::tempdir().unwrap();
        let owner_client = Client::open(
            ClientConfig {
                database_path: owner_store.path().join("owner.sqlite"),
                vault_id: VaultId(vault_id),
                device_id: DeviceId(owner_device_id),
            },
            DatabaseKey::new(&[31; 32]).unwrap(),
        )
        .unwrap();
        owner_client
            .unlock(&profile, &header, "correct horse battery staple")
            .unwrap();
        let approval: ApproveComputerEnrollmentRequest = serde_json::from_str(
            &owner
                .approve_computer_enrollment_with_client(true, &owner_client)
                .unwrap(),
        )
        .unwrap();
        let status = ComputerEnrollmentStatus {
            state: EnrollmentState::Approved,
            expires_in_seconds: 60,
            transcript: Some(approval.transcript),
            encrypted_key_package: Some(approval.encrypted_key_package),
            transfer_nonce: Some(approval.transfer_nonce),
            owner_proof: Some(approval.owner_proof),
        };
        let material = VaultMaterial {
            profile: serde_json::to_value(&profile).unwrap(),
            check_header: URL_SAFE_NO_PAD.encode(serde_json::to_vec(&header).unwrap()),
            fingerprint: profile.fingerprint().unwrap(),
            key_epoch: profile.key_epoch,
            admission_public_key: URL_SAFE_NO_PAD.encode([0; 32]),
        };
        (status, material)
    }

    #[test]
    fn invalid_admission_keeps_the_verified_claimant_for_a_matching_retry() {
        let root = tempfile::tempdir().unwrap();
        let mut manager = EnrollmentManager::new(config(&root));
        let request = approved_admission(&mut manager);
        let mut invalid = request.clone();
        invalid.vault_id = uuid::Uuid::new_v4();

        assert!(manager.complete(invalid).is_err());
        assert!(manager.complete(request).unwrap().state_dir().exists());
    }

    #[test]
    fn activation_rejects_a_valid_approval_for_the_wrong_vault_or_owner() {
        let root = tempfile::tempdir().unwrap();
        let mut manager = EnrollmentManager::new(config(&root));
        let vault_id = uuid::Uuid::new_v4();
        let owner_device_id = uuid::Uuid::new_v4();
        let prepared = prepare_for(&mut manager, vault_id, owner_device_id);

        let wrong_vault =
            approved_status(&prepared.pairing, uuid::Uuid::new_v4(), owner_device_id).0;
        assert!(
            manager
                .activate(McpWorkerActivateRequest {
                    vault_id,
                    operation_id: prepared.operation_id,
                    status: wrong_vault,
                })
                .is_err()
        );

        let wrong_owner = approved_status(&prepared.pairing, vault_id, uuid::Uuid::new_v4()).0;
        assert!(
            manager
                .activate(McpWorkerActivateRequest {
                    vault_id,
                    operation_id: prepared.operation_id,
                    status: wrong_owner,
                })
                .is_err()
        );

        assert!(
            manager
                .activate(McpWorkerActivateRequest {
                    vault_id,
                    operation_id: prepared.operation_id,
                    status: approved_status(&prepared.pairing, vault_id, owner_device_id).0,
                })
                .is_ok()
        );
    }

    #[test]
    fn failed_custody_write_reuses_the_verified_identity_and_database_key() {
        let root = tempfile::tempdir().unwrap();
        let mut manager = EnrollmentManager::new(config(&root));
        let request = approved_admission(&mut manager);
        let unusable_key_file = root.path().join("not-a-key-file");
        std::fs::create_dir(&unusable_key_file).unwrap();
        manager.config.key_file = unusable_key_file;

        assert!(manager.complete(request.clone()).is_err());
        manager.config.key_file = root.path().join("storage-key");
        assert!(!manager.complete(request).unwrap().device_id().0.is_nil());
    }

    #[test]
    fn completed_admission_replays_only_the_matching_request() {
        let root = tempfile::tempdir().unwrap();
        let mut manager = EnrollmentManager::new(config(&root));
        let request = approved_admission(&mut manager);
        let expected_device_id = request.admission.device_id;

        let runtime = manager.complete(request.clone()).unwrap();
        assert_eq!(runtime.device_id().0, expected_device_id);
        assert!(
            !state_bytes(root.path())
                .windows(b"correct horse battery staple".len())
                .any(|bytes| { bytes == b"correct horse battery staple" }),
            "worker state must not serialize the handoff root"
        );
        assert_eq!(
            manager.complete(request.clone()).unwrap().device_id().0,
            expected_device_id
        );
        let mut altered = serde_json::to_value(request).unwrap();
        altered["admission"]["material"]["fingerprint"] = serde_json::json!("altered");
        let altered = serde_json::from_value(altered).unwrap();
        assert!(manager.complete(altered).is_err());
    }

    #[test]
    fn runtime_record_recovers_the_exact_request_after_a_post_record_crash() {
        let root = tempfile::tempdir().unwrap();
        let config = config(&root);
        let mut manager = EnrollmentManager::new(config.clone());
        let request = approved_admission(&mut manager);
        let expected_device_id = request.admission.device_id;

        INSTALL_FAULT_AFTER_RUNTIME_RECORD.set(true);
        assert!(manager.complete(request.clone()).is_err());
        assert!(
            !INSTALL_FAULT_AFTER_RUNTIME_RECORD.get(),
            "the injected post-record fault was not reached"
        );
        let record = vault_state_dir(&config.state_dir, request.vault_id)
            .join("custody")
            .join("runtime.sealed");
        assert!(
            record.is_file(),
            "runtime record was not sealed before the fault"
        );
        drop(manager);

        let mut restored = EnrollmentManager::new(config.clone());
        assert_eq!(
            restored.complete(request.clone()).unwrap().device_id().0,
            expected_device_id
        );
        let mut altered = serde_json::to_value(&request).unwrap();
        altered["admission"]["material"]["fingerprint"] = serde_json::json!("altered");
        let altered = serde_json::from_value(altered).unwrap();
        assert!(restored.complete(altered).is_err());

        drop(restored);
        std::fs::write(&record, b"corrupt").unwrap();
        assert!(
            EnrollmentManager::new(config.clone())
                .complete(request.clone())
                .is_err()
        );
        std::fs::remove_file(&record).unwrap();
        assert!(EnrollmentManager::new(config).complete(request).is_err());
    }

    fn prepare_for(
        manager: &mut EnrollmentManager,
        vault_id: uuid::Uuid,
        owner_device_id: uuid::Uuid,
    ) -> McpWorkerPrepareResponse {
        manager
            .prepare(McpWorkerPrepareRequest {
                vault_id,
                owner_device_id,
            })
            .unwrap()
    }

    fn expire_claimant(manager: &mut EnrollmentManager, operation_id: uuid::Uuid) {
        let Some(PendingEnrollment::Claimant(claim)) = manager.pending.get_mut(&operation_id)
        else {
            panic!("expected a pending claimant");
        };
        claim.created_at = Instant::now()
            .checked_sub(CLAIM_TTL + Duration::from_secs(1))
            .unwrap();
    }

    fn expire_installing(manager: &mut EnrollmentManager, operation_id: uuid::Uuid) {
        let Some(PendingEnrollment::Installing(installing)) =
            manager.pending.get_mut(&operation_id)
        else {
            panic!("expected an installing enrollment");
        };
        installing.created_at = Instant::now()
            .checked_sub(CLAIM_TTL + Duration::from_secs(1))
            .unwrap();
    }

    #[test]
    fn repeated_prepare_keeps_one_claimant_per_vault_and_owner() {
        let root = tempfile::tempdir().unwrap();
        let mut manager = EnrollmentManager::new(config(&root));
        let vault_id = uuid::Uuid::new_v4();
        let owner_device_id = uuid::Uuid::new_v4();
        let abandoned = prepare_for(&mut manager, vault_id, owner_device_id);
        let other_owner = prepare_for(&mut manager, vault_id, uuid::Uuid::new_v4());
        let fresh = prepare_for(&mut manager, vault_id, owner_device_id);

        assert_eq!(manager.pending.len(), 2);
        assert!(!manager.pending.contains_key(&abandoned.operation_id));
        assert!(manager.pending.contains_key(&other_owner.operation_id));
        assert!(manager.pending.contains_key(&fresh.operation_id));
    }

    #[test]
    fn cancel_removes_only_the_exact_pending_claimant() {
        let root = tempfile::tempdir().unwrap();
        let mut manager = EnrollmentManager::new(config(&root));
        let vault_id = uuid::Uuid::new_v4();
        let owner_device_id = uuid::Uuid::new_v4();
        let prepared = prepare_for(&mut manager, vault_id, owner_device_id);
        let invitation_id = prepared.operation_id;

        manager.cancel(McpWorkerCancelRequest {
            vault_id: uuid::Uuid::new_v4(),
            owner_device_id,
            invitation_id,
        });
        manager.cancel(McpWorkerCancelRequest {
            vault_id,
            owner_device_id: uuid::Uuid::new_v4(),
            invitation_id,
        });
        assert!(
            manager.pending.contains_key(&invitation_id),
            "a mismatched vault or owner must not delete the claimant"
        );

        manager.cancel(McpWorkerCancelRequest {
            vault_id,
            owner_device_id,
            invitation_id: uuid::Uuid::new_v4(),
        });
        manager.cancel(McpWorkerCancelRequest {
            vault_id,
            owner_device_id,
            invitation_id,
        });
        assert!(!manager.pending.contains_key(&invitation_id));
        manager.cancel(McpWorkerCancelRequest {
            vault_id,
            owner_device_id,
            invitation_id,
        });
    }

    #[test]
    fn cancel_never_touches_a_completed_enrollment() {
        let root = tempfile::tempdir().unwrap();
        let mut manager = EnrollmentManager::new(config(&root));
        let request = approved_admission(&mut manager);
        let expected_device_id = request.admission.device_id;
        manager.complete(request.clone()).unwrap();

        manager.cancel(McpWorkerCancelRequest {
            vault_id: request.vault_id,
            owner_device_id: uuid::Uuid::nil(),
            invitation_id: request.operation_id,
        });

        assert_eq!(
            manager.complete(request).unwrap().device_id().0,
            expected_device_id
        );
    }

    #[test]
    fn cancel_and_expiry_drop_only_the_matching_installing_enrollment() {
        let root = tempfile::tempdir().unwrap();
        let mut manager = EnrollmentManager::new(config(&root));
        let request = approved_admission(&mut manager);
        let owner_device_id = manager
            .pending
            .get(&request.operation_id)
            .and_then(|pending| match pending {
                PendingEnrollment::Claimant(claim) => Some(claim.owner_device_id),
                _ => None,
            })
            .unwrap();
        let unusable_key_file = root.path().join("not-a-key-file");
        std::fs::create_dir(&unusable_key_file).unwrap();
        manager.config.key_file = unusable_key_file;
        assert!(manager.complete(request.clone()).is_err());

        manager.cancel(McpWorkerCancelRequest {
            vault_id: request.vault_id,
            owner_device_id: uuid::Uuid::new_v4(),
            invitation_id: request.operation_id,
        });
        assert!(manager.pending.contains_key(&request.operation_id));
        manager.cancel(McpWorkerCancelRequest {
            vault_id: request.vault_id,
            owner_device_id,
            invitation_id: request.operation_id,
        });
        assert!(!manager.pending.contains_key(&request.operation_id));

        let retry = approved_admission(&mut manager);
        assert!(manager.complete(retry.clone()).is_err());
        expire_installing(&mut manager, retry.operation_id);
        manager.sweep_expired(Instant::now());
        assert!(!manager.pending.contains_key(&retry.operation_id));
    }

    #[test]
    fn sweep_removes_only_claimants_older_than_the_ttl() {
        let root = tempfile::tempdir().unwrap();
        let mut manager = EnrollmentManager::new(config(&root));
        let vault_id = uuid::Uuid::new_v4();
        let stale = prepare_for(&mut manager, vault_id, uuid::Uuid::new_v4());
        let fresh = prepare_for(&mut manager, vault_id, uuid::Uuid::new_v4());
        expire_claimant(&mut manager, stale.operation_id);

        manager.sweep_expired(Instant::now());

        assert!(!manager.pending.contains_key(&stale.operation_id));
        assert!(manager.pending.contains_key(&fresh.operation_id));
    }

    #[test]
    fn expired_claimant_cannot_complete_and_is_removed() {
        let root = tempfile::tempdir().unwrap();
        let mut manager = EnrollmentManager::new(config(&root));
        let request = approved_admission(&mut manager);
        expire_claimant(&mut manager, request.operation_id);

        assert!(manager.complete(request.clone()).is_err());
        assert!(!manager.pending.contains_key(&request.operation_id));
    }
}
