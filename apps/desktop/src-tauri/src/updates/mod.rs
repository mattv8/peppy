pub mod types;

pub const STATE_EVENT: &str = "peppy://update-state";

#[cfg(feature = "self-update")]
mod enabled;

#[cfg(feature = "self-update")]
pub use enabled::*;

#[cfg(not(feature = "self-update"))]
mod disabled {
    use std::path::Path;

    use tauri::AppHandle;

    use crate::error::BridgeResult;

    use super::types::UpdateStatus;

    #[derive(Clone)]
    pub struct UpdateManager {
        status: UpdateStatus,
    }

    impl UpdateManager {
        pub fn new(current_version: String) -> Self {
            Self {
                status: UpdateStatus::disabled(current_version),
            }
        }

        pub fn initialize(&self, _app: &AppHandle, _root: &Path) -> BridgeResult<()> {
            Ok(())
        }

        pub fn status(&self) -> UpdateStatus {
            self.status.clone()
        }

        pub fn set_automatic(&self, _enabled: bool) -> BridgeResult<UpdateStatus> {
            Ok(self.status())
        }

        pub fn defer(&self) -> BridgeResult<UpdateStatus> {
            Ok(self.status())
        }

        pub fn report_activity(&self, _label: &str, _active: bool) {}
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use crate::updates::types::{UpdateChannel, UpdatePhase};

        #[test]
        fn disabled_build_preserves_the_disabled_update_contract() {
            let updates = UpdateManager::new("1.2.3".into());

            for status in [
                updates.status(),
                updates.set_automatic(true).unwrap(),
                updates.defer().unwrap(),
            ] {
                assert_eq!(status.channel, UpdateChannel::Disabled);
                assert_eq!(status.phase, UpdatePhase::Disabled);
                assert!(!status.automatic);
                assert!(!status.can_restart);
            }
        }
    }
}

#[cfg(not(feature = "self-update"))]
pub use disabled::UpdateManager;
