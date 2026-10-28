//! Native desktop notification authorization. The OS is authoritative; this module only reads
//! it, posts a fixed content-free test banner, and names the OS settings page.
use serde::Serialize;
use tauri::AppHandle;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PermissionStatus {
    Granted,
    Quiet,
    Denied,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NotificationCheck {
    pub status: PermissionStatus,
    pub test_sent: bool,
    pub can_open_settings: bool,
}

/// Platform-neutral mirror of `UNAuthorizationStatus` so mapping is testable everywhere.
#[cfg(any(target_os = "macos", test))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MacAuth {
    NotDetermined,
    Denied,
    Authorized,
    Provisional,
    Ephemeral,
}

/// Platform-neutral mirror of WinRT `NotificationSetting`.
#[cfg(any(windows, test))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WinSetting {
    Enabled,
    DisabledForApplication,
    DisabledForUser,
    DisabledByGroupPolicy,
    DisabledByManifest,
}

#[cfg(target_os = "macos")]
const MAC_SETTINGS: &str = "x-apple.systempreferences:com.apple.Notifications-Settings.extension";
#[cfg(windows)]
const WINDOWS_SETTINGS: &str = "ms-settings:notifications";
const TEST_TITLE: &str = "Peppy";
const TEST_BODY: &str = "Test notification: desktop banners are working.";

#[cfg(any(target_os = "macos", test))]
pub fn map_macos(auth: MacAuth, alerts_off: bool) -> PermissionStatus {
    match auth {
        MacAuth::Provisional => PermissionStatus::Quiet,
        MacAuth::Authorized | MacAuth::Ephemeral if alerts_off => PermissionStatus::Quiet,
        MacAuth::Authorized | MacAuth::Ephemeral => PermissionStatus::Granted,
        MacAuth::Denied => PermissionStatus::Denied,
        MacAuth::NotDetermined => PermissionStatus::Unknown,
    }
}

#[cfg(any(windows, test))]
pub fn map_windows(setting: WinSetting) -> PermissionStatus {
    match setting {
        WinSetting::Enabled => PermissionStatus::Granted,
        WinSetting::DisabledForApplication
        | WinSetting::DisabledForUser
        | WinSetting::DisabledByGroupPolicy
        | WinSetting::DisabledByManifest => PermissionStatus::Denied,
    }
}

#[cfg(target_os = "macos")]
pub fn settings_url(bundle_id: Option<&str>) -> Option<String> {
    Some(match bundle_id {
        Some(id) => format!("{MAC_SETTINGS}?id={id}"),
        None => MAC_SETTINGS.to_owned(),
    })
}

#[cfg(windows)]
pub fn settings_url(_bundle_id: Option<&str>) -> Option<String> {
    Some(WINDOWS_SETTINGS.to_owned())
}

#[cfg(not(any(target_os = "macos", windows)))]
pub fn settings_url(_bundle_id: Option<&str>) -> Option<String> {
    None
}

#[cfg(target_os = "macos")]
mod macos {
    use super::{map_macos, MacAuth, PermissionStatus};
    use block2::RcBlock;
    use objc2::runtime::Bool;
    use objc2_foundation::{NSBundle, NSError};
    use objc2_user_notifications::{
        UNAlertStyle, UNAuthorizationOptions, UNAuthorizationStatus, UNNotificationSettings,
        UNUserNotificationCenter,
    };
    use std::{ptr::NonNull, sync::mpsc, time::Duration};

    const READ_TIMEOUT: Duration = Duration::from_secs(5);
    const PROMPT_TIMEOUT: Duration = Duration::from_secs(60);

    // Dev or unbundled builds lack a reliable bundle identity to match in System Settings, so
    // `query` reports Unknown instead of a status that may belong to another app.
    pub fn bundle_id() -> Option<String> {
        NSBundle::mainBundle()
            .bundleIdentifier()
            .map(|id| id.to_string())
    }

    fn read(center: &UNUserNotificationCenter) -> Option<(MacAuth, bool)> {
        let (tx, rx) = mpsc::sync_channel(1);
        let block = RcBlock::new(move |settings: NonNull<UNNotificationSettings>| {
            // SAFETY: the framework passes a valid, non-null settings object for the callback.
            let settings = unsafe { settings.as_ref() };
            let auth = match settings.authorizationStatus() {
                UNAuthorizationStatus::Denied => MacAuth::Denied,
                UNAuthorizationStatus::Authorized => MacAuth::Authorized,
                UNAuthorizationStatus::Provisional => MacAuth::Provisional,
                UNAuthorizationStatus::Ephemeral => MacAuth::Ephemeral,
                _ => MacAuth::NotDetermined,
            };
            let _ = tx.try_send((auth, settings.alertStyle() == UNAlertStyle::None));
        });
        center.getNotificationSettingsWithCompletionHandler(&block);
        rx.recv_timeout(READ_TIMEOUT).ok()
    }

    fn request(center: &UNUserNotificationCenter) {
        let (tx, rx) = mpsc::sync_channel(1);
        let block = RcBlock::new(move |_granted: Bool, _error: *mut NSError| {
            let _ = tx.try_send(());
        });
        center.requestAuthorizationWithOptions_completionHandler(
            UNAuthorizationOptions::Alert
                | UNAuthorizationOptions::Sound
                | UNAuthorizationOptions::Badge,
            &block,
        );
        let _ = rx.recv_timeout(PROMPT_TIMEOUT);
    }

    pub fn query() -> PermissionStatus {
        if tauri::is_dev() || bundle_id().is_none() {
            return PermissionStatus::Unknown;
        }
        let center = UNUserNotificationCenter::currentNotificationCenter();
        let Some((mut auth, mut alerts_off)) = read(&center) else {
            return PermissionStatus::Unknown;
        };
        if auth == MacAuth::NotDetermined {
            request(&center);
            match read(&center) {
                Some(next) => (auth, alerts_off) = next,
                None => return PermissionStatus::Unknown,
            }
        }
        map_macos(auth, alerts_off)
    }
}

#[cfg(windows)]
mod windows_os {
    use super::{map_windows, PermissionStatus, WinSetting};
    use windows::{
        core::HSTRING,
        UI::Notifications::{NotificationSetting, ToastNotificationManager},
    };

    // Our legacy fallback sets System.AppUserModel.ID only outside target/debug
    // and target/release, so those development builds
    // are not matched to Peppy's toast identity and report Unknown here.
    fn installed() -> bool {
        let Ok(exe) = tauri::utils::platform::current_exe() else {
            return false;
        };
        let Some(dir) = exe.parent().map(|dir| dir.display().to_string()) else {
            return false;
        };
        let separator = std::path::MAIN_SEPARATOR;
        !(dir.ends_with(&format!("{separator}target{separator}debug"))
            || dir.ends_with(&format!("{separator}target{separator}release")))
    }

    pub fn query(identifier: &str) -> PermissionStatus {
        if !installed() {
            return PermissionStatus::Unknown;
        }
        let setting =
            ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(identifier))
                .and_then(|notifier| notifier.Setting());
        match setting {
            Ok(NotificationSetting::Enabled) => map_windows(WinSetting::Enabled),
            Ok(NotificationSetting::DisabledForApplication) => {
                map_windows(WinSetting::DisabledForApplication)
            }
            Ok(NotificationSetting::DisabledForUser) => map_windows(WinSetting::DisabledForUser),
            Ok(NotificationSetting::DisabledByGroupPolicy) => {
                map_windows(WinSetting::DisabledByGroupPolicy)
            }
            Ok(NotificationSetting::DisabledByManifest) => {
                map_windows(WinSetting::DisabledByManifest)
            }
            _ => PermissionStatus::Unknown,
        }
    }
}

#[cfg(target_os = "macos")]
fn query(_app: &AppHandle) -> PermissionStatus {
    macos::query()
}

#[cfg(windows)]
fn query(app: &AppHandle) -> PermissionStatus {
    windows_os::query(&app.config().identifier)
}

#[cfg(not(any(target_os = "macos", windows)))]
fn query(_app: &AppHandle) -> PermissionStatus {
    PermissionStatus::Unknown
}

pub fn current_settings_url(app: &AppHandle) -> Option<String> {
    #[cfg(target_os = "macos")]
    let identifier = macos::bundle_id().unwrap_or_else(|| app.config().identifier.clone());
    #[cfg(not(target_os = "macos"))]
    let identifier = app.config().identifier.clone();
    settings_url(Some(&identifier))
}

/// Blocking OS work. `test_sent` means fallback dispatch succeeded, not display;
/// the legacy macOS backend sends on handle Drop and cannot report send errors.
/// Call via `crate::sync::blocking` so authorization callbacks never wait on a Tokio worker.
pub fn check(app: &AppHandle) -> NotificationCheck {
    let status = query(app);
    let test_sent = status != PermissionStatus::Denied
        && crate::notification_activation::fallback::post(app, TEST_TITLE, TEST_BODY).is_ok();
    NotificationCheck {
        status,
        test_sent,
        can_open_settings: current_settings_url(app).is_some(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_macos_authorization() {
        assert_eq!(
            map_macos(MacAuth::Authorized, false),
            PermissionStatus::Granted
        );
        assert_eq!(
            map_macos(MacAuth::Provisional, false),
            PermissionStatus::Quiet
        );
        assert_eq!(
            map_macos(MacAuth::Ephemeral, false),
            PermissionStatus::Granted
        );
        assert_eq!(
            map_macos(MacAuth::Authorized, true),
            PermissionStatus::Quiet
        );
        assert_eq!(map_macos(MacAuth::Denied, false), PermissionStatus::Denied);
        assert_eq!(
            map_macos(MacAuth::NotDetermined, false),
            PermissionStatus::Unknown
        );
    }

    #[test]
    fn maps_windows_settings() {
        assert_eq!(map_windows(WinSetting::Enabled), PermissionStatus::Granted);
        for blocked in [
            WinSetting::DisabledForApplication,
            WinSetting::DisabledForUser,
            WinSetting::DisabledByGroupPolicy,
            WinSetting::DisabledByManifest,
        ] {
            assert_eq!(map_windows(blocked), PermissionStatus::Denied);
        }
    }

    #[test]
    fn serializes_check_for_the_bridge() {
        let json = serde_json::to_value(NotificationCheck {
            status: PermissionStatus::Quiet,
            test_sent: false,
            can_open_settings: true,
        })
        .unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "status": "quiet", "testSent": false, "canOpenSettings": true
            })
        );
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn builds_macos_settings_url() {
        assert_eq!(
            settings_url(Some("org.peppy.desktop")).as_deref(),
            Some("x-apple.systempreferences:com.apple.Notifications-Settings.extension?id=org.peppy.desktop")
        );
        assert_eq!(settings_url(None).as_deref(), Some(MAC_SETTINGS));
    }

    #[test]
    #[cfg(windows)]
    fn builds_windows_settings_url() {
        assert_eq!(
            settings_url(Some("ignored")).as_deref(),
            Some(WINDOWS_SETTINGS)
        );
    }

    #[test]
    #[cfg(not(any(target_os = "macos", windows)))]
    fn other_platforms_have_no_settings_url() {
        assert_eq!(settings_url(Some("x")), None);
    }
}
