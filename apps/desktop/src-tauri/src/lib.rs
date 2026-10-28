//! Peppy desktop native host. The webview only receives sanitized view models (`dto`) and
//! state-change hints; credentials, keys, passphrases, tokens and file paths stay in Rust.
use std::{
    collections::HashMap,
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::PathBuf,
    str::FromStr,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::Instant,
};
use tauri::{Emitter, Listener, Manager, RunEvent, State, WindowEvent};
use tauri_plugin_autostart::ManagerExt as AutostartExt;
use tauri_plugin_opener::OpenerExt;
use unicode_general_category::{get_general_category, GeneralCategory};

mod app_lock;
mod clipboard_image;
mod composer_scope;
mod contacts;
mod credentials;
mod devices;
mod dialogs;
mod dto;
mod enrollment;
mod error;
mod fsutil;
mod gateways;
mod head_palette;
mod heads;
mod heads_runtime;

#[cfg(test)]
mod activation_session_tests;
#[cfg(test)]
mod draft_command_tests;
mod hosted;
mod lifecycle;
mod material_state;
mod media;
mod net;
mod notification_permission;
mod notifications;
mod origin;
mod search_history;
mod secure_store;
#[cfg(test)]
mod security_tests;
mod session;
mod startup;
mod sync;
#[cfg(test)]
mod tests;
mod tls;
mod tray;
mod tray_badge;
#[cfg(any(target_os = "linux", test))]
mod update_linux;
mod update_safety;
mod updates;
mod window_chrome;
mod window_geometry;
mod window_placement;
mod windows;

use credentials::{
    check_origin_binding, ensure_database_key, load_config, parse_credential, save_config,
    store_credential, HostConfig,
};
use dto::{DraftView, Head, HistoryPage, PublicCopyView, SendResultView, Snapshot};
use error::{core_error, BridgeError, BridgeResult};
use notifications::{NotificationPreferences, NotificationSettings, NotificationView};
use peppy_client_core::{
    AttachmentId, AttachmentInfo, ConversationId, LocalSearchRequest, MessageId,
    NativePlaintextFile, NotificationTarget, SearchIndexState,
};
use peppy_desktop_api::compose::DiscardDraftInput;
#[cfg(target_os = "macos")]
use secure_store::BundledStore;
use secure_store::{KeyringStore, SecretStore};
use session::{open_session, DeferredHydration, DraftInput, Notifier, Session, VaultSummary};
use sync::{blocking, fetch_vault, vault_header};

#[cfg(target_os = "linux")]
static IMAGE_CLIPBOARD: std::sync::OnceLock<Mutex<Option<arboard::Clipboard>>> =
    std::sync::OnceLock::new();

pub const STATE_EVENT: &str = "peppy://state";
pub const DRAFT_EVENT: &str = "peppy://draft";
static TRAY_ACTIVE: AtomicBool = AtomicBool::new(false);
const MAX_OPEN_MEDIA_CAPABILITIES: usize = 8;
const MAX_OPEN_MEDIA_BYTES: u64 = 128 * 1024 * 1024;
const MAX_INLINE_MEDIA_BYTES: u64 = 32 * 1024 * 1024;
const MAX_MEDIA_RANGE_BYTES: u64 = 4 * 1024 * 1024;
const MAX_CONCURRENT_MEDIA_READS: usize = 2;
const MAX_MEDIA_SYNC_ATTACHMENTS: usize = 32;

struct MediaReadBudget {
    available: Mutex<usize>,
}

impl MediaReadBudget {
    fn new(maximum: usize) -> Self {
        Self {
            available: Mutex::new(maximum),
        }
    }

    fn try_acquire(self: &Arc<Self>) -> Option<MediaReadPermit> {
        let mut available = self
            .available
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if *available == 0 {
            return None;
        }
        *available -= 1;
        Some(MediaReadPermit {
            budget: self.clone(),
        })
    }
}

struct MediaReadPermit {
    budget: Arc<MediaReadBudget>,
}

impl Drop for MediaReadPermit {
    fn drop(&mut self) {
        if let Ok(mut available) = self.budget.available.lock() {
            *available += 1;
        }
    }
}

struct MediaCapability {
    session: Arc<Session>,
    attachment_id: AttachmentId,
    window_label: String,
    media_type: String,
    plaintext_bytes: u64,
    plaintext: NativePlaintextFile,
    opened_at: Instant,
}

type ReleasedMediaCapabilities = Vec<(Arc<Session>, AttachmentId)>;
type StoredMediaCapability = (OpenMediaView, ReleasedMediaCapabilities);

pub struct AppState {
    root: PathBuf,
    config_path: PathBuf,
    store: Arc<dyn SecretStore>,
    config_lock: Mutex<()>,
    session: tokio::sync::Mutex<Option<Arc<Session>>>,
    /// Serializes session retirement and opening without holding `session` across a worker drain.
    session_open_lock: tokio::sync::Mutex<()>,
    composer_scopes: Mutex<HashMap<String, composer_scope::MovedScope>>,
    enrollment: Mutex<enrollment::Controller>,
    /// Serializes credential import and post-dialog credential export against configuration changes.
    import_lock: tokio::sync::Mutex<()>,
    notifier: Notifier,
    notifications: Arc<NotificationSettings>,
    lifecycle: std::sync::Mutex<lifecycle::Coordinator>,
    lifecycle_gate: std::sync::Mutex<lifecycle::TopologyGate>,
    head_generation: std::sync::Mutex<heads::GenerationFence>,
    allow_exit: AtomicBool,
    quit_requested: AtomicBool,
    app_locked: AtomicBool,
    app_lock_storage_available: AtomicBool,
    app_lock_attempts: Arc<app_lock::Attempts>,
    media_capabilities: Mutex<HashMap<String, MediaCapability>>,
    media_read_budget: Arc<MediaReadBudget>,
    update_safety: update_safety::SafetyGate,
    updates: updates::UpdateManager,
}

/// Commands a composer window may call (mirrors `capabilities/composer.json`).
pub const COMPOSER_COMMANDS: &[&str] = &[
    "load_state",
    "load_history",
    "open_external_url",
    "save_draft",
    "send_draft",
    "mark_seen",
    "pick_attachments",
    "retry_attachment",
    "save_attachment",
    "request_attachment_download",
    "media_sync_request",
    "media_page",
    "media_page_around",
    "media_page_newer",
    "open_media",
    "load_media",
    "copy_attachment_image",
    "hydrate_media_preview",
    "close_media",
    "search_contact_recipients",
    "close_composer",
    "close_head_panel",
    "acknowledge_lifecycle",
];

fn window_error() -> BridgeError {
    BridgeError::new(
        "window-context",
        "This operation is only available in the main window.",
    )
}

fn admit_update_operation(state: &AppState) -> BridgeResult<update_safety::OperationGuard> {
    state
        .update_safety
        .enter()
        .map_err(|message| BridgeError::new("update-busy", message))
}

/// Runtime defense in depth behind the Tauri ACL: settings, import, unlock, publication,
/// composer creation and head commands are main-window only.
pub fn require_main(label: &str) -> BridgeResult<()> {
    if label == tray::MAIN {
        Ok(())
    } else {
        Err(window_error())
    }
}

fn require_mark_all_read_window(
    label: &str,
    visible: bool,
    minimized: bool,
    focused: bool,
) -> BridgeResult<()> {
    require_main(label)?;
    if !visible || minimized || !focused {
        return Err(BridgeError::new(
            "window-not-visible",
            "Messages can only be marked read from a visible focused window.",
        ));
    }
    Ok(())
}

/// A composer may follow only a move that was returned to this same window in its active session.
fn effective_composer_conversation(
    state: &AppState,
    label: &str,
    session: &Arc<Session>,
    conversation: Option<&str>,
) -> BridgeResult<Option<ConversationId>> {
    let source = tray::composer_conversation(label).ok_or_else(window_error)?;
    let requested = conversation
        .and_then(|id| ConversationId::from_str(id).ok())
        .ok_or_else(window_error)?;
    let scopes = state
        .composer_scopes
        .lock()
        .map_err(|_| BridgeError::host_state())?;
    composer_scope::effective_conversation(source, scopes.get(label), &session.binding, requested)
        .map(Some)
        .ok_or_else(window_error)
}

fn remember_composer_move(
    state: &AppState,
    label: &str,
    session: &Arc<Session>,
    error: &BridgeError,
) {
    if error.code != "draft-moved" || tray::composer_conversation(label).is_none() {
        return;
    }
    let Some(conversation) = error
        .conversation_id
        .as_deref()
        .and_then(|id| ConversationId::from_str(id).ok())
    else {
        return;
    };
    if let Ok(mut scopes) = state.composer_scopes.lock() {
        scopes.insert(
            label.to_owned(),
            composer_scope::MovedScope {
                binding: session.binding.clone(),
                conversation,
            },
        );
    }
}

fn remember_composer_target(
    state: &AppState,
    label: &str,
    session: &Arc<Session>,
    conversation: &str,
) {
    if tray::composer_conversation(label).is_none() {
        return;
    }
    let Some(conversation) = ConversationId::from_str(conversation).ok() else {
        return;
    };
    if let Ok(mut scopes) = state.composer_scopes.lock() {
        scopes.insert(
            label.to_owned(),
            composer_scope::MovedScope {
                binding: session.binding.clone(),
                conversation,
            },
        );
    }
}

impl AppState {
    pub fn new(root: PathBuf, store: Arc<dyn SecretStore>, notifier: Notifier) -> Self {
        let notifications = Arc::new(NotificationSettings::load(&root));
        Self {
            config_path: root.join("server.json"),
            root,
            store,
            config_lock: Mutex::new(()),
            session: tokio::sync::Mutex::new(None),
            session_open_lock: tokio::sync::Mutex::new(()),
            composer_scopes: Mutex::new(HashMap::new()),
            enrollment: Mutex::new(enrollment::Controller::default()),
            import_lock: tokio::sync::Mutex::new(()),
            notifier,
            notifications,
            lifecycle: std::sync::Mutex::new(lifecycle::Coordinator::default()),
            lifecycle_gate: std::sync::Mutex::new(lifecycle::TopologyGate::default()),
            head_generation: std::sync::Mutex::new(heads::GenerationFence::default()),
            allow_exit: AtomicBool::new(false),
            quit_requested: AtomicBool::new(false),
            app_locked: AtomicBool::new(true),
            app_lock_storage_available: AtomicBool::new(true),
            app_lock_attempts: Arc::new(app_lock::Attempts::default()),
            media_capabilities: Mutex::new(HashMap::new()),
            media_read_budget: Arc::new(MediaReadBudget::new(MAX_CONCURRENT_MEDIA_READS)),
            update_safety: update_safety::SafetyGate::default(),
            updates: updates::UpdateManager::new(env!("CARGO_PKG_VERSION").into()),
        }
    }

    fn config(&self) -> BridgeResult<HostConfig> {
        load_config(&self.config_path)
    }

    fn update_config(&self, update: impl FnOnce(&mut HostConfig)) -> BridgeResult<HostConfig> {
        let _guard = self
            .config_lock
            .lock()
            .map_err(|_| BridgeError::host_state())?;
        let mut config = load_config(&self.config_path)?;
        update(&mut config);
        save_config(&self.config_path, &config)?;
        Ok(config)
    }

    async fn close_session_preserving_heads(&self, preserve_heads: bool) {
        let _opening = self.session_open_lock.lock().await;
        if let Ok(mut scopes) = self.composer_scopes.lock() {
            scopes.clear();
        }
        if !preserve_heads {
            if let Ok(mut generation) = self.head_generation.lock() {
                generation.invalidate();
            }
        }
        let session = self.session.lock().await.take();
        self.revoke_media_capabilities();
        if let Some(session) = session {
            session.cancel.cancel();
        }
    }

    async fn close_session(&self) {
        self.close_session_preserving_heads(false).await;
    }

    /// The session for the active binding, opened on demand. A session for any other binding is
    /// stopped first, so a client/key opened for one credential is never reused for another.
    pub async fn session(&self) -> BridgeResult<Option<Arc<Session>>> {
        let operation = self
            .update_safety
            .enter()
            .map_err(|message| BridgeError::new("update-busy", message))?;
        self.session_admitted(&operation).await
    }

    /// Opens the active session while an existing operation admission remains held.
    pub(crate) async fn session_admitted(
        &self,
        _operation: &update_safety::OperationGuard,
    ) -> BridgeResult<Option<Arc<Session>>> {
        let _opening = self.session_open_lock.lock().await;
        let config = self.config()?;
        let Some(binding) = config.active_binding().cloned() else {
            if let Some(old) = self.session.lock().await.take() {
                old.cancel.cancel();
            }
            self.revoke_media_capabilities();
            return Ok(None);
        };
        let retiring = {
            let mut current = self.session.lock().await;
            if let Some(existing) = current.as_ref() {
                if existing.binding == binding && !existing.cancel.is_cancelled() {
                    return Ok(Some(existing.clone()));
                }
            }
            current.take()
        };
        if let Some(retiring) = retiring {
            retiring.cancel.cancel();
            self.revoke_media_capabilities();
            if !sync::drain(&retiring, std::time::Duration::from_secs(30)).await {
                // Keep the retiring session observable while its workers own
                // storage. `session_open_lock` serializes every opener, so a
                // timeout must never leave an empty slot that admits another.
                let mut current = self.session.lock().await;
                if current.is_none() {
                    *current = Some(retiring);
                }
                return Err(BridgeError::new(
                    "session-draining",
                    "Peppy is still finishing the previous account session.",
                ));
            }
            let current = self.session.lock().await.clone();
            if current.is_some() {
                return Ok(current);
            }
        }
        if let Ok(mut scopes) = self.composer_scopes.lock() {
            composer_scope::clear_for_binding(&mut scopes, &binding);
        }
        let epochs = config
            .known(&binding)
            .map(|known| known.cached_epochs.clone())
            .unwrap_or_default();
        let (root, store, notifier, opening_binding) = (
            self.root.clone(),
            self.store.clone(),
            self.notifier.clone(),
            binding.clone(),
        );
        let session = Arc::new(
            blocking(move || open_session(&root, &*store, &opening_binding, &epochs, notifier))
                .await?,
        );
        sync::start(&session);
        let mut current = self.session.lock().await;
        if let Some(existing) = current.as_ref() {
            if existing.binding == binding && !existing.cancel.is_cancelled() {
                return Ok(Some(existing.clone()));
            }
        }
        *current = Some(session.clone());
        Ok(Some(session))
    }

    async fn require_session(&self) -> BridgeResult<Arc<Session>> {
        self.session().await?.ok_or_else(BridgeError::no_session)
    }

    fn revoke_media_capabilities(&self) {
        if let Ok(mut capabilities) = self.media_capabilities.lock() {
            capabilities.clear();
        }
    }

    async fn require_session_admitted(
        &self,
        operation: &update_safety::OperationGuard,
    ) -> BridgeResult<Arc<Session>> {
        self.session_admitted(operation)
            .await?
            .ok_or_else(BridgeError::no_session)
    }
}

/// Opens the current binding for state snapshots, preserving secure-store and session failures.
#[cfg(test)]
async fn snapshot_session(state: &AppState) -> BridgeResult<Option<Arc<Session>>> {
    state.session().await
}

async fn release_media_capabilities(state: &AppState, capabilities: ReleasedMediaCapabilities) {
    let Ok(_operation) = admit_update_operation(state) else {
        return;
    };
    for (session, attachment) in capabilities {
        let active = state.session.lock().await;
        let current = !state.app_locked.load(Ordering::Acquire)
            && active
                .as_ref()
                .is_some_and(|candidate| Arc::ptr_eq(candidate, &session));
        drop(active);
        if current {
            let _ = blocking(move || {
                session
                    .client
                    .release_attachment_ciphertext(attachment)
                    .map_err(core_error)
            })
            .await;
        }
    }
}

fn take_window_media_capabilities(
    state: &AppState,
    window_label: &str,
) -> ReleasedMediaCapabilities {
    let Ok(mut capabilities) = state.media_capabilities.lock() else {
        return Vec::new();
    };
    let tokens: Vec<_> = capabilities
        .iter()
        .filter(|(_, capability)| capability.window_label == window_label)
        .map(|(token, _)| token.clone())
        .collect();
    tokens
        .into_iter()
        .filter_map(|token| {
            capabilities
                .remove(&token)
                .map(|capability| (capability.session, capability.attachment_id))
        })
        .collect()
}

fn head() -> Head {
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    let (enabled, capability, note) = (true, "available", None);
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let (enabled, capability, note) = (
        false,
        "unsupported",
        Some("Floating conversation heads are unavailable on this platform.".into()),
    );
    Head {
        enabled,
        capability,
        note,
        pinned_conversation_ids: None,
        panel: None,
    }
}

fn empty_snapshot(origin: Option<String>) -> Snapshot {
    let code = if origin.is_some() {
        "credentials-required"
    } else {
        "server-required"
    };
    Snapshot {
        version: "1",
        mode: "native",
        connection: dto::Connection {
            state: "offline",
            origin,
            error_code: Some(code),
        },
        encryption: dto::Encryption {
            state: "locked",
            profile_fingerprint: None,
        },
        gateways: vec![],
        conversations: vec![],
        notifications: vec![],
        app_filters: vec![],
        notification_preferences: NotificationPreferences::default(),
        active_conversation_id: None,
        draft: None,
        head: head(),
        desktop: None,
        device_role: None,
        pending_count: 0,
        quarantine_count: 0,
        contact_resolution: None,
        contact_books: None,
        contacts_pending_count: None,
        contact_sync: None,
        tone_overrides: None,
        sync_progress: None,
    }
}

fn locked_snapshot(storage_available: bool) -> Snapshot {
    let mut snapshot = empty_snapshot(None);
    snapshot.connection.error_code = Some(if storage_available {
        "app-locked"
    } else {
        "secure-store-unavailable"
    });
    snapshot
}

struct LoadStateOptions {
    conversation_id: Option<String>,
    history_from_message_id: Option<String>,
    origin: Option<String>,
    preferences: NotificationPreferences,
    metadata_only: bool,
}

/// Loads a snapshot using the same window scope policy as the Tauri command.
#[cfg(test)]
async fn load_state_for_window(
    state: &AppState,
    label: &str,
    session: Arc<Session>,
    conversation_id: Option<String>,
    history_from_message_id: Option<String>,
    origin: Option<String>,
    preferences: NotificationPreferences,
) -> BridgeResult<(Snapshot, DeferredHydration)> {
    load_state_for_window_with_options(
        state,
        label,
        session,
        LoadStateOptions {
            conversation_id,
            history_from_message_id,
            origin,
            preferences,
            metadata_only: false,
        },
    )
    .await
}

async fn load_state_for_window_with_options(
    state: &AppState,
    label: &str,
    session: Arc<Session>,
    options: LoadStateOptions,
) -> BridgeResult<(Snapshot, DeferredHydration)> {
    let LoadStateOptions {
        conversation_id,
        history_from_message_id,
        origin,
        preferences,
        metadata_only,
    } = options;
    if label == tray::MAIN {
        return blocking(move || {
            session.snapshot_with_metadata_only(
                conversation_id.as_deref(),
                history_from_message_id.as_deref(),
                head(),
                origin,
                preferences,
                metadata_only,
            )
        })
        .await;
    }
    let conversation_id =
        effective_composer_conversation(state, label, &session, conversation_id.as_deref())?
            .expect("composer conversation");
    blocking(move || {
        if metadata_only {
            return session.snapshot_scoped_metadata_only(
                &conversation_id.to_string(),
                head(),
                origin,
                preferences,
            );
        }
        session.snapshot_scoped(
            &conversation_id.to_string(),
            history_from_message_id.as_deref(),
            head(),
            origin,
            preferences,
        )
    })
    .await
}

#[tauri::command]
async fn load_state(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    conversation_id: Option<String>,
    history_from_message_id: Option<String>,
    metadata_only: Option<bool>,
) -> BridgeResult<Snapshot> {
    let operation = admit_update_operation(&state)?;
    if state.app_locked.load(Ordering::Acquire) {
        return Ok(locked_snapshot(
            state.app_lock_storage_available.load(Ordering::Acquire),
        ));
    }
    let origin = state.config()?.origin;
    let Some(session) = state.session_admitted(&operation).await? else {
        return Ok(empty_snapshot(origin));
    };
    session.consume_deferred_follow_up();
    let preferences = state.notifications.preferences();
    let (mut snapshot, deferred) = load_state_for_window_with_options(
        &state,
        window.label(),
        session.clone(),
        LoadStateOptions {
            conversation_id,
            history_from_message_id,
            origin,
            preferences,
            metadata_only: metadata_only.unwrap_or(false),
        },
    )
    .await?;
    if state.app_locked.load(Ordering::Acquire) {
        return Ok(locked_snapshot(
            state.app_lock_storage_available.load(Ordering::Acquire),
        ));
    }
    if !session_is_current(&state, &session).await {
        return Ok(empty_snapshot(state.config()?.origin));
    }
    let (pinned_conversation_ids, panel) = heads_runtime::snapshot(&app, window.label());
    snapshot.head.pinned_conversation_ids = Some(pinned_conversation_ids);
    snapshot.head.panel = Some(panel);
    snapshot.desktop = Some(dto::Desktop {
        tray_available: TRAY_ACTIVE.load(Ordering::Relaxed),
        start_at_login: app.autolaunch().is_enabled().unwrap_or(false),
        startup_supported: cfg!(any(target_os = "macos", target_os = "windows")),
        background: startup::background_requested(std::env::args()),
    });
    if session.schedule_deferred_follow_up(deferred) {
        session.notify_deferred_follow_up();
    }
    start_search_index_driver(app, session);
    Ok(snapshot)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SearchIndexDriverAction {
    Wait,
    Ready { notify: bool },
}

fn observe_search_index_state(
    session: &Session,
    prior_state: Option<SearchIndexState>,
    state: SearchIndexState,
) -> SearchIndexDriverAction {
    if prior_state == Some(SearchIndexState::Indexing) {
        session
            .index_driver_saw_indexing
            .store(true, Ordering::Release);
    }
    match state {
        SearchIndexState::Indexing => {
            session
                .index_driver_saw_indexing
                .store(true, Ordering::Release);
            SearchIndexDriverAction::Wait
        }
        SearchIndexState::Ready => SearchIndexDriverAction::Ready {
            notify: session
                .index_driver_saw_indexing
                .swap(false, Ordering::AcqRel),
        },
    }
}

async fn search_index_state(session: Arc<Session>) -> BridgeResult<SearchIndexState> {
    blocking(move || {
        session
            .client
            .search_local(LocalSearchRequest {
                query: String::new(),
                kind: None,
                cursor: None,
                limit: None,
            })
            .map(|page| page.index_state)
            .map_err(core_error)
    })
    .await
}

fn claim_search_index_driver(session: &Session) -> bool {
    !session.index_driver_ready.load(Ordering::Acquire)
        && session
            .index_driver_running
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
}

async fn complete_search_index_batch(
    session: &Session,
    operation: update_safety::OperationGuard,
    action: SearchIndexDriverAction,
) -> bool {
    drop(operation);
    match action {
        SearchIndexDriverAction::Ready { notify } => {
            session.index_driver_ready.store(true, Ordering::Release);
            if notify {
                session.notify();
            }
            false
        }
        SearchIndexDriverAction::Wait => {
            tokio::select! {
                _ = session.cancel.cancelled() => false,
                _ = tokio::time::sleep(std::time::Duration::from_secs(1)) => true,
            }
        }
    }
}

fn start_search_index_driver(app: tauri::AppHandle, session: Arc<Session>) {
    if !claim_search_index_driver(&session) {
        return;
    }
    let worker_session = session.clone();
    session.track_worker(tokio::spawn(async move {
        let session = worker_session;
        let mut initial_state = true;
        loop {
            let state = app.state::<AppState>();
            if state.app_locked.load(Ordering::Acquire)
                || session.cancel.is_cancelled()
                || !session_is_current(&state, &session).await
            {
                break;
            }
            let operation = match admit_update_operation(&state) {
                Ok(operation) => operation,
                Err(_) => break,
            };
            let prior_state = if initial_state {
                initial_state = false;
                match search_index_state(session.clone()).await {
                    Ok(state) => state,
                    Err(_) => break,
                }
            } else {
                SearchIndexState::Indexing
            };
            if state.app_locked.load(Ordering::Acquire)
                || session.cancel.is_cancelled()
                || !session_is_current(&state, &session).await
            {
                break;
            }
            if prior_state == SearchIndexState::Ready {
                let action = observe_search_index_state(&session, None, SearchIndexState::Ready);
                let _ = complete_search_index_batch(&session, operation, action).await;
                break;
            }
            let driver_session = session.clone();
            let progress = blocking(move || {
                driver_session
                    .client
                    .advance_search_index(Some(100))
                    .map_err(core_error)
            })
            .await;
            let Ok(progress) = progress else { break };
            if state.app_locked.load(Ordering::Acquire)
                || session.cancel.is_cancelled()
                || !session_is_current(&state, &session).await
            {
                break;
            }
            let action =
                observe_search_index_state(&session, Some(prior_state), progress.index_state);
            if !complete_search_index_batch(&session, operation, action).await {
                break;
            }
        }
        session.index_driver_running.store(false, Ordering::Release);
    }));
}

#[tauri::command]
async fn search_local(
    window: tauri::WebviewWindow,
    state: State<'_, AppState>,
    input: peppy_desktop_api::LocalSearchRequestView,
) -> BridgeResult<peppy_desktop_api::LocalSearchPageView> {
    let operation = admit_update_operation(&state)?;
    require_main(window.label())?;
    if state.app_locked.load(Ordering::Acquire) {
        return Err(BridgeError::new("app-locked", "Peppy is locked."));
    }
    let session = state.require_session_admitted(&operation).await?;
    let search_session = session.clone();
    let page = blocking(move || {
        search_session
            .client
            .search_local(input.into())
            .map(peppy_desktop_api::local_search_page_view)
            .map_err(core_error)
    })
    .await?;
    if state.app_locked.load(Ordering::Acquire) || !session_is_current(&state, &session).await {
        return Err(BridgeError::new("app-locked", "Peppy is locked."));
    }
    Ok(page)
}

#[tauri::command]
async fn advance_search_index(
    window: tauri::WebviewWindow,
    state: State<'_, AppState>,
    input: Option<peppy_desktop_api::AdvanceSearchIndexInputView>,
) -> BridgeResult<peppy_desktop_api::SearchIndexProgressView> {
    let operation = admit_update_operation(&state)?;
    require_main(window.label())?;
    if state.app_locked.load(Ordering::Acquire) {
        return Err(BridgeError::new("app-locked", "Peppy is locked."));
    }
    let session = state.require_session_admitted(&operation).await?;
    let budget_rows = input.and_then(|input| input.budget_rows);
    let index_session = session.clone();
    let progress = blocking(move || {
        index_session
            .client
            .advance_search_index(budget_rows)
            .map(peppy_desktop_api::search_index_progress_view)
            .map_err(core_error)
    })
    .await?;
    if state.app_locked.load(Ordering::Acquire) || !session_is_current(&state, &session).await {
        return Err(BridgeError::new("app-locked", "Peppy is locked."));
    }
    Ok(progress)
}

#[tauri::command]
async fn load_message_window(
    window: tauri::WebviewWindow,
    state: State<'_, AppState>,
    input: peppy_desktop_api::MessageWindowRequestView,
) -> BridgeResult<peppy_desktop_api::MessageWindowView<dto::MessageView>> {
    let operation = admit_update_operation(&state)?;
    require_main(window.label())?;
    if state.app_locked.load(Ordering::Acquire) {
        return Err(BridgeError::new("app-locked", "Peppy is locked."));
    }
    let session = state.require_session_admitted(&operation).await?;
    let history_session = session.clone();
    let result = blocking(move || history_session.message_window(input)).await?;
    if state.app_locked.load(Ordering::Acquire) || !session_is_current(&state, &session).await {
        return Err(BridgeError::new("app-locked", "Peppy is locked."));
    }
    Ok(result)
}

fn invalid_url() -> BridgeError {
    BridgeError::new("invalid-request", "The URL is invalid.")
}

fn contains_forbidden_url_character(value: &str) -> bool {
    value.chars().any(|character| {
        matches!(
            get_general_category(character),
            GeneralCategory::Control | GeneralCategory::Format
        )
    })
}

fn validate_external_url(value: &str) -> BridgeResult<url::Url> {
    if value.chars().count() > 4096
        || contains_forbidden_url_character(value)
        || value.contains('\\')
    {
        return Err(invalid_url());
    }
    let normalized = if value.starts_with("www.") {
        format!("https://{value}")
    } else {
        value.to_owned()
    };
    if !(normalized.starts_with("http://") || normalized.starts_with("https://")) {
        return Err(invalid_url());
    }
    let authority = normalized
        .split_once("://")
        .map(|(_, rest)| rest.split(['/', '?', '#']).next().unwrap_or_default())
        .unwrap_or_default();
    if authority.is_empty() || authority.ends_with(':') || authority.contains('@') {
        return Err(invalid_url());
    }
    let raw_host = authority
        .strip_prefix('[')
        .and_then(|authority| authority.split_once(']').map(|(host, _)| host))
        .unwrap_or_else(|| {
            authority
                .rsplit_once(':')
                .map_or(authority, |(host, port)| {
                    if port.chars().all(|character| character.is_ascii_digit()) {
                        host
                    } else {
                        authority
                    }
                })
        });
    if raw_host.is_empty()
        || (!authority.starts_with('[')
            && raw_host.split('.').any(|label| {
                label.is_empty()
                    || label.starts_with('-')
                    || label.ends_with('-')
                    || !label.chars().all(|character| {
                        character.is_alphanumeric()
                            || character == '-'
                            || matches!(character as u32, 0x0300..=0x036f | 0x1ab0..=0x1aff | 0x1dc0..=0x1dff | 0x20d0..=0x20ff | 0xfe20..=0xfe2f)
                    })
            }))
    {
        return Err(invalid_url());
    }
    let url = url::Url::parse(&normalized).map_err(|_| invalid_url())?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(invalid_url());
    }
    if matches!(url.host(), Some(url::Host::Ipv4(_)))
        && raw_host != url.host_str().unwrap_or_default()
    {
        return Err(invalid_url());
    }
    Ok(url)
}

fn has_url_credentials(url: &url::Url) -> bool {
    !url.username().is_empty() || url.password().is_some()
}

fn matches_origin(url: &url::Url, trusted: &url::Url) -> bool {
    !has_url_credentials(url)
        && !has_url_credentials(trusted)
        && url.scheme() == trusted.scheme()
        && url.host() == trusted.host()
        && url.port_or_known_default() == trusted.port_or_known_default()
}

fn allows_app_navigation(url: &url::Url, configured_dev_url: Option<&url::Url>) -> bool {
    let trusted_tauri_origin =
        url::Url::parse("tauri://localhost").expect("valid trusted asset origin");
    let trusted_windows_origin =
        url::Url::parse("http://tauri.localhost").expect("valid trusted asset origin");
    matches_origin(url, &trusted_tauri_origin)
        || matches_origin(url, &trusted_windows_origin)
        || (cfg!(debug_assertions)
            && configured_dev_url.is_some_and(|dev_url| matches_origin(url, dev_url)))
}

#[tauri::command]
fn open_external_url(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    url: String,
) -> BridgeResult<()> {
    if state.app_locked.load(Ordering::Acquire) {
        return Err(BridgeError::new("app-locked", "Peppy is locked."));
    }
    let _operation = admit_update_operation(&state)?;
    if window.label() != tray::MAIN && tray::composer_conversation(window.label()).is_none() {
        return Err(window_error());
    }
    let url = validate_external_url(&url)?;
    app.opener()
        .open_url(url.as_str(), None::<String>)
        .map_err(|_| BridgeError::new("open-failed", "Could not open the URL."))
}

async fn session_is_current(state: &AppState, session: &Arc<Session>) -> bool {
    state
        .session
        .lock()
        .await
        .as_ref()
        .is_some_and(|current| Arc::ptr_eq(current, session))
}

#[tauri::command]
async fn load_history(
    window: tauri::WebviewWindow,
    state: State<'_, AppState>,
    conversation_id: String,
    before_message_id: String,
) -> BridgeResult<HistoryPage> {
    let operation = admit_update_operation(&state)?;
    if state.app_locked.load(Ordering::Acquire) {
        return Err(BridgeError::new("app-locked", "Peppy is locked."));
    }
    let session = state.require_session_admitted(&operation).await?;
    let scoped_conversation = if window.label() == tray::MAIN {
        ConversationId::from_str(&conversation_id)
            .map_err(|_| BridgeError::new("invalid-request", "The conversation ID is invalid."))?
    } else {
        effective_composer_conversation(&state, window.label(), &session, Some(&conversation_id))?
            .expect("composer conversation")
    };
    let history_session = session.clone();
    let page = blocking(move || {
        history_session.history_page(&scoped_conversation.to_string(), &before_message_id)
    })
    .await?;
    if state.app_locked.load(Ordering::Acquire) || !session_is_current(&state, &session).await {
        return Err(BridgeError::new("app-locked", "Peppy is locked."));
    }
    Ok(page)
}

#[tauri::command]
async fn configure_server(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    origin: String,
) -> BridgeResult<()> {
    let operation = admit_update_operation(&state)?;
    require_main(window.label())?;
    let origin = origin::validate_origin(&origin)?;
    let _import = state.import_lock.lock().await;
    let previous = state.config()?;
    let switching = previous.origin.as_deref() != Some(origin.as_str());
    let permit = if switching {
        Some(lifecycle::prepare_switch(&app).await?)
    } else {
        None
    };
    // Credentials bound to another origin are deactivated (kept in secure storage, never sent to
    // the new origin); a binding previously imported for this origin is reactivated.
    let updated = state.update_config(|config| config.select_origin(&origin))?;
    let preserve_heads = previous.active_binding() == updated.active_binding();
    state.close_session_preserving_heads(preserve_heads).await;
    if let Err(error) = state.session_admitted(&operation).await {
        let rollback = previous.clone();
        let _ = state.update_config(|config| *config = rollback);
        state.close_session_preserving_heads(preserve_heads).await;
        let _ = state.session_admitted(&operation).await;
        return Err(error);
    }
    if let Some(permit) = permit {
        permit.finish().await?;
    }
    (state.notifier)();
    Ok(())
}

/// Activates a credential produced by a native enrollment flow. Its origin is selected first so
/// the existing origin-binding, verification, and activation checks apply unchanged; the previous
/// configuration is restored if any step fails.
async fn activate_native_credential(
    app: &tauri::AppHandle,
    state: &AppState,
    operation: &update_safety::OperationGuard,
    bytes: zeroize::Zeroizing<Vec<u8>>,
) -> BridgeResult<()> {
    let credential = Arc::new(parse_credential(&bytes)?);
    drop(bytes);
    let _import = state.import_lock.lock().await;
    let previous = state.config()?;
    let permit = if previous.active_binding() != Some(&credential.binding()) {
        Some(lifecycle::prepare_switch(app).await?)
    } else {
        None
    };
    let result = async {
        state.update_config(|config| config.select_origin(&credential.origin))?;
        verify_import(state, &credential).await?;
        activate_import_locked(state, credential.clone(), Some(operation)).await
    }
    .await;
    if let Err(error) = result {
        let rollback = previous.clone();
        let _ = state.update_config(|config| *config = rollback);
        return Err(error);
    }
    if let Some(permit) = permit {
        permit.finish().await?;
    }
    (state.notifier)();
    Ok(())
}

/// Shared by tests: proves the credential at its own origin,
/// then activates it under the import lock.
#[cfg(test)]
async fn verify_and_activate_test_credential(
    state: &AppState,
    credential: Arc<credentials::ImportedCredential>,
) -> BridgeResult<()> {
    verify_import(state, &credential).await?;
    activate_import(state, credential).await
}

async fn verify_import(
    state: &AppState,
    credential: &Arc<credentials::ImportedCredential>,
) -> BridgeResult<()> {
    // Refuse before any network use: the token must never be sent to a non-configured origin.
    check_configured_origin(state, credential)?;
    let (store, check) = (state.store.clone(), credential.clone());
    blocking(move || check_origin_binding(&*store, &check)).await?;
    let api = net::Api::new(&credential.origin, &credential.device_token)?;
    fetch_vault(&api, &credential.vault_id, &credential.device_id).await?;
    Ok(())
}

fn check_configured_origin(
    state: &AppState,
    credential: &credentials::ImportedCredential,
) -> BridgeResult<()> {
    match state.config()?.origin {
        Some(origin) if origin != credential.origin => Err(BridgeError::new(
            "origin-binding",
            "This credential belongs to a different server. Configure that server URL first; credentials are never sent to another origin.",
        )),
        _ => Ok(()),
    }
}

/// Serialized activation of an already verified credential: the origin binding is re-checked,
/// an existing database key is preserved (never replaced), the credential is stored, the config
/// activated, and any session for the binding is replaced so a rotated token takes effect.
#[cfg(test)]
async fn activate_import(
    state: &AppState,
    credential: Arc<credentials::ImportedCredential>,
) -> BridgeResult<()> {
    let _import = state.import_lock.lock().await;
    activate_import_locked(state, credential, None).await
}

async fn activate_import_locked(
    state: &AppState,
    credential: Arc<credentials::ImportedCredential>,
    operation: Option<&update_safety::OperationGuard>,
) -> BridgeResult<()> {
    // Re-checked under the lock: the origin may have changed while the credential was verified.
    check_configured_origin(state, &credential)?;
    let binding = credential.binding();
    let previous = state.config()?;
    let preserve_heads = previous.active_binding() == Some(&binding);
    let (store, root, keyed, stored) = (
        state.store.clone(),
        state.root.clone(),
        binding.clone(),
        credential.clone(),
    );
    blocking(move || {
        check_origin_binding(&*store, &stored)?;
        ensure_database_key(&*store, &keyed, &keyed.database_path(&root))?;
        store_credential(&*store, &stored)
    })
    .await?;
    state.update_config(|config| {
        config.select_origin(&binding.origin);
        config.remember(&binding);
        config.active = Some(binding.clone());
    })?;
    state.close_session_preserving_heads(preserve_heads).await;
    let open_session = match operation {
        Some(operation) => state.session_admitted(operation).await,
        None => state.session().await,
    };
    if let Err(error) = open_session {
        let rollback = previous.clone();
        let _ = state.update_config(|config| *config = rollback);
        state.close_session_preserving_heads(preserve_heads).await;
        let _ = match operation {
            Some(operation) => state.session_admitted(operation).await,
            None => state.session().await,
        };
        return Err(error);
    }
    Ok(())
}

#[tauri::command]
async fn unlock_sync(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> BridgeResult<()> {
    let operation = admit_update_operation(&state)?;
    require_main(window.label())?;
    let session = state.require_session_admitted(&operation).await?;
    let vault = fetch_vault(
        &session.api,
        &session.binding.vault_id,
        &session.binding.device_id,
    )
    .await?;
    let (profile, header) = vault_header(&vault)?;
    let Some(passphrase) = dialogs::passphrase(&app, dialogs::PassphrasePurpose::Unlock).await?
    else {
        return Ok(());
    };
    let verified_passphrase = passphrase.clone();
    let result = unlock_with(
        &state,
        &session,
        profile,
        header,
        passphrase,
        vault.profile_fingerprint,
    )
    .await;
    if result.is_ok() {
        if let Ok(current) = state.require_session_admitted(&operation).await {
            if Arc::ptr_eq(&current, &session) && current.binding == session.binding {
                let _ = enrollment::save_verified_vault_secret_if_empty(
                    &*state.store,
                    &session.binding.vault_secret_account(),
                    &verified_passphrase,
                );
            }
        }
    }
    session.notify();
    result
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct AppLockStatus {
    enabled: bool,
    locked: bool,
    storage_available: bool,
}

#[tauri::command]
fn get_app_lock_status(
    window: tauri::WebviewWindow,
    state: State<'_, AppState>,
) -> BridgeResult<AppLockStatus> {
    require_main(window.label())?;
    let configured = app_lock::status(&*state.store);
    let storage_available =
        state.app_lock_storage_available.load(Ordering::Acquire) && configured.is_ok();
    let enabled = storage_available && matches!(configured, Ok(app_lock::Status::Enabled));
    Ok(AppLockStatus {
        enabled,
        locked: state.app_locked.load(Ordering::Acquire),
        storage_available,
    })
}

#[tauri::command]
async fn unlock_app(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> BridgeResult<bool> {
    let _operation = admit_update_operation(&state)?;
    require_main(window.label())?;
    if !state.app_locked.load(Ordering::Acquire) {
        return Ok(true);
    }
    if !state.app_lock_storage_available.load(Ordering::Acquire) {
        return Ok(false);
    }
    let Some(password) =
        dialogs::passphrase(&app, dialogs::PassphrasePurpose::AppLockUnlock).await?
    else {
        return Ok(false);
    };
    let store = state.store.clone();
    let attempts = state.app_lock_attempts.clone();
    let verified = blocking(move || app_lock::verify(&*store, password, &attempts)).await?;
    if verified {
        state.app_locked.store(false, Ordering::Release);
        (state.notifier)();
    }
    Ok(verified)
}

#[tauri::command]
async fn set_app_lock(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    enabled: bool,
) -> BridgeResult<bool> {
    let _operation = admit_update_operation(&state)?;
    require_main(window.label())?;
    if !state.app_lock_storage_available.load(Ordering::Acquire) {
        return Ok(false);
    }
    let currently_enabled = matches!(app_lock::status(&*state.store)?, app_lock::Status::Enabled);
    if enabled == currently_enabled {
        return Ok(true);
    }
    if enabled {
        let Some(password) =
            dialogs::passphrase(&app, dialogs::PassphrasePurpose::AppLockCreate).await?
        else {
            return Ok(false);
        };
        let Some(confirm) =
            dialogs::passphrase(&app, dialogs::PassphrasePurpose::AppLockConfirm).await?
        else {
            return Ok(false);
        };
        if password != confirm {
            return Ok(false);
        }
        let store = state.store.clone();
        blocking(move || app_lock::enable(&*store, &password)).await?;
    } else {
        let Some(password) =
            dialogs::passphrase(&app, dialogs::PassphrasePurpose::AppLockDisable).await?
        else {
            return Ok(false);
        };
        let store = state.store.clone();
        let attempts = state.app_lock_attempts.clone();
        if !blocking(move || app_lock::verify(&*store, password, &attempts)).await? {
            return Ok(false);
        }
        let store = state.store.clone();
        blocking(move || app_lock::disable(&*store)).await?;
    }
    Ok(true)
}

async fn unlock_with(
    state: &AppState,
    session: &Arc<Session>,
    profile: peppy_client_core::KeyProfile,
    header: peppy_client_core::VaultCheckHeader,
    passphrase: zeroize::Zeroizing<String>,
    fingerprint: String,
) -> BridgeResult<()> {
    let epoch = profile.key_epoch;
    let (s, store) = (session.clone(), state.store.clone());
    blocking(move || {
        match s.client.unlock(&profile, &header, &passphrase) {
            Err(peppy_client_core::Error::InvalidProfile) => {
                s.mismatch.store(true, Ordering::Relaxed);
                return Err(core_error(peppy_client_core::Error::InvalidProfile));
            }
            other => other.map_err(core_error)?,
        }
        s.mismatch.store(false, Ordering::Relaxed);
        // The server reports this epoch as current; local work must be sealed under it.
        if s.client
            .key_status()
            .map_err(core_error)?
            .active_epoch
            .is_some_and(|active| active < epoch)
        {
            s.client.activate_epoch(epoch).map_err(core_error)?;
        }
        let cache = s
            .client
            .export_native_key_cache(epoch)
            .map_err(core_error)?;
        store.set(
            &s.binding.key_cache_account(epoch),
            cache.native_storage_bytes(),
        )
    })
    .await?;
    let binding = session.binding.clone();
    state.update_config(|config| config.add_cached_epoch(&binding, epoch))?;
    session.set_status(|status| status.vault = Some(VaultSummary { epoch, fingerprint }));
    // The renderer refreshes only on state events; the claim path notifies before this unlock.
    session.notify();
    session.request_work();
    session.reconnect_wake.notify_one();
    Ok(())
}

/// Saves a draft and records a returned move only for the originating composer window.
async fn save_draft_for_window(
    state: &AppState,
    label: &str,
    session: Arc<Session>,
    input: DraftInput,
) -> BridgeResult<DraftView> {
    if label != tray::MAIN {
        effective_composer_conversation(
            state,
            label,
            &session,
            Some(input.conversation_id.as_str()),
        )?;
    }
    let requested_conversation = input.conversation_id.clone();
    let result = blocking({
        let session = session.clone();
        move || session.save_draft(&input)
    })
    .await;
    if let Ok(draft) = &result {
        if draft.conversation_id != requested_conversation {
            remember_composer_target(state, label, &session, &draft.conversation_id);
        }
    } else if let Err(error) = &result {
        remember_composer_move(state, label, &session, error);
        if error.code == "draft-moved" {
            session.notify();
        }
    }
    result
}

/// Returns only windows allowed to receive a successful saved draft's sanitized view.
/// Composer windows are restricted to their source conversation or their own
/// locally observed move for this binding.
fn draft_event_target_labels_after_save<'a>(
    state: &AppState,
    session: &Session,
    save: &BridgeResult<DraftView>,
    app_locked: bool,
    session_current: bool,
    labels: impl IntoIterator<Item = &'a str>,
) -> Vec<String> {
    if app_locked || !session_current {
        return Vec::new();
    }
    let Ok(draft) = save else {
        return Vec::new();
    };
    let Ok(conversation) = ConversationId::from_str(&draft.conversation_id) else {
        return Vec::new();
    };
    let Ok(scopes) = state.composer_scopes.lock() else {
        return labels
            .into_iter()
            .filter(|label| *label == tray::MAIN)
            .map(str::to_owned)
            .collect();
    };
    labels
        .into_iter()
        .filter(|label| {
            if *label == tray::MAIN {
                return true;
            }
            let Some(source) = tray::composer_conversation(label) else {
                return false;
            };
            composer_scope::effective_conversation(
                source,
                scopes.get(*label),
                &session.binding,
                source,
            ) == Some(conversation)
        })
        .map(str::to_owned)
        .collect()
}

fn emit_saved_draft(
    app: &tauri::AppHandle,
    state: &AppState,
    session: &Session,
    save: &BridgeResult<DraftView>,
    session_current: bool,
) {
    let windows = app.webview_windows();
    for label in draft_event_target_labels_after_save(
        state,
        session,
        save,
        state.app_locked.load(Ordering::Acquire),
        session_current,
        windows.keys().map(String::as_str),
    ) {
        if state.app_locked.load(Ordering::Acquire) {
            break;
        }
        if let Ok(draft) = save {
            let _ = app.emit_to(label, DRAFT_EVENT, draft);
        }
    }
}

#[tauri::command]
async fn save_draft(
    window: tauri::WebviewWindow,
    state: State<'_, AppState>,
    input: DraftInput,
) -> BridgeResult<DraftView> {
    // The lifecycle reservation permits only the already-open session for a captured webview
    // that still owes its update draft flush. It must not create or replace a session.
    let update_flush = lifecycle::update_flush_pending(window.app_handle(), window.label());
    let (_operation, session) = if update_flush {
        let session = state
            .session
            .lock()
            .await
            .clone()
            .ok_or_else(BridgeError::no_session)?;
        (None, session)
    } else {
        let operation = admit_update_operation(&state)?;
        let session = state.require_session_admitted(&operation).await?;
        (Some(operation), session)
    };
    let save = save_draft_for_window(&state, window.label(), session.clone(), input).await;
    let session_current = save.is_ok() && session_is_current(&state, &session).await;
    emit_saved_draft(
        window.app_handle(),
        &state,
        &session,
        &save,
        session_current,
    );
    save
}

#[tauri::command]
async fn discard_draft(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    input: DiscardDraftInput,
) -> BridgeResult<()> {
    let operation = admit_update_operation(&state)?;
    require_main(window.label())?;
    let session = state.require_session_admitted(&operation).await?;
    let conversation_id = input.conversation_id.clone();
    blocking(move || session.discard_draft(&input)).await?;
    if let Err(error) = heads_runtime::forget_deleted_conversation(&app, &conversation_id) {
        eprintln!(
            "could not clear deleted conversation pin: {}",
            error.message
        );
    }
    close_deleted_composers(&app, &state, &conversation_id);
    Ok(())
}

fn close_deleted_composers(app: &tauri::AppHandle, state: &AppState, conversation_id: &str) {
    let Ok(deleted) = ConversationId::from_str(conversation_id) else {
        return;
    };
    let moved = state.composer_scopes.lock().ok();
    for label in app.webview_windows().keys() {
        let matches_source =
            tray::composer_conversation(label).is_some_and(|source| source == deleted);
        let matches_moved = moved
            .as_ref()
            .and_then(|scopes| scopes.get(label))
            .is_some_and(|scope| scope.conversation == deleted);
        if matches_source || matches_moved {
            let _ = lifecycle::request_window(app, label, lifecycle::Action::Close);
        }
    }
}

/// Sends a draft through the same scoped path as the Tauri command.
async fn send_draft_for_window(
    state: &AppState,
    label: &str,
    session: Arc<Session>,
    input: DraftInput,
) -> BridgeResult<SendResultView> {
    if label != tray::MAIN {
        effective_composer_conversation(
            state,
            label,
            &session,
            Some(input.conversation_id.as_str()),
        )?;
    }
    let s = session.clone();
    let result = blocking(move || s.send_draft(&input)).await;
    if let Err(error) = &result {
        remember_composer_move(state, label, &session, error);
        if error.code == "draft-moved" {
            session.notify();
        }
    } else {
        session.notify();
    }
    result
}

#[tauri::command]
async fn send_draft(
    window: tauri::WebviewWindow,
    state: State<'_, AppState>,
    input: DraftInput,
) -> BridgeResult<SendResultView> {
    let operation = admit_update_operation(&state)?;
    let session = state.require_session_admitted(&operation).await?;
    send_draft_for_window(&state, window.label(), session, input).await
}

#[tauri::command]
async fn mark_seen(
    window: tauri::WebviewWindow,
    state: State<'_, AppState>,
    visible_message_ids: Vec<String>,
) -> BridgeResult<()> {
    let operation = admit_update_operation(&state)?;
    // A composer must never acknowledge another conversation's messages. The
    // UI supplies message IDs rather than a conversation ID, so resolve each
    // one through the scoped native session before applying the acknowledgement.
    if !window.is_visible().unwrap_or(false)
        || window.is_minimized().unwrap_or(true)
        || !window.is_focused().unwrap_or(false)
    {
        return Err(BridgeError::new(
            "window-not-visible",
            "Messages can only be marked seen from a visible focused window.",
        ));
    }
    let session = state.require_session_admitted(&operation).await?;
    let conversation = if window.label() == tray::MAIN {
        None
    } else {
        let source = tray::composer_conversation(window.label()).ok_or_else(window_error)?;
        effective_composer_conversation(
            &state,
            window.label(),
            &session,
            Some(&source.to_string()),
        )?
    };
    blocking(move || session.mark_seen_scoped(&visible_message_ids, conversation)).await
}

#[tauri::command]
async fn mark_all_read(
    window: tauri::WebviewWindow,
    state: State<'_, AppState>,
) -> BridgeResult<String> {
    require_mark_all_read_window(
        window.label(),
        window.is_visible().unwrap_or(false),
        window.is_minimized().unwrap_or(true),
        window.is_focused().unwrap_or(false),
    )?;
    let operation = admit_update_operation(&state)?;
    let session = state.require_session_admitted(&operation).await?;
    blocking(move || session.mark_all_read()).await
}

#[tauri::command]
async fn list_contact_books(state: State<'_, AppState>) -> BridgeResult<Vec<serde_json::Value>> {
    let operation = admit_update_operation(&state)?;
    let session = state.require_session_admitted(&operation).await?;
    blocking(move || contacts::list_books(&session)).await
}

#[tauri::command]
async fn list_devices(state: State<'_, AppState>) -> BridgeResult<devices::DeviceRosterView> {
    let operation = admit_update_operation(&state)?;
    let session = state.require_session_admitted(&operation).await?;
    devices::list(&session).await
}

#[tauri::command]
async fn revoke_device(state: State<'_, AppState>, device_id: String) -> BridgeResult<()> {
    let operation = admit_update_operation(&state)?;
    let session = state.require_session_admitted(&operation).await?;
    devices::revoke(&session, &device_id).await
}

#[tauri::command]
async fn list_contacts(
    state: State<'_, AppState>,
    book_id: String,
    query: Option<String>,
    offset: Option<u32>,
    sort: Option<serde_json::Value>,
) -> BridgeResult<Vec<serde_json::Value>> {
    let operation = admit_update_operation(&state)?;
    let session = state.require_session_admitted(&operation).await?;
    blocking(move || {
        contacts::list_contacts(&session, &book_id, query.as_deref(), offset, sort.as_ref())
    })
    .await
}

/// Display-only recipient discovery: phone numbers of matching contacts (addresses, not IDs).
#[tauri::command]
async fn search_contact_recipients(
    state: State<'_, AppState>,
    query: String,
    source_device_id: Option<String>,
) -> BridgeResult<Vec<serde_json::Value>> {
    let operation = admit_update_operation(&state)?;
    let session = state.require_session_admitted(&operation).await?;
    blocking(move || contacts::search_recipients(&session, &query, source_device_id.as_deref()))
        .await
}

/// Latches a contact projection repair; the live loop runs one fenced snapshot.
#[tauri::command]
async fn request_contact_repair(state: State<'_, AppState>) -> BridgeResult<serde_json::Value> {
    let operation = admit_update_operation(&state)?;
    let session = state.require_session_admitted(&operation).await?;
    blocking(move || contacts::request_repair(&session)).await
}

#[tauri::command]
async fn request_contact_sync(
    state: State<'_, AppState>,
    book_id: String,
) -> BridgeResult<serde_json::Value> {
    let operation = admit_update_operation(&state)?;
    let session = state.require_session_admitted(&operation).await?;
    blocking(move || contacts::request_sync(&session, &book_id)).await
}

#[tauri::command]
async fn get_contact_sync_request(
    state: State<'_, AppState>,
    book_id: String,
) -> BridgeResult<serde_json::Value> {
    let operation = admit_update_operation(&state)?;
    let session = state.require_session_admitted(&operation).await?;
    blocking(move || contacts::sync_request_status(&session, &book_id)).await
}

/// This device's contact edit requests and the owner results recorded in the local ledger.
#[tauri::command]
async fn list_contact_edits(
    state: State<'_, AppState>,
    book_id: Option<String>,
) -> BridgeResult<Vec<serde_json::Value>> {
    let operation = admit_update_operation(&state)?;
    let session = state.require_session_admitted(&operation).await?;
    blocking(move || contacts::list_edits(&session, book_id.as_deref())).await
}

/// Native image picker for the contact photo cropper; the webview receives only a bounded,
/// re-encoded data URL, never a path or the original file bytes.
#[tauri::command]
async fn pick_contact_photo(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> BridgeResult<Option<serde_json::Value>> {
    let operation = admit_update_operation(&state)?;
    state.require_session_admitted(&operation).await?;
    let Some(path) = dialogs::pick_file(&app, "Choose a contact photo").await? else {
        return Ok(None);
    };
    blocking(move || contacts::photo_source(&path))
        .await
        .map(Some)
}

#[tauri::command]
async fn forget_contact_book(state: State<'_, AppState>, book_id: String) -> BridgeResult<()> {
    let operation = admit_update_operation(&state)?;
    let session = state.require_session_admitted(&operation).await?;
    blocking(move || contacts::forget_book(&session, &book_id)).await
}

#[tauri::command]
async fn submit_contact_edit(
    state: State<'_, AppState>,
    input: serde_json::Value,
) -> BridgeResult<serde_json::Value> {
    let operation = admit_update_operation(&state)?;
    let session = state.require_session_admitted(&operation).await?;
    blocking(move || contacts::submit(&session, &input)).await
}

#[tauri::command]
async fn list_restorable_contacts(
    state: State<'_, AppState>,
    book_id: String,
) -> BridgeResult<Vec<serde_json::Value>> {
    let operation = admit_update_operation(&state)?;
    let session = state.require_session_admitted(&operation).await?;
    blocking(move || contacts::restorable(&session, &book_id)).await
}

#[tauri::command]
async fn restore_contact(
    state: State<'_, AppState>,
    book_id: String,
    contact_id: String,
) -> BridgeResult<serde_json::Value> {
    let operation = admit_update_operation(&state)?;
    let session = state.require_session_admitted(&operation).await?;
    blocking(move || contacts::restore(&session, &book_id, &contact_id)).await
}

#[tauri::command]
async fn pick_attachments(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> BridgeResult<Vec<dto::AttachmentView>> {
    let operation = admit_update_operation(&state)?;
    let session = state.require_session_admitted(&operation).await?;
    let paths = dialogs::pick_files(&app, "Attach files (encrypted before upload)").await?;
    if paths.len() > 10 {
        return Err(BridgeError::new(
            "invalid-attachment",
            "Select at most 10 files.",
        ));
    }
    blocking(move || {
        paths
            .iter()
            .map(|path| session.prepare_attachment(path))
            .collect()
    })
    .await
}

fn safe_suggested_filename(name: &str, media_type: &str) -> String {
    let value: String = name
        .chars()
        .filter(|c| {
            !c.is_control()
                && !matches!(c, '/' | '\\' | ':')
                && !matches!(*c as u32, 0x200e..=0x200f | 0x202a..=0x202e | 0x2066..=0x2069)
        })
        .take(128)
        .collect();
    let stem = value
        .trim()
        .trim_matches('.')
        .split('.')
        .next()
        .unwrap_or("attachment");
    let extension = match media_type {
        "image/jpeg" => "jpg",
        "image/png" => "png",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/heic" => "heic",
        "video/3gpp" => "3gp",
        "video/mp4" => "mp4",
        "audio/amr" => "amr",
        "audio/mpeg" => "mp3",
        "text/x-vcard" | "text/vcard" => "vcf",
        "application/smil" => "smil",
        "application/pdf" => "pdf",
        "text/plain" => "txt",
        _ => "bin",
    };
    format!(
        "{}.{}",
        if stem.is_empty() { "attachment" } else { stem },
        extension
    )
}

#[cfg(test)]
mod mms_filename_tests {
    use super::safe_suggested_filename;
    #[test]
    fn suggested_filename_removes_spoofing_and_uses_verified_extension() {
        assert_eq!(
            safe_suggested_filename("../invoice\u{202e}fdp.exe", "application/pdf"),
            "invoicefdp.pdf"
        );
        assert_eq!(
            safe_suggested_filename("\0...", "unknown/type"),
            "attachment.bin"
        );
        for (media_type, extension) in [
            ("image/heic", "heic"),
            ("video/3gpp", "3gp"),
            ("video/mp4", "mp4"),
            ("audio/amr", "amr"),
            ("audio/mpeg", "mp3"),
            ("text/x-vcard", "vcf"),
            ("text/vcard", "vcf"),
            ("application/smil", "smil"),
        ] {
            assert_eq!(
                safe_suggested_filename("media.original", media_type),
                format!("media.{extension}")
            );
        }
    }
}

async fn check_attachment_window_scope(
    state: &AppState,
    session: &Arc<Session>,
    window: &tauri::WebviewWindow,
    id: &str,
) -> BridgeResult<()> {
    if window.label() == tray::MAIN {
        return Ok(());
    }
    let source = tray::composer_conversation(window.label()).ok_or_else(window_error)?;
    let conversation =
        effective_composer_conversation(state, window.label(), session, Some(&source.to_string()))?
            .ok_or_else(window_error)?;
    let (session, id) = (session.clone(), id.to_owned());
    let belongs = blocking(move || session.attachment_in_conversation(&id, conversation)).await?;
    belongs.then_some(()).ok_or_else(window_error)
}

async fn check_media_preview_window_scope(
    state: &AppState,
    session: &Arc<Session>,
    window: &tauri::WebviewWindow,
    message_id: &str,
) -> BridgeResult<()> {
    if window.label() == tray::MAIN {
        return Ok(());
    }
    let source = tray::composer_conversation(window.label()).ok_or_else(window_error)?;
    let conversation =
        effective_composer_conversation(state, window.label(), session, Some(&source.to_string()))?
            .ok_or_else(window_error)?;
    let (session, message_id) = (session.clone(), message_id.to_owned());
    let message_conversation =
        blocking(move || session.media_preview_message_conversation(&message_id)).await?;
    (message_conversation == conversation.to_string())
        .then_some(())
        .ok_or_else(window_error)
}

async fn demand_attachment_if_needed(
    session: &Arc<Session>,
    attachment: AttachmentId,
) -> BridgeResult<bool> {
    let client = session.client.clone();
    let demanded = blocking(move || {
        if client
            .attachment_download_requested(attachment)
            .map_err(core_error)?
        {
            return Ok(false);
        }
        client
            .request_attachment_download(attachment)
            .map_err(core_error)?;
        Ok(true)
    })
    .await?;
    if demanded {
        session.request_work();
        session.notify();
    }
    Ok(demanded)
}

struct NativeImageClipboard;

impl clipboard_image::ClipboardImageWriter for NativeImageClipboard {
    type Error = ();

    fn write_rgba(
        &mut self,
        image: clipboard_image::ClipboardRgbaImage,
    ) -> Result<(), Self::Error> {
        let image = arboard::ImageData {
            width: usize::try_from(image.width).map_err(|_| ())?,
            height: usize::try_from(image.height).map_err(|_| ())?,
            bytes: image.bytes.into(),
        };
        set_native_clipboard_image(image)
    }
}

#[cfg(target_os = "linux")]
fn set_native_clipboard_image(image: arboard::ImageData<'static>) -> Result<(), ()> {
    let mut owner = IMAGE_CLIPBOARD
        .get_or_init(|| Mutex::new(None))
        .lock()
        .map_err(|_| ())?;
    if owner.is_none() {
        *owner = Some(arboard::Clipboard::new().map_err(|_| ())?);
    }
    owner.as_mut().ok_or(())?.set_image(image).map_err(|_| ())
}

#[cfg(not(target_os = "linux"))]
fn set_native_clipboard_image(image: arboard::ImageData<'static>) -> Result<(), ()> {
    arboard::Clipboard::new()
        .and_then(|mut clipboard| clipboard.set_image(image))
        .map_err(|_| ())
}

fn clipboard_image_error(error: clipboard_image::ClipboardImageError) -> BridgeError {
    match error {
        clipboard_image::ClipboardImageError::Unsupported => BridgeError::new(
            "unsupported",
            "Only PNG, JPEG, WebP, and GIF images can be copied.",
        ),
        clipboard_image::ClipboardImageError::SourceTooLarge
        | clipboard_image::ClipboardImageError::DecodedTooLarge => {
            BridgeError::new("media-too-large", "This image is too large to copy safely.")
        }
        clipboard_image::ClipboardImageError::Invalid => BridgeError::new(
            "attachment-invalid",
            "The image could not be decoded for copying.",
        ),
        clipboard_image::ClipboardImageError::ClipboardUnavailable => BridgeError::new(
            "clipboard-unavailable",
            "The system clipboard is unavailable. Try again.",
        ),
    }
}

struct PreparedClipboardImage {
    _lease: RetainedAttachment,
    plaintext: NativePlaintextFile,
    image: clipboard_image::ClipboardRgbaImage,
}

struct RetainedAttachment {
    session: Arc<Session>,
    attachment: AttachmentId,
}

impl Drop for RetainedAttachment {
    fn drop(&mut self) {
        let _ = self
            .session
            .client
            .release_attachment_ciphertext(self.attachment);
    }
}

#[tauri::command]
async fn retry_attachment(
    window: tauri::WebviewWindow,
    state: State<'_, AppState>,
    id: String,
) -> BridgeResult<()> {
    let operation = admit_update_operation(&state)?;
    let session = state.require_session_admitted(&operation).await?;
    check_attachment_window_scope(&state, &session, &window, &id).await?;
    let s = session.clone();
    blocking(move || s.retry_attachment(&id)).await?;
    session.notify();
    Ok(())
}

#[tauri::command]
async fn save_attachment(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    id: String,
) -> BridgeResult<bool> {
    let operation = admit_update_operation(&state)?;
    let session = state.require_session_admitted(&operation).await?;
    check_attachment_window_scope(&state, &session, &window, &id).await?;
    let attachment = AttachmentId::from_str(&id)
        .map_err(|_| BridgeError::new("invalid-attachment", "The attachment ID is invalid."))?;
    let s = session.clone();
    let info = blocking(move || s.client.attachment_info(attachment).map_err(core_error)).await?;
    if !info.state.is_local() {
        demand_attachment_if_needed(&session, attachment).await?;
        return Err(BridgeError::new(
            "media-pending",
            "The attachment is downloading. Try Save again when it is ready.",
        ));
    }
    let Some(destination) = dialogs::save_file(
        &app,
        "Save attachment",
        &safe_suggested_filename(&info.display_name, &info.media_type),
    )
    .await?
    else {
        return Ok(false);
    };
    blocking(move || session.save_attachment(&id, &destination)).await?;
    Ok(true)
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct OpenMediaView {
    state: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    media_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    byte_size: Option<u64>,
}

#[cfg(target_os = "windows")]
fn media_capability_url(token: &str) -> String {
    format!("http://peppy-media.localhost/{token}")
}
#[cfg(not(target_os = "windows"))]
fn media_capability_url(token: &str) -> String {
    format!("peppy-media://localhost/{token}")
}

fn media_capability_token(url: &str) -> Option<&str> {
    let prefix = if cfg!(target_os = "windows") {
        "http://peppy-media.localhost/"
    } else {
        "peppy-media://localhost/"
    };
    let token = url.strip_prefix(prefix)?;
    (!token.is_empty() && !token.contains('/')).then_some(token)
}

fn inline_media_type(media_type: &str) -> Option<&'static str> {
    match media_type {
        "image/jpeg" => Some("image/jpeg"),
        "image/png" => Some("image/png"),
        "image/gif" => Some("image/gif"),
        "image/webp" => Some("image/webp"),
        "video/mp4" => Some("video/mp4"),
        "video/3gpp" => Some("video/3gpp"),
        "audio/mpeg" => Some("audio/mpeg"),
        "audio/amr" => Some("audio/amr"),
        _ => None,
    }
}

fn store_media_capability(
    state: &AppState,
    session: Arc<Session>,
    window_label: &str,
    info: AttachmentInfo,
    plaintext: NativePlaintextFile,
) -> BridgeResult<StoredMediaCapability> {
    let media_type = inline_media_type(&info.media_type).ok_or_else(|| {
        BridgeError::new(
            "unsupported",
            "Save this media to open it in another application.",
        )
    })?;
    if info.plaintext_bytes > MAX_INLINE_MEDIA_BYTES {
        return Err(BridgeError::new(
            "media-too-large",
            "This media item is too large to open safely.",
        ));
    }
    let mut capabilities = state
        .media_capabilities
        .lock()
        .map_err(|_| BridgeError::host_state())?;
    let mut released = Vec::new();
    while capabilities.len() >= MAX_OPEN_MEDIA_CAPABILITIES
        || capabilities
            .values()
            .map(|capability| capability.plaintext_bytes)
            .sum::<u64>()
            + info.plaintext_bytes
            > MAX_OPEN_MEDIA_BYTES
    {
        let Some(token) = capabilities
            .iter()
            .min_by_key(|(_, capability)| capability.opened_at)
            .map(|(token, _)| token.clone())
        else {
            break;
        };
        if let Some(capability) = capabilities.remove(&token) {
            released.push((capability.session, capability.attachment_id));
        }
    }
    let token = uuid::Uuid::new_v4().to_string();
    capabilities.insert(
        token.clone(),
        MediaCapability {
            session,
            attachment_id: info.attachment_id,
            window_label: window_label.to_owned(),
            media_type: media_type.to_owned(),
            plaintext_bytes: info.plaintext_bytes,
            plaintext,
            opened_at: Instant::now(),
        },
    );
    Ok((
        OpenMediaView {
            state: "ready",
            url: Some(media_capability_url(&token)),
            media_type: Some(media_type.to_owned()),
            byte_size: Some(info.plaintext_bytes),
        },
        released,
    ))
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct MediaCursorInput {
    sort_at_ms: String,
    local_order: String,
    message_id: String,
    attachment_position: u32,
}

impl TryFrom<MediaCursorInput> for peppy_client_core::MediaCursor {
    type Error = BridgeError;

    fn try_from(value: MediaCursorInput) -> Result<Self, Self::Error> {
        Ok(Self {
            sort_at_ms: value
                .sort_at_ms
                .parse()
                .map_err(|_| BridgeError::new("invalid-request", "The media cursor is invalid."))?,
            local_order: value
                .local_order
                .parse()
                .map_err(|_| BridgeError::new("invalid-request", "The media cursor is invalid."))?,
            message_id: value
                .message_id
                .parse()
                .map_err(|_| BridgeError::new("invalid-request", "The media cursor is invalid."))?,
            attachment_position: value.attachment_position,
        })
    }
}

#[tauri::command]
async fn request_attachment_download(
    window: tauri::WebviewWindow,
    state: State<'_, AppState>,
    id: String,
) -> BridgeResult<()> {
    let operation = admit_update_operation(&state)?;
    let session = state.require_session_admitted(&operation).await?;
    check_attachment_window_scope(&state, &session, &window, &id).await?;
    let attachment = AttachmentId::from_str(&id)
        .map_err(|_| BridgeError::new("invalid-attachment", "The attachment ID is invalid."))?;
    demand_attachment_if_needed(&session, attachment).await?;
    Ok(())
}

#[tauri::command]
async fn hydrate_media_preview(
    window: tauri::WebviewWindow,
    state: State<'_, AppState>,
    message_id: String,
    id: String,
) -> BridgeResult<dto::AttachmentView> {
    let operation = admit_update_operation(&state)?;
    let session = state.require_session_admitted(&operation).await?;
    let _media_read = state
        .media_read_budget
        .try_acquire()
        .ok_or_else(|| BridgeError::new("media-busy", "Media is busy. Try again shortly."))?;
    check_media_preview_window_scope(&state, &session, &window, &message_id).await?;
    AttachmentId::from_str(&id)
        .map_err(|_| BridgeError::new("invalid-attachment", "The attachment ID is invalid."))?;
    let requested_message_id = message_id;
    let requested_id = id;
    let preview_session = session.clone();
    let preview = blocking(move || {
        preview_session.hydrate_media_preview(&requested_message_id, &requested_id)
    })
    .await?;
    if state.app_locked.load(Ordering::Acquire) || !session_is_current(&state, &session).await {
        return Err(BridgeError::new("locked", "Peppy is locked."));
    }
    Ok(preview)
}

#[tauri::command]
async fn copy_attachment_image(
    window: tauri::WebviewWindow,
    state: State<'_, AppState>,
    id: String,
) -> BridgeResult<()> {
    let operation = admit_update_operation(&state)?;
    let session = state.require_session_admitted(&operation).await?;
    check_attachment_window_scope(&state, &session, &window, &id).await?;
    let attachment = AttachmentId::from_str(&id)
        .map_err(|_| BridgeError::new("invalid-attachment", "The attachment ID is invalid."))?;
    let _media_read = state
        .media_read_budget
        .try_acquire()
        .ok_or_else(|| BridgeError::new("media-busy", "Media is busy. Try Copy again shortly."))?;
    let info_session = session.clone();
    let info = blocking(move || {
        let info = info_session
            .client
            .attachment_info(attachment)
            .map_err(core_error)?;
        if !clipboard_image::is_supported_media_type(&info.media_type) {
            return Err(clipboard_image_error(
                clipboard_image::ClipboardImageError::Unsupported,
            ));
        }
        if info.plaintext_bytes > media::MAX_IMAGE_SOURCE_BYTES {
            return Err(clipboard_image_error(
                clipboard_image::ClipboardImageError::SourceTooLarge,
            ));
        }
        Ok(info)
    })
    .await?;
    if !info.state.is_local() {
        demand_attachment_if_needed(&session, attachment).await?;
        return Err(BridgeError::new(
            "media-pending",
            "The attachment is downloading. Try Copy again when it is ready.",
        ));
    }
    let prepared_session = session.clone();
    let prepared = blocking(move || {
        prepared_session
            .client
            .retain_attachment_ciphertext(attachment)
            .map_err(core_error)?;
        let lease = RetainedAttachment {
            session: prepared_session.clone(),
            attachment,
        };
        let plaintext = prepared_session
            .client
            .open_native_plaintext(attachment)
            .map_err(core_error)?;
        let image =
            clipboard_image::decode_authorized_image_file(plaintext.path(), &info.media_type)
                .map_err(clipboard_image_error)?;
        Ok(PreparedClipboardImage {
            _lease: lease,
            plaintext,
            image,
        })
    })
    .await?;
    let active = state.session.lock().await;
    if state.app_locked.load(Ordering::Acquire)
        || !active
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, &session))
    {
        drop(active);
        drop(prepared);
        return Err(BridgeError::new("locked", "Peppy is locked."));
    }
    let PreparedClipboardImage {
        _lease,
        plaintext,
        image,
    } = prepared;
    let result = blocking(move || {
        let _lease = _lease;
        let _plaintext = plaintext;
        clipboard_image::write_rgba(&mut NativeImageClipboard, image).map_err(clipboard_image_error)
    })
    .await;
    drop(active);
    result
}

#[tauri::command]
async fn media_sync_request(
    window: tauri::WebviewWindow,
    state: State<'_, AppState>,
    attachment_ids: Vec<String>,
) -> BridgeResult<()> {
    if !(1..=MAX_MEDIA_SYNC_ATTACHMENTS).contains(&attachment_ids.len()) {
        return Err(BridgeError::new(
            "invalid-request",
            "Choose between one and 32 attachments to sync.",
        ));
    }
    let operation = admit_update_operation(&state)?;
    let session = state.require_session_admitted(&operation).await?;
    for id in &attachment_ids {
        check_attachment_window_scope(&state, &session, &window, id).await?;
    }
    let attachments = attachment_ids
        .iter()
        .map(|id| {
            AttachmentId::from_str(id).map_err(|_| {
                BridgeError::new("invalid-attachment", "The attachment ID is invalid.")
            })
        })
        .collect::<BridgeResult<Vec<_>>>()?;
    let client = session.client.clone();
    let report = blocking(move || {
        client
            .request_mms_media_sync(&attachments)
            .map_err(core_error)
    })
    .await?;
    if !report.requested.is_empty() {
        session.request_work();
        session.notify();
    }
    Ok(())
}

#[tauri::command]
async fn media_page(
    window: tauri::WebviewWindow,
    state: State<'_, AppState>,
    conversation_id: String,
    cursor: Option<MediaCursorInput>,
    limit: Option<usize>,
) -> BridgeResult<peppy_desktop_api::MediaPageView> {
    let operation = admit_update_operation(&state)?;
    let session = state.require_session_admitted(&operation).await?;
    let conversation = ConversationId::from_str(&conversation_id)
        .map_err(|_| BridgeError::new("invalid-request", "The conversation ID is invalid."))?;
    if window.label() != tray::MAIN {
        let scoped = effective_composer_conversation(
            &state,
            window.label(),
            &session,
            Some(&conversation_id),
        )?;
        if scoped != Some(conversation) {
            return Err(window_error());
        }
    }
    let cursor = cursor
        .map(peppy_client_core::MediaCursor::try_from)
        .transpose()?;
    blocking(move || session.media_page(conversation, cursor.as_ref(), limit.unwrap_or(50))).await
}

#[tauri::command]
async fn media_page_around(
    window: tauri::WebviewWindow,
    state: State<'_, AppState>,
    conversation_id: String,
    attachment_id: String,
    message_id: Option<String>,
    limit: Option<usize>,
) -> BridgeResult<peppy_desktop_api::MediaPageAroundView> {
    let operation = admit_update_operation(&state)?;
    let session = state.require_session_admitted(&operation).await?;
    let conversation = ConversationId::from_str(&conversation_id)
        .map_err(|_| BridgeError::new("invalid-request", "The conversation ID is invalid."))?;
    let attachment = AttachmentId::from_str(&attachment_id)
        .map_err(|_| BridgeError::new("invalid-attachment", "The attachment ID is invalid."))?;
    let message = message_id
        .map(|id| {
            MessageId::from_str(&id)
                .map_err(|_| BridgeError::new("invalid-request", "The message ID is invalid."))
        })
        .transpose()?;
    if window.label() != tray::MAIN {
        let scoped = effective_composer_conversation(
            &state,
            window.label(),
            &session,
            Some(&conversation_id),
        )?;
        if scoped != Some(conversation) {
            return Err(window_error());
        }
    }
    blocking(move || {
        session.media_page_around(conversation, attachment, message, limit.unwrap_or(50))
    })
    .await
}

#[tauri::command]
async fn media_page_newer(
    window: tauri::WebviewWindow,
    state: State<'_, AppState>,
    conversation_id: String,
    cursor: MediaCursorInput,
    limit: Option<usize>,
) -> BridgeResult<peppy_desktop_api::MediaPageView> {
    let operation = admit_update_operation(&state)?;
    let session = state.require_session_admitted(&operation).await?;
    let conversation = ConversationId::from_str(&conversation_id)
        .map_err(|_| BridgeError::new("invalid-request", "The conversation ID is invalid."))?;
    if window.label() != tray::MAIN {
        let scoped = effective_composer_conversation(
            &state,
            window.label(),
            &session,
            Some(&conversation_id),
        )?;
        if scoped != Some(conversation) {
            return Err(window_error());
        }
    }
    let cursor = peppy_client_core::MediaCursor::try_from(cursor)?;
    blocking(move || session.media_page_newer(conversation, &cursor, limit.unwrap_or(50))).await
}

#[tauri::command]
async fn open_media(
    window: tauri::WebviewWindow,
    state: State<'_, AppState>,
    id: String,
) -> BridgeResult<OpenMediaView> {
    let operation = admit_update_operation(&state)?;
    let session = state.require_session_admitted(&operation).await?;
    check_attachment_window_scope(&state, &session, &window, &id).await?;
    let attachment = AttachmentId::from_str(&id)
        .map_err(|_| BridgeError::new("invalid-attachment", "The attachment ID is invalid."))?;
    let acquired_session = session.clone();
    let ready = blocking(move || {
        let info = acquired_session
            .client
            .attachment_info(attachment)
            .map_err(core_error)?;
        if !info.state.is_local() {
            return Ok(None);
        }
        acquired_session
            .client
            .retain_attachment_ciphertext(attachment)
            .map_err(core_error)?;
        match acquired_session.client.open_native_plaintext(attachment) {
            Ok(plaintext) => {
                if let Err(error) = acquired_session
                    .client
                    .touch_attachment_ciphertext(attachment)
                {
                    let _ = acquired_session
                        .client
                        .release_attachment_ciphertext(attachment);
                    return Err(core_error(error));
                }
                Ok(Some((info, plaintext)))
            }
            Err(error) => {
                let _ = acquired_session
                    .client
                    .release_attachment_ciphertext(attachment);
                Err(core_error(error))
            }
        }
    })
    .await?;
    match ready {
        Some((info, plaintext)) => {
            let active = state.session.lock().await;
            if state.app_locked.load(Ordering::Acquire)
                || !active
                    .as_ref()
                    .is_some_and(|current| Arc::ptr_eq(current, &session))
            {
                drop(active);
                let release = session.clone();
                let _ = blocking(move || {
                    release
                        .client
                        .release_attachment_ciphertext(attachment)
                        .map_err(core_error)
                })
                .await;
                return Err(BridgeError::new("locked", "Peppy is locked."));
            }
            match store_media_capability(&state, session.clone(), window.label(), info, plaintext) {
                Ok((view, evicted)) => {
                    drop(active);
                    release_media_capabilities(&state, evicted).await;
                    Ok(view)
                }
                Err(error) => {
                    drop(active);
                    let release = session.clone();
                    let _ = blocking(move || {
                        release
                            .client
                            .release_attachment_ciphertext(attachment)
                            .map_err(core_error)
                    })
                    .await;
                    Err(error)
                }
            }
        }
        None => Ok(OpenMediaView {
            state: "pending",
            url: None,
            media_type: None,
            byte_size: None,
        }),
    }
}

#[tauri::command]
async fn load_media(
    window: tauri::WebviewWindow,
    state: State<'_, AppState>,
    id: String,
) -> BridgeResult<OpenMediaView> {
    let _operation = admit_update_operation(&state)?;
    request_attachment_download(window.clone(), state.clone(), id.clone()).await?;
    open_media(window, state, id).await
}

#[tauri::command]
async fn close_media(
    window: tauri::WebviewWindow,
    state: State<'_, AppState>,
    id: String,
    url: Option<String>,
) -> BridgeResult<()> {
    let _operation = admit_update_operation(&state)?;
    let attachment = AttachmentId::from_str(&id)
        .map_err(|_| BridgeError::new("invalid-attachment", "The attachment ID is invalid."))?;
    let released_sessions = {
        let mut capabilities = state
            .media_capabilities
            .lock()
            .map_err(|_| BridgeError::host_state())?;
        let mut released_sessions = Vec::new();
        if let Some(url) = url {
            let token = media_capability_token(&url)
                .ok_or_else(|| BridgeError::new("invalid-request", "The media URL is invalid."))?;
            if capabilities.get(token).is_some_and(|capability| {
                capability.window_label == window.label() && capability.attachment_id == attachment
            }) {
                if let Some(capability) = capabilities.remove(token) {
                    released_sessions.push(capability.session);
                }
            }
        } else {
            let tokens: Vec<_> = capabilities
                .iter()
                .filter(|(_, capability)| {
                    capability.window_label == window.label()
                        && capability.attachment_id == attachment
                })
                .map(|(token, _)| token.clone())
                .collect();
            for token in tokens {
                if let Some(capability) = capabilities.remove(&token) {
                    released_sessions.push(capability.session);
                }
            }
        }
        released_sessions
    };
    for session in released_sessions {
        let active = state.session.lock().await;
        let current = !state.app_locked.load(Ordering::Acquire)
            && active
                .as_ref()
                .is_some_and(|candidate| Arc::ptr_eq(candidate, &session));
        drop(active);
        if current {
            blocking(move || {
                session
                    .client
                    .release_attachment_ciphertext(attachment)
                    .map_err(core_error)
            })
            .await?;
        }
    }
    Ok(())
}

fn media_range(value: Option<&tauri::http::HeaderValue>, total: u64) -> Result<(u64, u64), ()> {
    if total == 0 {
        return Err(());
    }
    let Some(value) = value else {
        return Ok((0, total.saturating_sub(1)));
    };
    let value = value.to_str().map_err(|_| ())?;
    let range = value.strip_prefix("bytes=").ok_or(())?;
    if range.contains(',') {
        return Err(());
    }
    let (start, end) = range.split_once('-').ok_or(())?;
    if start.is_empty() {
        if end.is_empty() || !end.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(());
        }
        let suffix = end.parse::<u64>().map_err(|_| ())?;
        if suffix == 0 {
            return Err(());
        }
        let start = total.saturating_sub(suffix);
        return Ok((start, (start + MAX_MEDIA_RANGE_BYTES - 1).min(total - 1)));
    }
    if !start.bytes().all(|byte| byte.is_ascii_digit())
        || (!end.is_empty() && !end.bytes().all(|byte| byte.is_ascii_digit()))
    {
        return Err(());
    }
    let start = start.parse::<u64>().map_err(|_| ())?;
    if start >= total {
        return Err(());
    }
    let end = if end.is_empty() {
        total - 1
    } else {
        end.parse::<u64>().map_err(|_| ())?.min(total - 1)
    };
    if end < start {
        return Err(());
    }
    Ok((start, end.min(start + MAX_MEDIA_RANGE_BYTES - 1)))
}

fn media_protocol_response<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    webview_label: String,
    request: tauri::http::Request<Vec<u8>>,
    admission: MediaReadPermit,
) -> tauri::http::Response<Vec<u8>> {
    media_protocol_response_for_state(&app.state::<AppState>(), &webview_label, request, admission)
}

fn media_protocol_response_for_state(
    state: &AppState,
    webview_label: &str,
    request: tauri::http::Request<Vec<u8>>,
    admission: MediaReadPermit,
) -> tauri::http::Response<Vec<u8>> {
    media_protocol_response_with_reader(
        state,
        webview_label,
        request,
        admission,
        |path, start, bytes| {
            let mut file = File::open(path)?;
            file.seek(SeekFrom::Start(start))?;
            file.read_exact(bytes)
        },
    )
}

fn media_protocol_response_with_reader<F>(
    state: &AppState,
    webview_label: &str,
    request: tauri::http::Request<Vec<u8>>,
    _admission: MediaReadPermit,
    read_range: F,
) -> tauri::http::Response<Vec<u8>>
where
    F: FnOnce(&std::path::Path, u64, &mut [u8]) -> std::io::Result<()>,
{
    let token = request
        .uri()
        .path()
        .strip_prefix('/')
        .filter(|value| !value.is_empty() && !value.contains('/'));
    let Some(token) = token else {
        return tauri::http::Response::builder()
            .status(404)
            .body(Vec::new())
            .unwrap();
    };
    if state.app_locked.load(Ordering::Acquire) {
        return tauri::http::Response::builder()
            .status(403)
            .body(Vec::new())
            .unwrap();
    }
    let (
        capability_session,
        capability_window,
        capability_media_type,
        capability_bytes,
        capability_path,
    ) = {
        let capabilities = match state.media_capabilities.lock() {
            Ok(value) => value,
            Err(_) => {
                return tauri::http::Response::builder()
                    .status(503)
                    .body(Vec::new())
                    .unwrap()
            }
        };
        let Some(capability) = capabilities.get(token) else {
            return tauri::http::Response::builder()
                .status(404)
                .body(Vec::new())
                .unwrap();
        };
        (
            capability.session.clone(),
            capability.window_label.clone(),
            capability.media_type.clone(),
            capability.plaintext_bytes,
            capability.plaintext.path().to_path_buf(),
        )
    };
    let active = tauri::async_runtime::block_on(state.session.lock());
    if !active
        .as_ref()
        .is_some_and(|session| Arc::ptr_eq(session, &capability_session))
        || capability_window != webview_label
        || !(request.method() == tauri::http::Method::GET
            || request.method() == tauri::http::Method::HEAD)
    {
        return tauri::http::Response::builder()
            .status(403)
            .body(Vec::new())
            .unwrap();
    }
    drop(active);
    let had_range = request.headers().contains_key(tauri::http::header::RANGE);
    if !had_range && capability_bytes > MAX_INLINE_MEDIA_BYTES {
        return tauri::http::Response::builder()
            .status(413)
            .header(tauri::http::header::CACHE_CONTROL, "no-store")
            .body(Vec::new())
            .unwrap();
    }
    if request
        .headers()
        .get_all(tauri::http::header::RANGE)
        .iter()
        .count()
        != 1
        && request.headers().contains_key(tauri::http::header::RANGE)
    {
        return tauri::http::Response::builder()
            .status(416)
            .body(Vec::new())
            .unwrap();
    }
    let (start, end) = match media_range(
        request.headers().get(tauri::http::header::RANGE),
        capability_bytes,
    ) {
        Ok(range) => range,
        Err(()) => {
            return tauri::http::Response::builder()
                .status(416)
                .header(
                    tauri::http::header::CONTENT_RANGE,
                    format!("bytes */{capability_bytes}"),
                )
                .body(Vec::new())
                .unwrap();
        }
    };
    let length = usize::try_from(end - start + 1).unwrap_or(0);
    let mut bytes = Vec::new();
    if request.method() == tauri::http::Method::GET {
        bytes.resize(length, 0);
        if read_range(&capability_path, start, &mut bytes).is_err() {
            return tauri::http::Response::builder()
                .status(404)
                .body(Vec::new())
                .unwrap();
        }
    }
    let active = tauri::async_runtime::block_on(state.session.lock());
    if state.app_locked.load(Ordering::Acquire)
        || !active
            .as_ref()
            .is_some_and(|session| Arc::ptr_eq(session, &capability_session))
    {
        return tauri::http::Response::builder()
            .status(403)
            .body(Vec::new())
            .unwrap();
    }
    let partial = had_range || start != 0 || end + 1 != capability_bytes;
    let mut response = tauri::http::Response::builder()
        .status(if partial { 206 } else { 200 })
        .header(tauri::http::header::CONTENT_TYPE, &capability_media_type)
        .header(tauri::http::header::ACCEPT_RANGES, "bytes")
        .header(tauri::http::header::CACHE_CONTROL, "no-store")
        .header("X-Content-Type-Options", "nosniff")
        .header(
            tauri::http::header::CONTENT_LENGTH,
            if request.method() == tauri::http::Method::HEAD {
                length
            } else {
                bytes.len()
            },
        );
    if partial {
        response = response.header(
            tauri::http::header::CONTENT_RANGE,
            format!("bytes {start}-{end}/{capability_bytes}"),
        );
    }
    response.body(bytes).unwrap()
}

#[cfg(test)]
mod media_protocol_tests {
    use super::{
        media_protocol_response_for_state, media_protocol_response_with_reader, media_range,
        release_media_capabilities, store_media_capability, take_window_media_capabilities,
        AppState, MediaReadBudget, MAX_MEDIA_RANGE_BYTES, MAX_OPEN_MEDIA_CAPABILITIES,
    };
    use crate::{secure_store::MemoryStore, tests};
    use peppy_client_core::AttachmentId;
    use std::{
        fs,
        sync::{mpsc, Arc},
        time::Duration,
    };
    use tauri::http::{HeaderValue, Method, Request, StatusCode};

    struct ProtocolHarness {
        _fixture: tests::Fixture,
        state: Arc<AppState>,
        session: Arc<crate::session::Session>,
        attachment: AttachmentId,
        token: String,
    }

    impl ProtocolHarness {
        fn new(bytes: &[u8]) -> Self {
            let fixture = tests::fixture_at("http://127.0.0.1:9");
            let session = Arc::new(tests::open(&fixture, &fixture.binding, &[]));
            session
                .client
                .unlock(&fixture.profile, &fixture.header, tests::PHRASE)
                .unwrap();
            let source = fixture.dir.path().join("protocol-media.png");
            fs::write(&source, bytes).unwrap();
            let info = session
                .client
                .prepare_attachment(&source, "image/png", "protocol-media.png")
                .unwrap();
            session
                .client
                .retain_attachment_ciphertext(info.attachment_id)
                .unwrap();
            let plaintext = session
                .client
                .open_native_plaintext(info.attachment_id)
                .unwrap();
            let state = Arc::new(AppState::new(
                fixture.dir.path().to_path_buf(),
                Arc::new(MemoryStore::default()),
                tests::noop(),
            ));
            state
                .app_locked
                .store(false, std::sync::atomic::Ordering::Release);
            tauri::async_runtime::block_on(async {
                *state.session.lock().await = Some(session.clone());
            });
            let (view, evicted) =
                store_media_capability(&state, session.clone(), "main", info.clone(), plaintext)
                    .unwrap();
            assert!(evicted.is_empty());
            let token = super::media_capability_token(view.url.as_deref().unwrap())
                .unwrap()
                .to_owned();
            Self {
                _fixture: fixture,
                state,
                session,
                attachment: info.attachment_id,
                token,
            }
        }

        fn request(&self, method: Method, range: Option<&str>) -> Request<Vec<u8>> {
            let mut request = Request::builder()
                .method(method)
                .uri(format!("/{}", self.token));
            if let Some(range) = range {
                request = request.header(tauri::http::header::RANGE, range);
            }
            request.body(Vec::new()).unwrap()
        }

        fn respond(&self, method: Method, range: Option<&str>) -> tauri::http::Response<Vec<u8>> {
            let admission = self
                .state
                .media_read_budget
                .try_acquire()
                .expect("request admitted");
            media_protocol_response_for_state(
                &self.state,
                "main",
                self.request(method, range),
                admission,
            )
        }

        fn add_capability(
            &self,
            name: &str,
            bytes: &[u8],
            window_label: &str,
        ) -> (
            AttachmentId,
            String,
            Vec<(Arc<crate::session::Session>, AttachmentId)>,
        ) {
            let source = self._fixture.dir.path().join(name);
            fs::write(&source, bytes).unwrap();
            let info = self
                .session
                .client
                .prepare_attachment(&source, "image/png", name)
                .unwrap();
            self.session
                .client
                .retain_attachment_ciphertext(info.attachment_id)
                .unwrap();
            let plaintext = self
                .session
                .client
                .open_native_plaintext(info.attachment_id)
                .unwrap();
            let attachment = info.attachment_id;
            let (view, evicted) = store_media_capability(
                &self.state,
                self.session.clone(),
                window_label,
                info,
                plaintext,
            )
            .unwrap();
            let token = super::media_capability_token(view.url.as_deref().unwrap())
                .unwrap()
                .to_owned();
            (attachment, token, evicted)
        }
    }

    #[test]
    fn valid_get_completes_without_relocking_its_session_guard() {
        let harness = ProtocolHarness::new(b"0123456789");
        let state = harness.state.clone();
        let request = harness.request(Method::GET, None);
        let admission = state.media_read_budget.try_acquire().unwrap();
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            sender
                .send(media_protocol_response_for_state(
                    &state, "main", request, admission,
                ))
                .unwrap();
        });

        let response = receiver
            .recv_timeout(Duration::from_secs(2))
            .expect("valid GET deadlocked while reacquiring the session mutex");
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.body(), b"0123456789");
    }

    #[test]
    fn get_and_head_return_expected_bodies_and_security_headers() {
        let harness = ProtocolHarness::new(b"0123456789");

        let get = harness.respond(Method::GET, None);
        assert_eq!(get.status(), StatusCode::OK);
        assert_eq!(get.body(), b"0123456789");
        assert_eq!(
            get.headers()[tauri::http::header::CONTENT_TYPE],
            "image/png"
        );
        assert_eq!(get.headers()[tauri::http::header::CONTENT_LENGTH], "10");
        assert_eq!(get.headers()[tauri::http::header::ACCEPT_RANGES], "bytes");
        assert_eq!(
            get.headers()[tauri::http::header::CACHE_CONTROL],
            "no-store"
        );
        assert_eq!(get.headers()["X-Content-Type-Options"], "nosniff");

        let head = harness.respond(Method::HEAD, None);
        assert_eq!(head.status(), StatusCode::OK);
        assert!(head.body().is_empty());
        assert_eq!(head.headers()[tauri::http::header::CONTENT_LENGTH], "10");
        assert_eq!(
            head.headers()[tauri::http::header::CONTENT_TYPE],
            "image/png"
        );
    }

    #[test]
    fn valid_and_invalid_ranges_return_206_and_416() {
        let harness = ProtocolHarness::new(b"0123456789");

        let partial = harness.respond(Method::GET, Some("bytes=2-5"));
        assert_eq!(partial.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(partial.body(), b"2345");
        assert_eq!(partial.headers()[tauri::http::header::CONTENT_LENGTH], "4");
        assert_eq!(
            partial.headers()[tauri::http::header::CONTENT_RANGE],
            "bytes 2-5/10"
        );

        let invalid = harness.respond(Method::GET, Some("bytes=10-11"));
        assert_eq!(invalid.status(), StatusCode::RANGE_NOT_SATISFIABLE);
        assert!(invalid.body().is_empty());
        assert_eq!(
            invalid.headers()[tauri::http::header::CONTENT_RANGE],
            "bytes */10"
        );
    }

    #[test]
    fn token_window_method_lock_and_session_checks_fail_closed() {
        let harness = ProtocolHarness::new(b"0123456789");
        let respond = |path: &str, window: &str, method: Method| {
            let request = Request::builder()
                .method(method)
                .uri(path)
                .body(Vec::new())
                .unwrap();
            let admission = harness.state.media_read_budget.try_acquire().unwrap();
            media_protocol_response_for_state(&harness.state, window, request, admission)
        };

        assert_eq!(respond("/wrong-token", "main", Method::GET).status(), 404);
        assert_eq!(
            respond(&format!("/{}", harness.token), "other", Method::GET).status(),
            403
        );
        assert_eq!(
            respond(&format!("/{}", harness.token), "main", Method::POST).status(),
            403
        );

        harness
            .state
            .app_locked
            .store(true, std::sync::atomic::Ordering::Release);
        assert_eq!(
            respond(&format!("/{}", harness.token), "main", Method::GET).status(),
            403
        );
        harness
            .state
            .app_locked
            .store(false, std::sync::atomic::Ordering::Release);

        let replacement = Arc::new(tests::open(
            &harness._fixture,
            &harness._fixture.binding,
            &[],
        ));
        tauri::async_runtime::block_on(async {
            *harness.state.session.lock().await = Some(replacement);
        });
        assert_eq!(
            respond(&format!("/{}", harness.token), "main", Method::GET).status(),
            403
        );
    }

    #[test]
    fn a_contended_session_lock_waits_instead_of_returning_busy_503() {
        let harness = ProtocolHarness::new(b"0123456789");
        let state = harness.state.clone();
        let request = harness.request(Method::GET, None);
        let admission = state.media_read_budget.try_acquire().unwrap();
        let guard = tauri::async_runtime::block_on(state.session.lock());
        let (sender, receiver) = mpsc::channel();
        let request_state = state.clone();
        std::thread::spawn(move || {
            sender
                .send(media_protocol_response_for_state(
                    &request_state,
                    "main",
                    request,
                    admission,
                ))
                .unwrap();
        });
        drop(guard);

        let response = receiver
            .recv_timeout(Duration::from_secs(2))
            .expect("handler did not resume after the session lock was released");
        assert_eq!(response.status(), StatusCode::OK);
        assert_ne!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[test]
    fn session_replacement_during_read_is_rejected_after_io() {
        let harness = ProtocolHarness::new(b"0123456789");
        let state = harness.state.clone();
        let request = harness.request(Method::GET, None);
        let admission = state.media_read_budget.try_acquire().unwrap();
        let (read_started_sender, read_started_receiver) = mpsc::channel();
        let (continue_sender, continue_receiver) = mpsc::channel();
        let (response_sender, response_receiver) = mpsc::channel();
        std::thread::spawn(move || {
            let response = media_protocol_response_with_reader(
                &state,
                "main",
                request,
                admission,
                move |_path, _start, bytes| {
                    read_started_sender.send(()).unwrap();
                    continue_receiver.recv().unwrap();
                    bytes.copy_from_slice(b"0123456789");
                    Ok(())
                },
            );
            response_sender.send(response).unwrap();
        });
        read_started_receiver
            .recv_timeout(Duration::from_secs(2))
            .expect("read hook was not reached");

        let replacement = Arc::new(tests::open(
            &harness._fixture,
            &harness._fixture.binding,
            &[],
        ));
        tauri::async_runtime::block_on(async {
            *harness.state.session.lock().await = Some(replacement);
        });
        continue_sender.send(()).unwrap();

        let response = response_receiver
            .recv_timeout(Duration::from_secs(2))
            .expect("post-read session validation did not complete");
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert!(response.body().is_empty());
    }

    #[test]
    fn capacity_eviction_and_window_teardown_release_core_leases() {
        let harness = ProtocolHarness::new(b"first");
        let (window_attachment, window_token, evicted) =
            harness.add_capability("window.png", b"window", "composer-test");
        assert!(evicted.is_empty());

        let removed = take_window_media_capabilities(&harness.state, "composer-test");
        assert_eq!(removed.len(), 1);
        tauri::async_runtime::block_on(release_media_capabilities(&harness.state, removed));
        assert!(!harness
            .session
            .client
            .release_attachment_ciphertext(window_attachment)
            .unwrap());
        assert!(!harness
            .state
            .media_capabilities
            .lock()
            .unwrap()
            .contains_key(&window_token));

        let mut evicted = Vec::new();
        let mut latest = harness.attachment;
        for index in 1..=MAX_OPEN_MEDIA_CAPABILITIES {
            let (attachment, _token, released) =
                harness.add_capability(&format!("capacity-{index}.png"), &[index as u8], "main");
            latest = attachment;
            evicted.extend(released);
        }
        assert!(
            !evicted.is_empty(),
            "capacity must evict at least one token"
        );
        let evicted_ids: Vec<_> = evicted.iter().map(|(_, id)| *id).collect();
        tauri::async_runtime::block_on(release_media_capabilities(&harness.state, evicted));
        for attachment in evicted_ids {
            assert!(!harness
                .session
                .client
                .release_attachment_ciphertext(attachment)
                .unwrap());
        }
        assert!(harness
            .session
            .client
            .release_attachment_ciphertext(latest)
            .unwrap());
    }

    #[test]
    fn window_teardown_keeps_media_lease_while_update_restart_is_reserved() {
        let harness = ProtocolHarness::new(b"reserved");
        let reservation =
            tauri::async_runtime::block_on(harness.state.update_safety.reserve_async()).unwrap();

        tauri::async_runtime::block_on(release_media_capabilities(
            &harness.state,
            vec![(harness.session.clone(), harness.attachment)],
        ));
        assert!(harness
            .session
            .client
            .release_attachment_ciphertext(harness.attachment)
            .unwrap());

        harness
            .session
            .client
            .retain_attachment_ciphertext(harness.attachment)
            .unwrap();
        drop(reservation);
        tauri::async_runtime::block_on(release_media_capabilities(
            &harness.state,
            vec![(harness.session.clone(), harness.attachment)],
        ));
        assert!(!harness
            .session
            .client
            .release_attachment_ciphertext(harness.attachment)
            .unwrap());
    }

    #[test]
    fn accepts_video_ranges_and_rejects_ambiguous_or_invalid_ones() {
        assert_eq!(media_range(None, 10).unwrap(), (0, 9));
        assert_eq!(
            media_range(Some(&HeaderValue::from_static("bytes=2-5")), 10).unwrap(),
            (2, 5)
        );
        assert_eq!(
            media_range(Some(&HeaderValue::from_static("bytes=7-")), 10).unwrap(),
            (7, 9)
        );
        assert_eq!(
            media_range(Some(&HeaderValue::from_static("bytes=-3")), 10).unwrap(),
            (7, 9)
        );
        assert_eq!(
            media_range(Some(&HeaderValue::from_static("bytes=7-100")), 10).unwrap(),
            (7, 9)
        );
        assert!(media_range(Some(&HeaderValue::from_static("bytes=0-1,3-4")), 10).is_err());
        assert!(media_range(Some(&HeaderValue::from_static("bytes=10-11")), 10).is_err());
        let oversized = format!("bytes=0-{}", MAX_MEDIA_RANGE_BYTES);
        assert_eq!(
            media_range(
                Some(&HeaderValue::from_str(&oversized).unwrap()),
                MAX_MEDIA_RANGE_BYTES + 1
            )
            .unwrap(),
            (0, MAX_MEDIA_RANGE_BYTES - 1)
        );
    }

    #[test]
    fn protocol_admission_is_bounded_before_request_threads_are_created() {
        let budget = Arc::new(MediaReadBudget::new(1));
        let first = budget.try_acquire().expect("first request admitted");
        assert!(budget.try_acquire().is_none());
        drop(first);
        assert!(budget.try_acquire().is_some());
    }
}

#[derive(serde::Deserialize)]
struct PublicCopyResponse {
    token: String,
    safe_name: String,
    #[serde(default)]
    expires_in_seconds: Option<u64>,
}

#[tauri::command]
async fn publish_attachment(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    id: String,
) -> BridgeResult<Option<PublicCopyView>> {
    let operation = admit_update_operation(&state)?;
    require_main(window.label())?;
    let session = state.require_session_admitted(&operation).await?;
    let attachment = AttachmentId::from_str(&id)
        .map_err(|_| BridgeError::new("invalid-attachment", "The attachment ID is invalid."))?;
    let s = session.clone();
    let info = blocking(move || s.client.attachment_info(attachment).map_err(core_error)).await?;
    if !media::is_previewable(&info.media_type) || !info.state.is_local() {
        return Err(BridgeError::new(
            "public-copy-unsupported",
            "Only images available on this device can be shared as a public copy.",
        ));
    }
    let remote = session
        .remote_ids
        .lock()
        .map_err(|_| BridgeError::host_state())?
        .get(attachment)
        .ok_or_else(|| {
            BridgeError::new(
                "public-copy-unavailable",
                "This image's server object is not known on this device yet; wait until it has finished sending or downloading.",
            )
        })?;
    // Decode/re-encode locally first so the confirmation names exactly what would be exposed.
    let prepared = prepare_public_copy(&session, attachment, &info.display_name).await?;
    let confirmed = dialogs::confirm(
        &app,
        "Create a public copy?",
        &public_copy_prompt(&info.display_name, &prepared),
        "Create public copy",
    )
    .await?;
    if !confirmed {
        return Ok(None);
    }
    let view = upload_public_copy(&session, &remote, prepared).await?;
    // The URL is shown to the person and returned to the UI; it is never logged.
    dialogs::inform(
        &app,
        "Public copy created",
        &format!("Anyone with this link can view the copy:\n{}", view.url),
    );
    Ok(Some(view))
}

pub struct PreparedPublicCopy {
    bytes: Vec<u8>,
    name: String,
    width: u32,
    height: u32,
}

/// Native confirmation text naming the sanitized image and the derivative that would be public.
pub fn public_copy_prompt(display_name: &str, prepared: &PreparedPublicCopy) -> String {
    let shown: String = display_name
        .chars()
        .filter(|c| !c.is_control())
        .take(80)
        .collect();
    format!(
        "Share \"{shown}\" publicly?\n\nPeppy will upload a SEPARATE, server-readable copy named \"{}\" ({}×{} px, {} KiB, re-encoded with metadata removed). Anyone with the link can view it until it expires or is revoked. The private encrypted original is not changed.",
        prepared.name,
        prepared.width,
        prepared.height,
        prepared.bytes.len().div_ceil(1024)
    )
}

/// Re-encodes the verified local plaintext into a metadata-free derivative (no network).
pub(crate) async fn prepare_public_copy(
    session: &Arc<Session>,
    attachment: AttachmentId,
    display_name: &str,
) -> BridgeResult<PreparedPublicCopy> {
    let s = session.clone();
    let public = blocking(move || {
        let file = s
            .client
            .open_native_plaintext(attachment)
            .map_err(core_error)?;
        let bytes = media::read_capped(file.path()).ok_or_else(|| {
            BridgeError::new(
                "public-copy-too-large",
                "The image is too large for a public copy.",
            )
        })?;
        drop(file);
        media::reencode_public(&bytes)
    })
    .await?;
    Ok(PreparedPublicCopy {
        name: media::public_name(display_name, public.extension),
        width: public.width,
        height: public.height,
        bytes: public.bytes,
    })
}

/// Uploads a prepared derivative. Callers must already hold explicit native user confirmation.
pub(crate) async fn upload_public_copy(
    session: &Arc<Session>,
    remote: &str,
    prepared: PreparedPublicCopy,
) -> BridgeResult<PublicCopyView> {
    let response: PublicCopyResponse = session
        .api
        .post_public_copy(
            &format!("/v1/attachments/{remote}/public-copies"),
            &prepared.name,
            prepared.bytes,
        )
        .await?;
    let safe = |value: &str, extra: &[char]| {
        !value.is_empty()
            && value.len() <= 128
            && value
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || extra.contains(&c))
    };
    if !safe(&response.token, &['-', '_']) || !safe(&response.safe_name, &['-', '_', '.']) {
        return Err(net::NetError::Invalid.into());
    }
    let url = format!(
        "{}/file/mms-usercontent/{}/{}",
        session.api.origin(),
        response.token,
        response.safe_name
    );
    Ok(PublicCopyView {
        url,
        expires_in_seconds: response.expires_in_seconds.unwrap_or(0),
    })
}

/// Test helper: prepare + upload (confirmation is the caller's responsibility).
#[cfg(test)]
async fn create_public_copy(
    session: &Arc<Session>,
    attachment: AttachmentId,
    display_name: &str,
    remote: &str,
) -> BridgeResult<PublicCopyView> {
    let prepared = prepare_public_copy(session, attachment, display_name).await?;
    upload_public_copy(session, remote, prepared).await
}

async fn new_composer(app: tauri::AppHandle, conversation: Option<String>) -> BridgeResult<()> {
    let operation = admit_update_operation(&app.state::<AppState>())?;
    let _window_permit = lifecycle::permit_window_creation(&app)?;
    let state = app.state::<AppState>();
    let conversation = match conversation {
        Some(id) => ConversationId::from_str(&id).map_err(|_| {
            BridgeError::new("invalid-conversation", "The conversation ID is invalid.")
        })?,
        None => {
            let session = state.require_session_admitted(&operation).await?;
            blocking(move || {
                session
                    .client
                    .recipientless_compose_draft()
                    .map_err(core_error)
            })
            .await?
            .conversation_id
        }
    };
    tray::open_composer(&app, conversation)
}

/// Explicit user action only (main window or tray). `None` starts a new conversation draft.
#[tauri::command]
async fn open_composer(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
    conversation_id: Option<String>,
) -> BridgeResult<()> {
    let _operation = admit_update_operation(&app.state::<AppState>())?;
    require_main(window.label())?;
    new_composer(app, conversation_id).await
}

/// Requests a dismissal effect from core. The phone applies it when it next synchronizes; this
/// command never claims that its Android notification-center item was already removed.
#[tauri::command]
async fn dismiss_notification(
    window: tauri::WebviewWindow,
    state: State<'_, AppState>,
    target: NotificationTarget,
) -> BridgeResult<()> {
    let operation = admit_update_operation(&state)?;
    require_main(window.label())?;
    let session = state.require_session_admitted(&operation).await?;
    let s = session.clone();
    blocking(move || s.client.dismiss_notification(target).map_err(core_error)).await?;
    session.notify();
    Ok(())
}

/// Queues at most 100 durable effects through the shared core eligibility policy.
#[tauri::command]
async fn dismiss_all_notifications(
    window: tauri::WebviewWindow,
    state: State<'_, AppState>,
) -> BridgeResult<()> {
    let operation = admit_update_operation(&state)?;
    require_main(window.label())?;
    let session = state.require_session_admitted(&operation).await?;
    let s = session.clone();
    blocking(move || s.client.dismiss_all_notifications().map_err(core_error)).await?;
    session.notify();
    Ok(())
}

#[tauri::command]
async fn set_app_muted(
    window: tauri::WebviewWindow,
    state: State<'_, AppState>,
    source_device_id: String,
    package_name: String,
    app_name: String,
    muted: bool,
) -> BridgeResult<()> {
    let operation = admit_update_operation(&state)?;
    require_main(window.label())?;
    let session = state.require_session_admitted(&operation).await?;
    let s = session.clone();
    blocking(move || {
        s.client
            .set_app_muted(&source_device_id, &package_name, &app_name, muted)
            .map_err(core_error)
    })
    .await?;
    session.notify();
    Ok(())
}

#[tauri::command]
async fn set_conversation_pinned(
    window: tauri::WebviewWindow,
    state: State<'_, AppState>,
    conversation_id: String,
    pinned: bool,
) -> BridgeResult<()> {
    let operation = admit_update_operation(&state)?;
    require_main(window.label())?;
    let conversation_id = ConversationId::from_str(&conversation_id)
        .map_err(|_| BridgeError::new("invalid-request", "The conversation ID is invalid."))?;
    let session = state.require_session_admitted(&operation).await?;
    let client = session.client.clone();
    blocking(move || {
        client
            .set_conversation_pinned(conversation_id, pinned)
            .map_err(core_error)
    })
    .await?;
    session.notify();
    Ok(())
}

#[tauri::command]
async fn set_conversation_tone(
    window: tauri::WebviewWindow,
    state: State<'_, AppState>,
    seed: String,
    palette_index: Option<u8>,
) -> BridgeResult<()> {
    let operation = admit_update_operation(&state)?;
    require_main(window.label())?;
    let session = state.require_session_admitted(&operation).await?;
    let client = session.client.clone();
    blocking(move || {
        client
            .set_conversation_tone(&seed, palette_index)
            .map_err(core_error)
    })
    .await?;
    session.notify();
    Ok(())
}

#[tauri::command]
async fn mark_notifications_seen(
    window: tauri::WebviewWindow,
    state: State<'_, AppState>,
    targets: Vec<NotificationTarget>,
) -> BridgeResult<()> {
    let operation = admit_update_operation(&state)?;
    require_main(window.label())?;
    let session = state.require_session_admitted(&operation).await?;
    blocking(move || {
        session
            .client
            .mark_notifications_seen(targets)
            .map_err(core_error)
    })
    .await?;
    Ok(())
}

#[tauri::command]
fn set_notification_preferences(
    window: tauri::WebviewWindow,
    state: State<'_, AppState>,
    preferences: NotificationPreferences,
) -> BridgeResult<()> {
    let _operation = admit_update_operation(&state)?;
    require_main(window.label())?;
    state.notifications.set_preferences(preferences)?;
    (state.notifier)();
    Ok(())
}

/// Main-window context is a routing hint for native banner suppression. Composer windows never
/// overwrite it, and focus is checked by the native backend before any future banner post.
#[tauri::command]
fn set_notification_context(
    window: tauri::WebviewWindow,
    state: State<'_, AppState>,
    view: NotificationView,
    conversation_id: Option<String>,
) -> BridgeResult<()> {
    let _operation = admit_update_operation(&state)?;
    require_main(window.label())?;
    state.notifications.set_context(view, conversation_id);
    Ok(())
}

/// Reads OS notification authorization and posts a fixed, content-free test banner unless the OS
/// reports Peppy as blocked.
#[tauri::command]
async fn request_notification_permission(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
) -> BridgeResult<notification_permission::NotificationCheck> {
    // Admit then release at once: the guard must not be held across the up-to-60s OS prompt.
    drop(admit_update_operation(&app.state::<AppState>())?);
    require_main(window.label())?;
    blocking(move || Ok(notification_permission::check(&app))).await
}

/// Opens Peppy's page in the OS notification settings. Main window only.
#[tauri::command]
fn open_notification_settings(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
) -> BridgeResult<()> {
    let _operation = admit_update_operation(&app.state::<AppState>())?;
    require_main(window.label())?;
    let url = notification_permission::current_settings_url(&app).ok_or_else(|| {
        BridgeError::new(
            "unsupported",
            "Notification settings are unavailable on this platform.",
        )
    })?;
    app.opener()
        .open_url(url, None::<String>)
        .map_err(|_| BridgeError::new("open-failed", "Could not open notification settings."))
}

/// OS registration is authoritative. This command is main-window-only and no
/// registration is attempted during setup or tests.
#[tauri::command]
fn set_start_at_login(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
    enabled: bool,
) -> BridgeResult<()> {
    let _operation = admit_update_operation(&app.state::<AppState>())?;
    require_main(window.label())?;
    let launcher = app.autolaunch();
    let result = if enabled {
        launcher.enable()
    } else {
        launcher.disable()
    };
    result.map_err(|_| BridgeError::new("start-at-login", "Could not update Start at login."))
}

/// Explicit user action only. Native head rendering failure leaves the normal
/// composer reachable; this command owns that fallback to prevent duplicate UI.
#[tauri::command]
async fn popout_conversation(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    conversation_id: String,
) -> BridgeResult<heads_runtime::PopoutResult> {
    let operation = admit_update_operation(&state)?;
    require_main(window.label())?;
    let _window_permit = lifecycle::permit_window_creation(&app)?;
    let conversation = ConversationId::from_str(&conversation_id)
        .map_err(|_| BridgeError::new("invalid-conversation", "The conversation ID is invalid."))?;
    let _ = state.require_session_admitted(&operation).await?;
    heads_runtime::popout(&app, conversation).await
}

#[tauri::command]
async fn hide_head(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
    conversation_id: String,
) -> BridgeResult<()> {
    let _operation = admit_update_operation(&app.state::<AppState>())?;
    require_main(window.label())?;
    let _conversation = ConversationId::from_str(&conversation_id)
        .map_err(|_| BridgeError::new("invalid-conversation", "The conversation ID is invalid."))?;
    heads_runtime::dismiss(&app, &conversation_id).await
}

#[tauri::command]
fn acknowledge_lifecycle(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
    id: String,
    ok: bool,
) -> BridgeResult<()> {
    lifecycle::acknowledge(&app, window.label(), &id, ok)
}

#[tauri::command]
fn close_composer(window: tauri::WebviewWindow) -> BridgeResult<()> {
    if tray::composer_conversation(window.label()).is_none() {
        return Err(BridgeError::new(
            "composer-context",
            "This command is only available in a composer window.",
        ));
    }
    let action = if heads_runtime::is_panel(window.app_handle(), window.label()) {
        lifecycle::Action::Collapse
    } else {
        lifecycle::Action::Close
    };
    lifecycle::request_window(window.app_handle(), window.label(), action)
}

/// Closes (rather than collapses) the floating conversation associated with
/// this composer window. The label is the authority for both conversation and
/// panel ownership; callers never supply an ID.
#[tauri::command]
async fn close_head_panel(window: tauri::WebviewWindow, app: tauri::AppHandle) -> BridgeResult<()> {
    let _operation = admit_update_operation(&app.state::<AppState>())?;
    let conversation = tray::composer_conversation(window.label()).ok_or_else(|| {
        BridgeError::new(
            "composer-context",
            "This command is only available in a composer window.",
        )
    })?;
    if !heads_runtime::is_panel(&app, window.label()) {
        return Err(BridgeError::new(
            "composer-context",
            "This command is only available in a floating conversation panel.",
        ));
    }
    let generation = heads_runtime::head_generation(&app, &conversation.to_string())?;
    let _window_permit = lifecycle::permit_window_creation(&app)?;
    lifecycle::request_window_and_wait(&app, window.label(), lifecycle::Action::Close).await?;
    heads_runtime::dismiss_generation(&app, &conversation.to_string(), generation).await
}

#[tauri::command]
fn update_status(state: State<'_, AppState>) -> updates::types::UpdateStatus {
    state.updates.status()
}

#[tauri::command]
async fn check_for_updates(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> BridgeResult<updates::types::UpdateStatus> {
    require_main(window.label())?;
    state.updates.check(&app).await
}

#[tauri::command]
fn set_automatic_updates(
    window: tauri::WebviewWindow,
    state: State<'_, AppState>,
    enabled: bool,
) -> BridgeResult<updates::types::UpdateStatus> {
    require_main(window.label())?;
    state.updates.set_automatic(enabled)
}

#[tauri::command]
fn defer_update(
    window: tauri::WebviewWindow,
    state: State<'_, AppState>,
) -> BridgeResult<updates::types::UpdateStatus> {
    require_main(window.label())?;
    state.updates.defer()
}

#[tauri::command]
fn report_update_activity(window: tauri::WebviewWindow, state: State<'_, AppState>, active: bool) {
    state.updates.report_activity(window.label(), active);
}

#[tauri::command]
async fn restart_for_update(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> BridgeResult<updates::types::UpdateStatus> {
    require_main(window.label())?;
    if let Some(version) = state.updates.installed_pending() {
        state.updates.clear_attempted()?;
        let never = restart_into_installed(
            &app,
            &state,
            &version,
            restart_in_background(main_window_visible(&app)),
            true,
        )
        .await?;
        match never {}
    }
    install_staged_update(&app, &state, updates::install::InstallIntent::User).await
}

async fn install_staged_update(
    app: &tauri::AppHandle,
    state: &AppState,
    intent: updates::install::InstallIntent,
) -> BridgeResult<updates::types::UpdateStatus> {
    // The updater plugin has no safe staged-payload handoff in the base build yet.
    // Refuse before entering lifecycle exclusion rather than exit with an unchecked payload.
    let status = state.updates.status();
    if !status.can_restart {
        return Err(BridgeError::new(
            "update-not-ready",
            "No verified update is ready to restart.",
        ));
    }
    let Some(staged) = state.updates.staged() else {
        return Err(BridgeError::new(
            "update-not-ready",
            "No verified update payload is staged.",
        ));
    };
    if let Err(error) = updates::install::preflight(&status.package_kind, intent) {
        if error.code == "update-authorization-required" {
            state.updates.set_authorization_required();
            state.updates.emit(app);
        }
        return Err(error);
    }
    let bytes = staged.verified_bytes()?;
    let permit =
        lifecycle::prepare_update(app, intent == updates::install::InstallIntent::User).await?;
    state.updates.set_installing(&staged.update.version)?;
    state.updates.emit(app);
    if !sync::drain_all(std::time::Duration::from_secs(30)).await {
        // Keep the reservation until every active and retired supervisor has completed.
        // Releasing it earlier would permit a new session while old storage work remains live.
        let app = app.clone();
        tauri::async_runtime::spawn(wait_for_background_work(
            app,
            permit,
            "Waiting for background work before restarting the update.",
            "Background work finished. Restart the verified update when ready.",
        ));
        return Err(BridgeError::new(
            "update-busy",
            "Peppy is still finishing background work.",
        ));
    }
    if let Err(error) = updates::install::write_restart_marker(
        &state.root,
        &staged.update.version,
        restart_in_background(main_window_visible(app)),
    ) {
        state
            .updates
            .install_failed("Could not record the update restart state.");
        state.updates.emit(app);
        return Err(error);
    }
    state
        .updates
        .record_install_attempt(&staged.update.version)?;
    let install_guard = permit.commit();
    #[cfg(target_os = "linux")]
    if matches!(status.package_kind.as_str(), "deb" | "rpm") {
        return install_linux_system_package(app, state, staged, install_guard, intent).await;
    }
    // `install` verifies the bytes again. On Windows it exits only after the updater launches;
    // on other supported bundles it replaces the payload before the immediate relaunch.
    if staged.update.install(bytes).is_err() {
        updates::install::clear_restart_marker(&state.root);
        state
            .updates
            .install_failed("Could not install the verified update.");
        state.updates.emit(app);
        return Err(BridgeError::new(
            "update-install",
            "Could not install the verified update.",
        ));
    }
    // Do not release the update reservation or tell windows that draft editing is safe until the
    // installer has completed its recoverable work. A failed installation drops `install_guard`,
    // which restores the captured UI and admission gates.
    install_guard.finish();
    state.allow_exit.store(true, Ordering::Release);
    app.restart();
}

async fn wait_for_background_work(
    app: tauri::AppHandle,
    permit: lifecycle::UpdatePermit,
    waiting_message: &'static str,
    final_message: &'static str,
) {
    let state = app.state::<AppState>();
    loop {
        state.updates.set_installing_message(waiting_message);
        state.updates.emit(&app);
        if sync::drain_all(std::time::Duration::from_secs(30)).await {
            state.updates.install_failed(final_message);
            state.updates.emit(&app);
            drop(permit);
            break;
        }
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
    }
}

#[cfg(target_os = "linux")]
async fn install_linux_system_package(
    app: &tauri::AppHandle,
    state: &AppState,
    staged: updates::StagedUpdate,
    install_guard: lifecycle::UpdateInstallGuard,
    intent: updates::install::InstallIntent,
) -> BridgeResult<updates::types::UpdateStatus> {
    use update_linux::{InstallOutcome, LinuxPackageInstaller, ZbusPackageKit};

    let expected_version = staged.update.version.clone();
    let backend = match ZbusPackageKit::connect().await {
        Ok(backend) => backend,
        Err(_) => {
            updates::install::clear_restart_marker(&state.root);
            state
                .updates
                .install_failed("PackageKit is unavailable on this Linux installation.");
            state.updates.emit(app);
            return Err(BridgeError::new(
                "update-packagekit",
                "PackageKit is unavailable on this Linux installation.",
            ));
        }
    };
    let installer = LinuxPackageInstaller::new(backend);
    match installer
        .install_verified(
            &staged.path,
            &expected_version,
            intent == updates::install::InstallIntent::User,
        )
        .await
    {
        InstallOutcome::Success { .. } => {
            if let Err(error) = state.updates.finish_external_update(&expected_version) {
                updates::install::clear_restart_marker(&state.root);
                state
                    .updates
                    .install_failed("Could not finalize the installed system update.");
                state.updates.emit(app);
                return Err(error);
            }
            install_guard.finish();
            state.allow_exit.store(true, Ordering::Release);
            app.restart();
        }
        InstallOutcome::CompletionUnknown { message: _ } => {
            let reconcile_app = app.clone();
            tauri::async_runtime::spawn(async move {
                // Keep this installer (and its private PackageKit transaction identity) alive.
                // Installed-version metadata alone cannot release the lifecycle fence.
                loop {
                    tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                    match installer.reconcile_pending(&expected_version).await {
                        Some(InstallOutcome::Success { .. }) => {
                            let state = reconcile_app.state::<AppState>();
                            if state
                                .updates
                                .finish_external_update(&expected_version)
                                .is_err()
                            {
                                updates::install::clear_restart_marker(&state.root);
                                state.updates.install_failed(
                                    "Could not finalize the installed system update.",
                                );
                                state.updates.emit(&reconcile_app);
                            } else {
                                install_guard.finish();
                                state.allow_exit.store(true, Ordering::Release);
                                reconcile_app.restart();
                            }
                            break;
                        }
                        Some(InstallOutcome::CompletionUnknown { .. }) | None => continue,
                        Some(outcome) => {
                            let state = reconcile_app.state::<AppState>();
                            updates::install::clear_restart_marker(&state.root);
                            state.updates.install_failed(&packagekit_message(outcome));
                            state.updates.emit(&reconcile_app);
                            break;
                        }
                    }
                }
            });
            // The lifecycle guard and manager installation claim both remain held
            // until this exact PackageKit transaction reaches a typed terminal state.
            state.updates.emit(app);
            Err(BridgeError::new(
                "update-packagekit-pending",
                "PackageKit completion is still being verified.",
            ))
        }
        outcome => {
            updates::install::clear_restart_marker(&state.root);
            if matches!(outcome, InstallOutcome::AuthorizationRequired) {
                state.updates.set_authorization_required();
                state.updates.emit(app);
                return Err(BridgeError::new(
                    "update-packagekit",
                    "PackageKit authorization is required.",
                ));
            }
            let message = packagekit_message(outcome);
            state.updates.install_failed(&message);
            state.updates.emit(app);
            Err(BridgeError::new("update-packagekit", message))
        }
    }
}

#[cfg(target_os = "linux")]
fn packagekit_message(outcome: update_linux::InstallOutcome) -> String {
    use update_linux::InstallOutcome;

    match outcome {
        InstallOutcome::Unsupported { message } | InstallOutcome::Failed { message } => message,
        InstallOutcome::AuthorizationRequired => "PackageKit authorization is required.".into(),
        InstallOutcome::Cancelled => "PackageKit authorization was cancelled.".into(),
        InstallOutcome::Locked => "PackageKit is busy with another transaction.".into(),
        InstallOutcome::CompletionUnknown { message } => message,
        InstallOutcome::SettledUnknown { message } => message,
        InstallOutcome::Success { .. } => "PackageKit completed unexpectedly.".into(),
    }
}

async fn restart_into_installed(
    app: &tauri::AppHandle,
    state: &AppState,
    version: &str,
    background: bool,
    wait_foroperations: bool,
) -> BridgeResult<std::convert::Infallible> {
    let permit = lifecycle::prepare_update(app, wait_foroperations).await?;
    state.updates.set_restarting_installed(version)?;
    state.updates.emit(app);
    if !sync::drain_all(std::time::Duration::from_secs(30)).await {
        let app = app.clone();
        tauri::async_runtime::spawn(wait_for_background_work(
            app,
            permit,
            "Waiting for background work before restarting the installed update.",
            "Background work finished. Restart the installed update when ready.",
        ));
        return Err(BridgeError::new(
            "update-busy",
            "Peppy is still finishing background work.",
        ));
    }
    if let Err(error) = updates::install::write_restart_marker(&state.root, version, background) {
        state
            .updates
            .install_failed("Could not record the update restart state.");
        state.updates.emit(app);
        return Err(error);
    }
    let guard = permit.commit();
    if let Err(error) = state.updates.finish_external_update(version) {
        updates::install::clear_restart_marker(&state.root);
        state
            .updates
            .install_failed("Could not finalize the installed update.");
        state.updates.emit(app);
        return Err(error);
    }
    guard.finish();
    state.allow_exit.store(true, Ordering::Release);
    app.restart();
}

#[cfg(target_os = "linux")]
async fn restart_after_external_package_update(
    app: &tauri::AppHandle,
    state: &AppState,
) -> BridgeResult<()> {
    use update_linux::{LinuxPackageInstaller, ZbusPackageKit};

    let backend = ZbusPackageKit::connect()
        .await
        .map_err(|_| BridgeError::new("update-packagekit", "PackageKit is unavailable."))?;
    let installer = LinuxPackageInstaller::new(backend);
    let Some(installed) = installer.installed_version().await else {
        return Ok(());
    };
    if !installer.daemon_is_idle().await || !state.updates.should_restart_external(app, &installed)
    {
        return Ok(());
    }
    restart_into_installed(
        app,
        state,
        &installed,
        restart_in_background(main_window_visible(app)),
        false,
    )
    .await
    .map(|never| match never {})
}

fn activate_main(app: &tauri::AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        tray::show_main(&app);
        #[cfg(target_os = "macos")]
        if let Some(state) = app.try_state::<AppState>() {
            let current_version = state.updates.status().current_version;
            if let updates::installed::Installed::Differs(version) = updates::installed::compare(
                &current_version,
                updates::installed::running_bundle_version(&app).as_deref(),
            ) {
                if state.updates.note_installed(&version) {
                    state.updates.emit(&app);
                }
                if state.updates.activation_should_restart(&version) {
                    match restart_into_installed(&app, &state, &version, false, false).await {
                        Ok(never) => match never {},
                        Err(error) => {
                            eprintln!("peppy: installed-update restart failed: {}", error.code)
                        }
                    }
                }
            }
        }
    });
}

fn main_window_visible(app: &tauri::AppHandle) -> bool {
    app.get_webview_window(tray::MAIN)
        .and_then(|window| window.is_visible().ok())
        // An unavailable visibility query must not turn a foreground restart
        // into a hidden launch.
        .unwrap_or(true)
}

fn restart_in_background(main_visible: bool) -> bool {
    !main_visible
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let builder = tauri::Builder::default()
        .register_asynchronous_uri_scheme_protocol("peppy-media", |context, request, responder| {
            let app = context.app_handle().clone();
            let label = context.webview_label().to_owned();
            let state = app.state::<AppState>();
            let Some(admission) = state.media_read_budget.try_acquire() else {
                responder.respond(
                    tauri::http::Response::builder()
                        .status(503)
                        .header(tauri::http::header::RETRY_AFTER, "1")
                        .header(tauri::http::header::CACHE_CONTROL, "no-store")
                        .body(Vec::new())
                        .unwrap(),
                );
                return;
            };
            std::thread::spawn(move || {
                responder.respond(media_protocol_response(app, label, request, admission))
            });
        })
        .on_page_load(|webview, payload| {
            if payload.event() != tauri::webview::PageLoadEvent::Finished {
                return;
            }
            if webview.label() != tray::MAIN
                && tray::composer_conversation(webview.label()).is_none()
            {
                return;
            }
            let head_panel = payload
                .url()
                .query_pairs()
                .any(|(key, value)| key == "head" && value == "1");
            window_chrome::sync_webview(webview, head_panel);
            window_chrome::page_loaded(webview);
        })
        .plugin(
            tauri::plugin::Builder::<tauri::Wry, ()>::new("external-navigation-deny")
                .on_navigation(|webview, url| {
                    allows_app_navigation(url, webview.app_handle().config().build.dev_url.as_ref())
                })
                .build(),
        )
        // Must precede setup: a second manual launch activates the resident
        // main window before it can open another database/session.
        .plugin(tauri_plugin_single_instance::init(|app, args, _| {
            if !startup::background_requested(args) {
                activate_main(app);
            }
        }))
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            Some(vec![startup::BACKGROUND_ARGUMENT]),
        ))
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .setup(|app| {
            let root = app
                .path()
                .app_data_dir()
                .map_err(|_| "native app data unavailable")?;
            std::fs::create_dir_all(&root).map_err(|_| "could not create native app data")?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))
                    .map_err(|_| "could not protect native app data")?;
            }
            let handle = app.handle().clone();
            // State hints only; no payload, no focus or window changes.
            let notifier: Notifier = Arc::new(move || {
                let _ = handle.emit(STATE_EVENT, ());
            });
            let secure_store = KeyringStore::new(app.config().identifier.clone());
            #[cfg(target_os = "macos")]
            app.manage(AppState::new(
                root.clone(),
                Arc::new(BundledStore::new(secure_store, root.join("secrets.lock"))),
                notifier.clone(),
            ));
            #[cfg(not(target_os = "macos"))]
            app.manage(AppState::new(
                root.clone(),
                Arc::new(secure_store),
                notifier,
            ));
            window_placement::install(app.handle(), &root);
            if let Some(main) = app.get_webview_window(tray::MAIN) {
                window_placement::restore_initial(&main);
            }
            let state = app.state::<AppState>();
            state
                .updates
                .initialize(app.handle(), &root)
                .map_err(|error| error.message)?;
            let restart_marker = updates::install::confirm_restart_marker(
                &root,
                &state.updates.status().current_version,
            );
            match app_lock::status(&*state.store) {
                Ok(app_lock::Status::Disabled) => state.app_locked.store(false, Ordering::Release),
                Ok(app_lock::Status::Enabled) => {}
                Err(_) => {
                    state
                        .app_lock_storage_available
                        .store(false, Ordering::Release);
                }
            }
            app.manage(hosted::HostedState::default());
            let updates = state.updates.clone();
            let update_handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_secs(updates::STARTUP_DELAY_SECS))
                    .await;
                loop {
                    if updates.check_due() {
                        let _ = updates.check_automatic(&update_handle).await;
                    }
                    tokio::time::sleep(updates.next_check_delay()).await;
                }
            });
            let updates = state.updates.clone();
            let update_handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                loop {
                    tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                    let state = update_handle.state::<AppState>();
                    if updates.should_auto_restart(&update_handle) {
                        let _ = install_staged_update(
                            &update_handle,
                            &state,
                            updates::install::InstallIntent::Automatic,
                        )
                        .await;
                    }
                }
            });
            #[cfg(target_os = "linux")]
            {
                let status = state.updates.status();
                if status.channel != updates::types::UpdateChannel::Disabled
                    && matches!(status.package_kind.as_str(), "deb" | "rpm")
                {
                    let update_handle = app.handle().clone();
                    tauri::async_runtime::spawn(async move {
                        loop {
                            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
                            let state = update_handle.state::<AppState>();
                            let _ =
                                restart_after_external_package_update(&update_handle, &state).await;
                        }
                    });
                }
            }
            #[cfg(target_os = "macos")]
            {
                let update_handle = app.handle().clone();
                tauri::async_runtime::spawn(async move {
                    let mut poll = updates::installed::StablePoll::default();
                    loop {
                        tokio::time::sleep(std::time::Duration::from_secs(60)).await;
                        let state = update_handle.state::<AppState>();
                        let current_version = state.updates.status().current_version;
                        let installed = updates::installed::compare(
                            &current_version,
                            updates::installed::running_bundle_version(&update_handle).as_deref(),
                        );
                        let Some(version) = poll.observe(installed) else {
                            continue;
                        };
                        if state.updates.note_installed(&version) {
                            state.updates.emit(&update_handle);
                        }
                        if state.updates.should_auto_restart_installed(&update_handle) {
                            match restart_into_installed(
                                &update_handle,
                                &state,
                                &version,
                                restart_in_background(main_window_visible(&update_handle)),
                                false,
                            )
                            .await
                            {
                                Err(error) => eprintln!(
                                    "peppy: installed-update restart failed: {}",
                                    error.code
                                ),
                                Ok(never) => match never {},
                            }
                        }
                    }
                });
            }
            // The state hint is emitted after normal live applies and snapshot work alike. Core's
            // queue contains only live first-insert candidates, so this native drain cannot turn
            // history/snapshot replay into banners.
            let banner_handle = app.handle().clone();
            app.listen(STATE_EVENT, move |_| {
                let settings = banner_handle.state::<AppState>().notifications.clone();
                if !settings.request_banner_drain() {
                    return;
                }
                let handle = banner_handle.clone();
                tauri::async_runtime::spawn(async move {
                    loop {
                        let state = handle.state::<AppState>();
                        let more = match admit_update_operation(&state) {
                            Ok(operation) => match state.session_admitted(&operation).await {
                                Ok(Some(session)) => {
                                    let drain_handle = handle.clone();
                                    let settings = settings.clone();
                                    blocking(move || {
                                        notifications::drain_banner_candidates(
                                            &drain_handle,
                                            &session,
                                            &settings,
                                        )
                                    })
                                    .await
                                }
                                _ => Ok(false),
                            },
                            Err(_) => Ok(false),
                        };
                        // `true` means this bounded drain acknowledged candidates and more remain.
                        if matches!(more, Ok(true)) {
                            let _ = handle.emit(STATE_EVENT, ());
                        }
                        if !settings.finish_banner_drain() {
                            break;
                        }
                    }
                });
            });
            let installed = tray::install(
                app.handle(),
                |app| {
                    let app = app.clone();
                    tauri::async_runtime::spawn(async move {
                        if new_composer(app.clone(), None).await.is_err() {
                            tray::show_main(&app);
                        }
                    });
                },
                |app| {
                    if app.state::<AppState>().app_locked.load(Ordering::Acquire) {
                        tray::show_main(app);
                        return;
                    }
                    let app = app.clone();
                    tauri::async_runtime::spawn(async move {
                        let state = app.state::<AppState>();
                        let installed = state.updates.installed_pending();
                        let result = if let Some(version) = installed.as_deref() {
                            match state.updates.clear_attempted() {
                                Ok(()) => restart_into_installed(
                                    &app,
                                    &state,
                                    version,
                                    restart_in_background(main_window_visible(&app)),
                                    true,
                                )
                                .await
                                .map(|never| match never {}),
                                Err(error) => Err(error),
                            }
                        } else {
                            install_staged_update(
                                &app,
                                &state,
                                updates::install::InstallIntent::User,
                            )
                            .await
                            .map(|_| ())
                        };
                        if let Err(error) = result {
                            tray::show_main(&app);
                            if installed.is_none() || error.code != "update-busy" {
                                dialogs::inform(
                                    &app,
                                    "Could not restart for update",
                                    &error.message,
                                );
                            }
                        }
                    });
                },
                |app| {
                    if let Err(error) = lifecycle::request_quit(app) {
                        tray::show_main(app);
                        dialogs::inform(app, "Could not quit Peppy", &error.message);
                    }
                },
            );
            TRAY_ACTIVE.store(installed, Ordering::Relaxed);
            if installed {
                tray_badge::install(app.handle());
            }
            let _ = tray::set_update_status(app.handle(), &state.updates.status());
            let tray_handle = app.handle().clone();
            app.listen(updates::STATE_EVENT, move |_| {
                let state = tray_handle.state::<AppState>();
                let _ = tray::set_update_status(&tray_handle, &state.updates.status());
            });
            heads_runtime::install(app.handle()).map_err(|e| e.message)?;
            if startup::hide_initial_main(
                startup::restart_background(
                    restart_marker,
                    startup::background_requested(std::env::args()),
                ),
                installed,
            ) {
                startup::background_main(app.handle()).map_err(|e| e.message)?;
            } else {
                tray::show_main(app.handle());
            }
            // Resume background sync for an already imported credential without any prompt.
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                let state = handle.state::<AppState>();
                let Ok(operation) = admit_update_operation(&state) else {
                    return;
                };
                let _ = state.session_admitted(&operation).await;
            });
            Ok(())
        })
        .on_window_event(|window, event| {
            window_placement::window_event(window, event);
            if matches!(event, WindowEvent::Destroyed) {
                window_chrome::forget_material(window.label());
                if let Some(state) = window.app_handle().try_state::<AppState>() {
                    if let Ok(mut scopes) = state.composer_scopes.lock() {
                        scopes.remove(window.label());
                    }
                    let released = take_window_media_capabilities(&state, window.label());
                    if !released.is_empty() {
                        let app = window.app_handle().clone();
                        tauri::async_runtime::spawn(async move {
                            release_media_capabilities(&app.state::<AppState>(), released).await;
                        });
                    }
                }
            }
            heads_runtime::panel_window_event(window, event);
            if matches!(
                event,
                WindowEvent::Resized(_) | WindowEvent::ScaleFactorChanged { .. }
            ) && (window.label() == tray::MAIN
                || tray::composer_conversation(window.label()).is_some())
            {
                if let Some(webview_window) = window.app_handle().get_webview_window(window.label())
                {
                    window_chrome::sync_webview_window(
                        &webview_window,
                        heads_runtime::is_panel(window.app_handle(), window.label()),
                    );
                }
            }
            if let WindowEvent::CloseRequested { api, .. } = event {
                let is_main = window.label() == tray::MAIN;
                if is_main || tray::composer_conversation(window.label()).is_some() {
                    api.prevent_close();
                    if is_main {
                        window_placement::flush(window.app_handle());
                    }
                    let operation = lifecycle::close_operation(
                        is_main,
                        TRAY_ACTIVE.load(Ordering::Relaxed),
                        heads_runtime::is_panel(window.app_handle(), window.label()),
                    );
                    match operation {
                        lifecycle::Operation::Window(action) => {
                            if let Err(error) = lifecycle::request_window(
                                window.app_handle(),
                                window.label(),
                                action,
                            ) {
                                let _ = window.show();
                                let _ = window.set_focus();
                                dialogs::inform(
                                    window.app_handle(),
                                    "Could not close window",
                                    &error.message,
                                );
                            }
                        }
                        lifecycle::Operation::Quit => {
                            if let Err(error) = lifecycle::request_quit(window.app_handle()) {
                                let _ = window.show();
                                let _ = window.set_focus();
                                dialogs::inform(
                                    window.app_handle(),
                                    "Could not quit Peppy",
                                    &error.message,
                                );
                            }
                        }
                        lifecycle::Operation::Switch | lifecycle::Operation::Update => {
                            unreachable!(
                                "close cannot initiate account switching or update restart"
                            )
                        }
                    }
                }
            }
        });
    macro_rules! command_handler {
        ($($extra:path,)*) => { tauri::generate_handler![
        load_state,
        load_history,
        search_local,
        advance_search_index,
        search_history::get_history_search,
        search_history::start_history_search,
        search_history::cancel_history_search,
        load_message_window,
        open_external_url,
        configure_server,
        unlock_sync,
        get_app_lock_status,
        unlock_app,
        set_app_lock,
        save_draft,
        discard_draft,
        send_draft,
        mark_seen,
        mark_all_read,
        pick_attachments,
        retry_attachment,
        save_attachment,
        request_attachment_download,
        hydrate_media_preview,
        copy_attachment_image,
        media_sync_request,
        media_page,
        media_page_around,
        media_page_newer,
        open_media,
        load_media,
        close_media,
        publish_attachment,
        open_composer,
        dismiss_notification,
        dismiss_all_notifications,
        set_app_muted,
        set_conversation_pinned,
        set_conversation_tone,
        mark_notifications_seen,
        set_notification_preferences,
        set_notification_context,
        request_notification_permission,
        open_notification_settings,
        list_devices,
        revoke_device,
        list_contact_books,
        list_contacts,
        forget_contact_book,
        submit_contact_edit,
        list_contact_edits,
        pick_contact_photo,
        search_contact_recipients,
        request_contact_repair,
        request_contact_sync,
        get_contact_sync_request,
        list_restorable_contacts,
        restore_contact,
        set_start_at_login,
        popout_conversation,
        hide_head,
        close_composer,
        close_head_panel,
        update_status,
        check_for_updates,
        restart_for_update,
        defer_update,
        set_automatic_updates,
        report_update_activity,
        acknowledge_lifecycle,
        $($extra,)*
    ] };
    }
    let builder = builder.invoke_handler(move |invoke: tauri::ipc::Invoke| {
        let command = invoke.message.command();
        let webview = invoke.message.webview();
        let state = webview.app_handle().state::<AppState>();
        let allowed = matches!(
            command,
            "load_state"
                | "load_history"
                | "get_app_lock_status"
                | "update_status"
                | "report_update_activity"
                | "unlock_app"
                | "acknowledge_lifecycle"
        );
        if state.app_locked.load(Ordering::Acquire) && !allowed {
            invoke
                .resolver
                .reject(BridgeError::new("app-locked", "Peppy is locked."));
            return true;
        }
        let handler: fn(tauri::ipc::Invoke) -> bool = command_handler!(
            hosted::hosted_account,
            hosted::hosted_session_renew,
            hosted::hosted_origin_url,
            hosted::hosted_sign_in,
            hosted::hosted_sign_out,
            hosted::hosted_open_billing,
            hosted::hosted_provision,
            enrollment::enrollment_discover,
            enrollment::enrollment_start_over,
            enrollment::enrollment_create_owner,
            enrollment::enrollment_sign_in,
            enrollment::enrollment_connect_shared_secret,
            enrollment::enrollment_begin_vault_request,
            enrollment::enrollment_vault_request_status,
            enrollment::enrollment_create_vault,
            enrollment::computer::enrollment_begin_computer_claimant,
            enrollment::computer::enrollment_computer_claimant_status,
            enrollment::computer::enrollment_confirm_computer_claimant,
            enrollment::computer::enrollment_cancel_computer_claimant,
            enrollment::enrollment_add_phone,
            enrollment::enrollment_rotate_phone,
            enrollment::enrollment_cancel_phone,
            enrollment::reveal_vault_secret,
            enrollment::enrollment_approve,
            enrollment::enrollment_status,
            enrollment::enrollment_cancel,
            enrollment::enrollment_sign_out,
        );
        handler(invoke)
    });
    let app = builder
        .build(tauri::generate_context!())
        .expect("error while building Peppy desktop");
    app.run(|app, event| match event {
        #[cfg(target_os = "macos")]
        RunEvent::Reopen { .. } => activate_main(app),
        RunEvent::ExitRequested { api, .. } => {
            if let Some(state) = app.try_state::<AppState>() {
                if !state.allow_exit.load(Ordering::Acquire) {
                    api.prevent_exit();
                    if let Err(error) = lifecycle::request_quit(app) {
                        tray::show_main(app);
                        dialogs::inform(app, "Could not quit Peppy", &error.message);
                    }
                }
            }
        }
        RunEvent::Exit => {
            window_placement::flush(app);
            heads_runtime::clear_on_exit(app);
        }
        _ => {}
    });
}
