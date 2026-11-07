use std::{
    collections::HashMap,
    fs::{self, File, OpenOptions},
    io,
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use peppy_domain::{DeviceId, VaultId};
use peppy_hosted_api::mcp::McpWorkerCompleteRequest;
use serde::{Deserialize, Serialize};
use tokio::{
    sync::{Mutex, watch},
    task::{JoinHandle, JoinSet},
};
use uuid::Uuid;

use crate::{
    config::WorkerConfig,
    custody::Custody,
    enrollment::{EnrollmentError, EnrollmentManager},
    runtime::{Runtime, RuntimeError},
};

const REGISTRY_RECORD: &str = "runtimes";
const REGISTRY_AAD: &[u8] = b"peppy-mcp-runtime-registry-v1";

/// Process-lifetime ownership of the complete persistent worker state. The
/// lock is acquired before local state is opened or the private listener binds.
pub struct StateLock {
    _file: File,
}

impl StateLock {
    pub fn acquire(state_dir: &Path) -> io::Result<Self> {
        fs::create_dir_all(state_dir)?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(state_dir.join("worker.lock"))?;
        // SAFETY: flock operates only on this valid, open file descriptor.
        let result = unsafe {
            libc::flock(
                std::os::fd::AsRawFd::as_raw_fd(&file),
                libc::LOCK_EX | libc::LOCK_NB,
            )
        };
        if result == 0 {
            Ok(Self { _file: file })
        } else {
            Err(io::Error::last_os_error())
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
struct RuntimeIdentity {
    vault_id: Uuid,
    device_id: Uuid,
    #[serde(default)]
    cleanup_pending: bool,
    #[serde(default)]
    revoked: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
struct RevokedFence {
    vault_id: Uuid,
    device_id: Uuid,
}

#[derive(Deserialize, Serialize)]
struct RegistryRecord {
    #[serde(default)]
    identities: Vec<RuntimeIdentity>,
    #[serde(default)]
    revoked_fences: Vec<RevokedFence>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum StoredRegistryRecord {
    Current(RegistryRecord),
    Legacy(Vec<RuntimeIdentity>),
}

/// The durable set of approved vault bindings. It owns independent runtime
/// tasks; an error in one vault never holds the registry or another vault.
pub struct RuntimeRegistry {
    context: RegistryContext,
    shutdown: watch::Sender<bool>,
}

#[derive(Clone)]
struct RegistryContext {
    state_dir: PathBuf,
    custody: Arc<Custody>,
    webhooks: Arc<crate::webhooks::engine::WebhookEngine>,
    identities: Arc<Mutex<HashMap<Uuid, RuntimeIdentity>>>,
    revoked_fences: Arc<Mutex<Vec<RevokedFence>>>,
    runtimes: Arc<Mutex<HashMap<Uuid, Arc<Runtime>>>>,
    tasks: Arc<Mutex<HashMap<Uuid, SyncTask>>>,
    cleanup_task: Arc<Mutex<Option<JoinHandle<()>>>>,
    mutation: Arc<Mutex<()>>,
    next_task_generation: Arc<AtomicU64>,
}

struct SyncTask {
    generation: u64,
    device_id: Uuid,
    stop: watch::Sender<bool>,
    handle: JoinHandle<()>,
}

impl RuntimeRegistry {
    pub fn open(config: WorkerConfig) -> Result<Self, RuntimeError> {
        crate::runtime::sweep_owned_scratch(&config.state_dir)
            .map_err(|_| RuntimeError::NeedsApproval)?;
        let custody_root = config.state_dir.join("registry-custody");
        let custody = match fs::symlink_metadata(&config.key_file) {
            Ok(_) => Custody::open_existing(custody_root, &config.key_file)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                if has_durable_state(&config.state_dir).map_err(|_| RuntimeError::NeedsApproval)? {
                    return Err(RuntimeError::NeedsApproval);
                }
                Custody::bootstrap(custody_root, &config.key_file)?
            }
            Err(error) => return Err(crate::custody::CustodyError::Io(error).into()),
        };
        let record = custody
            .load(REGISTRY_RECORD, REGISTRY_AAD)?
            .map(|record| serde_json::from_slice::<StoredRegistryRecord>(&record))
            .transpose()
            .map_err(|_| RuntimeError::NeedsApproval)?
            .map(|record| match record {
                StoredRegistryRecord::Current(record) => record,
                StoredRegistryRecord::Legacy(identities) => RegistryRecord {
                    identities,
                    revoked_fences: Vec::new(),
                },
            })
            .unwrap_or(RegistryRecord {
                identities: Vec::new(),
                revoked_fences: Vec::new(),
            });
        let mut runtimes = HashMap::new();
        let mut identity_map = HashMap::new();
        for identity in record.identities {
            identity_map.insert(identity.vault_id, identity);
            if identity.revoked {
                continue;
            }
            let vault_id = VaultId(identity.vault_id);
            let device_id = DeviceId(identity.device_id);
            let state_dir = vault_state_dir(&config.state_dir, identity.vault_id);
            match Runtime::open_existing(
                state_dir,
                config.key_file.clone(),
                config.api_url.clone(),
                vault_id,
                device_id,
            ) {
                Ok(runtime) => {
                    runtimes.insert(identity.vault_id, Arc::new(runtime));
                }
                Err(error) => log_failure(outcome_category(&error), identity.vault_id, &error),
            }
        }
        let (shutdown, _) = watch::channel(false);
        let custody = Arc::new(custody);
        let webhooks = Arc::new(crate::webhooks::engine::WebhookEngine::new(
            custody.clone(),
            config.clone(),
        ));
        Ok(Self {
            context: RegistryContext {
                state_dir: config.state_dir,
                custody,
                webhooks,
                identities: Arc::new(Mutex::new(identity_map)),
                revoked_fences: Arc::new(Mutex::new(record.revoked_fences)),
                runtimes: Arc::new(Mutex::new(runtimes)),
                tasks: Arc::new(Mutex::new(HashMap::new())),
                cleanup_task: Arc::new(Mutex::new(None)),
                mutation: Arc::new(Mutex::new(())),
                next_task_generation: Arc::new(AtomicU64::new(0)),
            },
            shutdown,
        })
    }

    pub async fn start_sync(&self) {
        self.retry_pending_cleanup().await;
        self.spawn_cleanup_retry().await;
        let runtimes = self
            .context
            .runtimes
            .lock()
            .await
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for runtime in runtimes {
            self.spawn_sync(runtime).await;
        }
    }

    async fn retry_pending_cleanup(&self) {
        let pending = self
            .context
            .identities
            .lock()
            .await
            .values()
            .filter(|identity| identity.cleanup_pending)
            .copied()
            .collect::<Vec<_>>();
        for identity in pending {
            if let Err(error) = self.revoke(identity.vault_id, identity.device_id).await {
                log_failure("cleanup", identity.vault_id, &error);
            }
        }
    }

    async fn spawn_cleanup_retry(&self) {
        let mut task = self.context.cleanup_task.lock().await;
        if task.is_some() || *self.shutdown.borrow() {
            return;
        }
        let context = self.context.clone();
        let mut shutdown = self.shutdown.subscribe();
        *task = Some(tokio::spawn(async move {
            let mut interval = tokio::time::interval(cleanup_retry_interval());
            interval.tick().await;
            loop {
                tokio::select! {
                    _ = shutdown.changed() => return,
                    _ = interval.tick() => retry_pending_cleanup(&context).await,
                }
            }
        }));
    }

    pub async fn get(&self, vault_id: Uuid) -> Option<Arc<Runtime>> {
        self.context.runtimes.lock().await.get(&vault_id).cloned()
    }

    pub fn webhooks(&self) -> Arc<crate::webhooks::engine::WebhookEngine> {
        self.context.webhooks.clone()
    }

    pub async fn insert(&self, runtime: Runtime) -> Result<(), RuntimeError> {
        let _mutation = self.context.mutation.lock().await;
        self.insert_locked(runtime).await
    }

    async fn insert_locked(&self, runtime: Runtime) -> Result<(), RuntimeError> {
        let identity = RuntimeIdentity {
            vault_id: runtime.vault_id().0,
            device_id: runtime.device_id().0,
            cleanup_pending: false,
            revoked: false,
        };
        if let Some(existing) = self
            .context
            .identities
            .lock()
            .await
            .get(&identity.vault_id)
            .copied()
        {
            if existing.cleanup_pending
                || (!existing.revoked && existing.device_id != identity.device_id)
                || (existing.device_id == identity.device_id
                    && (existing.cleanup_pending || existing.revoked))
            {
                return Err(RuntimeError::IdentityMismatch);
            }
            if self
                .context
                .runtimes
                .lock()
                .await
                .contains_key(&identity.vault_id)
            {
                return Ok(());
            }
        }
        let runtime = Arc::new(runtime);
        let mut identities = self.context.identities.lock().await;
        let mut next = identities.clone();
        next.insert(identity.vault_id, identity);
        persist_identities(
            &self.context.custody,
            &next,
            &self.context.revoked_fences.lock().await,
        )?;
        *identities = next;
        drop(identities);
        self.context
            .runtimes
            .lock()
            .await
            .insert(identity.vault_id, runtime.clone());
        self.spawn_sync(runtime).await;
        Ok(())
    }

    /// Serializes the synchronous on-disk installation through durable registry
    /// publication with revoke. A matching revoked fence is checked before any
    /// enrollment persistence can recreate local state.
    pub async fn complete(
        &self,
        enrollments: &mut EnrollmentManager,
        request: McpWorkerCompleteRequest,
    ) -> Result<(), EnrollmentError> {
        let _mutation = self.context.mutation.lock().await;
        let device_id = request.admission.device_id;
        if self
            .context
            .revoked_fences
            .lock()
            .await
            .iter()
            .any(|fence| fence.vault_id == request.vault_id && fence.device_id == device_id)
        {
            return Err(EnrollmentError::Invalid);
        }
        if let Some(identity) = self
            .context
            .identities
            .lock()
            .await
            .get(&request.vault_id)
            .copied()
            && identity.device_id != device_id
            && (!identity.revoked || identity.cleanup_pending)
        {
            return Err(EnrollmentError::Invalid);
        }
        let runtime = enrollments.complete(request)?;
        self.insert_locked(runtime).await?;
        Ok(())
    }

    /// Leaves the durable registration intact until every local-state artifact
    /// has been removed, so a failed cleanup is retried after restart.
    pub async fn revoke(&self, vault_id: Uuid, device_id: Uuid) -> Result<(), RuntimeError> {
        revoke_registered(&self.context, vault_id, device_id).await
    }

    pub async fn shutdown(&self) {
        // `send_replace` stores the flag even with no live receiver; `send`
        // would drop it, so a later `spawn_sync` could start a live loop.
        self.shutdown.send_replace(true);
        let tasks = self
            .context
            .tasks
            .lock()
            .await
            .drain()
            .map(|(_, task)| task)
            .collect::<Vec<_>>();
        for task in tasks {
            task.stop.send_replace(true);
            let _ = task.handle.await;
        }
        if let Some(task) = self.context.cleanup_task.lock().await.take() {
            task.abort();
            let _ = task.await;
        }
    }

    async fn spawn_sync(&self, runtime: Arc<Runtime>) {
        let mut shutdown = self.shutdown.subscribe();
        let vault_id = runtime.vault_id().0;
        let device_id = runtime.device_id().0;
        let context = self.context.clone();
        let webhooks = self.context.webhooks.clone();
        let mut tasks_guard = self.context.tasks.lock().await;
        // A receiver subscribed after `send(true)` never observes `changed()`,
        // so a post-shutdown spawn must be refused here rather than rely on it.
        if *shutdown.borrow() {
            return;
        }
        if let Some(existing) = tasks_guard.get(&vault_id)
            && existing.device_id == device_id
        {
            return;
        }
        if let Some(existing) = tasks_guard.remove(&vault_id) {
            existing.stop.send_replace(true);
            // A retiring loop can only remove its own generation, so it cannot
            // erase this replacement after its self-cleanup epilogue.
            tokio::spawn(async move {
                let _ = existing.handle.await;
            });
        }
        // `interval.tick()` below consumes the interval's immediate tick, so the
        // first pass comes from this coalesced wake instead of waiting 30 s.
        runtime.request_sync();
        let generation = self
            .context
            .next_task_generation
            .fetch_add(1, Ordering::Relaxed);
        let (stop, mut stop_requested) = watch::channel(false);
        let task = tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(30));
            interval.tick().await;
            loop {
                let stopping = *shutdown.borrow() || *stop_requested.borrow();
                if stopping {
                    break;
                }
                tokio::select! {
                    biased;
                    _ = shutdown.changed() => break,
                    _ = stop_requested.changed() => break,
                    _ = runtime.sync_requested() => {
                        if !supervise_sync(runtime.clone(), vault_id, &mut shutdown, &mut stop_requested).await {
                            if let Err(error) = revoke_registered_without_stopping(&context, vault_id, device_id).await {
                                log_failure("cleanup", vault_id, &error);
                            }
                            break;
                        }
                        run_webhooks(&webhooks, &runtime, vault_id).await;
                    }
                    _ = interval.tick() => {
                        if !supervise_sync(runtime.clone(), vault_id, &mut shutdown, &mut stop_requested).await {
                            if let Err(error) = revoke_registered_without_stopping(&context, vault_id, device_id).await {
                                log_failure("cleanup", vault_id, &error);
                            }
                            break;
                        }
                        run_webhooks(&webhooks, &runtime, vault_id).await;
                    }
                }
            }
            let mut tasks = context.tasks.lock().await;
            if tasks
                .get(&vault_id)
                .is_some_and(|task| task.generation == generation)
            {
                tasks.remove(&vault_id);
            }
        });
        tasks_guard.insert(
            vault_id,
            SyncTask {
                generation,
                device_id,
                stop,
                handle: task,
            },
        );
    }
}

async fn run_sync(runtime: &Runtime, vault_id: Uuid) -> bool {
    match runtime.sync_once_report().await {
        Ok(report) => {
            if report.replay_backlog {
                runtime.request_sync();
            }
        }
        Err(error) => {
            log_failure(outcome_category(&error), vault_id, &error);
            if matches!(error, RuntimeError::NotLive) {
                return false;
            }
        }
    }
    true
}

async fn supervise_sync(
    runtime: Arc<Runtime>,
    vault_id: Uuid,
    shutdown: &mut watch::Receiver<bool>,
    stop_requested: &mut watch::Receiver<bool>,
) -> bool {
    let pass_runtime = runtime.clone();
    let mut passes = JoinSet::new();
    passes.spawn(async move { run_sync(&pass_runtime, vault_id).await });
    let joined = tokio::select! {
        result = passes.join_next() => result,
        _ = shutdown.changed() => {
            passes.shutdown().await;
            return true;
        }
        _ = stop_requested.changed() => {
            passes.shutdown().await;
            return true;
        }
    };
    match joined {
        Some(Ok(keep_running)) => keep_running,
        Some(Err(error)) => {
            runtime.mark_offline().await;
            tracing::warn!(category = "sync", vault = %short_vault_id(vault_id), panic = error.is_panic(), "mcp runtime sync task failed");
            true
        }
        None => true,
    }
}

async fn retry_pending_cleanup(context: &RegistryContext) {
    let pending = context
        .identities
        .lock()
        .await
        .values()
        .filter(|identity| identity.cleanup_pending)
        .copied()
        .collect::<Vec<_>>();
    for identity in pending {
        if let Err(error) = revoke_registered(context, identity.vault_id, identity.device_id).await
        {
            log_failure("cleanup", identity.vault_id, &error);
        }
    }
}

async fn run_webhooks(
    engine: &crate::webhooks::engine::WebhookEngine,
    runtime: &Runtime,
    vault_id: Uuid,
) {
    let outcome = tokio::time::timeout(Duration::from_secs(60), async {
        engine.after_sync(runtime).await?;
        engine.dispatch_due(runtime).await
    })
    .await;
    if outcome.is_err() || outcome.ok().is_some_and(|result| result.is_err()) {
        tracing::warn!(category = "webhooks", vault = %short_vault_id(vault_id), "webhook processing failed");
    }
}

fn cleanup_retry_interval() -> Duration {
    #[cfg(test)]
    {
        Duration::from_millis(10)
    }
    #[cfg(not(test))]
    {
        Duration::from_secs(30)
    }
}

/// Logs a background outcome by category only: no tokens, keys, bodies, or
/// phone numbers, and the vault id is shortened. `RuntimeError` messages are
/// fixed strings that do not carry the wrapped custody or core detail.
fn log_failure(category: &'static str, vault_id: Uuid, error: &RuntimeError) {
    tracing::warn!(
        category,
        vault = %short_vault_id(vault_id),
        %error,
        "mcp runtime operation failed"
    );
}

fn outcome_category(error: &RuntimeError) -> &'static str {
    match error {
        RuntimeError::NeedsApproval => "needs_approval",
        RuntimeError::NotLive => "not_live",
        _ => "sync",
    }
}

fn short_vault_id(vault_id: Uuid) -> String {
    vault_id.to_string()[..8].to_owned()
}

async fn revoke_registered(
    context: &RegistryContext,
    vault_id: Uuid,
    device_id: Uuid,
) -> Result<(), RuntimeError> {
    // Only stop the exact binding that is being revoked. A delayed revoke for
    // an older device must never interrupt its replacement.
    if context
        .identities
        .lock()
        .await
        .get(&vault_id)
        .is_some_and(|identity| identity.device_id == device_id)
    {
        stop_task(&context.tasks, vault_id, device_id).await;
    }
    revoke_registered_inner(context, vault_id, device_id).await
}

async fn revoke_registered_without_stopping(
    context: &RegistryContext,
    vault_id: Uuid,
    device_id: Uuid,
) -> Result<(), RuntimeError> {
    revoke_registered_inner(context, vault_id, device_id).await
}

async fn revoke_registered_inner(
    context: &RegistryContext,
    vault_id: Uuid,
    device_id: Uuid,
) -> Result<(), RuntimeError> {
    let _mutation = context.mutation.lock().await;
    let identity = context.identities.lock().await.get(&vault_id).copied();
    let mut fences = context.revoked_fences.lock().await;
    let fence = RevokedFence {
        vault_id,
        device_id,
    };
    if !fences.contains(&fence) {
        fences.push(fence);
    }
    if identity.is_some_and(|identity| identity.device_id != device_id) {
        // This is a stale revoke for a replaced binding. Its exact denial is
        // durable, but the current device exclusively owns the vault state.
        let identities = context.identities.lock().await.clone();
        persist_identities(&context.custody, &identities, &fences)?;
        return Ok(());
    }
    let mut tombstone = identity.unwrap_or(RuntimeIdentity {
        vault_id,
        device_id,
        cleanup_pending: false,
        revoked: false,
    });
    tombstone.cleanup_pending = true;
    tombstone.revoked = true;
    let mut next = context.identities.lock().await.clone();
    next.insert(vault_id, tombstone);
    persist_identities(&context.custody, &next, &fences)?;
    *context.identities.lock().await = next;
    let runtime = context.runtimes.lock().await.remove(&vault_id);
    match runtime {
        Some(runtime) => runtime.revoke().await,
        None => Runtime::cleanup_state_dir(&vault_state_dir(&context.state_dir, vault_id)),
    }?;
    context
        .webhooks
        .remove_vault(vault_id, device_id)
        .await
        .map_err(|_| RuntimeError::Core)?;
    let mut next = context.identities.lock().await.clone();
    let mut revoked = tombstone;
    revoked.cleanup_pending = false;
    next.insert(vault_id, revoked);
    persist_identities(&context.custody, &next, &fences)?;
    *context.identities.lock().await = next;
    Ok(())
}

async fn stop_task(tasks: &Arc<Mutex<HashMap<Uuid, SyncTask>>>, vault_id: Uuid, device_id: Uuid) {
    let task = {
        let mut tasks = tasks.lock().await;
        if tasks
            .get(&vault_id)
            .is_some_and(|task| task.device_id == device_id)
        {
            tasks.remove(&vault_id)
        } else {
            None
        }
    };
    if let Some(task) = task {
        task.stop.send_replace(true);
        let _ = task.handle.await;
    }
}

fn persist_identities(
    custody: &Custody,
    identities: &HashMap<Uuid, RuntimeIdentity>,
    revoked_fences: &[RevokedFence],
) -> Result<(), RuntimeError> {
    let record = RegistryRecord {
        identities: identities.values().copied().collect(),
        revoked_fences: revoked_fences.to_vec(),
    };
    let bytes = serde_json::to_vec(&record).map_err(|_| RuntimeError::Core)?;
    custody.store(REGISTRY_RECORD, REGISTRY_AAD, &bytes)?;
    Ok(())
}

pub fn vault_state_dir(state_dir: &Path, vault_id: Uuid) -> PathBuf {
    state_dir.join("vaults").join(vault_id.to_string())
}

fn has_durable_state(state_dir: &Path) -> io::Result<bool> {
    if !state_dir.exists() {
        return Ok(false);
    }
    Ok(fs::read_dir(state_dir)?
        .flatten()
        .any(|entry| entry.file_name() != "worker.lock"))
}

#[cfg(test)]
mod tests {
    use std::{
        collections::HashMap,
        fs,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
        time::Duration,
    };

    use super::{
        RevokedFence, RuntimeIdentity, RuntimeRegistry, StateLock, outcome_category,
        persist_identities, short_vault_id, vault_state_dir,
    };
    use crate::{
        config::WorkerConfig,
        runtime::{Runtime, RuntimeError},
    };
    use axum::{Router, extract::State, http::StatusCode, routing::get};
    use tokio::sync::Notify;
    use uuid::Uuid;

    #[test]
    fn vault_ids_are_shortened_for_logs() {
        let vault_id = Uuid::parse_str("0123abcd-4567-89ef-0123-456789abcdef").unwrap();
        assert_eq!(short_vault_id(vault_id), "0123abcd");
    }

    #[test]
    fn background_outcomes_map_to_log_categories() {
        assert_eq!(
            outcome_category(&RuntimeError::NeedsApproval),
            "needs_approval"
        );
        assert_eq!(outcome_category(&RuntimeError::NotLive), "not_live");
        assert_eq!(outcome_category(&RuntimeError::Sync), "sync");
    }

    fn config(root: &tempfile::TempDir) -> WorkerConfig {
        WorkerConfig {
            api_url: "http://127.0.0.1:1".into(),
            public_api_url: "http://127.0.0.1:1".into(),
            state_dir: root.path().join("state"),
            key_file: root.path().join("key"),
            worker_token_file: root.path().join("token"),
            bind_addr: "127.0.0.1:0".parse().unwrap(),
            webhooks_enabled: false,
            webhooks_allow_private: false,
            app_origin: "http://127.0.0.1:1".into(),
        }
    }

    #[test]
    fn state_directory_allows_only_one_process_owner() {
        let directory = tempfile::tempdir().unwrap();
        let _first = StateLock::acquire(directory.path()).unwrap();
        assert!(StateLock::acquire(directory.path()).is_err());
    }

    #[test]
    fn opening_registry_sweeps_only_crash_left_vault_scratch() {
        let directory = tempfile::tempdir().unwrap();
        let config = config(&directory);
        drop(RuntimeRegistry::open(config.clone()).unwrap());
        let vault = vault_state_dir(&config.state_dir, Uuid::new_v4());
        fs::create_dir_all(&vault).unwrap();
        fs::write(vault.join("mcp-image-crash-left"), b"plaintext").unwrap();
        fs::write(vault.join("preserve"), b"state").unwrap();

        let registry = RuntimeRegistry::open(config).unwrap();

        assert!(!vault.join("mcp-image-crash-left").exists());
        assert!(vault.join("preserve").exists());
        drop(registry);
    }

    async fn counting_server() -> (String, Arc<AtomicUsize>, tokio::task::JoinHandle<()>) {
        async fn devices(State(count): State<Arc<AtomicUsize>>) -> StatusCode {
            count.fetch_add(1, Ordering::SeqCst);
            StatusCode::INTERNAL_SERVER_ERROR
        }

        let count = Arc::new(AtomicUsize::new(0));
        let server_count = count.clone();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new()
                    .route("/v1/devices", get(devices))
                    .with_state(server_count),
            )
            .await
            .unwrap();
        });
        (origin, count, server)
    }

    #[tokio::test]
    async fn panicked_sync_pass_marks_offline_and_the_loop_recovers() {
        let directory = tempfile::tempdir().unwrap();
        let (origin, count, server) = counting_server().await;
        let registry = RuntimeRegistry::open(config(&directory)).unwrap();
        let runtime = Runtime::test_runtime_for_registry(directory.path().join("runtime"), origin);
        let vault_id = runtime.vault_id().0;
        runtime.panic_next_sync();
        registry.insert(runtime).await.unwrap();
        let runtime = registry.get(vault_id).await.unwrap();

        tokio::time::timeout(Duration::from_secs(2), async {
            while runtime.sync_panic_pending() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("panic was not supervised");
        runtime.request_sync();
        wait_for_count(&count, 1).await;
        assert!(registry.context.tasks.lock().await.contains_key(&vault_id));
        registry.shutdown().await;
        server.abort();
    }

    async fn wait_for_count(count: &AtomicUsize, expected: usize) {
        tokio::time::timeout(Duration::from_secs(2), async {
            while count.load(Ordering::SeqCst) < expected {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("expected sync pass did not run");
    }

    #[tokio::test]
    async fn registered_runtime_wake_syncs_without_waiting_for_the_interval() {
        let directory = tempfile::tempdir().unwrap();
        let (origin, count, server) = counting_server().await;
        let registry = RuntimeRegistry::open(config(&directory)).unwrap();
        let runtime = Runtime::test_runtime_for_registry(directory.path().join("runtime"), origin);
        let vault_id = runtime.vault_id().0;
        registry.insert(runtime).await.unwrap();
        let runtime = registry.get(vault_id).await.unwrap();
        wait_for_count(&count, 1).await;

        runtime.request_sync();
        wait_for_count(&count, 2).await;
        registry.shutdown().await;
        server.abort();
    }

    #[tokio::test]
    async fn newly_registered_runtime_syncs_immediately_exactly_once() {
        let directory = tempfile::tempdir().unwrap();
        let (origin, count, server) = counting_server().await;
        let registry = RuntimeRegistry::open(config(&directory)).unwrap();
        let runtime = Runtime::test_runtime_for_registry(directory.path().join("runtime"), origin);
        registry.insert(runtime).await.unwrap();

        wait_for_count(&count, 1).await;
        tokio::time::sleep(Duration::from_secs(1)).await;
        assert_eq!(
            count.load(Ordering::SeqCst),
            1,
            "first pass must not double"
        );
        registry.shutdown().await;
        server.abort();
    }

    #[tokio::test]
    async fn shutdown_and_removal_cancel_pending_wakes_without_resurrection() {
        let directory = tempfile::tempdir().unwrap();
        let (origin, count, server) = counting_server().await;
        let registry = RuntimeRegistry::open(config(&directory)).unwrap();
        let runtime = Runtime::test_runtime_for_registry(directory.path().join("runtime"), origin);
        let vault_id = runtime.vault_id().0;
        let device_id = runtime.device_id().0;
        registry.insert(runtime).await.unwrap();
        let runtime = registry.get(vault_id).await.unwrap();
        let weak = Arc::downgrade(&runtime);
        wait_for_count(&count, 1).await;

        registry.revoke(vault_id, device_id).await.unwrap();
        let baseline = count.load(Ordering::SeqCst);
        runtime.request_sync();
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(count.load(Ordering::SeqCst), baseline);
        assert!(registry.context.tasks.lock().await.is_empty());
        drop(runtime);
        assert!(weak.upgrade().is_none());

        registry.shutdown().await;
        server.abort();
    }

    #[tokio::test]
    async fn shutdown_cancels_a_pending_wake_and_releases_the_runtime() {
        let directory = tempfile::tempdir().unwrap();
        let (origin, count, server) = counting_server().await;
        let registry = RuntimeRegistry::open(config(&directory)).unwrap();
        let runtime = Runtime::test_runtime_for_registry(directory.path().join("runtime"), origin);
        let vault_id = runtime.vault_id().0;
        registry.insert(runtime).await.unwrap();
        let runtime = registry.get(vault_id).await.unwrap();
        let weak = Arc::downgrade(&runtime);
        wait_for_count(&count, 1).await;

        runtime.request_sync();
        registry.shutdown().await;
        let baseline = count.load(Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(count.load(Ordering::SeqCst), baseline);
        assert!(registry.context.tasks.lock().await.is_empty());
        drop(runtime);
        drop(registry);
        assert!(weak.upgrade().is_none());
        server.abort();
    }

    #[tokio::test]
    async fn start_sync_after_shutdown_spawns_no_sync_loop() {
        let directory = tempfile::tempdir().unwrap();
        let (origin, count, server) = counting_server().await;
        let registry = RuntimeRegistry::open(config(&directory)).unwrap();
        let runtime = Runtime::test_runtime_for_registry(directory.path().join("runtime"), origin);
        let vault_id = runtime.vault_id().0;
        registry.insert(runtime).await.unwrap();
        wait_for_count(&count, 1).await;
        registry.shutdown().await;
        let baseline = count.load(Ordering::SeqCst);
        registry.start_sync().await;
        let runtime = registry.get(vault_id).await.unwrap();
        runtime.request_sync();

        tokio::time::sleep(Duration::from_millis(500)).await;
        assert_eq!(
            count.load(Ordering::SeqCst),
            baseline,
            "loop spawned after shutdown ran"
        );
        assert!(registry.context.tasks.lock().await.is_empty());
        registry.shutdown().await;
        server.abort();
    }

    #[tokio::test]
    async fn empty_registry_shutdown_before_insert_starts_no_loop() {
        let directory = tempfile::tempdir().unwrap();
        let (origin, count, server) = counting_server().await;
        let registry = RuntimeRegistry::open(config(&directory)).unwrap();
        registry.shutdown().await;
        let runtime = Runtime::test_runtime_for_registry(directory.path().join("runtime"), origin);
        let vault_id = runtime.vault_id().0;
        registry.insert(runtime).await.unwrap();
        let runtime = registry.get(vault_id).await.unwrap();
        runtime.request_sync();

        tokio::time::sleep(Duration::from_millis(500)).await;
        assert_eq!(
            count.load(Ordering::SeqCst),
            0,
            "loop started after shutdown"
        );
        assert!(registry.context.tasks.lock().await.is_empty());
        registry.shutdown().await;
        server.abort();
    }

    #[derive(Default)]
    struct Gate {
        hits: AtomicUsize,
        entered: Notify,
        release: Notify,
    }

    /// Holds only the first liveness request open, so a sync pass can be
    /// observed in flight without sleeping to guess when it has started.
    async fn gated_server() -> (String, Arc<Gate>, tokio::task::JoinHandle<()>) {
        async fn devices(State(gate): State<Arc<Gate>>) -> StatusCode {
            if gate.hits.fetch_add(1, Ordering::SeqCst) == 0 {
                gate.entered.notify_one();
                gate.release.notified().await;
            }
            StatusCode::INTERNAL_SERVER_ERROR
        }

        let gate = Arc::new(Gate::default());
        let server_gate = gate.clone();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new()
                    .route("/v1/devices", get(devices))
                    .with_state(server_gate),
            )
            .await
            .unwrap();
        });
        (origin, gate, server)
    }

    async fn wait_entered(gate: &Gate) {
        tokio::time::timeout(Duration::from_secs(2), gate.entered.notified())
            .await
            .expect("sync pass never reached the gated request");
    }

    async fn wait_for_hits(gate: &Gate, hits: usize) {
        tokio::time::timeout(Duration::from_secs(2), async {
            while gate.hits.load(Ordering::SeqCst) < hits {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("expected sync pass did not run");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn wakes_during_an_active_pass_yield_one_follow_up_then_quiesce() {
        let directory = tempfile::tempdir().unwrap();
        let (origin, gate, server) = gated_server().await;
        let registry = RuntimeRegistry::open(config(&directory)).unwrap();
        let runtime = Runtime::test_runtime_for_registry(directory.path().join("runtime"), origin);
        let vault_id = runtime.vault_id().0;
        registry.insert(runtime).await.unwrap();
        let runtime = registry.get(vault_id).await.unwrap();

        wait_entered(&gate).await;
        for _ in 0..3 {
            runtime.request_sync();
        }
        gate.release.notify_one();

        wait_for_hits(&gate, 2).await;
        tokio::time::sleep(Duration::from_secs(1)).await;
        assert_eq!(
            gate.hits.load(Ordering::SeqCst),
            2,
            "wakes during an active pass must yield exactly one follow-up"
        );
        registry.shutdown().await;
        server.abort();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn removal_during_an_active_pass_cancels_the_pending_wake() {
        let directory = tempfile::tempdir().unwrap();
        let (origin, gate, server) = gated_server().await;
        let registry = RuntimeRegistry::open(config(&directory)).unwrap();
        let runtime = Runtime::test_runtime_for_registry(directory.path().join("runtime"), origin);
        let vault_id = runtime.vault_id().0;
        let device_id = runtime.device_id().0;
        registry.insert(runtime).await.unwrap();
        let runtime = registry.get(vault_id).await.unwrap();
        let weak = Arc::downgrade(&runtime);

        wait_entered(&gate).await;
        runtime.request_sync();
        // The pass holds synchronous core work, so cooperative revocation must
        // join it rather than returning while that work is still live.
        gate.release.notify_one();
        registry.revoke(vault_id, device_id).await.unwrap();

        tokio::time::sleep(Duration::from_secs(1)).await;
        assert_eq!(gate.hits.load(Ordering::SeqCst), 1, "successor pass ran");
        assert!(registry.context.tasks.lock().await.is_empty());
        drop(runtime);
        assert!(weak.upgrade().is_none());
        server.abort();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn shutdown_during_an_active_pass_cancels_the_pending_wake() {
        let directory = tempfile::tempdir().unwrap();
        let (origin, gate, server) = gated_server().await;
        let registry = RuntimeRegistry::open(config(&directory)).unwrap();
        let runtime = Runtime::test_runtime_for_registry(directory.path().join("runtime"), origin);
        let vault_id = runtime.vault_id().0;
        registry.insert(runtime).await.unwrap();
        let runtime = registry.get(vault_id).await.unwrap();
        let weak = Arc::downgrade(&runtime);

        wait_entered(&gate).await;
        runtime.request_sync();
        registry.shutdown().await;
        gate.release.notify_one();

        tokio::time::sleep(Duration::from_secs(1)).await;
        assert_eq!(gate.hits.load(Ordering::SeqCst), 1, "successor pass ran");
        assert!(registry.context.tasks.lock().await.is_empty());
        drop(runtime);
        drop(registry);
        assert!(weak.upgrade().is_none());
        server.abort();
    }

    #[tokio::test]
    async fn partial_revocation_cleanup_keeps_a_durable_tombstone_for_retry() {
        let directory = tempfile::tempdir().unwrap();
        let registry = RuntimeRegistry::open(config(&directory)).unwrap();
        let vault_id = Uuid::new_v4();
        let device_id = Uuid::new_v4();
        let identity = RuntimeIdentity {
            vault_id,
            device_id,
            cleanup_pending: false,
            revoked: false,
        };
        let identities = HashMap::from([(vault_id, identity)]);
        persist_identities(&registry.context.custody, &identities, &[]).unwrap();
        *registry.context.identities.lock().await = identities;
        fs::create_dir_all(
            vault_state_dir(&registry.context.state_dir, vault_id).join("vault.sqlite"),
        )
        .unwrap();

        assert!(registry.revoke(vault_id, device_id).await.is_err());
        assert!(
            registry
                .context
                .identities
                .lock()
                .await
                .get(&vault_id)
                .unwrap()
                .cleanup_pending
        );

        fs::remove_dir_all(
            vault_state_dir(&registry.context.state_dir, vault_id).join("vault.sqlite"),
        )
        .unwrap();
        registry.start_sync().await;
        tokio::time::timeout(Duration::from_secs(2), async {
            while registry
                .context
                .identities
                .lock()
                .await
                .get(&vault_id)
                .is_none_or(|identity| identity.cleanup_pending)
            {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("cleanup retry did not clear the durable tombstone");
        assert!(
            registry
                .context
                .identities
                .lock()
                .await
                .get(&vault_id)
                .is_some_and(|identity| identity.revoked && !identity.cleanup_pending)
        );
    }

    #[tokio::test]
    async fn revoked_device_fences_survive_replacement_and_restart() {
        let directory = tempfile::tempdir().unwrap();
        let config = config(&directory);
        let registry = RuntimeRegistry::open(config.clone()).unwrap();
        let vault_id = Uuid::new_v4();
        let device_a = Uuid::new_v4();
        let device_b = Uuid::new_v4();

        registry.revoke(vault_id, device_a).await.unwrap();
        let binding_b = RuntimeIdentity {
            vault_id,
            device_id: device_b,
            cleanup_pending: false,
            revoked: false,
        };
        let identities = HashMap::from([(vault_id, binding_b)]);
        persist_identities(
            &registry.context.custody,
            &identities,
            &registry.context.revoked_fences.lock().await,
        )
        .unwrap();
        *registry.context.identities.lock().await = identities;

        // A delayed A revoke records its own fence but does not replace B.
        registry.revoke(vault_id, device_a).await.unwrap();
        assert_eq!(
            registry.context.identities.lock().await.get(&vault_id),
            Some(&binding_b)
        );
        registry.revoke(vault_id, device_b).await.unwrap();
        drop(registry);

        let reopened = RuntimeRegistry::open(config).unwrap();
        let fences = reopened.context.revoked_fences.lock().await;
        assert!(fences.contains(&RevokedFence {
            vault_id,
            device_id: device_a
        }));
        assert!(fences.contains(&RevokedFence {
            vault_id,
            device_id: device_b
        }));
    }
}
