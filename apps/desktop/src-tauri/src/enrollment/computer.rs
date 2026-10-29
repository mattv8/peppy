//! Native transport and lifecycle for a computer claimant. The renderer receives only the QR
//! payload that it renders and derived progress; bearer tokens and transfer keys stay here.
use super::*;
use cap_fs_ext::{DirExt, MetadataExt};
#[cfg(windows)]
use cap_std::fs::MetadataExt as WindowsMetadataExt;
use cap_std::fs::{Dir, Metadata};
use peppy_hosted_api::enrollment::EnrollmentState;
use std::io::{Read, Write};

const COMPUTER_STATUS_PATH: &str = "/v1/enrollment/computers";

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ComputerClaimantView {
    pub stage: &'static str,
    pub invitation_id: String,
    pub qr_payload: Option<String>,
    pub awaiting_owner: bool,
    pub state: &'static str,
    pub expires_in_seconds: u32,
    pub verification_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_code: Option<&'static str>,
}

const COMPUTER_INSTALL_ROLLBACK_FILE: &str = "computer-enrollment-rollback.json";

#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct ComputerInstallRollback {
    version: u8,
    attempt_id: uuid::Uuid,
    target: crate::credentials::Binding,
    previous_config: crate::credentials::HostConfig,
    data_dir: std::path::PathBuf,
    database_path: std::path::PathBuf,
    credential_account: String,
    db_key_account: String,
    vault_secret_account: String,
    key_cache_accounts: Vec<String>,
    credential_sha256: [u8; 32],
    db_key_sha256: [u8; 32],
    key_epoch: Option<u32>,
    directory_identity: Option<DirectoryIdentity>,
}

#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct DirectoryIdentity {
    volume: u64,
    file: u64,
    created_at: Option<(u64, u32)>,
}

impl DirectoryIdentity {
    fn of(metadata: &Metadata) -> BridgeResult<Self> {
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(discard_ownership_error());
        }
        Ok(Self {
            volume: metadata.dev(),
            file: metadata.ino(),
            created_at: metadata
                .created()
                .ok()
                .and_then(|time| time.into_std().duration_since(std::time::UNIX_EPOCH).ok())
                .map(|time| (time.as_secs(), time.subsec_nanos())),
        })
    }
}

fn canonical_binding(binding: &crate::credentials::Binding) -> bool {
    origin::validate_origin(&binding.origin).is_ok_and(|origin| origin == binding.origin)
        && [&binding.vault_id, &binding.device_id]
            .iter()
            .all(|id| uuid::Uuid::parse_str(id).is_ok_and(|uuid| uuid.to_string() == **id))
}

/// Each component is opened relative to a trusted directory capability, never by an absolute
/// marker path. Reject links (including Windows reparse points) at the open itself.
fn open_child_directory(parent: &Dir, name: &str) -> BridgeResult<Option<Dir>> {
    let metadata = match parent.symlink_metadata(name) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(discard_ownership_error()),
    };
    #[cfg(windows)]
    if metadata.file_attributes() & 0x400 != 0 {
        return Err(discard_ownership_error());
    }
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(discard_ownership_error());
    }
    let dir = parent
        .open_dir_nofollow(name)
        .map_err(|_| discard_ownership_error())?;
    Ok(Some(dir))
}

fn target_parent(
    state: &AppState,
    marker: &ComputerInstallRollback,
    create: bool,
) -> BridgeResult<Option<Dir>> {
    let root = Dir::open_ambient_dir(&state.root, cap_std::ambient_authority())
        .map_err(|_| BridgeError::host_state())?;
    let mut parent = root;
    for name in ["vaults", marker.target.vault_id.as_str()] {
        if create
            && parent
                .symlink_metadata(name)
                .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
        {
            create_private_directory(&parent, name)?;
        }
        let Some(child) = open_child_directory(&parent, name)? else {
            return Ok(None);
        };
        parent = child;
    }
    Ok(Some(parent))
}

const COMPUTER_DIRECTORY_STAMP: &str = ".computer-enrollment-attempt";

fn sync_directory(dir: &Dir) -> BridgeResult<()> {
    #[cfg(unix)]
    dir.try_clone()
        .and_then(|dir| dir.into_std_file().sync_all())
        .map_err(|_| BridgeError::host_state())?;
    #[cfg(not(unix))]
    let _ = dir;
    Ok(())
}

fn create_private_directory(parent: &Dir, name: &str) -> BridgeResult<()> {
    #[cfg(unix)]
    let mut builder = cap_std::fs::DirBuilder::new();
    #[cfg(not(unix))]
    let builder = cap_std::fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use cap_std::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    parent
        .create_dir_with(name, &builder)
        .map_err(|_| BridgeError::host_state())?;
    sync_directory(parent)
}

fn reserve_computer_directory(
    state: &AppState,
    marker: &mut ComputerInstallRollback,
) -> BridgeResult<()> {
    let parent = target_parent(state, marker, true)?.ok_or_else(discard_ownership_error)?;
    if parent.symlink_metadata(&marker.target.device_id).is_ok() {
        return Err(discard_ownership_error());
    }
    create_private_directory(&parent, &marker.target.device_id)?;
    let dir = open_child_directory(&parent, &marker.target.device_id)?
        .ok_or_else(discard_ownership_error)?;
    let mut options = cap_std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    let mut stamp = dir
        .open_with(COMPUTER_DIRECTORY_STAMP, &options)
        .map_err(|_| BridgeError::host_state())?;
    stamp
        .write_all(marker.attempt_id.to_string().as_bytes())
        .map_err(|_| BridgeError::host_state())?;
    stamp.sync_all().map_err(|_| BridgeError::host_state())?;
    marker.directory_identity = Some(DirectoryIdentity::of(
        &dir.dir_metadata().map_err(|_| BridgeError::host_state())?,
    )?);
    sync_directory(&dir)?;
    sync_directory(&parent)?;
    Ok(())
}

fn owned_computer_directory(
    state: &AppState,
    marker: &ComputerInstallRollback,
) -> BridgeResult<Option<Dir>> {
    let Some(parent) = target_parent(state, marker, false)? else {
        return Ok(None);
    };
    let Some(dir) = open_child_directory(&parent, &marker.target.device_id)? else {
        return Ok(None);
    };
    let actual =
        DirectoryIdentity::of(&dir.dir_metadata().map_err(|_| discard_ownership_error())?)?;
    if marker.directory_identity.as_ref() != Some(&actual) {
        return Err(discard_ownership_error());
    }
    let metadata = match dir.symlink_metadata(COMPUTER_DIRECTORY_STAMP) {
        Ok(metadata) => metadata,
        // Deletion removes the stamp last. A crash between stamp removal and rmdir may leave
        // only the original empty directory; never recursively delete an unstamped directory.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if dir
                .entries()
                .map_err(|_| discard_ownership_error())?
                .next()
                .is_none()
            {
                return Ok(Some(dir));
            }
            return Err(discard_ownership_error());
        }
        Err(_) => return Err(discard_ownership_error()),
    };
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(discard_ownership_error());
    }
    let mut stamp = String::new();
    dir.open(COMPUTER_DIRECTORY_STAMP)
        .map_err(|_| discard_ownership_error())?
        .take(37)
        .read_to_string(&mut stamp)
        .map_err(|_| discard_ownership_error())?;
    if stamp != marker.attempt_id.to_string() {
        return Err(discard_ownership_error());
    }
    Ok(Some(dir))
}

fn remove_owned_computer_directory(dir: Dir) -> BridgeResult<()> {
    for entry in dir.entries().map_err(|_| BridgeError::host_state())? {
        let entry = entry.map_err(|_| BridgeError::host_state())?;
        let name = entry.file_name();
        if name == COMPUTER_DIRECTORY_STAMP {
            continue;
        }
        if entry
            .file_type()
            .map_err(|_| BridgeError::host_state())?
            .is_dir()
        {
            dir.remove_dir_all(&name)
                .map_err(|_| BridgeError::host_state())?;
        } else {
            dir.remove_file(&name)
                .map_err(|_| BridgeError::host_state())?;
        }
    }
    match dir.remove_file(COMPUTER_DIRECTORY_STAMP) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return Err(BridgeError::host_state()),
    }
    dir.remove_open_dir().map_err(|_| BridgeError::host_state())
}

fn discard_ownership_error() -> BridgeError {
    BridgeError::new(
        "computer-discard-ownership-unproven",
        "Peppy cannot prove that the partially installed identity belongs only to this connection attempt.",
    )
}

impl ComputerInstallRollback {
    #[cfg(test)]
    fn new(
        state: &AppState,
        target: crate::credentials::Binding,
        previous_config: crate::credentials::HostConfig,
        credential: &[u8],
    ) -> BridgeResult<Self> {
        Self::new_with_key_epoch(state, target, previous_config, credential, None, &[7; 32])
    }

    fn new_with_key_epoch(
        state: &AppState,
        target: crate::credentials::Binding,
        previous_config: crate::credentials::HostConfig,
        credential: &[u8],
        key_epoch: Option<u32>,
        database_key: &[u8],
    ) -> BridgeResult<Self> {
        let data_dir = target.data_dir(&state.root);
        let key_cache_accounts = key_epoch
            .map(|epoch| vec![target.key_cache_account(epoch)])
            .unwrap_or_default();
        if !canonical_binding(&target)
            || database_key.len() != 32
            || state.config()? != previous_config
            || previous_config.known(&target).is_some()
            || previous_config.active_binding() == Some(&target)
            || std::fs::symlink_metadata(&data_dir).is_ok()
        {
            return Err(discard_ownership_error());
        }
        for account in [
            target.credential_account(),
            target.db_key_account(),
            target.vault_secret_account(),
        ]
        .iter()
        .chain(key_cache_accounts.iter())
        {
            if state
                .store
                .get(account)?
                .is_some_and(|value| !value.is_empty())
            {
                return Err(discard_ownership_error());
            }
        }
        let credential_sha256 = Sha256::digest(credential).into();
        Ok(Self {
            version: 2,
            attempt_id: uuid::Uuid::new_v4(),
            database_path: target.database_path(&state.root),
            data_dir,
            credential_account: target.credential_account(),
            db_key_account: target.db_key_account(),
            vault_secret_account: target.vault_secret_account(),
            key_cache_accounts,
            target,
            previous_config,
            credential_sha256,
            db_key_sha256: Sha256::digest(database_key).into(),
            key_epoch,
            directory_identity: None,
        })
    }

    fn validate_shape(&self, state: &AppState) -> BridgeResult<()> {
        let valid = self.version == 2
            && !self.attempt_id.is_nil()
            && canonical_binding(&self.target)
            && self.previous_config.origin.as_ref().is_none_or(|value| {
                origin::validate_origin(value).is_ok_and(|canonical| canonical == *value)
            })
            && self
                .previous_config
                .active
                .as_ref()
                .is_none_or(canonical_binding)
            && self
                .previous_config
                .bindings
                .iter()
                .all(|known| canonical_binding(&known.binding))
            && self.previous_config.known(&self.target).is_none()
            && self
                .previous_config
                .active
                .as_ref()
                .is_none_or(|binding| !binding.same_identity(&self.target))
            && self.data_dir == self.target.data_dir(&state.root)
            && self.database_path == self.target.database_path(&state.root)
            && self.credential_account == self.target.credential_account()
            && self.db_key_account == self.target.db_key_account()
            && self.vault_secret_account == self.target.vault_secret_account()
            && self.key_cache_accounts
                == self
                    .key_epoch
                    .map(|epoch| vec![self.target.key_cache_account(epoch)])
                    .unwrap_or_default();
        if valid {
            Ok(())
        } else {
            Err(discard_ownership_error())
        }
    }

    fn activated_config(&self) -> crate::credentials::HostConfig {
        let mut config = self.previous_config.clone();
        config.select_origin(&self.target.origin);
        config.remember(&self.target);
        config.active = Some(self.target.clone());
        config
    }

    fn selected_config(&self) -> crate::credentials::HostConfig {
        let mut config = self.previous_config.clone();
        config.select_origin(&self.target.origin);
        config
    }
}

fn computer_install_rollback_path(state: &AppState) -> std::path::PathBuf {
    state.root.join(COMPUTER_INSTALL_ROLLBACK_FILE)
}

fn persist_computer_install_rollback(
    state: &AppState,
    marker: &ComputerInstallRollback,
) -> BridgeResult<()> {
    marker.validate_shape(state)?;
    if let Some(existing) = load_computer_install_rollback(state)? {
        if existing.attempt_id != marker.attempt_id {
            return Err(discard_ownership_error());
        }
    }
    let bytes = serde_json::to_vec(marker).map_err(|_| BridgeError::host_state())?;
    crate::fsutil::write_private_atomic(&computer_install_rollback_path(state), &bytes)
        .map_err(|_| BridgeError::host_state())
}

fn load_computer_install_rollback(
    state: &AppState,
) -> BridgeResult<Option<ComputerInstallRollback>> {
    match std::fs::symlink_metadata(computer_install_rollback_path(state)) {
        Ok(metadata) if !metadata.is_file() || metadata.file_type().is_symlink() => {
            return Err(discard_ownership_error())
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(BridgeError::host_state()),
    }
    let file = match std::fs::File::open(computer_install_rollback_path(state)) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(BridgeError::host_state()),
    };
    let mut bytes = Vec::new();
    file.take(64 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| BridgeError::host_state())?;
    if bytes.len() > 64 * 1024 {
        return Err(discard_ownership_error());
    }
    let marker: ComputerInstallRollback =
        serde_json::from_slice(&bytes).map_err(|_| discard_ownership_error())?;
    marker.validate_shape(state)?;
    Ok(Some(marker))
}

fn clear_computer_install_rollback(
    state: &AppState,
    expected: &ComputerInstallRollback,
) -> BridgeResult<()> {
    require_current_marker(state, expected)?;
    match std::fs::remove_file(computer_install_rollback_path(state)) {
        Ok(()) => {
            let root = Dir::open_ambient_dir(&state.root, cap_std::ambient_authority())
                .map_err(|_| BridgeError::host_state())?;
            sync_directory(&root)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(BridgeError::host_state()),
    }
}

fn config_matches_rollback_state(
    current: &crate::credentials::HostConfig,
    marker: &ComputerInstallRollback,
) -> bool {
    current == &marker.previous_config
        || current == &marker.selected_config()
        || current == &marker.activated_config()
}

fn validate_computer_install_artifacts(
    state: &AppState,
    marker: &ComputerInstallRollback,
) -> BridgeResult<()> {
    for (account, expected) in [
        (&marker.credential_account, Some(marker.credential_sha256)),
        (&marker.db_key_account, Some(marker.db_key_sha256)),
        // This flow installs a VerifiedHandoff, never a phrase or purpose-key cache.
        // A nonempty entry in either account is unowned and must not be removed.
        (&marker.vault_secret_account, None),
    ]
    .into_iter()
    .chain(
        marker
            .key_cache_accounts
            .iter()
            .map(|account| (account, None)),
    ) {
        if let Some(value) = state.store.get(account)?.filter(|value| !value.is_empty()) {
            let actual: [u8; 32] = Sha256::digest(value.as_slice()).into();
            if Some(actual) != expected {
                return Err(discard_ownership_error());
            }
        }
    }
    Ok(())
}

fn finish_computer_install_rollback(
    state: &AppState,
    marker: &ComputerInstallRollback,
) -> BridgeResult<()> {
    let _config_guard = state
        .config_lock
        .lock()
        .map_err(|_| BridgeError::host_state())?;
    let current = crate::credentials::load_config(&state.config_path)?;
    if !config_matches_rollback_state(&current, marker) {
        return Err(discard_ownership_error());
    }
    require_current_marker(state, marker)?;
    validate_computer_install_artifacts(state, marker)?;
    if let Some(dir) = owned_computer_directory(state, marker)? {
        remove_owned_computer_directory(dir)?;
    }
    for account in
        std::iter::once(&marker.credential_account).chain(std::iter::once(&marker.db_key_account))
    {
        state.store.delete(account)?;
    }

    crate::credentials::save_config(&state.config_path, &marker.previous_config)?;
    clear_computer_install_rollback(state, marker)
}

fn require_current_marker(
    state: &AppState,
    expected: &ComputerInstallRollback,
) -> BridgeResult<()> {
    if load_computer_install_rollback(state)?.as_ref() != Some(expected) {
        return Err(discard_ownership_error());
    }
    Ok(())
}

pub(crate) fn require_no_pending_computer_install(state: &AppState) -> BridgeResult<()> {
    if std::fs::symlink_metadata(computer_install_rollback_path(state)).is_ok() {
        return Err(discard_ownership_error());
    }
    Ok(())
}

pub(crate) fn check_computer_install_access(
    state: &AppState,
    binding: &crate::credentials::Binding,
    attempt: Option<uuid::Uuid>,
) -> BridgeResult<()> {
    if let Some(marker) = load_computer_install_rollback(state)? {
        if attempt == Some(marker.attempt_id) && binding == &marker.target {
            require_current_marker(state, &marker)?;
            validate_computer_install_artifacts(state, &marker)?;
            owned_computer_directory(state, &marker)?.ok_or_else(discard_ownership_error)?;
        } else if binding.same_identity(&marker.target) || attempt.is_some() {
            return Err(discard_ownership_error());
        }
    } else if attempt.is_some() {
        return Err(discard_ownership_error());
    }
    Ok(())
}

pub(crate) fn recover_computer_install_rollback(state: &AppState) -> BridgeResult<()> {
    let Some(marker) = load_computer_install_rollback(state)? else {
        return Ok(());
    };
    marker.validate_shape(state)?;
    require_current_marker(state, &marker)?;
    owned_computer_directory(state, &marker)?;
    validate_computer_install_artifacts(state, &marker)?;
    finish_computer_install_rollback(state, &marker)
}

async fn rollback_computer_install(
    state: &AppState,
    marker: &ComputerInstallRollback,
) -> BridgeResult<()> {
    marker.validate_shape(state)?;
    require_current_marker(state, marker)?;
    owned_computer_directory(state, marker)?;
    let config_before = state.config()?;
    if !config_matches_rollback_state(&config_before, marker) {
        return Err(discard_ownership_error());
    }
    validate_computer_install_artifacts(state, marker)?;
    let _session_gate = state.session_open_lock.lock().await;
    let current_binding = state
        .session
        .lock()
        .await
        .as_ref()
        .map(|session| session.binding.clone());
    let prior_active = marker.previous_config.active_binding();
    let prior_session_is_safe = (config_before == marker.previous_config
        || config_before == marker.selected_config())
        && current_binding
            .as_ref()
            .is_some_and(|binding| Some(binding) == prior_active);
    if current_binding
        .as_ref()
        .is_some_and(|binding| binding != &marker.target)
        && !prior_session_is_safe
    {
        return Err(discard_ownership_error());
    }

    let must_retire_target = current_binding.as_ref() == Some(&marker.target)
        || (!prior_session_is_safe && config_before == marker.activated_config());
    if must_retire_target && !state.retire_session_preserving_heads(false).await {
        return Err(BridgeError::new(
            "session-draining",
            "Peppy is still finishing the computer connection session.",
        ));
    }

    finish_computer_install_rollback(state, marker)
}

pub struct ClaimantState {
    expected_binding: Option<crate::credentials::Binding>,
    qr_payload: String,
    view: ComputerClaimantView,
    confirm_request: Option<Zeroizing<String>>,
    prepared_identity: Option<NativeEnrollmentIdentity>,
    install_baseline: crate::credentials::HostConfig,
    install_rollback: Option<ComputerInstallRollback>,
    database_key: Option<Zeroizing<Vec<u8>>>,
    installed: bool,
}

impl ClaimantState {
    pub(super) fn resume_view(&self) -> ComputerClaimantView {
        let mut view = self.view.clone();
        if self.confirm_request.is_some() || self.prepared_identity.is_some() {
            view.stage = "finishing";
            view.awaiting_owner = false;
        }
        if claimant_must_finish(self) {
            view.qr_payload = None;
        }
        view
    }
}

pub(super) fn retained_discovery_view(
    controller: &Controller,
    selected: &str,
) -> BridgeResult<Option<EnrollmentView>> {
    if controller.computer_claimant.is_none() {
        return Ok(None);
    }
    if controller.origin.as_deref() != Some(selected) {
        return Err(enrollment_error());
    }
    Ok(Some(controller.view()))
}

#[derive(Deserialize)]
struct StatusRequest {
    path: String,
    body: serde_json::Value,
}

fn state_name(state: &EnrollmentState) -> &'static str {
    match state {
        EnrollmentState::Waiting => "waiting",
        EnrollmentState::Claimed => "claimed",
        EnrollmentState::Approved => "approved",
        EnrollmentState::Confirmed => "confirmed",
        EnrollmentState::Consumed => "consumed",
        EnrollmentState::Denied => "denied",
        EnrollmentState::Expired => "expired",
    }
}

fn claimant_view(
    value: peppy_hosted_client::newenrollment::ComputerClaimantView,
    qr_payload: Option<String>,
    has_local_material: bool,
) -> ComputerClaimantView {
    let stage = match value.state {
        EnrollmentState::Approved => "compare_codes",
        EnrollmentState::Confirmed | EnrollmentState::Consumed if has_local_material => "finishing",
        EnrollmentState::Confirmed | EnrollmentState::Consumed => "error",
        EnrollmentState::Denied => "denied",
        EnrollmentState::Expired => "expired",
        _ if value.awaiting_owner => "waiting_owner",
        _ => "waiting_owner",
    };
    ComputerClaimantView {
        stage,
        invitation_id: value.invitation_id.to_string(),
        qr_payload,
        awaiting_owner: value.awaiting_owner,
        state: state_name(&value.state),
        expires_in_seconds: value.expires_in_seconds,
        verification_code: value.sas,
        error_code: (stage == "error").then_some("computer_remote_consumed"),
    }
}

fn saved_identity_blocks_claimant(state: &AppState, origin: &str) -> BridgeResult<bool> {
    saved_identity_for_origin(state, origin)
}

#[cfg(test)]
fn config_has_saved_identity(config: &crate::credentials::HostConfig, origin: &str) -> bool {
    config
        .bindings
        .iter()
        .any(|known| known.binding.origin == origin)
}

fn claimant_cancellation_allowed(claimant: &ClaimantState) -> bool {
    claimant.view.stage == "error"
        || (claimant.confirm_request.is_none() && claimant.view.state != "approved")
}

fn cancel_computer_claimant(state: &AppState) -> BridgeResult<()> {
    let mut controller = state
        .enrollment
        .lock()
        .map_err(|_| BridgeError::host_state())?;
    let Some(claimant) = controller.computer_claimant.as_ref() else {
        return Ok(());
    };
    if !claimant_cancellation_allowed(claimant) || claimant.prepared_identity.is_some() {
        return Err(BridgeError::new(
            "enrollment-state",
            "An approved computer connection cannot be cancelled or replaced.",
        ));
    }
    controller.computer_claimant = None;
    controller.client = None;
    Ok(())
}

fn terminal_confirm_error_code(
    status: reqwest::StatusCode,
    code: Option<&str>,
) -> Option<&'static str> {
    match status.as_u16() {
        403 if code == Some("foreign_vault") => Some("foreign_vault"),
        401 | 409 => Some("computer_remote_consumed"),
        403 | 404 | 410 => Some("computer_verification_failed"),
        _ => None,
    }
}

fn uninstalled_prepared_target_is_discard_safe(
    active_target_matches: bool,
    database_exists: bool,
) -> bool {
    !active_target_matches && !database_exists
}

fn claimant_discard_safe(state: &AppState, claimant: &ClaimantState) -> BridgeResult<bool> {
    let Some(identity) = claimant.prepared_identity.as_ref() else {
        return Ok(true);
    };
    let metadata = identity.metadata();
    let target = crate::credentials::Binding {
        origin: identity.origin().to_owned(),
        vault_id: metadata.vault_id.to_string(),
        device_id: metadata.device_id.to_string(),
    };
    let active = state.config()?.active_binding().cloned();
    Ok(uninstalled_prepared_target_is_discard_safe(
        active.as_ref() == Some(&target),
        target.database_path(&state.root).exists(),
    ))
}

fn claimant_must_finish(claimant: &ClaimantState) -> bool {
    claimant.view.state == "approved"
        || claimant.confirm_request.is_some()
        || claimant.prepared_identity.is_some()
}

async fn install_prepared_identity(
    app: &AppHandle,
    state: &AppState,
    expected_binding: Option<crate::credentials::Binding>,
) -> BridgeResult<()> {
    if state
        .enrollment
        .lock()
        .map_err(|_| BridgeError::host_state())?
        .computer_claimant
        .as_ref()
        .is_some_and(|claimant| claimant.installed)
    {
        return Ok(());
    }
    prepare_computer_install(state)?;
    let attempt = computer_attempt(state)?;
    let mut identity = state
        .enrollment
        .lock()
        .map_err(|_| BridgeError::host_state())?
        .computer_claimant
        .as_mut()
        .ok_or_else(enrollment_error)?
        .prepared_identity
        .take()
        .ok_or_else(enrollment_error)?;
    let result = install_identity_locked(
        app,
        state,
        &mut identity,
        None,
        expected_binding,
        true,
        Some(attempt),
    )
    .await;
    let mut controller = state
        .enrollment
        .lock()
        .map_err(|_| BridgeError::host_state())?;
    let claimant = controller
        .computer_claimant
        .as_mut()
        .ok_or_else(enrollment_error)?;
    claimant.prepared_identity = Some(identity);
    claimant.installed = result.is_ok();
    result
}

fn computer_attempt(state: &AppState) -> BridgeResult<uuid::Uuid> {
    state
        .enrollment
        .lock()
        .map_err(|_| BridgeError::host_state())?
        .computer_claimant
        .as_ref()
        .and_then(|claimant| claimant.install_rollback.as_ref())
        .map(|marker| marker.attempt_id)
        .ok_or_else(enrollment_error)
}

/// Called only under import_lock. Retained verified material survives every fallible preflight,
/// reservation, secure-store and marker write. Installation cannot start before durable ownership.
fn prepare_computer_install(state: &AppState) -> BridgeResult<()> {
    let mut controller = state
        .enrollment
        .lock()
        .map_err(|_| BridgeError::host_state())?;
    let claimant = controller
        .computer_claimant
        .as_mut()
        .ok_or_else(enrollment_error)?;
    let identity = claimant
        .prepared_identity
        .as_ref()
        .ok_or_else(enrollment_error)?;
    if claimant.database_key.is_none() {
        let mut key = Zeroizing::new(vec![0; 32]);
        getrandom::fill(&mut key).map_err(|_| BridgeError::host_state())?;
        claimant.database_key = Some(key);
    }
    let key = claimant
        .database_key
        .as_ref()
        .ok_or_else(enrollment_error)?;
    if claimant.install_rollback.is_none() {
        require_no_pending_computer_install(state)?;
        let portable = native_identity_credential(identity)?;
        let target = crate::credentials::Binding {
            origin: identity.origin().to_owned(),
            vault_id: identity.metadata().vault_id.to_string(),
            device_id: identity.metadata().device_id.to_string(),
        };
        claimant.install_rollback = Some(ComputerInstallRollback::new_with_key_epoch(
            state,
            target,
            claimant.install_baseline.clone(),
            &portable,
            Some(identity.metadata().profile.key_epoch),
            key,
        )?);
    }
    let marker = claimant
        .install_rollback
        .as_mut()
        .ok_or_else(enrollment_error)?;
    persist_computer_install_rollback(state, marker)?;
    if marker.directory_identity.is_none() {
        reserve_computer_directory(state, marker)?;
        persist_computer_install_rollback(state, marker)?;
    }
    validate_computer_install_artifacts(state, marker)?;
    owned_computer_directory(state, marker)?.ok_or_else(discard_ownership_error)?;
    if state
        .store
        .get(&marker.db_key_account)?
        .is_none_or(|value| value.is_empty())
    {
        if marker.database_path.exists() {
            return Err(discard_ownership_error());
        }
        state.store.set(&marker.db_key_account, key)?;
    }
    Ok(())
}

fn retain_computer_admission(state: &AppState, complete: String) -> BridgeResult<()> {
    let mut controller = state
        .enrollment
        .lock()
        .map_err(|_| BridgeError::host_state())?;
    let Controller {
        client,
        computer_claimant,
        ..
    } = &mut *controller;
    let claimant = computer_claimant.as_mut().ok_or_else(enrollment_error)?;
    let identity = client
        .as_mut()
        .ok_or_else(enrollment_error)?
        .finish_computer_admission(complete)
        .map_err(|_| enrollment_error())?;
    // No fallible work may intervene between consuming the handoff and retaining this identity.
    claimant.prepared_identity = Some(identity);
    claimant.view.stage = "finishing";
    claimant.view.qr_payload = None;
    Ok(())
}

fn commit_computer_install(
    state: &AppState,
    has_gateway: Option<bool>,
) -> BridgeResult<ComputerClaimantView> {
    let mut controller = state
        .enrollment
        .lock()
        .map_err(|_| BridgeError::host_state())?;
    let claimant = controller
        .computer_claimant
        .as_ref()
        .ok_or_else(enrollment_error)?;
    let marker = claimant
        .install_rollback
        .as_ref()
        .ok_or_else(enrollment_error)?;
    if !claimant.installed || state.config()?.active_binding() != Some(&marker.target) {
        return Err(enrollment_error());
    }
    check_computer_install_access(state, &marker.target, Some(marker.attempt_id))?;
    let invitation_id = claimant.view.invitation_id.clone();
    clear_computer_install_rollback(state, marker)?;
    controller.computer_claimant = None;
    controller.has_gateway = has_gateway;
    set_identity_ready(&mut controller, Some(EnrollmentRole::Device));
    Ok(connected_view(invitation_id))
}

async fn computer_gateway_presence(state: &AppState) -> BridgeResult<Option<bool>> {
    let operation = crate::admit_update_operation(state)?;
    let session = state
        .session_admitted_for_computer(&operation, Some(computer_attempt(state)?))
        .await?
        .ok_or_else(BridgeError::no_session)?;
    Ok(known_gateway_presence(session.gateways()))
}

fn connected_view(invitation_id: String) -> ComputerClaimantView {
    ComputerClaimantView {
        stage: "connected",
        invitation_id,
        qr_payload: None,
        awaiting_owner: false,
        state: "consumed",
        expires_in_seconds: 0,
        verification_code: None,
        error_code: None,
    }
}

async fn fetch_status(
    state: &AppState,
    hosted: &crate::hosted::HostedState,
) -> BridgeResult<Option<String>> {
    let (origin, request) = {
        let controller = state
            .enrollment
            .lock()
            .map_err(|_| BridgeError::host_state())?;
        let origin = controller.origin()?.to_owned();
        let request = controller
            .client
            .as_ref()
            .ok_or_else(enrollment_error)?
            .computer_status_request()
            .map_err(|_| enrollment_error())?;
        (origin, request)
    };
    let request: StatusRequest = serde_json::from_str(&request).map_err(|_| enrollment_error())?;
    if !request.path.starts_with(COMPUTER_STATUS_PATH) {
        return Err(enrollment_error());
    }
    let account = hosted_account_header(state, hosted).await?;
    post_computer_status(&origin, request, account.as_deref().map(String::as_str)).await
}

async fn post_computer_status(
    origin: &str,
    request: StatusRequest,
    account_session: Option<&str>,
) -> BridgeResult<Option<String>> {
    let mut request_builder = http(origin)?
        .post(format!("{origin}{}", request.path))
        .header("content-type", "application/json")
        .body(serde_json::to_string(&request.body).map_err(|_| enrollment_error())?);
    if let Some(account_session) = account_session {
        request_builder = request_builder.header("Peppy-Account-Session", account_session);
    }
    let response = request_builder
        .send()
        .await
        .map_err(|_| NetError::Offline)?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    Ok(Some(body(response).await?))
}

async fn post_computer_confirm(
    origin: &str,
    invitation_id: &str,
    request: Zeroizing<String>,
    account_session: Option<&str>,
) -> BridgeResult<Result<String, (reqwest::StatusCode, Option<String>)>> {
    let mut call = http(origin)?
        .post(format!(
            "{origin}{COMPUTER_STATUS_PATH}/{invitation_id}/confirm"
        ))
        .header("content-type", "application/json")
        .body(request.to_string());
    if let Some(account_session) = account_session {
        call = call.header("Peppy-Account-Session", account_session);
    }
    let response = call.send().await.map_err(|_| NetError::Offline)?;
    if response.status().is_success() {
        return Ok(Ok(String::from_utf8(
            read_bounded(response, MAX_JSON_BYTES).await?,
        )
        .map_err(|_| enrollment_error())?));
    }
    let status = response.status();
    let code = read_bounded(response, MAX_ERROR_BODY_BYTES)
        .await
        .ok()
        .and_then(|body| serde_json::from_slice::<serde_json::Value>(&body).ok())
        .and_then(|value| value.get("code")?.as_str().map(ToOwned::to_owned));
    Ok(Err((status, code)))
}

fn terminal_confirm_view(
    state: &AppState,
    error_code: &'static str,
) -> BridgeResult<ComputerClaimantView> {
    let mut controller = state
        .enrollment
        .lock()
        .map_err(|_| BridgeError::host_state())?;
    let claimant = controller
        .computer_claimant
        .as_ref()
        .ok_or_else(enrollment_error)?;
    if claimant.prepared_identity.is_some() {
        return Err(enrollment_error());
    }
    let view = ComputerClaimantView {
        stage: "error",
        invitation_id: claimant.view.invitation_id.clone(),
        qr_payload: None,
        awaiting_owner: false,
        state: claimant.view.state,
        expires_in_seconds: 0,
        verification_code: None,
        error_code: Some(error_code),
    };
    controller.computer_claimant = None;
    controller.client = None;
    Ok(view)
}

fn update_view_from_status(
    state: &AppState,
    status: Option<String>,
) -> BridgeResult<ComputerClaimantView> {
    let mut controller = state
        .enrollment
        .lock()
        .map_err(|_| BridgeError::host_state())?;
    let client = controller.client.as_ref().ok_or_else(enrollment_error)?;
    let local = match status {
        Some(status) => client.computer_claimant_view(status),
        None => client.computer_awaiting_owner_view(),
    }
    .map_err(|_| enrollment_error())?;
    let claimant = controller
        .computer_claimant
        .as_mut()
        .ok_or_else(enrollment_error)?;
    let has_local_material =
        claimant.confirm_request.is_some() || claimant.prepared_identity.is_some();
    let view = claimant_view(local, Some(claimant.qr_payload.clone()), has_local_material);
    claimant.view = view.clone();
    Ok(view)
}

#[tauri::command]
pub async fn enrollment_begin_computer_claimant(
    window: WebviewWindow,
    state: State<'_, AppState>,
) -> BridgeResult<ComputerClaimantView> {
    let _operation = crate::admit_update_operation(&state)?;
    crate::require_main(window.label())?;
    let _import = state.import_lock.lock().await;
    require_no_pending_computer_install(&state)?;
    let origin = {
        let mut controller = state
            .enrollment
            .lock()
            .map_err(|_| BridgeError::host_state())?;
        let origin = controller.origin()?.to_owned();
        if let Some(claimant) = controller.computer_claimant.as_ref() {
            if claimant_must_finish(claimant) {
                return Err(BridgeError::new(
                    "enrollment-state",
                    "An approved computer connection must finish before another is started.",
                ));
            }
            // Waiting, claimed, denied, expired and remote terminal views have no opened
            // handoff, so they can be abandoned locally without rotating an approved flow.
            controller.computer_claimant = None;
            controller.client = None;
        }
        origin
    };
    if saved_identity_blocks_claimant(&state, &origin)? {
        return Err(BridgeError::new(
            "identity-preserved",
            "This device already has a saved identity. Unlock it instead of replacing it.",
        ));
    }
    let install_baseline = state.config()?;
    let expected_binding = install_baseline.active_binding().cloned();
    let qr_payload = {
        let mut controller = state
            .enrollment
            .lock()
            .map_err(|_| BridgeError::host_state())?;
        admission_client(&mut controller)?
            .begin_computer_claimant()
            .map_err(|_| enrollment_error())?
    };
    let invitation_id = serde_json::from_str::<serde_json::Value>(&qr_payload)
        .ok()
        .and_then(|value| value.get("invitation_id")?.as_str().map(ToOwned::to_owned))
        .ok_or_else(enrollment_error)?;
    let view = ComputerClaimantView {
        stage: "waiting_owner",
        invitation_id,
        qr_payload: Some(qr_payload.clone()),
        awaiting_owner: true,
        state: "waiting",
        expires_in_seconds: 0,
        verification_code: None,
        error_code: None,
    };
    state
        .enrollment
        .lock()
        .map_err(|_| BridgeError::host_state())?
        .computer_claimant = Some(ClaimantState {
        expected_binding,
        qr_payload,
        view: view.clone(),
        confirm_request: None,
        prepared_identity: None,
        install_baseline,
        install_rollback: None,
        database_key: None,
        installed: false,
    });
    Ok(view)
}

#[tauri::command]
pub async fn enrollment_computer_claimant_status(
    window: WebviewWindow,
    state: State<'_, AppState>,
    hosted: State<'_, crate::hosted::HostedState>,
) -> BridgeResult<ComputerClaimantView> {
    let _operation = crate::admit_update_operation(&state)?;
    crate::require_main(window.label())?;
    let _import = state.import_lock.lock().await;
    if let Some(view) = state
        .enrollment
        .lock()
        .map_err(|_| BridgeError::host_state())?
        .computer_claimant
        .as_ref()
        .filter(|claimant| {
            claimant.confirm_request.is_some() || claimant.prepared_identity.is_some()
        })
        .map(ClaimantState::resume_view)
    {
        return Ok(view);
    }
    update_view_from_status(&state, fetch_status(&state, &hosted).await?)
}

#[tauri::command]
pub async fn enrollment_confirm_computer_claimant(
    window: WebviewWindow,
    app: AppHandle,
    state: State<'_, AppState>,
    hosted: State<'_, crate::hosted::HostedState>,
) -> BridgeResult<ComputerClaimantView> {
    let _operation = crate::admit_update_operation(&state)?;
    crate::require_main(window.label())?;
    run_computer_confirmation(&state, confirm_computer_claimant(&app, &state, &hosted)).await
}

async fn run_computer_confirmation(
    state: &AppState,
    finish: impl std::future::Future<Output = BridgeResult<ComputerClaimantView>>,
) -> BridgeResult<ComputerClaimantView> {
    let _import = state.import_lock.lock().await;
    let result = finish.await;
    if let Err(error) = &result {
        let mut controller = state
            .enrollment
            .lock()
            .map_err(|_| BridgeError::host_state())?;
        if let Some(claimant) = controller.computer_claimant.as_mut() {
            claimant.view.error_code = Some(error.code);
            if claimant.confirm_request.is_some() || claimant.prepared_identity.is_some() {
                claimant.view.stage = "finishing";
            }
        }
    }
    result
}

async fn confirm_computer_claimant(
    app: &AppHandle,
    state: &AppState,
    hosted: &crate::hosted::HostedState,
) -> BridgeResult<ComputerClaimantView> {
    let prepared = {
        let controller = state
            .enrollment
            .lock()
            .map_err(|_| BridgeError::host_state())?;
        let claimant = controller
            .computer_claimant
            .as_ref()
            .ok_or_else(enrollment_error)?;
        (
            claimant.prepared_identity.is_some(),
            claimant.expected_binding.clone(),
        )
    };
    if prepared.0 {
        install_prepared_identity(app, state, prepared.1).await?;
        let has_gateway = computer_gateway_presence(state).await?;
        return commit_computer_install(state, has_gateway);
    }
    let saved_request = {
        let controller = state
            .enrollment
            .lock()
            .map_err(|_| BridgeError::host_state())?;
        controller
            .computer_claimant
            .as_ref()
            .and_then(|claimant| claimant.confirm_request.as_ref())
            .cloned()
    };
    let request = match saved_request {
        Some(request) => request,
        None => {
            let status = fetch_status(state, hosted).await?;
            let status_view = update_view_from_status(state, status.clone())?;
            if status_view.state != "approved" {
                return Ok(status_view);
            }
            let status = status.ok_or_else(enrollment_error)?;
            let request = state
                .enrollment
                .lock()
                .map_err(|_| BridgeError::host_state())?
                .client
                .as_mut()
                .ok_or_else(enrollment_error)?
                .confirm_computer_enrollment(status)
                .map_err(|_| enrollment_error())?;
            state
                .enrollment
                .lock()
                .map_err(|_| BridgeError::host_state())?
                .computer_claimant
                .as_mut()
                .ok_or_else(enrollment_error)?
                .confirm_request = Some(Zeroizing::new(request));
            state
                .enrollment
                .lock()
                .map_err(|_| BridgeError::host_state())?
                .computer_claimant
                .as_ref()
                .and_then(|claimant| claimant.confirm_request.as_ref())
                .cloned()
                .ok_or_else(enrollment_error)?
        }
    };
    let (origin, expected_binding, invitation_id) = {
        let controller = state
            .enrollment
            .lock()
            .map_err(|_| BridgeError::host_state())?;
        let claimant = controller
            .computer_claimant
            .as_ref()
            .ok_or_else(enrollment_error)?;
        (
            controller.origin()?.to_owned(),
            claimant.expected_binding.clone(),
            claimant.view.invitation_id.clone(),
        )
    };
    let account = hosted_account_header(state, hosted).await?;
    let complete = post_computer_confirm(
        &origin,
        &invitation_id,
        request,
        account.as_deref().map(String::as_str),
    )
    .await?;
    let complete = match complete {
        Ok(complete) => complete,
        Err((status, code)) => {
            if let Some(error_code) = terminal_confirm_error_code(status, code.as_deref()) {
                return terminal_confirm_view(state, error_code);
            }
            return Err(BridgeError::new(
                "server-rejected",
                "The server rejected the request.",
            ));
        }
    };
    retain_computer_admission(state, complete)?;
    install_prepared_identity(app, state, expected_binding).await?;
    let has_gateway = computer_gateway_presence(state).await?;
    commit_computer_install(state, has_gateway)
}

#[tauri::command]
pub fn enrollment_cancel_computer_claimant(
    window: WebviewWindow,
    state: State<'_, AppState>,
) -> BridgeResult<()> {
    crate::require_main(window.label())?;
    let _import = state
        .import_lock
        .try_lock()
        .map_err(|_| enrollment_error())?;
    cancel_computer_claimant(&state)
}

async fn discard_computer_claimant(state: &AppState) -> BridgeResult<()> {
    let _import = state.import_lock.lock().await;
    let retained_marker = {
        let controller = state
            .enrollment
            .lock()
            .map_err(|_| BridgeError::host_state())?;
        controller
            .computer_claimant
            .as_ref()
            .and_then(|claimant| claimant.install_rollback.clone())
    };
    let marker = load_computer_install_rollback(state)?;
    if let Some(retained) = retained_marker.as_ref() {
        if marker
            .as_ref()
            .is_some_and(|marker| marker.attempt_id != retained.attempt_id)
        {
            return Err(discard_ownership_error());
        }
        if marker.is_none() {
            // A failed initial marker write cannot have authorized any install write. Do not
            // invent deletion authority from the in-memory copy if a directory/item is present.
            if std::fs::symlink_metadata(&retained.data_dir).is_ok() {
                return Err(discard_ownership_error());
            }
            for account in [
                &retained.credential_account,
                &retained.db_key_account,
                &retained.vault_secret_account,
            ]
            .into_iter()
            .chain(retained.key_cache_accounts.iter())
            {
                if state
                    .store
                    .get(account)?
                    .is_some_and(|value| !value.is_empty())
                {
                    return Err(discard_ownership_error());
                }
            }
        }
    }

    if let Some(marker) = marker.as_ref() {
        rollback_computer_install(state, marker).await?;
    } else {
        let controller = state
            .enrollment
            .lock()
            .map_err(|_| BridgeError::host_state())?;
        if let Some(claimant) = controller.computer_claimant.as_ref() {
            if !claimant_discard_safe(state, claimant)? {
                return Err(discard_ownership_error());
            }
        } else {
            return Ok(());
        }
    }

    let mut controller = state
        .enrollment
        .lock()
        .map_err(|_| BridgeError::host_state())?;
    controller.computer_claimant = None;
    controller.client = None;
    Ok(())
}

/// Drops only this local claimant. It never changes the approved server invitation.
#[tauri::command]
pub async fn enrollment_discard_computer_claimant(
    window: WebviewWindow,
    state: State<'_, AppState>,
) -> BridgeResult<()> {
    let _operation = crate::admit_update_operation(&state)?;
    crate::require_main(window.label())?;
    discard_computer_claimant(&state).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        http::{HeaderMap, StatusCode},
        routing::post,
        Router,
    };
    use std::sync::{Arc, Mutex};

    fn claimant(state: &'static str) -> ClaimantState {
        ClaimantState {
            expected_binding: None,
            qr_payload: "qr".into(),
            view: ComputerClaimantView {
                stage: "waiting_owner",
                invitation_id: "id".into(),
                qr_payload: Some("qr".into()),
                awaiting_owner: state == "waiting",
                state,
                expires_in_seconds: 30,
                verification_code: None,
                error_code: None,
            },
            confirm_request: None,
            prepared_identity: None,
            install_baseline: crate::credentials::HostConfig::default(),
            install_rollback: None,
            database_key: None,
            installed: false,
        }
    }

    fn state_with_claimant(claimant: Option<ClaimantState>) -> (tempfile::TempDir, AppState) {
        let root = tempfile::tempdir().unwrap();
        let state = AppState::new(
            root.path().to_path_buf(),
            Arc::new(crate::secure_store::MemoryStore::default()),
            crate::tests::noop(),
        );
        let mut controller = state.enrollment.lock().unwrap();
        controller.origin = Some("https://enroll.example".into());
        controller.client = Some(
            EnrollmentClient::new(
                "https://enroll.example".into(),
                ClientKind::Desktop,
                DEVICE_NAME.into(),
            )
            .unwrap(),
        );
        controller.computer_claimant = claimant;
        drop(controller);
        (root, state)
    }

    fn hosted_view(
        state: EnrollmentState,
    ) -> peppy_hosted_client::newenrollment::ComputerClaimantView {
        peppy_hosted_client::newenrollment::ComputerClaimantView {
            invitation_id: uuid::Uuid::new_v4(),
            awaiting_owner: state == EnrollmentState::Waiting,
            state,
            expires_in_seconds: 30,
            sas: None,
        }
    }

    /// Real QR, owner-proof and encrypted handoff exchange; only the transport is omitted.
    fn pending_admission_fixture(
        store: Arc<dyn SecretStore>,
    ) -> (tempfile::TempDir, AppState, String) {
        use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
        use peppy_hosted_api::{
            computer_enrollment::{
                ApproveComputerEnrollmentRequest, ComputerEnrollmentCreated,
                ComputerEnrollmentStatus,
            },
            enrollment::{AdmissionComplete, VaultMaterial},
        };
        let origin = "https://enroll.example";
        let secret = "correct horse battery staple";
        let vault_id = uuid::Uuid::new_v4();
        let owner_device = uuid::Uuid::new_v4();
        let profile = KeyProfile::new(vault_id, 3).unwrap();
        let root_key = peppy_crypto::derive_root_key(secret, &profile).unwrap();
        let header = peppy_crypto::create_vault_check_header(&root_key, profile.clone()).unwrap();
        let mut client =
            EnrollmentClient::new(origin.into(), ClientKind::Desktop, DEVICE_NAME.into()).unwrap();
        let qr = client.begin_computer_claimant().unwrap();
        let mut owner = EnrollmentClient::for_existing_owner(
            origin.into(),
            ClientKind::Desktop,
            "Owner".into(),
            profile.clone(),
            header.clone(),
        )
        .unwrap();
        owner.configure_computer_owner_device(owner_device).unwrap();
        owner.scan_computer_qr(qr.clone()).unwrap();
        owner
            .accept_computer_created(
                serde_json::to_string(&ComputerEnrollmentCreated {
                    server_nonce: URL_SAFE_NO_PAD.encode([7; 32]),
                    expires_in_seconds: 60,
                })
                .unwrap(),
            )
            .unwrap();
        let owner_root = tempfile::tempdir().unwrap();
        let owner_client = peppy_client_core::Client::open(
            peppy_client_core::ClientConfig {
                database_path: owner_root.path().join("owner.sqlcipher"),
                vault_id: peppy_client_core::VaultId(vault_id),
                device_id: peppy_client_core::DeviceId(owner_device),
            },
            peppy_client_core::DatabaseKey::new(&[31; 32]).unwrap(),
        )
        .unwrap();
        owner_client.unlock(&profile, &header, secret).unwrap();
        let approval: ApproveComputerEnrollmentRequest = serde_json::from_str(
            &owner
                .approve_computer_enrollment_with_client(true, &owner_client)
                .unwrap(),
        )
        .unwrap();
        let device_id = approval.transcript.recipient.device.device_id;
        let status = ComputerEnrollmentStatus {
            state: EnrollmentState::Approved,
            expires_in_seconds: 60,
            transcript: Some(approval.transcript),
            encrypted_key_package: Some(approval.encrypted_key_package),
            transfer_nonce: Some(approval.transfer_nonce),
            owner_proof: Some(approval.owner_proof),
        };
        let status_json = serde_json::to_string(&status).unwrap();
        let view = claimant_view(
            client.computer_claimant_view(status_json.clone()).unwrap(),
            Some(qr.clone()),
            false,
        );
        let request = client.confirm_computer_enrollment(status_json).unwrap();
        let complete = serde_json::to_string(&AdmissionComplete {
            vault_id,
            device_id,
            role: EnrollmentRole::Device,
            material: VaultMaterial {
                profile: serde_json::to_value(&profile).unwrap(),
                check_header: URL_SAFE_NO_PAD.encode(serde_json::to_vec(&header).unwrap()),
                fingerprint: profile.fingerprint().unwrap(),
                key_epoch: profile.key_epoch,
                admission_public_key: URL_SAFE_NO_PAD.encode([0; 32]),
            },
        })
        .unwrap();
        let root = tempfile::tempdir().unwrap();
        let state = AppState::new(root.path().to_path_buf(), store, crate::tests::noop());
        {
            let mut controller = state.enrollment.lock().unwrap();
            controller.origin = Some(origin.into());
            controller.client = Some(client);
            let mut active = claimant("approved");
            active.view = view;
            active.qr_payload = qr;
            active.confirm_request = Some(Zeroizing::new(request));
            controller.computer_claimant = Some(active);
        }
        (root, state, complete)
    }

    #[derive(Default)]
    struct FailOneReadStore {
        inner: crate::secure_store::MemoryStore,
        fail: std::sync::atomic::AtomicBool,
    }

    impl SecretStore for FailOneReadStore {
        fn get(&self, account: &str) -> BridgeResult<Option<Zeroizing<Vec<u8>>>> {
            if self.fail.swap(false, std::sync::atomic::Ordering::SeqCst) {
                return Err(BridgeError::new("secure-store-unavailable", "injected"));
            }
            self.inner.get(account)
        }
        fn set(&self, account: &str, bytes: &[u8]) -> BridgeResult<()> {
            self.inner.set(account, bytes)
        }
        fn delete(&self, account: &str) -> BridgeResult<()> {
            self.inner.delete(account)
        }
    }

    #[tokio::test]
    async fn real_admission_survives_preflight_failure_reload_and_retry() {
        let store = Arc::new(FailOneReadStore::default());
        let (_root, state, complete) = pending_admission_fixture(store.clone());
        retain_computer_admission(&state, complete).unwrap();
        store.fail.store(true, std::sync::atomic::Ordering::SeqCst);
        let error = run_computer_confirmation(&state, async {
            prepare_computer_install(&state)?;
            Err(enrollment_error())
        })
        .await
        .err()
        .unwrap();
        assert_eq!(error.code, "secure-store-unavailable");
        let view = {
            let controller = state.enrollment.lock().unwrap();
            serde_json::to_value(
                retained_discovery_view(&controller, "https://enroll.example")
                    .unwrap()
                    .unwrap(),
            )
            .unwrap()
        };
        assert_eq!(view["computerClaimant"]["stage"], "finishing");
        assert_eq!(
            view["computerClaimant"]["errorCode"],
            "secure-store-unavailable"
        );
        assert!(view["computerClaimant"]["qrPayload"].is_null());
        prepare_computer_install(&state).unwrap();
        activate_retained_handoff_in_test(&state);
    }

    fn activate_retained_handoff_in_test(state: &AppState) {
        let mut controller = state.enrollment.lock().unwrap();
        let active = controller.computer_claimant.as_mut().unwrap();
        let marker = active.install_rollback.as_ref().unwrap();
        require_current_marker(state, marker).unwrap();
        let key = state.store.get(&marker.db_key_account).unwrap().unwrap();
        let core = peppy_client_core::Client::open(
            peppy_client_core::ClientConfig {
                database_path: marker.database_path.clone(),
                vault_id: peppy_client_core::VaultId(
                    uuid::Uuid::parse_str(&marker.target.vault_id).unwrap(),
                ),
                device_id: peppy_client_core::DeviceId(
                    uuid::Uuid::parse_str(&marker.target.device_id).unwrap(),
                ),
            },
            peppy_client_core::DatabaseKey::new(&key).unwrap(),
        )
        .unwrap();
        assert!(active
            .prepared_identity
            .as_mut()
            .unwrap()
            .activate_verified_handoff(&core)
            .unwrap());
        assert!(core.key_status().unwrap().active_epoch.is_some());
    }

    #[cfg(unix)]
    #[test]
    fn real_admission_survives_a_marker_write_failure_and_retry() {
        use std::os::unix::fs::PermissionsExt;
        let store = Arc::new(crate::secure_store::MemoryStore::default());
        let (root, state, complete) = pending_admission_fixture(store);
        retain_computer_admission(&state, complete).unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o500)).unwrap();
        let result = prepare_computer_install(&state);
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(result.unwrap_err().code, "host-state");
        assert!(state
            .enrollment
            .lock()
            .unwrap()
            .computer_claimant
            .as_ref()
            .unwrap()
            .prepared_identity
            .is_some());
        prepare_computer_install(&state).unwrap();
        activate_retained_handoff_in_test(&state);
    }

    #[test]
    fn reload_preserves_approved_and_lost_confirm_claimants_and_the_matching_client() {
        for lost_confirm in [false, true] {
            let mut active = claimant("approved");
            active.view.stage = "compare_codes";
            if lost_confirm {
                active.confirm_request = Some(Zeroizing::new("private-confirm".into()));
            }
            let (_root, state) = state_with_claimant(Some(active));
            assert_eq!(
                discovery_origin(&state, None).unwrap(),
                "https://enroll.example"
            );
            let controller = state.enrollment.lock().unwrap();
            let request = controller.client.as_ref().unwrap() as *const EnrollmentClient;
            let view = retained_discovery_view(&controller, "https://enroll.example")
                .unwrap()
                .unwrap();
            let json = serde_json::to_value(view).unwrap();
            assert_eq!(
                json["computerClaimant"]["stage"],
                if lost_confirm {
                    "finishing"
                } else {
                    "compare_codes"
                }
            );
            assert!(json["computerClaimant"]["qrPayload"].is_null());
            assert!(!json.to_string().contains("private-confirm"));
            assert_eq!(
                controller.client.as_ref().unwrap() as *const EnrollmentClient,
                request
            );
            assert_eq!(
                controller
                    .computer_claimant
                    .as_ref()
                    .unwrap()
                    .confirm_request
                    .is_some(),
                lost_confirm
            );
            assert!(retained_discovery_view(&controller, "https://other.example").is_err());
        }
    }

    #[test]
    fn discovery_omits_computer_claimant_when_none_is_retained() {
        let (_root, state) = state_with_claimant(None);
        let controller = state.enrollment.lock().unwrap();
        assert!(serde_json::to_value(controller.view())
            .unwrap()
            .get("computerClaimant")
            .is_none());
        assert!(
            retained_discovery_view(&controller, "https://enroll.example")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn generic_cancel_and_other_admission_cannot_destroy_retained_computer_material() {
        let mut active = claimant("approved");
        active.confirm_request = Some(Zeroizing::new("retained-confirm".into()));
        let (_root, state) = state_with_claimant(Some(active));
        assert_eq!(
            finish_enrollment_cancel(&state.enrollment, &*state.store, 0, None)
                .unwrap_err()
                .code,
            "enrollment-state"
        );
        let mut controller = state.enrollment.lock().unwrap();
        assert_eq!(
            admission_client(&mut controller).err().unwrap().code,
            "enrollment-state"
        );
        assert!(controller.client.is_some());
        assert!(controller
            .computer_claimant
            .as_ref()
            .unwrap()
            .confirm_request
            .is_some());
    }

    #[test]
    fn confirmed_or_consumed_without_local_material_is_remote_consumed() {
        for state in [EnrollmentState::Confirmed, EnrollmentState::Consumed] {
            let view = claimant_view(hosted_view(state), None, false);
            assert_eq!(view.stage, "error");
            assert_eq!(view.error_code, Some("computer_remote_consumed"));
        }
    }

    #[test]
    fn confirmed_or_consumed_with_local_material_is_finishing() {
        for state in [EnrollmentState::Confirmed, EnrollmentState::Consumed] {
            let view = claimant_view(hosted_view(state), None, true);
            assert_eq!(view.stage, "finishing");
            assert_eq!(view.error_code, None);
        }
    }

    #[test]
    fn confirm_without_local_material_returns_nonapproved_server_view() {
        for (state, expected_stage, expected_error) in [
            (EnrollmentState::Denied, "denied", None),
            (EnrollmentState::Expired, "expired", None),
            (
                EnrollmentState::Consumed,
                "error",
                Some("computer_remote_consumed"),
            ),
        ] {
            let view = claimant_view(hosted_view(state), None, false);
            assert_eq!(view.stage, expected_stage);
            assert_eq!(view.error_code, expected_error);
        }
    }

    #[test]
    fn cancel_without_a_claimant_is_idempotent() {
        let (_root, state) = state_with_claimant(None);
        assert_eq!(cancel_computer_claimant(&state), Ok(()));
        assert_eq!(cancel_computer_claimant(&state), Ok(()));
    }

    #[tokio::test]
    async fn discard_drops_claimant_confirm_material_and_client() {
        let mut active = claimant("approved");
        active.confirm_request = Some(Zeroizing::new("private-confirm".into()));
        let (_root, state) = state_with_claimant(Some(active));

        assert_eq!(discard_computer_claimant(&state).await, Ok(()));
        let mut controller = state.enrollment.lock().unwrap();
        assert!(controller.computer_claimant.is_none());
        assert!(controller.client.is_none());
        assert!(admission_client(&mut controller)
            .unwrap()
            .begin_computer_claimant()
            .is_ok());
    }

    #[test]
    fn terminal_status_table_has_discriminating_error_codes() {
        for (status, code, expected) in [
            (
                StatusCode::NOT_FOUND,
                None,
                Some("computer_verification_failed"),
            ),
            (StatusCode::GONE, None, Some("computer_verification_failed")),
            (StatusCode::CONFLICT, None, Some("computer_remote_consumed")),
            (
                StatusCode::UNAUTHORIZED,
                None,
                Some("computer_remote_consumed"),
            ),
            (
                StatusCode::FORBIDDEN,
                None,
                Some("computer_verification_failed"),
            ),
            (
                StatusCode::FORBIDDEN,
                Some("foreign_vault"),
                Some("foreign_vault"),
            ),
            (StatusCode::INTERNAL_SERVER_ERROR, None, None),
            (StatusCode::BAD_GATEWAY, None, None),
        ] {
            assert_eq!(terminal_confirm_error_code(status, code), expected);
        }
    }

    #[test]
    fn terminal_view_drops_an_unprepared_claimant_and_client() {
        let (_root, state) = state_with_claimant(Some(claimant("approved")));
        let view = terminal_confirm_view(&state, "computer_verification_failed").unwrap();
        assert_eq!(view.stage, "error");
        assert_eq!(view.error_code, Some("computer_verification_failed"));
        let controller = state.enrollment.lock().unwrap();
        assert!(controller.computer_claimant.is_none());
        assert!(controller.client.is_none());
    }

    #[test]
    fn discard_accepts_only_an_uninstalled_prepared_target() {
        assert!(uninstalled_prepared_target_is_discard_safe(false, false));
        assert!(!uninstalled_prepared_target_is_discard_safe(true, false));
        assert!(!uninstalled_prepared_target_is_discard_safe(false, true));
        assert!(!uninstalled_prepared_target_is_discard_safe(true, true));
    }

    #[tokio::test]
    async fn server_and_network_failures_leave_the_claimant_for_retry() {
        for status in [StatusCode::INTERNAL_SERVER_ERROR, StatusCode::BAD_GATEWAY] {
            let app = Router::new().route(
                "/v1/enrollment/computers/test/confirm",
                post(move || async move { (status, "server failure") }),
            );
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let origin = format!("http://{}", listener.local_addr().unwrap());
            let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
            let (_root, state) = state_with_claimant(Some(claimant("approved")));

            let response =
                post_computer_confirm(&origin, "test", Zeroizing::new("{}".into()), None)
                    .await
                    .unwrap();
            server.abort();

            assert!(matches!(response, Err((actual, _)) if actual == status));
            assert!(state.enrollment.lock().unwrap().computer_claimant.is_some());
        }

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        drop(listener);
        let (_root, state) = state_with_claimant(Some(claimant("approved")));
        let error = post_computer_confirm(&origin, "test", Zeroizing::new("{}".into()), None)
            .await
            .unwrap_err();
        assert_eq!(error.code, "offline");
        assert!(state.enrollment.lock().unwrap().computer_claimant.is_some());
    }

    fn binding(origin: &str) -> crate::credentials::Binding {
        crate::credentials::Binding {
            origin: origin.into(),
            vault_id: uuid::Uuid::new_v4().to_string(),
            device_id: uuid::Uuid::new_v4().to_string(),
        }
    }

    fn install_marker(
        state: &AppState,
        target: crate::credentials::Binding,
        previous_config: crate::credentials::HostConfig,
        credential: &[u8],
    ) -> ComputerInstallRollback {
        let mut marker =
            ComputerInstallRollback::new(state, target, previous_config, credential).unwrap();
        persist_computer_install_rollback(state, &marker).unwrap();
        reserve_computer_directory(state, &mut marker).unwrap();
        persist_computer_install_rollback(state, &marker).unwrap();
        marker
    }

    #[test]
    fn tampered_marker_paths_and_baselines_never_authorize_deletion() {
        for mutation in ["traversal", "absolute", "origin", "known", "epoch"] {
            let (_root, state) = state_with_claimant(None);
            let target = binding("https://enroll.example");
            let credential = b"credential";
            let mut marker = install_marker(&state, target, state.config().unwrap(), credential);
            let outside = tempfile::tempdir().unwrap();
            let sentinel = outside.path().join("keep");
            std::fs::write(&sentinel, b"unrelated").unwrap();
            match mutation {
                "traversal" => marker.target.device_id = "../../../outside".into(),
                "absolute" => marker.target.device_id = outside.path().to_str().unwrap().into(),
                "origin" => marker.target.origin = "HTTPS://ENROLL.EXAMPLE/".into(),
                "known" => marker.previous_config.remember(&marker.target.clone()),
                "epoch" => marker.key_cache_accounts = vec![marker.target.key_cache_account(91)],
                _ => unreachable!(),
            }
            marker.data_dir = marker.target.data_dir(&state.root);
            marker.database_path = marker.target.database_path(&state.root);
            marker.credential_account = marker.target.credential_account();
            marker.db_key_account = marker.target.db_key_account();
            marker.vault_secret_account = marker.target.vault_secret_account();
            std::fs::write(
                computer_install_rollback_path(&state),
                serde_json::to_vec(&marker).unwrap(),
            )
            .unwrap();
            assert_eq!(
                recover_computer_install_rollback(&state).unwrap_err().code,
                "computer-discard-ownership-unproven",
                "{mutation}"
            );
            assert_eq!(std::fs::read(sentinel).unwrap(), b"unrelated");
            assert!(computer_install_rollback_path(&state).exists());
        }
    }

    #[cfg(unix)]
    #[test]
    fn rollback_refuses_a_symlink_ancestor_without_deleting_its_child() {
        let (_root, state) = state_with_claimant(None);
        let target = binding("https://enroll.example");
        let marker = install_marker(
            &state,
            target.clone(),
            state.config().unwrap(),
            b"credential",
        );
        let outside = tempfile::tempdir().unwrap();
        let child = outside.path().join(&target.device_id);
        std::fs::create_dir(&child).unwrap();
        std::fs::write(child.join("keep"), b"unrelated").unwrap();
        let vault = marker.data_dir.parent().unwrap();
        std::fs::rename(vault, state.root.join("reserved-vault")).unwrap();
        std::os::unix::fs::symlink(outside.path(), vault).unwrap();
        assert_eq!(
            recover_computer_install_rollback(&state).unwrap_err().code,
            "computer-discard-ownership-unproven"
        );
        assert_eq!(std::fs::read(child.join("keep")).unwrap(), b"unrelated");
        assert!(computer_install_rollback_path(&state).exists());
    }

    #[tokio::test]
    async fn replacement_directory_is_preserved_even_with_a_copied_attempt_stamp() {
        for credential_already_removed in [false, true] {
            let (_root, state) = state_with_claimant(None);
            let target = binding("https://enroll.example");
            let marker = install_marker(&state, target, state.config().unwrap(), b"credential");
            simulate_partial_install(&state, &marker, b"credential");
            if credential_already_removed {
                state.store.delete(&marker.credential_account).unwrap();
            }
            std::fs::rename(&marker.data_dir, state.root.join("original-directory")).unwrap();
            std::fs::create_dir(&marker.data_dir).unwrap();
            std::fs::write(
                marker.data_dir.join(COMPUTER_DIRECTORY_STAMP),
                marker.attempt_id.to_string(),
            )
            .unwrap();
            std::fs::write(&marker.database_path, b"replacement-database").unwrap();
            assert_eq!(
                discard_computer_claimant(&state).await.unwrap_err().code,
                "computer-discard-ownership-unproven"
            );
            assert_eq!(
                std::fs::read(&marker.database_path).unwrap(),
                b"replacement-database"
            );
            assert!(computer_install_rollback_path(&state).exists());
        }
    }

    #[tokio::test]
    async fn replacement_directory_after_interrupted_cleanup_is_not_adopted_on_retry() {
        let root = tempfile::tempdir().unwrap();
        let store = Arc::new(FailOneDeleteStore::default());
        let state = AppState::new(
            root.path().to_path_buf(),
            store.clone(),
            crate::tests::noop(),
        );
        let target = binding("https://enroll.example");
        let marker = install_marker(&state, target, state.config().unwrap(), b"credential");
        simulate_partial_install(&state, &marker, b"credential");
        *store.fail_account.lock().unwrap() = marker.db_key_account.clone();
        assert_eq!(
            discard_computer_claimant(&state).await.unwrap_err().code,
            "secure-store-unavailable"
        );
        assert!(!marker.data_dir.exists());
        std::fs::create_dir(&marker.data_dir).unwrap();
        std::fs::write(
            marker.data_dir.join(COMPUTER_DIRECTORY_STAMP),
            marker.attempt_id.to_string(),
        )
        .unwrap();
        std::fs::write(&marker.database_path, b"replacement-database").unwrap();
        assert_eq!(
            discard_computer_claimant(&state).await.unwrap_err().code,
            "computer-discard-ownership-unproven"
        );
        assert_eq!(
            std::fs::read(&marker.database_path).unwrap(),
            b"replacement-database"
        );
        assert!(computer_install_rollback_path(&state).exists());
    }

    #[tokio::test]
    async fn interrupted_directory_removal_keeps_ownership_until_the_last_empty_rmdir() {
        let (_root, state) = state_with_claimant(None);
        let target = binding("https://enroll.example");
        let marker = install_marker(&state, target, state.config().unwrap(), b"credential");
        // Crash after contents and stamp are gone, before the final empty rmdir.
        std::fs::remove_file(marker.data_dir.join(COMPUTER_DIRECTORY_STAMP)).unwrap();
        discard_computer_claimant(&state).await.unwrap();
        assert!(!marker.data_dir.exists());
        assert!(!computer_install_rollback_path(&state).exists());
    }

    #[tokio::test]
    async fn replacement_secure_items_survive_initial_cleanup_and_interrupted_retry() {
        for retry in [false, true] {
            for item in ["db-key", "vault-secret", "key-cache"] {
                let root = tempfile::tempdir().unwrap();
                let store = Arc::new(FailOneDeleteStore::default());
                let state = AppState::new(
                    root.path().to_path_buf(),
                    store.clone(),
                    crate::tests::noop(),
                );
                let target = binding("https://enroll.example");
                let mut marker =
                    install_marker(&state, target, state.config().unwrap(), b"credential");
                marker.key_epoch = Some(3);
                marker.key_cache_accounts = vec![marker.target.key_cache_account(3)];
                persist_computer_install_rollback(&state, &marker).unwrap();
                simulate_partial_install(&state, &marker, b"credential");
                if retry {
                    *store.fail_account.lock().unwrap() = marker.db_key_account.clone();
                    assert_eq!(
                        discard_computer_claimant(&state).await.unwrap_err().code,
                        "secure-store-unavailable"
                    );
                    assert!(store.get(&marker.credential_account).unwrap().is_none());
                }
                let account = match item {
                    "db-key" => &marker.db_key_account,
                    "vault-secret" => &marker.vault_secret_account,
                    "key-cache" => &marker.key_cache_accounts[0],
                    _ => unreachable!(),
                };
                store.set(account, &[9; 32]).unwrap();
                assert_eq!(
                    discard_computer_claimant(&state).await.unwrap_err().code,
                    "computer-discard-ownership-unproven",
                    "{retry}/{item}"
                );
                assert_eq!(store.get(account).unwrap().unwrap().as_slice(), &[9; 32]);
                assert!(computer_install_rollback_path(&state).exists());
                if !retry {
                    assert!(marker.database_path.exists());
                }
            }
        }
    }

    #[test]
    fn selected_origin_crash_recovers_for_fresh_and_switching_configs() {
        for previous_origin in [None, Some("https://prior.example")] {
            let root = tempfile::tempdir().unwrap();
            let store = Arc::new(crate::secure_store::MemoryStore::default());
            let state = AppState::new(
                root.path().to_path_buf(),
                store.clone(),
                crate::tests::noop(),
            );
            state
                .update_config(|config| {
                    if let Some(origin) = previous_origin {
                        config.select_origin(origin);
                    }
                })
                .unwrap();
            let previous = state.config().unwrap();
            let target = binding("https://enroll.example");
            let marker = install_marker(&state, target, previous.clone(), b"credential");
            state
                .update_config(|config| config.select_origin(&marker.target.origin))
                .unwrap();
            drop(state);
            let reopened = AppState::new(root.path().to_path_buf(), store, crate::tests::noop());
            assert_eq!(reopened.config().unwrap(), previous);
            assert!(!marker.data_dir.exists());
            assert!(!computer_install_rollback_path(&reopened).exists());
        }
    }

    #[tokio::test]
    async fn selected_origin_rollback_preserves_the_real_prior_session() {
        let (_root, state) = state_with_claimant(None);
        let prior = binding("https://prior.example");
        let bytes = crate::credentials::tests::credential_json(
            &prior.origin,
            &prior.vault_id,
            &prior.device_id,
            crate::credentials::tests::TOKEN,
        );
        state
            .store
            .set(&prior.credential_account(), &bytes)
            .unwrap();
        state
            .update_config(|config| {
                config.select_origin(&prior.origin);
                config.remember(&prior);
                config.active = Some(prior.clone());
            })
            .unwrap();
        let session = Arc::new(
            crate::session::open_session(
                &state.root,
                &*state.store,
                &prior,
                &[],
                crate::tests::noop(),
            )
            .unwrap(),
        );
        *state.session.lock().await = Some(session.clone());
        let previous = state.config().unwrap();
        let target = binding("https://enroll.example");
        let marker = install_marker(&state, target, previous.clone(), b"credential");
        state
            .update_config(|config| config.select_origin(&marker.target.origin))
            .unwrap();
        discard_computer_claimant(&state).await.unwrap();
        assert_eq!(state.config().unwrap(), previous);
        assert!(Arc::ptr_eq(
            state.session.lock().await.as_ref().unwrap(),
            &session
        ));
        assert!(!session.cancel.is_cancelled());
        assert!(prior.database_path(&state.root).exists());
        assert!(!marker.data_dir.exists());
    }

    #[tokio::test]
    async fn unresolved_marker_fences_session_activation_and_overwrite() {
        let (_root, state) = state_with_claimant(None);
        let target = binding("https://enroll.example");
        let marker = install_marker(
            &state,
            target.clone(),
            state.config().unwrap(),
            b"credential",
        );
        simulate_partial_install(&state, &marker, b"credential");
        let mut replacement = marker.clone();
        replacement.attempt_id = uuid::Uuid::new_v4();
        assert_eq!(
            persist_computer_install_rollback(&state, &replacement)
                .unwrap_err()
                .code,
            "computer-discard-ownership-unproven"
        );
        assert_eq!(
            state.session().await.err().unwrap().code,
            "computer-discard-ownership-unproven"
        );
        assert_eq!(
            check_computer_install_access(&state, &target, None)
                .unwrap_err()
                .code,
            "computer-discard-ownership-unproven"
        );
        let credential = std::sync::Arc::new(
            parse_credential(&crate::credentials::tests::credential_json(
                &target.origin,
                &target.vault_id,
                &target.device_id,
                crate::credentials::tests::TOKEN,
            ))
            .unwrap(),
        );
        assert_eq!(
            crate::verify_import(&state, &credential, None)
                .await
                .unwrap_err()
                .code,
            "computer-discard-ownership-unproven"
        );
        let retained = load_computer_install_rollback(&state).unwrap().unwrap();
        assert_eq!(retained.attempt_id, marker.attempt_id);
        assert!(marker.database_path.exists());
    }

    fn committable_attempt() -> (tempfile::TempDir, AppState, ComputerInstallRollback) {
        let (root, state) = state_with_claimant(Some(claimant("approved")));
        let target = binding("https://enroll.example");
        let marker = install_marker(&state, target, state.config().unwrap(), b"credential");
        simulate_partial_install(&state, &marker, b"credential");
        {
            let mut controller = state.enrollment.lock().unwrap();
            let active = controller.computer_claimant.as_mut().unwrap();
            active.install_rollback = Some(marker.clone());
            active.installed = true;
        }
        (root, state, marker)
    }

    #[tokio::test]
    async fn confirm_finalization_wins_before_queued_discard_without_deleting_connected_identity() {
        let (_root, state, marker) = committable_attempt();
        let (release, presence_ready) = tokio::sync::oneshot::channel();
        let confirm = run_computer_confirmation(&state, async {
            presence_ready.await.unwrap();
            commit_computer_install(&state, None)
        });
        tokio::pin!(confirm);
        assert!(futures_util::poll!(&mut confirm).is_pending());
        let discard = discard_computer_claimant(&state);
        tokio::pin!(discard);
        assert!(futures_util::poll!(&mut discard).is_pending());
        assert!(marker.database_path.exists());
        release.send(()).unwrap();
        assert_eq!(confirm.await.unwrap().stage, "connected");
        discard.await.unwrap();
        assert!(marker.database_path.exists());
        assert!(state
            .store
            .get(&marker.credential_account)
            .unwrap()
            .is_some());
        assert!(!computer_install_rollback_path(&state).exists());
        // A clone acquired before commit is no longer deletion authority either.
        assert_eq!(
            rollback_computer_install(&state, &marker)
                .await
                .unwrap_err()
                .code,
            "computer-discard-ownership-unproven"
        );
        assert!(marker.database_path.exists());
    }

    #[tokio::test]
    async fn discard_in_flight_wins_before_queued_confirmation_can_report_connected() {
        let (_root, state, marker) = committable_attempt();
        // The real rollback waits here before deciding/draining its target session.
        let session_gate = state.session.lock().await;
        let discard = discard_computer_claimant(&state);
        tokio::pin!(discard);
        assert!(futures_util::poll!(&mut discard).is_pending());
        let confirm =
            run_computer_confirmation(&state, async { commit_computer_install(&state, None) });
        tokio::pin!(confirm);
        assert!(futures_util::poll!(&mut confirm).is_pending());
        assert!(marker.database_path.exists());
        drop(session_gate);
        discard.await.unwrap();
        assert!(confirm.await.is_err());
        assert!(!marker.database_path.exists());
        assert!(state
            .store
            .get(&marker.credential_account)
            .unwrap()
            .is_none());
    }

    #[test]
    fn oversized_marker_is_bounded_and_left_for_safe_recovery() {
        let (_root, state) = state_with_claimant(None);
        std::fs::write(
            computer_install_rollback_path(&state),
            vec![b' '; 64 * 1024 + 1],
        )
        .unwrap();
        assert_eq!(
            recover_computer_install_rollback(&state).unwrap_err().code,
            "computer-discard-ownership-unproven"
        );
        assert!(computer_install_rollback_path(&state).exists());
    }

    fn simulate_partial_install(
        state: &AppState,
        marker: &ComputerInstallRollback,
        credential: &[u8],
    ) {
        state
            .store
            .set(&marker.credential_account, credential)
            .unwrap();
        state.store.set(&marker.db_key_account, &[7; 32]).unwrap();
        std::fs::create_dir_all(&marker.data_dir).unwrap();
        std::fs::write(&marker.database_path, b"db").unwrap();
        std::fs::write(marker.database_path.with_extension("sqlcipher-wal"), b"wal").unwrap();
        state
            .update_config(|config| {
                config.select_origin(&marker.target.origin);
                config.remember(&marker.target);
                config.active = Some(marker.target.clone());
            })
            .unwrap();
        persist_computer_install_rollback(state, marker).unwrap();
    }

    #[tokio::test]
    async fn rollback_removes_only_attempt_artifacts_and_restores_prior_active_target() {
        let root = tempfile::tempdir().unwrap();
        let store = Arc::new(crate::secure_store::MemoryStore::default());
        let state = AppState::new(
            root.path().to_path_buf(),
            store.clone(),
            crate::tests::noop(),
        );
        let prior = binding("https://prior.example");
        let target = binding("https://enroll.example");
        state
            .update_config(|config| {
                config.origin = Some(prior.origin.clone());
                config.remember(&prior);
                config.active = Some(prior.clone());
            })
            .unwrap();
        let previous = state.config().unwrap();
        std::fs::create_dir_all(prior.data_dir(root.path())).unwrap();
        std::fs::write(prior.database_path(root.path()), b"prior-db").unwrap();
        store
            .set(&prior.credential_account(), b"prior-credential")
            .unwrap();
        let credential = crate::credentials::tests::credential_json(
            &target.origin,
            &target.vault_id,
            &target.device_id,
            crate::credentials::tests::TOKEN,
        );
        let marker = install_marker(&state, target.clone(), previous.clone(), &credential);
        simulate_partial_install(&state, &marker, &credential);

        discard_computer_claimant(&state).await.unwrap();

        assert_eq!(state.config().unwrap(), previous);
        assert!(!target.data_dir(root.path()).exists());
        assert_eq!(store.get(&target.credential_account()).unwrap(), None);
        assert_eq!(store.get(&target.db_key_account()).unwrap(), None);
        assert_eq!(store.get(&target.vault_secret_account()).unwrap(), None);
        assert_eq!(
            std::fs::read(prior.database_path(root.path())).unwrap(),
            b"prior-db"
        );
        assert_eq!(
            store
                .get(&prior.credential_account())
                .unwrap()
                .unwrap()
                .as_slice(),
            b"prior-credential"
        );
        assert!(!computer_install_rollback_path(&state).exists());
    }

    #[tokio::test]
    async fn fresh_begin_succeeds_after_discarding_an_activated_attempt() {
        let root = tempfile::tempdir().unwrap();
        let store = Arc::new(crate::secure_store::MemoryStore::default());
        let state = AppState::new(root.path().to_path_buf(), store, crate::tests::noop());
        let target = binding("https://enroll.example");
        state
            .update_config(|config| config.origin = Some(target.origin.clone()))
            .unwrap();
        let previous = state.config().unwrap();
        let credential = crate::credentials::tests::credential_json(
            &target.origin,
            &target.vault_id,
            &target.device_id,
            crate::credentials::tests::TOKEN,
        );
        let marker = install_marker(&state, target, previous.clone(), &credential);
        simulate_partial_install(&state, &marker, &credential);
        {
            let mut controller = state.enrollment.lock().unwrap();
            controller.origin = previous.origin.clone();
            controller.client = Some(
                EnrollmentClient::new(
                    previous.origin.clone().unwrap(),
                    ClientKind::Desktop,
                    DEVICE_NAME.into(),
                )
                .unwrap(),
            );
            let mut active = claimant("approved");
            active.install_baseline = previous;
            active.install_rollback = Some(marker);
            controller.computer_claimant = Some(active);
        }

        discard_computer_claimant(&state).await.unwrap();

        let mut controller = state.enrollment.lock().unwrap();
        assert!(controller.computer_claimant.is_none());
        assert!(admission_client(&mut controller)
            .unwrap()
            .begin_computer_claimant()
            .is_ok());
    }

    #[test]
    fn relaunch_converges_an_interrupted_attempt_before_opening_a_session() {
        let root = tempfile::tempdir().unwrap();
        let store = Arc::new(crate::secure_store::MemoryStore::default());
        let state = AppState::new(
            root.path().to_path_buf(),
            store.clone(),
            crate::tests::noop(),
        );
        let target = binding("https://enroll.example");
        state
            .update_config(|config| config.origin = Some(target.origin.clone()))
            .unwrap();
        let previous = state.config().unwrap();
        let credential = crate::credentials::tests::credential_json(
            &target.origin,
            &target.vault_id,
            &target.device_id,
            crate::credentials::tests::TOKEN,
        );
        let marker = install_marker(&state, target.clone(), previous.clone(), &credential);
        simulate_partial_install(&state, &marker, &credential);
        drop(state);

        let reopened = AppState::new(
            root.path().to_path_buf(),
            store.clone(),
            crate::tests::noop(),
        );

        assert_eq!(reopened.config().unwrap(), previous);
        assert!(!target.data_dir(root.path()).exists());
        assert!(store.get(&target.credential_account()).unwrap().is_none());
        assert!(!computer_install_rollback_path(&reopened).exists());
    }

    #[tokio::test]
    async fn rollback_refuses_a_credential_that_does_not_match_the_attempt() {
        let root = tempfile::tempdir().unwrap();
        let store = Arc::new(crate::secure_store::MemoryStore::default());
        let state = AppState::new(
            root.path().to_path_buf(),
            store.clone(),
            crate::tests::noop(),
        );
        let target = binding("https://enroll.example");
        let previous = state.config().unwrap();
        let credential = crate::credentials::tests::credential_json(
            &target.origin,
            &target.vault_id,
            &target.device_id,
            crate::credentials::tests::TOKEN,
        );
        let marker = install_marker(&state, target.clone(), previous, &credential);
        simulate_partial_install(&state, &marker, b"different-credential");

        let error = rollback_computer_install(&state, &marker)
            .await
            .unwrap_err();

        assert_eq!(error.code, "computer-discard-ownership-unproven");
        assert!(target.data_dir(root.path()).exists());
        assert!(store.get(&target.credential_account()).unwrap().is_some());
    }

    #[derive(Default)]
    struct FailOneDeleteStore {
        inner: crate::secure_store::MemoryStore,
        failed: std::sync::atomic::AtomicBool,
        fail_account: Mutex<String>,
    }

    impl SecretStore for FailOneDeleteStore {
        fn get(&self, account: &str) -> BridgeResult<Option<Zeroizing<Vec<u8>>>> {
            self.inner.get(account)
        }

        fn set(&self, account: &str, secret: &[u8]) -> BridgeResult<()> {
            self.inner.set(account, secret)
        }

        fn delete(&self, account: &str) -> BridgeResult<()> {
            if account == self.fail_account.lock().unwrap().as_str()
                && !self.failed.swap(true, std::sync::atomic::Ordering::SeqCst)
            {
                return Err(BridgeError::new("secure-store-unavailable", "injected"));
            }
            self.inner.delete(account)
        }
    }

    #[tokio::test]
    async fn rollback_retry_converges_after_an_interrupted_secure_store_cleanup() {
        let root = tempfile::tempdir().unwrap();
        let store = Arc::new(FailOneDeleteStore::default());
        let state = AppState::new(
            root.path().to_path_buf(),
            store.clone(),
            crate::tests::noop(),
        );
        let target = binding("https://enroll.example");
        *store.fail_account.lock().unwrap() = target.db_key_account();
        let previous = state.config().unwrap();
        let credential = crate::credentials::tests::credential_json(
            &target.origin,
            &target.vault_id,
            &target.device_id,
            crate::credentials::tests::TOKEN,
        );
        let marker = install_marker(&state, target.clone(), previous.clone(), &credential);
        simulate_partial_install(&state, &marker, &credential);

        assert_eq!(
            rollback_computer_install(&state, &marker)
                .await
                .unwrap_err()
                .code,
            "secure-store-unavailable"
        );
        assert!(computer_install_rollback_path(&state).exists());
        assert!(!target.data_dir(root.path()).exists());

        rollback_computer_install(&state, &marker).await.unwrap();
        assert_eq!(state.config().unwrap(), previous);
        assert!(store.get(&target.db_key_account()).unwrap().is_none());
        assert!(!computer_install_rollback_path(&state).exists());
    }

    #[tokio::test]
    async fn rollback_retry_converges_after_files_were_already_removed() {
        let root = tempfile::tempdir().unwrap();
        let store = Arc::new(crate::secure_store::MemoryStore::default());
        let state = AppState::new(
            root.path().to_path_buf(),
            store.clone(),
            crate::tests::noop(),
        );
        let target = binding("https://enroll.example");
        let previous = state.config().unwrap();
        let credential = crate::credentials::tests::credential_json(
            &target.origin,
            &target.vault_id,
            &target.device_id,
            crate::credentials::tests::TOKEN,
        );
        let marker = install_marker(&state, target.clone(), previous.clone(), &credential);
        simulate_partial_install(&state, &marker, &credential);
        std::fs::remove_dir_all(&marker.data_dir).unwrap();
        store.delete(&marker.credential_account).unwrap();

        rollback_computer_install(&state, &marker).await.unwrap();
        discard_computer_claimant(&state).await.unwrap();

        assert_eq!(state.config().unwrap(), previous);
        assert!(!target.data_dir(root.path()).exists());
    }

    #[test]
    fn claimant_view_never_exposes_a_status_transport_secret() {
        let view = ComputerClaimantView {
            stage: "waiting_owner",
            invitation_id: "id".into(),
            qr_payload: Some("qr-only-payload".into()),
            awaiting_owner: true,
            state: "waiting",
            expires_in_seconds: 0,
            verification_code: None,
            error_code: None,
        };
        let value = serde_json::to_value(view).unwrap();
        assert!(value.get("claimToken").is_none());
        assert!(value.get("transferPrivateKey").is_none());
    }

    #[test]
    fn saved_locked_identity_blocks_new_claimant_without_touching_secure_storage() {
        let binding = crate::credentials::Binding {
            origin: "https://enroll.example".into(),
            vault_id: uuid::Uuid::new_v4().to_string(),
            device_id: uuid::Uuid::new_v4().to_string(),
        };
        let mut config = crate::credentials::HostConfig::default();
        config.remember(&binding);
        assert!(config_has_saved_identity(&config, "https://enroll.example"));
        assert!(!config_has_saved_identity(&config, "https://other.example"));
    }

    #[test]
    fn approval_or_a_retained_confirm_request_cannot_be_cancelled() {
        let waiting = ClaimantState {
            expected_binding: None,
            qr_payload: "qr".into(),
            view: ComputerClaimantView {
                stage: "waiting_owner",
                invitation_id: "id".into(),
                qr_payload: Some("qr".into()),
                awaiting_owner: true,
                state: "waiting",
                expires_in_seconds: 0,
                verification_code: None,
                error_code: None,
            },
            confirm_request: None,
            prepared_identity: None,
            install_baseline: crate::credentials::HostConfig::default(),
            install_rollback: None,
            database_key: None,
            installed: false,
        };
        assert!(claimant_cancellation_allowed(&waiting));
        let approved = ClaimantState {
            view: ComputerClaimantView {
                state: "approved",
                ..waiting.view.clone()
            },
            ..waiting
        };
        assert!(!claimant_cancellation_allowed(&approved));
    }

    #[test]
    fn expired_claimant_can_be_restarted_but_approved_claimant_cannot() {
        let claimant = ClaimantState {
            expected_binding: None,
            qr_payload: "qr".into(),
            view: ComputerClaimantView {
                stage: "expired",
                invitation_id: "id".into(),
                qr_payload: Some("qr".into()),
                awaiting_owner: false,
                state: "expired",
                expires_in_seconds: 0,
                verification_code: None,
                error_code: None,
            },
            confirm_request: None,
            prepared_identity: None,
            install_baseline: crate::credentials::HostConfig::default(),
            install_rollback: None,
            database_key: None,
            installed: false,
        };
        assert!(!claimant_must_finish(&claimant));
        assert!(claimant_must_finish(&ClaimantState {
            view: ComputerClaimantView {
                state: "approved",
                ..claimant.view.clone()
            },
            ..claimant
        }));
    }

    #[tokio::test]
    async fn hosted_account_header_is_sent_for_status_and_confirm_retries_keep_the_body() {
        let headers = Arc::new(Mutex::new(Vec::new()));
        let confirm_bodies = Arc::new(Mutex::new(Vec::new()));
        let app = Router::new()
            .route(
                "/v1/enrollment/computers/test/status",
                post({
                    let headers = headers.clone();
                    move |request_headers: HeaderMap| {
                        let headers = headers.clone();
                        async move {
                            headers.lock().unwrap().push(
                                request_headers
                                    .get("Peppy-Account-Session")
                                    .and_then(|value| value.to_str().ok())
                                    .unwrap_or_default()
                                    .to_owned(),
                            );
                            (StatusCode::OK, "{}")
                        }
                    }
                }),
            )
            .route(
                "/v1/enrollment/computers/test/confirm",
                post({
                    let headers = headers.clone();
                    let confirm_bodies = confirm_bodies.clone();
                    move |request_headers: HeaderMap, body: String| {
                        let headers = headers.clone();
                        let confirm_bodies = confirm_bodies.clone();
                        async move {
                            headers.lock().unwrap().push(
                                request_headers
                                    .get("Peppy-Account-Session")
                                    .and_then(|value| value.to_str().ok())
                                    .unwrap_or_default()
                                    .to_owned(),
                            );
                            confirm_bodies.lock().unwrap().push(body);
                            (StatusCode::OK, "{}")
                        }
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let status = post_computer_status(
            &origin,
            StatusRequest {
                path: "/v1/enrollment/computers/test/status".into(),
                body: serde_json::json!({"claim_token":"private"}),
            },
            Some("hosted-session"),
        )
        .await;
        assert_eq!(status.unwrap().as_deref(), Some("{}"));
        let request = Zeroizing::<String>::new("{\"claim_token\":\"private\"}".into());
        let _ = post_computer_confirm(&origin, "test", request.clone(), Some("hosted-session"))
            .await
            .unwrap();
        let _ = post_computer_confirm(&origin, "test", request, Some("hosted-session"))
            .await
            .unwrap();
        server.abort();

        assert_eq!(headers.lock().unwrap().as_slice(), ["hosted-session"; 3]);
        assert_eq!(
            confirm_bodies.lock().unwrap().as_slice(),
            ["{\"claim_token\":\"private\"}"; 2]
        );
    }
}
