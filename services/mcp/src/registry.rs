use std::{
    collections::HashMap,
    fs::{self, File, OpenOptions},
    io,
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use peppy_domain::{DeviceId, VaultId};
use serde::{Deserialize, Serialize};
use tokio::{
    sync::{Mutex, watch},
    task::JoinHandle,
};
use uuid::Uuid;

use crate::{
    config::WorkerConfig,
    custody::Custody,
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

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
struct RuntimeIdentity {
    vault_id: Uuid,
    device_id: Uuid,
    #[serde(default)]
    cleanup_pending: bool,
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
    identities: Arc<Mutex<HashMap<Uuid, RuntimeIdentity>>>,
    runtimes: Arc<Mutex<HashMap<Uuid, Arc<Runtime>>>>,
    tasks: Arc<Mutex<HashMap<Uuid, JoinHandle<()>>>>,
    mutation: Arc<Mutex<()>>,
}

impl RuntimeRegistry {
    pub fn open(config: WorkerConfig) -> Result<Self, RuntimeError> {
        if !config.key_file.exists()
            && has_durable_state(&config.state_dir).map_err(|_| RuntimeError::NeedsApproval)?
        {
            return Err(RuntimeError::NeedsApproval);
        }
        let custody = Custody::open(config.state_dir.join("registry-custody"), &config.key_file)?;
        let identities = custody
            .load(REGISTRY_RECORD, REGISTRY_AAD)?
            .map(|record| serde_json::from_slice::<Vec<RuntimeIdentity>>(&record))
            .transpose()
            .map_err(|_| RuntimeError::NeedsApproval)?
            .unwrap_or_default();
        let mut runtimes = HashMap::new();
        let mut identity_map = HashMap::new();
        for identity in identities {
            identity_map.insert(identity.vault_id, identity);
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
        Ok(Self {
            context: RegistryContext {
                state_dir: config.state_dir,
                custody: Arc::new(custody),
                identities: Arc::new(Mutex::new(identity_map)),
                runtimes: Arc::new(Mutex::new(runtimes)),
                tasks: Arc::new(Mutex::new(HashMap::new())),
                mutation: Arc::new(Mutex::new(())),
            },
            shutdown,
        })
    }

    pub async fn start_sync(&self) {
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

    pub async fn get(&self, vault_id: Uuid) -> Option<Arc<Runtime>> {
        self.context.runtimes.lock().await.get(&vault_id).cloned()
    }

    pub async fn insert(&self, runtime: Runtime) -> Result<(), RuntimeError> {
        let _mutation = self.context.mutation.lock().await;
        let identity = RuntimeIdentity {
            vault_id: runtime.vault_id().0,
            device_id: runtime.device_id().0,
            cleanup_pending: false,
        };
        if let Some(existing) = self
            .context
            .identities
            .lock()
            .await
            .get(&identity.vault_id)
            .copied()
        {
            if existing.device_id != identity.device_id || existing.cleanup_pending {
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
        persist_identities(&self.context.custody, &next)?;
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

    /// Leaves the durable registration intact until every local-state artifact
    /// has been removed, so a failed cleanup is retried after restart.
    pub async fn revoke(&self, vault_id: Uuid, device_id: Uuid) -> Result<(), RuntimeError> {
        revoke_registered(&self.context, vault_id, device_id).await
    }

    pub async fn shutdown(&self) {
        // `send_replace` stores the flag even with no live receiver; `send`
        // would drop it, so a later `spawn_sync` could start a live loop.
        self.shutdown.send_replace(true);
        for (_, task) in self.context.tasks.lock().await.drain() {
            task.abort();
            let _ = task.await;
        }
    }

    async fn spawn_sync(&self, runtime: Arc<Runtime>) {
        let mut shutdown = self.shutdown.subscribe();
        let vault_id = runtime.vault_id().0;
        let device_id = runtime.device_id().0;
        let context = self.context.clone();
        let mut tasks_guard = self.context.tasks.lock().await;
        // A receiver subscribed after `send(true)` never observes `changed()`,
        // so a post-shutdown spawn must be refused here rather than rely on it.
        if tasks_guard.contains_key(&vault_id) || *shutdown.borrow() {
            return;
        }
        // `interval.tick()` below consumes the interval's immediate tick, so the
        // first pass comes from this coalesced wake instead of waiting 30 s.
        runtime.request_sync();
        let task = tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(30));
            interval.tick().await;
            loop {
                let stopping = *shutdown.borrow();
                if stopping {
                    return;
                }
                tokio::select! {
                    biased;
                    _ = shutdown.changed() => return,
                    _ = runtime.sync_requested() => {
                        if !run_sync(&runtime, &context, vault_id, device_id).await {
                            return;
                        }
                    }
                    _ = interval.tick() => {
                        if !run_sync(&runtime, &context, vault_id, device_id).await {
                            return;
                        }
                    }
                }
            }
        });
        tasks_guard.insert(vault_id, task);
    }
}

async fn run_sync(
    runtime: &Runtime,
    context: &RegistryContext,
    vault_id: Uuid,
    device_id: Uuid,
) -> bool {
    if let Err(error) = runtime.sync_once().await {
        log_failure(outcome_category(&error), vault_id, &error);
        if matches!(error, RuntimeError::NotLive) {
            let context = context.clone();
            tokio::spawn(async move {
                if let Err(error) = revoke_registered(&context, vault_id, device_id).await {
                    log_failure("cleanup", vault_id, &error);
                }
            });
            return false;
        }
    }
    true
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
    let _mutation = context.mutation.lock().await;
    let identity = context.identities.lock().await.get(&vault_id).copied();
    let Some(identity) = identity else {
        return Runtime::cleanup_state_dir(&vault_state_dir(&context.state_dir, vault_id));
    };
    if identity.device_id != device_id {
        return Err(RuntimeError::IdentityMismatch);
    }
    let mut tombstone = identity;
    tombstone.cleanup_pending = true;
    let mut next = context.identities.lock().await.clone();
    next.insert(vault_id, tombstone);
    persist_identities(&context.custody, &next)?;
    *context.identities.lock().await = next;
    stop_task(&context.tasks, vault_id).await;
    let runtime = context.runtimes.lock().await.remove(&vault_id);
    match runtime {
        Some(runtime) => runtime.revoke().await,
        None => Runtime::cleanup_state_dir(&vault_state_dir(&context.state_dir, vault_id)),
    }?;
    let mut next = context.identities.lock().await.clone();
    next.remove(&vault_id);
    persist_identities(&context.custody, &next)?;
    *context.identities.lock().await = next;
    Ok(())
}

async fn stop_task(tasks: &Arc<Mutex<HashMap<Uuid, JoinHandle<()>>>>, vault_id: Uuid) {
    if let Some(task) = tasks.lock().await.remove(&vault_id) {
        task.abort();
        let _ = task.await;
    }
}

fn persist_identities(
    custody: &Custody,
    identities: &HashMap<Uuid, RuntimeIdentity>,
) -> Result<(), RuntimeError> {
    let identities = identities.values().copied().collect::<Vec<_>>();
    let bytes = serde_json::to_vec(&identities).map_err(|_| RuntimeError::Core)?;
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
        RuntimeIdentity, RuntimeRegistry, StateLock, outcome_category, persist_identities,
        short_vault_id, vault_state_dir,
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
        }
    }

    #[test]
    fn state_directory_allows_only_one_process_owner() {
        let directory = tempfile::tempdir().unwrap();
        let _first = StateLock::acquire(directory.path()).unwrap();
        assert!(StateLock::acquire(directory.path()).is_err());
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
        registry.revoke(vault_id, device_id).await.unwrap();
        gate.release.notify_one();

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
        };
        let identities = HashMap::from([(vault_id, identity)]);
        persist_identities(&registry.context.custody, &identities).unwrap();
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
        registry.revoke(vault_id, device_id).await.unwrap();
        assert!(
            !registry
                .context
                .identities
                .lock()
                .await
                .contains_key(&vault_id)
        );
    }
}
