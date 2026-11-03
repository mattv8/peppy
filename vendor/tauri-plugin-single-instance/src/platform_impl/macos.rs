// Copyright 2019-2023 Tauri Programme within The Commons Conservancy
// SPDX-License-Identifier: Apache-2.0
// SPDX-License-Identifier: MIT

use std::{
    io::{BufWriter, Error, ErrorKind, Write},
    os::fd::AsRawFd,
    os::unix::net::{UnixListener, UnixStream},
    path::PathBuf,
    thread,
    time::{Duration, Instant},
};

use crate::SingleInstanceCallback;
#[cfg(feature = "semver")]
use crate::semver_compat::semver_compat_string;
use tauri::{
    AppHandle, Config, Manager, RunEvent, Runtime,
    plugin::{self, TauriPlugin},
};
use tokio::io::AsyncReadExt;

pub fn init<R: Runtime>(cb: Box<SingleInstanceCallback<R>>) -> TauriPlugin<R> {
    plugin::Builder::new("single-instance")
        .setup(|app, _api| {
            let socket = socket_path(app.config(), app.package_info());
            match acquire_lock_with_retry(lock_path(app.config())) {
                Ok(lock) => {
                    app.manage(LockFile(lock));
                    if let Some(socket) = socket {
                        socket_cleanup(&socket);
                        if let Err(error) = listen_for_other_instances(socket, app.clone(), cb) {
                            tracing::error!("single_instance activation forwarding is unavailable: {error}");
                        }
                    } else {
                        tracing::warn!("single_instance activation forwarding is unavailable: socket path is too long");
                    }
                }
                Err(error) if error.kind() == ErrorKind::WouldBlock => {
                    if let Some(socket) = socket {
                        match notify_singleton(&socket) {
                            Ok(_) => {}
                            Err(error) => tracing::error!("single_instance failed to notify the locked primary: {error}"),
                        }
                    } else {
                        tracing::warn!("single_instance activation forwarding is unavailable: socket path is too long");
                    }
                    std::process::exit(0);
                }
                Err(error) => return Err(error.into()),
            }
            Ok(())
        })
        .on_event(|app, event| {
            if let RunEvent::Exit = event {
                destroy(app);
            }
        })
        .build()
}

pub fn destroy<R: Runtime, M: Manager<R>>(manager: &M) {
    if let Some(socket) = socket_path(manager.config(), manager.package_info()) {
        socket_cleanup(&socket);
    }
}

struct LockFile(std::fs::File);

impl Drop for LockFile {
    fn drop(&mut self) {
        // SAFETY: the file descriptor stays open for the lifetime of this lock.
        unsafe { libc::flock(self.0.as_raw_fd(), libc::LOCK_UN) };
    }
}

fn socket_path(config: &Config, _package_info: &tauri::PackageInfo) -> Option<PathBuf> {
    socket_path_for_tmpdir(std::env::var_os("TMPDIR"), &config.identifier)
}

fn socket_path_for_tmpdir(
    tmpdir: Option<std::ffi::OsString>,
    bundle_identifier: &str,
) -> Option<PathBuf> {
    use sha2::{Digest, Sha256};

    let digest = Sha256::digest(bundle_identifier.as_bytes());
    let identifier = digest[..8]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();

    #[cfg(feature = "semver")]
    let identifier = format!(
        "{identifier}_{}",
        semver_compat_string(&_package_info.version),
    );

    let socket_name = format!("peppy-{identifier}.sock");
    let maximum_directory_length = 103usize.saturating_sub(socket_name.len() + 1);
    let directory = tmpdir
        .map(PathBuf::from)
        .filter(|path| path.as_os_str().len() <= maximum_directory_length)?;
    Some(directory.join(socket_name))
}

fn lock_path(config: &Config) -> PathBuf {
    lock_path_for_tmpdir(std::env::var_os("TMPDIR"), &config.identifier)
}

fn lock_path_for_tmpdir(tmpdir: Option<std::ffi::OsString>, bundle_identifier: &str) -> PathBuf {
    use sha2::{Digest, Sha256};

    let identifier = Sha256::digest(bundle_identifier.as_bytes());
    let identifier = identifier[..8]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    tmpdir
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join(format!("peppy-{identifier}.lock"))
}

fn acquire_lock(path: PathBuf) -> Result<std::fs::File, Error> {
    let file = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(path)?;
    // SAFETY: flock operates on the valid descriptor held by `file`.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
        Ok(file)
    } else {
        Err(Error::last_os_error())
    }
}

fn acquire_lock_with_retry(path: PathBuf) -> Result<std::fs::File, Error> {
    const RETRY_TIMEOUT: Duration = Duration::from_secs(2);
    const RETRY_INTERVAL: Duration = Duration::from_millis(25);

    let deadline = Instant::now() + RETRY_TIMEOUT;
    loop {
        match acquire_lock(path.clone()) {
            Ok(lock) => return Ok(lock),
            Err(error) if error.kind() == ErrorKind::WouldBlock && Instant::now() < deadline => {
                thread::sleep(RETRY_INTERVAL);
            }
            Err(error) => return Err(error),
        }
    }
}

fn socket_cleanup(socket: &PathBuf) {
    let _ = std::fs::remove_file(socket);
}

fn notify_singleton(socket: &PathBuf) -> Result<(), Error> {
    let stream = UnixStream::connect(socket)?;
    let mut bf = BufWriter::new(&stream);
    let cwd = std::env::current_dir()
        .unwrap_or_default()
        .to_str()
        .unwrap_or_default()
        .to_string();
    bf.write_all(cwd.as_bytes())?;
    bf.write_all(b"\0\0")?;
    let args_joined = std::env::args().collect::<Vec<String>>().join("\0");
    bf.write_all(args_joined.as_bytes())?;
    bf.flush()?;
    drop(bf);
    Ok(())
}

fn listen_for_other_instances<A: Runtime>(
    socket: PathBuf,
    app: AppHandle<A>,
    mut cb: Box<SingleInstanceCallback<A>>,
) -> Result<(), Error> {
    let listener = UnixListener::bind(&socket)?;
    listener.set_nonblocking(true)?;
    tauri::async_runtime::spawn(async move {
        match tokio::net::UnixListener::from_std(listener) {
            Ok(listener) => loop {
                match listener.accept().await {
                    Ok((mut stream, _addr)) => {
                        let mut s = String::new();
                        match stream.read_to_string(&mut s).await {
                            Ok(_) => {
                                let (cwd, args) = s.split_once("\0\0").unwrap_or_default();
                                let args: Vec<String> =
                                    args.split('\0').map(String::from).collect();
                                cb(app.app_handle(), args, cwd.to_string());
                            }
                            Err(e) => {
                                tracing::debug!("single_instance failed to be notified: {e}")
                            }
                        }
                    }
                    Err(err) => {
                        tracing::debug!("single_instance failed to be notified: {}", err);
                        continue;
                    }
                }
            },
            Err(err) => tracing::error!("single_instance failed to initialize listener: {}", err),
        }
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{sync::mpsc, thread, time::Duration};

    use super::{
        acquire_lock, acquire_lock_with_retry, lock_path_for_tmpdir, socket_path_for_tmpdir,
    };

    #[test]
    fn long_sandboxed_tmpdir_keeps_a_lock_when_socket_forwarding_cannot_fit() {
        let tmpdir = format!(
            "/Users/{}/Library/Containers/org.peppy.desktop/Data/tmp",
            "x".repeat(80)
        );
        assert!(socket_path_for_tmpdir(Some(tmpdir.clone().into()), "org.peppy.desktop").is_none());
        let lock = lock_path_for_tmpdir(Some(tmpdir.into()), "org.peppy.desktop");
        assert!(lock.as_os_str().len() > 103);
    }

    #[test]
    fn socket_path_stays_within_sun_path_when_tmpdir_fits() {
        let path =
            socket_path_for_tmpdir(Some("/private/tmp".into()), "org.peppy.desktop").unwrap();
        assert!(path.as_os_str().len() <= 103);
    }

    #[test]
    fn retries_a_released_lock_before_treating_the_process_as_secondary() {
        let path = std::env::temp_dir().join(format!(
            "peppy-single-instance-test-{}-{}.lock",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let (locked, release) = mpsc::channel();
        let held_path = path.clone();
        let holder = thread::spawn(move || {
            let lock = acquire_lock(held_path).unwrap();
            locked.send(()).unwrap();
            thread::sleep(Duration::from_millis(50));
            drop(lock);
        });
        release.recv().unwrap();

        let lock = acquire_lock_with_retry(path.clone()).unwrap();
        drop(lock);
        holder.join().unwrap();
        std::fs::remove_file(path).unwrap();
    }
}
