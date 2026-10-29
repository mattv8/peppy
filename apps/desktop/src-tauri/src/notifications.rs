//! Per-desktop banner delivery. Preferences are vault state owned by core; the legacy
//! per-installation file seeds at most one vault before it is retired.
use crate::{
    credentials::Binding,
    error::{BridgeError, BridgeResult},
    session::Session,
    tray, AppState,
};
use peppy_client_core::{BannerCandidate, Client};
pub use peppy_desktop_api::{NotificationPreferences, Preview};
use serde::Deserialize;
use std::{
    fs,
    path::{Path, PathBuf},
    sync::Mutex,
    time::{SystemTime, UNIX_EPOCH},
};
use tauri::{AppHandle, Manager};

const PREFERENCES_FILE: &str = "notification-preferences.json";
const SOUND_FILE: &str = "notification-sound.json";
const MAX_PREFERENCES_BYTES: u64 = 8 * 1024;
const BANNER_BATCH: usize = 20;
const MAX_BANNER_BATCHES_PER_WAKE: usize = 5;
const BANNER_BURST_SUMMARY_THRESHOLD: usize = 3;
const STALE_CANDIDATE_MS: i64 = 2 * 60 * 1000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NotificationView {
    Conversations,
    Notifications,
    Settings,
    Contacts,
}

#[derive(Default)]
pub struct NotificationContext {
    pub view: Option<NotificationView>,
    pub conversation_id: Option<String>,
}

pub struct NotificationSettings {
    root: PathBuf,
    path: PathBuf,
    /// Serializes legacy claim, core adoption, and retirement across snapshot and banner reads.
    migration_lock: Mutex<()>,
    pub context: Mutex<NotificationContext>,
    /// Serializes core queue read/post/ack without holding the application/session mutex.
    drain_lock: Mutex<()>,
    banner_drain: Mutex<BannerDrainState>,
    sound: Mutex<bool>,
}

#[derive(Default)]
struct BannerDrainState {
    running: bool,
    trailing: bool,
}

#[derive(Clone, Copy)]
pub enum PreferenceRead {
    Snapshot,
    Delivery,
}

impl NotificationSettings {
    pub fn sound_enabled(&self) -> bool {
        *self
            .sound
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    pub fn set_sound_enabled(&self, enabled: bool) -> BridgeResult<()> {
        let mut sound = self
            .sound
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        crate::fsutil::write_private_atomic(
            &self.root.join(SOUND_FILE),
            if enabled { b"true" } else { b"false" },
        )
        .map_err(|_| {
            BridgeError::new(
                "notification-sound",
                "Could not save notification sound settings.",
            )
        })?;
        *sound = enabled;
        Ok(())
    }

    pub fn load(root: &Path) -> Self {
        let path = root.join(PREFERENCES_FILE);
        Self {
            root: root.to_owned(),
            path,
            migration_lock: Mutex::new(()),
            context: Mutex::new(NotificationContext::default()),
            drain_lock: Mutex::new(()),
            banner_drain: Mutex::new(BannerDrainState::default()),
            sound: Mutex::new(
                fs::read(root.join(SOUND_FILE))
                    .ok()
                    .and_then(|bytes| serde_json::from_slice::<bool>(&bytes).ok())
                    .unwrap_or(false),
            ),
        }
    }

    /// Resolves the vault value and performs a crash-safe, vault-bound legacy adoption.
    ///
    /// The legacy filename is renamed into the binding's private data directory before core sees
    /// it. A restart can therefore retry only that binding; another vault cannot race a clone.
    pub fn notification_preferences(
        &self,
        client: &Client,
        binding: &Binding,
        read: PreferenceRead,
    ) -> BridgeResult<(NotificationPreferences, bool)> {
        let _migration = self
            .migration_lock
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let current = client
            .notification_preferences()
            .map_err(crate::error::core_error)?;
        if let Some(preferences) = current {
            if let Err(error) = self.retire_legacy_after_established_value(binding) {
                eprintln!(
                    "peppy: notification preference legacy cleanup failed: {}",
                    error.code
                );
            }
            return Ok((preferences, false));
        }

        let key_status = client.key_status().map_err(crate::error::core_error)?;
        let unlocked = key_status
            .active_epoch
            .is_some_and(|epoch| key_status.unlocked_epochs.contains(&epoch));
        if !unlocked {
            return self.unresolved_preferences(binding, read);
        }

        let Some(legacy) = self.claim_legacy(binding)? else {
            return Ok((NotificationPreferences::default(), false));
        };
        let adopted = client
            .adopt_legacy_notification_preferences(legacy)
            .map_err(crate::error::core_error)?;
        if adopted {
            self.retire_claim(binding)?;
            return Ok((
                client
                    .notification_preferences()
                    .map_err(crate::error::core_error)?
                    .unwrap_or_default(),
                true,
            ));
        }
        if let Some(preferences) = client
            .notification_preferences()
            .map_err(crate::error::core_error)?
        {
            self.retire_claim(binding)?;
            return Ok((preferences, false));
        }
        self.unresolved_preferences(binding, read)
    }

    pub(crate) fn claim_path(&self, binding: &Binding) -> PathBuf {
        binding
            .data_dir(&self.root)
            .join("notification-preferences.legacy.json")
    }

    fn pending_legacy(&self, binding: &Binding) -> BridgeResult<bool> {
        Ok(read_claimed_legacy(&self.claim_path(binding))?.is_some()
            || read_unclaimed_legacy(&self.path).is_some())
    }

    fn unresolved_preferences(
        &self,
        binding: &Binding,
        read: PreferenceRead,
    ) -> BridgeResult<(NotificationPreferences, bool)> {
        if matches!(read, PreferenceRead::Delivery) && self.pending_legacy(binding)? {
            return Err(legacy_file_error());
        }
        Ok((NotificationPreferences::default(), false))
    }

    fn claim_legacy(&self, binding: &Binding) -> BridgeResult<Option<NotificationPreferences>> {
        let claim = self.claim_path(binding);
        if let Some(preferences) = read_claimed_legacy(&claim)? {
            return Ok(Some(preferences));
        }
        if claim.exists() {
            return Err(legacy_file_error());
        }
        if read_unclaimed_legacy(&self.path).is_none() {
            return Ok(None);
        }
        match fs::rename(&self.path, &claim) {
            Ok(()) => {
                sync_parent(&self.path)?;
                sync_parent(&claim)?;
                read_claimed_legacy(&claim)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                match fs::metadata(&self.path) {
                    Err(source_error) if source_error.kind() == std::io::ErrorKind::NotFound => {
                        Ok(None)
                    }
                    _ => Err(legacy_file_error()),
                }
            }
            Err(_) => Err(legacy_file_error()),
        }
    }

    fn retire_claim(&self, binding: &Binding) -> BridgeResult<()> {
        let claim = self.claim_path(binding);
        fs::remove_file(&claim).map_err(|_| legacy_file_error())?;
        sync_parent(&claim)
    }

    fn retire_legacy_after_established_value(&self, binding: &Binding) -> BridgeResult<()> {
        if self.claim_legacy(binding)?.is_some() {
            self.retire_claim(binding)?;
        }
        Ok(())
    }

    pub fn set_context(&self, view: NotificationView, conversation_id: Option<String>) {
        *self.context.lock().unwrap_or_else(|p| p.into_inner()) = NotificationContext {
            view: Some(view),
            conversation_id,
        };
    }

    /// Starts one drain or records one coalesced trailing drain while it is active.
    pub fn request_banner_drain(&self) -> bool {
        let mut drain = self.banner_drain.lock().unwrap_or_else(|p| p.into_inner());
        if drain.running {
            drain.trailing = true;
            false
        } else {
            drain.running = true;
            true
        }
    }

    /// Releases the single-flight drain, or consumes its coalesced trailing request.
    pub fn finish_banner_drain(&self) -> bool {
        let mut drain = self.banner_drain.lock().unwrap_or_else(|p| p.into_inner());
        if drain.trailing {
            drain.trailing = false;
            true
        } else {
            drain.running = false;
            false
        }
    }
}

fn read_unclaimed_legacy(path: &Path) -> Option<NotificationPreferences> {
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(_) => return None,
    };
    if !metadata.is_file() || metadata.len() > MAX_PREFERENCES_BYTES {
        return None;
    }
    serde_json::from_slice(&fs::read(path).ok()?).ok().filter(
        |preferences: &NotificationPreferences| preferences != &NotificationPreferences::default(),
    )
}

fn read_claimed_legacy(path: &Path) -> BridgeResult<Option<NotificationPreferences>> {
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(legacy_file_error()),
    };
    if !metadata.is_file() || metadata.len() > MAX_PREFERENCES_BYTES {
        return Err(legacy_file_error());
    }
    let preferences = serde_json::from_slice(&fs::read(path).map_err(|_| legacy_file_error())?)
        .map_err(|_| legacy_file_error())?;
    if preferences == NotificationPreferences::default() {
        return Err(legacy_file_error());
    }
    Ok(Some(preferences))
}

fn sync_parent(path: &Path) -> BridgeResult<()> {
    #[cfg(unix)]
    fs::File::open(path.parent().expect("notification preferences parent"))
        .and_then(|parent| parent.sync_all())
        .map_err(|_| legacy_file_error())?;
    Ok(())
}

fn legacy_file_error() -> BridgeError {
    BridgeError::new(
        "notification-preferences",
        "Could not migrate notification preferences.",
    )
}

fn focused_for_candidate(
    app: &AppHandle,
    settings: &NotificationSettings,
    candidate: &BannerCandidate,
) -> bool {
    let main_focused = app
        .get_webview_window(tray::MAIN)
        .and_then(|window| window.is_focused().ok())
        .unwrap_or(false);
    let context = settings.context.lock().unwrap_or_else(|p| p.into_inner());
    if should_suppress_main_focus(main_focused, &context, candidate) {
        return true;
    }
    candidate
        .conversation_id
        .as_ref()
        .is_some_and(|conversation| {
            app.get_webview_window(&format!("{}{}", tray::COMPOSER_PREFIX, conversation))
                .and_then(|window| window.is_focused().ok())
                .unwrap_or(false)
        })
}

fn should_suppress_main_focus(
    main_focused: bool,
    context: &NotificationContext,
    candidate: &BannerCandidate,
) -> bool {
    if !main_focused {
        return false;
    }
    match context.view {
        Some(NotificationView::Notifications) => candidate.kind == "notification",
        Some(NotificationView::Conversations) => context
            .conversation_id
            .as_deref()
            .zip(candidate.conversation_id.as_deref())
            .is_some_and(|(active, candidate)| active == candidate),
        _ => false,
    }
}

fn allowed(preferences: &NotificationPreferences, candidate: &BannerCandidate) -> bool {
    match candidate.kind.as_str() {
        "message" => preferences.message_banners,
        "notification" => preferences.mirrored_banners,
        _ => false,
    }
}

/// Only SMS/MMS banners for a conversation with a live chat head are muted. Mirrored app
/// notifications and other conversations keep their banners.
fn suppressed_by_head(
    preferences: &NotificationPreferences,
    candidate: &BannerCandidate,
    has_head: impl Fn(&str) -> bool,
) -> bool {
    preferences.mute_with_heads
        && candidate.kind == "message"
        && candidate.conversation_id.as_deref().is_some_and(has_head)
}

fn banner_text(
    preferences: &NotificationPreferences,
    candidate: &BannerCandidate,
) -> (String, String) {
    match preferences.preview {
        Preview::Full => (candidate.title.clone(), candidate.body.clone()),
        Preview::Hidden => match candidate.kind.as_str() {
            "message" => ("New message".into(), "Open Peppy to view it.".into()),
            _ => ("New notification".into(), "Open Peppy to view it.".into()),
        },
    }
}

/// Drains only durable live candidates produced by core. A candidate is acknowledged after an OS
/// acceptance (or intentional policy suppression), leaving the unavoidable crash window after
/// delivery and before acknowledgement explicit. Snapshot/history imports never create these rows.
pub fn drain_banner_candidates(
    app: &AppHandle,
    session: &Session,
    settings: &NotificationSettings,
) -> BridgeResult<bool> {
    drain_with_delivery(
        session,
        settings,
        || {
            app.state::<AppState>()
                .app_locked
                .load(std::sync::atomic::Ordering::Acquire)
        },
        |candidate, preferences| {
            focused_for_candidate(app, settings, candidate)
                || suppressed_by_head(preferences, candidate, |id| {
                    crate::heads_runtime::has_head(app, id)
                })
        },
        |destination, title, body| {
            crate::notification_activation::post(app, session, destination, title, body)
        },
    )
}

pub(crate) fn drain_with_delivery(
    session: &Session,
    settings: &NotificationSettings,
    app_locked: impl Fn() -> bool,
    suppress: impl Fn(&BannerCandidate, &NotificationPreferences) -> bool,
    mut post: impl FnMut(
        peppy_desktop_api::NotificationActivationDestination,
        String,
        String,
    ) -> Result<(), crate::notification_activation::PostError>,
) -> BridgeResult<bool> {
    let _drain = settings
        .drain_lock
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let mut preferences = session.delivery_notification_preferences(settings)?;
    if app_locked() {
        preferences.preview = Preview::Hidden;
    }
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(i64::MAX);
    let mut more = false;
    for _ in 0..MAX_BANNER_BATCHES_PER_WAKE {
        let candidates = session
            .client
            .pending_banner_candidates(BANNER_BATCH)
            .map_err(crate::error::core_error)?;
        if candidates.is_empty() {
            break;
        }
        let mut acknowledge = Vec::with_capacity(candidates.len());
        let mut summary_count = 0usize;
        let mut summary_ids = Vec::new();
        let mut recent = Vec::new();
        for candidate in candidates {
            if !allowed(&preferences, &candidate) || suppress(&candidate, &preferences) {
                acknowledge.push(candidate.id);
            } else if now.saturating_sub(candidate.created_at) > STALE_CANDIDATE_MS {
                summary_count += 1;
                summary_ids.push(candidate.id);
            } else {
                recent.push(candidate);
            }
        }
        if recent.len() > BANNER_BURST_SUMMARY_THRESHOLD {
            summary_count += recent.len();
            summary_ids.extend(recent.into_iter().map(|candidate| candidate.id));
        } else {
            for candidate in recent {
                let (mut title, body) = banner_text(&preferences, &candidate);
                // Full previews only: a phone-number title may show the contact's name. Hidden
                // previews stay generic and never resolve names.
                if matches!(preferences.preview, Preview::Full) {
                    let source = candidate
                        .notification_target
                        .as_ref()
                        .map(|target| target.source_device_id.as_str());
                    title = crate::contacts::banner_title(&session.client, &title, source);
                }
                match post(
                    crate::notification_activation::destination(&candidate),
                    title,
                    body,
                ) {
                    Ok(()) | Err(crate::notification_activation::PostError::NotAllowed) => {
                        acknowledge.push(candidate.id);
                    }
                    Err(crate::notification_activation::PostError::AwaitingAuthorization) => {
                        if !acknowledge.is_empty() {
                            session
                                .client
                                .ack_banner_candidates(acknowledge)
                                .map_err(crate::error::core_error)?;
                        }
                        session.set_status(|status| {
                            if status.work_error == Some("notification-post") {
                                status.work_error = None;
                            }
                        });
                        // Candidates are ordered, and every later candidate needs the same OS
                        // authorization. Hold this and the remaining queue until it resolves.
                        return Ok(false);
                    }
                    Err(crate::notification_activation::PostError::Failed) => {
                        if !acknowledge.is_empty() {
                            session
                                .client
                                .ack_banner_candidates(acknowledge)
                                .map_err(crate::error::core_error)?;
                        }
                        session.set_status(|status| status.work_error = Some("notification-post"));
                        return Err(BridgeError::new(
                            "notification-post",
                            "Could not post a native notification.",
                        ));
                    }
                }
            }
        }
        if summary_count > 0 && (preferences.message_banners || preferences.mirrored_banners) {
            match post(
                peppy_desktop_api::NotificationActivationDestination::Main,
                "Peppy".into(),
                format!("{summary_count} new items arrived while Peppy was away."),
            ) {
                Ok(()) | Err(crate::notification_activation::PostError::NotAllowed) => {}
                Err(crate::notification_activation::PostError::AwaitingAuthorization) => {
                    if !acknowledge.is_empty() {
                        session
                            .client
                            .ack_banner_candidates(acknowledge)
                            .map_err(crate::error::core_error)?;
                    }
                    session.set_status(|status| {
                        if status.work_error == Some("notification-post") {
                            status.work_error = None;
                        }
                    });
                    return Ok(false);
                }
                Err(crate::notification_activation::PostError::Failed) => {
                    if !acknowledge.is_empty() {
                        session
                            .client
                            .ack_banner_candidates(acknowledge)
                            .map_err(crate::error::core_error)?;
                    }
                    session.set_status(|status| status.work_error = Some("notification-post"));
                    return Err(BridgeError::new(
                        "notification-post",
                        "Could not post a native notification.",
                    ));
                }
            }
        }
        acknowledge.extend(summary_ids);
        session
            .client
            .ack_banner_candidates(acknowledge)
            .map_err(crate::error::core_error)?;
        more = session
            .client
            .pending_banner_candidates(1)
            .map_err(crate::error::core_error)?
            .len()
            == 1;
        if !more {
            break;
        }
    }
    if !more {
        session.set_status(|status| {
            if status.work_error == Some("notification-post") {
                status.work_error = None;
            }
        });
    }
    Ok(more)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notification_sound_is_opt_in_and_survives_reopening() {
        let dir = tempfile::tempdir().unwrap();
        let settings = NotificationSettings::load(dir.path());
        assert!(!settings.sound_enabled());
        settings.set_sound_enabled(true).unwrap();
        assert!(settings.sound_enabled());
        assert!(NotificationSettings::load(dir.path()).sound_enabled());
        settings.set_sound_enabled(false).unwrap();
        assert!(!NotificationSettings::load(dir.path()).sound_enabled());
    }

    #[test]
    fn notification_sound_failed_save_keeps_last_applied_choice() {
        let dir = tempfile::tempdir().unwrap();
        let settings = NotificationSettings::load(dir.path());
        settings.set_sound_enabled(true).unwrap();
        fs::remove_file(dir.path().join("notification-sound.json")).unwrap();
        fs::create_dir(dir.path().join("notification-sound.json")).unwrap();
        assert!(settings.set_sound_enabled(false).is_err());
        assert!(settings.sound_enabled());
    }

    fn binding(vault_id: &str) -> Binding {
        Binding {
            origin: "https://example.test".into(),
            vault_id: vault_id.into(),
            device_id: "device".into(),
        }
    }

    #[test]
    fn legacy_file_is_read_only_adoption_input() {
        let dir = tempfile::tempdir().unwrap();
        let preferences = NotificationPreferences {
            message_banners: false,
            mirrored_banners: true,
            preview: Preview::Hidden,
            ..NotificationPreferences::default()
        };
        fs::write(
            dir.path().join(PREFERENCES_FILE),
            serde_json::to_vec(&preferences).unwrap(),
        )
        .unwrap();
        let settings = NotificationSettings::load(dir.path());
        let vault = binding("vault-a");
        fs::create_dir_all(vault.data_dir(dir.path())).unwrap();
        assert_eq!(settings.claim_legacy(&vault).unwrap(), Some(preferences));
        assert!(!dir.path().join(PREFERENCES_FILE).exists());
        assert!(settings.claim_path(&vault).exists());
    }

    #[test]
    fn legacy_claim_is_vault_bound_across_restart() {
        let dir = tempfile::tempdir().unwrap();
        let first = binding("vault-a");
        let second = binding("vault-b");
        fs::create_dir_all(first.data_dir(dir.path())).unwrap();
        fs::create_dir_all(second.data_dir(dir.path())).unwrap();
        fs::write(
            dir.path().join(PREFERENCES_FILE),
            br#"{"messageBanners":true,"mirroredBanners":true,"preview":"hidden"}"#,
        )
        .unwrap();
        let settings = NotificationSettings::load(dir.path());
        assert!(settings.claim_legacy(&first).unwrap().is_some());
        assert!(settings.claim_legacy(&second).unwrap().is_none());

        let restarted = NotificationSettings::load(dir.path());
        assert!(restarted.claim_legacy(&first).unwrap().is_some());
        assert!(restarted.claim_legacy(&second).unwrap().is_none());
    }

    #[test]
    fn legacy_claim_failure_is_reported_without_consuming_source() {
        let dir = tempfile::tempdir().unwrap();
        let vault = binding("vault-a");
        fs::write(
            dir.path().join(PREFERENCES_FILE),
            br#"{"messageBanners":true,"mirroredBanners":true,"preview":"hidden"}"#,
        )
        .unwrap();
        let settings = NotificationSettings::load(dir.path());
        assert!(settings.claim_legacy(&vault).is_err());
        assert!(dir.path().join(PREFERENCES_FILE).exists());
    }

    #[test]
    fn missing_or_oversized_legacy_file_is_not_adopted() {
        let dir = tempfile::tempdir().unwrap();
        let vault = binding("vault-a");
        fs::create_dir_all(vault.data_dir(dir.path())).unwrap();
        let settings = NotificationSettings::load(dir.path());
        assert!(settings.claim_legacy(&vault).unwrap().is_none());
        fs::write(dir.path().join(PREFERENCES_FILE), vec![b' '; 9 * 1024]).unwrap();
        assert!(settings.claim_legacy(&vault).unwrap().is_none());
    }

    #[test]
    fn malformed_unclaimed_legacy_file_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let vault = binding("vault-a");
        fs::create_dir_all(vault.data_dir(dir.path())).unwrap();
        fs::write(dir.path().join(PREFERENCES_FILE), b"not json").unwrap();
        let settings = NotificationSettings::load(dir.path());

        assert!(settings.claim_legacy(&vault).unwrap().is_none());
        assert!(dir.path().join(PREFERENCES_FILE).exists());
    }

    #[test]
    fn malformed_claimed_legacy_file_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let vault = binding("vault-a");
        fs::create_dir_all(vault.data_dir(dir.path())).unwrap();
        let settings = NotificationSettings::load(dir.path());
        fs::write(settings.claim_path(&vault), b"not json").unwrap();

        assert!(settings.claim_legacy(&vault).is_err());
    }

    #[test]
    fn banner_drain_is_single_flight_with_one_trailing_run() {
        let dir = tempfile::tempdir().unwrap();
        let settings = NotificationSettings::load(dir.path());

        assert!(settings.request_banner_drain());
        assert!(!settings.request_banner_drain());
        assert!(!settings.request_banner_drain());
        assert!(settings.finish_banner_drain());
        assert!(!settings.finish_banner_drain());
        assert!(settings.request_banner_drain());
    }

    #[test]
    fn hidden_preview_never_uses_candidate_content() {
        let candidate = BannerCandidate {
            id: "candidate".into(),
            kind: "notification".into(),
            conversation_id: None,
            notification_target: None,
            title: "private title".into(),
            body: "private body".into(),
            created_at: 0,
        };
        let rendered = banner_text(
            &NotificationPreferences {
                preview: Preview::Hidden,
                ..NotificationPreferences::default()
            },
            &candidate,
        );
        assert!(!rendered.0.contains("private"));
        assert!(!rendered.1.contains("private"));
        assert!(allowed(&NotificationPreferences::default(), &candidate));
    }

    #[test]
    fn focus_policy_only_suppresses_matching_content() {
        let message = BannerCandidate {
            id: "message".into(),
            kind: "message".into(),
            conversation_id: Some("conversation-a".into()),
            notification_target: None,
            title: String::new(),
            body: String::new(),
            created_at: 0,
        };
        let mirrored = BannerCandidate {
            id: "mirror".into(),
            kind: "notification".into(),
            conversation_id: None,
            notification_target: None,
            title: String::new(),
            body: String::new(),
            created_at: 0,
        };
        let notifications = NotificationContext {
            view: Some(NotificationView::Notifications),
            conversation_id: None,
        };
        assert!(should_suppress_main_focus(true, &notifications, &mirrored));
        assert!(!should_suppress_main_focus(true, &notifications, &message));
        let conversations = NotificationContext {
            view: Some(NotificationView::Conversations),
            conversation_id: Some("conversation-a".into()),
        };
        assert!(should_suppress_main_focus(true, &conversations, &message));
        assert!(!should_suppress_main_focus(true, &conversations, &mirrored));
    }

    #[test]
    fn head_suppression_only_mutes_messages_with_a_head_when_enabled() {
        let message = |conversation: Option<&str>| BannerCandidate {
            id: "message".into(),
            kind: "message".into(),
            conversation_id: conversation.map(Into::into),
            notification_target: None,
            title: String::new(),
            body: String::new(),
            created_at: 0,
        };
        let headed = |id: &str| id == "conversation-a";
        let enabled = NotificationPreferences::default();
        let disabled = NotificationPreferences {
            mute_with_heads: false,
            ..NotificationPreferences::default()
        };

        assert!(suppressed_by_head(
            &enabled,
            &message(Some("conversation-a")),
            headed
        ));
        assert!(!suppressed_by_head(
            &enabled,
            &message(Some("conversation-b")),
            headed
        ));
        assert!(!suppressed_by_head(&enabled, &message(None), headed));
        assert!(!suppressed_by_head(
            &disabled,
            &message(Some("conversation-a")),
            headed
        ));
        let mirrored = BannerCandidate {
            kind: "notification".into(),
            ..message(Some("conversation-a"))
        };
        assert!(!suppressed_by_head(&enabled, &mirrored, headed));
    }

    #[test]
    fn finite_wake_bounds_burst_processing() {
        assert_eq!(BANNER_BATCH * MAX_BANNER_BATCHES_PER_WAKE, 100);
        const {
            assert!(BANNER_BATCH >= 20);
            assert!(BANNER_BURST_SUMMARY_THRESHOLD < BANNER_BATCH);
        }
    }
}
