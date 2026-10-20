use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Component, Path, PathBuf},
};

#[cfg(unix)]
use std::fs::File;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::{BridgeError, BridgeResult};

pub const MAX_PAYLOAD_BYTES: u64 = 1024 * 1024 * 1024;

#[derive(Clone, Deserialize, Serialize)]
pub struct Settings {
    #[serde(default = "default_automatic")]
    pub automatic: bool,
    #[serde(default)]
    pub deferred_until: Option<String>,
    #[serde(default)]
    pub attempted_version: Option<String>,
}

const fn default_automatic() -> bool {
    true
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            automatic: true,
            deferred_until: None,
            attempted_version: None,
        }
    }
}

#[derive(Clone, Deserialize, Serialize)]
pub struct CachedUpdate {
    pub version: String,
    pub channel: String,
    pub package_kind: String,
    pub manifest: serde_json::Value,
    pub payload: String,
}

pub fn directory(root: &Path) -> PathBuf {
    root.join("updates")
}

/// Missing preferences use safe defaults; present but corrupt preferences must be surfaced so
/// callers do not accidentally re-enable automatic update checks.
pub fn load_settings(root: &Path) -> BridgeResult<Settings> {
    let path = directory(root).join("settings.json");
    match fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map_err(|_| BridgeError::new("update-cache", "Update settings are invalid.")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Settings::default()),
        Err(_) => Err(BridgeError::new(
            "update-cache",
            "Could not read update settings.",
        )),
    }
}

pub fn save_settings(root: &Path, settings: &Settings) -> BridgeResult<()> {
    atomic_json(
        &directory(root).join("settings.json"),
        settings,
        "Could not save update settings.",
    )
}

/// Returns only a regular, private payload located beneath the current cache root.
pub fn load_cached(root: &Path) -> BridgeResult<Option<(CachedUpdate, PathBuf)>> {
    let dir = directory(root);
    let pointer = dir.join("pending.json");
    let bytes = match fs::read(&pointer) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            sweep_owned(root, None)?;
            return Ok(None);
        }
        Err(_) => {
            return Err(BridgeError::new(
                "update-cache",
                "Could not read cached update.",
            ));
        }
    };
    let cached: CachedUpdate = serde_json::from_slice(&bytes)
        .map_err(|_| BridgeError::new("update-cache", "Cached update metadata is invalid."))?;
    ensure_owned_directory(&dir)?;
    let relative = Path::new(&cached.payload);
    if !safe_relative(relative) {
        return Err(BridgeError::new(
            "update-cache",
            "Cached update path is invalid.",
        ));
    }
    let expected_name = payload_filename(&cached.package_kind).ok_or_else(|| {
        BridgeError::new("update-cache", "Cached update package kind is invalid.")
    })?;
    let legacy_payload = relative
        .file_name()
        .is_some_and(|name| name == "payload.bin");
    if relative
        .file_name()
        .is_none_or(|name| name != expected_name && !legacy_payload)
    {
        return Err(BridgeError::new(
            "update-cache",
            "Cached update payload name is invalid.",
        ));
    }
    let payload = dir.join(relative);
    let root_canonical = fs::canonicalize(&dir)
        .map_err(|_| BridgeError::new("update-cache", "Update cache is unavailable."))?;
    let payload_canonical = fs::canonicalize(&payload)
        .map_err(|_| BridgeError::new("update-cache", "Cached update payload is unavailable."))?;
    if !payload_canonical.starts_with(&root_canonical)
        || fs::symlink_metadata(&payload)
            .map(|m| m.file_type().is_symlink())
            .unwrap_or(true)
    {
        return Err(BridgeError::new(
            "update-cache",
            "Cached update path is invalid.",
        ));
    }
    let metadata = fs::metadata(&payload)
        .map_err(|_| BridgeError::new("update-cache", "Cached update payload is unavailable."))?;
    if !metadata.is_file() || metadata.len() > MAX_PAYLOAD_BYTES {
        return Err(BridgeError::new(
            "update-cache",
            "Cached update payload is invalid.",
        ));
    }
    if legacy_payload {
        let migrated = payload.with_file_name(expected_name);
        fs::rename(&payload, &migrated).map_err(|_| {
            BridgeError::new("update-cache", "Could not migrate cached update payload.")
        })?;
        let mut migrated_cached = cached.clone();
        migrated_cached.payload = relative
            .with_file_name(expected_name)
            .to_string_lossy()
            .into();
        atomic_json(
            &pointer,
            &migrated_cached,
            "Could not finalize cached update.",
        )?;
        sweep_owned(root, Some(Path::new(&migrated_cached.payload)))?;
        return Ok(Some((migrated_cached, migrated)));
    }
    sweep_owned(root, Some(relative))?;
    Ok(Some((cached, payload)))
}

/// Promotes a verified private partial into a new immutable generation.  The pointer is written
/// last, so readers see either the previous complete generation or this complete generation.
pub fn commit_cached(
    root: &Path,
    metadata: &CachedUpdate,
    verified_path: &Path,
) -> BridgeResult<PathBuf> {
    let dir = directory(root);
    create_private_dir(&dir)?;
    let source_metadata = fs::symlink_metadata(verified_path)
        .map_err(|_| BridgeError::new("update-cache", "Verified update payload is unavailable."))?;
    if source_metadata.file_type().is_symlink()
        || !source_metadata.is_file()
        || source_metadata.len() > MAX_PAYLOAD_BYTES
    {
        return Err(BridgeError::new(
            "update-cache",
            "Verified update payload is invalid.",
        ));
    }
    let generation = Uuid::new_v4().to_string();
    let generations = dir.join("generations");
    create_private_dir(&generations)?;
    let generation_dir = generations.join(&generation);
    create_private_dir(&generation_dir)?;
    let filename = payload_filename(&metadata.package_kind).ok_or_else(|| {
        BridgeError::new("update-cache", "Cached update package kind is invalid.")
    })?;
    let payload = generation_dir.join(filename);
    fs::rename(verified_path, &payload)
        .or_else(|_| {
            fs::copy(verified_path, &payload)
                .map(|_| ())
                .and_then(|_| fs::remove_file(verified_path))
        })
        .map_err(|_| BridgeError::new("update-cache", "Could not finalize update payload."))?;
    sync_file(&payload)?;
    sync_dir(&generation_dir)?;
    let mut committed = metadata.clone();
    committed.payload = format!("generations/{generation}/{filename}");
    atomic_json(
        &generation_dir.join("metadata.json"),
        &committed,
        "Could not save cached update metadata.",
    )?;
    atomic_json(
        &dir.join("pending.json"),
        &committed,
        "Could not finalize cached update.",
    )?;
    sweep_owned(root, Some(Path::new(&committed.payload)))?;
    Ok(payload)
}

pub fn remove_cached(root: &Path) -> BridgeResult<()> {
    let dir = directory(root);
    match fs::remove_file(dir.join("pending.json")) {
        Ok(()) => {
            sync_dir(&dir)?;
            sweep_owned(root, None)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => sweep_owned(root, None),
        Err(_) => Err(BridgeError::new(
            "update-cache",
            "Could not remove cached update.",
        )),
    }
}

/// Removes only UUID-named cache generations and known private download partials. It never
/// traverses a symlink and preserves the supplied active relative payload generation.
#[cfg(test)]
pub fn sweep_orphans(root: &Path) -> BridgeResult<()> {
    let pointer = directory(root).join("pending.json");
    let active = fs::read(pointer)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<CachedUpdate>(&bytes).ok())
        .and_then(|cached| safe_relative(Path::new(&cached.payload)).then_some(cached.payload));
    sweep_owned(root, active.as_deref().map(Path::new))
}

fn sweep_owned(root: &Path, active_payload: Option<&Path>) -> BridgeResult<()> {
    let dir = directory(root);
    let directory_metadata = match fs::symlink_metadata(&dir) {
        Ok(metadata) if !metadata.file_type().is_symlink() && metadata.is_dir() => metadata,
        Ok(_) => {
            return Err(BridgeError::new(
                "update-cache",
                "Update cache path is invalid.",
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(_) => {
            return Err(BridgeError::new(
                "update-cache",
                "Could not inspect update cache.",
            ));
        }
    };
    let _ = directory_metadata;
    for entry in fs::read_dir(&dir)
        .map_err(|_| BridgeError::new("update-cache", "Could not inspect update cache."))?
    {
        let entry = entry
            .map_err(|_| BridgeError::new("update-cache", "Could not inspect update cache."))?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with(".payload-") && name.ends_with(".partial") {
            remove_owned_file(&entry.path())?;
        }
    }
    let generations = dir.join("generations");
    let entries = match fs::symlink_metadata(&generations) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            return Err(BridgeError::new(
                "update-cache",
                "Update cache path is invalid.",
            ));
        }
        Ok(_) => fs::read_dir(&generations)
            .map_err(|_| BridgeError::new("update-cache", "Could not inspect update cache."))?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(_) => {
            return Err(BridgeError::new(
                "update-cache",
                "Could not inspect update cache.",
            ));
        }
    };
    for entry in entries {
        let entry = entry
            .map_err(|_| BridgeError::new("update-cache", "Could not inspect update cache."))?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if Uuid::parse_str(name).is_err() {
            continue;
        }
        let active_generation = active_payload.and_then(Path::parent);
        if active_generation == Some(Path::new("generations").join(name).as_path()) {
            continue;
        }
        remove_owned_tree(&entry.path())?;
    }
    sync_dir(&dir)
}

fn ensure_owned_directory(path: &Path) -> BridgeResult<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if !metadata.file_type().is_symlink() && metadata.is_dir() => Ok(()),
        _ => Err(BridgeError::new(
            "update-cache",
            "Update cache path is invalid.",
        )),
    }
}

fn remove_owned_file(path: &Path) -> BridgeResult<()> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|_| BridgeError::new("update-cache", "Could not clean update cache."))?;
    if metadata.is_file() || metadata.file_type().is_symlink() {
        fs::remove_file(path)
            .map_err(|_| BridgeError::new("update-cache", "Could not clean update cache."))?;
    }
    Ok(())
}

fn remove_owned_tree(path: &Path) -> BridgeResult<()> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|_| BridgeError::new("update-cache", "Could not clean update cache."))?;
    if metadata.file_type().is_symlink() || metadata.is_file() {
        return remove_owned_file(path);
    }
    if !metadata.is_dir() {
        return Ok(());
    }
    for entry in fs::read_dir(path)
        .map_err(|_| BridgeError::new("update-cache", "Could not clean update cache."))?
    {
        remove_owned_tree(
            &entry
                .map_err(|_| BridgeError::new("update-cache", "Could not clean update cache."))?
                .path(),
        )?;
    }
    fs::remove_dir(path)
        .map_err(|_| BridgeError::new("update-cache", "Could not clean update cache."))
}

fn safe_relative(path: &Path) -> bool {
    !path.as_os_str().is_empty()
        && path
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
}

fn payload_filename(package_kind: &str) -> Option<&'static str> {
    match package_kind {
        "deb" => Some("payload.deb"),
        "rpm" => Some("payload.rpm"),
        "appimage" | "linux" => Some("payload.AppImage"),
        "msi" => Some("payload.msi"),
        "nsis" | "windows" => Some("payload.exe"),
        "app" | "macos" => Some("payload.tar.gz"),
        _ => None,
    }
}

fn create_private_dir(path: &Path) -> BridgeResult<()> {
    fs::create_dir_all(path)
        .map_err(|_| BridgeError::new("update-cache", "Could not create update cache."))?;
    if fs::symlink_metadata(path)
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(true)
    {
        return Err(BridgeError::new(
            "update-cache",
            "Update cache path is invalid.",
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .map_err(|_| BridgeError::new("update-cache", "Could not secure update cache."))?;
    }
    Ok(())
}

fn atomic_json<T: Serialize>(path: &Path, value: &T, message: &'static str) -> BridgeResult<()> {
    let bytes = serde_json::to_vec(value).map_err(|_| BridgeError::host_state())?;
    create_private_dir(path.parent().expect("cache path parent"))?;
    let temporary = path.with_file_name(format!(
        ".{}.{}.tmp",
        path.file_name().and_then(|n| n.to_str()).unwrap_or("cache"),
        Uuid::new_v4()
    ));
    let result = (|| -> std::io::Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        fs::rename(&temporary, path)?;
        sync_directory(path.parent().expect("cache path parent"))
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
        return Err(BridgeError::new("update-cache", message));
    }
    Ok(())
}

fn sync_file(path: &Path) -> BridgeResult<()> {
    OpenOptions::new()
        .write(true)
        .open(path)
        .and_then(|f| f.sync_all())
        .map_err(|_| BridgeError::new("update-cache", "Could not finalize update payload."))
}

fn sync_dir(path: &Path) -> BridgeResult<()> {
    sync_directory(path)
        .map_err(|_| BridgeError::new("update-cache", "Could not finalize update cache."))
}

fn sync_directory(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        File::open(path)?.sync_all()
    }

    #[cfg(windows)]
    {
        // Windows does not support syncing directory handles. File contents are still flushed;
        // cache recovery verifies renamed entries, but this is not full power-loss durability.
        let _ = path;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn cached(payload: &str) -> CachedUpdate {
        CachedUpdate {
            version: "1.2.3".into(),
            channel: "stable".into(),
            package_kind: "app".into(),
            manifest: serde_json::json!({}),
            payload: payload.into(),
        }
    }

    #[test]
    fn corrupt_settings_are_not_treated_as_enabled_defaults() {
        let root = tempdir().unwrap();
        fs::create_dir_all(directory(root.path())).unwrap();
        fs::write(directory(root.path()).join("settings.json"), b"nope").unwrap();
        assert!(load_settings(root.path()).is_err());
    }

    #[test]
    fn settings_round_trip_and_rewrite() {
        let root = tempdir().unwrap();
        let initial = Settings {
            automatic: false,
            deferred_until: Some("2026-10-18T00:00:00Z".into()),
            attempted_version: Some("1.2.3".into()),
        };
        save_settings(root.path(), &initial).unwrap();
        let rewritten = Settings {
            automatic: true,
            deferred_until: None,
            attempted_version: Some("1.2.4".into()),
        };
        save_settings(root.path(), &rewritten).unwrap();

        let loaded = load_settings(root.path()).unwrap();
        assert!(loaded.automatic);
        assert_eq!(loaded.deferred_until, None);
        assert_eq!(loaded.attempted_version.as_deref(), Some("1.2.4"));
    }

    #[test]
    fn commit_is_generation_scoped_and_rejects_metadata_traversal() {
        let root = tempdir().unwrap();
        let partial = root.path().join("partial");
        fs::write(&partial, b"signed").unwrap();
        let path = commit_cached(root.path(), &cached("ignored"), &partial).unwrap();
        let (loaded, loaded_path) = load_cached(root.path()).unwrap().unwrap();
        assert_eq!(path, loaded_path);
        assert!(loaded.payload.starts_with("generations/"));
        fs::write(
            directory(root.path()).join("pending.json"),
            serde_json::to_vec(&cached("../secret")).unwrap(),
        )
        .unwrap();
        assert!(load_cached(root.path()).is_err());
    }

    #[test]
    fn commit_retains_active_generation_and_removal_cleans_owned_generations() {
        let root = tempdir().unwrap();
        let first = root.path().join("first");
        fs::write(&first, b"first").unwrap();
        let old = commit_cached(root.path(), &cached(""), &first).unwrap();
        let second = root.path().join("second");
        fs::write(&second, b"second").unwrap();
        let active = commit_cached(root.path(), &cached(""), &second).unwrap();
        assert!(!old.exists());
        assert!(active.exists());
        fs::write(
            directory(root.path()).join(".payload-crashed.partial"),
            b"partial",
        )
        .unwrap();
        sweep_orphans(root.path()).unwrap();
        assert!(active.exists());
        assert!(
            !directory(root.path())
                .join(".payload-crashed.partial")
                .exists()
        );
        remove_cached(root.path()).unwrap();
        assert!(!active.exists());
        assert!(
            directory(root.path())
                .join("generations")
                .read_dir()
                .unwrap()
                .next()
                .is_none()
        );
    }
}
