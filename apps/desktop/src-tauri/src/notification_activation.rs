//! Running-process notification activation. OS callbacks retain identifiers only.
use crate::{error::BridgeResult, session::Session, AppState};
use peppy_client_core::OneTimeCodeTarget;
use peppy_desktop_api::{
    NotificationActivation, NotificationActivationDestination as Destination,
    NotificationActivationTarget,
};
use std::sync::{atomic::Ordering, Arc, Mutex};
use tauri::{Emitter, Manager};

mod action_availability;
use action_availability::ActionAvailability;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(any(target_os = "macos", target_os = "windows"))]
mod owner;
pub(crate) mod quick_reply;
mod recovery_store;
mod test_notification;
#[cfg(target_os = "windows")]
mod windows;
#[cfg(any(target_os = "windows", test))]
mod windows_codec;
#[cfg(any(target_os = "windows", test))]
mod windows_xml;

pub const EVENT: &str = "peppy://notification-activation";
pub const MAX_REPLY_BYTES: usize = 64 * 1024;

/// Native-only actions whose OS arguments carry only allowlisted action IDs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PopupAction {
    Open,
    Copy,
    Reveal,
    Reply(String),
}

/// Host-owned capability retained by the owner until terminal retirement.
#[derive(Clone)]
pub(crate) struct PopupActions {
    pub reveal: bool,
    pub reply: bool,
    pub handler: Arc<dyn Fn(PopupAction) + Send + Sync>,
}

impl PopupActions {
    pub(crate) fn enabled(&self) -> bool {
        cfg!(feature = "notification-quick-reply") && (self.reveal || self.reply)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PostError {
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    AwaitingAuthorization,
    #[cfg_attr(not(any(target_os = "macos", target_os = "windows")), allow(dead_code))]
    NotAllowed,
    Failed,
}

#[cfg(any(target_os = "windows", test))]
pub(super) fn windows_audio(sound: bool) -> &'static str {
    if sound {
        "<audio src=\"ms-winsoundevent:Notification.Default\"/>"
    } else {
        "<audio silent=\"true\"/>"
    }
}
pub(crate) mod fallback;

#[derive(Default)]
pub struct ActivationState {
    pending: Mutex<Pending>,
    /// Serializes host queue admission with lock/session retirement.
    pub(crate) effect_gate: Mutex<()>,
    quick_reply_cancel: Mutex<tokio_util::sync::CancellationToken>,
    pub(super) recovery: Arc<recovery_store::RecoveryStore>,
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    owner: Mutex<Option<owner::Owner>>,
}

impl ActivationState {
    pub fn quick_reply_generation_cancel(&self) -> tokio_util::sync::CancellationToken {
        self.quick_reply_cancel.lock().unwrap().clone()
    }
    /// QA feature builds still require a successfully installed native owner.
    pub fn quick_reply_supported(&self) -> bool {
        #[cfg(all(
            feature = "notification-quick-reply",
            any(target_os = "macos", target_os = "windows")
        ))]
        return self.owner.lock().unwrap().is_some();
        #[cfg(not(all(
            feature = "notification-quick-reply",
            any(target_os = "macos", target_os = "windows")
        )))]
        false
    }
}

pub fn destination(candidate: &peppy_client_core::BannerCandidate) -> Destination {
    if candidate.kind == "notification" {
        if let Some(target) = &candidate.notification_target {
            return Destination::Mirrored {
                target: NotificationActivationTarget {
                    source_device_id: target.source_device_id.clone(),
                    notification_key: target.notification_key.clone(),
                    lifetime: target.lifetime.clone(),
                },
            };
        }
    } else if candidate.kind == "message" {
        if let Some(conversation_id) = &candidate.conversation_id {
            return Destination::Conversation {
                conversation_id: conversation_id.clone(),
            };
        }
    }
    Destination::Main
}

pub fn install(app: &tauri::AppHandle) {
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    {
        *app.state::<AppState>()
            .notification_activation
            .owner
            .lock()
            .unwrap() = owner::Owner::start(app.clone());
    }
}

pub fn post(
    app: &tauri::AppHandle,
    session: &Session,
    destination: Destination,
    title: String,
    body: String,
    icon: Option<crate::notification_icon::Icon>,
    copy: Option<OneTimeCodeTarget>,
) -> Result<(), PostError> {
    post_with_activation(
        app,
        Some(NotificationActivation {
            request_id: uuid::Uuid::new_v4().simple().to_string(),
            session_generation: session.generation.clone(),
            destination,
        }),
        session.cancel.clone(),
        title,
        body,
        icon,
        copy,
    )
}

pub fn post_test(app: &tauri::AppHandle, title: &str, body: &str) -> Result<(), ()> {
    if test_notification::post(app).is_ok_and(|posted| posted) {
        return Ok(());
    }
    post_ordinary_test(app, title, body)
}

fn post_ordinary_test(app: &tauri::AppHandle, title: &str, body: &str) -> Result<(), ()> {
    post_with_activation(
        app,
        None,
        tokio_util::sync::CancellationToken::new(),
        title.to_owned(),
        body.to_owned(),
        None,
        None,
    )
    .map_err(|_| ())
}

fn post_with_activation(
    app: &tauri::AppHandle,
    activation: Option<NotificationActivation>,
    cancel: tokio_util::sync::CancellationToken,
    title: String,
    body: String,
    icon: Option<crate::notification_icon::Icon>,
    copy: Option<OneTimeCodeTarget>,
) -> Result<(), PostError> {
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    {
        let state = app.state::<AppState>();
        let guard = state.notification_activation.owner.lock().unwrap();
        if let Some(owner) = guard.as_ref() {
            let acceptance = owner.submit(owner::Post {
                activation,
                cancel,
                sound: state.notifications.sound_enabled(),
                title,
                body,
                icon,
                copy,
                popup_actions: None,
            });
            // OS completion can wake state listeners, and shutdown takes this
            // mutex. Never hold it while waiting for backend acceptance.
            drop(guard);
            return acceptance.and_then(owner::Acceptance::wait);
        }
    }
    // Only the Linux fallback posts Copy actions; it needs the posting generation to resolve the
    // session at action time. Other fallbacks carry no copy request.
    #[cfg(target_os = "linux")]
    let fallback_copy = copy.and_then(|target| {
        activation.as_ref().map(|activation| fallback::CopyRequest {
            session_generation: activation.session_generation.clone(),
            target,
        })
    });
    #[cfg(not(target_os = "linux"))]
    let fallback_copy = {
        let _ = (activation, copy);
        None
    };
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let _ = cancel;
    fallback::post(app, &title, &body, icon, fallback_copy).map_err(|_| PostError::Failed)
}

/// Runs a popup's Copy action. The owner thread has already checked the generation; this
/// repeats the check under the session guard so retirement that lands in between still
/// wins. Copy never navigates, shows a window, or reports an error surface.
#[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
fn copy_from_popup(app: &tauri::AppHandle, generation: &str, target: &OneTimeCodeTarget) {
    let state = app.state::<AppState>();
    // The guard is held through the clipboard write on purpose. Retirement cancels while it holds
    // this guard, so a copy that passed the check finishes before its session can retire. The
    // write never takes the session lock (it only uses the session it was handed), so there is no
    // lock cycle. Command handlers similarly hold their session while blocking work runs.
    let session = tauri::async_runtime::block_on(state.session.lock());
    copy_from_popup_in(
        &state,
        session.as_deref(),
        generation,
        target,
        crate::one_time_code::write_clipboard_text,
    );
}

/// The popup Copy gate: the same generation, cancellation and unlock rules as the in-app
/// notification commands. A locked app, stale generation or revoked session writes nothing.
#[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
fn copy_from_popup_in(
    state: &AppState,
    session: Option<&Session>,
    generation: &str,
    target: &OneTimeCodeTarget,
    write: impl FnOnce(&str) -> Result<(), ()>,
) {
    let Some(session) =
        session.filter(|session| current(Some(session), generation) && unlocked(state, session))
    else {
        return;
    };
    let _ = crate::one_time_code::copy_and_mark_with(session, target, None, write);
}

/// Owner-thread admission for an activation: its cancellation and session generation must
/// still be current before any click or copy is published.
#[cfg(any(target_os = "macos", target_os = "windows", test))]
fn activation_admitted(
    session: Option<&Session>,
    cancel: &tokio_util::sync::CancellationToken,
    generation: &str,
) -> bool {
    !cancel.is_cancelled() && current(session, generation)
}

impl ActivationState {
    pub fn invalidate_quick_reply(&self) {
        let _gate = self.effect_gate.lock().unwrap_or_else(|p| p.into_inner());
        self.invalidate_quick_reply_under_gate();
    }

    pub(crate) fn invalidate_quick_reply_under_gate(&self) {
        let mut generation = self.quick_reply_cancel.lock().unwrap();
        generation.cancel();
        *generation = tokio_util::sync::CancellationToken::new();
        drop(generation);
        self.recovery.clear();
        self.wake();
    }

    pub fn retire(&self) {
        self.pending.lock().unwrap().0 = None;
        self.recovery.clear();
        self.wake();
    }

    pub fn wake(&self) {
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        if let Some(owner) = self.owner.lock().unwrap().as_ref() {
            owner.wake();
        }
    }

    pub fn shutdown(&self) {
        self.recovery.clear();
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        {
            let owner = self.owner.lock().unwrap().take();
            if let Some(owner) = owner {
                owner.stop();
            }
        }
    }
}

fn unlocked(state: &AppState, session: &Session) -> bool {
    if state.app_locked.load(Ordering::Acquire) {
        return false;
    }
    let Ok(keys) = session.client.key_status() else {
        return false;
    };
    let status = session.status.lock().unwrap();
    !status.revoked
        && matches!(
            crate::session::encryption_state(
                session.mismatch.load(Ordering::Acquire),
                status.vault.as_ref(),
                &keys
            )
            .0,
            crate::session::EncryptionState::Unlocked { .. }
        )
}

fn current(session: Option<&Session>, generation: &str) -> bool {
    session.is_some_and(|session| {
        session.generation == generation
            && !session.cancel.is_cancelled()
            && !session.status.lock().unwrap().revoked
    })
}

#[tauri::command]
pub async fn peek_notification_activation(
    window: tauri::WebviewWindow,
    state: tauri::State<'_, AppState>,
) -> BridgeResult<Option<NotificationActivation>> {
    crate::require_main(window.label())?;
    let session = state.session.lock().await;
    let mut pending = state.notification_activation.pending.lock().unwrap();
    let active = session
        .as_deref()
        .filter(|session| current(Some(session), &session.generation));
    Ok(pending.peek(
        active.map(|session| session.generation.as_str()),
        active.is_some_and(|session| unlocked(&state, session)),
    ))
}

#[tauri::command]
pub async fn ack_notification_activation(
    window: tauri::WebviewWindow,
    state: tauri::State<'_, AppState>,
    request_id: String,
    session_generation: String,
) -> BridgeResult<bool> {
    crate::require_main(window.label())?;
    let session = state.session.lock().await;
    let mut pending = state.notification_activation.pending.lock().unwrap();
    if !current(session.as_deref(), &session_generation)
        || !unlocked(&state, session.as_deref().unwrap())
    {
        return Ok(false);
    }
    Ok(pending.ack(&request_id, &session_generation))
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
fn activate(
    app: &tauri::AppHandle,
    activation: NotificationActivation,
    cancel: &tokio_util::sync::CancellationToken,
    sequence: u64,
    copy: Option<OneTimeCodeTarget>,
) {
    let state = app.state::<AppState>();
    // Lock order is session -> pending, also used by both IPC commands.
    let session = tauri::async_runtime::block_on(state.session.lock());
    if !activation_admitted(session.as_deref(), cancel, &activation.session_generation) {
        return;
    }
    if let Some(target) = copy {
        // Hand the clipboard and Client work to a blocking thread; the owner thread only
        // checks generation and cancellation. A copy never publishes a navigation click.
        drop(session);
        let app = app.clone();
        let generation = activation.session_generation;
        drop(tauri::async_runtime::spawn_blocking(move || {
            copy_from_popup(&app, &generation, &target)
        }));
        return;
    }
    if !state
        .notification_activation
        .pending
        .lock()
        .unwrap()
        .publish(activation, sequence)
    {
        return;
    }
    drop(session);
    let handle = app.clone();
    let cancel = cancel.clone();
    let _ = app.run_on_main_thread(move || {
        // Every session retirement cancels this fence before releasing its
        // session guard. Do not wait for that guard on the UI thread.
        if cancel.is_cancelled() {
            return;
        }
        crate::tray::show_main(&handle);
        let _ = handle.emit_to(crate::tray::MAIN, EVENT, ());
    });
}

#[derive(Default)]
struct Pending(Option<NotificationActivation>, u64);

impl Pending {
    fn peek(&mut self, generation: Option<&str>, unlocked: bool) -> Option<NotificationActivation> {
        if self
            .0
            .as_ref()
            .is_some_and(|value| Some(value.session_generation.as_str()) != generation)
        {
            self.0 = None;
        }
        if unlocked {
            self.0.clone()
        } else {
            None
        }
    }
    fn publish(&mut self, activation: NotificationActivation, sequence: u64) -> bool {
        if sequence <= self.1 {
            return false;
        }
        self.0 = Some(activation);
        self.1 = sequence;
        true
    }
    fn ack(&mut self, request: &str, generation: &str) -> bool {
        if !self.0.as_ref().is_some_and(|value| {
            value.request_id == request && value.session_generation == generation
        }) {
            return false;
        }
        self.0 = None;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notification_sound_windows_requests_default_audio_only_when_enabled() {
        assert_eq!(windows_audio(false), "<audio silent=\"true\"/>");
        assert_eq!(
            windows_audio(true),
            "<audio src=\"ms-winsoundevent:Notification.Default\"/>"
        );
    }
    use peppy_desktop_api::NotificationActivationDestination;

    fn activation(request: &str, generation: &str) -> NotificationActivation {
        NotificationActivation {
            request_id: request.into(),
            session_generation: generation.into(),
            destination: NotificationActivationDestination::Main,
        }
    }

    #[test]
    fn notification_latest_click_survives_old_ack() {
        let mut pending = Pending::default();
        pending.publish(activation("first", "session"), 1);
        pending.publish(activation("second", "session"), 2);
        assert!(!pending.ack("first", "session"));
        assert_eq!(pending.0.as_ref().unwrap().request_id, "second");
        assert!(!pending.ack("second", "old-session"));
        assert!(pending.ack("second", "session"));
        assert!(pending.0.is_none());
    }

    #[test]
    fn notification_lock_defers_but_new_identity_invalidates() {
        let mut pending = Pending::default();
        pending.publish(activation("click", "first-instance"), 1);
        assert!(pending.peek(Some("first-instance"), false).is_none());
        assert_eq!(
            pending
                .peek(Some("first-instance"), true)
                .unwrap()
                .request_id,
            "click"
        );
        assert!(pending
            .peek(Some("same-binding-new-instance"), false)
            .is_none());
        assert!(pending.peek(Some("first-instance"), true).is_none());
    }

    #[test]
    fn notification_non_main_callers_are_denied() {
        assert!(crate::require_main("main").is_ok());
        for caller in ["composer-example", "head-example", "", "MAIN"] {
            assert!(crate::require_main(caller).is_err());
        }
    }

    #[test]
    fn notification_delayed_callback_cannot_replace_later_click_or_reappear_after_ack() {
        let mut pending = Pending::default();
        assert!(pending.publish(activation("later", "instance"), 2));
        assert!(!pending.publish(activation("delayed", "instance"), 1));
        assert_eq!(
            pending.peek(Some("instance"), true).unwrap().request_id,
            "later"
        );
        assert!(pending.ack("later", "instance"));
        assert!(!pending.publish(activation("delayed", "instance"), 1));
        assert!(pending.peek(Some("instance"), true).is_none());
    }

    #[test]
    fn notification_same_binding_reopened_is_a_new_instance_and_revocation_rejects() {
        let fixture = crate::tests::fixture_at("http://127.0.0.1:9");
        let first = crate::tests::open(&fixture, &fixture.binding, &[]);
        let old_generation = first.generation.clone();
        assert!(current(Some(&first), &old_generation));
        first.cancel.cancel();
        assert!(!current(Some(&first), &old_generation));
        drop(first);
        let replacement = crate::tests::open(&fixture, &fixture.binding, &[]);
        assert_ne!(replacement.generation, old_generation);
        assert!(!current(Some(&replacement), &old_generation));
        assert!(current(Some(&replacement), &replacement.generation));
        replacement.status.lock().unwrap().revoked = true;
        assert!(!current(Some(&replacement), &replacement.generation));
    }

    #[test]
    fn notification_native_app_and_sync_locks_both_gate_identifiers() {
        use std::sync::Arc;
        let fixture = crate::tests::fixture_at("http://127.0.0.1:9");
        let session = crate::tests::open(&fixture, &fixture.binding, &[]);
        let state = AppState::new(
            fixture.dir.path().to_owned(),
            Arc::new(crate::secure_store::MemoryStore::default()),
            crate::tests::noop(),
        );
        state.app_locked.store(false, Ordering::Release);
        assert!(!unlocked(&state, &session));
        session
            .client
            .unlock(&fixture.profile, &fixture.header, crate::tests::PHRASE)
            .unwrap();
        assert!(unlocked(&state, &session));
        state.app_locked.store(true, Ordering::Release);
        assert!(!unlocked(&state, &session));
        state.app_locked.store(false, Ordering::Release);
        session.mismatch.store(true, Ordering::Release);
        assert!(!unlocked(&state, &session));
        session.mismatch.store(false, Ordering::Release);
        session.status.lock().unwrap().revoked = true;
        assert!(!unlocked(&state, &session));
    }

    #[test]
    fn popup_copy_writes_and_marks_nothing_while_locked_or_stale() {
        use peppy_client_core::IncomingSms;
        use std::{cell::Cell, sync::Arc};

        let fixture = crate::tests::fixture_at("http://127.0.0.1:9");
        let session = crate::tests::open(&fixture, &fixture.binding, &[]);
        session
            .client
            .unlock(&fixture.profile, &fixture.header, crate::tests::PHRASE)
            .unwrap();
        let captured = session
            .client
            .capture_incoming(IncomingSms {
                conversation_id: None,
                sender_address: "+15555550401".into(),
                body: "Your verification code is 482913".into(),
                provider_message_id: Some("popup-copy".into()),
                imported: false,
            })
            .unwrap();
        let message_id = session.client.messages(captured.conversation_id).unwrap()[0]
            .payload
            .record
            .message_id
            .to_string();
        let target = OneTimeCodeTarget::Message { message_id };
        let unread = || {
            session
                .client
                .list_conversations()
                .unwrap()
                .into_iter()
                .find(|item| item.conversation_id == captured.conversation_id)
                .unwrap()
                .unread_count
        };
        let state = AppState::new(
            fixture.dir.path().to_owned(),
            Arc::new(crate::secure_store::MemoryStore::default()),
            crate::tests::noop(),
        );
        let writes = Cell::new(0);
        let generation = session.generation.clone();

        state.app_locked.store(true, Ordering::Release);
        copy_from_popup_in(&state, Some(&session), &generation, &target, |_| {
            writes.set(writes.get() + 1);
            Ok(())
        });
        assert_eq!(writes.get(), 0, "a locked app copies nothing");
        assert_eq!(unread(), 1, "a locked app marks nothing read");

        state.app_locked.store(false, Ordering::Release);
        copy_from_popup_in(&state, Some(&session), "stale-generation", &target, |_| {
            writes.set(writes.get() + 1);
            Ok(())
        });
        assert_eq!(writes.get(), 0, "a stale generation copies nothing");
        assert_eq!(unread(), 1);
        let cancel = tokio_util::sync::CancellationToken::new();
        assert!(!activation_admitted(
            Some(&session),
            &cancel,
            "stale-generation"
        ));
        assert!(activation_admitted(Some(&session), &cancel, &generation));
        cancel.cancel();
        assert!(!activation_admitted(Some(&session), &cancel, &generation));

        copy_from_popup_in(&state, Some(&session), &generation, &target, |_| {
            writes.set(writes.get() + 1);
            Ok(())
        });
        assert_eq!(
            writes.get(),
            1,
            "the same gate copies when unlocked and current"
        );
        assert_eq!(unread(), 0);
    }

    #[test]
    fn notification_mapping_preserves_source_tuple_and_never_guesses_from_body() {
        let mut candidate = peppy_client_core::BannerCandidate {
            id: "banner".into(),
            kind: "notification".into(),
            conversation_id: Some("unrelated-conversation".into()),
            notification_target: Some(peppy_client_core::NotificationTarget {
                source_device_id: "phone-a".into(),
                notification_key: "shared-key".into(),
                lifetime: "original-lifetime".into(),
            }),
            title: "secret-title".into(),
            body: "secret-body".into(),
            created_at: 0,
            source_name: None,
            app_icon_png_base64: None,
            one_time_code: None,
        };
        assert_eq!(
            destination(&candidate),
            Destination::Mirrored {
                target: NotificationActivationTarget {
                    source_device_id: "phone-a".into(),
                    notification_key: "shared-key".into(),
                    lifetime: "original-lifetime".into()
                }
            }
        );
        candidate.kind = "message".into();
        assert_eq!(
            destination(&candidate),
            Destination::Conversation {
                conversation_id: "unrelated-conversation".into()
            }
        );
        candidate.kind = "summary".into();
        assert_eq!(destination(&candidate), Destination::Main);
        candidate.kind = "notification".into();
        candidate.notification_target = None;
        assert_eq!(destination(&candidate), Destination::Main);
    }

    #[test]
    fn app_lock_check_state_transition_completes_and_invalidates_under_one_gate() {
        let activation = Arc::new(ActivationState::default());
        let stale = activation.quick_reply_generation_cancel();
        let checking = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (completed, completion) = std::sync::mpsc::sync_channel(1);
        let worker_activation = activation.clone();
        let worker_checking = checking.clone();
        std::thread::spawn(move || {
            crate::begin_app_lock_check_state(&worker_checking, &worker_activation);
            completed.send(()).unwrap();
        });

        completion
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("app-lock transition must acquire the effect gate only once");
        assert!(checking.load(Ordering::Acquire));
        assert!(stale.is_cancelled());
        assert!(!activation.quick_reply_generation_cancel().is_cancelled());
    }

    #[test]
    fn quick_reply_invalidation_under_effect_gate_completes_and_rotates_generation() {
        let state = Arc::new(ActivationState::default());
        let stale = state.quick_reply_generation_cancel();
        let (completed, completion) = std::sync::mpsc::sync_channel(1);
        let worker = state.clone();
        std::thread::spawn(move || {
            let _gate = worker.effect_gate.lock().unwrap();
            worker.invalidate_quick_reply_under_gate();
            completed.send(()).unwrap();
        });

        completion
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("under-gate invalidation must not relock the effect gate");
        assert!(stale.is_cancelled());
        assert!(!state.quick_reply_generation_cancel().is_cancelled());
    }

    #[test]
    fn quick_reply_invalidation_cancels_the_current_generation_and_rotates_it() {
        let state = ActivationState::default();
        let stale = state.quick_reply_generation_cancel();
        state.invalidate_quick_reply();
        let fresh = state.quick_reply_generation_cancel();
        assert!(stale.is_cancelled());
        assert!(!fresh.is_cancelled());
        state.invalidate_quick_reply();
        assert!(fresh.is_cancelled());
    }

    #[test]
    fn notification_ack_racing_new_publication_cannot_clear_new_slot() {
        use std::sync::{Arc, Barrier};
        let pending = Arc::new(Mutex::new(Pending::default()));
        pending
            .lock()
            .unwrap()
            .publish(activation("old", "session"), 1);
        let observed_old = Arc::new(Barrier::new(2));
        let new_published = Arc::new(Barrier::new(2));
        let reader = pending.clone();
        let observed = observed_old.clone();
        let published = new_published.clone();
        let ack = std::thread::spawn(move || {
            let old = reader.lock().unwrap().peek(Some("session"), true).unwrap();
            observed.wait();
            published.wait();
            reader
                .lock()
                .unwrap()
                .ack(&old.request_id, &old.session_generation)
        });
        observed_old.wait();
        pending
            .lock()
            .unwrap()
            .publish(activation("new", "session"), 2);
        new_published.wait();
        assert!(!ack.join().unwrap());
        assert_eq!(
            pending
                .lock()
                .unwrap()
                .peek(Some("session"), true)
                .unwrap()
                .request_id,
            "new"
        );
    }
}
