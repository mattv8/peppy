use serde::Serialize;

/// The complete, renderer-safe update contract. Paths, feed URLs, signatures and
/// installer arguments deliberately never cross the IPC boundary.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct UpdateStatus {
    pub current_version: String,
    pub channel: UpdateChannel,
    pub phase: UpdatePhase,
    pub automatic: bool,
    pub available_version: Option<String>,
    pub progress_percent: Option<u8>,
    pub last_checked_at: Option<String>,
    pub deferred_until: Option<String>,
    pub message: Option<String>,
    pub release_notes_url: Option<String>,
    pub can_restart: bool,
    pub package_kind: String,
    pub unattended: bool,
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum UpdateChannel {
    Stable,
    Staging,
    Disabled,
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum UpdatePhase {
    Disabled,
    Idle,
    Checking,
    Downloading,
    Ready,
    Deferred,
    AuthorizationRequired,
    Installing,
    Error,
}

impl UpdateStatus {
    pub fn idle(current_version: String, channel: UpdateChannel) -> Self {
        Self {
            current_version,
            channel,
            phase: UpdatePhase::Idle,
            automatic: true,
            available_version: None,
            progress_percent: None,
            last_checked_at: None,
            deferred_until: None,
            message: None,
            release_notes_url: None,
            can_restart: false,
            package_kind: package_kind().into(),
            unattended: false,
        }
    }

    pub fn reset_to_idle(&mut self) {
        self.phase = UpdatePhase::Idle;
        self.available_version = None;
        self.progress_percent = None;
        self.message = None;
        self.release_notes_url = None;
        self.can_restart = false;
    }

    pub fn disabled(current_version: String) -> Self {
        Self {
            current_version,
            channel: UpdateChannel::Disabled,
            phase: UpdatePhase::Disabled,
            automatic: false,
            available_version: None,
            progress_percent: None,
            last_checked_at: None,
            deferred_until: None,
            message: Some("Updates are disabled for this development build.".into()),
            release_notes_url: None,
            can_restart: false,
            package_kind: package_kind().into(),
            unattended: false,
        }
    }
}

impl UpdateChannel {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Stable => "stable",
            Self::Staging => "staging",
            Self::Disabled => "disabled",
        }
    }
}

pub fn package_kind() -> &'static str {
    use tauri::utils::{config::BundleType, platform::bundle_type};

    match bundle_type() {
        Some(BundleType::Deb) => return "deb",
        Some(BundleType::Rpm) => return "rpm",
        Some(BundleType::AppImage) => return "appimage",
        Some(BundleType::Msi) => return "msi",
        Some(BundleType::Nsis) => return "nsis",
        Some(BundleType::App | BundleType::Dmg) => return "app",
        None => {}
    }
    #[cfg(target_os = "windows")]
    return "windows";
    #[cfg(target_os = "macos")]
    return "macos";
    #[cfg(target_os = "linux")]
    return "linux";
    #[allow(unreachable_code)]
    "unsupported"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializes_the_frozen_renderer_contract() {
        let status = UpdateStatus {
            current_version: "1.2.3".into(),
            channel: UpdateChannel::Stable,
            phase: UpdatePhase::Ready,
            automatic: true,
            available_version: Some("1.2.4".into()),
            progress_percent: Some(100),
            last_checked_at: Some("2026-10-16T12:00:00Z".into()),
            deferred_until: None,
            message: None,
            release_notes_url: Some("https://github.com/mattv8/peppy/releases/tag/v1.2.4".into()),
            can_restart: true,
            package_kind: "appimage".into(),
            unattended: false,
        };
        let fixture: serde_json::Value =
            serde_json::from_str(include_str!("../../../src/update-status.fixture.json")).unwrap();
        assert_eq!(serde_json::to_value(status).unwrap(), fixture);
    }

    #[test]
    fn reset_to_idle_clears_only_staged_update_fields() {
        let mut status = UpdateStatus::idle("1.2.3".into(), UpdateChannel::Stable);
        status.phase = UpdatePhase::Ready;
        status.available_version = Some("1.2.4".into());
        status.progress_percent = Some(100);
        status.can_restart = true;
        status.release_notes_url = Some("https://example.test".into());
        status.reset_to_idle();
        assert_eq!(status.phase, UpdatePhase::Idle);
        assert_eq!(status.current_version, "1.2.3");
        assert!(status.available_version.is_none());
        assert!(!status.can_restart);
    }
}
