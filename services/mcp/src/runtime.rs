use std::{path::PathBuf, sync::Arc, time::SystemTime};

use peppy_client_core::{Client, ClientConfig, DatabaseKey, NativeKeyCache};
use peppy_domain::{DeviceId, VaultId};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::{Mutex, Notify};
use uuid::Uuid;
use zeroize::{Zeroize, Zeroizing};

use crate::custody::{Custody, CustodyError};

const RECORD_NAME: &str = "runtime";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RuntimeState {
    Offline,
    Online,
    NeedsApproval,
    Revoked,
}

#[derive(Debug, Error)]
pub enum RuntimeError {
    #[error("runtime needs owner approval")]
    NeedsApproval,
    #[error("runtime identity does not match the requested vault")]
    IdentityMismatch,
    #[error("runtime state is unavailable")]
    Custody(#[from] CustodyError),
    #[error("client core rejected local state")]
    Core,
    #[error("device sync failed")]
    Sync,
    #[error("outbox upload was rejected by the server")]
    OutboxRejected,
    #[error("device is no longer authorized")]
    NotLive,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredRuntime {
    vault_id: Uuid,
    device_id: Uuid,
    database_key: Vec<u8>,
    device_credential: String,
    key_cache: Vec<u8>,
}

impl Drop for StoredRuntime {
    fn drop(&mut self) {
        self.database_key.zeroize();
        self.device_credential.zeroize();
        self.key_cache.zeroize();
    }
}

/// One actual `Client` and credential for one enabled vault. Calls serialize
/// sync/publish locally; server lifecycle fencing still decides whether it may run.
pub struct Runtime {
    state_dir: PathBuf,
    transport: peppy_gateway_simulator::HttpTransport,
    client: Client,
    vault_id: VaultId,
    device_id: DeviceId,
    credential: Zeroizing<String>,
    state: Arc<Mutex<RuntimeState>>,
    last_synced_at: Arc<Mutex<Option<SystemTime>>>,
    work: Mutex<()>,
    sync_notify: Arc<Notify>,
}

impl Runtime {
    #[cfg(test)]
    pub(crate) fn test_runtime_for_registry(path: PathBuf, origin: String) -> Self {
        let vault_id = VaultId(Uuid::new_v4());
        let device_id = DeviceId(Uuid::new_v4());
        let client = Client::open(
            ClientConfig {
                database_path: path.join("vault.sqlite"),
                vault_id,
                device_id,
            },
            DatabaseKey::new(&[7; 32]).unwrap(),
        )
        .unwrap();
        let profile = peppy_crypto::KeyProfile::new(vault_id.0, 1).unwrap();
        let phrase = "correct horse battery staple";
        let root = peppy_crypto::derive_root_key(phrase, &profile).unwrap();
        let header = peppy_crypto::create_vault_check_header(&root, profile.clone()).unwrap();
        client.unlock(&profile, &header, phrase).unwrap();
        Self {
            state_dir: path,
            transport: peppy_gateway_simulator::private_http_transport(&origin).unwrap(),
            client,
            vault_id,
            device_id,
            credential: Zeroizing::new("test-device-token".into()),
            state: Arc::new(Mutex::new(RuntimeState::Offline)),
            last_synced_at: Arc::new(Mutex::new(None)),
            work: Mutex::new(()),
            sync_notify: Arc::new(Notify::new()),
        }
    }
    /// Deletes only artifacts owned by this vault state directory. It is usable
    /// for a tombstone whose sealed record can no longer be opened.
    pub fn cleanup_state_dir(state_dir: &std::path::Path) -> Result<(), RuntimeError> {
        remove_if_present(&state_dir.join("vault.sqlite"))
            .map_err(|_| RuntimeError::Custody(CustodyError::Invalid))?;
        remove_if_present(&state_dir.join("vault.sqlite-shm"))
            .map_err(|_| RuntimeError::Custody(CustodyError::Invalid))?;
        remove_if_present(&state_dir.join("vault.sqlite-wal"))
            .map_err(|_| RuntimeError::Custody(CustodyError::Invalid))?;
        remove_dir_if_present(&state_dir.join("vault.sqlite.media"))
            .map_err(|_| RuntimeError::Custody(CustodyError::Invalid))?;
        remove_dir_if_present(&state_dir.join("SIMULATED-downloads"))
            .map_err(|_| RuntimeError::Custody(CustodyError::Invalid))?;
        remove_owned_scratch(state_dir)
            .map_err(|_| RuntimeError::Custody(CustodyError::Invalid))?;
        remove_dir_if_present(&state_dir.join("custody"))
            .map_err(|_| RuntimeError::Custody(CustodyError::Invalid))?;
        Ok(())
    }
    /// Opens only a complete already-approved binding. Any absent, altered, or
    /// incompatible component leaves the vault in `NeedsApproval`; it never
    /// allocates a replacement device, database key, or cache.
    pub fn open_existing(
        state_dir: PathBuf,
        key_file: PathBuf,
        api_url: String,
        expected_vault: VaultId,
        expected_device: DeviceId,
    ) -> Result<Self, RuntimeError> {
        let custody = Custody::open(state_dir.join("custody"), key_file)?;
        let aad = runtime_aad(expected_vault, expected_device);
        let Some(bytes) = custody.load(RECORD_NAME, &aad)? else {
            return Err(RuntimeError::NeedsApproval);
        };
        let mut stored: StoredRuntime =
            serde_json::from_slice(&bytes).map_err(|_| RuntimeError::NeedsApproval)?;
        if stored.vault_id != expected_vault.0
            || stored.device_id != expected_device.0
            || stored.database_key.len() != 32
            || stored.device_credential.is_empty()
            || stored.key_cache.is_empty()
        {
            return Err(RuntimeError::NeedsApproval);
        }
        let client = Client::open(
            ClientConfig {
                database_path: state_dir.join("vault.sqlite"),
                vault_id: expected_vault,
                device_id: expected_device,
            },
            DatabaseKey::new(&stored.database_key).map_err(|_| RuntimeError::NeedsApproval)?,
        )
        .map_err(|_| RuntimeError::NeedsApproval)?;
        client
            .import_native_key_cache(&NativeKeyCache::from_native_storage(std::mem::take(
                &mut stored.key_cache,
            )))
            .map_err(|_| RuntimeError::NeedsApproval)?;
        Ok(Self {
            state_dir,
            transport: peppy_gateway_simulator::private_http_transport(&api_url)
                .map_err(|_| RuntimeError::Sync)?,
            client,
            vault_id: expected_vault,
            device_id: expected_device,
            credential: Zeroizing::new(std::mem::take(&mut stored.device_credential)),
            state: Arc::new(Mutex::new(RuntimeState::Offline)),
            last_synced_at: Arc::new(Mutex::new(None)),
            work: Mutex::new(()),
            sync_notify: Arc::new(Notify::new()),
        })
    }

    /// Durably writes all restart material as one encrypted custody record only
    /// after client-core has installed the verified handoff and exported its
    /// bound purpose-key cache.
    pub fn persist_approved(
        state_dir: PathBuf,
        key_file: PathBuf,
        vault_id: VaultId,
        device_id: DeviceId,
        database_key: [u8; 32],
        device_credential: String,
        key_cache: NativeKeyCache,
    ) -> Result<(), RuntimeError> {
        if device_credential.is_empty() {
            return Err(RuntimeError::NeedsApproval);
        }
        let custody = Custody::open(state_dir.join("custody"), key_file)?;
        let record = StoredRuntime {
            vault_id: vault_id.0,
            device_id: device_id.0,
            database_key: database_key.to_vec(),
            device_credential,
            key_cache: key_cache.native_storage_bytes().to_vec(),
        };
        let bytes = serde_json::to_vec(&record).map_err(|_| RuntimeError::Core)?;
        custody.store(RECORD_NAME, &runtime_aad(vault_id, device_id), &bytes)?;
        Ok(())
    }

    pub fn client(&self) -> &Client {
        &self.client
    }
    pub(crate) fn state_dir(&self) -> &std::path::Path {
        &self.state_dir
    }
    pub(crate) async fn work_guard(&self) -> tokio::sync::MutexGuard<'_, ()> {
        self.work.lock().await
    }
    pub fn request_sync(&self) {
        self.sync_notify.notify_one();
    }
    pub(crate) async fn sync_requested(&self) {
        self.sync_notify.notified().await;
    }

    /// Uses the device bearer against the ordinary roster endpoint. This is a
    /// live fence, not an administrator or worker-control credential bypass.
    pub async fn ensure_live(&self) -> Result<(), RuntimeError> {
        self.ensure_device_live().await?;
        self.ensure_current_key_profile().await
    }

    async fn ensure_device_live(&self) -> Result<(), RuntimeError> {
        let response = self
            .transport
            .client()
            .get(format!("{}/v1/devices", self.transport.origin()))
            .bearer_auth(self.credential.as_str())
            .send()
            .await
            .map_err(|_| RuntimeError::Sync)?;
        if matches!(response.status().as_u16(), 401 | 403) {
            *self.state.lock().await = RuntimeState::Revoked;
            return Err(RuntimeError::NotLive);
        }
        if !response.status().is_success() {
            self.mark_offline().await;
            return Err(RuntimeError::Sync);
        }
        let roster: serde_json::Value = response.json().await.map_err(|_| RuntimeError::Sync)?;
        let device_is_live = roster
            .get("devices")
            .and_then(serde_json::Value::as_array)
            .and_then(|devices| {
                devices.iter().find(|device| {
                    device.get("device_id").and_then(serde_json::Value::as_str)
                        == Some(&self.device_id.0.to_string())
                })
            })
            .is_some_and(|device| {
                !device
                    .get("revoked")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(true)
            });
        if device_is_live {
            Ok(())
        } else {
            *self.state.lock().await = RuntimeState::Revoked;
            Err(RuntimeError::NotLive)
        }
    }

    async fn ensure_current_key_profile(&self) -> Result<(), RuntimeError> {
        let response = self
            .transport
            .client()
            .get(format!("{}/v1/vault", self.transport.origin()))
            .bearer_auth(self.credential.as_str())
            .send()
            .await
            .map_err(|_| RuntimeError::Sync)?;
        if matches!(response.status().as_u16(), 401 | 403) {
            *self.state.lock().await = RuntimeState::Revoked;
            return Err(RuntimeError::NotLive);
        }
        if !response.status().is_success() {
            self.mark_offline().await;
            return Err(RuntimeError::Sync);
        }
        let header: serde_json::Value = response.json().await.map_err(|_| RuntimeError::Sync)?;
        let profile = self
            .client
            .active_key_profile()
            .map_err(|_| RuntimeError::NeedsApproval)?;
        let vault_id = self.vault_id.0.to_string();
        let device_id = self.device_id.0.to_string();
        let fingerprint = profile
            .fingerprint()
            .map_err(|_| RuntimeError::NeedsApproval)?;
        let matches = header.get("vault_id").and_then(serde_json::Value::as_str)
            == Some(vault_id.as_str())
            && header.get("device_id").and_then(serde_json::Value::as_str)
                == Some(device_id.as_str())
            && header.get("key_epoch").and_then(serde_json::Value::as_u64)
                == Some(profile.key_epoch as u64)
            && header
                .get("profile_fingerprint")
                .and_then(serde_json::Value::as_str)
                == Some(fingerprint.as_str());
        if matches {
            Ok(())
        } else {
            *self.state.lock().await = RuntimeState::NeedsApproval;
            Err(RuntimeError::NeedsApproval)
        }
    }

    /// Gets only server-declared gateway routes for this authenticated device.
    /// The active roster identity is validated before capability data is used.
    pub async fn gateway_views(&self) -> Result<Vec<peppy_desktop_api::GatewayView>, RuntimeError> {
        let http = self.transport.client();
        let devices = http
            .get(format!("{}/v1/devices", self.transport.origin()))
            .bearer_auth(self.credential.as_str())
            .send()
            .await
            .map_err(|_| RuntimeError::Sync)?
            .error_for_status()
            .map_err(|_| RuntimeError::Sync)?
            .json::<peppy_desktop_api::gateways::DevicesResponse>()
            .await
            .map_err(|_| RuntimeError::Sync)?;
        if peppy_desktop_api::gateways::extract_device_role(&devices, &self.device_id.0.to_string())
            .is_none()
        {
            *self.state.lock().await = RuntimeState::Revoked;
            return Err(RuntimeError::NotLive);
        }
        let capabilities = http
            .get(format!("{}/v1/capabilities", self.transport.origin()))
            .bearer_auth(self.credential.as_str())
            .send()
            .await
            .map_err(|_| RuntimeError::Sync)?
            .error_for_status()
            .map_err(|_| RuntimeError::Sync)?
            .json::<peppy_desktop_api::gateways::CapabilitiesResponse>()
            .await
            .map_err(|_| RuntimeError::Sync)?;
        Ok(peppy_desktop_api::gateways::gateway_views(
            &devices,
            &capabilities,
        ))
    }
    pub fn vault_id(&self) -> VaultId {
        self.vault_id
    }
    pub fn device_id(&self) -> DeviceId {
        self.device_id
    }
    pub async fn state(&self) -> RuntimeState {
        *self.state.lock().await
    }
    pub async fn last_synced_at(&self) -> Option<SystemTime> {
        *self.last_synced_at.lock().await
    }

    async fn mark_offline(&self) {
        let mut state = self.state.lock().await;
        if !matches!(*state, RuntimeState::NeedsApproval | RuntimeState::Revoked) {
            *state = RuntimeState::Offline;
        }
    }

    /// Runs replay, retained-history snapshot recovery, and byte-identical
    /// outbox publishing through the existing gateway host loop.
    pub async fn sync_once(&self) -> Result<usize, RuntimeError> {
        let _work = self.work.lock().await;
        match self.state().await {
            RuntimeState::Revoked => return Err(RuntimeError::NotLive),
            RuntimeState::NeedsApproval => match self.ensure_device_live().await {
                Ok(()) | Err(RuntimeError::Sync) => return Err(RuntimeError::NeedsApproval),
                Err(error) => return Err(error),
            },
            RuntimeState::Offline | RuntimeState::Online => {}
        }
        if let Err(error) = self.ensure_live().await {
            self.mark_offline().await;
            return Err(error);
        }
        let applied = match peppy_gateway_simulator::sync_http_with_transport(
            &self.client,
            &self.transport,
            &self.credential,
        )
        .await
        {
            Ok(applied) => applied,
            Err(_) => {
                self.mark_offline().await;
                return Err(RuntimeError::Sync);
            }
        };
        if peppy_gateway_simulator::download_media_with_transport(
            &self.client,
            &self.transport,
            &self.credential,
            &self.state_dir,
        )
        .await
        .is_err()
        {
            self.mark_offline().await;
            return Err(RuntimeError::Sync);
        }
        if let Err(error) = peppy_gateway_simulator::upload_media_with_transport(
            &self.client,
            &self.transport,
            &self.credential,
        )
        .await
            && !matches!(error, peppy_gateway_simulator::SimulatorError::Rejected(_))
        {
            self.mark_offline().await;
            return Err(RuntimeError::Sync);
        }
        if let Err(error) = peppy_gateway_simulator::upload_pending_with_transport(
            &self.client,
            &self.transport,
            &self.credential,
        )
        .await
        {
            match error {
                peppy_gateway_simulator::SimulatorError::Rejected(
                    peppy_gateway_simulator::RejectedEndpoint::Outbox,
                ) => {
                    self.mark_offline().await;
                    return Err(RuntimeError::OutboxRejected);
                }
                _ => {
                    self.mark_offline().await;
                    return Err(RuntimeError::Sync);
                }
            }
        }
        *self.state.lock().await = RuntimeState::Online;
        *self.last_synced_at.lock().await = Some(SystemTime::now());
        Ok(applied)
    }

    /// Server fencing precedes this call. The durable record is removed only
    /// after that fence; failure to remove is surfaced instead of reopening.
    pub async fn revoke(&self) -> Result<(), RuntimeError> {
        let _work = self.work.lock().await;
        *self.state.lock().await = RuntimeState::Revoked;
        Self::cleanup_state_dir(&self.state_dir)
    }
}

fn remove_if_present(path: &std::path::Path) -> std::io::Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn remove_dir_if_present(path: &std::path::Path) -> std::io::Result<()> {
    match std::fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn remove_owned_scratch(state_dir: &std::path::Path) -> std::io::Result<()> {
    let entries = match std::fs::read_dir(state_dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    for entry in entries {
        let entry = entry?;
        if entry
            .file_name()
            .to_string_lossy()
            .starts_with("mcp-image-")
        {
            if entry.file_type()?.is_dir() {
                std::fs::remove_dir_all(entry.path())?;
            } else {
                std::fs::remove_file(entry.path())?;
            }
        }
    }
    Ok(())
}

fn runtime_aad(vault_id: VaultId, device_id: DeviceId) -> Vec<u8> {
    format!("peppy-mcp-runtime-v1:{}:{}", vault_id.0, device_id.0).into_bytes()
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::PathBuf,
        sync::{Arc as StdArc, Mutex as StdMutex},
    };

    use super::*;
    use axum::{
        Json, Router,
        body::Bytes,
        extract::State,
        http::StatusCode,
        response::{IntoResponse, Response},
        routing::{get, post, put},
    };
    use peppy_client_core::{IncomingMms, KeyProfile, VaultCheckHeader};

    #[derive(Clone, Copy)]
    enum EventResponse {
        Accept,
        PolicyRejected,
        BodilessRejected,
    }

    struct SyncFixture {
        devices_status: StatusCode,
        device_id: String,
        vault: serde_json::Value,
        events: Vec<serde_json::Value>,
        ciphertext: Vec<u8>,
        event_response: EventResponse,
        reject_media: bool,
        transfers: usize,
    }

    type SharedFixture = StdArc<StdMutex<SyncFixture>>;

    fn policy_rejection() -> Response {
        (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({"code":"operation_not_permitted"})),
        )
            .into_response()
    }

    async fn fixture_devices(State(fixture): State<SharedFixture>) -> Response {
        let fixture = fixture.lock().unwrap();
        if !fixture.devices_status.is_success() {
            return fixture.devices_status.into_response();
        }
        Json(serde_json::json!({"devices":[{"device_id":fixture.device_id,"revoked":false}]}))
            .into_response()
    }

    async fn fixture_vault(State(fixture): State<SharedFixture>) -> Json<serde_json::Value> {
        Json(fixture.lock().unwrap().vault.clone())
    }

    async fn fixture_events_get(State(fixture): State<SharedFixture>) -> Json<serde_json::Value> {
        let mut fixture = fixture.lock().unwrap();
        fixture.transfers += 1;
        Json(
            serde_json::json!({"events":fixture.events.iter().enumerate().map(|(index, envelope)| serde_json::json!({"cursor":(index + 1).to_string(),"envelope":envelope})).collect::<Vec<_>>() }),
        )
    }

    async fn fixture_events_post(
        State(fixture): State<SharedFixture>,
        Json(envelope): Json<serde_json::Value>,
    ) -> Response {
        let mut fixture = fixture.lock().unwrap();
        fixture.transfers += 1;
        match fixture.event_response {
            EventResponse::Accept => {
                fixture.events.push(envelope);
                Json(serde_json::json!({"cursor":fixture.events.len().to_string(),"duplicate":false})).into_response()
            }
            EventResponse::PolicyRejected => policy_rejection(),
            EventResponse::BodilessRejected => StatusCode::FORBIDDEN.into_response(),
        }
    }

    async fn fixture_reserve(State(fixture): State<SharedFixture>) -> Response {
        let mut fixture = fixture.lock().unwrap();
        fixture.transfers += 1;
        if fixture.reject_media {
            return policy_rejection();
        }
        Json(serde_json::json!({"attachment_id":"6f1d2c3b-4a59-4e7f-8a1b-2c3d4e5f6a7b"}))
            .into_response()
    }

    async fn fixture_upload(State(fixture): State<SharedFixture>, body: Bytes) -> StatusCode {
        let mut fixture = fixture.lock().unwrap();
        fixture.transfers += 1;
        fixture.ciphertext = body.to_vec();
        StatusCode::NO_CONTENT
    }

    async fn fixture_finalize(State(fixture): State<SharedFixture>) -> StatusCode {
        fixture.lock().unwrap().transfers += 1;
        StatusCode::OK
    }

    async fn fixture_download(State(fixture): State<SharedFixture>) -> (StatusCode, Vec<u8>) {
        let mut fixture = fixture.lock().unwrap();
        fixture.transfers += 1;
        (StatusCode::OK, fixture.ciphertext.clone())
    }

    fn test_runtime(path: &std::path::Path, origin: String) -> Runtime {
        let vault_id = VaultId(Uuid::new_v4());
        let device_id = DeviceId(Uuid::new_v4());
        let client = Client::open(
            ClientConfig {
                database_path: path.join("vault.sqlite"),
                vault_id,
                device_id,
            },
            DatabaseKey::new(&[7; 32]).unwrap(),
        )
        .unwrap();
        let profile = peppy_crypto::KeyProfile::new(vault_id.0, 1).unwrap();
        let phrase = "correct horse battery staple";
        let root = peppy_crypto::derive_root_key(phrase, &profile).unwrap();
        let header = peppy_crypto::create_vault_check_header(&root, profile.clone()).unwrap();
        client.unlock(&profile, &header, phrase).unwrap();
        Runtime {
            state_dir: path.to_owned(),
            transport: peppy_gateway_simulator::private_http_transport(&origin).unwrap(),
            client,
            vault_id,
            device_id,
            credential: Zeroizing::new("test-device-token".into()),
            state: Arc::new(Mutex::new(RuntimeState::Offline)),
            last_synced_at: Arc::new(Mutex::new(None)),
            work: Mutex::new(()),
            sync_notify: Arc::new(Notify::new()),
        }
    }

    async fn sync_fixture(
        path: &std::path::Path,
    ) -> (Runtime, SharedFixture, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let runtime = test_runtime(path, format!("http://{}", listener.local_addr().unwrap()));
        let profile = runtime.client.active_key_profile().unwrap();
        let fixture = StdArc::new(StdMutex::new(SyncFixture {
            devices_status: StatusCode::OK,
            device_id: runtime.device_id.0.to_string(),
            vault: serde_json::json!({
                "vault_id": runtime.vault_id.0,
                "device_id": runtime.device_id.0,
                "key_epoch": profile.key_epoch,
                "profile_fingerprint": profile.fingerprint().unwrap(),
            }),
            events: Vec::new(),
            ciphertext: Vec::new(),
            event_response: EventResponse::Accept,
            reject_media: false,
            transfers: 0,
        }));
        let router = Router::new()
            .route("/v1/devices", get(fixture_devices))
            .route("/v1/vault", get(fixture_vault))
            .route(
                "/v1/events",
                get(fixture_events_get).post(fixture_events_post),
            )
            .route("/v1/attachments/reserve", post(fixture_reserve))
            .route("/v1/attachments/{id}/upload", put(fixture_upload))
            .route("/v1/attachments/{id}/finalize", post(fixture_finalize))
            .route("/v1/attachments/{id}", get(fixture_download))
            .with_state(fixture.clone());
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        (runtime, fixture, server)
    }

    fn fixture_gateway(path: &std::path::Path, runtime: &Runtime) -> Client {
        let client = Client::open(
            ClientConfig {
                database_path: path.join("gateway.sqlite"),
                vault_id: runtime.vault_id,
                device_id: DeviceId::new(),
            },
            DatabaseKey::new(&[8; 32]).unwrap(),
        )
        .unwrap();
        let profile = KeyProfile::new(runtime.vault_id.0, 1).unwrap();
        let phrase = "correct horse battery staple";
        let root = peppy_crypto::derive_root_key(phrase, &profile).unwrap();
        let header: VaultCheckHeader =
            peppy_crypto::create_vault_check_header(&root, profile.clone()).unwrap();
        client.unlock(&profile, &header, phrase).unwrap();
        client
    }

    fn queue_outbox(client: &Client, provider_message_id: &str) {
        client
            .capture_incoming_mms(IncomingMms {
                conversation_id: None,
                sender_address: "+15555550100".into(),
                body: "fixture outbox".into(),
                recipients: Vec::new(),
                subject: None,
                provider_message_id: Some(provider_message_id.into()),
                imported: false,
                attachment_ids: Vec::new(),
            })
            .unwrap();
    }

    #[tokio::test]
    async fn sync_requests_coalesce_to_one_follow_up_permit() {
        let temp = tempfile::tempdir().unwrap();
        let runtime = test_runtime(temp.path(), "http://127.0.0.1:1".into());
        runtime.request_sync();
        runtime.request_sync();

        runtime.sync_requested().await;
        assert!(
            tokio::time::timeout(
                std::time::Duration::from_millis(10),
                runtime.sync_requested()
            )
            .await
            .is_err(),
            "multiple requests must collapse to one follow-up pass"
        );
    }

    #[tokio::test]
    async fn sync_downloads_before_a_coded_outbox_rejection() {
        let temp = tempfile::tempdir().unwrap();
        let (runtime, fixture, server) = sync_fixture(temp.path()).await;
        let gateway = fixture_gateway(temp.path(), &runtime);
        let png = fs::read(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../tests/fixtures/media/valid-1x1.png"),
        )
        .unwrap();
        let source = temp.path().join("incoming.png");
        fs::write(&source, &png).unwrap();
        let attachment = gateway
            .prepare_attachment(&source, "image/png", "incoming.png")
            .unwrap();
        gateway
            .capture_incoming_mms(IncomingMms {
                conversation_id: None,
                sender_address: "+15555550101".into(),
                body: "fixture image".into(),
                recipients: Vec::new(),
                subject: None,
                provider_message_id: Some("fixture-image".into()),
                imported: false,
                attachment_ids: vec![attachment.attachment_id],
            })
            .unwrap();
        peppy_gateway_simulator::upload_media_with_transport(
            &gateway,
            &runtime.transport,
            &runtime.credential,
        )
        .await
        .unwrap();
        peppy_gateway_simulator::upload_pending_with_transport(
            &gateway,
            &runtime.transport,
            &runtime.credential,
        )
        .await
        .unwrap();
        queue_outbox(&runtime.client, "fixture-local");
        fixture.lock().unwrap().event_response = EventResponse::PolicyRejected;

        assert!(matches!(
            runtime.sync_once().await,
            Err(RuntimeError::OutboxRejected)
        ));
        assert_eq!(runtime.state().await, RuntimeState::Offline);
        assert!(runtime.client.pending_downloads().unwrap().is_empty());
        assert!(
            fs::read_dir(runtime.state_dir.join("vault.sqlite.media"))
                .unwrap()
                .next()
                .is_some(),
            "the replayed image was not installed"
        );
        assert!(!runtime.client.pending_outbox_batch(100).unwrap().is_empty());
        server.abort();
    }

    #[tokio::test]
    async fn sync_keeps_online_after_a_coded_attachment_rejection() {
        let temp = tempfile::tempdir().unwrap();
        let (runtime, fixture, server) = sync_fixture(temp.path()).await;
        queue_outbox(&runtime.client, "fixture-independent-outbox");
        let source = temp.path().join("outgoing.png");
        fs::write(
            &source,
            fs::read(
                PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("../../tests/fixtures/media/valid-1x1.png"),
            )
            .unwrap(),
        )
        .unwrap();
        let attachment = runtime
            .client
            .prepare_attachment(&source, "image/png", "outgoing.png")
            .unwrap();
        runtime
            .client
            .capture_incoming_mms(IncomingMms {
                conversation_id: None,
                sender_address: "+15555550102".into(),
                body: "fixture attachment".into(),
                recipients: Vec::new(),
                subject: None,
                provider_message_id: Some("fixture-attachment".into()),
                imported: false,
                attachment_ids: vec![attachment.attachment_id],
            })
            .unwrap();
        fixture.lock().unwrap().reject_media = true;

        runtime.sync_once().await.unwrap();
        assert_eq!(runtime.state().await, RuntimeState::Online);
        assert_eq!(fixture.lock().unwrap().events.len(), 1);
        server.abort();
    }

    #[tokio::test]
    async fn sync_uploads_an_attachment_and_its_dependent_envelope_in_one_pass() {
        let temp = tempfile::tempdir().unwrap();
        let (runtime, fixture, server) = sync_fixture(temp.path()).await;
        let source = temp.path().join("queued-image.png");
        fs::write(
            &source,
            fs::read(
                PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("../../tests/fixtures/media/valid-1x1.png"),
            )
            .unwrap(),
        )
        .unwrap();
        let attachment = runtime
            .client
            .prepare_attachment(&source, "image/png", "queued-image.png")
            .unwrap();
        runtime
            .client
            .capture_incoming_mms(IncomingMms {
                conversation_id: None,
                sender_address: "+15555550103".into(),
                body: "queued image command".into(),
                recipients: Vec::new(),
                subject: None,
                provider_message_id: Some("fixture-dependent-image".into()),
                imported: false,
                attachment_ids: vec![attachment.attachment_id],
            })
            .unwrap();

        runtime.sync_once().await.unwrap();

        assert_eq!(runtime.state().await, RuntimeState::Online);
        assert_eq!(fixture.lock().unwrap().events.len(), 1);
        assert!(runtime.client.pending_uploads().unwrap().is_empty());
        assert!(runtime.client.pending_outbox_batch(100).unwrap().is_empty());
        server.abort();
    }

    #[tokio::test]
    async fn sync_marks_bodiless_event_rejections_offline() {
        let temp = tempfile::tempdir().unwrap();
        let (runtime, fixture, server) = sync_fixture(temp.path()).await;
        queue_outbox(&runtime.client, "fixture-bodiless");
        fixture.lock().unwrap().event_response = EventResponse::BodilessRejected;

        assert!(matches!(runtime.sync_once().await, Err(RuntimeError::Sync)));
        assert_eq!(runtime.state().await, RuntimeState::Offline);
        server.abort();
    }

    #[tokio::test]
    async fn liveness_rejection_precedes_all_transfers() {
        let temp = tempfile::tempdir().unwrap();
        let (runtime, fixture, server) = sync_fixture(temp.path()).await;
        queue_outbox(&runtime.client, "fixture-liveness");
        fixture.lock().unwrap().devices_status = StatusCode::FORBIDDEN;

        assert!(matches!(
            runtime.sync_once().await,
            Err(RuntimeError::NotLive)
        ));
        assert_eq!(runtime.state().await, RuntimeState::Revoked);
        assert_eq!(fixture.lock().unwrap().transfers, 0);
        server.abort();
    }

    #[tokio::test]
    async fn status_reports_exact_wire_states_and_recorded_sync_time() {
        let temp = tempfile::tempdir().unwrap();
        let runtime = test_runtime(temp.path(), "http://127.0.0.1:1".into());
        let timestamp = std::time::UNIX_EPOCH + std::time::Duration::from_millis(1_700_000_000_123);
        *runtime.last_synced_at.lock().await = Some(timestamp);
        assert_eq!(runtime.last_synced_at().await, Some(timestamp));
        for (state, wire) in [
            (RuntimeState::Offline, "offline"),
            (RuntimeState::Online, "online"),
            (RuntimeState::NeedsApproval, "needs_approval"),
            (RuntimeState::Revoked, "revoked"),
        ] {
            *runtime.state.lock().await = state;
            let status = crate::http::status_response(runtime.vault_id.0, Some(&runtime)).await;
            assert_eq!(status["state"], wire);
            assert_eq!(status["last_synced_at"], "2023-11-14T22:13:20.123Z");
        }
        let missing = crate::http::status_response(runtime.vault_id.0, None).await;
        assert_eq!(missing["state"], "needs_approval");
        assert!(missing["last_synced_at"].is_null());
    }

    #[tokio::test]
    async fn profile_mismatch_sync_preserves_needs_approval_and_revocation() {
        let temp = tempfile::tempdir().unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let runtime = test_runtime(
            temp.path(),
            format!("http://{}", listener.local_addr().unwrap()),
        );
        let device_id = runtime.device_id.0;
        let router = axum::Router::new()
            .route(
                "/v1/devices",
                axum::routing::get(move || async move {
                    axum::Json(
                        serde_json::json!({"devices":[{"device_id":device_id,"revoked":false}]}),
                    )
                }),
            )
            .route(
                "/v1/vault",
                axum::routing::get(|| async {
                    axum::Json(serde_json::json!({"profile_fingerprint":"mismatched"}))
                }),
            );
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        assert!(matches!(
            runtime.sync_once().await,
            Err(RuntimeError::NeedsApproval)
        ));
        assert_eq!(runtime.state().await, RuntimeState::NeedsApproval);
        server.abort();
        assert!(matches!(
            runtime.sync_once().await,
            Err(RuntimeError::NeedsApproval)
        ));
        assert_eq!(runtime.state().await, RuntimeState::NeedsApproval);
        *runtime.state.lock().await = RuntimeState::Revoked;
        assert!(runtime.sync_once().await.is_err());
        runtime.mark_offline().await;
        assert_eq!(runtime.state().await, RuntimeState::Revoked);
    }

    #[tokio::test]
    async fn revoked_before_sync_reports_terminal_not_live() {
        let temp = tempfile::tempdir().unwrap();
        let runtime = test_runtime(temp.path(), "http://127.0.0.1:1".into());
        *runtime.state.lock().await = RuntimeState::Revoked;

        assert!(matches!(
            runtime.sync_once().await,
            Err(RuntimeError::NotLive)
        ));
    }

    #[tokio::test]
    async fn needs_approval_still_detects_deleted_device_without_key_access() {
        let temp = tempfile::tempdir().unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let runtime = test_runtime(
            temp.path(),
            format!("http://{}", listener.local_addr().unwrap()),
        );
        *runtime.state.lock().await = RuntimeState::NeedsApproval;
        let router = axum::Router::new().route(
            "/v1/devices",
            axum::routing::get(|| async { axum::http::StatusCode::UNAUTHORIZED }),
        );
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });

        assert!(matches!(
            runtime.sync_once().await,
            Err(RuntimeError::NotLive)
        ));
        assert_eq!(runtime.state().await, RuntimeState::Revoked);
        server.abort();
    }

    #[test]
    fn cleanup_removes_sqlite_media_and_owned_scratch_only() {
        let state = tempfile::tempdir().unwrap();
        for file in [
            "vault.sqlite",
            "vault.sqlite-wal",
            "vault.sqlite-shm",
            "mcp-image-a",
        ] {
            fs::write(state.path().join(file), b"local").unwrap();
        }
        fs::create_dir(state.path().join("vault.sqlite.media")).unwrap();
        fs::create_dir(state.path().join("SIMULATED-downloads")).unwrap();
        fs::create_dir(state.path().join("custody")).unwrap();
        fs::write(state.path().join("unrelated"), b"preserve").unwrap();

        Runtime::cleanup_state_dir(state.path()).unwrap();

        assert!(!state.path().join("vault.sqlite").exists());
        assert!(!state.path().join("vault.sqlite-wal").exists());
        assert!(!state.path().join("vault.sqlite-shm").exists());
        assert!(!state.path().join("vault.sqlite.media").exists());
        assert!(!state.path().join("SIMULATED-downloads").exists());
        assert!(!state.path().join("mcp-image-a").exists());
        assert!(state.path().join("unrelated").exists());
    }

    #[test]
    fn cleanup_accepts_a_missing_state_directory() {
        let state = tempfile::tempdir().unwrap();

        Runtime::cleanup_state_dir(&state.path().join("missing")).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn cleanup_unlinks_a_symlinked_download_directory_without_following_it() {
        use std::os::unix::fs::{PermissionsExt, symlink};

        let state = tempfile::tempdir().unwrap();
        let external = tempfile::tempdir().unwrap();
        let external_part = external.path().join("x.part");
        fs::write(&external_part, b"preserve").unwrap();
        fs::set_permissions(external.path(), fs::Permissions::from_mode(0o755)).unwrap();
        let original_permissions = fs::metadata(external.path()).unwrap().permissions().mode();
        let download_dir = state.path().join("SIMULATED-downloads");
        symlink(external.path(), &download_dir).unwrap();

        Runtime::cleanup_state_dir(state.path()).unwrap();

        assert!(fs::symlink_metadata(&download_dir).is_err());
        assert_eq!(fs::read(&external_part).unwrap(), b"preserve");
        assert_eq!(
            fs::metadata(external.path()).unwrap().permissions().mode(),
            original_permissions
        );
    }
}
