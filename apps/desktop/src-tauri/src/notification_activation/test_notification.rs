//! Synthetic quick-reply exercise for the notification settings test button.
//! It is deliberately detached from conversations, drafts, and the outbound queue.
#[cfg(any(target_os = "macos", target_os = "windows"))]
use super::{Destination, PopupAction, PopupActions, PostError};
#[cfg(any(target_os = "macos", target_os = "windows"))]
use crate::AppState;
#[cfg(any(test, target_os = "macos", target_os = "windows"))]
use peppy_client_core::NotificationPreview;
#[cfg(any(target_os = "macos", target_os = "windows"))]
use std::sync::{atomic::Ordering, Arc, Mutex};
use tauri::AppHandle;
#[cfg(any(target_os = "macos", target_os = "windows"))]
use tauri::Manager;
#[cfg(any(test, target_os = "macos", target_os = "windows"))]
use tokio_util::sync::CancellationToken;

#[cfg(any(test, target_os = "macos", target_os = "windows"))]
const TEST_TITLE: &str = "Peppy test message";
#[cfg(any(test, target_os = "macos", target_os = "windows"))]
const TEST_BODY: &str = "This is a test message. Reply here to try quick reply.";
#[cfg(any(test, target_os = "macos", target_os = "windows"))]
const HIDDEN_TITLE: &str = "New notification";
#[cfg(any(test, target_os = "macos", target_os = "windows"))]
const REPLY_TITLE: &str = "Test reply received";
#[cfg(any(test, target_os = "macos", target_os = "windows"))]
const REPLY_BODY: &str = "No message was sent.";

#[cfg(any(test, target_os = "macos", target_os = "windows"))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum TestPrivacy {
    Full,
    Source,
    Hidden,
}

#[cfg(any(test, target_os = "macos", target_os = "windows"))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct TestPresentation {
    pub(super) title: &'static str,
    pub(super) body: &'static str,
    pub(super) reveal: bool,
}

#[cfg(any(test, target_os = "macos", target_os = "windows"))]
pub(super) fn presentation(privacy: TestPrivacy) -> TestPresentation {
    match privacy {
        TestPrivacy::Full => TestPresentation {
            title: TEST_TITLE,
            body: TEST_BODY,
            reveal: false,
        },
        TestPrivacy::Source => TestPresentation {
            title: TEST_TITLE,
            body: "",
            reveal: true,
        },
        TestPrivacy::Hidden => TestPresentation {
            title: HIDDEN_TITLE,
            body: "",
            reveal: true,
        },
    }
}

#[cfg(any(test, target_os = "macos", target_os = "windows"))]
pub(super) fn request_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

#[cfg(any(test, target_os = "macos", target_os = "windows"))]
fn privacy(preview: NotificationPreview, show_source: bool) -> TestPrivacy {
    match (preview, show_source) {
        (NotificationPreview::Full, _) => TestPrivacy::Full,
        (NotificationPreview::Hidden, true) => TestPrivacy::Source,
        (NotificationPreview::Hidden, false) => TestPrivacy::Hidden,
    }
}

#[cfg(any(test, target_os = "macos", target_os = "windows"))]
fn enabled(
    locked: bool,
    lock_actions_allowed: bool,
    quick_reply_enabled: bool,
    supported: bool,
) -> bool {
    !locked && lock_actions_allowed && quick_reply_enabled && supported
}

#[cfg(any(test, target_os = "macos", target_os = "windows"))]
fn reply_is_admitted(body: &str) -> bool {
    body.len() <= super::MAX_REPLY_BYTES && !body.trim().is_empty()
}

pub(super) fn post(app: &AppHandle) -> Result<bool, ()> {
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    return post_supported(app);
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let _ = app;
        Ok(false)
    }
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
type PresentationVersion = Arc<Mutex<Option<super::owner::PopupVersion>>>;

#[cfg(any(target_os = "macos", target_os = "windows"))]
struct CapturedTest {
    session_generation: String,
    session_cancel: CancellationToken,
    policy_cancel: CancellationToken,
    generation_cancel: CancellationToken,
    privacy: TestPrivacy,
    has_session: bool,
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
fn post_supported(app: &AppHandle) -> Result<bool, ()> {
    let state = app.state::<AppState>();
    // Capture eligibility, privacy, and cancellation generations atomically with invalidation.
    // The native owner lock is acquired only after this effect gate has been released.
    let captured = {
        let _effect = state
            .notification_activation
            .effect_gate
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if !eligible(&state) {
            return Ok(false);
        }
        let session = match state.session.try_lock() {
            Ok(session) => session.clone(),
            Err(_) => return Ok(false),
        };
        match session {
            Some(session) => {
                if !super::current(Some(&session), &session.generation)
                    || !super::unlocked(&state, &session)
                {
                    return Ok(false);
                }
                let Ok(preferences) = session.notification_preferences(&state.notifications) else {
                    return Ok(false);
                };
                CapturedTest {
                    session_generation: session.generation.clone(),
                    session_cancel: session.cancel.clone(),
                    policy_cancel: state.notifications.quick_reply_policy_cancel(),
                    generation_cancel: state
                        .notification_activation
                        .quick_reply_generation_cancel(),
                    privacy: privacy(preferences.preview, preferences.show_source),
                    has_session: true,
                }
            }
            // No vault is not an excuse to open one. The app-lock gate above permits a
            // bounded default-Full exercise only for an already-unlocked application.
            None => CapturedTest {
                session_generation: "test-notification".into(),
                session_cancel: CancellationToken::new(),
                policy_cancel: state.notifications.quick_reply_policy_cancel(),
                generation_cancel: state
                    .notification_activation
                    .quick_reply_generation_cancel(),
                privacy: TestPrivacy::Full,
                has_session: false,
            },
        }
    };
    post_popup(app, captured).map(|()| true).map_err(|_| ())
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
fn eligible(state: &AppState) -> bool {
    enabled(
        state.app_locked.load(Ordering::Acquire),
        crate::app_lock_actions_allowed(
            state.app_lock_checking.load(Ordering::Acquire),
            state.app_lock_storage_available.load(Ordering::Acquire),
        ),
        state.notifications.quick_reply_enabled(),
        state.notification_activation.quick_reply_supported(),
    )
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
fn post_popup(app: &AppHandle, captured: CapturedTest) -> Result<(), PostError> {
    let capability = Arc::new(TestCapability {
        app: app.clone(),
        request_id: request_id(),
        session_generation: captured.session_generation,
        has_session: captured.has_session,
        session_cancel: captured.session_cancel,
        policy_cancel: captured.policy_cancel,
        generation_cancel: captured.generation_cancel,
        cancel: CancellationToken::new(),
        stop: CancellationToken::new(),
        presentation: Arc::new(Mutex::new(None)),
    });
    watch_cancellation(
        app.clone(),
        capability.session_cancel.clone(),
        capability.policy_cancel.clone(),
        capability.generation_cancel.clone(),
        capability.cancel.clone(),
        capability.stop.clone(),
    );
    submit(capability, presentation(captured.privacy), true)
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
struct TestCapability {
    app: AppHandle,
    request_id: String,
    session_generation: String,
    has_session: bool,
    session_cancel: CancellationToken,
    policy_cancel: CancellationToken,
    generation_cancel: CancellationToken,
    cancel: CancellationToken,
    stop: CancellationToken,
    presentation: PresentationVersion,
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
impl Drop for TestCapability {
    fn drop(&mut self) {
        self.stop.cancel();
        self.cancel.cancel();
    }
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
fn watch_cancellation(
    app: AppHandle,
    session_cancel: CancellationToken,
    policy_cancel: CancellationToken,
    generation_cancel: CancellationToken,
    owner_cancel: CancellationToken,
    stop: CancellationToken,
) {
    drop(tauri::async_runtime::spawn(async move {
        tokio::select! {
            _ = session_cancel.cancelled() => {},
            _ = policy_cancel.cancelled() => {},
            _ = generation_cancel.cancelled() => {},
            _ = stop.cancelled() => return,
        }
        owner_cancel.cancel();
        app.state::<AppState>().notification_activation.wake();
    }));
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
impl TestCapability {
    fn current(&self) -> bool {
        if !cancellation_current(
            &self.cancel,
            &self.session_cancel,
            &self.policy_cancel,
            &self.generation_cancel,
        ) {
            return false;
        }
        let state = self.app.state::<AppState>();
        if !eligible(&state) {
            return false;
        }
        let Ok(session) = state.session.try_lock() else {
            return false;
        };
        match (self.has_session, session.as_ref()) {
            (true, Some(session)) => {
                super::current(Some(session), &self.session_generation)
                    && super::unlocked(&state, session)
            }
            (false, None) => true,
            _ => false,
        }
    }
}

#[cfg(any(test, target_os = "macos", target_os = "windows"))]
fn cancellation_current(
    owner_cancel: &CancellationToken,
    session_cancel: &CancellationToken,
    policy_cancel: &CancellationToken,
    generation_cancel: &CancellationToken,
) -> bool {
    !owner_cancel.is_cancelled()
        && !session_cancel.is_cancelled()
        && !policy_cancel.is_cancelled()
        && !generation_cancel.is_cancelled()
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
fn submit(
    capability: Arc<TestCapability>,
    presentation: TestPresentation,
    sound: bool,
) -> Result<(), PostError> {
    if !capability.current() {
        return Err(PostError::Failed);
    }
    let state = capability.app.state::<AppState>();
    let guard = state.notification_activation.owner.lock().unwrap();
    let Some(owner) = guard.as_ref() else {
        return Err(PostError::Failed);
    };
    let handler_capability = capability.clone();
    let post = super::owner::Post {
        activation: Some(peppy_desktop_api::NotificationActivation {
            request_id: capability.request_id.clone(),
            session_generation: capability.session_generation.clone(),
            destination: Destination::Main,
        }),
        cancel: capability.cancel.clone(),
        sound: sound && state.notifications.sound_enabled(),
        title: presentation.title.into(),
        body: presentation.body.into(),
        icon: Some(crate::notification_icon::Icon::Message),
        copy: None,
        popup_actions: Some(PopupActions {
            reveal: presentation.reveal,
            reply: !presentation.reveal,
            handler: Arc::new(move |action| handle_action(handler_capability.clone(), action)),
        }),
    };
    let acceptance = match capability.presentation.lock().unwrap().clone() {
        Some(version) => owner.replace(version, post),
        None => owner.submit(post),
    }?;
    // Store the version before waiting: a native callback may be ready as soon as acceptance
    // completes and Reveal needs this exact version for its replacement.
    *capability.presentation.lock().unwrap() = Some(acceptance.version());
    drop(guard);
    acceptance.and_then(super::owner::Acceptance::wait)
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
fn handle_action(capability: Arc<TestCapability>, action: PopupAction) {
    if !capability.current() {
        return;
    }
    match action {
        PopupAction::Reveal => {
            if submit(capability.clone(), presentation(TestPrivacy::Full), false).is_err() {
                capability.cancel.cancel();
                capability
                    .app
                    .state::<AppState>()
                    .notification_activation
                    .wake();
            }
        }
        PopupAction::Reply(body) if reply_is_admitted(&body) => {
            // The body is intentionally not retained, logged, queued, or forwarded.
            post_confirmation(&capability.app);
        }
        PopupAction::Open | PopupAction::Copy | PopupAction::Reply(_) => {}
    }
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
fn post_confirmation(app: &AppHandle) {
    // Deliberately ordinary: it cannot recurse into this composer and ignores OS reply text.
    let _ = super::post_ordinary_test(app, REPLY_TITLE, REPLY_BODY);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn privacy_controls_payload_and_reveal_action() {
        assert_eq!(
            presentation(TestPrivacy::Full),
            TestPresentation {
                title: TEST_TITLE,
                body: TEST_BODY,
                reveal: false
            }
        );
        assert_eq!(
            presentation(TestPrivacy::Source),
            TestPresentation {
                title: TEST_TITLE,
                body: "",
                reveal: true
            }
        );
        assert_eq!(
            presentation(TestPrivacy::Hidden),
            TestPresentation {
                title: HIDDEN_TITLE,
                body: "",
                reveal: true
            }
        );
        assert_eq!(
            privacy(NotificationPreview::Hidden, true),
            TestPrivacy::Source
        );
        assert_eq!(
            privacy(NotificationPreview::Hidden, false),
            TestPrivacy::Hidden
        );
    }

    #[test]
    fn quick_reply_dispatch_requires_every_gate() {
        assert!(enabled(false, true, true, true));
        for flags in [
            (true, true, true, true),
            (false, false, true, true),
            (false, true, false, true),
            (false, true, true, false),
        ] {
            assert!(!enabled(flags.0, flags.1, flags.2, flags.3));
        }
    }

    #[test]
    fn arbitrary_reply_is_never_part_of_the_confirmation() {
        let arbitrary_reply = "private text that must not be echoed";
        assert!(reply_is_admitted(arbitrary_reply));
        assert!(!reply_is_admitted("   "));
        assert!(!reply_is_admitted(
            &"x".repeat(super::super::MAX_REPLY_BYTES + 1)
        ));
        assert_ne!(REPLY_TITLE, arbitrary_reply);
        assert_ne!(REPLY_BODY, arbitrary_reply);
        assert!(!REPLY_TITLE.contains(arbitrary_reply));
        assert!(!REPLY_BODY.contains(arbitrary_reply));
        assert_eq!(REPLY_BODY, "No message was sent.");
    }

    #[test]
    fn optional_session_cancellation_makes_callbacks_stale() {
        let owner = CancellationToken::new();
        let session = CancellationToken::new();
        let policy = CancellationToken::new();
        let generation = CancellationToken::new();
        assert!(cancellation_current(&owner, &session, &policy, &generation));
        session.cancel();
        assert!(!cancellation_current(
            &owner,
            &session,
            &policy,
            &generation
        ));
    }
}
