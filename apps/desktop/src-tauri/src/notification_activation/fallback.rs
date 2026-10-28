//! Legacy Linux/development delivery and fixed permission-test banners. No click
//! handlers or activation destinations are registered through this adapter.
use notify_rust::Notification;
use tauri::AppHandle;

pub(crate) fn post(app: &AppHandle, title: &str, body: &str) -> Result<(), ()> {
    let notification = notification(title, body);
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

fn notification(title: &str, body: &str) -> Notification {
    let mut notification = Notification::new();
    notification.summary(title).body(body).auto_icon();
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
        let notification = notification("provided title", "provided body");
        assert_eq!(notification.summary, "provided title");
        assert_eq!(notification.body, "provided body");
        assert!(!notification.icon.is_empty());
        assert!(notification.actions.is_empty());
        assert!(notification.subtitle.is_none());
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
