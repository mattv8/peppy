use serde::{de::Error as _, Deserialize, Deserializer, Serialize};
use std::{fs, io, path::Path};

pub const FILE: &str = "notification-persistence.json";
const DEFAULT_SECONDS: u16 = 5;
const MAX_SECONDS: u16 = 3600;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct NotificationPersistence {
    pub seconds: Option<u16>,
}

impl NotificationPersistence {
    pub fn default_value() -> Self {
        Self {
            seconds: Some(DEFAULT_SECONDS),
        }
    }
}

pub fn supported() -> bool {
    cfg!(any(target_os = "windows", target_os = "linux"))
}

pub fn settings_supported() -> bool {
    supported() || cfg!(target_os = "macos")
}

pub fn platform() -> &'static str {
    #[cfg(target_os = "macos")]
    {
        "macos"
    }
    #[cfg(target_os = "windows")]
    {
        "windows"
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        "linux"
    }
}

pub fn load(root: &Path) -> NotificationPersistence {
    fs::read(root.join(FILE))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_else(NotificationPersistence::default_value)
}

pub fn save(root: &Path, value: NotificationPersistence) -> io::Result<()> {
    let bytes = serde_json::to_vec(&value).expect("notification persistence serializes");
    crate::fsutil::write_private_atomic(&root.join(FILE), &bytes)
}

fn validate(seconds: Option<u64>) -> Result<NotificationPersistence, &'static str> {
    match seconds {
        None => Ok(NotificationPersistence { seconds: None }),
        Some(seconds) if (1..=u64::from(MAX_SECONDS)).contains(&seconds) => {
            Ok(NotificationPersistence {
                seconds: Some(seconds as u16),
            })
        }
        Some(_) => Err("seconds must be null or an integer from 1 through 3600"),
    }
}

impl<'de> Deserialize<'de> for NotificationPersistence {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Raw {
            seconds: serde_json::Value,
        }

        let raw = Raw::deserialize(deserializer)?;
        let seconds = match raw.seconds {
            serde_json::Value::Null => None,
            serde_json::Value::Number(number) => number.as_u64().map(Some).ok_or_else(|| {
                D::Error::custom("seconds must be null or an integer from 1 through 3600")
            })?,
            _ => {
                return Err(D::Error::custom(
                    "seconds must be null or an integer from 1 through 3600",
                ))
            }
        };
        validate(seconds).map_err(D::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::{load, platform, settings_supported, supported, NotificationPersistence, FILE};

    #[test]
    fn persistence_rejects_invalid_json_and_accepts_valid_values() {
        for value in [
            "{}",
            r#"{"seconds":0}"#,
            r#"{"seconds":3601}"#,
            r#"{"seconds":5.5}"#,
            r#"{"seconds":5.0}"#,
            r#"{"seconds":-1}"#,
            r#"{"seconds":"5"}"#,
            r#"{"seconds":5,"x":1}"#,
        ] {
            assert!(serde_json::from_str::<NotificationPersistence>(value).is_err());
        }
        for value in [
            r#"{"seconds":null}"#,
            r#"{"seconds":1}"#,
            r#"{"seconds":3600}"#,
        ] {
            assert!(serde_json::from_str::<NotificationPersistence>(value).is_ok());
        }
    }

    #[test]
    fn persistence_file_round_trips_null_and_defaults_when_malformed() {
        let dir = tempfile::tempdir().unwrap();
        let indefinite = NotificationPersistence { seconds: None };
        super::save(dir.path(), indefinite).unwrap();
        assert_eq!(load(dir.path()), indefinite);
        std::fs::write(dir.path().join(FILE), b"malformed").unwrap();
        assert_eq!(load(dir.path()), NotificationPersistence::default_value());
        std::fs::write(dir.path().join(FILE), br#"{"seconds":3601}"#).unwrap();
        assert_eq!(load(dir.path()), NotificationPersistence::default_value());
    }

    #[test]
    fn persistence_support_matches_the_platform() {
        assert_eq!(
            supported(),
            cfg!(any(target_os = "windows", target_os = "linux"))
        );
    }

    #[test]
    fn persistence_settings_report_the_platform_and_are_supported() {
        let expected_platform = if cfg!(target_os = "macos") {
            "macos"
        } else if cfg!(target_os = "windows") {
            "windows"
        } else {
            "linux"
        };
        assert_eq!(platform(), expected_platform);
        assert!(settings_supported());
    }
}
