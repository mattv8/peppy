//! Cross-platform filesystem operations used by authenticated file publication.

use std::{
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::Path,
};

/// Publishes `source` at `destination` without replacing an existing destination.
///
/// Linux and Android try an atomic no-replace rename, then a hard link, then an exclusive file
/// copy. Other native targets try a hard link, then the copy. Emscripten MEMFS does not implement
/// hard links, so its single-owner runtime uses only the exclusive copy. `AlreadyExists` is
/// returned at any stage and never falls through. A failed copy, flush, or sync removes only the
/// destination created by this call.
///
/// On `Ok`, `source` has either been consumed (rename) or may still exist (link or copy), so
/// callers must tolerate both.
pub fn promote_no_clobber(source: &Path, destination: &Path) -> io::Result<()> {
    #[cfg(target_os = "emscripten")]
    {
        promote_by_exclusive_copy(source, destination)
    }
    #[cfg(not(target_os = "emscripten"))]
    {
        match rename_no_replace(source, destination) {
            Err(error) if error.kind() != io::ErrorKind::AlreadyExists => {}
            result => return result,
        }
        match fs::hard_link(source, destination) {
            Err(error) if error.kind() != io::ErrorKind::AlreadyExists => {}
            result => return result,
        }
        promote_by_exclusive_copy(source, destination)
    }
}

/// Uses the raw syscall because bionic exports the `renameat2` wrapper only from API 30.
#[cfg(any(target_os = "linux", target_os = "android"))]
fn rename_no_replace(source: &Path, destination: &Path) -> io::Result<()> {
    let source = path_to_cstring(source)?;
    let destination = path_to_cstring(destination)?;
    // SAFETY: both pointers are NUL-terminated CStrings that outlive the call; the syscall reads no other memory.
    let result = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            libc::AT_FDCWD,
            source.as_ptr(),
            libc::AT_FDCWD,
            destination.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn path_to_cstring(path: &Path) -> io::Result<std::ffi::CString> {
    use std::os::unix::ffi::OsStrExt;
    std::ffi::CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))
}

#[cfg(not(any(target_os = "linux", target_os = "android", target_os = "emscripten")))]
fn rename_no_replace(_: &Path, _: &Path) -> io::Result<()> {
    Err(io::Error::from(io::ErrorKind::Unsupported))
}

fn promote_by_exclusive_copy(source: &Path, destination: &Path) -> io::Result<()> {
    let mut source = File::open(source)?;
    let result = {
        let mut destination_file = create_private_destination(destination)?;
        io::copy(&mut source, &mut destination_file)
            .and_then(|_| destination_file.flush())
            .and_then(|_| destination_file.sync_all())
    };
    if result.is_err() {
        let _ = fs::remove_file(destination);
    }
    result
}

fn create_private_destination(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options.open(path)
}

#[cfg(test)]
mod tests {
    use super::{promote_by_exclusive_copy, promote_no_clobber};
    use std::{fs, io, path::PathBuf};
    use uuid::Uuid;

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!("peppy-promotion-{}", Uuid::new_v4()));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn copy_backend_copies_bytes() {
        let directory = TestDirectory::new();
        let source = directory.0.join("source");
        let destination = directory.0.join("destination");
        fs::write(&source, b"ciphertext bytes").unwrap();

        promote_by_exclusive_copy(&source, &destination).unwrap();

        assert_eq!(fs::read(destination).unwrap(), b"ciphertext bytes");
    }

    #[test]
    fn copy_backend_does_not_overwrite_an_existing_destination() {
        let directory = TestDirectory::new();
        let source = directory.0.join("source");
        let destination = directory.0.join("destination");
        fs::write(&source, b"new ciphertext").unwrap();
        fs::write(&destination, b"verified ciphertext").unwrap();

        let error = promote_by_exclusive_copy(&source, &destination).unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read(destination).unwrap(), b"verified ciphertext");
    }

    #[test]
    fn copy_backend_removes_destination_after_copy_failure() {
        let directory = TestDirectory::new();
        let source = directory.0.join("source-directory");
        let destination = directory.0.join("destination");
        fs::create_dir(&source).unwrap();

        assert!(promote_by_exclusive_copy(&source, &destination).is_err());

        assert!(!destination.exists());
    }

    #[test]
    fn promote_creates_destination_with_source_bytes() {
        let directory = TestDirectory::new();
        let source = directory.0.join("source");
        let destination = directory.0.join("destination");
        fs::write(&source, b"ciphertext bytes").unwrap();

        promote_no_clobber(&source, &destination).unwrap();

        assert_eq!(fs::read(destination).unwrap(), b"ciphertext bytes");
    }

    #[test]
    fn promote_does_not_overwrite_an_existing_destination() {
        let directory = TestDirectory::new();
        let first = directory.0.join("first");
        let second = directory.0.join("second");
        let destination = directory.0.join("destination");
        fs::write(&first, b"first ciphertext").unwrap();
        fs::write(&second, b"second ciphertext").unwrap();
        promote_no_clobber(&first, &destination).unwrap();

        let error = promote_no_clobber(&second, &destination).unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read(destination).unwrap(), b"first ciphertext");
    }

    #[test]
    fn promoted_source_is_gone_or_removable() {
        let directory = TestDirectory::new();
        let source = directory.0.join("source");
        let destination = directory.0.join("destination");
        fs::write(&source, b"ciphertext bytes").unwrap();
        promote_no_clobber(&source, &destination).unwrap();

        match fs::remove_file(&source) {
            Ok(()) => {}
            Err(error) => assert_eq!(error.kind(), io::ErrorKind::NotFound),
        }
    }

    #[cfg(any(target_os = "linux", target_os = "android"))]
    #[test]
    fn rename_backend_consumes_source_and_never_replaces() {
        use super::rename_no_replace;

        let directory = TestDirectory::new();
        let source = directory.0.join("source");
        let destination = directory.0.join("destination");
        let existing = directory.0.join("existing");
        fs::write(&source, b"new ciphertext").unwrap();
        fs::write(&existing, b"verified ciphertext").unwrap();

        let error = rename_no_replace(&source, &existing).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert!(source.exists());

        rename_no_replace(&source, &destination).unwrap();

        assert!(!source.exists());
        assert_eq!(fs::read(destination).unwrap(), b"new ciphertext");
    }
}
