//! Startup policy kept separate from OS registration side effects.
use tauri::Manager;
pub const BACKGROUND_ARGUMENT: &str = "--background";
pub const LOGIN_ARGUMENT: &str = "--login";

pub fn background_requested(arguments: impl IntoIterator<Item = String>) -> bool {
    arguments
        .into_iter()
        .any(|argument| argument == BACKGROUND_ARGUMENT)
}

pub fn login_requested(arguments: impl IntoIterator<Item = String>) -> bool {
    arguments
        .into_iter()
        .any(|argument| argument == LOGIN_ARGUMENT)
}

/// Older autostart registrations used `--background`. When their registration is still enabled,
/// treat that argument as a login launch so the saved background preference remains authoritative.
pub fn legacy_background_is_login(autostart_enabled: bool, background_requested: bool) -> bool {
    autostart_enabled && background_requested
}

/// A restart marker is a durable statement about the prior process's actual
/// main-window state, and therefore takes precedence over a stale launch arg.
pub fn restart_background(marker: Option<bool>, argument_requested: bool) -> bool {
    marker.unwrap_or(argument_requested)
}

pub fn initial_background(
    restart_marker: Option<bool>,
    background_preference: bool,
    login_launch: bool,
    argument_requested: bool,
) -> bool {
    restart_background(
        restart_marker,
        argument_requested || (background_preference && login_launch),
    )
}

/// A failed tray must never create an unreachable hidden launch.
pub fn hide_initial_main(background: bool, tray_available: bool) -> bool {
    background && tray_available
}

/// `None` means the launch event was unavailable, so callers should show the window.
#[cfg(target_os = "macos")]
#[allow(unexpected_cfgs, deprecated)]
pub fn login_launch() -> Option<bool> {
    use cocoa::base::{id, nil};
    use objc::{class, msg_send, sel, sel_impl};
    const KEY_AE_LAUNCHED_AS_LOGIN_ITEM: u32 = u32::from_be_bytes(*b"isli");
    unsafe {
        let manager: id = msg_send![class!(NSAppleEventManager), sharedAppleEventManager];
        let event: id = msg_send![manager, currentAppleEvent];
        if event == nil {
            return None;
        }
        let descriptor: id =
            msg_send![event, paramDescriptorForKeyword: KEY_AE_LAUNCHED_AS_LOGIN_ITEM];
        if descriptor == nil {
            return Some(false);
        }
        let value: bool = msg_send![descriptor, booleanValue];
        Some(value)
    }
}

#[cfg(not(target_os = "macos"))]
pub fn login_launch() -> Option<bool> {
    None
}

/// Quiet login startup is allowed only after the tray is known reachable.
/// On macOS the app becomes an accessory while the main window is hidden;
/// `tray::show_main` restores regular activation policy.
pub fn background_main(app: &tauri::AppHandle) -> crate::error::BridgeResult<()> {
    let main = app
        .get_webview_window(crate::tray::MAIN)
        .ok_or_else(|| crate::error::BridgeError::new("window", "Main window is unavailable."))?;
    #[cfg(target_os = "macos")]
    app.set_activation_policy(tauri::ActivationPolicy::Accessory)
        .map_err(|_| {
            crate::error::BridgeError::new("window", "Could not enter background mode.")
        })?;
    crate::window_placement::flush(app);
    main.hide()
        .map_err(|_| crate::error::BridgeError::new("window", "Could not hide main window."))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn background_needs_explicit_argument_and_tray() {
        assert!(background_requested([BACKGROUND_ARGUMENT.into()]));
        assert!(!background_requested(["--other".into()]));
        assert!(hide_initial_main(true, true));
        assert!(!hide_initial_main(true, false));
    }

    #[test]
    fn login_argument_is_distinct_from_background_argument() {
        assert!(login_requested([LOGIN_ARGUMENT.into()]));
        assert!(!login_requested([BACKGROUND_ARGUMENT.into()]));
        assert!(legacy_background_is_login(true, true));
        assert!(!legacy_background_is_login(false, true));
        assert!(!legacy_background_is_login(true, false));
    }

    #[test]
    fn restart_marker_overrides_the_launch_argument_when_present() {
        assert!(!restart_background(Some(false), true));
        assert!(restart_background(Some(true), false));
        assert!(restart_background(None, true));
    }

    #[test]
    fn background_truth_table_preserves_restart_and_explicit_background() {
        assert!(initial_background(None, false, false, true));
        assert!(initial_background(None, true, true, false));
        assert!(!initial_background(None, true, false, false));
        assert!(!initial_background(None, false, true, false));
        assert!(!initial_background(Some(false), true, true, true));
        assert!(initial_background(Some(true), false, false, false));
    }
}
