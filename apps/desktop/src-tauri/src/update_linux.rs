//! Linux system-package updates. This module deliberately never starts a shell,
//! sudo, or the Tauri updater for deb/rpm payloads.
//!
//! The native update coordinator supplies only a previously verified private-cache
//! path. PackageKit owns authorization through Polkit.

use std::path::Path;
#[cfg(target_os = "linux")]
use std::sync::Mutex;
#[cfg(target_os = "linux")]
use std::time::Duration;

pub const PEPPY_PACKAGE_NAME: &str = "peppy";
const PACKAGEKIT_EXIT_SUCCESS: u32 = 1;
const PACKAGEKIT_EXIT_NEED_UNTRUSTED: u32 = 8;
const PACKAGEKIT_ERROR_DEP_RESOLUTION_FAILED: u32 = 13;
const PACKAGEKIT_ERROR_TRANSACTION_CANCELLED: u32 = 17;
const PACKAGEKIT_ERROR_CANNOT_GET_LOCK: u32 = 26;
const PACKAGEKIT_ERROR_BAD_GPG_SIGNATURE: u32 = 30;
const PACKAGEKIT_ERROR_MISSING_GPG_SIGNATURE: u32 = 31;
const PACKAGEKIT_ERROR_NOT_AUTHORIZED: u32 = 48;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Capability {
    Supported,
    Unsupported,
    AuthorizationRequired,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallOutcome {
    Success { installed_version: String },
    Unsupported { message: String },
    AuthorizationRequired,
    Cancelled,
    Locked,
    CompletionUnknown { message: String },
    SettledUnknown { message: String },
    Failed { message: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransactionEvent {
    FinishedSuccess,
    FinishedCancelled,
    FinishedLocked,
    FinishedAuthorizationRequired,
    FinishedUntrustedPackage,
    CompletionUnknown(String),
    SettledUnknown(String),
    FinishedFailed(String),
}

/// The repository route never accepts an ID supplied by a caller. A backend may
/// decline it only when no exact trusted configured-repository target exists;
/// terminal PackageKit outcomes must not fall through to local installation.
pub enum TrustedRepositoryAttempt {
    Unavailable,
    NoExactTrustedTarget,
    Outcome(TransactionEvent),
}

/// Interpret only a terminal PackageKit event. In particular, a cancelled
/// authorization request never falls through to another installer mechanism.
pub fn interpret_transaction(
    event: TransactionEvent,
    installed_version: Option<String>,
) -> InstallOutcome {
    match event {
        TransactionEvent::FinishedSuccess => installed_version
            .map(|installed_version| InstallOutcome::Success { installed_version })
            .unwrap_or_else(|| InstallOutcome::Failed {
                message: "PackageKit completed without a trusted installed Peppy package".into(),
            }),
        TransactionEvent::FinishedCancelled => InstallOutcome::Cancelled,
        TransactionEvent::FinishedLocked => InstallOutcome::Locked,
        TransactionEvent::FinishedAuthorizationRequired => InstallOutcome::AuthorizationRequired,
        TransactionEvent::FinishedUntrustedPackage => InstallOutcome::Unsupported {
            message: "PackageKit refused the verified local package; configure the signed Peppy repository or install it with your distribution package manager".into(),
        },
        TransactionEvent::CompletionUnknown(message) => InstallOutcome::CompletionUnknown { message },
        TransactionEvent::SettledUnknown(message) => InstallOutcome::SettledUnknown { message },
        TransactionEvent::FinishedFailed(message) => InstallOutcome::Failed { message },
    }
}

/// Boundary used by the coordinator and unit tests. The production implementation
/// is a zbus PackageKit client supplied by native integration.
#[allow(async_fn_in_trait)]
pub trait PackageKitBackend {
    async fn available(&self) -> bool;
    /// Installed-version observation is not proof that a separate OS update has
    /// finished. External restart monitoring requires the daemon's idle state.
    async fn daemon_is_idle(&self) -> bool {
        false
    }
    async fn install_files(&self, verified_path: &Path, interactive: bool) -> TransactionEvent;
    async fn install_trusted_peppy(
        &self,
        _expected_version: &str,
        _interactive: bool,
    ) -> TrustedRepositoryAttempt {
        TrustedRepositoryAttempt::Unavailable
    }
    /// Re-observe a timed-out transaction. Backends retain the transaction
    /// identity; callers never provide a D-Bus path or package ID.
    async fn wait_for_pending(&self) -> Option<TransactionEvent> {
        None
    }
    async fn has_pending_transaction(&self) -> bool {
        false
    }
    /// Return the version of the installed `peppy` package, not a display name or
    /// an arbitrary package selected by an IPC caller.
    async fn installed_peppy_version(&self) -> Option<String>;
}

/// Production PackageKit system-bus client. It is Linux-only because PackageKit
/// and its Polkit authorization boundary are Linux platform services.
///
/// The native integration owner must add the Linux-target dependency
/// `zbus = { version = "5", default-features = false, features = ["tokio"] }`.
#[cfg(target_os = "linux")]
pub struct ZbusPackageKit {
    connection: zbus::Connection,
    deadline: Duration,
    cancellation_grace: Duration,
    pending_transaction: Mutex<Option<String>>,
}

#[cfg(target_os = "linux")]
impl ZbusPackageKit {
    const SERVICE: &'static str = "org.freedesktop.PackageKit";
    const ROOT_PATH: &'static str = "/org/freedesktop/PackageKit";
    const ROOT_INTERFACE: &'static str = "org.freedesktop.PackageKit";
    const TRANSACTION_INTERFACE: &'static str = "org.freedesktop.PackageKit.Transaction";
    // ONLY_TRUSTED is only valid on mutating PackageKit calls. Query methods
    // take filter bitfields, where zero means no filter.
    const ONLY_TRUSTED: u64 = 1 << 1;
    const FILTER_NONE: u64 = 0;
    const FILTER_INSTALLED: u64 = 1 << 2;

    pub async fn connect() -> Result<Self, String> {
        let connection = zbus::Connection::system()
            .await
            .map_err(|error| error.to_string())?;
        Ok(Self {
            connection,
            deadline: Duration::from_secs(10 * 60),
            cancellation_grace: Duration::from_secs(30),
            pending_transaction: Mutex::new(None),
        })
    }

    pub fn with_deadline(mut self, deadline: Duration, cancellation_grace: Duration) -> Self {
        self.deadline = deadline;
        self.cancellation_grace = cancellation_grace;
        self
    }

    async fn transaction(&self) -> Result<(zbus::Proxy<'_>, String), String> {
        let root = zbus::Proxy::new(
            &self.connection,
            Self::SERVICE,
            Self::ROOT_PATH,
            Self::ROOT_INTERFACE,
        )
        .await
        .map_err(|error| error.to_string())?;
        let path: zbus::zvariant::OwnedObjectPath =
            tokio::time::timeout(self.deadline, root.call("CreateTransaction", &()))
                .await
                .map_err(|_| "PackageKit CreateTransaction timed out".to_string())?
                .map_err(|error| error.to_string())?;
        let transaction = zbus::Proxy::new(
            &self.connection,
            Self::SERVICE,
            path.clone(),
            Self::TRANSACTION_INTERFACE,
        )
        .await
        .map_err(|error| error.to_string())?;
        Ok((transaction, path.to_string()))
    }

    async fn install_files_transaction(&self, path: &Path, interactive: bool) -> TransactionEvent {
        use futures_util::StreamExt;

        let path = match std::fs::symlink_metadata(path)
            .ok()
            .filter(|metadata| metadata.file_type().is_file() && !metadata.file_type().is_symlink())
            .and_then(|_| std::fs::canonicalize(path).ok())
        {
            Some(path) => path.to_string_lossy().into_owned(),
            None => {
                return TransactionEvent::FinishedFailed(
                    "verified package cache path is not a local file".into(),
                );
            }
        };
        let (transaction, transaction_path) = match self.transaction().await {
            Ok(transaction) => transaction,
            Err(error) => return TransactionEvent::FinishedFailed(error),
        };

        // Subscribe before invoking InstallFiles so a fast transaction cannot
        // finish between the method reply and subscription setup.
        let mut finished = match transaction.receive_signal("Finished").await {
            Ok(stream) => stream,
            Err(error) => return TransactionEvent::FinishedFailed(error.to_string()),
        };
        let mut errors = match transaction.receive_signal("ErrorCode").await {
            Ok(stream) => stream,
            Err(error) => return TransactionEvent::FinishedFailed(error.to_string()),
        };
        let hint = format!("interactive={interactive}");
        if let Err(error) = tokio::time::timeout(
            self.deadline,
            transaction.call::<_, _, ()>("SetHints", &vec![hint]),
        )
        .await
        .map_err(|_| "PackageKit SetHints timed out".to_string())
        .and_then(|result| result.map_err(|error| error.to_string()))
        {
            return TransactionEvent::FinishedFailed(error.to_string());
        }
        if let Err(error) = tokio::time::timeout(
            self.deadline,
            transaction.call::<_, _, ()>("InstallFiles", &(Self::ONLY_TRUSTED, vec![path])),
        )
        .await
        .map_err(|_| "PackageKit InstallFiles timed out".to_string())
        .and_then(|result| result.map_err(|error| error.to_string()))
        {
            return TransactionEvent::FinishedFailed(error.to_string());
        }

        let completion = tokio::time::timeout(self.deadline, async {
            let mut error = None;
            loop {
                tokio::select! {
                    biased;
                    signal = errors.next() => match signal {
                        Some(signal) => error = packagekit_error(&signal),
                        None => return TransactionEvent::CompletionUnknown("PackageKit closed its error signal stream before completion".into()),
                    },
                    signal = finished.next() => match signal {
                        Some(signal) => return terminal_event(&signal, error),
                        None => return TransactionEvent::CompletionUnknown("PackageKit closed its transaction signal stream before completion".into()),
                    },
                }
            }
        }).await;

        let event = match completion {
            Ok(event) => event,
            Err(_) => {
                // Request cancellation, but do not claim that a timeout stopped
                // the OS transaction. The coordinator must retain its operation
                // fence until it has independently observed completion.
                let _ = tokio::time::timeout(
                    self.cancellation_grace,
                    transaction.call::<_, _, ()>("Cancel", &()),
                )
                .await;
                match tokio::time::timeout(self.cancellation_grace, finished.next()).await {
                    Ok(Some(signal)) => terminal_event(&signal, None),
                    _ => {
                        *self
                            .pending_transaction
                            .lock()
                            .expect("pending transaction mutex poisoned") =
                            Some(transaction_path.clone());
                        TransactionEvent::CompletionUnknown("PackageKit deadline elapsed; cancellation was requested but transaction completion is unknown".into())
                    }
                }
            }
        };
        if matches!(event, TransactionEvent::CompletionUnknown(_)) {
            *self
                .pending_transaction
                .lock()
                .expect("pending transaction mutex poisoned") = Some(transaction_path);
        }
        event
    }

    async fn pending_transaction(&self, path: &str) -> Result<zbus::Proxy<'_>, String> {
        let path = zbus::zvariant::OwnedObjectPath::try_from(path)
            .map_err(|error| format!("invalid retained PackageKit transaction path: {error}"))?;
        zbus::Proxy::new(
            &self.connection,
            Self::SERVICE,
            path,
            Self::TRANSACTION_INTERFACE,
        )
        .await
        .map_err(|error| error.to_string())
    }

    async fn retained_transaction_exists(&self, path: &str) -> Result<bool, String> {
        let root = zbus::Proxy::new(
            &self.connection,
            Self::SERVICE,
            Self::ROOT_PATH,
            Self::ROOT_INTERFACE,
        )
        .await
        .map_err(|error| error.to_string())?;
        let paths: Vec<zbus::zvariant::OwnedObjectPath> =
            tokio::time::timeout(self.deadline, root.call("GetTransactionList", &()))
                .await
                .map_err(|_| "PackageKit transaction-list lookup timed out".to_string())?
                .map_err(|error| error.to_string())?;
        Ok(paths.iter().any(|candidate| candidate.as_str() == path))
    }

    async fn wait_for_retained_transaction(&self) -> Option<TransactionEvent> {
        use futures_util::StreamExt;

        let path = self
            .pending_transaction
            .lock()
            .expect("pending transaction mutex poisoned")
            .clone()?;
        match self.retained_transaction_exists(&path).await {
            Ok(false) => {
                *self
                    .pending_transaction
                    .lock()
                    .expect("pending transaction mutex poisoned") = None;
                return Some(TransactionEvent::SettledUnknown(
                    "PackageKit no longer retains the transaction; verify package state and restart manually if needed".into(),
                ));
            }
            Err(error) => return Some(TransactionEvent::CompletionUnknown(error)),
            Ok(true) => {}
        }
        let transaction =
            match tokio::time::timeout(self.deadline, self.pending_transaction(&path)).await {
                Ok(Ok(transaction)) => transaction,
                Ok(Err(error)) => return Some(TransactionEvent::CompletionUnknown(error)),
                Err(_) => {
                    return Some(TransactionEvent::CompletionUnknown(
                        "PackageKit transaction reattachment timed out".into(),
                    ));
                }
            };
        let mut finished = match transaction.receive_signal("Finished").await {
            Ok(stream) => stream,
            Err(error) => return Some(TransactionEvent::CompletionUnknown(error.to_string())),
        };
        let mut errors = match transaction.receive_signal("ErrorCode").await {
            Ok(stream) => stream,
            Err(error) => return Some(TransactionEvent::CompletionUnknown(error.to_string())),
        };
        let event = match tokio::time::timeout(self.deadline, async {
            let mut error = None;
            loop {
                tokio::select! {
                    biased;
                    signal = errors.next() => match signal {
                        Some(signal) => error = packagekit_error(&signal),
                        None => return TransactionEvent::CompletionUnknown("PackageKit closed its error stream while reconciling".into()),
                    },
                    signal = finished.next() => match signal {
                        Some(signal) => return terminal_event(&signal, error),
                        None => return TransactionEvent::CompletionUnknown("PackageKit closed its completion stream while reconciling".into()),
                    },
                }
            }
        }).await {
            Ok(event) => event,
            Err(_) => TransactionEvent::CompletionUnknown("PackageKit reconciliation deadline elapsed".into()),
        };
        if !matches!(event, TransactionEvent::CompletionUnknown(_)) {
            *self
                .pending_transaction
                .lock()
                .expect("pending transaction mutex poisoned") = None;
        }
        Some(event)
    }

    async fn exact_trusted_update_id(
        &self,
        expected_version: &str,
    ) -> Result<Option<String>, TransactionEvent> {
        use futures_util::StreamExt;

        let (transaction, _) = self
            .transaction()
            .await
            .map_err(TransactionEvent::FinishedFailed)?;
        let mut packages = transaction
            .receive_signal("Package")
            .await
            .map_err(|error| TransactionEvent::FinishedFailed(error.to_string()))?;
        let mut finished = transaction
            .receive_signal("Finished")
            .await
            .map_err(|error| TransactionEvent::FinishedFailed(error.to_string()))?;
        let mut errors = transaction
            .receive_signal("ErrorCode")
            .await
            .map_err(|error| TransactionEvent::FinishedFailed(error.to_string()))?;
        tokio::time::timeout(
            self.deadline,
            transaction.call::<_, _, ()>("GetUpdates", &(Self::FILTER_NONE,)),
        )
        .await
        .map_err(|_| TransactionEvent::FinishedFailed("PackageKit GetUpdates timed out".into()))?
        .map_err(|error| TransactionEvent::FinishedFailed(error.to_string()))?;

        tokio::time::timeout(self.deadline, async {
            let mut candidates = Vec::new();
            let mut error = None;
            loop {
                tokio::select! {
                    biased;
                    signal = errors.next() => match signal {
                        Some(signal) => error = packagekit_error(&signal),
                        None => return Err(TransactionEvent::CompletionUnknown("PackageKit closed its error stream while selecting a trusted update".into())),
                    },
                    signal = packages.next() => match signal {
                        Some(signal) => {
                            if let Some(package_id) = package_id_from_signal(&signal) {
                                match exact_trusted_peppy_package_id(&package_id, expected_version) {
                                    Ok(true) => candidates.push(package_id),
                                    Ok(false) if is_expected_peppy_version(&package_id, expected_version) => {
                                        return Err(TransactionEvent::FinishedFailed(
                                            "PackageKit returned an ambiguous or non-host Peppy update target".into(),
                                        ));
                                    },
                                    Ok(false) => {},
                                    Err(message) => return Err(TransactionEvent::FinishedFailed(message)),
                                }
                            }
                        },
                        None => return Err(TransactionEvent::CompletionUnknown("PackageKit closed its package stream while selecting a trusted update".into())),
                    },
                    signal = finished.next() => match signal {
                        Some(signal) => match terminal_event(&signal, error) {
                            TransactionEvent::FinishedSuccess => {
                                candidates.sort();
                                candidates.dedup();
                                return match candidates.len() {
                                    0 => Ok(None),
                                    1 => Ok(Some(candidates.remove(0))),
                                    _ => Err(TransactionEvent::FinishedFailed(
                                        "PackageKit returned multiple exact trusted Peppy update targets".into(),
                                    )),
                                };
                            },
                            event => return Err(event),
                        },
                        None => return Err(TransactionEvent::CompletionUnknown("PackageKit closed its completion stream while selecting a trusted update".into())),
                    },
                }
            }
        }).await.map_err(|_| TransactionEvent::CompletionUnknown("PackageKit trusted update selection timed out".into()))?
    }

    async fn update_trusted_package(
        &self,
        package_id: String,
        interactive: bool,
    ) -> TransactionEvent {
        use futures_util::StreamExt;

        let (transaction, transaction_path) = match self.transaction().await {
            Ok(transaction) => transaction,
            Err(error) => return TransactionEvent::FinishedFailed(error),
        };
        let mut finished = match transaction.receive_signal("Finished").await {
            Ok(stream) => stream,
            Err(error) => return TransactionEvent::FinishedFailed(error.to_string()),
        };
        let mut errors = match transaction.receive_signal("ErrorCode").await {
            Ok(stream) => stream,
            Err(error) => return TransactionEvent::FinishedFailed(error.to_string()),
        };
        let hint = format!("interactive={interactive}");
        let start = async {
            transaction
                .call::<_, _, ()>("SetHints", &vec![hint])
                .await?;
            transaction
                .call::<_, _, ()>("UpdatePackages", &(Self::ONLY_TRUSTED, vec![package_id]))
                .await
        };
        if let Err(error) = tokio::time::timeout(self.deadline, start)
            .await
            .map_err(|_| "PackageKit UpdatePackages setup timed out".to_string())
            .and_then(|result| result.map_err(|error| error.to_string()))
        {
            return TransactionEvent::FinishedFailed(error);
        }
        let event = match tokio::time::timeout(self.deadline, async {
            let mut error = None;
            loop {
                tokio::select! {
                    biased;
                    signal = errors.next() => match signal {
                        Some(signal) => error = packagekit_error(&signal),
                        None => return TransactionEvent::CompletionUnknown("PackageKit closed its error stream before trusted update completion".into()),
                    },
                    signal = finished.next() => match signal {
                        Some(signal) => return terminal_event(&signal, error),
                        None => return TransactionEvent::CompletionUnknown("PackageKit closed its completion stream before trusted update completion".into()),
                    },
                }
            }
        }).await {
            Ok(event) => event,
            Err(_) => TransactionEvent::CompletionUnknown("PackageKit trusted update deadline elapsed; completion is unknown".into()),
        };
        if matches!(event, TransactionEvent::CompletionUnknown(_)) {
            *self
                .pending_transaction
                .lock()
                .expect("pending transaction mutex poisoned") = Some(transaction_path);
        }
        event
    }

    async fn resolve_installed_peppy_version(&self) -> Option<String> {
        use futures_util::StreamExt;

        let (transaction, _) = self.transaction().await.ok()?;
        let mut packages = transaction.receive_signal("Package").await.ok()?;
        let mut finished = transaction.receive_signal("Finished").await.ok()?;
        let mut errors = transaction.receive_signal("ErrorCode").await.ok()?;
        transaction
            .call::<_, _, ()>(
                "Resolve",
                &(Self::FILTER_INSTALLED, vec![PEPPY_PACKAGE_NAME]),
            )
            .await
            .ok()?;

        let resolved = tokio::time::timeout(self.deadline, async {
            let mut found = None;
            let mut error = None;
            loop {
                tokio::select! {
                    biased;
                    signal = errors.next() => match signal {
                        Some(signal) => error = packagekit_error(&signal),
                        None => return None,
                    },
                    signal = packages.next() => match signal {
                        Some(signal) => {
                            if let Some(version) = installed_peppy_version_from_signal(&signal) {
                                found = Some(version);
                            }
                        },
                        None => return None,
                    },
                    signal = finished.next() => match signal {
                        Some(signal) if matches!(terminal_event(&signal, error), TransactionEvent::FinishedSuccess) => return found,
                        Some(_) | None => return None,
                    },
                }
            }
        }).await;
        resolved.ok().flatten()
    }
}

#[cfg(target_os = "linux")]
#[allow(async_fn_in_trait)]
impl PackageKitBackend for ZbusPackageKit {
    async fn available(&self) -> bool {
        self.connection
            .call_method(
                Some(Self::SERVICE),
                Self::ROOT_PATH,
                Some(Self::ROOT_INTERFACE),
                "GetTransactionList",
                &(),
            )
            .await
            .is_ok()
    }

    async fn daemon_is_idle(&self) -> bool {
        let reply = match self
            .connection
            .call_method(
                Some(Self::SERVICE),
                Self::ROOT_PATH,
                Some(Self::ROOT_INTERFACE),
                "GetTransactionList",
                &(),
            )
            .await
        {
            Ok(reply) => reply,
            Err(_) => return false,
        };
        reply
            .body()
            .deserialize::<Vec<zbus::zvariant::OwnedObjectPath>>()
            .map(|transactions| transactions.is_empty())
            .unwrap_or(false)
    }

    async fn install_files(&self, verified_path: &Path, interactive: bool) -> TransactionEvent {
        self.install_files_transaction(verified_path, interactive)
            .await
    }

    async fn install_trusted_peppy(
        &self,
        expected_version: &str,
        interactive: bool,
    ) -> TrustedRepositoryAttempt {
        match self.exact_trusted_update_id(expected_version).await {
            Ok(Some(package_id)) => TrustedRepositoryAttempt::Outcome(
                self.update_trusted_package(package_id, interactive).await,
            ),
            Ok(None) => TrustedRepositoryAttempt::NoExactTrustedTarget,
            Err(event) => TrustedRepositoryAttempt::Outcome(event),
        }
    }

    async fn wait_for_pending(&self) -> Option<TransactionEvent> {
        self.wait_for_retained_transaction().await
    }

    async fn has_pending_transaction(&self) -> bool {
        self.pending_transaction
            .lock()
            .expect("pending transaction mutex poisoned")
            .is_some()
    }

    async fn installed_peppy_version(&self) -> Option<String> {
        self.resolve_installed_peppy_version().await
    }
}

#[cfg(target_os = "linux")]
fn terminal_event(signal: &zbus::Message, error: Option<TransactionEvent>) -> TransactionEvent {
    let (exit, _runtime): (u32, u32) = match signal.body().deserialize() {
        Ok(body) => body,
        Err(error) => return TransactionEvent::FinishedFailed(error.to_string()),
    };
    terminal_event_from_parts(exit, error)
}

#[cfg(target_os = "linux")]
fn packagekit_error(signal: &zbus::Message) -> Option<TransactionEvent> {
    let (code, details): (u32, String) = signal.body().deserialize().ok()?;
    Some(packagekit_error_from_parts(code, details))
}

fn terminal_event_from_parts(exit: u32, error: Option<TransactionEvent>) -> TransactionEvent {
    if let Some(error) = error {
        return error;
    }
    match exit {
        PACKAGEKIT_EXIT_SUCCESS => TransactionEvent::FinishedSuccess,
        PACKAGEKIT_EXIT_NEED_UNTRUSTED => TransactionEvent::FinishedUntrustedPackage,
        _ => TransactionEvent::FinishedFailed(format!(
            "PackageKit transaction exited with status {exit}"
        )),
    }
}

fn packagekit_error_from_parts(code: u32, details: String) -> TransactionEvent {
    match code {
        PACKAGEKIT_ERROR_TRANSACTION_CANCELLED => TransactionEvent::FinishedCancelled,
        PACKAGEKIT_ERROR_CANNOT_GET_LOCK => TransactionEvent::FinishedLocked,
        PACKAGEKIT_ERROR_NOT_AUTHORIZED => TransactionEvent::FinishedAuthorizationRequired,
        PACKAGEKIT_ERROR_BAD_GPG_SIGNATURE | PACKAGEKIT_ERROR_MISSING_GPG_SIGNATURE => {
            TransactionEvent::FinishedUntrustedPackage
        }
        PACKAGEKIT_ERROR_DEP_RESOLUTION_FAILED => TransactionEvent::FinishedFailed(
            "PackageKit could not resolve Peppy dependencies".into(),
        ),
        _ => TransactionEvent::FinishedFailed(details),
    }
}

#[cfg(target_os = "linux")]
fn installed_peppy_version_from_signal(signal: &zbus::Message) -> Option<String> {
    let package_id = package_id_from_signal(signal)?;
    let mut fields = package_id.split(';');
    match (
        fields.next(),
        fields.next(),
        fields.next(),
        fields.next(),
        fields.next(),
    ) {
        (Some(PEPPY_PACKAGE_NAME), Some(version), Some(_architecture), Some(_repository), None)
            if !version.is_empty() =>
        {
            canonical_package_version(version)
        }
        _ => None,
    }
}

#[cfg(target_os = "linux")]
fn package_id_from_signal(signal: &zbus::Message) -> Option<String> {
    let (_info, package_id, _summary): (u32, String, String) = signal.body().deserialize().ok()?;
    Some(package_id)
}

fn exact_trusted_peppy_package_id(
    package_id: &str,
    expected_version: &str,
) -> Result<bool, String> {
    let mut fields = package_id.split(';');
    let (name, version, architecture, repository, extra) = (
        fields.next(),
        fields.next(),
        fields.next(),
        fields.next(),
        fields.next(),
    );
    if name != Some(PEPPY_PACKAGE_NAME) || extra.is_some() {
        return Ok(false);
    }
    let Some(version) = version.and_then(canonical_package_version) else {
        return Ok(false);
    };
    if version != expected_version {
        return Ok(false);
    }
    let Some(architecture) = architecture else {
        return Ok(false);
    };
    let Some(repository) = repository else {
        return Ok(false);
    };
    if !supported_package_architecture(architecture)
        || repository.is_empty()
        || repository.starts_with('@')
    {
        return Ok(false);
    }
    if repository.chars().any(|character| character.is_control()) {
        return Err("PackageKit returned an invalid trusted repository identifier".into());
    }
    Ok(true)
}

fn is_expected_peppy_version(package_id: &str, expected_version: &str) -> bool {
    let mut fields = package_id.split(';');
    matches!(
        (fields.next(), fields.next(), fields.next(), fields.next(), fields.next()),
        (Some(PEPPY_PACKAGE_NAME), Some(version), Some(_), Some(_), None)
            if canonical_package_version(version).as_deref() == Some(expected_version)
    )
}

fn supported_package_architecture(architecture: &str) -> bool {
    match std::env::consts::ARCH {
        "x86_64" => matches!(architecture, "x86_64" | "amd64"),
        "aarch64" => matches!(architecture, "aarch64" | "arm64"),
        host => architecture == host,
    }
}

fn canonical_package_version(version: &str) -> Option<String> {
    let without_epoch = version
        .rsplit_once(':')
        .map_or(version, |(_, version)| version);
    let normalized_prerelease = without_epoch.replace("~staging.", "-staging.");
    let without_rpm_release = normalized_prerelease
        .rsplit_once('-')
        .filter(|(_, release)| is_rpm_release(release))
        .map_or(normalized_prerelease.as_str(), |(version, _)| version);
    let version = semver::Version::parse(without_rpm_release).ok()?;
    if !version.build.is_empty()
        || (!version.pre.is_empty()
            && !version
                .pre
                .as_str()
                .strip_prefix("staging.")
                .is_some_and(|number| {
                    !number.is_empty() && number.chars().all(|character| character.is_ascii_digit())
                }))
    {
        return None;
    }
    Some(version.to_string())
}

fn is_rpm_release(release: &str) -> bool {
    release.starts_with(|character: char| character.is_ascii_digit())
        && release.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '+')
        })
}

pub struct LinuxPackageInstaller<B> {
    backend: B,
}

impl<B: PackageKitBackend> LinuxPackageInstaller<B> {
    pub fn new(backend: B) -> Self {
        Self { backend }
    }

    pub async fn capability(&self) -> Capability {
        if self.backend.available().await {
            Capability::Supported
        } else {
            Capability::Unsupported
        }
    }

    pub async fn installed_version(&self) -> Option<String> {
        self.backend.installed_peppy_version().await
    }

    pub async fn daemon_is_idle(&self) -> bool {
        self.backend.daemon_is_idle().await
    }

    /// This is an OS metadata observation only. A matching version is never a
    /// substitute for reconciling a retained PackageKit transaction.
    pub async fn has_pending_reconciliation(&self) -> bool {
        self.backend.has_pending_transaction().await
    }

    /// Continue observing the exact PackageKit transaction that previously timed
    /// out. This deliberately cannot start another installation.
    pub async fn reconcile_pending(&self, expected_version: &str) -> Option<InstallOutcome> {
        let expected_version = canonical_package_version(expected_version)?;
        let event = self.backend.wait_for_pending().await?;
        let version = if matches!(event, TransactionEvent::FinishedSuccess) {
            self.backend.installed_peppy_version().await
        } else {
            None
        };
        Some(match interpret_transaction(event, version) {
            InstallOutcome::Success { installed_version }
                if installed_version == expected_version =>
            {
                InstallOutcome::Success { installed_version }
            }
            InstallOutcome::Success { installed_version } => InstallOutcome::Failed {
                message: format!(
                    "PackageKit installed Peppy {installed_version}, expected {expected_version}"
                ),
            },
            outcome => outcome,
        })
    }

    pub async fn install_verified(
        &self,
        path: &Path,
        expected_version: &str,
        interactive: bool,
    ) -> InstallOutcome {
        let Some(expected_version) = canonical_package_version(expected_version) else {
            return InstallOutcome::Failed {
                message: "update target version is not canonical SemVer".into(),
            };
        };
        if !self.backend.available().await {
            return InstallOutcome::Unsupported {
                message: "PackageKit is unavailable on this Linux installation".into(),
            };
        }
        if !is_package_file(path) {
            return InstallOutcome::Unsupported {
                message: "Peppy system updates require a verified .deb or .rpm cache file".into(),
            };
        }

        let event = match self
            .backend
            .install_trusted_peppy(&expected_version, interactive)
            .await
        {
            TrustedRepositoryAttempt::Outcome(event) => event,
            // Only the absence of an exact signed-repository candidate may use
            // the local path. Cancellation, Polkit denial, lock and unknown
            // completion are all terminal and must never trigger a fallback.
            TrustedRepositoryAttempt::Unavailable
            | TrustedRepositoryAttempt::NoExactTrustedTarget => {
                self.backend.install_files(path, interactive).await
            }
        };
        let version = if matches!(&event, TransactionEvent::FinishedSuccess) {
            self.backend.installed_peppy_version().await
        } else {
            None
        };
        match interpret_transaction(event, version) {
            InstallOutcome::Success { installed_version }
                if installed_version == expected_version =>
            {
                InstallOutcome::Success { installed_version }
            }
            InstallOutcome::Success { installed_version } => InstallOutcome::Failed {
                message: format!(
                    "PackageKit installed Peppy {installed_version}, expected {expected_version}"
                ),
            },
            outcome => outcome,
        }
    }
}

fn is_package_file(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|extension| extension.to_str()),
        Some("deb" | "rpm")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancelled_authorization_never_falls_back() {
        assert_eq!(
            interpret_transaction(TransactionEvent::FinishedCancelled, Some("1.2.3".into())),
            InstallOutcome::Cancelled
        );
        assert_eq!(interpret_transaction(TransactionEvent::FinishedUntrustedPackage, None), InstallOutcome::Unsupported {
            message: "PackageKit refused the verified local package; configure the signed Peppy repository or install it with your distribution package manager".into(),
        });
    }

    #[test]
    fn success_requires_the_trusted_installed_package() {
        assert!(matches!(
            interpret_transaction(TransactionEvent::FinishedSuccess, None),
            InstallOutcome::Failed { .. }
        ));
        assert_eq!(
            interpret_transaction(TransactionEvent::FinishedSuccess, Some("1.2.3".into())),
            InstallOutcome::Success {
                installed_version: "1.2.3".into()
            }
        );
    }

    #[test]
    fn only_distribution_package_extensions_are_accepted() {
        assert!(is_package_file(Path::new("/private/cache/peppy.deb")));
        assert!(is_package_file(Path::new("/private/cache/peppy.rpm")));
        assert!(!is_package_file(Path::new("/private/cache/peppy.AppImage")));
    }

    #[test]
    fn packagekit_terminal_and_error_states_are_typed() {
        assert_eq!(
            packagekit_error_from_parts(PACKAGEKIT_ERROR_TRANSACTION_CANCELLED, "cancelled".into()),
            TransactionEvent::FinishedCancelled
        );
        assert_eq!(
            packagekit_error_from_parts(PACKAGEKIT_ERROR_NOT_AUTHORIZED, "denied".into()),
            TransactionEvent::FinishedAuthorizationRequired
        );
        assert_eq!(
            terminal_event_from_parts(PACKAGEKIT_EXIT_NEED_UNTRUSTED, None),
            TransactionEvent::FinishedUntrustedPackage
        );
        assert_eq!(
            packagekit_error_from_parts(PACKAGEKIT_ERROR_CANNOT_GET_LOCK, "busy".into()),
            TransactionEvent::FinishedLocked
        );
        assert_eq!(
            packagekit_error_from_parts(
                PACKAGEKIT_ERROR_DEP_RESOLUTION_FAILED,
                "dependency resolution failed".into()
            ),
            TransactionEvent::FinishedFailed(
                "PackageKit could not resolve Peppy dependencies".into()
            )
        );
    }

    #[test]
    fn installed_version_must_be_canonical_semver() {
        assert_eq!(
            canonical_package_version("1.2.3-staging.4"),
            Some("1.2.3-staging.4".into())
        );
        assert_eq!(canonical_package_version("1:2.3.4-1"), Some("2.3.4".into()));
        assert_eq!(
            canonical_package_version("1.2.3-1.fc42"),
            Some("1.2.3".into())
        );
        assert_eq!(canonical_package_version("1.2.3-alpha"), None);
        assert_eq!(canonical_package_version("1.2.3+build.7"), None);
    }

    #[test]
    fn trusted_repository_selection_requires_fixed_target_version_architecture_and_repository() {
        let architecture = match std::env::consts::ARCH {
            "x86_64" => "amd64",
            "aarch64" => "arm64",
            host => host,
        };
        assert_eq!(
            exact_trusted_peppy_package_id(
                &format!("peppy;1.2.3;{architecture};peppy-stable"),
                "1.2.3"
            ),
            Ok(true)
        );
        assert_eq!(
            exact_trusted_peppy_package_id("other;1.2.3;amd64;peppy-stable", "1.2.3"),
            Ok(false)
        );
        assert_eq!(
            exact_trusted_peppy_package_id("peppy;1.2.4;amd64;peppy-stable", "1.2.3"),
            Ok(false)
        );
        let other_architecture = if architecture == "amd64" {
            "arm64"
        } else {
            "amd64"
        };
        assert_eq!(
            exact_trusted_peppy_package_id(
                &format!("peppy;1.2.3;{other_architecture};peppy-stable"),
                "1.2.3"
            ),
            Ok(false)
        );
        assert_eq!(
            exact_trusted_peppy_package_id("peppy;1.2.3;amd64;@System", "1.2.3"),
            Ok(false)
        );
    }

    struct PendingBackend;

    impl PackageKitBackend for PendingBackend {
        async fn available(&self) -> bool {
            true
        }
        async fn install_files(&self, _: &Path, _: bool) -> TransactionEvent {
            TransactionEvent::CompletionUnknown("deadline".into())
        }
        async fn installed_peppy_version(&self) -> Option<String> {
            None
        }
        async fn wait_for_pending(&self) -> Option<TransactionEvent> {
            Some(TransactionEvent::FinishedLocked)
        }
    }

    #[tokio::test]
    async fn pending_reconciliation_preserves_a_typed_terminal_outcome() {
        let installer = LinuxPackageInstaller::new(PendingBackend);
        assert_eq!(
            installer.reconcile_pending("1.2.3").await,
            Some(InstallOutcome::Locked)
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn install_files_body_has_the_packagekit_u64_and_string_array_signature() {
        use zbus::zvariant::{Endian, serialized::Context};

        let body = (
            ZbusPackageKit::ONLY_TRUSTED,
            vec!["/cache/peppy.deb".to_owned()],
        );
        let encoded = zbus::zvariant::to_bytes(Context::new_dbus(Endian::Little, 0), &body)
            .expect("InstallFiles arguments serialize as D-Bus");
        let (decoded, consumed): ((u64, Vec<String>), usize) = encoded
            .deserialize()
            .expect("InstallFiles arguments deserialize as D-Bus");
        assert_eq!(consumed, encoded.bytes().len());
        assert_eq!(decoded, body);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn update_packages_body_keeps_the_trusted_flag_and_exact_selected_id() {
        use zbus::zvariant::{Endian, serialized::Context};

        let body = (
            ZbusPackageKit::ONLY_TRUSTED,
            vec!["peppy;1.2.3;amd64;peppy-stable".to_owned()],
        );
        let encoded = zbus::zvariant::to_bytes(Context::new_dbus(Endian::Little, 0), &body)
            .expect("UpdatePackages arguments serialize as D-Bus");
        let (decoded, _): ((u64, Vec<String>), usize) = encoded
            .deserialize()
            .expect("UpdatePackages arguments deserialize as D-Bus");
        assert_eq!(decoded, body);
    }
}
