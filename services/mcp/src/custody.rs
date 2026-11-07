use std::{
    fs, io,
    io::Read,
    path::{Path, PathBuf},
};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use libsodium_rs::crypto_aead::xchacha20poly1305;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use zeroize::Zeroizing;

const RECORD_VERSION: u8 = 1;

#[derive(Debug, Error)]
pub enum CustodyError {
    #[error("worker custody storage is unavailable")]
    Io(#[from] io::Error),
    #[error("worker custody record is malformed or has been altered")]
    Invalid,
    #[error("worker custody crypto operation failed")]
    Crypto,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SealedRecord {
    version: u8,
    nonce: String,
    ciphertext: String,
}

/// A host-private KEK-backed record store. The KEK and worker control secret
/// deliberately live in different files; the latter never decrypts client state.
pub struct Custody {
    root: PathBuf,
    key: Zeroizing<[u8; xchacha20poly1305::KEYBYTES]>,
}

impl Custody {
    /// Initializes the worker-wide KEK exactly once, before any binding exists.
    pub fn bootstrap(
        root: impl AsRef<Path>,
        key_path: impl AsRef<Path>,
    ) -> Result<Self, CustodyError> {
        libsodium_rs::ensure_init().map_err(|_| CustodyError::Crypto)?;
        let root = root.as_ref().to_path_buf();
        fs::create_dir_all(&root)?;
        set_private_dir(&root)?;
        remove_stale_temps(&root)?;
        let key_path = key_path.as_ref();
        match fs::symlink_metadata(key_path) {
            Ok(_) => return Self::open_existing(&root, key_path),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        if fs::read_dir(&root)?.next().is_some() {
            return Err(CustodyError::Invalid);
        }
        let key = {
            let mut key = [0_u8; xchacha20poly1305::KEYBYTES];
            libsodium_rs::random::fill_bytes(&mut key);
            if let Some(parent) = key_path.parent() {
                fs::create_dir_all(parent)?;
                set_private_dir(parent)?;
            }
            write_new_private(key_path, &key)?;
            key
        };
        Ok(Self {
            root,
            key: Zeroizing::new(key),
        })
    }

    /// Opens custody using the startup-created KEK. Missing or unsafe keys are
    /// never replaced after startup.
    pub fn open_existing(
        root: impl AsRef<Path>,
        key_path: impl AsRef<Path>,
    ) -> Result<Self, CustodyError> {
        libsodium_rs::ensure_init().map_err(|_| CustodyError::Crypto)?;
        let root = root.as_ref().to_path_buf();
        fs::create_dir_all(&root)?;
        set_private_dir(&root)?;
        remove_stale_temps(&root)?;
        let bytes = read_private(key_path.as_ref())?;
        let key = bytes.try_into().map_err(|_| CustodyError::Invalid)?;
        Ok(Self {
            root,
            key: Zeroizing::new(key),
        })
    }

    pub fn store(&self, name: &str, aad: &[u8], plaintext: &[u8]) -> Result<(), CustodyError> {
        let path = self.record_path(name)?;
        let nonce = xchacha20poly1305::Nonce::generate();
        let key =
            xchacha20poly1305::Key::from_bytes(&*self.key).map_err(|_| CustodyError::Crypto)?;
        let ciphertext = xchacha20poly1305::encrypt(plaintext, Some(aad), &nonce, &key)
            .map_err(|_| CustodyError::Crypto)?;
        let record = SealedRecord {
            version: RECORD_VERSION,
            nonce: URL_SAFE_NO_PAD
                .encode(<xchacha20poly1305::Nonce as AsRef<[u8]>>::as_ref(&nonce)),
            ciphertext: URL_SAFE_NO_PAD.encode(ciphertext),
        };
        let encoded =
            Zeroizing::new(serde_json::to_vec(&record).map_err(|_| CustodyError::Invalid)?);
        write_private(&path, &encoded)?;
        Ok(())
    }

    pub fn load(&self, name: &str, aad: &[u8]) -> Result<Option<Zeroizing<Vec<u8>>>, CustodyError> {
        let path = self.record_path(name)?;
        let bytes = match fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let record: SealedRecord =
            serde_json::from_slice(&bytes).map_err(|_| CustodyError::Invalid)?;
        if record.version != RECORD_VERSION {
            return Err(CustodyError::Invalid);
        }
        let nonce = URL_SAFE_NO_PAD
            .decode(record.nonce)
            .map_err(|_| CustodyError::Invalid)?;
        let nonce: [u8; xchacha20poly1305::NPUBBYTES] =
            nonce.try_into().map_err(|_| CustodyError::Invalid)?;
        let ciphertext = URL_SAFE_NO_PAD
            .decode(record.ciphertext)
            .map_err(|_| CustodyError::Invalid)?;
        let key =
            xchacha20poly1305::Key::from_bytes(&*self.key).map_err(|_| CustodyError::Crypto)?;
        xchacha20poly1305::decrypt(
            &ciphertext,
            Some(aad),
            &xchacha20poly1305::Nonce::from_bytes(nonce),
            &key,
        )
        .map(|value| Some(Zeroizing::new(value)))
        .map_err(|_| CustodyError::Invalid)
    }

    pub fn remove(&self, name: &str) -> Result<(), CustodyError> {
        let path = self.record_path(name)?;
        match fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    fn record_path(&self, name: &str) -> Result<PathBuf, CustodyError> {
        if name.is_empty()
            || name.len() > 128
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        {
            return Err(CustodyError::Invalid);
        }
        Ok(self.root.join(format!("{name}.sealed")))
    }
}

pub(crate) fn read_private(path: &Path) -> Result<Vec<u8>, io::Error> {
    #[cfg(unix)]
    let mut file = {
        use std::os::unix::fs::OpenOptionsExt;

        let file = fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)?;
        validate_private_file(&file)?;
        file
    };
    #[cfg(not(unix))]
    let mut file = fs::File::open(path)?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    Ok(bytes)
}

#[cfg(unix)]
pub(crate) fn validate_private_file(file: &fs::File) -> Result<(), io::Error> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let metadata = file.metadata()?;
    if !metadata.file_type().is_file()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.permissions().mode() & 0o077 != 0
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "private file must be owner-owned, regular, and mode 0600",
        ));
    }
    Ok(())
}

fn write_private(path: &Path, bytes: &[u8]) -> Result<(), io::Error> {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "private file has no name"))?;
    let temporary = path.with_file_name(format!(".{name}.{}.tmp", uuid::Uuid::new_v4()));
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&temporary)?;
        io::Write::write_all(&mut file, bytes)?;
        file.sync_all()?;
    }
    #[cfg(not(unix))]
    fs::write(&temporary, bytes)?;
    fs::rename(temporary, path)?;
    if let Some(parent) = path.parent() {
        fs::File::open(parent)?.sync_all()?;
    }
    Ok(())
}

fn write_new_private(path: &Path, bytes: &[u8]) -> Result<(), io::Error> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let mut file = options.open(path)?;
    io::Write::write_all(&mut file, bytes)?;
    file.sync_all()?;
    #[cfg(unix)]
    validate_private_file(&file)?;
    if let Some(parent) = path.parent() {
        fs::File::open(parent)?.sync_all()?;
    }
    Ok(())
}

fn remove_stale_temps(root: &Path) -> Result<(), io::Error> {
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let name = entry.file_name();
        if name.to_string_lossy().starts_with('.') && name.to_string_lossy().ends_with(".tmp") {
            fs::remove_file(entry.path())?;
        }
    }
    Ok(())
}

fn set_private_dir(path: &Path) -> Result<(), io::Error> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encrypted_records_reopen_and_bind_to_aad() {
        let directory = tempfile::tempdir().unwrap();
        let key = directory.path().join("worker-kek");
        let first = Custody::bootstrap(directory.path(), &key).unwrap();
        first
            .store("vault", b"vault-a", b"credential and key cache")
            .unwrap();
        assert!(
            !fs::read(directory.path().join("vault.sealed"))
                .unwrap()
                .windows(10)
                .any(|item| item == b"credential")
        );
        drop(first);
        let reopened = Custody::open_existing(directory.path(), &key).unwrap();
        assert_eq!(
            &*reopened.load("vault", b"vault-a").unwrap().unwrap(),
            b"credential and key cache"
        );
        assert!(matches!(
            reopened.load("vault", b"vault-b"),
            Err(CustodyError::Invalid)
        ));
    }

    #[test]
    fn stale_write_temp_does_not_block_a_new_atomic_record() {
        let directory = tempfile::tempdir().unwrap();
        let key = directory.path().join("worker-kek");
        fs::write(directory.path().join(".vault.sealed.crash.tmp"), b"partial").unwrap();
        let custody = Custody::bootstrap(directory.path(), &key).unwrap();
        custody.store("vault", b"aad", b"complete").unwrap();
        assert_eq!(
            &*custody.load("vault", b"aad").unwrap().unwrap(),
            b"complete"
        );
    }

    #[cfg(unix)]
    #[test]
    fn existing_keys_must_be_private_regular_files_without_symlinks() {
        use std::os::unix::fs::{PermissionsExt, symlink};

        let directory = tempfile::tempdir().unwrap();
        let key = directory.path().join("worker-kek");
        fs::write(&key, [7; xchacha20poly1305::KEYBYTES]).unwrap();
        fs::set_permissions(&key, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(Custody::open_existing(directory.path(), &key).is_err());

        let target = directory.path().join("target");
        fs::write(&target, [7; xchacha20poly1305::KEYBYTES]).unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).unwrap();
        let link = directory.path().join("link");
        symlink(&target, &link).unwrap();
        assert!(Custody::open_existing(directory.path(), &link).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn private_file_loading_rejects_a_fifo_without_blocking() {
        use std::{
            ffi::CString,
            time::{Duration, Instant},
        };

        let directory = tempfile::tempdir().unwrap();
        let fifo = directory.path().join("secret-fifo");
        let fifo_name = CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(fifo_name.as_ptr(), 0o600) }, 0);

        let started = Instant::now();
        assert!(read_private(&fifo).is_err());
        assert!(started.elapsed() < Duration::from_secs(1));
    }
}
