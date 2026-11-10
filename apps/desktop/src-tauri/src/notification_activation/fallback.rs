//! Legacy Linux/development delivery and fixed permission-test banners. Linux banners may carry
//! one Copy action with a bounded waiter; no click handlers or activation destinations are
//! registered through this adapter.
use crate::notification_persistence::NotificationPersistence;
use notify_rust::Notification;
use peppy_client_core::OneTimeCodeTarget;
#[cfg(any(target_os = "linux", test))]
use std::sync::atomic::{AtomicUsize, Ordering};
#[cfg(target_os = "linux")]
use std::sync::OnceLock;
use tauri::{AppHandle, Manager};

/// A popup Copy action. It carries the posting session's generation, never the code.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) struct CopyRequest {
    pub session_generation: String,
    pub target: OneTimeCodeTarget,
}

#[cfg(any(target_os = "linux", test))]
const COPY_ACTION: &str = "copy-code";
#[cfg(any(target_os = "linux", test))]
const COPY_LABEL: &str = "Copy code";
#[cfg(any(target_os = "linux", test))]
const MAX_COPY_WAITERS: usize = 4;
#[cfg(target_os = "linux")]
static COPY_WAITERS: AtomicUsize = AtomicUsize::new(0);
/// The server's capabilities do not change while it runs, so probe once rather than making a
/// blocking D-Bus call for every copy post. A failed probe is kept too: a server that starts
/// later gets no Copy action until Peppy restarts, which is the accepted degradation.
#[cfg(target_os = "linux")]
static ACTIONS_SUPPORTED: OnceLock<bool> = OnceLock::new();

pub(crate) fn post(
    app: &AppHandle,
    title: &str,
    body: &str,
    #[cfg_attr(not(target_os = "linux"), allow(unused_variables))] icon: Option<
        crate::notification_icon::Icon,
    >,
    #[cfg_attr(not(target_os = "linux"), allow(unused_variables))] copy: Option<CopyRequest>,
) -> Result<(), ()> {
    let sound = app.state::<crate::AppState>().notifications.sound_enabled();
    let persistence = app.state::<crate::AppState>().notifications.persistence();
    #[cfg(target_os = "linux")]
    let icon = crate::notification_icon::inline_image(icon.as_ref());
    #[cfg(target_os = "linux")]
    let mut notification = notification(title, body, sound, persistence);
    #[cfg(not(target_os = "linux"))]
    let notification = notification(title, body, sound, persistence);
    #[cfg(target_os = "linux")]
    if let Some(icon) = icon {
        notification.image_data(icon);
    }
    #[cfg(target_os = "linux")]
    let copy_waiter = copy.and_then(|request| {
        let supported = *ACTIONS_SUPPORTED.get_or_init(|| {
            notify_rust::get_capabilities()
                .is_ok_and(|capabilities| capabilities.iter().any(|cap| cap == "actions"))
        });
        apply_copy_action(&mut notification, persistence, supported, &COPY_WAITERS)
            .map(|slot| (request, slot))
    });
    #[cfg(target_os = "windows")]
    let notification = {
        let mut notification = notification;
        let executable = tauri::utils::platform::current_exe().map_err(|_| ())?;
        if let Some(identifier) = windows_app_id(&app.config().identifier, &executable) {
            notification.app_id(identifier);
        }
        notification
    };
    #[cfg(target_os = "macos")]
    let _ =
        notify_rust::set_application(mac_application(&app.config().identifier, tauri::is_dev()));
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let _ = app;

    // Linux/Windows show errors are returned directly. In notify-rust 4.18.1's
    // legacy macOS backend, show only creates a handle; Drop dispatches and
    // discards send errors. Do not mistake this fallback result for the direct
    // UN owner's confirmed acceptance, or add a click waiter to obtain it.
    let handle = notification.show().map_err(|_| ())?;
    #[cfg(target_os = "linux")]
    if let Some((request, slot)) = copy_waiter {
        await_copy(app.clone(), handle, request, slot);
    }
    #[cfg(not(target_os = "linux"))]
    let _ = handle;
    Ok(())
}

/// Adds the Copy action only when the server advertises actions, the banner expires (an
/// indefinite banner could outlive any waiter), and a waiter slot is free.
#[cfg(any(target_os = "linux", test))]
fn apply_copy_action<'a>(
    notification: &mut Notification,
    persistence: NotificationPersistence,
    actions_supported: bool,
    waiters: &'a AtomicUsize,
) -> Option<WaiterSlot<'a>> {
    if !actions_supported || persistence.seconds.is_none() {
        return None;
    }
    let slot = WaiterSlot::reserve(waiters, MAX_COPY_WAITERS)?;
    notification.action(COPY_ACTION, COPY_LABEL);
    Some(slot)
}

/// Frees its capacity on drop, so every waiter outcome (action, close, failure) releases it.
#[cfg(any(target_os = "linux", test))]
struct WaiterSlot<'a>(&'a AtomicUsize);

#[cfg(any(target_os = "linux", test))]
impl<'a> WaiterSlot<'a> {
    fn reserve(counter: &'a AtomicUsize, max: usize) -> Option<Self> {
        counter
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |active| {
                (active < max).then_some(active + 1)
            })
            .ok()?;
        Some(Self(counter))
    }
}

#[cfg(any(target_os = "linux", test))]
impl Drop for WaiterSlot<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Waits for the server's action or close signal on a plain thread. It captures only the app
/// handle and the posting generation, never a session; the action resolves the current session.
/// Servers normally send `__closed` when a banner expires, but a shell may keep the notification
/// in its tray, so the waiter can outlive the banner. The slot cap bounds how many can.
#[cfg(target_os = "linux")]
fn await_copy(
    app: AppHandle,
    handle: notify_rust::NotificationHandle,
    request: CopyRequest,
    slot: WaiterSlot<'static>,
) {
    let _ = std::thread::Builder::new()
        .name("notification-copy".into())
        .spawn(move || {
            // Released when the waiter returns for any reason, including `__closed`.
            let _slot = slot;
            handle.wait_for_action(|action| {
                if action == COPY_ACTION {
                    crate::notification_activation::copy_from_popup(
                        &app,
                        &request.session_generation,
                        &request.target,
                    );
                }
            });
        });
}

fn notification(
    title: &str,
    body: &str,
    sound: bool,
    persistence: NotificationPersistence,
) -> Notification {
    let mut notification = Notification::new();
    notification.summary(title).body(body).auto_icon();
    if sound {
        #[cfg(target_os = "macos")]
        notification.sound_name("NSUserNotificationDefaultSoundName");
        #[cfg(target_os = "windows")]
        notification.sound_name("Default");
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        notification.sound_name("message-new-instant");
    }
    #[cfg(target_os = "linux")]
    notification.hint(notify_rust::Hint::SuppressSound(!sound));
    #[cfg(target_os = "linux")]
    notification.timeout(match persistence.seconds {
        Some(seconds) => notify_rust::Timeout::Milliseconds(u32::from(seconds) * 1000),
        None => notify_rust::Timeout::Never,
    });
    #[cfg(not(target_os = "linux"))]
    let _ = persistence;
    notification
}

#[cfg(any(target_os = "macos", test))]
fn mac_application(identifier: &str, development: bool) -> &str {
    if development {
        "com.apple.Terminal"
    } else {
        identifier
    }
}

#[cfg(any(target_os = "windows", test))]
fn windows_app_id<'a>(identifier: &'a str, executable: &std::path::Path) -> Option<&'a str> {
    let directory = executable.parent()?;
    if directory.ends_with("target/debug") || directory.ends_with("target/release") {
        None
    } else {
        Some(identifier)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn expiring() -> NotificationPersistence {
        NotificationPersistence { seconds: Some(5) }
    }

    #[test]
    fn notification_fallback_preserves_content_auto_icon_and_no_actions() {
        let notification = notification(
            "provided title",
            "provided body",
            false,
            crate::notification_persistence::NotificationPersistence::default_value(),
        );
        assert_eq!(notification.summary, "provided title");
        assert_eq!(notification.body, "provided body");
        assert!(!notification.icon.is_empty());
        assert!(notification.actions.is_empty());
        assert!(notification.subtitle.is_none());
        #[cfg(not(target_os = "linux"))]
        assert_eq!(notification.timeout, notify_rust::Timeout::Default);
        #[cfg(target_os = "linux")]
        assert_eq!(
            notification.timeout,
            notify_rust::Timeout::Milliseconds(5_000)
        );
    }

    #[test]
    fn notification_copy_post_adds_only_the_copy_action_and_reserves_a_waiter() {
        let waiters = AtomicUsize::new(0);
        let mut notification = notification("title", "body", false, expiring());
        let slot = apply_copy_action(&mut notification, expiring(), true, &waiters);
        assert!(slot.is_some());
        assert_eq!(notification.actions, ["copy-code", "Copy code"]);
        assert_eq!(waiters.load(Ordering::Acquire), 1);
        drop(slot);
        assert_eq!(waiters.load(Ordering::Acquire), 0);
    }

    #[test]
    fn notification_copy_action_needs_server_actions_and_an_expiring_banner() {
        let waiters = AtomicUsize::new(0);
        let indefinite = NotificationPersistence { seconds: None };
        let mut unsupported = notification("title", "body", false, expiring());
        assert!(apply_copy_action(&mut unsupported, expiring(), false, &waiters).is_none());
        assert!(unsupported.actions.is_empty());
        let mut sticky = notification("title", "body", false, indefinite);
        assert!(apply_copy_action(&mut sticky, indefinite, true, &waiters).is_none());
        assert!(sticky.actions.is_empty());
        assert_eq!(waiters.load(Ordering::Acquire), 0);
    }

    #[test]
    fn notification_copy_waiters_are_capped_and_freed_on_every_drop() {
        let waiters = AtomicUsize::new(0);
        let held: Vec<_> = (0..MAX_COPY_WAITERS)
            .map(|_| WaiterSlot::reserve(&waiters, MAX_COPY_WAITERS).unwrap())
            .collect();
        assert!(WaiterSlot::reserve(&waiters, MAX_COPY_WAITERS).is_none());
        let mut fifth = notification("title", "body", false, expiring());
        assert!(apply_copy_action(&mut fifth, expiring(), true, &waiters).is_none());
        assert!(fifth.actions.is_empty());
        drop(held);
        assert_eq!(waiters.load(Ordering::Acquire), 0);
        assert!(WaiterSlot::reserve(&waiters, MAX_COPY_WAITERS).is_some());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn notification_fallback_keeps_indefinite_banners_until_dismissed() {
        let indefinite = serde_json::from_str(r#"{"seconds":null}"#).unwrap();
        let notification = notification("title", "body", false, indefinite);
        assert_eq!(notification.timeout, notify_rust::Timeout::Never);
    }

    #[test]
    fn notification_fallback_keeps_legacy_development_identity_only_in_fallback() {
        assert_eq!(
            mac_application("org.peppy.desktop", true),
            "com.apple.Terminal"
        );
        assert_eq!(
            mac_application("org.peppy.desktop.dev", false),
            "org.peppy.desktop.dev"
        );
        for path in [
            "checkout/target/debug/peppy.exe",
            "checkout/target/release/peppy.exe",
        ] {
            assert_eq!(
                windows_app_id("org.peppy.desktop", std::path::Path::new(path)),
                None
            );
        }
        assert_eq!(
            windows_app_id(
                "org.peppy.desktop",
                std::path::Path::new("installed/Peppy/peppy.exe")
            ),
            Some("org.peppy.desktop")
        );
    }
}
