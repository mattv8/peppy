use crate::{
    admit_update_operation, error::BridgeError, error::BridgeResult, require_main, AppState,
};
use tauri::{Manager, WebviewWindow};
use tauri_plugin_autostart::ManagerExt as AutostartExt;

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StartupState {
    On,
    Off,
    ApprovalNeeded,
    Unavailable,
}

#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StartupStatus {
    pub state: StartupState,
    pub background: bool,
}

#[tauri::command]
pub(crate) fn startup_status(
    window: WebviewWindow,
    app: tauri::AppHandle,
) -> BridgeResult<StartupStatus> {
    require_main(window.label())?;
    let background = app.state::<AppState>().config()?.open_in_background;
    Ok(StartupStatus {
        state: registration_status(&app),
        background,
    })
}

#[tauri::command]
pub(crate) fn set_start_at_login(
    window: WebviewWindow,
    app: tauri::AppHandle,
    enabled: bool,
) -> BridgeResult<()> {
    let _operation = admit_update_operation(&app.state::<AppState>())?;
    require_main(window.label())?;
    set_registration(&app, enabled)
}

#[tauri::command]
pub(crate) fn set_open_in_background(
    window: WebviewWindow,
    app: tauri::AppHandle,
    enabled: bool,
) -> BridgeResult<()> {
    let _operation = admit_update_operation(&app.state::<AppState>())?;
    require_main(window.label())?;
    app.state::<AppState>()
        .update_config(|config| config.open_in_background = enabled)?;
    Ok(())
}

#[tauri::command]
pub(crate) fn open_login_items(window: WebviewWindow, app: tauri::AppHandle) -> BridgeResult<()> {
    let _operation = admit_update_operation(&app.state::<AppState>())?;
    require_main(window.label())?;
    #[cfg(target_os = "macos")]
    {
        if macos_13_or_later() {
            unsafe { objc2_service_management::SMAppService::openSystemSettingsLoginItems() };
            return Ok(());
        }
    }
    Err(BridgeError::new(
        "unsupported",
        "Login Items settings are unavailable on this platform.",
    ))
}

fn set_registration(app: &tauri::AppHandle, enabled: bool) -> BridgeResult<()> {
    #[cfg(target_os = "macos")]
    if macos_13_or_later() {
        let service = unsafe { objc2_service_management::SMAppService::mainAppService() };
        let result = if enabled {
            unsafe { service.registerAndReturnError() }
        } else {
            unsafe { service.unregisterAndReturnError() }
        };
        result
            .map_err(|_| BridgeError::new("start-at-login", "Could not update Start at login."))?;
        return app
            .autolaunch()
            .disable()
            .map_err(|_| BridgeError::new("start-at-login", "Could not update Start at login."));
    }
    let launcher = app.autolaunch();
    let result = if enabled {
        launcher.enable()
    } else {
        launcher.disable()
    };
    result.map_err(|_| BridgeError::new("start-at-login", "Could not update Start at login."))
}

fn registration_status(app: &tauri::AppHandle) -> StartupState {
    #[cfg(target_os = "macos")]
    if macos_13_or_later() {
        let status = macos_service_status();
        let legacy_enabled = app.autolaunch().is_enabled().unwrap_or(false);
        return macos_status_with_legacy(status, legacy_enabled);
    }
    match app.autolaunch().is_enabled() {
        Ok(true) if windows_startup_disabled(app) => StartupState::ApprovalNeeded,
        Ok(true) => StartupState::On,
        Ok(false) => StartupState::Off,
        Err(_) => StartupState::Unavailable,
    }
}

/// Runs once during startup, before status is requested, to replace enabled legacy macOS
/// LaunchAgents with SMAppService registrations without changing the person's enabled state.
pub(crate) fn migrate_startup_registration(app: &tauri::AppHandle) {
    #[cfg(target_os = "macos")]
    if macos_13_or_later() {
        let legacy_enabled = app.autolaunch().is_enabled().unwrap_or(false);
        let status = macos_service_status();
        if legacy_enabled && (status == StartupState::On || register_macos_service().is_ok()) {
            let _ = app.autolaunch().disable();
        }
    }
}

#[cfg(target_os = "windows")]
fn windows_startup_disabled(app: &tauri::AppHandle) -> bool {
    use windows::{
        core::PCWSTR,
        Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_BINARY},
    };

    let value_name = app
        .package_info()
        .name
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let mut data = [0_u8; 16];
    let mut data_len = data.len() as u32;
    unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            windows::core::w!(
                r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run"
            ),
            PCWSTR(value_name.as_ptr()),
            RRF_RT_REG_BINARY,
            None,
            Some(data.as_mut_ptr().cast()),
            Some(&mut data_len),
        )
        .is_ok()
            && startup_approved_disabled(&data[..data_len.min(data.len() as u32) as usize])
    }
}

#[cfg(not(target_os = "windows"))]
fn windows_startup_disabled(_: &tauri::AppHandle) -> bool {
    false
}

#[cfg(any(target_os = "windows", test))]
fn startup_approved_disabled(data: &[u8]) -> bool {
    data.first().is_some_and(|byte| byte & 1 == 1)
}

#[cfg(any(target_os = "macos", test))]
fn macos_status_with_legacy(service_status: StartupState, legacy_enabled: bool) -> StartupState {
    if legacy_enabled {
        StartupState::On
    } else {
        service_status
    }
}

#[cfg(target_os = "macos")]
fn macos_service_status() -> StartupState {
    match unsafe { objc2_service_management::SMAppService::mainAppService().status() } {
        objc2_service_management::SMAppServiceStatus::Enabled => StartupState::On,
        objc2_service_management::SMAppServiceStatus::NotRegistered => StartupState::Off,
        objc2_service_management::SMAppServiceStatus::RequiresApproval => {
            StartupState::ApprovalNeeded
        }
        _ => StartupState::Unavailable,
    }
}

#[cfg(target_os = "macos")]
fn register_macos_service() -> Result<(), objc2::rc::Retained<objc2_foundation::NSError>> {
    unsafe { objc2_service_management::SMAppService::mainAppService().registerAndReturnError() }
}

#[cfg(target_os = "macos")]
fn macos_13_or_later() -> bool {
    use objc2_foundation::NSProcessInfo;
    NSProcessInfo::processInfo()
        .operatingSystemVersion()
        .majorVersion
        >= 13
}

#[cfg(test)]
mod tests {
    use super::{macos_status_with_legacy, StartupState};

    #[test]
    fn status_states_remain_serializable() {
        assert_eq!(
            serde_json::to_string(&StartupState::ApprovalNeeded).unwrap(),
            "\"approval_needed\""
        );
        assert_eq!(
            serde_json::to_string(&StartupState::Unavailable).unwrap(),
            "\"unavailable\""
        );
    }

    #[test]
    fn legacy_launch_agent_keeps_start_at_login_on_during_migration() {
        assert_eq!(
            macos_status_with_legacy(StartupState::Off, true),
            StartupState::On
        );
        assert_eq!(
            macos_status_with_legacy(StartupState::ApprovalNeeded, true),
            StartupState::On
        );
        assert_eq!(
            macos_status_with_legacy(StartupState::Off, false),
            StartupState::Off
        );
    }

    #[test]
    fn startup_approved_binary_value_uses_its_disabled_bit() {
        assert!(super::startup_approved_disabled(&[0x03]));
        assert!(super::startup_approved_disabled(&[0x01, 0, 0]));
        assert!(!super::startup_approved_disabled(&[0x02]));
        assert!(!super::startup_approved_disabled(&[]));
    }
}
