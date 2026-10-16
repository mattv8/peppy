//! Platform installation preconditions and durable restart handoff state.
use std::{fs, io::Write, path::Path, path::PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{BridgeError, BridgeResult};

#[derive(Debug, Serialize, Deserialize)]
struct RestartMarker {
    version: String,
    background: bool,
}

/// The scheduler must never accidentally acquire the permissions available to a
/// user pressing “Restart now”.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InstallIntent {
    Automatic,
    User,
}

fn marker_path(root: &Path) -> PathBuf {
    root.join("updates/restart.json")
}

pub fn write_restart_marker(root: &Path, version: &str, background: bool) -> BridgeResult<()> {
    let path = marker_path(root);
    let temporary = path.with_extension("partial");
    let bytes = serde_json::to_vec(&RestartMarker {
        version: version.into(),
        background,
    })
    .map_err(|_| BridgeError::host_state())?;
    fs::create_dir_all(path.parent().expect("restart marker parent"))
        .map_err(|_| BridgeError::new("update-marker", "Could not prepare restart state."))?;
    fs::write(&temporary, bytes)
        .map_err(|_| BridgeError::new("update-marker", "Could not record restart state."))?;
    fs::rename(temporary, path)
        .map_err(|_| BridgeError::new("update-marker", "Could not finalize restart state."))
}

/// Returns a confirmed background restart request. A version mismatch stays on disk so an
/// unsuccessfully applied update cannot continually relaunch itself.
pub fn confirm_restart_marker(root: &Path, current_version: &str) -> Option<bool> {
    let path = marker_path(root);
    let marker: RestartMarker = serde_json::from_slice(&fs::read(&path).ok()?).ok()?;
    if marker.version != current_version {
        return None;
    }
    let _ = fs::remove_file(path);
    Some(marker.background)
}

pub fn clear_restart_marker(root: &Path) {
    let _ = fs::remove_file(marker_path(root));
}

pub fn preflight(package_kind: &str, intent: InstallIntent) -> BridgeResult<()> {
    #[cfg(target_os = "macos")]
    if package_kind == "app" {
        let executable = std::env::current_exe()
            .map_err(|_| BridgeError::new("update-install", "Could not locate this app bundle."))?;
        let path = executable.to_string_lossy();
        if path.starts_with("/Volumes/") || path.contains("/AppTranslocation/") {
            return Err(BridgeError::new(
                "update-install",
                "Move Peppy to Applications before updating.",
            ));
        }
        let bundle = executable
            .ancestors()
            .find(|path| path.extension().is_some_and(|extension| extension == "app"))
            .ok_or_else(|| {
                BridgeError::new(
                    "update-install",
                    "Peppy is not running from an installed app bundle.",
                )
            })?;
        let parent = bundle.parent().ok_or_else(|| {
            BridgeError::new("update-install", "Installed app location is unavailable.")
        })?;
        if !parent_is_writable(parent)
            || (intent == InstallIntent::Automatic && !parent_is_writable(bundle))
        {
            return Err(BridgeError::new(
                "update-install",
                "Peppy's installed app bundle is not writable for an automatic update.",
            ));
        }
    }
    #[cfg(target_os = "windows")]
    if requires_windows_authorization(package_kind, intent, windows_per_user_scope()) {
        return Err(BridgeError::new(
            "update-authorization-required",
            "This Windows installation requires authorization. Restart Peppy and install the update manually.",
        ));
    }
    #[cfg(target_os = "linux")]
    if package_kind == "appimage" {
        let app_image = std::env::var_os("APPIMAGE")
            .map(PathBuf::from)
            .filter(|path| path.is_file())
            .ok_or_else(|| {
                BridgeError::new(
                    "update-install",
                    "Could not locate the original AppImage for this update.",
                )
            })?;
        let parent = app_image.parent().ok_or_else(|| {
            BridgeError::new("update-install", "The AppImage location is unavailable.")
        })?;
        if !parent_is_writable(parent) {
            return Err(BridgeError::new(
                "update-install",
                "The original AppImage location is not writable.",
            ));
        }
    }
    let _ = package_kind;
    let _ = intent;
    Ok(())
}

fn requires_windows_authorization(
    package_kind: &str,
    intent: InstallIntent,
    per_user_scope: bool,
) -> bool {
    matches!(package_kind, "msi" | "nsis" | "windows")
        && intent == InstallIntent::Automatic
        && !per_user_scope
}

fn parent_is_writable(parent: &Path) -> bool {
    let candidate = parent.join(format!(".peppy-update-write-{}", uuid::Uuid::new_v4()));
    let result = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&candidate)
        .and_then(|mut file| file.write_all(b"probe"));
    let _ = fs::remove_file(candidate);
    result.is_ok()
}

#[cfg(target_os = "windows")]
fn windows_per_user_scope() -> bool {
    let Ok(executable) = std::env::current_exe() else {
        return false;
    };
    let Ok(local_app_data) = std::env::var("LOCALAPPDATA") else {
        return false;
    };
    let Ok(executable) = executable.canonicalize() else {
        return false;
    };
    let Ok(local_app_data) = PathBuf::from(local_app_data).canonicalize() else {
        return false;
    };
    executable.starts_with(local_app_data) && executable.parent().is_some_and(parent_is_writable)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn confirmed_marker_is_one_shot_and_version_bound() {
        let root = tempfile::tempdir().unwrap();
        write_restart_marker(root.path(), "1.2.3", true).unwrap();
        assert_eq!(confirm_restart_marker(root.path(), "1.2.2"), None);
        assert_eq!(confirm_restart_marker(root.path(), "1.2.3"), Some(true));
        assert_eq!(confirm_restart_marker(root.path(), "1.2.3"), None);
    }

    #[test]
    fn failed_install_marker_can_be_cleared() {
        let root = tempfile::tempdir().unwrap();
        write_restart_marker(root.path(), "1.2.3", false).unwrap();
        clear_restart_marker(root.path());
        assert_eq!(confirm_restart_marker(root.path(), "1.2.3"), None);
    }

    #[test]
    fn writable_parent_probe_creates_and_removes_its_own_file() {
        let root = tempfile::tempdir().unwrap();
        assert!(parent_is_writable(root.path()));
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 0);
    }

    #[test]
    fn automatic_windows_machine_scope_requires_explicit_authorization() {
        assert!(requires_windows_authorization(
            "msi",
            InstallIntent::Automatic,
            false
        ));
        assert!(!requires_windows_authorization(
            "msi",
            InstallIntent::User,
            false
        ));
        assert!(!requires_windows_authorization(
            "msi",
            InstallIntent::Automatic,
            true
        ));
    }
}
