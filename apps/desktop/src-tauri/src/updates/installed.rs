#[cfg(any(target_os = "macos", test))]
use std::path::Path;

#[cfg(any(target_os = "macos", test))]
use tauri::Manager;

#[cfg(any(target_os = "macos", test))]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Installed {
    Matches,
    Differs(String),
    Unavailable,
}

#[cfg(any(target_os = "macos", test))]
pub fn compare(running: &str, installed: Option<&str>) -> Installed {
    let running = running.trim();
    let Some(installed) = installed
        .map(str::trim)
        .filter(|version| !version.is_empty())
    else {
        return Installed::Unavailable;
    };
    if running.is_empty() {
        return Installed::Unavailable;
    }
    if running == installed {
        Installed::Matches
    } else {
        Installed::Differs(installed.into())
    }
}

#[cfg(any(target_os = "macos", test))]
pub fn bundle_version(executable: &Path) -> Option<String> {
    let bundle = executable
        .ancestors()
        .find(|path| path.extension().is_some_and(|extension| extension == "app"))?;
    let info = plist::Value::from_file(bundle.join("Contents/Info.plist")).ok()?;
    let dictionary = info.as_dictionary()?;
    let version = dictionary
        .get("CFBundleShortVersionString")?
        .as_string()?
        .trim();
    let binary = dictionary.get("CFBundleExecutable")?.as_string()?.trim();
    if version.is_empty()
        || binary.is_empty()
        || !bundle.join("Contents/MacOS").join(binary).is_file()
    {
        return None;
    }
    Some(version.into())
}

#[cfg(any(target_os = "macos", test))]
pub fn running_bundle_version(app: &tauri::AppHandle) -> Option<String> {
    bundle_version(&tauri::process::current_binary(&app.env()).ok()?)
}

#[cfg(any(target_os = "macos", test))]
#[derive(Default)]
pub struct StablePoll {
    last: Option<String>,
}

#[cfg(any(target_os = "macos", test))]
impl StablePoll {
    pub fn observe(&mut self, candidate: Installed) -> Option<String> {
        let Installed::Differs(version) = candidate else {
            self.last = None;
            return None;
        };
        if self.last.as_deref() == Some(version.as_str()) {
            return Some(version);
        }
        self.last = Some(version);
        None
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn compares_trimmed_versions_and_rejects_missing_values() {
        assert_eq!(compare("1.0.0", Some(" 1.0.0 ")), Installed::Matches);
        assert_eq!(
            compare("1.0.0", Some("1.0.1")),
            Installed::Differs("1.0.1".into())
        );
        assert_eq!(compare("", Some("1.0.1")), Installed::Unavailable);
        assert_eq!(compare("1.0.0", Some(" ")), Installed::Unavailable);
        assert_eq!(compare("1.0.0", None), Installed::Unavailable);
    }

    #[test]
    fn reads_only_complete_app_bundles() {
        let temp = tempfile::tempdir().unwrap();
        let bundle = temp.path().join("Peppy.app");
        let contents = bundle.join("Contents");
        let macos = contents.join("MacOS");
        fs::create_dir_all(&macos).unwrap();
        let mut info = plist::Dictionary::new();
        info.insert(
            "CFBundleShortVersionString".into(),
            plist::Value::String("1.2.3".into()),
        );
        info.insert(
            "CFBundleExecutable".into(),
            plist::Value::String("Peppy".into()),
        );
        plist::to_file_xml(contents.join("Info.plist"), &plist::Value::Dictionary(info)).unwrap();

        let executable = macos.join("Peppy");
        assert_eq!(bundle_version(&executable), None);
        fs::write(&executable, []).unwrap();
        assert_eq!(bundle_version(&executable), Some("1.2.3".into()));

        fs::write(
            contents.join("Info.plist"),
            "<?xml version=\"1.0\"?><plist version=\"1.0\"><dict><key>CFBundleExecutable</key><string>Peppy</string></dict></plist>",
        )
        .unwrap();
        assert_eq!(bundle_version(&executable), None);
    }

    #[test]
    fn requires_two_consecutive_differences() {
        let mut poll = StablePoll::default();
        assert_eq!(poll.observe(Installed::Differs("1.0.1".into())), None);
        assert_eq!(poll.observe(Installed::Matches), None);
        assert_eq!(poll.observe(Installed::Differs("1.0.1".into())), None);
        assert_eq!(poll.observe(Installed::Differs("1.0.2".into())), None);
        assert_eq!(
            poll.observe(Installed::Differs("1.0.2".into())),
            Some("1.0.2".into())
        );
    }
}
