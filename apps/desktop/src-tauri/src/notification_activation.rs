//! Running-process notification activation. OS callbacks retain identifiers only.
use crate::{error::BridgeResult, session::Session, AppState};
use peppy_desktop_api::{
    NotificationActivation, NotificationActivationDestination as Destination,
    NotificationActivationTarget,
};
use std::sync::{atomic::Ordering, Mutex};
use tauri::{Emitter, Manager};

#[cfg(target_os = "macos")]
mod macos;
#[cfg(any(target_os = "macos", target_os = "windows"))]
mod owner;
#[cfg(target_os = "windows")]
mod windows;

pub const EVENT: &str = "peppy://notification-activation";
pub(crate) mod fallback;

#[derive(Default)]
pub struct ActivationState {
    pending: Mutex<Pending>,
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    owner: Mutex<Option<owner::Owner>>,
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
) -> Result<(), ()> {
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    {
        let state = app.state::<AppState>();
        let guard = state.notification_activation.owner.lock().unwrap();
        if let Some(owner) = guard.as_ref() {
            let acceptance = owner.submit(owner::Post {
                activation: NotificationActivation {
                    request_id: uuid::Uuid::new_v4().simple().to_string(),
                    session_generation: session.generation.clone(),
                    destination,
                },
                cancel: session.cancel.clone(),
                title,
                body,
            });
            // OS completion can wake state listeners, and shutdown takes this
            // mutex. Never hold it while waiting for backend acceptance.
            drop(guard);
            return acceptance.and_then(owner::Acceptance::wait);
        }
    }
    fallback::post(app, &title, &body)
}

impl ActivationState {
    pub fn retire(&self) {
        self.pending.lock().unwrap().0 = None;
        self.wake();
    }

    pub fn wake(&self) {
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        if let Some(owner) = self.owner.lock().unwrap().as_ref() {
            owner.wake();
        }
    }

    pub fn shutdown(&self) {
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
) {
    let state = app.state::<AppState>();
    // Lock order is session -> pending, also used by both IPC commands.
    let session = tauri::async_runtime::block_on(state.session.lock());
    if cancel.is_cancelled() || !current(session.as_deref(), &activation.session_generation) {
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
