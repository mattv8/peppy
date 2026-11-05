use std::sync::OnceLock;

use serde::Serialize;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Distribution {
    Direct,
    AppStore,
}

impl Distribution {
    pub fn detect() -> Self {
        static DISTRIBUTION: OnceLock<Distribution> = OnceLock::new();

        *DISTRIBUTION.get_or_init(|| {
            #[cfg(target_os = "macos")]
            {
                detect_from_facts(
                    std::env::var("APP_SANDBOX_CONTAINER_ID").ok().as_deref(),
                    running_bundle_has_receipt(),
                )
            }

            #[cfg(not(target_os = "macos"))]
            Self::Direct
        })
    }
}

fn detect_from_facts(sandbox_container_id: Option<&str>, receipt_exists: bool) -> Distribution {
    if sandbox_container_id.is_some() || receipt_exists {
        Distribution::AppStore
    } else {
        Distribution::Direct
    }
}

#[cfg(target_os = "macos")]
fn running_bundle_has_receipt() -> bool {
    std::env::current_exe()
        .ok()
        .and_then(|path| {
            path.ancestors()
                .find(|ancestor| ancestor.file_name().is_some_and(|name| name == "Contents"))
                .map(|contents| contents.join("_MASReceipt/receipt"))
        })
        .is_some_and(|receipt| receipt.is_file())
}

#[cfg(test)]
mod tests {
    use super::{detect_from_facts, Distribution};

    #[test]
    fn detection_matrix_prefers_app_store_when_either_fact_is_present() {
        assert_eq!(detect_from_facts(None, false), Distribution::Direct);
        assert_eq!(
            detect_from_facts(Some("container"), false),
            Distribution::AppStore
        );
        assert_eq!(detect_from_facts(None, true), Distribution::AppStore);
        assert_eq!(detect_from_facts(Some(""), true), Distribution::AppStore);
    }
}
