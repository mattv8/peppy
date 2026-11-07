use std::{
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

const MARKER_NAME: &str = "owner.lock";
const ICON_NAME: &str = "icon.png";
const MARKER: &[u8] = b"peppy-notification-icon-owner-v1\n";

#[derive(Clone)]
pub(crate) struct Lease(Arc<LeaseInner>);

struct LeaseInner {
    directory: PathBuf,
    path: PathBuf,
    marker: Mutex<Option<File>>,
}

impl Drop for LeaseInner {
    fn drop(&mut self) {
        if let Ok(mut marker) = self.marker.lock() {
            cleanup_owned(&self.directory, &mut marker);
        }
    }
}

impl Lease {
    #[cfg_attr(not(any(target_os = "windows", target_os = "linux")), allow(dead_code))]
    pub(crate) fn path(&self) -> &Path {
        &self.0.path
    }
}

pub(crate) fn materialize(root: &Path, bytes: &[u8]) -> Option<Lease> {
    materialize_with_writer(root, bytes, crate::fsutil::write_private_atomic)
}

fn materialize_with_writer<F>(root: &Path, bytes: &[u8], write: F) -> Option<Lease>
where
    F: FnOnce(&Path, &[u8]) -> io::Result<()>,
{
    prepare_root(root).ok()?;
    recover(root);
    // Windows cannot rename a directory while the marker inside it is open, so the owner
    // directory is created in place. Recovery skips it until the marker is locked and complete.
    let directory = root.join(uuid::Uuid::new_v4().to_string());
    create_private_dir(&directory).ok()?;
    let marker_path = directory.join(MARKER_NAME);
    let Ok(mut marker) = create_marker(&marker_path) else {
        let _ = fs::remove_dir(&directory);
        return None;
    };
    if marker.lock().is_err() || marker.write_all(MARKER).is_err() || marker.sync_all().is_err() {
        drop(marker);
        let _ = fs::remove_file(&marker_path);
        let _ = fs::remove_dir(&directory);
        return None;
    }
    let path = directory.join(ICON_NAME);
    let lease = Lease(Arc::new(LeaseInner {
        directory,
        path: path.clone(),
        marker: Mutex::new(Some(marker)),
    }));
    write(&path, bytes).ok()?;
    Some(lease)
}

pub(crate) fn recover(root: &Path) {
    let Ok(root_metadata) = fs::symlink_metadata(root) else {
        return;
    };
    if root_metadata.file_type().is_symlink()
        || is_reparse_point(&root_metadata)
        || !root_metadata.is_dir()
    {
        return;
    }
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        if !canonical_uuid(entry.file_name().as_os_str()) {
            continue;
        }
        let directory = entry.path();
        let Ok(directory_metadata) = fs::symlink_metadata(&directory) else {
            continue;
        };
        if directory_metadata.file_type().is_symlink()
            || is_reparse_point(&directory_metadata)
            || !directory_metadata.is_dir()
        {
            continue;
        }
        let marker_path = directory.join(MARKER_NAME);
        let Ok(marker_metadata) = fs::symlink_metadata(&marker_path) else {
            continue;
        };
        if marker_metadata.file_type().is_symlink()
            || is_reparse_point(&marker_metadata)
            || !marker_metadata.is_file()
        {
            continue;
        }
        let Ok(marker) = OpenOptions::new().read(true).write(true).open(&marker_path) else {
            continue;
        };
        if fs::read(&marker_path).ok().as_deref() != Some(MARKER) {
            continue;
        }
        if marker.try_lock().is_err() {
            continue;
        }
        let mut marker = Some(marker);
        cleanup_owned(&directory, &mut marker);
    }
}

fn prepare_root(root: &Path) -> io::Result<()> {
    match fs::symlink_metadata(root) {
        Ok(metadata)
            if metadata.file_type().is_symlink()
                || is_reparse_point(&metadata)
                || !metadata.is_dir() =>
        {
            return Err(io::Error::other(
                "notification icon root is not a directory",
            ));
        }
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => create_private_dir(root)?,
        Err(error) => return Err(error),
    }
    #[cfg(unix)]
    fs::set_permissions(root, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

fn create_private_dir(path: &Path) -> io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path)
}

fn create_marker(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

fn cleanup_owned(directory: &Path, marker: &mut Option<File>) {
    let mut sensitive_cleanup_failed = false;
    for path in owned_sensitive_paths(directory) {
        if let Err(error) = fs::remove_file(path) {
            if error.kind() != io::ErrorKind::NotFound {
                sensitive_cleanup_failed = true;
            }
        }
    }
    if sensitive_cleanup_failed {
        return;
    }
    if !directory_contains_only_marker(directory) {
        return;
    }
    drop(marker.take());
    let marker_path = directory.join(MARKER_NAME);
    if fs::remove_file(marker_path).is_err_and(|error| error.kind() != io::ErrorKind::NotFound) {
        return;
    }
    let _ = fs::remove_dir(directory);
}

fn directory_contains_only_marker(directory: &Path) -> bool {
    let Ok(mut entries) = fs::read_dir(directory) else {
        return false;
    };
    matches!(
        (entries.next(), entries.next()),
        (Some(Ok(entry)), None) if entry.file_name() == std::ffi::OsStr::new(MARKER_NAME)
    )
}

fn owned_sensitive_paths(directory: &Path) -> Vec<PathBuf> {
    let icon = directory.join(ICON_NAME);
    let mut paths = is_regular_private_file(&icon)
        .then_some(icon)
        .into_iter()
        .collect();
    let Ok(entries) = fs::read_dir(directory) else {
        return paths;
    };
    paths.extend(entries.flatten().filter_map(|entry| {
        let path = entry.path();
        (is_regular_private_file(&path) && icon_temporary_name(entry.file_name().as_os_str()))
            .then_some(entry.path())
    }));
    paths
}

fn is_regular_private_file(path: &Path) -> bool {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return false;
    };
    !metadata.file_type().is_symlink() && !is_reparse_point(&metadata) && metadata.is_file()
}

fn is_reparse_point(metadata: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        let _ = metadata;
        false
    }
}

fn canonical_uuid(name: &std::ffi::OsStr) -> bool {
    let Some(name) = name.to_str() else {
        return false;
    };
    uuid::Uuid::parse_str(name).is_ok_and(|uuid| uuid.to_string() == name)
}

fn icon_temporary_name(name: &std::ffi::OsStr) -> bool {
    let Some(name) = name.to_str() else {
        return false;
    };
    let Some(uuid) = name
        .strip_prefix(".icon.png.")
        .and_then(|name| name.strip_suffix(".tmp"))
    else {
        return false;
    };
    uuid.len() == 32
        && uuid
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{BufRead, BufReader, Read},
        process::{Child, Command, Stdio},
        sync::mpsc,
    };

    fn live_lease(root: &Path) -> Lease {
        materialize(root, b"private png").unwrap()
    }

    #[test]
    fn leased_png_is_readable_without_locking_content() {
        let directory = tempfile::tempdir().unwrap();
        let lease = live_lease(directory.path());
        assert_eq!(fs::read(lease.path()).unwrap(), b"private png");
    }

    #[test]
    fn concurrent_last_clone_drop_cleans_once_and_preserves_unrelated_files() {
        let directory = tempfile::tempdir().unwrap();
        let unrelated = directory.path().join("keep");
        fs::write(&unrelated, b"keep").unwrap();
        let lease = live_lease(directory.path());
        let owner = lease.0.directory.clone();
        let other = lease.clone();
        std::thread::scope(|scope| {
            scope.spawn(move || drop(lease));
            scope.spawn(move || drop(other));
        });
        assert!(!owner.exists());
        assert_eq!(fs::read(unrelated).unwrap(), b"keep");
    }

    #[test]
    fn recovery_preserves_live_owner_during_publication_and_write() {
        let directory = tempfile::tempdir().unwrap();
        let (entered_tx, entered_rx) = mpsc::channel();
        let (continue_tx, continue_rx) = mpsc::channel();
        let root = directory.path().to_owned();
        let join = std::thread::spawn(move || {
            materialize_with_writer(&root, b"private", |path, bytes| {
                entered_tx.send(path.parent().unwrap().to_owned()).unwrap();
                continue_rx.recv().unwrap();
                crate::fsutil::write_private_atomic(path, bytes)
            })
        });
        let owner = entered_rx.recv().unwrap();
        let entries: Vec<_> = fs::read_dir(&owner)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(entries, [MARKER_NAME]);
        recover(directory.path());
        assert!(owner.exists());
        continue_tx.send(()).unwrap();
        drop(join.join().unwrap());
        recover(directory.path());
        assert!(!owner.exists());
    }

    #[test]
    fn write_failure_after_final_publication_removes_sensitive_bytes() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        assert!(materialize_with_writer(root, b"private", |path, bytes| {
            fs::write(path, bytes)?;
            Err(io::Error::other("injected post-rename failure"))
        })
        .is_none());
        assert_eq!(fs::read_dir(root).unwrap().count(), 0);
    }

    #[test]
    fn recovery_removes_only_owned_partial_atomic_temporaries() {
        let directory = tempfile::tempdir().unwrap();
        let owner = directory.path().join(uuid::Uuid::new_v4().to_string());
        create_private_dir(&owner).unwrap();
        let mut marker = create_marker(&owner.join(MARKER_NAME)).unwrap();
        marker.write_all(MARKER).unwrap();
        drop(marker);
        let temporary = owner.join(format!(".icon.png.{}.tmp", uuid::Uuid::new_v4().simple()));
        fs::write(&temporary, b"partial").unwrap();
        let unknown = owner.join(".other.tmp");
        fs::write(&unknown, b"keep").unwrap();
        let hyphenated = owner.join(format!(".icon.png.{}.tmp", uuid::Uuid::new_v4()));
        fs::write(&hyphenated, b"keep").unwrap();
        let uppercase = owner.join(format!(
            ".icon.png.{}.tmp",
            uuid::Uuid::new_v4().simple().to_string().to_uppercase()
        ));
        fs::write(&uppercase, b"keep").unwrap();
        recover(directory.path());
        assert!(!temporary.exists());
        assert!(
            owner.exists(),
            "unknown files prevent removing the owner directory"
        );
        assert!(unknown.exists());
        assert!(hyphenated.exists());
        assert!(uppercase.exists());
        assert!(owner.join(MARKER_NAME).exists());
    }

    #[cfg(unix)]
    #[test]
    fn recovery_preserves_an_icon_symlink() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().unwrap();
        let owner = directory.path().join(uuid::Uuid::new_v4().to_string());
        create_private_dir(&owner).unwrap();
        let mut marker = create_marker(&owner.join(MARKER_NAME)).unwrap();
        marker.write_all(MARKER).unwrap();
        drop(marker);
        let target = directory.path().join("outside-private-icon");
        fs::write(&target, b"keep").unwrap();
        let icon = owner.join(ICON_NAME);
        symlink(&target, &icon).unwrap();

        recover(directory.path());

        assert!(icon.exists());
        assert_eq!(fs::read(&target).unwrap(), b"keep");
        assert!(owner.join(MARKER_NAME).exists());
    }

    #[test]
    fn cleanup_failure_retains_the_marker_for_later_recovery() {
        let directory = tempfile::tempdir().unwrap();
        let lease = live_lease(directory.path());
        let owner = lease.0.directory.clone();
        fs::remove_file(lease.path()).unwrap();
        fs::create_dir(lease.path()).unwrap();
        drop(lease);
        assert!(owner.join(MARKER_NAME).exists());
        fs::remove_dir(owner.join(ICON_NAME)).unwrap();
        recover(directory.path());
        assert!(!owner.exists());
    }

    #[test]
    fn recovery_skips_unknown_invalid_and_symlink_entries() {
        let directory = tempfile::tempdir().unwrap();
        let unknown = directory.path().join("unknown");
        fs::create_dir(&unknown).unwrap();
        let invalid = directory.path().join("not-a-uuid");
        fs::create_dir(&invalid).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            let target = directory.path().join(uuid::Uuid::new_v4().to_string());
            fs::create_dir(&target).unwrap();
            let link = directory.path().join(uuid::Uuid::new_v4().to_string());
            symlink(&target, &link).unwrap();
            let owned = directory.path().join(uuid::Uuid::new_v4().to_string());
            fs::create_dir(&owned).unwrap();
            let marker_target = directory.path().join("marker-target");
            fs::write(&marker_target, MARKER).unwrap();
            symlink(&marker_target, owned.join(MARKER_NAME)).unwrap();
            recover(directory.path());
            assert!(link.exists());
            assert!(target.exists());
            assert!(owned.exists());
            assert!(owned.join(MARKER_NAME).exists());
        }
        #[cfg(not(unix))]
        recover(directory.path());
        assert!(unknown.exists());
        assert!(invalid.exists());
    }

    #[test]
    fn child_holds_a_live_owner_until_its_stdin_closes() {
        let Ok(root) = std::env::var("PEPPY_ICON_CHILD_ROOT") else {
            return;
        };
        let lease = live_lease(Path::new(&root));
        println!("READY:{}", lease.0.directory.display());
        let _ = std::io::stdin().read_to_end(&mut Vec::new());
        drop(lease);
    }

    struct ChildGuard(Child);

    impl Drop for ChildGuard {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    #[test]
    fn recovery_skips_live_child_owner_then_recovers_after_death() {
        let directory = tempfile::tempdir().unwrap();
        let mut child = ChildGuard(
            Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "notification_icon::files::tests::child_holds_a_live_owner_until_its_stdin_closes",
                    "--nocapture",
                ])
                .env("PEPPY_ICON_CHILD_ROOT", directory.path())
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .spawn()
                .unwrap(),
        );
        let stdout = child.0.stdout.take().unwrap();
        let mut lines = BufReader::new(stdout).lines();
        let ready = loop {
            let line = lines.next().unwrap().unwrap();
            if let Some(path) = line.strip_prefix("READY:") {
                break PathBuf::from(path);
            }
        };
        recover(directory.path());
        assert!(ready.exists());
        child.0.kill().unwrap();
        child.0.wait().unwrap();
        recover(directory.path());
        assert!(!ready.exists());
    }
}
