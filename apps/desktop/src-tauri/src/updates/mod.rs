mod cache;
mod feed;
pub mod install;
mod policy;
pub mod types;

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_updater::UpdaterExt;

use crate::error::{BridgeError, BridgeResult};

use self::{
    policy::{LATER_SECS, RetrySchedule, defer_is_active, parse_channel, strictly_newer},
    types::{UpdateChannel, UpdatePhase, UpdateStatus},
};

pub const STATE_EVENT: &str = "peppy://update-state";
const VISIBLE_HEARTBEAT_MAX_AGE: Duration = Duration::from_secs(90);
const IDLE_RESTART_DELAY: Duration = Duration::from_secs(5 * 60);

#[derive(Clone)]
pub struct UpdateManager(Arc<Mutex<Inner>>);

struct Inner {
    status: UpdateStatus,
    operation: Operation,
    activity: HashMap<String, Activity>,
    root: Option<PathBuf>,
    staged: Option<StagedUpdate>,
    attempted_version: Option<String>,
    retry_schedule: RetrySchedule,
    last_progress_emit: Instant,
    persistence: Arc<Mutex<()>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Operation {
    Idle,
    Checking,
    Installing,
}

#[derive(Clone, Copy)]
struct Activity {
    heartbeat: Instant,
    input: Instant,
}

struct WebviewFact {
    label: String,
    visible: bool,
}

#[derive(Clone)]
pub struct StagedUpdate {
    pub update: tauri_plugin_updater::Update,
    pub path: PathBuf,
}

impl StagedUpdate {
    pub fn verified_bytes(&self) -> BridgeResult<Vec<u8>> {
        let metadata = std::fs::metadata(&self.path)
            .map_err(|_| BridgeError::new("update-cache", "Update payload is unavailable."))?;
        if metadata.len() > cache::MAX_PAYLOAD_BYTES {
            return Err(BridgeError::new(
                "update-payload",
                "Update payload exceeds the size limit.",
            ));
        }
        let bytes = std::fs::read(&self.path)
            .map_err(|_| BridgeError::new("update-cache", "Could not read update payload."))?;
        self.update.verify(&bytes).map_err(|_| {
            BridgeError::new("update-signature", "Cached update verification failed.")
        })?;
        Ok(bytes)
    }
}

impl UpdateManager {
    pub fn new(current_version: String) -> Self {
        let channel = parse_channel(option_env!("PEPPY_UPDATE_CHANNEL"));
        let key_present =
            option_env!("PEPPY_UPDATE_PUBLIC_KEY").is_some_and(|key| !key.trim().is_empty());
        let status = if channel == UpdateChannel::Disabled || !key_present {
            UpdateStatus::disabled(current_version)
        } else {
            UpdateStatus::idle(current_version, channel)
        };
        Self(Arc::new(Mutex::new(Inner {
            status,
            operation: Operation::Idle,
            activity: HashMap::new(),
            root: None,
            staged: None,
            attempted_version: None,
            retry_schedule: RetrySchedule::new(),
            last_progress_emit: Instant::now(),
            persistence: Arc::new(Mutex::new(())),
        })))
    }

    pub fn initialize(&self, app: &AppHandle, root: &Path) -> BridgeResult<()> {
        let (mut settings, settings_error) = match cache::load_settings(root) {
            Ok(settings) => (settings, false),
            Err(_) => (
                cache::Settings {
                    automatic: false,
                    deferred_until: None,
                    attempted_version: None,
                },
                true,
            ),
        };
        let current_version = app.package_info().version.to_string();
        let channel = self.status().channel;
        let cached = cache::load_cached(root).ok().flatten();
        if settings.attempted_version.as_deref() == Some(current_version.as_str()) {
            settings.attempted_version = None;
            let _ = cache::save_settings(root, &settings);
        }

        {
            let mut inner = self.0.lock().map_err(|_| BridgeError::host_state())?;
            inner.root = Some(root.to_path_buf());
            inner.status.current_version = current_version.clone();
            if channel != UpdateChannel::Disabled {
                inner.status.automatic = settings.automatic;
                inner.status.deferred_until = settings
                    .deferred_until
                    .clone()
                    .filter(|value| defer_is_active(value));
            }
            inner.attempted_version = settings.attempted_version.clone();
            if settings_error {
                inner.status.phase = UpdatePhase::Error;
                inner.status.message =
                    Some("Update settings are invalid; automatic updates are disabled.".into());
            }
        }

        let Some((cached, path)) = cached else {
            return Ok(());
        };
        let status = self.status();
        if cached.channel != channel.as_str()
            || cached.package_kind != status.package_kind
            || !strictly_newer(&current_version, &cached.version, channel)
        {
            let _ = cache::remove_cached(root);
            return Ok(());
        }

        let key = option_env!("PEPPY_UPDATE_PUBLIC_KEY").unwrap_or_default();
        let restored = app
            .updater_builder()
            .pubkey(key)
            .target(match qualified_target(&status.package_kind) {
                Ok(target) => target,
                Err(_) => {
                    let _ = cache::remove_cached(root);
                    return Ok(());
                }
            })
            .build()
            .and_then(|updater| updater.restore_update(cached.manifest));
        let Ok(restored) = restored else {
            let _ = cache::remove_cached(root);
            return Ok(());
        };
        let Some(update) = restored else {
            let _ = cache::remove_cached(root);
            return Ok(());
        };
        if update.version != cached.version
            || (StagedUpdate {
                update: update.clone(),
                path: path.clone(),
            })
            .verified_bytes()
            .is_err()
        {
            let _ = cache::remove_cached(root);
            return Ok(());
        }
        let mut inner = self.0.lock().map_err(|_| BridgeError::host_state())?;
        inner.staged = Some(StagedUpdate {
            update: update.clone(),
            path,
        });
        inner.status.phase = UpdatePhase::Ready;
        inner.status.available_version = Some(update.version.clone());
        inner.status.release_notes_url = Some(release_notes(&update.version));
        inner.status.progress_percent = Some(100);
        inner.status.can_restart = true;
        inner.status.message = Some("Update downloaded and verified; restart when ready.".into());
        Ok(())
    }

    pub fn status(&self) -> UpdateStatus {
        let mut inner = self.0.lock().expect("update state poisoned");
        if inner.status.phase == UpdatePhase::Deferred
            && !defer_is_active_option(inner.status.deferred_until.as_deref())
        {
            inner.status.deferred_until = None;
            restore_staged_phase(&mut inner.status);
        }
        inner.status.clone()
    }

    pub fn set_automatic(&self, enabled: bool) -> BridgeResult<UpdateStatus> {
        self.persist_preference(
            |settings| settings.automatic = enabled,
            |inner| {
                if inner.status.channel != UpdateChannel::Disabled {
                    inner.status.automatic = enabled;
                }
            },
        )
    }

    pub fn defer(&self) -> BridgeResult<UpdateStatus> {
        if !self
            .0
            .lock()
            .map_err(|_| BridgeError::host_state())?
            .staged
            .is_some()
        {
            return Ok(self.status());
        }
        let deferred_until = timestamp_after(Duration::from_secs(LATER_SECS));
        self.persist_preference(
            |settings| settings.deferred_until = Some(deferred_until.clone()),
            |inner| {
                inner.status.phase = UpdatePhase::Deferred;
                inner.status.deferred_until = Some(deferred_until.clone());
                inner.status.can_restart = true;
            },
        )
    }

    pub fn report_activity(&self, label: &str, active: bool) {
        let mut inner = self.0.lock().expect("update state poisoned");
        let now = Instant::now();
        let activity = inner.activity.entry(label.into()).or_insert(Activity {
            heartbeat: now,
            input: now,
        });
        activity.heartbeat = now;
        if active {
            activity.input = now;
        }
    }

    pub async fn check(&self, app: &AppHandle) -> BridgeResult<UpdateStatus> {
        self.check_with_mode(app, false).await
    }

    pub async fn check_automatic(&self, app: &AppHandle) -> BridgeResult<UpdateStatus> {
        self.check_with_mode(app, true).await
    }

    async fn check_with_mode(
        &self,
        app: &AppHandle,
        automatic: bool,
    ) -> BridgeResult<UpdateStatus> {
        let (channel, root, current_version) = {
            let mut inner = self.0.lock().map_err(|_| BridgeError::host_state())?;
            if inner.status.channel == UpdateChannel::Disabled || !begin_check(&mut inner) {
                return Ok(inner.status.clone());
            }
            (
                inner.status.channel,
                inner.root.clone(),
                inner.status.current_version.clone(),
            )
        };
        self.emit(app);
        let result = self
            .check_and_stage(app, channel, root, &current_version, automatic)
            .await;
        self.finish_check(result, app)
    }

    async fn check_and_stage(
        &self,
        app: &AppHandle,
        channel: UpdateChannel,
        root: Option<PathBuf>,
        current_version: &str,
        automatic: bool,
    ) -> BridgeResult<Option<StagedUpdate>> {
        let manifest = feed::manifest(channel).await?;
        let key = option_env!("PEPPY_UPDATE_PUBLIC_KEY").unwrap_or_default();
        let update = app
            .updater_builder()
            .pubkey(key)
            .target(qualified_target(&self.status().package_kind)?)
            .timeout(Duration::from_secs(30))
            .build()
            .and_then(|updater| updater.restore_update(manifest))
            .map_err(|_| {
                BridgeError::new("update-feed", "Update manifest could not be verified.")
            })?;
        let Some(update) = update else {
            return Ok(None);
        };
        if !strictly_newer(current_version, &update.version, channel)
            || (automatic && suppresses_attempt(&self.attempted_version(), &update.version))
            || self
                .staged()
                .is_some_and(|staged| staged.update.version == update.version)
        {
            return Ok(None);
        }
        let root = root
            .ok_or_else(|| BridgeError::new("update-cache", "Update cache is not initialized."))?;
        self.set_downloading(app)?;
        let manager = self.clone();
        let handle = app.clone();
        let verified_path = feed::download_payload(&update, &root, move |received, total| {
            manager.update_progress(&handle, received, total);
        })
        .await?;
        let cached = cache::CachedUpdate {
            version: update.version.clone(),
            channel: channel.as_str().into(),
            package_kind: self.status().package_kind,
            manifest: update.raw_json.clone(),
            payload: String::new(),
        };
        let path = cache::commit_cached(&root, &cached, &verified_path)?;
        Ok(Some(StagedUpdate { update, path }))
    }

    fn finish_check(
        &self,
        result: BridgeResult<Option<StagedUpdate>>,
        app: &AppHandle,
    ) -> BridgeResult<UpdateStatus> {
        let mut inner = self.0.lock().map_err(|_| BridgeError::host_state())?;
        inner.operation = Operation::Idle;
        inner.status.last_checked_at = Some(timestamp_now());
        match result {
            Ok(Some(staged)) => {
                let version = staged.update.version.clone();
                inner.staged = Some(staged);
                inner.status.phase = UpdatePhase::Ready;
                inner.status.available_version = Some(version.clone());
                inner.status.release_notes_url = Some(release_notes(&version));
                inner.status.progress_percent = Some(100);
                inner.status.can_restart = true;
                inner.status.message =
                    Some("Update downloaded and verified; restart when ready.".into());
                inner.retry_schedule.record_success();
            }
            Ok(None) => {
                if inner.staged.is_none() {
                    inner.status.reset_to_idle();
                } else {
                    restore_staged_phase(&mut inner.status);
                }
                inner.retry_schedule.record_success();
            }
            Err(error) => {
                mark_check_failure(&mut inner);
                drop(inner);
                self.emit(app);
                return Err(error);
            }
        }
        let status = inner.status.clone();
        drop(inner);
        self.emit(app);
        Ok(status)
    }

    pub fn staged(&self) -> Option<StagedUpdate> {
        self.0.lock().ok()?.staged.clone()
    }
    pub fn automatic(&self) -> bool {
        self.status().automatic
    }
    pub fn check_due(&self) -> bool {
        self.automatic()
            && self
                .0
                .lock()
                .map(|inner| inner.operation == Operation::Idle)
                .unwrap_or(false)
    }
    pub fn next_check_delay(&self) -> Duration {
        self.0
            .lock()
            .map(|inner| inner.retry_schedule.next_delay())
            .unwrap_or(Duration::from_secs(policy::CHECK_INTERVAL_SECS))
    }

    pub fn should_auto_restart(&self, app: &AppHandle) -> bool {
        let mut inner = self.0.lock().expect("update state poisoned");
        let now = Instant::now();
        let webviews = app.webview_windows();
        inner
            .activity
            .retain(|label, _| webviews.contains_key(label));
        let webview_facts = webviews
            .into_iter()
            .map(|(label, window)| WebviewFact {
                label,
                visible: window.is_visible().unwrap_or(true),
            })
            .collect::<Vec<_>>();
        let visible_window_blocks = webviews_block_restart(&inner.activity, &webview_facts, now);
        inner.staged.is_some()
            && inner.status.automatic
            && inner.operation == Operation::Idle
            && !defer_is_active_option(inner.status.deferred_until.as_deref())
            && inner.attempted_version.as_deref() != inner.status.available_version.as_deref()
            && !visible_window_blocks
    }

    pub fn should_restart_external(&self, app: &AppHandle, installed_version: &str) -> bool {
        let mut inner = self.0.lock().expect("update state poisoned");
        let now = Instant::now();
        let webviews = app.webview_windows();
        inner
            .activity
            .retain(|label, _| webviews.contains_key(label));
        let facts = webviews
            .into_iter()
            .map(|(label, window)| WebviewFact {
                label,
                visible: window.is_visible().unwrap_or(true),
            })
            .collect::<Vec<_>>();
        external_restart_eligible(&inner, installed_version)
            && !webviews_block_restart(&inner.activity, &facts, now)
    }

    pub fn set_installing(&self, expected_version: &str) -> BridgeResult<()> {
        let (_root, _version) = {
            let mut inner = self.0.lock().map_err(|_| BridgeError::host_state())?;
            let version = inner
                .staged
                .as_ref()
                .ok_or_else(|| {
                    BridgeError::new("update-not-ready", "No verified update is ready.")
                })?
                .update
                .version
                .clone();
            if version != expected_version {
                return Err(BridgeError::new(
                    "update-not-ready",
                    "The verified update changed; try restarting it again.",
                ));
            }
            if inner.operation != Operation::Idle {
                return Err(BridgeError::new(
                    "update-busy",
                    "An update operation is already in progress.",
                ));
            }
            let root = inner.root.clone().ok_or_else(|| {
                BridgeError::new("update-cache", "Update cache is not initialized.")
            })?;
            inner.operation = Operation::Installing;
            (root, version)
        };
        let mut inner = self.0.lock().map_err(|_| BridgeError::host_state())?;
        if inner.operation != Operation::Installing {
            return Err(BridgeError::new(
                "update-busy",
                "An update operation is already in progress.",
            ));
        }
        inner.status.phase = UpdatePhase::Installing;
        inner.status.message = Some("Installing verified update…".into());
        Ok(())
    }

    /// Record only the irreversible installer attempt. Preparation, drain and
    /// authorization failures leave the verified payload available on relaunch.
    pub fn record_install_attempt(&self, expected_version: &str) -> BridgeResult<()> {
        let persistence = self
            .0
            .lock()
            .map_err(|_| BridgeError::host_state())?
            .persistence
            .clone();
        let _persistence = persistence.lock().map_err(|_| BridgeError::host_state())?;
        let (root, settings) = {
            let inner = self.0.lock().map_err(|_| BridgeError::host_state())?;
            let staged = inner.staged.as_ref().ok_or_else(|| {
                BridgeError::new("update-not-ready", "No verified update is ready.")
            })?;
            if inner.operation != Operation::Installing || staged.update.version != expected_version
            {
                return Err(BridgeError::new(
                    "update-busy",
                    "The update install is no longer current.",
                ));
            }
            let root = inner.root.clone().ok_or_else(|| {
                BridgeError::new("update-cache", "Update cache is not initialized.")
            })?;
            (
                root,
                cache::Settings {
                    automatic: inner.status.automatic,
                    deferred_until: inner.status.deferred_until.clone(),
                    attempted_version: Some(expected_version.into()),
                },
            )
        };
        cache::save_settings(&root, &settings)?;
        let mut inner = self.0.lock().map_err(|_| BridgeError::host_state())?;
        if inner.operation != Operation::Installing
            || inner
                .staged
                .as_ref()
                .map(|staged| staged.update.version.as_str())
                != Some(expected_version)
        {
            return Err(BridgeError::new(
                "update-busy",
                "The update install is no longer current.",
            ));
        }
        inner.attempted_version = settings.attempted_version;
        Ok(())
    }

    pub fn set_authorization_required(&self) {
        if let Ok(mut inner) = self.0.lock() {
            // A Polkit denial is terminal for this installer attempt. Keep the
            // verified payload ready for an explicit retry, but release the
            // lease so the user is not permanently blocked behind Installing.
            inner.operation = Operation::Idle;
            inner.status.phase = UpdatePhase::AuthorizationRequired;
            inner.status.can_restart = inner.staged.is_some();
            inner.status.message =
                Some("This system-package update requires PackageKit authorization.".into());
        }
    }

    pub fn finish_external_update(&self, installed_version: &str) -> BridgeResult<()> {
        let (root, persistence) = {
            let inner = self.0.lock().map_err(|_| BridgeError::host_state())?;
            (inner.root.clone(), inner.persistence.clone())
        };
        let _persistence = persistence.lock().map_err(|_| BridgeError::host_state())?;
        let settings = {
            let inner = self.0.lock().map_err(|_| BridgeError::host_state())?;
            cache::Settings {
                automatic: inner.status.automatic,
                deferred_until: inner.status.deferred_until.clone(),
                attempted_version: Some(installed_version.into()),
            }
        };
        if let Some(root) = root {
            cache::save_settings(&root, &settings)?;
            cache::remove_cached(&root)?;
        }
        let mut inner = self.0.lock().map_err(|_| BridgeError::host_state())?;
        inner.staged = None;
        inner.operation = Operation::Idle;
        inner.attempted_version = Some(installed_version.into());
        inner.status.reset_to_idle();
        Ok(())
    }

    pub fn install_failed(&self, message: &str) {
        if let Ok(mut inner) = self.0.lock() {
            inner.operation = Operation::Idle;
            inner.status.phase = UpdatePhase::Error;
            inner.status.message = Some(message.into());
            inner.status.can_restart = inner.staged.is_some();
        }
    }

    pub(crate) fn emit(&self, app: &AppHandle) {
        let _ = app.emit(STATE_EVENT, ());
    }

    fn set_downloading(&self, app: &AppHandle) -> BridgeResult<()> {
        let mut inner = self.0.lock().map_err(|_| BridgeError::host_state())?;
        inner.status.phase = UpdatePhase::Downloading;
        inner.status.progress_percent = None;
        drop(inner);
        self.emit(app);
        Ok(())
    }

    fn update_progress(&self, app: &AppHandle, received: u64, total: Option<u64>) {
        let mut should_emit = false;
        if let Ok(mut inner) = self.0.lock() {
            let progress = total
                .filter(|total| *total > 0)
                .map(|total| ((received.saturating_mul(100) / total).min(100)) as u8);
            should_emit = progress != inner.status.progress_percent
                || (progress.is_none()
                    && inner.last_progress_emit.elapsed() >= Duration::from_secs(1));
            inner.status.progress_percent = progress;
            if should_emit {
                inner.last_progress_emit = Instant::now();
            }
        }
        if should_emit {
            self.emit(app);
        }
    }

    fn attempted_version(&self) -> Option<String> {
        self.0.lock().ok()?.attempted_version.clone()
    }

    fn persist_preference(
        &self,
        mutate_settings: impl FnOnce(&mut cache::Settings),
        commit: impl FnOnce(&mut Inner),
    ) -> BridgeResult<UpdateStatus> {
        let persistence = self
            .0
            .lock()
            .map_err(|_| BridgeError::host_state())?
            .persistence
            .clone();
        let _persistence = persistence.lock().map_err(|_| BridgeError::host_state())?;
        let (root, mut settings) = {
            let inner = self.0.lock().map_err(|_| BridgeError::host_state())?;
            let root = inner.root.clone().ok_or_else(|| {
                BridgeError::new("update-cache", "Update cache is not initialized.")
            })?;
            (
                root,
                cache::Settings {
                    automatic: inner.status.automatic,
                    deferred_until: inner.status.deferred_until.clone(),
                    attempted_version: inner.attempted_version.clone(),
                },
            )
        };
        mutate_settings(&mut settings);
        cache::save_settings(&root, &settings)?;
        let mut inner = self.0.lock().map_err(|_| BridgeError::host_state())?;
        commit(&mut inner);
        Ok(inner.status.clone())
    }
}

fn webviews_block_restart(
    activity: &HashMap<String, Activity>,
    webviews: &[WebviewFact],
    now: Instant,
) -> bool {
    webviews.iter().any(|webview| {
        activity
            .get(&webview.label)
            .map(|activity| {
                now.duration_since(activity.input) < IDLE_RESTART_DELAY
                    || (webview.visible
                        && now.duration_since(activity.heartbeat) > VISIBLE_HEARTBEAT_MAX_AGE)
            })
            .unwrap_or(webview.visible)
    })
}

fn begin_check(inner: &mut Inner) -> bool {
    if inner.operation != Operation::Idle {
        return false;
    }
    inner.operation = Operation::Checking;
    inner.status.phase = UpdatePhase::Checking;
    inner.status.message = None;
    true
}

fn mark_check_failure(inner: &mut Inner) {
    inner.operation = Operation::Idle;
    if inner.staged.is_none() {
        inner.status.phase = UpdatePhase::Error;
        inner.status.message = Some("Could not check for updates. Try again later.".into());
    }
    if inner.staged.is_some() {
        restore_staged_phase(&mut inner.status);
    }
    inner.retry_schedule.record_failure();
}

fn restore_staged_phase(status: &mut UpdateStatus) {
    status.phase = if defer_is_active_option(status.deferred_until.as_deref()) {
        UpdatePhase::Deferred
    } else {
        UpdatePhase::Ready
    };
}

fn suppresses_attempt(attempted_version: &Option<String>, candidate_version: &str) -> bool {
    attempted_version.as_deref() == Some(candidate_version)
}

fn external_restart_eligible(inner: &Inner, installed_version: &str) -> bool {
    inner.status.automatic
        && inner.operation == Operation::Idle
        && !defer_is_active_option(inner.status.deferred_until.as_deref())
        && strictly_newer(
            &inner.status.current_version,
            installed_version,
            inner.status.channel,
        )
        && !suppresses_attempt(&inner.attempted_version, installed_version)
}

fn defer_is_active_option(value: Option<&str>) -> bool {
    value.is_some_and(defer_is_active)
}
fn release_notes(version: &str) -> String {
    format!("https://github.com/mattv8/peppy/releases/tag/v{version}")
}
fn qualified_target(package_kind: &str) -> BridgeResult<String> {
    let base = tauri_plugin_updater::target()
        .ok_or_else(|| BridgeError::new("update-target", "This update target is unsupported."))?;
    Ok(format!("{base}-{package_kind}"))
}
fn timestamp_now() -> String {
    unix_timestamp(SystemTime::now())
}
fn timestamp_after(delay: Duration) -> String {
    unix_timestamp(SystemTime::now() + delay)
}
fn unix_timestamp(time: SystemTime) -> String {
    time::OffsetDateTime::from_unix_timestamp(
        time.duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_secs() as i64)
            .unwrap_or_default(),
    )
    .ok()
    .and_then(|time| {
        time.format(&time::format_description::well_known::Rfc3339)
            .ok()
    })
    .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> Inner {
        Inner {
            status: UpdateStatus::idle("1.0.0".into(), UpdateChannel::Stable),
            operation: Operation::Idle,
            activity: HashMap::new(),
            root: None,
            staged: None,
            attempted_version: None,
            retry_schedule: RetrySchedule::new(),
            last_progress_emit: Instant::now(),
            persistence: Arc::new(Mutex::new(())),
        }
    }

    fn fact(label: &str, visible: bool) -> WebviewFact {
        WebviewFact {
            label: label.into(),
            visible,
        }
    }

    #[test]
    fn unknown_visible_webview_blocks_an_automatic_restart() {
        assert!(webviews_block_restart(
            &HashMap::new(),
            &[fact("main", true)],
            Instant::now(),
        ));
    }

    #[test]
    fn stale_or_recent_visible_webview_blocks_but_hidden_webview_does_not() {
        let now = Instant::now();
        let stale = Activity {
            heartbeat: now - VISIBLE_HEARTBEAT_MAX_AGE - Duration::from_secs(1),
            input: now - IDLE_RESTART_DELAY - Duration::from_secs(1),
        };
        let recent = Activity {
            heartbeat: now,
            input: now - Duration::from_secs(1),
        };
        assert!(webviews_block_restart(
            &HashMap::from([("main".into(), stale)]),
            &[fact("main", true)],
            now,
        ));
        assert!(webviews_block_restart(
            &HashMap::from([("main".into(), recent)]),
            &[fact("main", true)],
            now,
        ));
        assert!(webviews_block_restart(
            &HashMap::from([("main".into(), recent)]),
            &[fact("main", false)],
            now,
        ));
    }

    #[test]
    fn release_notes_are_version_bound() {
        assert_eq!(
            release_notes("1.2.3-staging.1"),
            "https://github.com/mattv8/peppy/releases/tag/v1.2.3-staging.1"
        );
    }

    #[test]
    fn network_check_failure_releases_operation_and_records_error() {
        let mut inner = state();
        assert!(begin_check(&mut inner));
        mark_check_failure(&mut inner);
        assert_eq!(inner.operation, Operation::Idle);
        assert_eq!(inner.status.phase, UpdatePhase::Error);
        assert_eq!(
            inner.status.message.as_deref(),
            Some("Could not check for updates. Try again later.")
        );
        assert!(inner.retry_schedule.next_delay() > Duration::from_secs(15 * 60));
    }

    #[test]
    fn operation_admission_is_exclusive_across_check_and_install() {
        let mut inner = state();
        assert!(begin_check(&mut inner));
        assert!(!begin_check(&mut inner));
        assert_ne!(inner.operation, Operation::Idle);
        mark_check_failure(&mut inner);
        assert_eq!(inner.operation, Operation::Idle);
    }

    #[test]
    fn failed_attempt_suppresses_only_the_same_target_version() {
        let attempted = Some("1.0.1".into());
        assert!(suppresses_attempt(&attempted, "1.0.1"));
        assert!(!suppresses_attempt(&attempted, "1.0.2"));
        assert!(!suppresses_attempt(&None, "1.0.1"));
    }

    #[test]
    fn external_restart_requires_a_new_unattempted_version_without_a_staged_payload() {
        let mut inner = state();
        assert!(external_restart_eligible(&inner, "1.0.1"));
        inner.attempted_version = Some("1.0.1".into());
        assert!(!external_restart_eligible(&inner, "1.0.1"));
        assert!(!external_restart_eligible(&inner, "1.0.0"));
    }
}
