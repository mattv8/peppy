//! Cross-platform filesystem operations used by authenticated file publication.

use std::{fs, io, path::Path};

#[cfg(any(target_os = "emscripten", test))]
use std::{
    fs::{File, OpenOptions},
    io::Write,
};

/// Publishes `source` at `destination` without replacing an existing destination.
///
/// Native targets use a hard link. Emscripten MEMFS does not implement hard links, so its
/// single-owner runtime uses an exclusive file copy. A failed copy, flush, or sync removes
/// only the destination created by this call.
pub fn promote_no_clobber(source: &Path, destination: &Path) -> io::Result<()> {
    #[cfg(target_os = "emscripten")]
    {
        promote_by_exclusive_copy(source, destination)
    }
    #[cfg(not(target_os = "emscripten"))]
    {
        fs::hard_link(source, destination)
    }
}

#[cfg(any(target_os = "emscripten", test))]
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

#[cfg(any(target_os = "emscripten", test))]
fn create_private_destination(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options.open(path)
}

#[cfg(test)]
mod tests {
    use super::promote_by_exclusive_copy;
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
}
