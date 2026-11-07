//! Legacy Linux/development delivery and fixed permission-test banners. No click
//! handlers or activation destinations are registered through this adapter.
use notify_rust::Notification;
use tauri::{AppHandle, Manager};

pub(crate) fn post(
    app: &AppHandle,
    title: &str,
    body: &str,
    #[cfg_attr(not(target_os = "linux"), allow(unused_variables))] icon: Option<
        crate::notification_icon::Icon,
    >,
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
    notification.show().map(drop).map_err(|_| ())
}

fn notification(
    title: &str,
    body: &str,
    sound: bool,
    persistence: crate::notification_persistence::NotificationPersistence,
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
