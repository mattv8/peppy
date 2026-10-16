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
};
use tauri::{Emitter, Listener, Manager, RunEvent, State, WindowEvent};
use tauri_plugin_autostart::ManagerExt as AutostartExt;
use tauri_plugin_notification::NotificationExt;

mod app_lock;
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
mod notifications;
mod origin;
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
mod window_chrome;
mod windows;

use credentials::{
    check_origin_binding, ensure_database_key, load_config, parse_credential, save_config,
    store_credential, HostConfig,
};
use dto::{DraftView, Head, HistoryPage, PublicCopyView, SendResultView, Snapshot};
use error::{core_error, BridgeError, BridgeResult};
use notifications::{NotificationPreferences, NotificationSettings, NotificationView};
use peppy_client_core::{
    AttachmentId, AttachmentInfo, ConversationId, NativePlaintextFile, NotificationTarget,
};
use peppy_desktop_api::compose::DiscardDraftInput;
#[cfg(target_os = "macos")]
use secure_store::BundledStore;
use secure_store::{KeyringStore, SecretStore};
use session::{open_session, DraftInput, Notifier, Session, VaultSummary};
use sync::{blocking, fetch_vault, vault_header};

pub const STATE_EVENT: &str = "peppy://state";
static TRAY_ACTIVE: AtomicBool = AtomicBool::new(false);
const MAX_OPEN_MEDIA_CAPABILITIES: usize = 8;
const MAX_OPEN_MEDIA_BYTES: u64 = 128 * 1024 * 1024;
const MAX_MEDIA_RANGE_BYTES: u64 = 4 * 1024 * 1024;

struct MediaCapability {
    session: Arc<Session>,
    attachment_id: AttachmentId,
    window_label: String,
    media_type: String,
    plaintext_bytes: u64,
    plaintext: NativePlaintextFile,
}

pub struct AppState {
    root: PathBuf,
    config_path: PathBuf,
    store: Arc<dyn SecretStore>,
    config_lock: Mutex<()>,
    session: tokio::sync::Mutex<Option<Arc<Session>>>,
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
}

/// Commands a composer window may call (mirrors `capabilities/composer.json`).
pub const COMPOSER_COMMANDS: &[&str] = &[
    "load_state",
    "load_history",
    "save_draft",
    "send_draft",
    "mark_seen",
    "pick_attachments",
    "retry_attachment",
    "save_attachment",
    "request_attachment_download",
    "media_page",
    "open_media",
    "load_media",
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

/// Runtime defense in depth behind the Tauri ACL: settings, import, unlock, publication,
/// composer creation and head commands are main-window only.
pub fn require_main(label: &str) -> BridgeResult<()> {
    if label == tray::MAIN {
        Ok(())
    } else {
        Err(window_error())
    }
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
        let config = self.config()?;
        let mut guard = self.session.lock().await;
        let Some(binding) = config.active_binding().cloned() else {
            if let Some(old) = guard.take() {
                old.cancel.cancel();
            }
            self.revoke_media_capabilities();
            return Ok(None);
        };
        if let Some(existing) = guard.as_ref() {
            if existing.binding == binding {
                return Ok(Some(existing.clone()));
            }
            existing.cancel.cancel();
            *guard = None;
            self.revoke_media_capabilities();
        }
        if let Ok(mut scopes) = self.composer_scopes.lock() {
            composer_scope::clear_for_binding(&mut scopes, &binding);
        }
        let epochs = config
            .known(&binding)
            .map(|known| known.cached_epochs.clone())
            .unwrap_or_default();
        let (root, store, notifier) =
            (self.root.clone(), self.store.clone(), self.notifier.clone());
        let session = Arc::new(
            blocking(move || open_session(&root, &*store, &binding, &epochs, notifier)).await?,
        );
        sync::start(&session);
        *guard = Some(session.clone());
        Ok(Some(session))
    }

    fn revoke_media_capabilities(&self) {
        if let Ok(mut capabilities) = self.media_capabilities.lock() {
            capabilities.clear();
        }
    }

    async fn require_session(&self) -> BridgeResult<Arc<Session>> {
        self.session().await?.ok_or_else(BridgeError::no_session)
    }
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

/// Opens the current binding for state snapshots, preserving secure-store and session failures.
async fn snapshot_session(state: &AppState) -> BridgeResult<Option<Arc<Session>>> {
    state.session().await
}

/// Loads a snapshot using the same window scope policy as the Tauri command.
async fn load_state_for_window(
    state: &AppState,
    label: &str,
    session: Arc<Session>,
    conversation_id: Option<String>,
    history_from_message_id: Option<String>,
    origin: Option<String>,
    preferences: NotificationPreferences,
) -> BridgeResult<(Snapshot, bool)> {
    if label == tray::MAIN {
        return blocking(move || {
            session.snapshot(
                conversation_id.as_deref(),
                history_from_message_id.as_deref(),
                head(),
                origin,
                preferences,
            )
        })
        .await;
    }
    let conversation_id =
        effective_composer_conversation(state, label, &session, conversation_id.as_deref())?
            .expect("composer conversation");
    blocking(move || {
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
) -> BridgeResult<Snapshot> {
    if state.app_locked.load(Ordering::Acquire) {
        return Ok(locked_snapshot(
            state.app_lock_storage_available.load(Ordering::Acquire),
        ));
    }
    let origin = state.config()?.origin;
    let Some(session) = snapshot_session(&state).await? else {
        return Ok(empty_snapshot(origin));
    };
    let preferences = state.notifications.preferences();
    let (mut snapshot, deferred) = load_state_for_window(
        &state,
        window.label(),
        session.clone(),
        conversation_id,
        history_from_message_id,
        origin,
        preferences,
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
    if deferred {
        session.notify();
    }
    Ok(snapshot)
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
    if state.app_locked.load(Ordering::Acquire) {
        return Err(BridgeError::new("app-locked", "Peppy is locked."));
    }
    let session = state.require_session().await?;
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
    if let Err(error) = state.session().await {
        let rollback = previous.clone();
        let _ = state.update_config(|config| *config = rollback);
        state.close_session_preserving_heads(preserve_heads).await;
        let _ = state.session().await;
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
        activate_import_locked(state, credential.clone()).await
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
    activate_import_locked(state, credential).await
}

async fn activate_import_locked(
    state: &AppState,
    credential: Arc<credentials::ImportedCredential>,
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
    if let Err(error) = state.session().await {
        let rollback = previous.clone();
        let _ = state.update_config(|config| *config = rollback);
        state.close_session_preserving_heads(preserve_heads).await;
        let _ = state.session().await;
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
    require_main(window.label())?;
    let session = state.require_session().await?;
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
        if let Ok(current) = state.require_session().await {
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

#[tauri::command]
async fn save_draft(
    window: tauri::WebviewWindow,
    state: State<'_, AppState>,
    input: DraftInput,
) -> BridgeResult<DraftView> {
    let session = state.require_session().await?;
    save_draft_for_window(&state, window.label(), session, input).await
}

#[tauri::command]
async fn discard_draft(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    input: DiscardDraftInput,
) -> BridgeResult<()> {
    require_main(window.label())?;
    let session = state.require_session().await?;
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
    let session = state.require_session().await?;
    send_draft_for_window(&state, window.label(), session, input).await
}

#[tauri::command]
async fn mark_seen(
    window: tauri::WebviewWindow,
    state: State<'_, AppState>,
    visible_message_ids: Vec<String>,
) -> BridgeResult<()> {
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
    let session = state.require_session().await?;
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
async fn list_contact_books(state: State<'_, AppState>) -> BridgeResult<Vec<serde_json::Value>> {
    let session = state.require_session().await?;
    blocking(move || contacts::list_books(&session)).await
}

#[tauri::command]
async fn list_devices(state: State<'_, AppState>) -> BridgeResult<devices::DeviceRosterView> {
    let session = state.require_session().await?;
    devices::list(&session).await
}

#[tauri::command]
async fn revoke_device(state: State<'_, AppState>, device_id: String) -> BridgeResult<()> {
    let session = state.require_session().await?;
    devices::revoke(&session, &device_id).await
}

#[tauri::command]
async fn list_contacts(
    state: State<'_, AppState>,
    book_id: String,
    query: Option<String>,
    offset: Option<u32>,
) -> BridgeResult<Vec<serde_json::Value>> {
    let session = state.require_session().await?;
    blocking(move || contacts::list_contacts(&session, &book_id, query.as_deref(), offset)).await
}

/// Display-only recipient discovery: phone numbers of matching contacts (addresses, not IDs).
#[tauri::command]
async fn search_contact_recipients(
    state: State<'_, AppState>,
    query: String,
    source_device_id: Option<String>,
) -> BridgeResult<Vec<serde_json::Value>> {
    let session = state.require_session().await?;
    blocking(move || contacts::search_recipients(&session, &query, source_device_id.as_deref()))
        .await
}

/// Latches a contact projection repair; the live loop runs one fenced snapshot.
#[tauri::command]
async fn request_contact_repair(state: State<'_, AppState>) -> BridgeResult<serde_json::Value> {
    let session = state.require_session().await?;
    blocking(move || contacts::request_repair(&session)).await
}

#[tauri::command]
async fn request_contact_sync(
    state: State<'_, AppState>,
    book_id: String,
) -> BridgeResult<serde_json::Value> {
    let session = state.require_session().await?;
    blocking(move || contacts::request_sync(&session, &book_id)).await
}

#[tauri::command]
async fn get_contact_sync_request(
    state: State<'_, AppState>,
    book_id: String,
) -> BridgeResult<serde_json::Value> {
    let session = state.require_session().await?;
    blocking(move || contacts::sync_request_status(&session, &book_id)).await
}

/// This device's contact edit requests and the owner results recorded in the local ledger.
#[tauri::command]
async fn list_contact_edits(
    state: State<'_, AppState>,
    book_id: Option<String>,
) -> BridgeResult<Vec<serde_json::Value>> {
    let session = state.require_session().await?;
    blocking(move || contacts::list_edits(&session, book_id.as_deref())).await
}

/// Native image picker for the contact photo cropper; the webview receives only a bounded,
/// re-encoded data URL, never a path or the original file bytes.
#[tauri::command]
async fn pick_contact_photo(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> BridgeResult<Option<serde_json::Value>> {
    state.require_session().await?;
    let Some(path) = dialogs::pick_file(&app, "Choose a contact photo").await? else {
        return Ok(None);
    };
    blocking(move || contacts::photo_source(&path))
        .await
        .map(Some)
}

#[tauri::command]
async fn forget_contact_book(state: State<'_, AppState>, book_id: String) -> BridgeResult<()> {
    let session = state.require_session().await?;
    blocking(move || contacts::forget_book(&session, &book_id)).await
}

#[tauri::command]
async fn submit_contact_edit(
    state: State<'_, AppState>,
    input: serde_json::Value,
) -> BridgeResult<serde_json::Value> {
    let session = state.require_session().await?;
    blocking(move || contacts::submit(&session, &input)).await
}

#[tauri::command]
async fn list_restorable_contacts(
    state: State<'_, AppState>,
    book_id: String,
) -> BridgeResult<Vec<serde_json::Value>> {
    let session = state.require_session().await?;
    blocking(move || contacts::restorable(&session, &book_id)).await
}

#[tauri::command]
async fn restore_contact(
    state: State<'_, AppState>,
    book_id: String,
    contact_id: String,
) -> BridgeResult<serde_json::Value> {
    let session = state.require_session().await?;
    blocking(move || contacts::restore(&session, &book_id, &contact_id)).await
}

#[tauri::command]
async fn pick_attachments(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> BridgeResult<Vec<dto::AttachmentView>> {
    let session = state.require_session().await?;
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

#[tauri::command]
async fn retry_attachment(
    window: tauri::WebviewWindow,
    state: State<'_, AppState>,
    id: String,
) -> BridgeResult<()> {
    let session = state.require_session().await?;
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
    let session = state.require_session().await?;
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
) -> BridgeResult<OpenMediaView> {
    let media_type = inline_media_type(&info.media_type).ok_or_else(|| {
        BridgeError::new(
            "unsupported",
            "Save this media to open it in another application.",
        )
    })?;
    if info.plaintext_bytes > MAX_OPEN_MEDIA_BYTES {
        return Err(BridgeError::new(
            "media-too-large",
            "This media item is too large to open safely.",
        ));
    }
    let mut capabilities = state
        .media_capabilities
        .lock()
        .map_err(|_| BridgeError::host_state())?;
    while capabilities.len() >= MAX_OPEN_MEDIA_CAPABILITIES
        || capabilities
            .values()
            .map(|capability| capability.plaintext_bytes)
            .sum::<u64>()
            + info.plaintext_bytes
            > MAX_OPEN_MEDIA_BYTES
    {
        let Some(token) = capabilities.keys().next().cloned() else {
            break;
        };
        capabilities.remove(&token);
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
        },
    );
    Ok(OpenMediaView {
        state: "ready",
        url: Some(media_capability_url(&token)),
        media_type: Some(media_type.to_owned()),
        byte_size: Some(info.plaintext_bytes),
    })
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
    let session = state.require_session().await?;
    check_attachment_window_scope(&state, &session, &window, &id).await?;
    let attachment = AttachmentId::from_str(&id)
        .map_err(|_| BridgeError::new("invalid-attachment", "The attachment ID is invalid."))?;
    demand_attachment_if_needed(&session, attachment).await?;
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
    let session = state.require_session().await?;
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
async fn open_media(
    window: tauri::WebviewWindow,
    state: State<'_, AppState>,
    id: String,
) -> BridgeResult<OpenMediaView> {
    let session = state.require_session().await?;
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
            .open_native_plaintext(attachment)
            .map(|plaintext| Some((info, plaintext)))
            .map_err(core_error)
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
                return Err(BridgeError::new("locked", "Peppy is locked."));
            }
            store_media_capability(&state, session.clone(), window.label(), info, plaintext)
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
    let attachment = AttachmentId::from_str(&id)
        .map_err(|_| BridgeError::new("invalid-attachment", "The attachment ID is invalid."))?;
    let (released_session, has_remaining_lease) = {
        let mut capabilities = state
            .media_capabilities
            .lock()
            .map_err(|_| BridgeError::host_state())?;
        let mut released_session = None;
        if let Some(url) = url {
            let token = media_capability_token(&url)
                .ok_or_else(|| BridgeError::new("invalid-request", "The media URL is invalid."))?;
            if capabilities.get(token).is_some_and(|capability| {
                capability.window_label == window.label() && capability.attachment_id == attachment
            }) {
                released_session = capabilities
                    .remove(token)
                    .map(|capability| capability.session);
            }
        } else {
            released_session = capabilities
                .values()
                .find(|capability| {
                    capability.window_label == window.label()
                        && capability.attachment_id == attachment
                })
                .map(|capability| capability.session.clone());
            capabilities.retain(|_, capability| {
                capability.window_label != window.label() || capability.attachment_id != attachment
            });
        }
        (
            released_session,
            capabilities
                .values()
                .any(|capability| capability.attachment_id == attachment),
        )
    };
    if let Some(session) = released_session.filter(|_| !has_remaining_lease) {
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
                    .evict_attachment_ciphertext(attachment)
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
    context: tauri::UriSchemeContext<'_, R>,
    request: tauri::http::Request<Vec<u8>>,
) -> tauri::http::Response<Vec<u8>> {
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
    let state = context.app_handle().state::<AppState>();
    if state.app_locked.load(Ordering::Acquire) {
        return tauri::http::Response::builder()
            .status(403)
            .body(Vec::new())
            .unwrap();
    }
    let active = match state.session.try_lock() {
        Ok(value) => value,
        Err(_) => {
            return tauri::http::Response::builder()
                .status(503)
                .body(Vec::new())
                .unwrap();
        }
    };
    let capabilities = match state.media_capabilities.lock() {
        Ok(value) => value,
        Err(_) => {
            return tauri::http::Response::builder()
                .status(503)
                .body(Vec::new())
                .unwrap();
        }
    };
    let Some(capability) = capabilities.get(token) else {
        return tauri::http::Response::builder()
            .status(404)
            .body(Vec::new())
            .unwrap();
    };
    if !active
        .as_ref()
        .is_some_and(|session| Arc::ptr_eq(session, &capability.session))
        || capability.window_label != context.webview_label()
        || request.method() != tauri::http::Method::GET
    {
        return tauri::http::Response::builder()
            .status(403)
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
    let had_range = request.headers().contains_key(tauri::http::header::RANGE);
    let (start, end) = match media_range(
        request.headers().get(tauri::http::header::RANGE),
        capability.plaintext_bytes,
    ) {
        Ok(range) => range,
        Err(()) => {
            return tauri::http::Response::builder()
                .status(416)
                .header(
                    tauri::http::header::CONTENT_RANGE,
                    format!("bytes */{}", capability.plaintext_bytes),
                )
                .body(Vec::new())
                .unwrap();
        }
    };
    let length = usize::try_from(end - start + 1).unwrap_or(0);
    let mut bytes = vec![0; length];
    let read = File::open(capability.plaintext.path()).and_then(|mut file| {
        file.seek(SeekFrom::Start(start))?;
        file.read_exact(&mut bytes)
    });
    if read.is_err() {
        return tauri::http::Response::builder()
            .status(404)
            .body(Vec::new())
            .unwrap();
    }
    let partial = had_range || start != 0 || end + 1 != capability.plaintext_bytes;
    let mut response = tauri::http::Response::builder()
        .status(if partial { 206 } else { 200 })
        .header(tauri::http::header::CONTENT_TYPE, &capability.media_type)
        .header(tauri::http::header::ACCEPT_RANGES, "bytes")
        .header(tauri::http::header::CACHE_CONTROL, "no-store")
        .header("X-Content-Type-Options", "nosniff")
        .header(tauri::http::header::CONTENT_LENGTH, bytes.len());
    if partial {
        response = response.header(
            tauri::http::header::CONTENT_RANGE,
            format!("bytes {start}-{end}/{}", capability.plaintext_bytes),
        );
    }
    response.body(bytes).unwrap()
}

#[cfg(test)]
mod media_protocol_tests {
    use super::{media_range, MAX_MEDIA_RANGE_BYTES};
    use tauri::http::HeaderValue;

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
    require_main(window.label())?;
    let session = state.require_session().await?;
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
    let _window_permit = lifecycle::permit_window_creation(&app)?;
    let state = app.state::<AppState>();
    let conversation = match conversation {
        Some(id) => ConversationId::from_str(&id).map_err(|_| {
            BridgeError::new("invalid-conversation", "The conversation ID is invalid.")
        })?,
        None => {
            let session = state.require_session().await?;
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
    require_main(window.label())?;
    let session = state.require_session().await?;
    let s = session.clone();
    blocking(move || s.client.dismiss_notification(target).map_err(core_error)).await?;
    session.notify();
    Ok(())
}

/// Fans out at most 100 durable effects, avoiding an unbounded local outbox action.
#[tauri::command]
async fn dismiss_all_notifications(
    window: tauri::WebviewWindow,
    state: State<'_, AppState>,
) -> BridgeResult<()> {
    require_main(window.label())?;
    let session = state.require_session().await?;
    let s = session.clone();
    blocking(move || {
        let snapshot = s.client.notification_snapshot().map_err(core_error)?;
        for notification in snapshot
            .notifications
            .into_iter()
            .filter(|notification| notification.dismissible && !notification.dismissal_pending)
            .take(100)
        {
            s.client
                .dismiss_notification(notification.target)
                .map_err(core_error)?;
        }
        Ok(())
    })
    .await?;
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
    require_main(window.label())?;
    let session = state.require_session().await?;
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
async fn mark_notifications_seen(
    window: tauri::WebviewWindow,
    state: State<'_, AppState>,
    targets: Vec<NotificationTarget>,
) -> BridgeResult<()> {
    require_main(window.label())?;
    let session = state.require_session().await?;
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
    require_main(window.label())?;
    state.notifications.set_context(view, conversation_id);
    Ok(())
}

/// Permission is native-only. Desktop plugin status is not a reliable OS authorization query, so
/// callers must guide users to OS settings rather than treating this as a banner grant.
#[tauri::command]
fn request_notification_permission(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
) -> BridgeResult<String> {
    require_main(window.label())?;
    let _plugin_state = app.notification().request_permission().map_err(|_| {
        BridgeError::new(
            "notification-permission",
            "Could not request notification permission.",
        )
    })?;
    Ok("unknown".into())
}

/// OS registration is authoritative. This command is main-window-only and no
/// registration is attempted during setup or tests.
#[tauri::command]
fn set_start_at_login(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
    enabled: bool,
) -> BridgeResult<()> {
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
    require_main(window.label())?;
    let _window_permit = lifecycle::permit_window_creation(&app)?;
    let conversation = ConversationId::from_str(&conversation_id)
        .map_err(|_| BridgeError::new("invalid-conversation", "The conversation ID is invalid."))?;
    let _ = state.require_session().await?;
    heads_runtime::popout(&app, conversation).await
}

#[tauri::command]
async fn hide_head(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
    conversation_id: String,
) -> BridgeResult<()> {
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

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let builder = tauri::Builder::default()
        .register_uri_scheme_protocol("peppy-media", media_protocol_response)
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
        // Must precede setup: a second manual launch activates the resident
        // main window before it can open another database/session.
        .plugin(tauri_plugin_single_instance::init(|app, args, _| {
            if !startup::background_requested(args) {
                tray::show_main(app);
            }
        }))
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            Some(vec![startup::BACKGROUND_ARGUMENT]),
        ))
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init())
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
            app.manage(AppState::new(root, Arc::new(secure_store), notifier));
            let state = app.state::<AppState>();
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
            // The state hint is emitted after normal live applies and snapshot work alike. Core's
            // queue contains only live first-insert candidates, so this native drain cannot turn
            // history/snapshot replay into banners.
            let banner_handle = app.handle().clone();
            app.listen(STATE_EVENT, move |_| {
                let handle = banner_handle.clone();
                tauri::async_runtime::spawn(async move {
                    let state = handle.state::<AppState>();
                    let Ok(Some(session)) = state.session().await else {
                        return;
                    };
                    let settings = state.notifications.clone();
                    let drain_handle = handle.clone();
                    let more = blocking(move || {
                        notifications::drain_banner_candidates(&drain_handle, &session, &settings)
                    })
                    .await;
                    if matches!(more, Ok(true)) {
                        let _ = handle.emit(STATE_EVENT, ());
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
                    if let Err(error) = lifecycle::request_quit(app) {
                        tray::show_main(app);
                        dialogs::inform(app, "Could not quit Peppy", &error.message);
                    }
                },
            );
            TRAY_ACTIVE.store(installed, Ordering::Relaxed);
            heads_runtime::install(app.handle()).map_err(|e| e.message)?;
            if startup::hide_initial_main(
                startup::background_requested(std::env::args()),
                installed,
            ) {
                startup::background_main(app.handle()).map_err(|e| e.message)?;
            } else {
                tray::show_main(app.handle());
            }
            // Resume background sync for an already imported credential without any prompt.
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                let _ = handle.state::<AppState>().session().await;
            });
            Ok(())
        })
        .on_window_event(|window, event| {
            if matches!(event, WindowEvent::Destroyed) {
                window_chrome::forget_material(window.label());
                if let Some(state) = window.app_handle().try_state::<AppState>() {
                    if let Ok(mut scopes) = state.composer_scopes.lock() {
                        scopes.remove(window.label());
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
                        lifecycle::Operation::Switch => unreachable!("close cannot switch account"),
                    }
                }
            }
        });
    macro_rules! command_handler {
        ($($extra:path,)*) => { tauri::generate_handler![
        load_state,
        load_history,
        configure_server,
        unlock_sync,
        get_app_lock_status,
        unlock_app,
        set_app_lock,
        save_draft,
        discard_draft,
        send_draft,
        mark_seen,
        pick_attachments,
        retry_attachment,
        save_attachment,
        request_attachment_download,
        media_page,
        open_media,
        load_media,
        close_media,
        publish_attachment,
        open_composer,
        dismiss_notification,
        dismiss_all_notifications,
        set_app_muted,
        mark_notifications_seen,
        set_notification_preferences,
        set_notification_context,
        request_notification_permission,
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
        RunEvent::Reopen { .. } => tray::show_main(app),
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
        RunEvent::Exit => heads_runtime::clear_on_exit(app),
        _ => {}
    });
}
