//! Native-only enrollment orchestration. Secrets are collected by native dialogs and never cross IPC.
use crate::{
    credentials::{parse_credential, ImportedCredential},
    dialogs::{self, PassphrasePurpose},
    error::{BridgeError, BridgeResult},
    net::{read_bounded, NetError, MAX_JSON_BYTES},
    origin,
    secure_store::SecretStore,
    AppState,
};
use peppy_client_core::{KeyProfile, VaultCheckHeader};
use peppy_hosted_api::enrollment::{
    self, AdmissionChallenge, ClientKind, DeploymentMode, EnrollmentMethod, EnrollmentRole,
    EnrollmentState, InstanceInfo, InstanceLoginRequest, InvitationCreated, InvitationStatus,
};
use peppy_hosted_client::newenrollment::{EnrollmentClient, NativeEnrollmentIdentity};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::time::Duration;
use tauri::{AppHandle, State, WebviewWindow};
use zeroize::Zeroizing;

const DEVICE_NAME: &str = "Peppy Desktop";
const HOSTED_ORIGIN: &str = "https://peppy.pro";
const OWNER_CHECKPOINT_PREFIX: &str = "enrollment-owner:v1:";
const OWNER_SECRET_PREFIX: &str = "enrollment-owner-secret:v1:";
const VAULT_REQUEST_CHECKPOINT_PREFIX: &str = "enrollment-vault-request:v1:";
const VAULT_SECRET_PREFIX: &str = "enrollment-vault-secret:v1:";
const MAX_ERROR_BODY_BYTES: usize = 4 * 1024;

#[derive(Default)]
pub struct Controller {
    client: Option<EnrollmentClient>,
    origin: Option<String>,
    info: Option<InstanceInfo>,
    admin_session: Option<Zeroizing<String>>,
    ready: bool,
    owner: bool,
    /// `None` until the active session's gateway registry is known.
    has_gateway: Option<bool>,
    invitation_id: Option<uuid::Uuid>,
    invitation: Option<InvitationStatus>,
    invitation_method: Option<EnrollmentMethod>,
    owner_resume: bool,
    owner_claim_in_flight: usize,
    owner_passphrase: Option<Zeroizing<String>>,
    vault_request_state: Option<String>,
    vault_creation_in_flight: bool,
    operation_epoch: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EnrollmentView {
    mode: &'static str,
    stage: &'static str,
    origin: Option<String>,
    owner: Option<bool>,
    has_gateway: Option<bool>,
    qr_payload: Option<String>,
    verification_code: Option<String>,
    expires_in_seconds: Option<u32>,
    error_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    setup_code_required: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    vault_owner_required: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    display_name: Option<String>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    owner_resume_pending: bool,
}

impl Controller {
    fn view(&self) -> EnrollmentView {
        let mode = match self.info.as_ref().map(|info| info.mode) {
            Some(DeploymentMode::Hosted) => "hosted",
            _ => "community",
        };
        let pending_request = self
            .client
            .as_ref()
            .and_then(EnrollmentClient::pending_vault_request);
        let stage = match self.info.as_ref() {
            None => "server",
            Some(_)
                if self
                    .invitation
                    .as_ref()
                    .is_some_and(|status| status.state == EnrollmentState::Claimed) =>
            {
                "verify_phone"
            }
            Some(_)
                if self.invitation.as_ref().is_some_and(|status| {
                    matches!(
                        status.state,
                        EnrollmentState::Waiting | EnrollmentState::Approved
                    )
                }) =>
            {
                "waiting_phone"
            }
            Some(_)
                if self
                    .invitation
                    .as_ref()
                    .is_some_and(|status| status.state == EnrollmentState::Expired) =>
            {
                "expired"
            }
            Some(_) if self.ready => "ready",
            Some(_)
                if matches!(self.vault_request_state.as_deref(), Some("approved"))
                    || (matches!(self.vault_request_state.as_deref(), Some("completed"))
                        && self
                            .client
                            .as_ref()
                            .is_some_and(EnrollmentClient::has_prepared_owner_material)) =>
            {
                "invite_approved"
            }
            Some(_) if matches!(self.vault_request_state.as_deref(), Some("completed")) => "error",
            Some(_) if matches!(self.vault_request_state.as_deref(), Some("denied")) => {
                "invite_denied"
            }
            Some(_) if matches!(self.vault_request_state.as_deref(), Some("expired")) => {
                "invite_expired"
            }
            Some(_) if pending_request.is_some() => "invite_waiting",
            Some(info) if !info.claimed || info.setup_required => "setup",
            Some(_) if self.admin_session.is_some() => "locked",
            Some(_) => "signin",
        };
        let setup_code_required = self.info.as_ref().and_then(|info| {
            (stage == "setup" && info.mode == DeploymentMode::Community && !info.claimed)
                .then_some(info.setup_required)
        });
        EnrollmentView {
            mode,
            stage,
            origin: self.origin.clone(),
            owner: self.ready.then_some(self.owner),
            has_gateway: self.has_gateway,
            qr_payload: self
                .client
                .as_ref()
                .filter(|_| self.owner)
                .and_then(|client| client.get_qr_payload().ok()),
            verification_code: self.invitation.as_ref().and_then(|status| {
                self.client
                    .as_ref()
                    .and_then(|client| client.phone_sas(serde_json::to_string(status).ok()?).ok())
            }),
            expires_in_seconds: self
                .invitation
                .as_ref()
                .map(|status| status.expires_in_seconds),
            error_code: matches!(self.vault_request_state.as_deref(), Some("completed"))
                .then_some("request_completed_without_material".into()),
            setup_code_required,
            vault_owner_required: None,
            display_name: pending_request.map(|(_, _, display_name)| display_name),
            // On a claimed server the earlier attempt is offered from sign-in, so a claim won by
            // someone else never traps the device in setup.
            owner_resume_pending: matches!(stage, "setup" | "signin")
                && self
                    .info
                    .as_ref()
                    .is_some_and(|info| info.mode == DeploymentMode::Community)
                && self.owner_resume,
        }
    }

    fn origin(&self) -> BridgeResult<&str> {
        self.origin
            .as_deref()
            .ok_or_else(|| BridgeError::new("server-required", "Choose a server before enrolling."))
    }
}

/// Only an explicitly discovered, unclaimed community server without a setup requirement is open.
/// Missing, claimed, and non-community state all fail closed to a native setup-token prompt,
/// except resuming this device's own attempt on a claimed server: the server replays a matching
/// operation without checking the setup code.
fn setup_code_required(controller: &std::sync::Mutex<Controller>) -> BridgeResult<bool> {
    let controller = controller.lock().map_err(|_| BridgeError::host_state())?;
    Ok(!matches!(
        controller.info.as_ref(),
        Some(info)
            if info.mode == DeploymentMode::Community
                && ((!info.claimed && !info.setup_required)
                    || (info.claimed && controller.owner_resume))
    ))
}

/// Returns a client usable for joining an existing vault. A restored pending first-owner client
/// cannot join, so it is replaced in memory; its protected checkpoint stays in secure storage and
/// is offered again by the next discovery.
fn admission_client(controller: &mut Controller) -> BridgeResult<&mut EnrollmentClient> {
    let origin = controller.origin()?.to_owned();
    let pending_owner = controller
        .client
        .as_ref()
        .is_some_and(|client| client.is_pending_owner_creation_for_origin(&origin));
    if pending_owner || controller.client.is_none() {
        controller.client = Some(
            EnrollmentClient::new(origin, ClientKind::Desktop, DEVICE_NAME.into())
                .map_err(|_| enrollment_error())?,
        );
        controller.owner_resume = false;
        controller.owner_passphrase = None;
    }
    controller.client.as_mut().ok_or_else(enrollment_error)
}

/// A device identity already unlocked in this app for the selected origin.
enum ExistingIdentity {
    /// Owner identity; its protected vault header enables owner invitations.
    Owner(KeyProfile, VaultCheckHeader),
    /// Non-owner device identity; it is ready without any owner invitation client.
    Device,
}

impl ExistingIdentity {
    fn role(&self) -> EnrollmentRole {
        match self {
            Self::Owner(..) => EnrollmentRole::Owner,
            Self::Device => EnrollmentRole::Device,
        }
    }
}

/// Marks the controller ready for a verified identity. Only an owner role may invite phones.
fn set_identity_ready(controller: &mut Controller, role: Option<EnrollmentRole>) {
    controller.ready = role.is_some();
    controller.owner = role == Some(EnrollmentRole::Owner);
}

/// Reconcile controller state with the active unlocked identity after an activation-side refresh
/// has advanced the operation epoch. Only an owner identity may repair an owner setup view.
fn reconcile_status_core(controller: &mut Controller, identity: Option<EnrollmentRole>) {
    if !controller.ready && identity == Some(EnrollmentRole::Owner) {
        set_identity_ready(controller, identity);
    }
}

/// Chooses which restored client discovery resumes for a device without an identity. A pending
/// vault request wins: it is the newer intent (joining is only offered once a server is claimed),
/// and the owner checkpoint stays in secure storage. Returns whether an owner attempt is resumed.
fn choose_pending_client(
    pending_owner: Option<EnrollmentClient>,
    pending_vault_request: Option<EnrollmentClient>,
) -> (Option<EnrollmentClient>, bool) {
    match (pending_vault_request, pending_owner) {
        (Some(request), _) => (Some(request), false),
        (None, Some(owner)) => (Some(owner), true),
        (None, None) => (None, false),
    }
}

/// A pending owner checkpoint stays resumable even after the server reports a claim: the claim may
/// be this device's own lost-response attempt, which the server replays for the same operation and
/// admin password.
fn can_restore_owner_checkpoint(mode: DeploymentMode, has_identity: bool) -> bool {
    mode == DeploymentMode::Community && !has_identity
}

/// Known registry only: an unknown registry yields `None`, never a fabricated `false`.
fn known_gateway_presence((gateways, known): (Vec<crate::dto::GatewayView>, bool)) -> Option<bool> {
    known.then_some(!gateways.is_empty())
}

/// Reads gateway presence from the active session for `origin`. No network request is made.
async fn gateway_presence(state: &AppState, origin: &str) -> BridgeResult<Option<bool>> {
    if state
        .config()?
        .active_binding()
        .is_none_or(|binding| binding.origin != origin)
    {
        return Ok(None);
    }
    let Some(session) = state.session().await? else {
        return Ok(None);
    };
    if session.binding.origin != origin {
        return Ok(None);
    }
    Ok(known_gateway_presence(session.gateways()))
}

/// Refreshes presence for the controller's current origin. The controller lock is not held.
async fn refresh_gateway_presence(state: &AppState) -> BridgeResult<Option<bool>> {
    let origin = state
        .enrollment
        .lock()
        .map_err(|_| BridgeError::host_state())?
        .origin
        .clone();
    match origin {
        Some(origin) => gateway_presence(state, &origin).await,
        None => Ok(None),
    }
}

fn owner_checkpoint_key(origin: &str) -> String {
    format!("{OWNER_CHECKPOINT_PREFIX}{origin}")
}

fn pending_secret_key(prefix: &str, origin: &str) -> String {
    let digest = Sha256::digest(origin.as_bytes());
    format!("{prefix}{digest:x}")
}

fn owner_secret_key(origin: &str) -> String {
    pending_secret_key(OWNER_SECRET_PREFIX, origin)
}

fn vault_secret_key(origin: &str) -> String {
    pending_secret_key(VAULT_SECRET_PREFIX, origin)
}

fn read_text_secret(
    store: &dyn SecretStore,
    account: &str,
) -> BridgeResult<Option<Zeroizing<String>>> {
    let Some(value) = store.get(account)?.filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    String::from_utf8(value.to_vec())
        .map(Zeroizing::new)
        .map(Some)
        .map_err(|_| enrollment_error())
}

fn tombstone_pending_secrets(store: &dyn SecretStore, origin: &str) -> BridgeResult<()> {
    store.set(&owner_secret_key(origin), b"")?;
    store.set(&vault_secret_key(origin), b"")
}

fn tombstone_if_present(store: &dyn SecretStore, account: &str) -> BridgeResult<()> {
    if store.get(account)?.is_some_and(|value| !value.is_empty()) {
        store.set(account, b"")?;
    }
    Ok(())
}

fn vault_request_checkpoint_key(origin: &str) -> String {
    let digest = Sha256::digest(origin.as_bytes());
    format!("{VAULT_REQUEST_CHECKPOINT_PREFIX}{digest:x}")
}

fn tombstone_vault_request_checkpoint(store: &dyn SecretStore, origin: &str) -> BridgeResult<()> {
    store.set(&vault_request_checkpoint_key(origin), b"")
}

fn definitive_vault_request_error(code: &str) -> bool {
    matches!(
        code,
        "invite_invalid" | "request_not_found" | "invalid_username" | "username_taken"
    )
}

/// Clears the vault request while the caller already holds the controller lock. Callers must
/// have verified `operation_epoch` under that same guard so a stale finish cannot wipe a newer
/// request or its checkpoint.
fn clear_vault_request_locked(
    controller: &mut Controller,
    store: &dyn SecretStore,
    origin: &str,
) -> BridgeResult<()> {
    controller
        .client
        .as_mut()
        .map(EnrollmentClient::clear_vault_request);
    controller.vault_request_state = None;
    controller.vault_creation_in_flight = false;
    tombstone_vault_request_checkpoint(store, origin)?;
    store.set(&vault_secret_key(origin), b"")
}

/// Resubmits an unaccepted vault request. Definitive rejections finish the request as expired;
/// transient failures keep the persisted checkpoint so the same submission can be retried.
async fn resubmit_unaccepted_vault_request<Submit, Fut>(
    enrollment: &std::sync::Mutex<Controller>,
    store: &dyn SecretStore,
    epoch: u64,
    origin: &str,
    request: String,
    checkpoint: &[u8],
    submit: Submit,
) -> BridgeResult<Option<EnrollmentView>>
where
    Submit: FnOnce(String) -> Fut,
    Fut: std::future::Future<Output = BridgeResult<String>>,
{
    {
        let controller = enrollment.lock().map_err(|_| BridgeError::host_state())?;
        if controller.operation_epoch != epoch {
            return Ok(Some(controller.view()));
        }
        store.set(&vault_request_checkpoint_key(origin), checkpoint)?;
    }
    let created = match submit(request).await {
        Ok(created) => created,
        Err(error) if definitive_vault_request_error(error.code) => {
            let mut controller = enrollment.lock().map_err(|_| BridgeError::host_state())?;
            if controller.operation_epoch != epoch {
                return Ok(Some(controller.view()));
            }
            clear_vault_request_locked(&mut controller, store, origin)?;
            controller.vault_request_state = Some("expired".into());
            return Ok(Some(controller.view()));
        }
        Err(error) => return Err(error),
    };
    let controller = enrollment.lock().map_err(|_| BridgeError::host_state())?;
    if controller.operation_epoch != epoch {
        return Ok(Some(controller.view()));
    }
    let client = controller.client.as_ref().ok_or_else(enrollment_error)?;
    client
        .accept_vault_request_created(created)
        .map_err(|_| enrollment_error())?;
    let checkpoint = client.checkpoint().map_err(|_| enrollment_error())?;
    store.set(&vault_request_checkpoint_key(origin), &checkpoint)?;
    Ok(None)
}

fn restore_pending_vault_request(
    store: &dyn SecretStore,
    origin: &str,
) -> BridgeResult<Option<EnrollmentClient>> {
    let Some(bytes) = store
        .get(&vault_request_checkpoint_key(origin))?
        .filter(|bytes| !bytes.is_empty())
    else {
        return Ok(None);
    };
    match EnrollmentClient::restore(bytes.to_vec())
        .ok()
        .filter(|client| {
            client
                .pending_vault_request()
                .is_some_and(|(request_origin, _, _)| request_origin == origin)
        }) {
        Some(client) => Ok(Some(client)),
        None => {
            tombstone_vault_request_checkpoint(store, origin)?;
            Ok(None)
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsernameInput {
    username: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VaultOwnerInput {
    vault_owner: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BeginVaultRequestInput {
    invite_link: String,
    display_name: String,
}

fn tombstone_owner_checkpoint(store: &dyn SecretStore, origin: &str) -> BridgeResult<()> {
    store.set(&owner_checkpoint_key(origin), b"")
}

fn restore_pending_owner(
    store: &dyn SecretStore,
    origin: &str,
) -> BridgeResult<Option<EnrollmentClient>> {
    let Some(bytes) = store
        .get(&owner_checkpoint_key(origin))?
        .filter(|bytes| !bytes.is_empty())
    else {
        return Ok(None);
    };
    let restored = EnrollmentClient::restore(bytes.to_vec()).ok();
    match restored {
        Some(client) if client.is_pending_owner_creation_for_origin(origin) => Ok(Some(client)),
        _ => {
            tombstone_owner_checkpoint(store, origin)?;
            Ok(None)
        }
    }
}

fn begin_owner_operation(controller: &std::sync::Mutex<Controller>) -> BridgeResult<u64> {
    let mut controller = controller.lock().map_err(|_| BridgeError::host_state())?;
    controller.operation_epoch = controller.operation_epoch.wrapping_add(1);
    Ok(controller.operation_epoch)
}

fn cancel_owner_operation(controller: &std::sync::Mutex<Controller>) -> BridgeResult<u64> {
    begin_owner_operation(controller)
}

fn owner_operation_current(
    controller: &std::sync::Mutex<Controller>,
    epoch: u64,
) -> BridgeResult<bool> {
    Ok(controller
        .lock()
        .map_err(|_| BridgeError::host_state())?
        .operation_epoch
        == epoch)
}

async fn collect_new_admin_password<P, PF>(
    controller: &std::sync::Mutex<Controller>,
    epoch: u64,
    mut prompt: P,
) -> BridgeResult<Option<Zeroizing<String>>>
where
    P: FnMut(PassphrasePurpose) -> PF,
    PF: std::future::Future<Output = BridgeResult<Option<Zeroizing<String>>>>,
{
    let Some(mut password) = prompt(PassphrasePurpose::AdminPasswordCreate).await? else {
        return Ok(None);
    };
    if !owner_operation_current(controller, epoch)? {
        return Ok(None);
    }

    loop {
        if !(12..=1024).contains(&password.len()) {
            return Err(BridgeError::new(
                "admin-password-length",
                "Administrator password must be between 12 and 1024 bytes.",
            ));
        }
        let Some(confirm) = prompt(PassphrasePurpose::AdminPasswordConfirm).await? else {
            return Ok(None);
        };
        if !owner_operation_current(controller, epoch)? {
            return Ok(None);
        }
        if password == confirm {
            return Ok(Some(password));
        }

        let Some(replacement) = prompt(PassphrasePurpose::AdminPasswordMismatch).await? else {
            return Ok(None);
        };
        if !owner_operation_current(controller, epoch)? {
            return Ok(None);
        }
        password = replacement;
    }
}

fn wrong_owner_passphrase() -> BridgeError {
    BridgeError::new(
        "wrong-passphrase",
        "That vault shared secret does not match the pending vault.",
    )
}

fn vault_secret_mismatch() -> BridgeError {
    BridgeError::new(
        "vault-secret-mismatch",
        "The stored vault shared secret does not match the pending vault.",
    )
}

struct OwnerClaimGuard<'a>(&'a std::sync::Mutex<Controller>);

impl Drop for OwnerClaimGuard<'_> {
    fn drop(&mut self) {
        if let Ok(mut controller) = self.0.lock() {
            controller.owner_claim_in_flight = controller.owner_claim_in_flight.saturating_sub(1);
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum OwnerCreation {
    Cancelled,
    Activated { superseded: bool },
}

#[allow(clippy::too_many_arguments)]
async fn create_owner_core<P, PF, G, H, HF, A, AF>(
    controller: &std::sync::Mutex<Controller>,
    store: &dyn SecretStore,
    expected_binding: Option<crate::credentials::Binding>,
    username: String,
    mut prompt: P,
    generate: G,
    post_claim: H,
    activate: A,
) -> BridgeResult<OwnerCreation>
where
    P: FnMut(PassphrasePurpose) -> PF,
    PF: std::future::Future<Output = BridgeResult<Option<Zeroizing<String>>>>,
    G: FnOnce() -> Zeroizing<String>,
    H: FnOnce(String, String) -> HF,
    HF: std::future::Future<Output = BridgeResult<String>>,
    A: FnOnce(
        NativeEnrollmentIdentity,
        Zeroizing<String>,
        Option<crate::credentials::Binding>,
    ) -> AF,
    AF: std::future::Future<Output = BridgeResult<()>>,
{
    let epoch = begin_owner_operation(controller)?;
    let setup_token = if setup_code_required(controller)? {
        let Some(setup_token) = prompt(PassphrasePurpose::SetupToken).await? else {
            return Ok(OwnerCreation::Cancelled);
        };
        setup_token
    } else {
        Zeroizing::new(String::new())
    };
    if !owner_operation_current(controller, epoch)? {
        return Ok(OwnerCreation::Cancelled);
    }
    let Some(admin_password) = collect_new_admin_password(controller, epoch, &mut prompt).await?
    else {
        return Ok(OwnerCreation::Cancelled);
    };

    let (retained, resume, origin_for_secret) = {
        let controller = controller.lock().map_err(|_| BridgeError::host_state())?;
        (
            controller.owner_passphrase.clone(),
            controller.owner_resume,
            controller.origin()?.to_owned(),
        )
    };
    let mut generated_secret = false;
    let passphrase = if let Some(passphrase) = retained {
        passphrase
    } else if let Some(passphrase) = read_text_secret(store, &owner_secret_key(&origin_for_secret))?
    {
        passphrase
    } else if resume {
        let Some(passphrase) = prompt(PassphrasePurpose::ResumeOwnerSecret).await? else {
            return Ok(OwnerCreation::Cancelled);
        };
        passphrase
    } else {
        generated_secret = true;
        generate()
    };
    if !owner_operation_current(controller, epoch)? {
        return Ok(OwnerCreation::Cancelled);
    }

    let (origin, request, checkpoint) = {
        let mut controller = controller.lock().map_err(|_| BridgeError::host_state())?;
        if controller.operation_epoch != epoch {
            return Ok(OwnerCreation::Cancelled);
        }
        let origin = controller.origin()?.to_owned();
        let client = controller.client.as_mut().ok_or_else(enrollment_error)?;
        if client.is_pending_owner_creation_for_origin(&origin)
            && !client
                .owner_passphrase_matches(&passphrase)
                .map_err(|_| enrollment_error())?
        {
            return Err(wrong_owner_passphrase());
        }
        let request = client
            .create_owner_request(
                setup_token.to_string(),
                username,
                admin_password.to_string(),
                passphrase.to_string(),
            )
            .map_err(|_| enrollment_error())?;
        let checkpoint = Zeroizing::new(client.checkpoint().map_err(|_| enrollment_error())?);
        controller.owner_passphrase = Some(Zeroizing::new(passphrase.to_string()));
        controller.owner_resume = false;
        (origin, request, checkpoint)
    };
    if generated_secret {
        store.set(&owner_secret_key(&origin), passphrase.as_bytes())?;
    }
    store.set(&owner_checkpoint_key(&origin), &checkpoint)?;
    if !owner_operation_current(controller, epoch)? {
        return Ok(OwnerCreation::Cancelled);
    }

    let claim_guard = {
        let mut state = controller.lock().map_err(|_| BridgeError::host_state())?;
        if state.operation_epoch != epoch {
            return Ok(OwnerCreation::Cancelled);
        }
        state.owner_claim_in_flight += 1;
        OwnerClaimGuard(controller)
    };
    let response = post_claim(origin.clone(), request).await?;
    drop(claim_guard);
    let identity = {
        let controller = controller.lock().map_err(|_| BridgeError::host_state())?;
        if controller.operation_epoch != epoch {
            return Ok(OwnerCreation::Cancelled);
        }
        controller
            .client
            .as_ref()
            .ok_or_else(enrollment_error)?
            .finish_owner_admission(response, passphrase.to_string())
            .map_err(|_| enrollment_error())?
    };
    activate(identity, passphrase, expected_binding).await?;
    {
        let mut controller = controller.lock().map_err(|_| BridgeError::host_state())?;
        if controller.origin.as_deref() == Some(origin.as_str()) {
            controller.owner_passphrase = None;
            controller.owner_resume = false;
            controller.ready = true;
            controller.owner = true;
        }
    }
    // Activation is the durable commit point. Clear recovery material before checking the UI
    // epoch so a concurrent refresh cannot leave a completed owner claim resumable.
    // The pending secret is cleared by `activate` only after the binding copy is stored.
    tombstone_owner_checkpoint(store, &origin)?;
    if !owner_operation_current(controller, epoch)? {
        return Ok(OwnerCreation::Activated { superseded: true });
    }
    let controller = controller.lock().map_err(|_| BridgeError::host_state())?;
    if controller.operation_epoch != epoch {
        return Ok(OwnerCreation::Activated { superseded: true });
    }
    Ok(OwnerCreation::Activated { superseded: false })
}

fn enrollment_error() -> BridgeError {
    BridgeError::new(
        "enrollment",
        "The enrollment response was invalid or no longer matches this device.",
    )
}

async fn start_over_core<F, FF>(
    controller: &std::sync::Mutex<Controller>,
    store: &dyn SecretStore,
    fetch_instance: F,
) -> BridgeResult<EnrollmentView>
where
    F: FnOnce(String) -> FF,
    FF: std::future::Future<Output = BridgeResult<InstanceInfo>>,
{
    let (epoch, origin) = {
        let mut controller = controller.lock().map_err(|_| BridgeError::host_state())?;
        let origin = controller.origin()?.to_owned();
        if controller.owner_claim_in_flight > 0 {
            return Err(BridgeError::new(
                "operation_conflict",
                "The owner claim is still in progress.",
            ));
        }
        if !controller.owner_resume
            || !controller
                .client
                .as_ref()
                .is_some_and(|client| client.is_pending_owner_creation_for_origin(&origin))
        {
            return Ok(controller.view());
        }
        controller.operation_epoch = controller.operation_epoch.wrapping_add(1);
        (controller.operation_epoch, origin)
    };
    let info = fetch_instance(origin.clone()).await?;
    if info.claimed {
        return Err(BridgeError::new(
            "instance_already_claimed",
            "The server was already claimed.",
        ));
    }
    let mut controller = controller.lock().map_err(|_| BridgeError::host_state())?;
    if controller.operation_epoch != epoch {
        return Ok(controller.view());
    }
    if info.mode != DeploymentMode::Community {
        return Ok(controller.view());
    }
    let fresh_client =
        EnrollmentClient::new(origin.clone(), ClientKind::Desktop, DEVICE_NAME.into())
            .map_err(|_| enrollment_error())?;
    tombstone_owner_checkpoint(store, &origin)?;
    tombstone_pending_secrets(store, &origin)?;
    controller.owner_passphrase = None;
    controller.owner_resume = false;
    controller.info = Some(info);
    controller.client = Some(fresh_client);
    Ok(controller.view())
}

async fn fetch_instance(origin: String) -> BridgeResult<InstanceInfo> {
    serde_json::from_str(
        &body(
            http(&origin)?
                .get(format!("{origin}{}", enrollment::routes::INSTANCE))
                .send()
                .await
                .map_err(|_| NetError::Offline)?,
        )
        .await?,
    )
    .map_err(|_| enrollment_error())
}

fn http(origin: &str) -> BridgeResult<reqwest::Client> {
    crate::tls::http_builder(origin)?
        .redirect(reqwest::redirect::Policy::none())
        .https_only(!origin::is_loopback_http(origin))
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(20))
        .user_agent("Peppy-Desktop")
        .build()
        .map_err(|_| BridgeError::new("network", "Could not create the native network client."))
}

async fn body(response: reqwest::Response) -> BridgeResult<String> {
    if !response.status().is_success() {
        let error_code = read_bounded(response, MAX_ERROR_BODY_BYTES)
            .await
            .ok()
            .and_then(|body| known_claim_error_code(&body));
        return Err(BridgeError::new(
            error_code.unwrap_or("server-rejected"),
            "The server rejected the request.",
        ));
    }
    String::from_utf8(read_bounded(response, MAX_JSON_BYTES).await?).map_err(|_| enrollment_error())
}

fn known_claim_error_code(body: &[u8]) -> Option<&'static str> {
    match serde_json::from_slice::<serde_json::Value>(body)
        .ok()?
        .get("code")?
        .as_str()?
    {
        "invalid_setup_token" => Some("invalid_setup_token"),
        "instance_already_claimed" => Some("instance_already_claimed"),
        "invalid_username" => Some("invalid_username"),
        "username_taken" => Some("username_taken"),
        "vault_owner_required" => Some("vault_owner_required"),
        "vault_not_found" => Some("vault_not_found"),
        "invite_invalid" => Some("invite_invalid"),
        "request_not_found" => Some("request_not_found"),
        "request_not_approved" => Some("request_not_approved"),
        "operation_conflict" => Some("operation_conflict"),
        "active_invitation_limit" => Some("active_invitation_limit"),
        _ => None,
    }
}

async fn post(
    client: &reqwest::Client,
    origin: &str,
    path: &str,
    request: String,
    session: Option<&str>,
    account_session: Option<&str>,
) -> BridgeResult<String> {
    let mut call = client
        .post(format!("{origin}{path}"))
        .header("content-type", "application/json")
        .body(request);
    if let Some(session) = session {
        call = call.header("Peppy-Instance-Session", session);
    }
    if let Some(account_session) = account_session {
        call = call.header("Peppy-Account-Session", account_session);
    }
    body(call.send().await.map_err(|_| NetError::Offline)?).await
}

async fn hosted_account_header(
    state: &AppState,
    hosted: &crate::hosted::HostedState,
) -> BridgeResult<Option<Zeroizing<String>>> {
    let hosted_mode = state
        .enrollment
        .lock()
        .map_err(|_| BridgeError::host_state())?
        .info
        .as_ref()
        .is_some_and(|info| info.mode == DeploymentMode::Hosted);
    if !hosted_mode {
        return Ok(None);
    }
    let origin = state
        .enrollment
        .lock()
        .map_err(|_| BridgeError::host_state())?
        .origin()?
        .to_owned();
    Ok(Some(
        crate::hosted::enrollment_account_session(state, hosted, &origin).await?,
    ))
}

fn device_request(
    session: &crate::session::Session,
    origin: &str,
    request: reqwest::RequestBuilder,
    account_session: Option<&str>,
) -> BridgeResult<reqwest::RequestBuilder> {
    if session.api.origin() != origin {
        return Err(BridgeError::new(
            "origin-binding",
            "A device credential is never sent to another server.",
        ));
    }
    let mut request = request.bearer_auth(session.api.bearer());
    if let Some(account_session) = account_session {
        request = request.header("Peppy-Account-Session", account_session);
    }
    Ok(request)
}

async fn install_identity(
    app: &AppHandle,
    state: &AppState,
    identity: NativeEnrollmentIdentity,
    passphrase: Zeroizing<String>,
    expected_binding: Option<crate::credentials::Binding>,
) -> BridgeResult<()> {
    let metadata = identity.metadata().clone();
    let origin = identity.origin().to_owned();
    let vault_secret = passphrase.clone();
    let credential = identity.device_credential_for_native_secure_store();
    let portable = Zeroizing::new(
        serde_json::to_vec(&ImportedCredential {
            version: 1,
            origin: identity.origin().to_owned(),
            vault_id: metadata.vault_id.to_string(),
            device_id: metadata.device_id.to_string(),
            device_token: credential,
        })
        .map_err(|_| enrollment_error())?,
    );
    let credential = std::sync::Arc::new(parse_credential(&portable)?);
    drop(portable);
    let _import = state.import_lock.lock().await;
    let previous = state.config()?;
    if previous.active_binding() != expected_binding.as_ref() {
        return Err(BridgeError::new(
            "credential-changed",
            "The active credential changed while enrollment was in progress.",
        ));
    }
    let permit = if previous.active_binding() != Some(&credential.binding()) {
        Some(crate::lifecycle::prepare_switch(app).await?)
    } else {
        None
    };
    let result = async {
        state.update_config(|config| config.select_origin(&credential.origin))?;
        crate::verify_import(state, &credential).await?;
        crate::activate_import_locked(state, credential).await
    }
    .await;
    if let Err(error) = result {
        let rollback = previous;
        let _ = state.update_config(|config| *config = rollback);
        return Err(error);
    }
    if let Some(permit) = permit {
        permit.finish().await?;
    }
    (state.notifier)();
    let session = state.require_session().await?;
    if let Some(activation) = identity.into_activation() {
        let activation_session = session.clone();
        crate::sync::blocking(move || {
            activation
                .activate(&activation_session.client)
                .map_err(|_| enrollment_error())
        })
        .await?;
    } else {
        let fingerprint = metadata
            .profile
            .fingerprint()
            .map_err(|_| enrollment_error())?;
        crate::unlock_with(
            state,
            &session,
            metadata.profile,
            metadata.header,
            passphrase,
            fingerprint,
        )
        .await?;
    }
    persist_binding_secret(
        &*state.store,
        &session.binding.vault_secret_account(),
        &vault_secret,
        &origin,
    );
    Ok(())
}

/// Stores the verified vault secret for its device binding, then clears the origin's pending
/// copies. Activation has already committed, so failures are tolerated: a retained pending copy
/// is moved by the next discovery sweep instead of turning a completed enrollment into an error.
fn persist_binding_secret(
    store: &dyn SecretStore,
    binding_account: &str,
    secret: &str,
    origin: &str,
) {
    if store.set(binding_account, secret.as_bytes()).is_ok() {
        for pending_account in [owner_secret_key(origin), vault_secret_key(origin)] {
            if read_text_secret(store, &pending_account)
                .ok()
                .flatten()
                .is_some_and(|pending| pending.as_str() == secret)
            {
                let _ = tombstone_if_present(store, &pending_account);
            }
        }
    }
}

/// Moves a pending secret left by an interrupted post-activation write into the empty binding
/// account. A non-empty binding account is authoritative and is never overwritten here.
fn sweep_pending_secret(
    store: &dyn SecretStore,
    origin: &str,
    binding_account: &str,
    verify: impl Fn(&str) -> bool,
) -> BridgeResult<()> {
    if let Some(binding) = read_text_secret(store, binding_account)? {
        for pending_account in [owner_secret_key(origin), vault_secret_key(origin)] {
            if read_text_secret(store, &pending_account)?
                .is_some_and(|pending| pending.as_str() == binding.as_str())
            {
                let _ = tombstone_if_present(store, &pending_account);
            }
        }
        return Ok(());
    }
    for pending_account in [owner_secret_key(origin), vault_secret_key(origin)] {
        if let Some(secret) = read_text_secret(store, &pending_account)? {
            if verify(secret.as_str()) && store.set(binding_account, secret.as_bytes()).is_ok() {
                let _ = tombstone_if_present(store, &pending_account);
                break;
            }
        }
    }
    Ok(())
}

fn vault_secret_unavailable() -> BridgeError {
    BridgeError::new(
        "vault-secret-unavailable",
        "The vault shared secret is not stored on this device.",
    )
}

/// Reads the binding's stored vault secret. An absent or tombstoned value is unavailable.
fn stored_vault_secret(
    store: &dyn SecretStore,
    binding_account: &str,
) -> BridgeResult<Zeroizing<String>> {
    read_text_secret(store, binding_account)?.ok_or_else(vault_secret_unavailable)
}

/// Persists a phrase only after native unlock verification and only when this binding has none.
pub(crate) fn save_verified_vault_secret_if_empty(
    store: &dyn SecretStore,
    binding_account: &str,
    passphrase: &str,
) -> BridgeResult<()> {
    if read_text_secret(store, binding_account)?.is_none() {
        store.set(binding_account, passphrase.as_bytes())?;
    }
    Ok(())
}

#[tauri::command]
pub async fn enrollment_discover(
    window: WebviewWindow,
    state: State<'_, AppState>,
    origin: Option<String>,
) -> BridgeResult<EnrollmentView> {
    crate::require_main(window.label())?;
    let selected = match origin {
        Some(value) => origin::validate_origin(&value)?,
        None => state
            .config()?
            .origin
            .ok_or_else(|| BridgeError::new("server-required", "Enter a server URL first."))?,
    };
    let discovery_epoch = cancel_owner_operation(&state.enrollment)?;
    let discovery_path = if selected == HOSTED_ORIGIN {
        enrollment::routes::HOSTED_ENROLLMENT_CONFIG
    } else {
        enrollment::routes::INSTANCE
    };
    let response = if discovery_path == enrollment::routes::INSTANCE {
        fetch_instance(selected.clone()).await?
    } else {
        serde_json::from_str(
            &body(
                http(&selected)?
                    .get(format!("{selected}{discovery_path}"))
                    .send()
                    .await
                    .map_err(|_| NetError::Offline)?,
            )
            .await?,
        )
        .map_err(|_| enrollment_error())?
    };
    if response.mode == DeploymentMode::Hosted && selected != HOSTED_ORIGIN {
        return Err(BridgeError::new(
            "invalid-origin",
            "Hosted enrollment is only available at Peppy Hosted.",
        ));
    }
    let existing = existing_identity(&state, &selected).await?;
    let existing_role = existing.as_ref().map(ExistingIdentity::role);
    if existing_role == Some(EnrollmentRole::Owner) {
        let _ = tombstone_if_present(&*state.store, &owner_checkpoint_key(&selected));
        if let Some(session) = state.session().await? {
            if session.binding.origin == selected {
                let verify = |secret: &str| {
                    matches!(
                        existing.as_ref(),
                        Some(ExistingIdentity::Owner(profile, header))
                            if EnrollmentClient::for_existing_owner(
                                selected.clone(),
                                ClientKind::Desktop,
                                DEVICE_NAME.into(),
                                profile.clone(),
                                header.clone(),
                            )
                            .and_then(|client| client.owner_passphrase_matches(secret))
                            .unwrap_or(false)
                    )
                };
                let _ = sweep_pending_secret(
                    &*state.store,
                    &selected,
                    &session.binding.vault_secret_account(),
                    verify,
                );
            }
        }
    }
    let has_gateway = gateway_presence(&state, &selected).await?;
    let can_resume_pending = can_restore_owner_checkpoint(response.mode, existing.is_some());
    let pending_owner = if can_resume_pending {
        restore_pending_owner(&*state.store, &selected)?
    } else {
        None
    };
    if can_resume_pending && pending_owner.is_none() {
        // A secret without its resume checkpoint cannot authorize a replay. Treat it as an
        // interrupted pre-checkpoint write and remove it on the next discovery.
        let _ = tombstone_if_present(&*state.store, &owner_secret_key(&selected));
    }
    let pending_vault_request = if response.mode == DeploymentMode::Community && existing.is_none()
    {
        restore_pending_vault_request(&*state.store, &selected)?
    } else {
        None
    };
    let (pending_client, resumed_pending) =
        choose_pending_client(pending_owner, pending_vault_request);
    // An existing device stays ready with its own credential; only an owner gets an invitation client.
    let client = match (existing, pending_client) {
        (Some(ExistingIdentity::Owner(profile, header)), _) => Some(
            EnrollmentClient::for_existing_owner(
                selected.clone(),
                ClientKind::Desktop,
                DEVICE_NAME.into(),
                profile,
                header,
            )
            .map_err(|_| enrollment_error())?,
        ),
        (Some(ExistingIdentity::Device), _) => None,
        (None, Some(client)) => Some(client),
        (None, None) => Some(
            EnrollmentClient::new(selected.clone(), ClientKind::Desktop, DEVICE_NAME.into())
                .map_err(|_| enrollment_error())?,
        ),
    };

    let mut controller = state
        .enrollment
        .lock()
        .map_err(|_| BridgeError::host_state())?;
    if controller.operation_epoch != discovery_epoch {
        return Ok(controller.view());
    }
    apply_discovery(
        &mut controller,
        selected,
        response,
        existing_role,
        client,
        resumed_pending,
        has_gateway,
    );
    Ok(controller.view())
}

#[tauri::command]
pub async fn enrollment_start_over(
    window: WebviewWindow,
    state: State<'_, AppState>,
) -> BridgeResult<EnrollmentView> {
    crate::require_main(window.label())?;
    start_over_core(&state.enrollment, &*state.store, fetch_instance).await
}

/// Applies a discovery result. An existing device keeps its client when the origin is unchanged.
fn apply_discovery(
    controller: &mut Controller,
    selected: String,
    response: InstanceInfo,
    existing_role: Option<EnrollmentRole>,
    client: Option<EnrollmentClient>,
    resumed_pending: bool,
    has_gateway: Option<bool>,
) {
    let same_origin = controller.origin.as_deref() == Some(selected.as_str());
    controller.origin = Some(selected);
    controller.info = Some(response);
    match client {
        Some(client) => controller.client = Some(client),
        None if !same_origin => controller.client = None,
        None => {}
    }
    controller.owner_resume = resumed_pending;
    controller.owner_passphrase = None;
    controller.vault_request_state = controller
        .client
        .as_ref()
        .and_then(EnrollmentClient::pending_vault_request)
        .map(|_| "pending".into());
    controller.vault_creation_in_flight = controller
        .client
        .as_ref()
        .is_some_and(EnrollmentClient::has_prepared_owner_material);
    controller.has_gateway = has_gateway;
    set_identity_ready(controller, existing_role);
}

/// Returns the unlocked identity for `selected`, if this app already holds one.
async fn existing_identity(
    state: &AppState,
    selected: &str,
) -> BridgeResult<Option<ExistingIdentity>> {
    if state
        .config()?
        .active_binding()
        .is_none_or(|binding| binding.origin != selected)
    {
        return Ok(None);
    }
    let Some(session) = state.session().await? else {
        return Ok(None);
    };
    let role = session
        .status
        .lock()
        .map_err(|_| BridgeError::host_state())?
        .device_role
        .clone();
    let unlocked = crate::sync::blocking({
        let session = session.clone();
        move || {
            session
                .client
                .key_status()
                .map(|status| status.active_epoch.is_some())
                .map_err(crate::core_error)
        }
    })
    .await?;
    if !unlocked || session.binding.origin != selected {
        return Ok(None);
    }
    match role.as_deref() {
        Some("owner") => {
            let vault = crate::fetch_vault(
                &session.api,
                &session.binding.vault_id,
                &session.binding.device_id,
            )
            .await?;
            let (profile, header) = crate::vault_header(&vault)?;
            Ok(Some(ExistingIdentity::Owner(profile, header)))
        }
        Some("device") => Ok(Some(ExistingIdentity::Device)),
        _ => Ok(None),
    }
}

#[tauri::command]
pub async fn enrollment_create_owner(
    window: WebviewWindow,
    app: AppHandle,
    state: State<'_, AppState>,
    hosted: State<'_, crate::hosted::HostedState>,
    input: Option<UsernameInput>,
) -> BridgeResult<EnrollmentView> {
    crate::require_main(window.label())?;
    if state
        .enrollment
        .lock()
        .map_err(|_| BridgeError::host_state())?
        .info
        .as_ref()
        .is_some_and(|info| info.mode == DeploymentMode::Hosted)
    {
        crate::hosted::provision_enrollment(&app, &state, &hosted).await?;
        let has_gateway = refresh_gateway_presence(&state).await?;
        let mut controller = state
            .enrollment
            .lock()
            .map_err(|_| BridgeError::host_state())?;
        controller.ready = true;
        controller.owner = true;
        controller.has_gateway = has_gateway;
        return Ok(controller.view());
    }

    let expected_binding = state.config()?.active_binding().cloned();
    let app_ref = &app;
    let state_ref = &*state;
    let completed = create_owner_core(
        &state_ref.enrollment,
        &*state_ref.store,
        expected_binding,
        input.and_then(|value| value.username).unwrap_or_default(),
        |purpose| dialogs::passphrase(app_ref, purpose),
        || Zeroizing::new(peppy_crypto::passphrase::generate_passphrase()),
        |origin, request| async move {
            post(
                &http(&origin)?,
                &origin,
                enrollment::routes::INSTANCE_CLAIM,
                request,
                None,
                None,
            )
            .await
        },
        |identity, passphrase, expected_binding| async move {
            install_identity(app_ref, state_ref, identity, passphrase, expected_binding).await
        },
    )
    .await;
    let outcome = match completed {
        Ok(outcome) => outcome,
        Err(error)
            if matches!(
                error.code,
                "invalid_setup_token" | "instance_already_claimed"
            ) =>
        {
            let mut view = state
                .enrollment
                .lock()
                .map_err(|_| BridgeError::host_state())?
                .view();
            view.stage = "error";
            view.error_code = Some(error.code.into());
            return Ok(view);
        }
        Err(error) => return Err(error),
    };

    let has_gateway = match &outcome {
        OwnerCreation::Activated { .. } => refresh_gateway_presence(&state).await?,
        OwnerCreation::Cancelled => None,
    };
    let mut controller = state
        .enrollment
        .lock()
        .map_err(|_| BridgeError::host_state())?;
    if matches!(outcome, OwnerCreation::Activated { .. }) {
        controller.has_gateway = has_gateway;
        Ok(EnrollmentView {
            stage: "ready",
            owner: Some(true),
            ..controller.view()
        })
    } else {
        Ok(controller.view())
    }
}

#[tauri::command]
pub async fn enrollment_sign_in(
    window: WebviewWindow,
    app: AppHandle,
    state: State<'_, AppState>,
    hosted: State<'_, crate::hosted::HostedState>,
    input: Option<UsernameInput>,
) -> BridgeResult<EnrollmentView> {
    crate::require_main(window.label())?;
    if state
        .enrollment
        .lock()
        .map_err(|_| BridgeError::host_state())?
        .info
        .as_ref()
        .is_some_and(|info| info.mode == DeploymentMode::Hosted)
    {
        crate::hosted::hosted_sign_in(window, app, state.clone(), hosted, "google".into()).await?;
        return Ok(EnrollmentView {
            stage: "signin",
            ..state
                .enrollment
                .lock()
                .map_err(|_| BridgeError::host_state())?
                .view()
        });
    }
    let Some(password) = dialogs::passphrase(&app, PassphrasePurpose::AdminPassword).await? else {
        return Ok(state
            .enrollment
            .lock()
            .map_err(|_| BridgeError::host_state())?
            .view());
    };
    let origin = state
        .enrollment
        .lock()
        .map_err(|_| BridgeError::host_state())?
        .origin()?
        .to_owned();
    let request = serde_json::to_string(&InstanceLoginRequest {
        username: input.and_then(|value| value.username).unwrap_or_default(),
        admin_password: password.to_string(),
        client: ClientKind::Desktop,
    })
    .map_err(|_| enrollment_error())?;
    let response: peppy_hosted_api::enrollment::InstanceSession = serde_json::from_str(
        &post(
            &http(&origin)?,
            &origin,
            enrollment::routes::INSTANCE_SESSION,
            request,
            None,
            None,
        )
        .await?,
    )
    .map_err(|_| enrollment_error())?;
    let mut controller = state
        .enrollment
        .lock()
        .map_err(|_| BridgeError::host_state())?;
    controller.admin_session = Some(Zeroizing::new(
        response.session_token.ok_or_else(enrollment_error)?,
    ));
    Ok(controller.view())
}

#[tauri::command]
pub async fn enrollment_connect_shared_secret(
    window: WebviewWindow,
    app: AppHandle,
    state: State<'_, AppState>,
    hosted: State<'_, crate::hosted::HostedState>,
    input: Option<VaultOwnerInput>,
) -> BridgeResult<EnrollmentView> {
    crate::require_main(window.label())?;
    let expected_binding = state.config()?.active_binding().cloned();
    let Some(passphrase) = dialogs::passphrase(&app, PassphrasePurpose::Unlock).await? else {
        return Ok(state
            .enrollment
            .lock()
            .map_err(|_| BridgeError::host_state())?
            .view());
    };
    let (origin, challenge_request) = {
        let mut controller = state
            .enrollment
            .lock()
            .map_err(|_| BridgeError::host_state())?;
        let origin = controller.origin()?.to_owned();
        let request = admission_client(&mut controller)?
            .manual_challenge_request(None, input.and_then(|value| value.vault_owner))
            .map_err(|_| enrollment_error())?;
        (origin, request)
    };
    let client = http(&origin)?;
    let account_session = hosted_account_header(&state, &hosted).await?;
    let challenge_json = match post(
        &client,
        &origin,
        enrollment::routes::ENROLLMENT_CHALLENGES,
        challenge_request,
        None,
        account_session.as_ref().map(|value| value.as_str()),
    )
    .await
    {
        Err(error) if error.code == "vault_owner_required" => {
            let mut view = state
                .enrollment
                .lock()
                .map_err(|_| BridgeError::host_state())?
                .view();
            view.stage = "locked";
            view.vault_owner_required = Some(true);
            return Ok(view);
        }
        Err(error) => return Err(error),
        Ok(response) => response,
    };
    let challenge: AdmissionChallenge =
        serde_json::from_str(&challenge_json).map_err(|_| enrollment_error())?;
    let complete_request = state
        .enrollment
        .lock()
        .map_err(|_| BridgeError::host_state())?
        .client
        .as_mut()
        .ok_or_else(enrollment_error)?
        .complete_manual_challenge_request(challenge_json, passphrase.to_string())
        .map_err(|_| enrollment_error())?;
    let complete_json = post(
        &client,
        &origin,
        &format!(
            "{}/{}/complete",
            enrollment::routes::ENROLLMENT_CHALLENGES,
            challenge.enrollment_id
        ),
        complete_request,
        None,
        account_session.as_ref().map(|value| value.as_str()),
    )
    .await?;
    let identity = state
        .enrollment
        .lock()
        .map_err(|_| BridgeError::host_state())?
        .client
        .as_mut()
        .ok_or_else(enrollment_error)?
        .finish_manual_admission(complete_json, passphrase.to_string())
        .map_err(|_| enrollment_error())?;
    let role = identity.metadata().role;
    install_identity(&app, &state, identity, passphrase, expected_binding).await?;
    let has_gateway = refresh_gateway_presence(&state).await?;
    let mut controller = state
        .enrollment
        .lock()
        .map_err(|_| BridgeError::host_state())?;
    controller.has_gateway = has_gateway;
    set_identity_ready(&mut controller, Some(role));
    Ok(EnrollmentView {
        stage: "ready",
        ..controller.view()
    })
}

#[tauri::command]
pub async fn enrollment_begin_vault_request(
    window: WebviewWindow,
    state: State<'_, AppState>,
    input: BeginVaultRequestInput,
) -> BridgeResult<EnrollmentView> {
    crate::require_main(window.label())?;
    let invite = EnrollmentClient::parse_vault_invite_link(&input.invite_link)
        .map_err(|_| enrollment_error())?;
    let epoch = begin_owner_operation(&state.enrollment)?;
    let (origin, request, checkpoint) = {
        let mut controller = state
            .enrollment
            .lock()
            .map_err(|_| BridgeError::host_state())?;
        if controller.operation_epoch != epoch {
            return Ok(controller.view());
        }
        if controller.origin.as_deref() != Some(invite.origin.as_str()) {
            return Err(BridgeError::new(
                "invite_invalid",
                "This invite belongs to a different server.",
            ));
        }
        let client = admission_client(&mut controller)?;
        let request = client
            .begin_vault_request(
                invite.origin.clone(),
                invite.invite_token,
                input.display_name,
            )
            .map_err(|_| enrollment_error())?;
        let checkpoint = Zeroizing::new(client.checkpoint().map_err(|_| enrollment_error())?);
        controller.vault_request_state = Some("pending".into());
        (invite.origin, request, checkpoint)
    };
    {
        let controller = state
            .enrollment
            .lock()
            .map_err(|_| BridgeError::host_state())?;
        if controller.operation_epoch != epoch {
            return Ok(controller.view());
        }
        state
            .store
            .set(&vault_request_checkpoint_key(&origin), &checkpoint)?;
    }
    if !owner_operation_current(&state.enrollment, epoch)? {
        return Ok(state
            .enrollment
            .lock()
            .map_err(|_| BridgeError::host_state())?
            .view());
    }
    let response = match post(
        &http(&origin)?,
        &origin,
        enrollment::routes::INSTANCE_VAULT_REQUESTS,
        request,
        None,
        None,
    )
    .await
    {
        Ok(response) => response,
        Err(error) if definitive_vault_request_error(error.code) => {
            let mut controller = state
                .enrollment
                .lock()
                .map_err(|_| BridgeError::host_state())?;
            if controller.operation_epoch != epoch {
                return Ok(controller.view());
            }
            clear_vault_request_locked(&mut controller, &*state.store, &origin)?;
            return Err(error);
        }
        Err(error) => return Err(error),
    };
    let controller = state
        .enrollment
        .lock()
        .map_err(|_| BridgeError::host_state())?;
    if controller.operation_epoch != epoch {
        return Ok(controller.view());
    }
    controller
        .client
        .as_ref()
        .ok_or_else(enrollment_error)?
        .accept_vault_request_created(response)
        .map_err(|_| enrollment_error())?;
    let checkpoint = controller
        .client
        .as_ref()
        .ok_or_else(enrollment_error)?
        .checkpoint()
        .map_err(|_| enrollment_error())?;
    state
        .store
        .set(&vault_request_checkpoint_key(&origin), &checkpoint)?;
    Ok(controller.view())
}

#[tauri::command]
pub async fn enrollment_vault_request_status(
    window: WebviewWindow,
    state: State<'_, AppState>,
) -> BridgeResult<EnrollmentView> {
    crate::require_main(window.label())?;
    let epoch = begin_owner_operation(&state.enrollment)?;
    let resubmit = {
        let mut controller = state
            .enrollment
            .lock()
            .map_err(|_| BridgeError::host_state())?;
        let origin = controller.origin()?.to_owned();
        let Some(client) = controller.client.as_mut() else {
            return Err(enrollment_error());
        };
        if !client.vault_request_accepted() {
            let request = client
                .begin_vault_request(origin.clone(), String::new(), String::new())
                .map_err(|_| enrollment_error())?;
            let checkpoint = client.checkpoint().map_err(|_| enrollment_error())?;
            Some((origin, request, checkpoint))
        } else {
            None
        }
    };
    if let Some((origin, request, checkpoint)) = resubmit {
        let terminal = resubmit_unaccepted_vault_request(
            &state.enrollment,
            &*state.store,
            epoch,
            &origin,
            request,
            &checkpoint,
            |request| {
                let origin = origin.clone();
                async move {
                    post(
                        &http(&origin)?,
                        &origin,
                        enrollment::routes::INSTANCE_VAULT_REQUESTS,
                        request,
                        None,
                        None,
                    )
                    .await
                }
            },
        )
        .await?;
        if let Some(view) = terminal {
            return Ok(view);
        }
    }
    let (origin, request_id, secret, known_state) = {
        let controller = state
            .enrollment
            .lock()
            .map_err(|_| BridgeError::host_state())?;
        let (origin, request_id, _) = controller
            .client
            .as_ref()
            .and_then(EnrollmentClient::pending_vault_request)
            .ok_or_else(enrollment_error)?;
        let secret = controller
            .client
            .as_ref()
            .ok_or_else(enrollment_error)?
            .vault_request_header()
            .map_err(|_| enrollment_error())?;
        (
            origin,
            request_id,
            secret,
            controller
                .vault_request_state
                .clone()
                .unwrap_or_else(|| "pending".into()),
        )
    };
    let response = match body(
        http(&origin)?
            .get(format!(
                "{origin}{}/{request_id}?wait=15",
                enrollment::routes::INSTANCE_VAULT_REQUESTS
            ))
            .header(enrollment::VAULT_REQUEST_HEADER, secret)
            .header(enrollment::KNOWN_STATE_HEADER, known_state)
            .send()
            .await
            .map_err(|_| NetError::Offline)?,
    )
    .await
    {
        Ok(response) => response,
        Err(error) if error.code == "request_not_found" => {
            let mut controller = state
                .enrollment
                .lock()
                .map_err(|_| BridgeError::host_state())?;
            if controller.operation_epoch != epoch {
                return Ok(controller.view());
            }
            clear_vault_request_locked(&mut controller, &*state.store, &origin)?;
            controller.vault_request_state = Some("expired".into());
            return Ok(controller.view());
        }
        Err(error) => return Err(error),
    };
    let mut controller = state
        .enrollment
        .lock()
        .map_err(|_| BridgeError::host_state())?;
    if controller.operation_epoch != epoch {
        return Ok(controller.view());
    }
    let status = controller
        .client
        .as_ref()
        .ok_or_else(enrollment_error)?
        .parse_vault_request_status(response)
        .map_err(|_| enrollment_error())?;
    controller.vault_request_state = Some(status.clone());
    if matches!(status.as_str(), "denied" | "expired") {
        controller
            .client
            .as_mut()
            .ok_or_else(enrollment_error)?
            .clear_vault_request();
        tombstone_vault_request_checkpoint(&*state.store, &origin)?;
    }
    Ok(controller.view())
}

#[tauri::command]
pub async fn enrollment_create_vault(
    window: WebviewWindow,
    app: AppHandle,
    state: State<'_, AppState>,
    input: UsernameInput,
) -> BridgeResult<EnrollmentView> {
    crate::require_main(window.label())?;
    let epoch = begin_owner_operation(&state.enrollment)?;
    let Some(password) = collect_new_admin_password(&state.enrollment, epoch, |purpose| {
        dialogs::passphrase(&app, purpose)
    })
    .await?
    else {
        return Ok(state
            .enrollment
            .lock()
            .map_err(|_| BridgeError::host_state())?
            .view());
    };
    let retained = state
        .enrollment
        .lock()
        .map_err(|_| BridgeError::host_state())?
        .owner_passphrase
        .clone();
    let secret_origin = state
        .enrollment
        .lock()
        .map_err(|_| BridgeError::host_state())?
        .origin()?
        .to_owned();
    let mut generated_secret = false;
    let passphrase = if let Some(passphrase) = retained {
        passphrase
    } else if let Some(passphrase) =
        read_text_secret(&*state.store, &vault_secret_key(&secret_origin))?
    {
        passphrase
    } else {
        let has_saved_material = state
            .enrollment
            .lock()
            .map_err(|_| BridgeError::host_state())?
            .client
            .as_ref()
            .is_some_and(EnrollmentClient::has_prepared_owner_material);
        if has_saved_material {
            let Some(passphrase) =
                dialogs::passphrase(&app, PassphrasePurpose::RecoverVaultSecret).await?
            else {
                return Ok(state
                    .enrollment
                    .lock()
                    .map_err(|_| BridgeError::host_state())?
                    .view());
            };
            if !owner_operation_current(&state.enrollment, epoch)? {
                return Ok(state
                    .enrollment
                    .lock()
                    .map_err(|_| BridgeError::host_state())?
                    .view());
            }
            passphrase
        } else {
            generated_secret = true;
            Zeroizing::new(peppy_crypto::passphrase::generate_passphrase())
        }
    };
    let expected_binding = state.config()?.active_binding().cloned();
    let (origin, request, checkpoint) = {
        let mut controller = state
            .enrollment
            .lock()
            .map_err(|_| BridgeError::host_state())?;
        if controller.operation_epoch != epoch {
            return Ok(controller.view());
        }
        let origin = controller.origin()?.to_owned();
        let client = controller.client.as_mut().ok_or_else(enrollment_error)?;
        if client
            .owner_passphrase_matches(&passphrase)
            .is_ok_and(|matches| !matches)
        {
            return Err(wrong_owner_passphrase());
        }
        let request = client
            .create_vault_request(
                input.username.unwrap_or_default(),
                password.to_string(),
                passphrase.to_string(),
            )
            .map_err(|_| enrollment_error())?;
        let checkpoint = Zeroizing::new(client.checkpoint().map_err(|_| enrollment_error())?);
        controller.owner_passphrase = Some(Zeroizing::new(passphrase.to_string()));
        controller.vault_creation_in_flight = true;
        (origin, request, checkpoint)
    };
    if generated_secret {
        state
            .store
            .set(&vault_secret_key(&origin), passphrase.as_bytes())?;
    }
    state
        .store
        .set(&vault_request_checkpoint_key(&origin), &checkpoint)?;
    let response = match post(
        &http(&origin)?,
        &origin,
        enrollment::routes::INSTANCE_VAULTS,
        request,
        None,
        None,
    )
    .await
    {
        Err(error) if matches!(error.code, "username_taken" | "invalid_username") => {
            let mut controller = state
                .enrollment
                .lock()
                .map_err(|_| BridgeError::host_state())?;
            controller.vault_request_state = Some("approved".into());
            let mut view = controller.view();
            view.error_code = Some(error.code.into());
            return Ok(view);
        }
        Err(error)
            if error.code == "request_not_approved"
                && !state
                    .enrollment
                    .lock()
                    .map_err(|_| BridgeError::host_state())?
                    .client
                    .as_ref()
                    .is_some_and(EnrollmentClient::has_prepared_owner_material) =>
        {
            let mut controller = state
                .enrollment
                .lock()
                .map_err(|_| BridgeError::host_state())?;
            if controller.operation_epoch != epoch {
                return Ok(controller.view());
            }
            clear_vault_request_locked(&mut controller, &*state.store, &origin)?;
            controller.vault_request_state = Some("expired".into());
            return Ok(controller.view());
        }
        Err(error) => return Err(error),
        Ok(response) => response,
    };
    let identity = {
        let controller = state
            .enrollment
            .lock()
            .map_err(|_| BridgeError::host_state())?;
        if controller.operation_epoch != epoch {
            return Ok(controller.view());
        }
        controller
            .client
            .as_ref()
            .ok_or_else(enrollment_error)?
            .finish_owner_admission(response, passphrase.to_string())
            .map_err(|_| enrollment_error())?
    };
    install_identity(&app, &state, identity, passphrase, expected_binding).await?;
    // The identity is durably imported; never retain the request secret merely because a later
    // UI operation was cancelled or superseded.
    tombstone_vault_request_checkpoint(&*state.store, &origin)?;
    if !owner_operation_current(&state.enrollment, epoch)? {
        return Ok(state
            .enrollment
            .lock()
            .map_err(|_| BridgeError::host_state())?
            .view());
    }
    let mut controller = state
        .enrollment
        .lock()
        .map_err(|_| BridgeError::host_state())?;
    controller
        .client
        .as_mut()
        .ok_or_else(enrollment_error)?
        .clear_vault_request();
    controller.vault_request_state = None;
    controller.vault_creation_in_flight = false;
    controller.owner_passphrase = None;
    controller.ready = true;
    controller.owner = true;
    Ok(controller.view())
}

async fn enrollment_add_phone_core(
    state: &AppState,
    method: EnrollmentMethod,
    hosted: &crate::hosted::HostedState,
) -> BridgeResult<EnrollmentView> {
    ensure_owner_client(state).await?;
    let request = state
        .enrollment
        .lock()
        .map_err(|_| BridgeError::host_state())?
        .client
        .as_mut()
        .ok_or_else(enrollment_error)?
        .create_phone_invitation(method)
        .map_err(|_| enrollment_error())?;
    let session = state.require_session().await?;
    let origin = session.api.origin().to_owned();
    let account = hosted_account_header(state, hosted).await?;
    let created: InvitationCreated = serde_json::from_str(
        &body(
            device_request(
                &session,
                &origin,
                http(&origin)?
                    .post(format!(
                        "{origin}{}",
                        enrollment::routes::ENROLLMENT_INVITATIONS
                    ))
                    .header("content-type", "application/json")
                    .body(request),
                account.as_ref().map(|value| value.as_str()),
            )?
            .send()
            .await
            .map_err(|_| NetError::Offline)?,
        )
        .await?,
    )
    .map_err(|_| enrollment_error())?;
    let mut controller = state
        .enrollment
        .lock()
        .map_err(|_| BridgeError::host_state())?;
    controller
        .client
        .as_mut()
        .ok_or_else(enrollment_error)?
        .consume_created_invitation(
            serde_json::to_string(&created).map_err(|_| enrollment_error())?,
        )
        .map_err(|_| enrollment_error())?;
    controller.invitation_id = Some(created.invitation_id);
    controller.invitation = Some(InvitationStatus {
        state: EnrollmentState::Waiting,
        expires_in_seconds: created.expires_in_seconds,
        claim: None,
        transcript: None,
        encrypted_key_package: None,
        transfer_nonce: None,
        profile: None,
    });
    controller.invitation_method = Some(method);
    Ok(controller.view())
}

#[tauri::command]
pub async fn enrollment_add_phone(
    window: WebviewWindow,
    state: State<'_, AppState>,
    method: EnrollmentMethod,
    hosted: State<'_, crate::hosted::HostedState>,
) -> BridgeResult<EnrollmentView> {
    crate::require_main(window.label())?;
    enrollment_add_phone_core(&state, method, &hosted).await
}

/// Rebuild the in-memory invitation client after a process restart. This is deliberately guarded
/// by the selected origin and the unlocked owner identity; a credential for another server must
/// never become an invitation authority here.
async fn ensure_owner_client(state: &AppState) -> BridgeResult<()> {
    let origin = state
        .enrollment
        .lock()
        .map_err(|_| BridgeError::host_state())?
        .origin
        .clone()
        .ok_or_else(enrollment_error)?;
    if state
        .enrollment
        .lock()
        .map_err(|_| BridgeError::host_state())?
        .client
        .is_some()
    {
        return Ok(());
    }
    let Some(ExistingIdentity::Owner(profile, header)) = existing_identity(state, &origin).await?
    else {
        return Err(enrollment_error());
    };
    let client = EnrollmentClient::for_existing_owner(
        origin.clone(),
        ClientKind::Desktop,
        DEVICE_NAME.into(),
        profile,
        header,
    )
    .map_err(|_| enrollment_error())?;
    let mut controller = state
        .enrollment
        .lock()
        .map_err(|_| BridgeError::host_state())?;
    if controller.origin.as_deref() != Some(origin.as_str()) {
        return Err(enrollment_error());
    }
    controller.client.get_or_insert(client);
    Ok(())
}

async fn refresh_invitation(
    state: &AppState,
    hosted: &crate::hosted::HostedState,
) -> BridgeResult<()> {
    let id = state
        .enrollment
        .lock()
        .map_err(|_| BridgeError::host_state())?
        .invitation_id
        .ok_or_else(enrollment_error)?;
    let session = state.require_session().await?;
    let origin = session.api.origin().to_owned();
    let account = hosted_account_header(state, hosted).await?;
    let status: InvitationStatus = serde_json::from_str(
        &body(
            device_request(
                &session,
                &origin,
                http(&origin)?.get(format!(
                    "{origin}{}/{id}",
                    enrollment::routes::ENROLLMENT_INVITATIONS
                )),
                account.as_ref().map(|value| value.as_str()),
            )?
            .send()
            .await
            .map_err(|_| NetError::Offline)?,
        )
        .await?,
    )
    .map_err(|_| enrollment_error())?;
    state
        .enrollment
        .lock()
        .map_err(|_| BridgeError::host_state())?
        .invitation = Some(status);
    Ok(())
}

async fn cancel_phone_invitation(
    state: &AppState,
    hosted: &crate::hosted::HostedState,
) -> BridgeResult<()> {
    let id = state
        .enrollment
        .lock()
        .map_err(|_| BridgeError::host_state())?
        .invitation_id;
    let Some(id) = id else {
        return Ok(());
    };
    let session = state.require_session().await?;
    let origin = session.api.origin().to_owned();
    let account = hosted_account_header(state, hosted).await?;
    body(
        device_request(
            &session,
            &origin,
            http(&origin)?.delete(format!(
                "{origin}{}/{id}",
                enrollment::routes::ENROLLMENT_INVITATIONS
            )),
            account.as_ref().map(|value| value.as_str()),
        )?
        .send()
        .await
        .map_err(|_| NetError::Offline)?,
    )
    .await?;
    clear_confirmed_phone_cancellation(&state.enrollment, id)
}

/// Clears local invitation state only after the matching server cancellation succeeded.
fn clear_confirmed_phone_cancellation(
    controller: &std::sync::Mutex<Controller>,
    expected_id: uuid::Uuid,
) -> BridgeResult<()> {
    let mut controller = controller.lock().map_err(|_| BridgeError::host_state())?;
    if controller.invitation_id != Some(expected_id) {
        return Ok(());
    }
    controller
        .client
        .as_mut()
        .ok_or_else(enrollment_error)?
        .clear_cancelled_owner_invitation(expected_id)
        .map_err(|_| enrollment_error())?;
    controller.invitation_id = None;
    controller.invitation = None;
    controller.invitation_method = None;
    Ok(())
}

/// Replaces a phone invitation only when it is expired or about to expire. The server cancels any
/// unconsumed invitation unconditionally, so a claimed or approved invitation is never touched.
/// Hitting the active-invitation limit (older invitations from earlier runs) yields the expired
/// stage so the person can restart once those lapse, instead of an error.
async fn rotate_phone_core<R, RF, C, CF, A, AF>(
    controller: &std::sync::Mutex<Controller>,
    method: EnrollmentMethod,
    refresh: R,
    cancel: C,
    add: A,
) -> BridgeResult<EnrollmentView>
where
    R: FnOnce() -> RF,
    RF: std::future::Future<Output = BridgeResult<()>>,
    C: FnOnce() -> CF,
    CF: std::future::Future<Output = BridgeResult<()>>,
    A: FnOnce() -> AF,
    AF: std::future::Future<Output = BridgeResult<EnrollmentView>>,
{
    if controller
        .lock()
        .map_err(|_| BridgeError::host_state())?
        .invitation_id
        .is_none()
    {
        return add().await;
    }
    refresh().await?;
    let rotate = {
        let guard = controller.lock().map_err(|_| BridgeError::host_state())?;
        guard.invitation.as_ref().is_some_and(|status| {
            status.state == EnrollmentState::Expired
                || (status.state == EnrollmentState::Waiting && status.expires_in_seconds <= 15)
                || (status.state == EnrollmentState::Waiting
                    && guard.invitation_method != Some(method))
        })
    };
    if !rotate {
        return Ok(controller
            .lock()
            .map_err(|_| BridgeError::host_state())?
            .view());
    }
    cancel().await?;
    match add().await {
        Ok(view) => Ok(view),
        Err(error) if error.code == "active_invitation_limit" => {
            let mut view = controller
                .lock()
                .map_err(|_| BridgeError::host_state())?
                .view();
            view.stage = "expired";
            Ok(view)
        }
        Err(error) => Err(error),
    }
}

async fn cancel_phone_core<R, RF, C, CF>(
    controller: &std::sync::Mutex<Controller>,
    refresh: R,
    cancel: C,
) -> BridgeResult<EnrollmentView>
where
    R: FnOnce() -> RF,
    RF: std::future::Future<Output = BridgeResult<()>>,
    C: FnOnce() -> CF,
    CF: std::future::Future<Output = BridgeResult<()>>,
{
    if controller
        .lock()
        .map_err(|_| BridgeError::host_state())?
        .invitation_id
        .is_none()
    {
        cancel().await?;
        return Ok(controller
            .lock()
            .map_err(|_| BridgeError::host_state())?
            .view());
    }
    refresh().await?;
    let cancelable = controller
        .lock()
        .map_err(|_| BridgeError::host_state())?
        .invitation
        .as_ref()
        .is_some_and(|status| {
            matches!(
                status.state,
                EnrollmentState::Waiting | EnrollmentState::Expired
            )
        });
    if cancelable {
        cancel().await?;
    }
    Ok(controller
        .lock()
        .map_err(|_| BridgeError::host_state())?
        .view())
}

#[tauri::command]
pub async fn enrollment_cancel_phone(
    window: WebviewWindow,
    state: State<'_, AppState>,
    hosted: State<'_, crate::hosted::HostedState>,
) -> BridgeResult<EnrollmentView> {
    crate::require_main(window.label())?;
    cancel_phone_core(
        &state.enrollment,
        || refresh_invitation(&state, &hosted),
        || cancel_phone_invitation(&state, &hosted),
    )
    .await
}

#[tauri::command]
pub async fn enrollment_rotate_phone(
    window: WebviewWindow,
    state: State<'_, AppState>,
    method: EnrollmentMethod,
    hosted: State<'_, crate::hosted::HostedState>,
) -> BridgeResult<EnrollmentView> {
    crate::require_main(window.label())?;
    rotate_phone_core(
        &state.enrollment,
        method,
        || refresh_invitation(&state, &hosted),
        || cancel_phone_invitation(&state, &hosted),
        || enrollment_add_phone_core(&state, method, &hosted),
    )
    .await
}

#[derive(Clone)]
struct PhoneApprovalPreparation {
    invitation_id: uuid::Uuid,
    claim_digest: String,
    method: EnrollmentMethod,
    vault_id: uuid::Uuid,
    key_epoch: u32,
    fingerprint: String,
}

fn claimed_phone_invitation(controller: &Controller) -> BridgeResult<(uuid::Uuid, String)> {
    let status = controller
        .invitation
        .as_ref()
        .ok_or_else(enrollment_error)?;
    if status.state != EnrollmentState::Claimed {
        return Err(BridgeError::new(
            "enrollment-state",
            "The phone has not completed its connection request.",
        ));
    }
    let invitation_id = controller.invitation_id.ok_or_else(enrollment_error)?;
    let view = controller
        .client
        .as_ref()
        .ok_or_else(enrollment_error)?
        .owner_phone_view(serde_json::to_string(status).map_err(|_| enrollment_error())?)
        .map_err(|_| enrollment_error())?;
    if view.invitation_id != invitation_id {
        return Err(enrollment_error());
    }
    Ok((
        invitation_id,
        view.claim_digest.ok_or_else(enrollment_error)?,
    ))
}

fn invitation_is_current(
    controller: &Controller,
    invitation_id: uuid::Uuid,
    claim_digest: &str,
) -> bool {
    claimed_phone_invitation(controller).is_ok_and(|(current_id, current_claim_digest)| {
        current_id == invitation_id && current_claim_digest == claim_digest
    })
}

/// Captures the validated invitation identity before approval begins. Approval never collects a
/// vault shared secret; transfer uses the active client's verified handoff capability instead.
fn prepare_phone_approval_without_secret(
    controller: &std::sync::Mutex<Controller>,
) -> BridgeResult<PhoneApprovalPreparation> {
    let (invitation_id, claim_digest, method, vault_id, key_epoch, fingerprint) = {
        let controller = controller.lock().map_err(|_| BridgeError::host_state())?;
        let (invitation_id, claim_digest) = claimed_phone_invitation(&controller)?;
        let transcript = controller
            .invitation
            .as_ref()
            .and_then(|status| status.transcript.as_ref())
            .ok_or_else(enrollment_error)?;
        (
            invitation_id,
            claim_digest,
            controller.invitation_method.ok_or_else(enrollment_error)?,
            transcript.vault_id,
            transcript.key_epoch,
            transcript.fingerprint.clone(),
        )
    };
    Ok(PhoneApprovalPreparation {
        invitation_id,
        claim_digest,
        method,
        vault_id,
        key_epoch,
        fingerprint,
    })
}

fn prepare_phone_approval_request(
    controller: &std::sync::Mutex<Controller>,
    preparation: &PhoneApprovalPreparation,
    session: &crate::session::Session,
) -> BridgeResult<Result<Option<String>, peppy_hosted_client::HostedClientError>> {
    let controller = controller.lock().map_err(|_| BridgeError::host_state())?;
    if !invitation_is_current(
        &controller,
        preparation.invitation_id,
        &preparation.claim_digest,
    ) {
        return Ok(Ok(None));
    }
    let status = controller
        .invitation
        .as_ref()
        .ok_or_else(enrollment_error)?;
    if controller.invitation_method != Some(preparation.method) {
        return Ok(Ok(None));
    }
    let client = controller.client.as_ref().ok_or_else(enrollment_error)?;
    let status = serde_json::to_string(status).map_err(|_| enrollment_error())?;
    Ok(match preparation.method {
        EnrollmentMethod::SharedSecret => client
            .approve_phone_invitation(status, String::new())
            .map(Some),
        EnrollmentMethod::Transfer => client
            .approve_phone_invitation_with_client(status, &session.client)
            .map(Some),
    })
}

/// Re-establishes the active client's verified handoff capability from the binding's existing
/// secret. This is only used after the private client approval path reports unavailable key
/// material; it never opens an approval dialog.
async fn restore_phone_handoff_capability(
    state: &AppState,
    session: &std::sync::Arc<crate::session::Session>,
    binding: &crate::credentials::Binding,
    preparation: &PhoneApprovalPreparation,
) -> BridgeResult<bool> {
    let vault = crate::fetch_vault(&session.api, &binding.vault_id, &binding.device_id).await?;
    let (profile, header) = crate::vault_header(&vault)?;
    let current = state.require_session().await?;
    if current.binding != *binding || !std::sync::Arc::ptr_eq(&current, session) {
        return Ok(false);
    }
    if !phone_handoff_recovery_is_current(&state.enrollment, session, binding, preparation)? {
        return Ok(false);
    }
    let secret = stored_vault_secret(&*state.store, &binding.vault_secret_account())?;
    let session = session.clone();
    let binding = binding.clone();
    let preparation = preparation.clone();
    crate::sync::blocking(move || {
        recover_phone_handoff_after_fetch(
            &session,
            &binding,
            &preparation,
            &profile,
            &header,
            &secret,
        )
    })
    .await
}

fn phone_handoff_recovery_is_current(
    controller: &std::sync::Mutex<Controller>,
    session: &crate::session::Session,
    binding: &crate::credentials::Binding,
    preparation: &PhoneApprovalPreparation,
) -> BridgeResult<bool> {
    if session.binding != *binding {
        return Ok(false);
    }
    let controller = controller.lock().map_err(|_| BridgeError::host_state())?;
    Ok(controller.invitation_method == Some(preparation.method)
        && invitation_is_current(
            &controller,
            preparation.invitation_id,
            &preparation.claim_digest,
        ))
}

/// Restores only an already-active epoch after the caller has fetched and pinned vault material.
/// A stale caller must return false before unlocking; malformed fetched material is an enrollment
/// error rather than a stale invitation.
fn recover_phone_handoff_after_fetch(
    session: &crate::session::Session,
    binding: &crate::credentials::Binding,
    preparation: &PhoneApprovalPreparation,
    profile: &KeyProfile,
    header: &VaultCheckHeader,
    secret: &str,
) -> BridgeResult<bool> {
    if session.binding != *binding {
        return Ok(false);
    }
    if !phone_profile_matches(profile, preparation)? || header.profile != *profile {
        return Err(enrollment_error());
    }
    if session
        .client
        .key_status()
        .map_err(crate::core_error)?
        .active_epoch
        != Some(profile.key_epoch)
    {
        return Err(enrollment_error());
    }
    match session.client.unlock(profile, header, secret) {
        Ok(()) => Ok(true),
        Err(
            peppy_client_core::Error::WrongPassphrase | peppy_client_core::Error::InvalidPassphrase,
        ) => Err(vault_secret_mismatch()),
        Err(error) => Err(crate::core_error(error)),
    }
}

fn phone_profile_matches(
    profile: &KeyProfile,
    preparation: &PhoneApprovalPreparation,
) -> BridgeResult<bool> {
    Ok(profile.vault_id == preparation.vault_id
        && profile.key_epoch == preparation.key_epoch
        && profile.fingerprint().map_err(|_| enrollment_error())? == preparation.fingerprint)
}

fn handoff_capability_unavailable(error: &peppy_hosted_client::HostedClientError) -> bool {
    matches!(
        error,
        peppy_hosted_client::HostedClientError::KeysUnavailable
    )
}

#[tauri::command]
pub async fn enrollment_approve(
    window: WebviewWindow,
    state: State<'_, AppState>,
    hosted: State<'_, crate::hosted::HostedState>,
) -> BridgeResult<EnrollmentView> {
    crate::require_main(window.label())?;
    refresh_invitation(&state, &hosted).await?;
    let session = state.require_session().await?;
    let binding = session.binding.clone();
    let preparation = prepare_phone_approval_without_secret(&state.enrollment)?;
    if state.require_session().await?.binding != binding {
        return Ok(state
            .enrollment
            .lock()
            .map_err(|_| BridgeError::host_state())?
            .view());
    }
    let request = match prepare_phone_approval_request(&state.enrollment, &preparation, &session)? {
        Ok(Some(request)) => request,
        Ok(None) => {
            return Ok(state
                .enrollment
                .lock()
                .map_err(|_| BridgeError::host_state())?
                .view());
        }
        // The shared approval API reports only a missing verified handoff capability here.
        // Re-unlock only with the matching binding secret, then retry the same validated claim.
        Err(error)
            if preparation.method == EnrollmentMethod::Transfer
                && handoff_capability_unavailable(&error) =>
        {
            if !restore_phone_handoff_capability(&state, &session, &binding, &preparation).await?
                || state.require_session().await?.binding != binding
            {
                return Ok(state
                    .enrollment
                    .lock()
                    .map_err(|_| BridgeError::host_state())?
                    .view());
            }
            match prepare_phone_approval_request(&state.enrollment, &preparation, &session)? {
                Ok(Some(request)) => request,
                Ok(None) => {
                    return Ok(state
                        .enrollment
                        .lock()
                        .map_err(|_| BridgeError::host_state())?
                        .view());
                }
                Err(_) => return Err(enrollment_error()),
            }
        }
        Err(_) => return Err(enrollment_error()),
    };
    let origin = session.api.origin().to_owned();
    let account = hosted_account_header(&state, &hosted).await?;
    body(
        device_request(
            &session,
            &origin,
            http(&origin)?
                .post(format!(
                    "{origin}{}/{}/approve",
                    enrollment::routes::ENROLLMENT_INVITATIONS,
                    preparation.invitation_id
                ))
                .header("content-type", "application/json")
                .body(request),
            account.as_ref().map(|value| value.as_str()),
        )?
        .send()
        .await
        .map_err(|_| NetError::Offline)?,
    )
    .await?;
    refresh_invitation(&state, &hosted).await?;
    Ok(state
        .enrollment
        .lock()
        .map_err(|_| BridgeError::host_state())?
        .view())
}

#[tauri::command]
pub async fn enrollment_status(
    window: WebviewWindow,
    state: State<'_, AppState>,
    hosted: State<'_, crate::hosted::HostedState>,
) -> BridgeResult<EnrollmentView> {
    crate::require_main(window.label())?;
    let (invitation_active, reconcile_origin) = {
        let controller = state
            .enrollment
            .lock()
            .map_err(|_| BridgeError::host_state())?;
        (
            controller.invitation_id.is_some(),
            (!controller.ready)
                .then(|| controller.origin.clone())
                .flatten(),
        )
    };
    if invitation_active {
        refresh_invitation(&state, &hosted).await?;
    }
    let identity = match reconcile_origin.as_deref() {
        Some(origin) => existing_identity(&state, origin)
            .await?
            .map(|identity| identity.role()),
        None => None,
    };
    let has_gateway = refresh_gateway_presence(&state).await?;
    let mut controller = state
        .enrollment
        .lock()
        .map_err(|_| BridgeError::host_state())?;
    controller.has_gateway = has_gateway;
    if reconcile_origin.as_deref() == controller.origin.as_deref() {
        reconcile_status_core(&mut controller, identity);
    }
    Ok(controller.view())
}

#[tauri::command]
pub async fn reveal_vault_secret(
    window: WebviewWindow,
    app: AppHandle,
    state: State<'_, AppState>,
) -> BridgeResult<()> {
    crate::require_main(window.label())?;
    let session = state.require_session().await?;
    let unlocked = crate::sync::blocking({
        let session = session.clone();
        move || {
            session
                .client
                .key_status()
                .map(|status| status.active_epoch.is_some())
                .map_err(crate::core_error)
        }
    })
    .await?;
    if !unlocked {
        return Err(vault_secret_unavailable());
    }
    let secret = stored_vault_secret(&*state.store, &session.binding.vault_secret_account())?;
    dialogs::show_vault_secret(&app, secret).await
}

#[tauri::command]
pub async fn enrollment_cancel(
    window: WebviewWindow,
    state: State<'_, AppState>,
    hosted: State<'_, crate::hosted::HostedState>,
) -> BridgeResult<()> {
    crate::require_main(window.label())?;
    let id = state
        .enrollment
        .lock()
        .map_err(|_| BridgeError::host_state())?
        .invitation_id;
    if let Some(id) = id {
        let session = state.require_session().await?;
        let origin = session.api.origin().to_owned();
        let account = hosted_account_header(&state, &hosted).await?;
        body(
            device_request(
                &session,
                &origin,
                http(&origin)?.delete(format!(
                    "{origin}{}/{id}",
                    enrollment::routes::ENROLLMENT_INVITATIONS
                )),
                account.as_ref().map(|value| value.as_str()),
            )?
            .send()
            .await
            .map_err(|_| NetError::Offline)?,
        )
        .await?;
    }
    let mut controller = state
        .enrollment
        .lock()
        .map_err(|_| BridgeError::host_state())?;
    controller.operation_epoch = controller.operation_epoch.wrapping_add(1);
    if !controller.vault_creation_in_flight {
        if let Some(origin) = controller.origin.clone() {
            if controller
                .client
                .as_ref()
                .and_then(EnrollmentClient::pending_vault_request)
                .is_some()
            {
                controller
                    .client
                    .as_mut()
                    .expect("checked above")
                    .clear_vault_request();
                tombstone_vault_request_checkpoint(&*state.store, &origin)?;
            }
        }
    }
    controller.client = None;
    controller.owner_passphrase = None;
    controller.owner_resume = false;
    controller.vault_request_state = None;
    controller.vault_creation_in_flight = false;
    controller.invitation_id = None;
    controller.invitation = None;
    controller.invitation_method = None;
    Ok(())
}

#[tauri::command]
pub async fn enrollment_sign_out(
    window: WebviewWindow,
    state: State<'_, AppState>,
) -> BridgeResult<()> {
    crate::require_main(window.label())?;
    state
        .enrollment
        .lock()
        .map_err(|_| BridgeError::host_state())?
        .admin_session = None;
    Ok(())
}

#[cfg(test)]
mod owner_retry_tests {
    use super::*;
    use crate::secure_store::{MemoryStore, SecretStore};
    use crate::tests::gateway;
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
    use peppy_hosted_api::enrollment::{
        ClaimInstanceRequest, ClaimInstanceResponse, ClaimInvitationRequest,
        CreateInvitationRequest, EnrollmentTranscript, InstanceSession, InvitationClaim,
    };
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    };

    const ORIGIN: &str = "https://community.example";
    const OTHER_ORIGIN: &str = "https://other.example";
    const RECOVERY_SECRET: &str = "alpha bravo charlie delta echo foxtrot";

    struct CountingStore {
        reads: AtomicUsize,
    }

    impl SecretStore for CountingStore {
        fn get(&self, _account: &str) -> BridgeResult<Option<zeroize::Zeroizing<Vec<u8>>>> {
            self.reads.fetch_add(1, Ordering::SeqCst);
            Ok(None)
        }

        fn set(&self, _account: &str, _secret: &[u8]) -> BridgeResult<()> {
            Ok(())
        }
    }

    struct FailingStore;

    impl SecretStore for FailingStore {
        fn get(&self, _account: &str) -> BridgeResult<Option<zeroize::Zeroizing<Vec<u8>>>> {
            Err(BridgeError::new("secure-store-unavailable", "unavailable"))
        }

        fn set(&self, _account: &str, _secret: &[u8]) -> BridgeResult<()> {
            Err(BridgeError::new("secure-store-unavailable", "unavailable"))
        }
    }

    #[derive(Default)]
    struct RecordingStore {
        values: Mutex<std::collections::HashMap<String, Vec<u8>>>,
        writes: Mutex<Vec<String>>,
    }

    impl SecretStore for RecordingStore {
        fn get(&self, account: &str) -> BridgeResult<Option<zeroize::Zeroizing<Vec<u8>>>> {
            Ok(self
                .values
                .lock()
                .unwrap()
                .get(account)
                .cloned()
                .map(Zeroizing::new))
        }

        fn set(&self, account: &str, secret: &[u8]) -> BridgeResult<()> {
            self.writes.lock().unwrap().push(account.into());
            self.values
                .lock()
                .unwrap()
                .insert(account.into(), secret.into());
            Ok(())
        }
    }

    fn active_binding() -> crate::credentials::Binding {
        crate::credentials::Binding {
            origin: ORIGIN.into(),
            vault_id: uuid::Uuid::new_v4().to_string(),
            device_id: uuid::Uuid::new_v4().to_string(),
        }
    }

    fn state_with_active_binding(
        root: &tempfile::TempDir,
        store: Arc<dyn SecretStore>,
        binding: &crate::credentials::Binding,
    ) -> AppState {
        let state = AppState::new(root.path().to_path_buf(), store, crate::tests::noop());
        state
            .update_config(|config| {
                config.origin = Some(binding.origin.clone());
                config.remember(binding);
                config.active = Some(binding.clone());
            })
            .unwrap();
        state
    }

    fn unclaimed_controller() -> Mutex<Controller> {
        Mutex::new(Controller {
            origin: Some(ORIGIN.into()),
            info: Some(InstanceInfo {
                mode: DeploymentMode::Community,
                claimed: false,
                setup_required: true,
                password_auth: true,
                qr_pairing: true,
                shared_secret_pairing: true,
            }),
            client: Some(
                EnrollmentClient::new(ORIGIN.into(), ClientKind::Desktop, DEVICE_NAME.into())
                    .unwrap(),
            ),
            ..Controller::default()
        })
    }

    fn pending_owner_controller(store: &MemoryStore) -> Mutex<Controller> {
        let controller = unclaimed_controller();
        let checkpoint = {
            let mut guard = controller.lock().unwrap();
            let client = guard.client.as_mut().unwrap();
            client
                .create_owner_request(
                    "setup-token".into(),
                    "admin".into(),
                    "password long enough".into(),
                    RECOVERY_SECRET.into(),
                )
                .unwrap();
            let checkpoint = client.checkpoint().unwrap();
            guard.owner_resume = true;
            checkpoint
        };
        store
            .set(&owner_checkpoint_key(ORIGIN), &checkpoint)
            .unwrap();
        controller
    }

    fn claim_response(request: &str, replayed: bool) -> String {
        let request: ClaimInstanceRequest = serde_json::from_str(request).unwrap();
        let profile: peppy_crypto::KeyProfile =
            serde_json::from_value(request.material.profile).unwrap();
        serde_json::to_string(&ClaimInstanceResponse {
            vault_id: profile.vault_id,
            device_id: request.device.device_id,
            replayed,
            session: InstanceSession {
                session_token: Some("session".into()),
                csrf_token: None,
                expires_in_seconds: 60,
            },
        })
        .unwrap()
    }

    fn prompt(purpose: PassphrasePurpose) -> BridgeResult<Option<Zeroizing<String>>> {
        let value = match purpose {
            PassphrasePurpose::SetupToken => "setup-token",
            PassphrasePurpose::AdminPasswordCreate | PassphrasePurpose::AdminPasswordConfirm => {
                "password long enough"
            }
            PassphrasePurpose::Unlock => RECOVERY_SECRET,
            _ => panic!("unexpected prompt: {purpose:?}"),
        };
        Ok(Some(Zeroizing::new(value.into())))
    }

    #[tokio::test]
    async fn open_setup_skips_the_native_setup_token_prompt_and_uses_an_empty_token() {
        let controller = unclaimed_controller();
        controller
            .lock()
            .unwrap()
            .info
            .as_mut()
            .unwrap()
            .setup_required = false;
        let store = MemoryStore::default();
        let prompted = Arc::new(Mutex::new(Vec::new()));
        let prompted_by_test = prompted.clone();
        let request = Arc::new(Mutex::new(None));
        let request_by_test = request.clone();

        assert_eq!(
            create_owner_core(
                &controller,
                &store,
                None,
                String::new(),
                move |purpose| {
                    prompted_by_test.lock().unwrap().push(purpose);
                    std::future::ready(prompt(purpose))
                },
                || Zeroizing::new(RECOVERY_SECRET.into()),
                move |_, serialized| {
                    *request_by_test.lock().unwrap() = Some(serialized.clone());
                    std::future::ready(Ok(claim_response(&serialized, false)))
                },
                |_, _, _| std::future::ready(Ok(())),
            )
            .await
            .unwrap(),
            OwnerCreation::Activated { superseded: false }
        );

        assert_eq!(
            *prompted.lock().unwrap(),
            vec![
                PassphrasePurpose::AdminPasswordCreate,
                PassphrasePurpose::AdminPasswordConfirm,
            ]
        );
        let request: ClaimInstanceRequest =
            serde_json::from_str(request.lock().unwrap().as_ref().unwrap()).unwrap();
        assert!(request.setup_token.is_empty());
    }

    #[tokio::test]
    async fn invalid_admin_password_lengths_stop_before_generating_or_persisting_owner_material() {
        for password in [
            String::new(),
            "a".repeat(11),
            "é".repeat(5) + "a",
            "a".repeat(1025),
        ] {
            let controller = unclaimed_controller();
            let store = MemoryStore::default();
            let prompted = Arc::new(Mutex::new(Vec::new()));
            let prompted_by_test = prompted.clone();

            let error = create_owner_core(
                &controller,
                &store,
                None,
                String::new(),
                move |purpose| {
                    prompted_by_test.lock().unwrap().push(purpose);
                    let value = match purpose {
                        PassphrasePurpose::SetupToken => "setup-token",
                        PassphrasePurpose::AdminPasswordCreate => &password,
                        _ => panic!("invalid password must stop before another prompt"),
                    };
                    std::future::ready(Ok(Some(Zeroizing::new(value.into()))))
                },
                || panic!("invalid password must not generate a secret"),
                |_, _| async move {
                    panic!("invalid password must not post a claim");
                    #[allow(unreachable_code)]
                    Ok(String::new())
                },
                |_, _, _| async move {
                    panic!("invalid password must not activate an identity");
                    #[allow(unreachable_code)]
                    Ok(())
                },
            )
            .await
            .unwrap_err();

            assert_eq!(error.code, "admin-password-length");
            assert!(store.get(&owner_checkpoint_key(ORIGIN)).unwrap().is_none());
            assert_eq!(
                *prompted.lock().unwrap(),
                vec![
                    PassphrasePurpose::SetupToken,
                    PassphrasePurpose::AdminPasswordCreate,
                ]
            );
            assert!(!controller
                .lock()
                .unwrap()
                .client
                .as_ref()
                .unwrap()
                .has_prepared_owner_material());
        }
    }

    #[tokio::test]
    async fn invalid_replacement_password_stops_before_confirmation_or_owner_mutation() {
        let controller = unclaimed_controller();
        let store = MemoryStore::default();
        let prompted = Arc::new(Mutex::new(Vec::new()));
        let prompted_by_test = prompted.clone();
        let responses = Arc::new(Mutex::new(vec![
            "setup-token",
            "first password long enough",
            "different password long enough",
            "short",
        ]));
        let responses_by_test = responses.clone();

        let error = create_owner_core(
            &controller,
            &store,
            None,
            String::new(),
            move |purpose| {
                prompted_by_test.lock().unwrap().push(purpose);
                let value = responses_by_test.lock().unwrap().remove(0);
                std::future::ready(Ok(Some(Zeroizing::new(value.into()))))
            },
            || panic!("an invalid replacement password must not generate a secret"),
            |_, _| async move {
                panic!("an invalid replacement password must not post a claim");
                #[allow(unreachable_code)]
                Ok(String::new())
            },
            |_, _, _| async move {
                panic!("an invalid replacement password must not activate an identity");
                #[allow(unreachable_code)]
                Ok(())
            },
        )
        .await
        .unwrap_err();

        assert_eq!(error.code, "admin-password-length");
        assert_eq!(
            *prompted.lock().unwrap(),
            vec![
                PassphrasePurpose::SetupToken,
                PassphrasePurpose::AdminPasswordCreate,
                PassphrasePurpose::AdminPasswordConfirm,
                PassphrasePurpose::AdminPasswordMismatch,
            ]
        );
        assert!(store.get(&owner_checkpoint_key(ORIGIN)).unwrap().is_none());
        assert!(!controller
            .lock()
            .unwrap()
            .client
            .as_ref()
            .unwrap()
            .has_prepared_owner_material());
    }

    #[tokio::test]
    async fn owner_password_mismatch_recollects_and_confirms_before_creating_material() {
        let controller = unclaimed_controller();
        let store = MemoryStore::default();
        let prompted = Arc::new(Mutex::new(Vec::new()));
        let prompted_by_test = prompted.clone();
        let responses = Arc::new(Mutex::new(vec![
            Some("setup-token"),
            Some("first password long enough"),
            Some("different password long enough"),
            Some("correct password long enough"),
            Some("correct password long enough"),
        ]));
        let responses_by_test = responses.clone();

        assert_eq!(
            create_owner_core(
                &controller,
                &store,
                None,
                String::new(),
                move |purpose| {
                    prompted_by_test.lock().unwrap().push(purpose);
                    let value = responses_by_test.lock().unwrap().remove(0);
                    std::future::ready(Ok(value.map(|value| Zeroizing::new(value.into()))))
                },
                || Zeroizing::new(RECOVERY_SECRET.into()),
                |_, request| std::future::ready(Ok(claim_response(&request, false))),
                |_, _, _| std::future::ready(Ok(())),
            )
            .await
            .unwrap(),
            OwnerCreation::Activated { superseded: false }
        );

        assert_eq!(
            *prompted.lock().unwrap(),
            vec![
                PassphrasePurpose::SetupToken,
                PassphrasePurpose::AdminPasswordCreate,
                PassphrasePurpose::AdminPasswordConfirm,
                PassphrasePurpose::AdminPasswordMismatch,
                PassphrasePurpose::AdminPasswordConfirm,
            ]
        );
    }

    #[tokio::test]
    async fn cancelled_mismatch_cannot_create_owner_material() {
        let controller = unclaimed_controller();
        let store = MemoryStore::default();
        let responses = Arc::new(Mutex::new(vec![
            Some("setup-token"),
            Some("first password long enough"),
            Some("different password long enough"),
            None,
        ]));
        let responses_by_test = responses.clone();

        assert_eq!(
            create_owner_core(
                &controller,
                &store,
                None,
                String::new(),
                move |_| {
                    let value = responses_by_test.lock().unwrap().remove(0);
                    std::future::ready(Ok(value.map(|value| Zeroizing::new(value.into()))))
                },
                || panic!("a cancelled password confirmation must not generate a secret"),
                |_, _| async move {
                    panic!("a cancelled password confirmation must not post a claim");
                    #[allow(unreachable_code)]
                    Ok(String::new())
                },
                |_, _, _| async move {
                    panic!("a cancelled password confirmation must not activate an identity");
                    #[allow(unreachable_code)]
                    Ok(())
                },
            )
            .await
            .unwrap(),
            OwnerCreation::Cancelled
        );

        assert!(store.get(&owner_checkpoint_key(ORIGIN)).unwrap().is_none());
        assert!(!controller
            .lock()
            .unwrap()
            .client
            .as_ref()
            .unwrap()
            .has_prepared_owner_material());
    }

    #[tokio::test]
    async fn stale_operation_while_confirming_a_new_password_stops_before_secret_generation() {
        let controller = Arc::new(unclaimed_controller());
        let store = MemoryStore::default();
        let prompt_controller = controller.clone();
        let posts = Arc::new(Mutex::new(0));
        let posts_by_test = posts.clone();

        assert_eq!(
            create_owner_core(
                &controller,
                &store,
                None,
                String::new(),
                move |purpose| {
                    if purpose == PassphrasePurpose::AdminPasswordConfirm {
                        cancel_owner_operation(&prompt_controller).unwrap();
                    }
                    std::future::ready(prompt(purpose))
                },
                || panic!("a stale confirmation must not generate a secret"),
                move |_, _| {
                    *posts_by_test.lock().unwrap() += 1;
                    std::future::ready(Ok(String::new()))
                },
                |_, _, _| async move {
                    panic!("a stale confirmation must not activate an identity");
                    #[allow(unreachable_code)]
                    Ok(())
                },
            )
            .await
            .unwrap(),
            OwnerCreation::Cancelled
        );

        assert_eq!(*posts.lock().unwrap(), 0);
        assert!(store.get(&owner_checkpoint_key(ORIGIN)).unwrap().is_none());
        let controller = controller.lock().unwrap();
        assert!(!controller.ready);
        assert!(!controller
            .client
            .as_ref()
            .unwrap()
            .has_prepared_owner_material());
    }

    #[tokio::test]
    async fn fresh_owner_creation_accepts_byte_length_boundaries_without_reprompting_for_a_secret()
    {
        for password in ["a".repeat(12), "é".repeat(6), "a".repeat(1024)] {
            let controller = unclaimed_controller();
            let store = MemoryStore::default();
            let prompted = Arc::new(Mutex::new(Vec::new()));
            let seen = prompted.clone();

            assert_eq!(
                create_owner_core(
                    &controller,
                    &store,
                    None,
                    String::new(),
                    move |purpose| {
                        seen.lock().unwrap().push(purpose);
                        let value = match purpose {
                            PassphrasePurpose::SetupToken => "setup-token",
                            PassphrasePurpose::AdminPasswordCreate
                            | PassphrasePurpose::AdminPasswordConfirm => &password,
                            _ => panic!("fresh owner creation must not re-prompt for a secret"),
                        };
                        std::future::ready(Ok(Some(Zeroizing::new(value.into()))))
                    },
                    || Zeroizing::new(RECOVERY_SECRET.into()),
                    |_, request| std::future::ready(Ok(claim_response(&request, false))),
                    |_, _, _| std::future::ready(Ok(())),
                )
                .await
                .unwrap(),
                OwnerCreation::Activated { superseded: false }
            );

            assert_eq!(
                *prompted.lock().unwrap(),
                vec![
                    PassphrasePurpose::SetupToken,
                    PassphrasePurpose::AdminPasswordCreate,
                    PassphrasePurpose::AdminPasswordConfirm,
                ]
            );
        }
    }

    #[tokio::test]
    async fn fresh_owner_writes_its_pending_secret_before_the_resume_checkpoint() {
        let controller = unclaimed_controller();
        let store = RecordingStore::default();

        create_owner_core(
            &controller,
            &store,
            None,
            String::new(),
            |purpose| std::future::ready(prompt(purpose)),
            || Zeroizing::new(RECOVERY_SECRET.into()),
            |_, request| std::future::ready(Ok(claim_response(&request, false))),
            |_, _, _| std::future::ready(Ok(())),
        )
        .await
        .unwrap();

        let writes = store.writes.lock().unwrap();
        let secret = writes
            .iter()
            .position(|key| key == &owner_secret_key(ORIGIN))
            .unwrap();
        let checkpoint = writes
            .iter()
            .position(|key| key == &owner_checkpoint_key(ORIGIN))
            .unwrap();
        assert!(secret < checkpoint);
    }

    #[tokio::test]
    async fn lost_response_retry_reuses_operation_device_and_credential_metadata() {
        let controller = unclaimed_controller();
        let store = MemoryStore::default();
        let requests = Arc::new(Mutex::new(Vec::<String>::new()));
        let accepted_claim = Arc::new(Mutex::new(None::<String>));

        let first_requests = requests.clone();
        let first_accepted_claim = accepted_claim.clone();
        let first = create_owner_core(
            &controller,
            &store,
            None,
            String::new(),
            |purpose| std::future::ready(prompt(purpose)),
            || Zeroizing::new(RECOVERY_SECRET.into()),
            move |_, request| {
                first_requests.lock().unwrap().push(request.clone());
                let _server_response = claim_response(&request, false);
                *first_accepted_claim.lock().unwrap() = Some(request);
                std::future::ready(Err(BridgeError::new("offline", "response lost")))
            },
            |_, _, _| std::future::ready(Ok(())),
        )
        .await;
        assert_eq!(first.unwrap_err().code, "offline");
        let protected_checkpoint = store
            .get(&owner_checkpoint_key(ORIGIN))
            .unwrap()
            .filter(|value| !value.is_empty())
            .expect("the pending client is protected before the claim POST");
        assert!(
            !protected_checkpoint
                .windows(RECOVERY_SECRET.len())
                .any(|window| window == RECOVERY_SECRET.as_bytes()),
            "the recovery secret is never serialized into the checkpoint",
        );

        let second_requests = requests.clone();
        let second_accepted_claim = accepted_claim.clone();
        let activated = Arc::new(Mutex::new(None));
        let activated_result = activated.clone();
        assert_eq!(
            create_owner_core(
                &controller,
                &store,
                None,
                String::new(),
                |purpose| std::future::ready(prompt(purpose)),
                || panic!("an immediate retry must retain the generated secret"),
                move |_, request| {
                    second_requests.lock().unwrap().push(request.clone());
                    assert_eq!(
                        second_accepted_claim.lock().unwrap().as_deref(),
                        Some(request.as_str()),
                        "the server only replays an exactly matching first claim",
                    );
                    std::future::ready(Ok(claim_response(&request, true)))
                },
                move |identity, _, expected_binding| {
                    *activated_result.lock().unwrap() = Some((
                        identity.metadata().vault_id,
                        identity.metadata().device_id,
                        identity.device_credential_for_native_secure_store(),
                        expected_binding,
                    ));
                    std::future::ready(Ok(()))
                },
            )
            .await
            .unwrap(),
            OwnerCreation::Activated { superseded: false }
        );

        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        let first: ClaimInstanceRequest = serde_json::from_str(&requests[0]).unwrap();
        let second: ClaimInstanceRequest = serde_json::from_str(&requests[1]).unwrap();
        assert_eq!(first.operation_id, second.operation_id);
        assert_eq!(first.device, second.device);
        assert_eq!(first.credential_digest, second.credential_digest);
        assert_eq!(
            serde_json::to_value(&first.material).unwrap(),
            serde_json::to_value(&second.material).unwrap()
        );
        let activated = activated.lock().unwrap();
        let activated = activated.as_ref().unwrap();
        assert_eq!(
            activated.0,
            serde_json::from_value::<peppy_crypto::KeyProfile>(first.material.profile)
                .unwrap()
                .vault_id
        );
        assert_eq!(activated.1, first.device.device_id);
        assert_eq!(activated.2.len(), 96);
        assert!(activated.3.is_none());
        assert_eq!(
            store
                .get(&owner_checkpoint_key(ORIGIN))
                .unwrap()
                .unwrap()
                .as_slice(),
            b""
        );
    }

    #[tokio::test]
    async fn resumed_wrong_secret_never_posts_or_replaces_the_pending_owner() {
        let store = MemoryStore::default();
        let mut pending =
            EnrollmentClient::new(ORIGIN.into(), ClientKind::Desktop, DEVICE_NAME.into()).unwrap();
        let original: ClaimInstanceRequest = serde_json::from_str(
            &pending
                .create_owner_request(
                    "setup-token".into(),
                    "admin".into(),
                    "password long enough".into(),
                    RECOVERY_SECRET.into(),
                )
                .unwrap(),
        )
        .unwrap();
        store
            .set(
                &owner_checkpoint_key(ORIGIN),
                &pending.checkpoint().unwrap(),
            )
            .unwrap();
        let controller = Mutex::new(Controller {
            client: restore_pending_owner(&store, ORIGIN).unwrap(),
            origin: Some(ORIGIN.into()),
            info: Some(InstanceInfo {
                mode: DeploymentMode::Community,
                claimed: false,
                setup_required: true,
                password_auth: true,
                qr_pairing: true,
                shared_secret_pairing: true,
            }),
            owner_resume: true,
            ..Controller::default()
        });
        let posts = Arc::new(Mutex::new(0));
        let posts_seen = posts.clone();

        let error = create_owner_core(
            &controller,
            &store,
            None,
            String::new(),
            |purpose| {
                std::future::ready(match purpose {
                    PassphrasePurpose::ResumeOwnerSecret => Ok(Some(Zeroizing::new(
                        "wrong secret with enough separate words".into(),
                    ))),
                    _ => prompt(purpose),
                })
            },
            || panic!("resume must prompt instead of generating"),
            move |_, _| {
                *posts_seen.lock().unwrap() += 1;
                std::future::ready(Ok(String::new()))
            },
            |_, _, _| std::future::ready(Ok(())),
        )
        .await
        .unwrap_err();

        assert_eq!(error.code, "wrong-passphrase");
        assert_eq!(*posts.lock().unwrap(), 0);
        let mut restored = restore_pending_owner(&store, ORIGIN).unwrap().unwrap();
        let retried: ClaimInstanceRequest = serde_json::from_str(
            &restored
                .create_owner_request(
                    "setup-token".into(),
                    "admin".into(),
                    "password long enough".into(),
                    RECOVERY_SECRET.into(),
                )
                .unwrap(),
        )
        .unwrap();
        assert_eq!(retried.operation_id, original.operation_id);
        assert_eq!(retried.device, original.device);
        assert_eq!(retried.credential_digest, original.credential_digest);
    }

    #[tokio::test]
    async fn resumed_owner_creation_requests_the_earlier_owner_secret() {
        let store = MemoryStore::default();
        let controller = unclaimed_controller();
        controller.lock().unwrap().owner_resume = true;
        let prompted = Arc::new(Mutex::new(Vec::new()));
        let seen = prompted.clone();

        let _ = create_owner_core(
            &controller,
            &store,
            None,
            String::new(),
            move |purpose| {
                seen.lock().unwrap().push(purpose);
                std::future::ready(match purpose {
                    PassphrasePurpose::ResumeOwnerSecret => Ok(None),
                    _ => prompt(purpose),
                })
            },
            || panic!("resuming must not generate a secret"),
            |_, _| std::future::ready(Ok(String::new())),
            |_, _, _| std::future::ready(Ok(())),
        )
        .await
        .unwrap();

        assert_eq!(
            *prompted.lock().unwrap(),
            vec![
                PassphrasePurpose::SetupToken,
                PassphrasePurpose::AdminPasswordCreate,
                PassphrasePurpose::AdminPasswordConfirm,
                PassphrasePurpose::ResumeOwnerSecret,
            ]
        );
    }

    #[tokio::test]
    async fn start_over_replaces_an_unclaimed_pending_owner_and_tombstones_its_checkpoint() {
        let store = MemoryStore::default();
        let controller = pending_owner_controller(&store);
        store
            .set(&owner_secret_key(ORIGIN), RECOVERY_SECRET.as_bytes())
            .unwrap();
        let view = start_over_core(&controller, &store, |_| async {
            Ok(InstanceInfo {
                mode: DeploymentMode::Community,
                claimed: false,
                setup_required: true,
                password_auth: true,
                qr_pairing: true,
                shared_secret_pairing: true,
            })
        })
        .await
        .unwrap();

        assert_eq!(view.stage, "setup");
        assert!(!view.owner_resume_pending);
        assert_eq!(
            store
                .get(&owner_checkpoint_key(ORIGIN))
                .unwrap()
                .unwrap()
                .as_slice(),
            b""
        );
        assert_eq!(
            store
                .get(&owner_secret_key(ORIGIN))
                .unwrap()
                .unwrap()
                .as_slice(),
            b""
        );
        let guard = controller.lock().unwrap();
        assert!(!guard.owner_resume);
        assert!(!guard
            .client
            .as_ref()
            .unwrap()
            .is_pending_owner_creation_for_origin(ORIGIN));
    }

    #[tokio::test]
    async fn start_over_keeps_checkpoint_when_server_is_claimed_or_fetch_fails() {
        for result in [
            Err(BridgeError::new("offline", "offline")),
            Ok(InstanceInfo {
                mode: DeploymentMode::Community,
                claimed: true,
                setup_required: true,
                password_auth: true,
                qr_pairing: true,
                shared_secret_pairing: true,
            }),
        ] {
            let store = MemoryStore::default();
            let controller = pending_owner_controller(&store);
            let error =
                match start_over_core(&controller, &store, |_| std::future::ready(result)).await {
                    Err(error) => error,
                    Ok(_) => panic!("start over should reject this response"),
                };
            assert!(matches!(error.code, "offline" | "instance_already_claimed"));
            assert!(store
                .get(&owner_checkpoint_key(ORIGIN))
                .unwrap()
                .is_some_and(|value| !value.is_empty()));
        }
    }

    #[tokio::test]
    async fn start_over_leaves_non_pending_or_stale_operations_unchanged() {
        let store = MemoryStore::default();
        let controller = unclaimed_controller();
        let before = controller.lock().unwrap().view();
        let view = start_over_core(&controller, &store, |_| async {
            Ok(InstanceInfo {
                mode: DeploymentMode::Community,
                claimed: false,
                setup_required: true,
                password_auth: true,
                qr_pairing: true,
                shared_secret_pairing: true,
            })
        })
        .await
        .unwrap();
        assert_eq!(view.stage, before.stage);
        assert!(store.get(&owner_checkpoint_key(ORIGIN)).unwrap().is_none());

        let controller = pending_owner_controller(&store);
        let checkpoint = store
            .get(&owner_checkpoint_key(ORIGIN))
            .unwrap()
            .unwrap()
            .to_vec();
        let view = start_over_core(&controller, &store, |_: String| {
            begin_owner_operation(&controller).unwrap();
            std::future::ready(Ok(InstanceInfo {
                mode: DeploymentMode::Community,
                claimed: false,
                setup_required: true,
                password_auth: true,
                qr_pairing: true,
                shared_secret_pairing: true,
            }))
        })
        .await
        .unwrap();
        assert!(view.owner_resume_pending);
        assert_eq!(
            store
                .get(&owner_checkpoint_key(ORIGIN))
                .unwrap()
                .unwrap()
                .to_vec(),
            checkpoint
        );
    }

    #[tokio::test]
    async fn start_over_refuses_an_in_flight_claim_without_changing_its_epoch_or_checkpoint() {
        let store = MemoryStore::default();
        let controller = pending_owner_controller(&store);
        let (epoch, checkpoint) = {
            let mut guard = controller.lock().unwrap();
            guard.owner_claim_in_flight = 1;
            (
                guard.operation_epoch,
                store
                    .get(&owner_checkpoint_key(ORIGIN))
                    .unwrap()
                    .unwrap()
                    .to_vec(),
            )
        };

        let result = start_over_core(&controller, &store, |_| async {
            panic!("in-flight precondition must refuse before discovery")
        })
        .await;
        let Err(error) = result else {
            panic!("in-flight claim must conflict");
        };

        assert_eq!(error.code, "operation_conflict");
        assert_eq!(controller.lock().unwrap().operation_epoch, epoch);
        assert_eq!(
            store
                .get(&owner_checkpoint_key(ORIGIN))
                .unwrap()
                .unwrap()
                .to_vec(),
            checkpoint
        );
    }

    #[tokio::test]
    async fn failed_claim_post_releases_its_in_flight_claim() {
        let controller = unclaimed_controller();
        let store = MemoryStore::default();

        let result = create_owner_core(
            &controller,
            &store,
            None,
            String::new(),
            |purpose| std::future::ready(prompt(purpose)),
            || Zeroizing::new(RECOVERY_SECRET.into()),
            |_, _| std::future::ready(Err(BridgeError::new("offline", "claim failed"))),
            |_, _, _| std::future::ready(Ok(())),
        )
        .await;

        assert_eq!(result.unwrap_err().code, "offline");
        assert_eq!(controller.lock().unwrap().owner_claim_in_flight, 0);
    }

    #[tokio::test]
    async fn start_over_keeps_refusing_while_any_claim_guard_is_held() {
        let store = MemoryStore::default();
        let controller = pending_owner_controller(&store);
        let (first, second) = {
            let mut guard = controller.lock().unwrap();
            guard.owner_claim_in_flight += 2;
            drop(guard);
            (OwnerClaimGuard(&controller), OwnerClaimGuard(&controller))
        };

        drop(first);
        let refused = start_over_core(&controller, &store, |_| async {
            panic!("an in-flight claim must refuse before discovery")
        })
        .await;
        let Err(error) = refused else {
            panic!("a held claim guard must refuse start over");
        };
        assert_eq!(error.code, "operation_conflict");
        assert_eq!(controller.lock().unwrap().owner_claim_in_flight, 1);

        drop(second);
        assert_eq!(controller.lock().unwrap().owner_claim_in_flight, 0);
    }

    #[tokio::test]
    async fn start_over_refuses_a_non_community_instance_without_tombstoning() {
        let store = MemoryStore::default();
        let controller = pending_owner_controller(&store);
        let checkpoint = store
            .get(&owner_checkpoint_key(ORIGIN))
            .unwrap()
            .unwrap()
            .to_vec();

        let view = start_over_core(&controller, &store, |_| async {
            Ok(InstanceInfo {
                mode: DeploymentMode::Hosted,
                claimed: false,
                setup_required: false,
                password_auth: true,
                qr_pairing: true,
                shared_secret_pairing: true,
            })
        })
        .await
        .unwrap();

        assert!(view.owner_resume_pending);
        assert_eq!(
            store
                .get(&owner_checkpoint_key(ORIGIN))
                .unwrap()
                .unwrap()
                .to_vec(),
            checkpoint
        );
    }

    #[tokio::test]
    async fn cancellation_during_native_prompt_never_activates_a_different_profile() {
        let controller = Arc::new(unclaimed_controller());
        let store = MemoryStore::default();
        let active_profile = Arc::new(Mutex::new("existing-profile"));
        let prompt_controller = controller.clone();
        let activation = active_profile.clone();

        let completed = create_owner_core(
            &controller,
            &store,
            None,
            String::new(),
            move |purpose| {
                if purpose == PassphrasePurpose::SetupToken {
                    cancel_owner_operation(&prompt_controller).unwrap();
                }
                std::future::ready(prompt(purpose))
            },
            || Zeroizing::new(RECOVERY_SECRET.into()),
            |_, _| std::future::ready(Ok(String::new())),
            move |_, _, _| {
                *activation.lock().unwrap() = "new-owner-profile";
                std::future::ready(Ok(()))
            },
        )
        .await
        .unwrap();

        assert_eq!(completed, OwnerCreation::Cancelled);
        assert_eq!(*active_profile.lock().unwrap(), "existing-profile");
        assert!(store.get(&owner_checkpoint_key(ORIGIN)).unwrap().is_none());
        assert!(!controller.lock().unwrap().ready);
    }

    #[tokio::test]
    async fn activated_owner_creation_reports_supersession_after_tombstoning_the_checkpoint() {
        let controller = Arc::new(unclaimed_controller());
        let store = MemoryStore::default();
        let activation_controller = controller.clone();

        let outcome = create_owner_core(
            &controller,
            &store,
            None,
            String::new(),
            |purpose| std::future::ready(prompt(purpose)),
            || Zeroizing::new(RECOVERY_SECRET.into()),
            |_, request| std::future::ready(Ok(claim_response(&request, false))),
            move |_, _, _| {
                cancel_owner_operation(&activation_controller).unwrap();
                std::future::ready(Ok(()))
            },
        )
        .await
        .unwrap();

        assert_eq!(outcome, OwnerCreation::Activated { superseded: true });
        assert_eq!(
            store
                .get(&owner_checkpoint_key(ORIGIN))
                .unwrap()
                .map(|value| value.to_vec()),
            Some(Vec::new())
        );
        let controller = controller.lock().unwrap();
        assert!(controller.ready);
        assert!(controller.owner);
        assert_eq!(controller.view().stage, "ready");
    }

    #[tokio::test]
    async fn superseded_activation_immediately_reports_a_ready_status_view() {
        let controller = Arc::new(unclaimed_controller());
        let store = MemoryStore::default();
        let activation_controller = controller.clone();

        let outcome = create_owner_core(
            &controller,
            &store,
            None,
            String::new(),
            |purpose| std::future::ready(prompt(purpose)),
            || Zeroizing::new(RECOVERY_SECRET.into()),
            |_, request| std::future::ready(Ok(claim_response(&request, false))),
            move |_, _, _| {
                cancel_owner_operation(&activation_controller).unwrap();
                std::future::ready(Ok(()))
            },
        )
        .await
        .unwrap();

        assert_eq!(outcome, OwnerCreation::Activated { superseded: true });
        let view = controller.lock().unwrap().view();
        assert_eq!(view.stage, "ready");
        assert_eq!(view.owner, Some(true));
    }

    #[tokio::test]
    async fn superseded_activation_does_not_mark_a_different_origin_ready() {
        let controller = Arc::new(unclaimed_controller());
        let store = MemoryStore::default();
        let activation_controller = controller.clone();

        let outcome = create_owner_core(
            &controller,
            &store,
            None,
            String::new(),
            |purpose| std::future::ready(prompt(purpose)),
            || Zeroizing::new(RECOVERY_SECRET.into()),
            |_, request| std::future::ready(Ok(claim_response(&request, false))),
            move |_, _, _| {
                cancel_owner_operation(&activation_controller).unwrap();
                let mut controller = activation_controller.lock().unwrap();
                controller.origin = Some("https://different-origin.test".into());
                controller.info = Some(InstanceInfo {
                    mode: DeploymentMode::Community,
                    claimed: false,
                    setup_required: true,
                    password_auth: true,
                    qr_pairing: true,
                    shared_secret_pairing: true,
                });
                std::future::ready(Ok(()))
            },
        )
        .await
        .unwrap();

        assert_eq!(outcome, OwnerCreation::Activated { superseded: true });
        let controller = controller.lock().unwrap();
        assert!(!controller.ready);
        assert!(!controller.owner);
        assert_eq!(controller.view().stage, "setup");
    }

    fn ready_controller() -> Controller {
        Controller {
            origin: Some(ORIGIN.into()),
            info: Some(InstanceInfo {
                mode: DeploymentMode::Community,
                claimed: true,
                setup_required: false,
                password_auth: true,
                qr_pairing: true,
                shared_secret_pairing: true,
            }),
            ..Controller::default()
        }
    }

    fn invitation_status(state: EnrollmentState, expires_in_seconds: u32) -> InvitationStatus {
        InvitationStatus {
            state,
            expires_in_seconds,
            claim: None,
            transcript: None,
            encrypted_key_package: None,
            transfer_nonce: None,
            profile: None,
        }
    }

    fn waiting_phone_controller(
        invitation_id: uuid::Uuid,
        expires_in_seconds: u32,
    ) -> Mutex<Controller> {
        let mut owner =
            EnrollmentClient::new(ORIGIN.into(), ClientKind::Desktop, DEVICE_NAME.into()).unwrap();
        owner
            .create_owner_request(
                "setup".into(),
                "admin".into(),
                "password long enough".into(),
                RECOVERY_SECRET.into(),
            )
            .unwrap();
        owner
            .create_phone_invitation(EnrollmentMethod::Transfer)
            .unwrap();
        owner
            .consume_created_invitation(
                serde_json::to_string(&InvitationCreated {
                    invitation_id,
                    expires_in_seconds,
                })
                .unwrap(),
            )
            .unwrap();
        Mutex::new(Controller {
            client: Some(owner),
            invitation_id: Some(invitation_id),
            invitation: Some(invitation_status(
                EnrollmentState::Waiting,
                expires_in_seconds,
            )),
            invitation_method: Some(EnrollmentMethod::Transfer),
            ready: true,
            owner: true,
            ..ready_controller()
        })
    }

    fn claimed_phone_fixture(method: EnrollmentMethod) -> (Mutex<Controller>, EnrollmentClient) {
        let (controller, recipient, _, _) = claimed_phone_fixture_with_material(method);
        (controller, recipient)
    }

    fn claimed_phone_fixture_with_material(
        method: EnrollmentMethod,
    ) -> (
        Mutex<Controller>,
        EnrollmentClient,
        KeyProfile,
        VaultCheckHeader,
    ) {
        let mut owner =
            EnrollmentClient::new(ORIGIN.into(), ClientKind::Desktop, DEVICE_NAME.into()).unwrap();
        let owner_request: ClaimInstanceRequest = serde_json::from_str(
            &owner
                .create_owner_request(
                    "setup".into(),
                    "admin".into(),
                    "password long enough".into(),
                    RECOVERY_SECRET.into(),
                )
                .unwrap(),
        )
        .unwrap();
        let invitation: CreateInvitationRequest =
            serde_json::from_str(&owner.create_phone_invitation(method).unwrap()).unwrap();
        let invitation_id = uuid::Uuid::from_u128(7);
        owner
            .consume_created_invitation(
                serde_json::to_string(&InvitationCreated {
                    invitation_id,
                    expires_in_seconds: 300,
                })
                .unwrap(),
            )
            .unwrap();
        let qr = owner.get_qr_payload().unwrap();
        let mut recipient =
            EnrollmentClient::new(ORIGIN.into(), ClientKind::Android, "phone".into()).unwrap();
        let claim_request: ClaimInvitationRequest =
            serde_json::from_str(&recipient.claim_phone_invitation(qr).unwrap()).unwrap();
        let profile: peppy_crypto::KeyProfile =
            serde_json::from_value(owner_request.material.profile.clone()).unwrap();
        let header = peppy_crypto::create_vault_check_header(
            &peppy_crypto::derive_root_key(RECOVERY_SECRET, &profile).unwrap(),
            profile.clone(),
        )
        .unwrap();
        recipient
            .consume_claim_response(
                serde_json::to_string(&InvitationClaim {
                    invitation_id,
                    claim_secret: URL_SAFE_NO_PAD.encode([4; 32]),
                    origin: ORIGIN.into(),
                    vault_id: profile.vault_id,
                    fingerprint: owner_request.material.fingerprint.clone(),
                    key_epoch: 1,
                    method,
                    owner_transfer_public_key: invitation.transfer_public_key.clone(),
                    expires_in_seconds: 300,
                })
                .unwrap(),
            )
            .unwrap();
        let transcript = EnrollmentTranscript {
            origin: ORIGIN.into(),
            vault_id: profile.vault_id,
            fingerprint: owner_request.material.fingerprint,
            key_epoch: 1,
            enrollment_id: invitation_id,
            nonce: claim_request.invitation_token_digest,
            owner_transfer_public_key: invitation.transfer_public_key,
            recipient_transfer_public_key: claim_request.transfer_public_key.clone(),
            signing_public_key: claim_request.device.signing_public_key.clone(),
            device_id: claim_request.device.device_id,
            credential_digest: claim_request.credential_digest.clone(),
            role: EnrollmentRole::Gateway,
        };
        let claimed = InvitationStatus {
            state: EnrollmentState::Claimed,
            expires_in_seconds: 300,
            claim: Some(peppy_hosted_api::enrollment::InvitationPublicClaim {
                device: claim_request.device,
                transfer_public_key: transcript.recipient_transfer_public_key.clone(),
                credential_digest: transcript.credential_digest.clone(),
                claim_authenticator: claim_request.claim_authenticator,
            }),
            transcript: Some(transcript),
            encrypted_key_package: None,
            transfer_nonce: None,
            profile: None,
        };
        (
            Mutex::new(Controller {
                client: Some(owner),
                invitation_id: Some(invitation_id),
                invitation: Some(claimed),
                invitation_method: Some(method),
                ..ready_controller()
            }),
            recipient,
            profile,
            header,
        )
    }

    fn session_for_phone_profile(
        profile: &KeyProfile,
    ) -> (
        crate::session::Session,
        crate::credentials::Binding,
        MemoryStore,
        tempfile::TempDir,
    ) {
        let binding = crate::credentials::Binding {
            origin: ORIGIN.into(),
            vault_id: profile.vault_id.to_string(),
            device_id: uuid::Uuid::new_v4().to_string(),
        };
        let store = MemoryStore::default();
        let credential =
            crate::credentials::parse_credential(&crate::credentials::tests::credential_json(
                ORIGIN,
                &binding.vault_id,
                &binding.device_id,
                crate::credentials::tests::TOKEN,
            ))
            .unwrap();
        crate::credentials::store_credential(&store, &credential).unwrap();
        let root = tempfile::tempdir().unwrap();
        let session =
            crate::session::open_session(root.path(), &store, &binding, &[], crate::tests::noop())
                .unwrap();
        (session, binding, store, root)
    }

    #[test]
    fn transfer_phone_approval_preparation_never_reads_or_prompts_for_a_secret() {
        let (controller, _) = claimed_phone_fixture(EnrollmentMethod::Transfer);

        let preparation = prepare_phone_approval_without_secret(&controller).unwrap();

        assert_eq!(preparation.invitation_id, uuid::Uuid::from_u128(7));
        assert_eq!(preparation.method, EnrollmentMethod::Transfer);
    }

    #[test]
    fn handoff_recovery_accepts_only_the_invitation_profile_and_epoch() {
        let profile = KeyProfile::new(uuid::Uuid::new_v4(), 1).unwrap();
        let preparation = PhoneApprovalPreparation {
            invitation_id: uuid::Uuid::new_v4(),
            claim_digest: String::new(),
            method: EnrollmentMethod::Transfer,
            vault_id: profile.vault_id,
            key_epoch: profile.key_epoch,
            fingerprint: profile.fingerprint().unwrap(),
        };

        assert!(phone_profile_matches(&profile, &preparation).unwrap());
        let wrong_epoch = KeyProfile::new(preparation.vault_id, preparation.key_epoch + 1).unwrap();
        assert!(!phone_profile_matches(&wrong_epoch, &preparation).unwrap());
    }

    #[test]
    fn phone_approval_refuses_a_transcript_changed_after_preparation() {
        let (controller, _) = claimed_phone_fixture(EnrollmentMethod::Transfer);
        let preparation = prepare_phone_approval_without_secret(&controller).unwrap();
        controller
            .lock()
            .unwrap()
            .invitation
            .as_mut()
            .unwrap()
            .transcript
            .as_mut()
            .unwrap()
            .key_epoch += 1;

        assert!(!invitation_is_current(
            &controller.lock().unwrap(),
            preparation.invitation_id,
            &preparation.claim_digest,
        ));
    }

    #[test]
    fn phone_approval_rejects_a_forged_claimed_status() {
        let (controller, _) = claimed_phone_fixture(EnrollmentMethod::Transfer);
        controller
            .lock()
            .unwrap()
            .invitation
            .as_mut()
            .unwrap()
            .transcript
            .as_mut()
            .unwrap()
            .enrollment_id = uuid::Uuid::new_v4();

        let Err(error) = prepare_phone_approval_without_secret(&controller) else {
            panic!("forged claimed status must fail");
        };

        assert_eq!(error.code, "enrollment");
    }

    #[test]
    fn shared_secret_phone_approval_needs_no_stored_secret() {
        let (controller, _) = claimed_phone_fixture(EnrollmentMethod::SharedSecret);

        let preparation = prepare_phone_approval_without_secret(&controller).unwrap();

        assert_eq!(preparation.method, EnrollmentMethod::SharedSecret);
    }

    #[test]
    fn only_an_unavailable_verified_handoff_capability_can_trigger_recovery() {
        assert!(handoff_capability_unavailable(
            &peppy_hosted_client::HostedClientError::KeysUnavailable
        ));
        assert!(!handoff_capability_unavailable(
            &peppy_hosted_client::HostedClientError::Crypto
        ));
        assert!(!handoff_capability_unavailable(
            &peppy_hosted_client::HostedClientError::InvalidProfile
        ));
    }

    #[test]
    fn missing_binding_secret_fails_closed_for_handoff_recovery() {
        let binding_account = active_binding().vault_secret_account();

        let error = stored_vault_secret(&MemoryStore::default(), &binding_account).unwrap_err();

        assert_eq!(error.code, "vault-secret-unavailable");
    }

    #[test]
    fn verified_unlock_saves_a_missing_binding_secret_without_replacing_an_existing_value() {
        let binding_account = active_binding().vault_secret_account();
        let store = MemoryStore::default();

        save_verified_vault_secret_if_empty(&store, &binding_account, RECOVERY_SECRET).unwrap();
        assert_eq!(
            stored(&store, &binding_account).as_deref(),
            Some(RECOVERY_SECRET.as_bytes())
        );

        save_verified_vault_secret_if_empty(&store, &binding_account, "replacement").unwrap();
        assert_eq!(
            stored(&store, &binding_account).as_deref(),
            Some(RECOVERY_SECRET.as_bytes())
        );
    }

    #[test]
    fn recovered_cached_owner_creates_a_phone_handoff_without_changing_the_active_epoch() {
        let (controller, mut recipient, profile, header) =
            claimed_phone_fixture_with_material(EnrollmentMethod::Transfer);
        let (session, binding, store, root) = session_for_phone_profile(&profile);
        session
            .client
            .unlock(&profile, &header, RECOVERY_SECRET)
            .unwrap();
        let cache = session
            .client
            .export_native_key_cache(profile.key_epoch)
            .unwrap();
        store
            .set(
                &binding.key_cache_account(profile.key_epoch),
                cache.native_storage_bytes(),
            )
            .unwrap();
        drop(session);
        let session = crate::session::open_session(
            root.path(),
            &store,
            &binding,
            &[profile.key_epoch],
            crate::tests::noop(),
        )
        .unwrap();
        let preparation = prepare_phone_approval_without_secret(&controller).unwrap();
        assert_eq!(
            session.client.key_status().unwrap().active_epoch,
            Some(profile.key_epoch)
        );

        assert!(recover_phone_handoff_after_fetch(
            &session,
            &binding,
            &preparation,
            &profile,
            &header,
            RECOVERY_SECRET,
        )
        .unwrap());
        assert_eq!(
            session.client.key_status().unwrap().active_epoch,
            Some(profile.key_epoch)
        );
        let request = prepare_phone_approval_request(&controller, &preparation, &session)
            .unwrap()
            .unwrap()
            .unwrap();
        let mut approved = controller.lock().unwrap().invitation.clone().unwrap();
        approved.state = EnrollmentState::Approved;
        let request: peppy_hosted_api::enrollment::ApproveInvitationRequest =
            serde_json::from_str(&request).unwrap();
        approved.encrypted_key_package = request.encrypted_key_package;
        approved.transfer_nonce = request.transfer_nonce;
        assert!(!recipient
            .confirm_phone_invitation(serde_json::to_string(&approved).unwrap(), None)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn recovery_with_no_secret_wrong_secret_or_stale_material_creates_no_phone_request() {
        let (controller, _, profile, header) =
            claimed_phone_fixture_with_material(EnrollmentMethod::Transfer);
        let (session, binding, store, root) = session_for_phone_profile(&profile);
        session
            .client
            .unlock(&profile, &header, RECOVERY_SECRET)
            .unwrap();
        let cache = session
            .client
            .export_native_key_cache(profile.key_epoch)
            .unwrap();
        store
            .set(
                &binding.key_cache_account(profile.key_epoch),
                cache.native_storage_bytes(),
            )
            .unwrap();
        drop(session);
        let session = crate::session::open_session(
            root.path(),
            &store,
            &binding,
            &[profile.key_epoch],
            crate::tests::noop(),
        )
        .unwrap();
        let preparation = prepare_phone_approval_without_secret(&controller).unwrap();

        assert_eq!(
            stored_vault_secret(&MemoryStore::default(), &binding.vault_secret_account())
                .unwrap_err()
                .code,
            "vault-secret-unavailable"
        );
        assert_eq!(
            recover_phone_handoff_after_fetch(
                &session,
                &binding,
                &preparation,
                &profile,
                &header,
                "wrong secret",
            )
            .unwrap_err()
            .code,
            "vault-secret-mismatch"
        );
        let swapped_binding = crate::credentials::Binding {
            device_id: uuid::Uuid::new_v4().to_string(),
            ..binding.clone()
        };
        assert!(!phone_handoff_recovery_is_current(
            &controller,
            &session,
            &swapped_binding,
            &preparation,
        )
        .unwrap());
        assert!(!recover_phone_handoff_after_fetch(
            &session,
            &swapped_binding,
            &preparation,
            &profile,
            &header,
            RECOVERY_SECRET,
        )
        .unwrap());
        assert!(matches!(
            prepare_phone_approval_request(&controller, &preparation, &session).unwrap(),
            Err(peppy_hosted_client::HostedClientError::KeysUnavailable)
        ));
    }

    #[test]
    fn changed_claim_or_fetched_header_never_creates_a_phone_request() {
        let (controller, _, profile, _header) =
            claimed_phone_fixture_with_material(EnrollmentMethod::Transfer);
        let (session, binding, _, _) = session_for_phone_profile(&profile);
        let preparation = prepare_phone_approval_without_secret(&controller).unwrap();
        controller
            .lock()
            .unwrap()
            .invitation
            .as_mut()
            .unwrap()
            .transcript
            .as_mut()
            .unwrap()
            .key_epoch += 1;

        assert!(
            !phone_handoff_recovery_is_current(&controller, &session, &binding, &preparation,)
                .unwrap()
        );
        assert!(
            prepare_phone_approval_request(&controller, &preparation, &session)
                .unwrap()
                .unwrap()
                .is_none()
        );

        let (controller, _, profile, _) =
            claimed_phone_fixture_with_material(EnrollmentMethod::Transfer);
        let (session, binding, _, _) = session_for_phone_profile(&profile);
        let preparation = prepare_phone_approval_without_secret(&controller).unwrap();
        let other = KeyProfile::new(profile.vault_id, profile.key_epoch + 1).unwrap();
        let bad_header = peppy_crypto::create_vault_check_header(
            &peppy_crypto::derive_root_key(RECOVERY_SECRET, &other).unwrap(),
            other,
        )
        .unwrap();

        assert_eq!(
            recover_phone_handoff_after_fetch(
                &session,
                &binding,
                &preparation,
                &profile,
                &bad_header,
                RECOVERY_SECRET,
            )
            .unwrap_err()
            .code,
            "enrollment"
        );
        assert!(matches!(
            prepare_phone_approval_request(&controller, &preparation, &session).unwrap(),
            Err(peppy_hosted_client::HostedClientError::KeysUnavailable)
        ));
    }

    #[test]
    fn invitation_views_cover_waiting_and_expired_stages() {
        let mut controller = ready_controller();
        controller.invitation = Some(invitation_status(EnrollmentState::Waiting, 300));
        assert_eq!(controller.view().stage, "waiting_phone");

        controller.invitation = Some(invitation_status(EnrollmentState::Expired, 0));
        assert_eq!(controller.view().stage, "expired");
    }

    #[test]
    fn claimed_community_checkpoint_is_resumable_without_an_identity() {
        assert!(can_restore_owner_checkpoint(
            DeploymentMode::Community,
            false
        ));
        assert!(!can_restore_owner_checkpoint(
            DeploymentMode::Community,
            true
        ));
        assert!(!can_restore_owner_checkpoint(DeploymentMode::Hosted, false));

        let mut controller = ready_controller();
        controller.owner_resume = true;
        assert_eq!(controller.view().stage, "signin");
        assert!(controller.view().owner_resume_pending);
    }

    #[test]
    fn a_pending_vault_request_wins_over_an_old_owner_checkpoint() {
        let store = MemoryStore::default();
        let owner = pending_owner_controller(&store)
            .into_inner()
            .unwrap()
            .client
            .unwrap();
        let mut request =
            EnrollmentClient::new(ORIGIN.into(), ClientKind::Desktop, DEVICE_NAME.into()).unwrap();
        request
            .begin_vault_request(ORIGIN.into(), "A".repeat(43), "Taylor".into())
            .unwrap();

        let (client, resumed) = choose_pending_client(Some(owner), Some(request));
        assert!(!resumed);
        assert!(client.unwrap().pending_vault_request().is_some());

        let owner = pending_owner_controller(&store)
            .into_inner()
            .unwrap()
            .client
            .unwrap();
        let (client, resumed) = choose_pending_client(Some(owner), None);
        assert!(resumed);
        assert!(client.unwrap().is_pending_owner_creation_for_origin(ORIGIN));
    }

    #[test]
    fn joining_keeps_a_pending_vault_request_client() {
        let mut request =
            EnrollmentClient::new(ORIGIN.into(), ClientKind::Desktop, DEVICE_NAME.into()).unwrap();
        request
            .begin_vault_request(ORIGIN.into(), "A".repeat(43), "Taylor".into())
            .unwrap();
        let mut controller = Controller {
            client: Some(request),
            ..ready_controller()
        };
        assert!(admission_client(&mut controller)
            .unwrap()
            .pending_vault_request()
            .is_some());
    }

    #[test]
    fn resuming_on_a_claimed_server_skips_only_the_replayed_setup_code() {
        let controller = Mutex::new(ready_controller());
        assert!(setup_code_required(&controller).unwrap());
        controller.lock().unwrap().owner_resume = true;
        assert!(!setup_code_required(&controller).unwrap());
    }

    #[test]
    fn joining_replaces_only_a_pending_owner_client_and_keeps_its_checkpoint() {
        let store = MemoryStore::default();
        let controller = pending_owner_controller(&store);
        let mut guard = controller.lock().unwrap();
        let client = admission_client(&mut guard).unwrap();
        assert!(!client.is_pending_owner_creation_for_origin(ORIGIN));
        assert!(!guard.owner_resume);
        assert!(store
            .get(&owner_checkpoint_key(ORIGIN))
            .unwrap()
            .is_some_and(|checkpoint| !checkpoint.is_empty()));

        let mut fresh = unclaimed_controller().into_inner().unwrap();
        let before = fresh.client.as_ref().unwrap().checkpoint().unwrap();
        let kept = admission_client(&mut fresh).unwrap().checkpoint().unwrap();
        assert_eq!(before, kept);
    }

    #[test]
    fn manual_admission_flags_follow_the_verified_role() {
        let mut owner = ready_controller();
        set_identity_ready(&mut owner, Some(EnrollmentRole::Owner));
        let owner_view = owner.view();
        assert_eq!(owner_view.stage, "ready");
        assert_eq!(owner_view.owner, Some(true));

        let mut device = ready_controller();
        set_identity_ready(&mut device, Some(EnrollmentRole::Device));
        let device_view = device.view();
        assert_eq!(device_view.stage, "ready");
        assert_eq!(device_view.owner, Some(false));
        assert!(device_view.qr_payload.is_none());

        let mut gateway = ready_controller();
        set_identity_ready(&mut gateway, Some(EnrollmentRole::Gateway));
        assert_eq!(gateway.view().owner, Some(false));
        assert!(gateway.view().has_gateway.is_none());
    }

    #[test]
    fn status_reconciliation_repairs_only_an_unready_owner_controller() {
        let mut owner = ready_controller();
        reconcile_status_core(&mut owner, Some(EnrollmentRole::Owner));
        assert_eq!(owner.view().stage, "ready");
        assert_eq!(owner.view().owner, Some(true));

        let mut device = ready_controller();
        reconcile_status_core(&mut device, Some(EnrollmentRole::Device));
        assert_eq!(device.view().stage, "signin");
        assert_eq!(device.view().owner, None);
    }

    #[tokio::test]
    async fn phone_rotation_only_replaces_expiring_or_expired_invitations() {
        for (status, expected_events) in [
            (invitation_status(EnrollmentState::Claimed, 10), vec![]),
            (invitation_status(EnrollmentState::Approved, 0), vec![]),
            (invitation_status(EnrollmentState::Waiting, 290), vec![]),
            (
                invitation_status(EnrollmentState::Expired, 0),
                vec!["cancel", "create"],
            ),
            (
                invitation_status(EnrollmentState::Waiting, 15),
                vec!["cancel", "create"],
            ),
        ] {
            let controller = Mutex::new(Controller {
                invitation_id: Some(uuid::Uuid::nil()),
                invitation: Some(status),
                invitation_method: Some(EnrollmentMethod::Transfer),
                ..ready_controller()
            });
            let events = Arc::new(Mutex::new(Vec::new()));
            let cancelled = events.clone();
            let created = events.clone();

            rotate_phone_core(
                &controller,
                EnrollmentMethod::Transfer,
                || std::future::ready(Ok(())),
                move || {
                    cancelled.lock().unwrap().push("cancel");
                    std::future::ready(Ok(()))
                },
                move || {
                    created.lock().unwrap().push("create");
                    std::future::ready(Ok(ready_controller().view()))
                },
            )
            .await
            .unwrap();

            assert_eq!(*events.lock().unwrap(), expected_events);
        }
    }

    #[tokio::test]
    async fn invitation_limit_cancels_the_old_invitation_then_returns_expired() {
        let controller = Mutex::new(Controller {
            invitation_id: Some(uuid::Uuid::nil()),
            invitation: Some(invitation_status(EnrollmentState::Expired, 0)),
            ..ready_controller()
        });
        let cancellations = Arc::new(AtomicUsize::new(0));
        let cancelled = cancellations.clone();

        let view = rotate_phone_core(
            &controller,
            EnrollmentMethod::Transfer,
            || std::future::ready(Ok(())),
            move || {
                cancelled.fetch_add(1, Ordering::SeqCst);
                std::future::ready(Ok(()))
            },
            || std::future::ready(Err(BridgeError::new("active_invitation_limit", "limit"))),
        )
        .await
        .unwrap();

        assert_eq!(cancellations.load(Ordering::SeqCst), 1);
        assert_eq!(view.stage, "expired");
    }

    #[tokio::test]
    async fn phone_rotation_replaces_a_waiting_invitation_for_a_new_method() {
        let controller = Mutex::new(Controller {
            invitation_id: Some(uuid::Uuid::nil()),
            invitation: Some(invitation_status(EnrollmentState::Waiting, 290)),
            invitation_method: Some(EnrollmentMethod::Transfer),
            ..ready_controller()
        });
        let events = Arc::new(Mutex::new(Vec::new()));
        let cancelled = events.clone();
        let created = events.clone();

        rotate_phone_core(
            &controller,
            EnrollmentMethod::SharedSecret,
            || std::future::ready(Ok(())),
            move || {
                cancelled.lock().unwrap().push("cancel");
                std::future::ready(Ok(()))
            },
            move || {
                created.lock().unwrap().push("create");
                std::future::ready(Ok(ready_controller().view()))
            },
        )
        .await
        .unwrap();

        assert_eq!(*events.lock().unwrap(), ["cancel", "create"]);
    }

    #[tokio::test]
    async fn phone_rotation_creates_directly_without_an_active_invitation() {
        let controller = Mutex::new(ready_controller());
        let events = Arc::new(Mutex::new(Vec::new()));
        let refreshed = events.clone();
        let cancelled = events.clone();
        let created = events.clone();

        rotate_phone_core(
            &controller,
            EnrollmentMethod::Transfer,
            move || {
                refreshed.lock().unwrap().push("refresh");
                std::future::ready(Ok(()))
            },
            move || {
                cancelled.lock().unwrap().push("cancel");
                std::future::ready(Ok(()))
            },
            move || {
                created.lock().unwrap().push("create");
                std::future::ready(Ok(ready_controller().view()))
            },
        )
        .await
        .unwrap();

        assert_eq!(*events.lock().unwrap(), ["create"]);
    }

    #[tokio::test]
    async fn cancel_phone_leaves_a_claimed_invitation_untouched() {
        let controller = Mutex::new(Controller {
            invitation_id: Some(uuid::Uuid::nil()),
            invitation: Some(invitation_status(EnrollmentState::Claimed, 10)),
            ..ready_controller()
        });
        let cancellations = Arc::new(AtomicUsize::new(0));
        let cancelled = cancellations.clone();

        let view = cancel_phone_core(
            &controller,
            || std::future::ready(Ok(())),
            move || {
                cancelled.fetch_add(1, Ordering::SeqCst);
                std::future::ready(Ok(()))
            },
        )
        .await
        .unwrap();

        assert_eq!(view.stage, "verify_phone");
        assert_eq!(cancellations.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn confirmed_cancellation_clears_the_real_owner_client_for_a_new_qr() {
        let invitation_id = uuid::Uuid::from_u128(7);
        let controller = waiting_phone_controller(invitation_id, 300);

        cancel_phone_core(
            &controller,
            || std::future::ready(Ok(())),
            || {
                std::future::ready(clear_confirmed_phone_cancellation(
                    &controller,
                    invitation_id,
                ))
            },
        )
        .await
        .unwrap();

        let mut controller = controller.lock().unwrap();
        assert!(controller.invitation_id.is_none());
        let client = controller.client.as_mut().unwrap();
        client
            .create_phone_invitation(EnrollmentMethod::Transfer)
            .unwrap();
        client
            .consume_created_invitation(
                serde_json::to_string(&InvitationCreated {
                    invitation_id: uuid::Uuid::from_u128(8),
                    expires_in_seconds: 300,
                })
                .unwrap(),
            )
            .unwrap();
        assert!(client.get_qr_payload().is_ok());
    }

    #[tokio::test]
    async fn repeated_confirmed_cancellation_is_harmless() {
        let invitation_id = uuid::Uuid::from_u128(7);
        let controller = waiting_phone_controller(invitation_id, 300);

        for _ in 0..2 {
            cancel_phone_core(
                &controller,
                || std::future::ready(Ok(())),
                || {
                    std::future::ready(clear_confirmed_phone_cancellation(
                        &controller,
                        invitation_id,
                    ))
                },
            )
            .await
            .unwrap();
        }

        assert!(controller.lock().unwrap().invitation_id.is_none());
    }

    #[tokio::test]
    async fn failed_phone_cancellation_preserves_the_real_owner_qr() {
        let invitation_id = uuid::Uuid::from_u128(7);
        let controller = waiting_phone_controller(invitation_id, 300);
        let qr = controller
            .lock()
            .unwrap()
            .client
            .as_ref()
            .unwrap()
            .get_qr_payload()
            .unwrap();

        assert!(cancel_phone_core(
            &controller,
            || std::future::ready(Ok(())),
            || std::future::ready(Err(BridgeError::new("offline", "offline"))),
        )
        .await
        .is_err());

        let controller = controller.lock().unwrap();
        assert_eq!(controller.invitation_id, Some(invitation_id));
        assert_eq!(
            controller
                .client
                .as_ref()
                .unwrap()
                .get_qr_payload()
                .unwrap(),
            qr
        );
    }

    #[test]
    fn stale_confirmed_cancellation_preserves_a_newer_invitation() {
        let old_id = uuid::Uuid::from_u128(7);
        let new_id = uuid::Uuid::from_u128(8);
        let controller = waiting_phone_controller(old_id, 300);
        {
            let mut controller = controller.lock().unwrap();
            controller
                .client
                .as_mut()
                .unwrap()
                .clear_cancelled_owner_invitation(old_id)
                .unwrap();
            let client = controller.client.as_mut().unwrap();
            client
                .create_phone_invitation(EnrollmentMethod::Transfer)
                .unwrap();
            client
                .consume_created_invitation(
                    serde_json::to_string(&InvitationCreated {
                        invitation_id: new_id,
                        expires_in_seconds: 300,
                    })
                    .unwrap(),
                )
                .unwrap();
            controller.invitation_id = Some(new_id);
        }

        clear_confirmed_phone_cancellation(&controller, old_id).unwrap();

        let controller = controller.lock().unwrap();
        assert_eq!(controller.invitation_id, Some(new_id));
        assert!(controller.client.as_ref().unwrap().get_qr_payload().is_ok());
    }

    #[tokio::test]
    async fn no_id_cancellation_preserves_an_owner_draft() {
        let mut owner =
            EnrollmentClient::new(ORIGIN.into(), ClientKind::Desktop, DEVICE_NAME.into()).unwrap();
        owner
            .create_owner_request(
                "setup".into(),
                "admin".into(),
                "password long enough".into(),
                RECOVERY_SECRET.into(),
            )
            .unwrap();
        owner
            .create_phone_invitation(EnrollmentMethod::Transfer)
            .unwrap();
        let controller = Mutex::new(Controller {
            client: Some(owner),
            ready: true,
            owner: true,
            ..ready_controller()
        });

        cancel_phone_core(
            &controller,
            || std::future::ready(Ok(())),
            || std::future::ready(Ok(())),
        )
        .await
        .unwrap();

        let mut client = controller.lock().unwrap().client.take().unwrap();
        assert!(client.owner_invitation_id().is_none());
        client
            .consume_created_invitation(
                serde_json::to_string(&InvitationCreated {
                    invitation_id: uuid::Uuid::from_u128(7),
                    expires_in_seconds: 300,
                })
                .unwrap(),
            )
            .unwrap();
        assert!(client.get_qr_payload().is_ok());
    }

    #[tokio::test]
    async fn near_expiry_rotation_replaces_the_real_owner_invitation() {
        let old_id = uuid::Uuid::from_u128(7);
        let new_id = uuid::Uuid::from_u128(8);
        let controller = waiting_phone_controller(old_id, 15);

        rotate_phone_core(
            &controller,
            EnrollmentMethod::Transfer,
            || std::future::ready(Ok(())),
            || std::future::ready(clear_confirmed_phone_cancellation(&controller, old_id)),
            || {
                let mut controller = controller.lock().unwrap();
                let client = controller.client.as_mut().unwrap();
                client
                    .create_phone_invitation(EnrollmentMethod::Transfer)
                    .unwrap();
                client
                    .consume_created_invitation(
                        serde_json::to_string(&InvitationCreated {
                            invitation_id: new_id,
                            expires_in_seconds: 300,
                        })
                        .unwrap(),
                    )
                    .unwrap();
                controller.invitation_id = Some(new_id);
                controller.invitation = Some(invitation_status(EnrollmentState::Waiting, 300));
                std::future::ready(Ok(controller.view()))
            },
        )
        .await
        .unwrap();

        let controller = controller.lock().unwrap();
        assert_eq!(controller.invitation_id, Some(new_id));
        assert!(controller.client.as_ref().unwrap().get_qr_payload().is_ok());
    }

    struct BindingWriteFails {
        inner: MemoryStore,
        binding_account: String,
    }

    impl SecretStore for BindingWriteFails {
        fn get(&self, account: &str) -> BridgeResult<Option<zeroize::Zeroizing<Vec<u8>>>> {
            self.inner.get(account)
        }

        fn set(&self, account: &str, secret: &[u8]) -> BridgeResult<()> {
            if account == self.binding_account {
                return Err(BridgeError::new("secure-store-unavailable", "unavailable"));
            }
            self.inner.set(account, secret)
        }
    }

    fn stored(store: &dyn SecretStore, account: &str) -> Option<Vec<u8>> {
        store.get(account).unwrap().map(|value| value.to_vec())
    }

    #[tokio::test]
    async fn owner_creation_leaves_the_pending_secret_for_the_binding_write() {
        let controller = unclaimed_controller();
        let store = MemoryStore::default();

        assert_eq!(
            create_owner_core(
                &controller,
                &store,
                None,
                String::new(),
                |purpose| std::future::ready(prompt(purpose)),
                || Zeroizing::new(RECOVERY_SECRET.into()),
                |_, request| std::future::ready(Ok(claim_response(&request, false))),
                |_, _, _| std::future::ready(Ok(())),
            )
            .await
            .unwrap(),
            OwnerCreation::Activated { superseded: false }
        );

        assert_eq!(
            stored(&store, &owner_secret_key(ORIGIN)).as_deref(),
            Some(RECOVERY_SECRET.as_bytes())
        );
        assert_eq!(
            stored(&store, &owner_checkpoint_key(ORIGIN)).as_deref(),
            Some(&b""[..])
        );
    }

    #[test]
    fn binding_secret_write_clears_pending_secrets_only_after_it_succeeds() {
        let binding_account = active_binding().vault_secret_account();
        let stored_ok = MemoryStore::default();
        stored_ok
            .set(&owner_secret_key(ORIGIN), RECOVERY_SECRET.as_bytes())
            .unwrap();
        persist_binding_secret(&stored_ok, &binding_account, RECOVERY_SECRET, ORIGIN);
        assert_eq!(
            stored(&stored_ok, &binding_account).as_deref(),
            Some(RECOVERY_SECRET.as_bytes())
        );
        assert_eq!(
            stored(&stored_ok, &owner_secret_key(ORIGIN)).as_deref(),
            Some(&b""[..])
        );

        let failing = BindingWriteFails {
            inner: MemoryStore::default(),
            binding_account: binding_account.clone(),
        };
        failing
            .set(&owner_secret_key(ORIGIN), RECOVERY_SECRET.as_bytes())
            .unwrap();
        persist_binding_secret(&failing, &binding_account, RECOVERY_SECRET, ORIGIN);
        assert_eq!(
            stored(&failing, &owner_secret_key(ORIGIN)).as_deref(),
            Some(RECOVERY_SECRET.as_bytes())
        );
    }

    #[test]
    fn discovery_sweep_moves_a_pending_secret_only_into_an_empty_binding() {
        let binding_account = active_binding().vault_secret_account();
        let store = MemoryStore::default();
        store
            .set(&vault_secret_key(ORIGIN), RECOVERY_SECRET.as_bytes())
            .unwrap();
        sweep_pending_secret(&store, ORIGIN, &binding_account, |_| true).unwrap();
        assert_eq!(
            stored(&store, &binding_account).as_deref(),
            Some(RECOVERY_SECRET.as_bytes())
        );
        assert_eq!(
            stored(&store, &vault_secret_key(ORIGIN)).as_deref(),
            Some(&b""[..])
        );

        store
            .set(&owner_secret_key(ORIGIN), b"newer pending value")
            .unwrap();
        sweep_pending_secret(&store, ORIGIN, &binding_account, |_| true).unwrap();
        assert_eq!(
            stored(&store, &binding_account).as_deref(),
            Some(RECOVERY_SECRET.as_bytes())
        );
    }

    #[test]
    fn discovery_sweep_keeps_wrong_and_unrelated_pending_secrets() {
        let binding_account = active_binding().vault_secret_account();
        let store = MemoryStore::default();
        store
            .set(&owner_secret_key(ORIGIN), b"wrong secret")
            .unwrap();
        store
            .set(&vault_secret_key(ORIGIN), b"unrelated secret")
            .unwrap();

        sweep_pending_secret(&store, ORIGIN, &binding_account, |_| false).unwrap();

        assert!(stored(&store, &binding_account).is_none());
        assert_eq!(
            stored(&store, &owner_secret_key(ORIGIN)).as_deref(),
            Some(&b"wrong secret"[..])
        );
        assert_eq!(
            stored(&store, &vault_secret_key(ORIGIN)).as_deref(),
            Some(&b"unrelated secret"[..])
        );
    }

    #[test]
    fn discovery_sweep_clears_duplicate_pending_secret() {
        let binding_account = active_binding().vault_secret_account();
        let store = MemoryStore::default();
        store
            .set(&binding_account, RECOVERY_SECRET.as_bytes())
            .unwrap();
        store
            .set(&owner_secret_key(ORIGIN), RECOVERY_SECRET.as_bytes())
            .unwrap();
        store
            .set(&vault_secret_key(ORIGIN), b"unrelated secret")
            .unwrap();

        sweep_pending_secret(&store, ORIGIN, &binding_account, |_| true).unwrap();

        assert_eq!(
            stored(&store, &owner_secret_key(ORIGIN)).as_deref(),
            Some(&b""[..])
        );
        assert_eq!(
            stored(&store, &vault_secret_key(ORIGIN)).as_deref(),
            Some(&b"unrelated secret"[..])
        );
    }

    #[test]
    fn stored_vault_secret_is_unavailable_when_absent_or_tombstoned() {
        let binding_account = active_binding().vault_secret_account();
        let store = MemoryStore::default();
        assert_eq!(
            stored_vault_secret(&store, &binding_account)
                .unwrap_err()
                .code,
            "vault-secret-unavailable"
        );
        store.set(&binding_account, b"").unwrap();
        assert_eq!(
            stored_vault_secret(&store, &binding_account)
                .unwrap_err()
                .code,
            "vault-secret-unavailable"
        );
        store
            .set(&binding_account, RECOVERY_SECRET.as_bytes())
            .unwrap();
        assert_eq!(
            stored_vault_secret(&store, &binding_account)
                .unwrap()
                .as_str(),
            RECOVERY_SECRET
        );
    }

    #[test]
    fn clearing_a_vault_request_tombstones_its_pending_secret() {
        let store = MemoryStore::default();
        store
            .set(&vault_secret_key(ORIGIN), RECOVERY_SECRET.as_bytes())
            .unwrap();
        let mut controller = ready_controller();
        clear_vault_request_locked(&mut controller, &store, ORIGIN).unwrap();
        assert_eq!(
            stored(&store, &vault_secret_key(ORIGIN)).as_deref(),
            Some(&b""[..])
        );
    }

    #[test]
    fn setup_policy_is_emitted_only_for_an_unclaimed_community_setup() {
        let mut controller = unclaimed_controller().into_inner().unwrap();
        assert_eq!(controller.view().setup_code_required, Some(true));

        controller.info.as_mut().unwrap().setup_required = false;
        assert_eq!(controller.view().setup_code_required, Some(false));

        let ready = ready_controller();
        assert_eq!(ready.view().setup_code_required, None);
        assert_eq!(Controller::default().view().setup_code_required, None);
    }

    #[test]
    fn claim_error_codes_are_whitelisted_from_bounded_json() {
        assert_eq!(
            known_claim_error_code(br#"{"code":"invalid_setup_token"}"#),
            Some("invalid_setup_token")
        );
        assert_eq!(
            known_claim_error_code(br#"{"code":"instance_already_claimed"}"#),
            Some("instance_already_claimed")
        );
        assert_eq!(known_claim_error_code(b"invalid_setup_token"), None);
        assert_eq!(
            known_claim_error_code(br#"{"code":"unexpected_server_detail"}"#),
            None
        );
        assert_eq!(
            known_claim_error_code(br#"{"code":"vault_owner_required"}"#),
            Some("vault_owner_required")
        );
    }

    async fn unaccepted_resubmit_fixture() -> (Mutex<Controller>, MemoryStore, String, Vec<u8>) {
        let controller = unclaimed_controller();
        let (request, checkpoint) = {
            let mut guard = controller.lock().unwrap();
            guard.vault_request_state = Some("pending".into());
            let client = guard.client.as_mut().unwrap();
            let request = client
                .begin_vault_request(ORIGIN.into(), "A".repeat(43), "Taylor".into())
                .unwrap();
            (request, client.checkpoint().unwrap())
        };
        (controller, MemoryStore::default(), request, checkpoint)
    }

    #[tokio::test]
    async fn definitive_resubmit_rejection_finishes_request_as_invite_expired() {
        let (controller, store, request, checkpoint) = unaccepted_resubmit_fixture().await;
        let epoch = controller.lock().unwrap().operation_epoch;
        let view = resubmit_unaccepted_vault_request(
            &controller,
            &store,
            epoch,
            ORIGIN,
            request,
            &checkpoint,
            |_| async {
                Err::<String, _>(BridgeError::new(
                    "invite_invalid",
                    "This invite is no longer valid.",
                ))
            },
        )
        .await
        .unwrap()
        .unwrap();

        assert_eq!(view.stage, "invite_expired");
        assert!(store
            .get(&vault_request_checkpoint_key(ORIGIN))
            .unwrap()
            .is_none_or(|bytes| bytes.is_empty()));
        assert!(controller
            .lock()
            .unwrap()
            .client
            .as_ref()
            .unwrap()
            .pending_vault_request()
            .is_none());
    }

    #[tokio::test]
    async fn stale_definitive_rejection_preserves_a_newer_pending_request() {
        let (controller, store, request, checkpoint) = unaccepted_resubmit_fixture().await;
        let epoch = controller.lock().unwrap().operation_epoch;
        let old_request_id = controller
            .lock()
            .unwrap()
            .client
            .as_ref()
            .unwrap()
            .pending_vault_request()
            .unwrap()
            .1;
        let (newer_controller, _, _, _) = unaccepted_resubmit_fixture().await;
        let newer_client = newer_controller.lock().unwrap().client.take().unwrap();
        let newer_request_id = newer_client.pending_vault_request().unwrap().1;
        let newer_checkpoint = newer_client.checkpoint().unwrap();
        assert_ne!(newer_request_id, old_request_id);

        let view = resubmit_unaccepted_vault_request(
            &controller,
            &store,
            epoch,
            ORIGIN,
            request,
            &checkpoint,
            |_| async {
                // A cancel followed by a new begin advances the epoch and installs a newer request.
                {
                    let mut guard = controller.lock().unwrap();
                    guard.operation_epoch = guard.operation_epoch.wrapping_add(1);
                    guard.vault_request_state = Some("pending".into());
                    guard.client = Some(newer_client);
                }
                store
                    .set(&vault_request_checkpoint_key(ORIGIN), &newer_checkpoint)
                    .unwrap();
                Err::<String, _>(BridgeError::new(
                    "invite_invalid",
                    "This invite is no longer valid.",
                ))
            },
        )
        .await
        .unwrap()
        .unwrap();

        assert_ne!(view.stage, "invite_expired");
        let guard = controller.lock().unwrap();
        assert_eq!(guard.vault_request_state.as_deref(), Some("pending"));
        assert_eq!(
            guard
                .client
                .as_ref()
                .unwrap()
                .pending_vault_request()
                .map(|pending| pending.1),
            Some(newer_request_id)
        );
        drop(guard);
        assert_eq!(
            store
                .get(&vault_request_checkpoint_key(ORIGIN))
                .unwrap()
                .map(|bytes| bytes.to_vec()),
            Some(newer_checkpoint)
        );
    }

    #[tokio::test]
    async fn transient_resubmit_failure_keeps_the_pending_checkpoint_for_retry() {
        for code in ["offline", "server-rejected"] {
            let (controller, store, request, checkpoint) = unaccepted_resubmit_fixture().await;
            let epoch = controller.lock().unwrap().operation_epoch;
            let error = resubmit_unaccepted_vault_request(
                &controller,
                &store,
                epoch,
                ORIGIN,
                request,
                &checkpoint,
                |_| async move { Err::<String, _>(BridgeError::new(code, "transient")) },
            )
            .await
            .err()
            .unwrap();

            assert_eq!(error.code, code);
            assert!(store
                .get(&vault_request_checkpoint_key(ORIGIN))
                .unwrap()
                .is_some_and(|bytes| !bytes.is_empty()));
            let guard = controller.lock().unwrap();
            assert_eq!(guard.vault_request_state.as_deref(), Some("pending"));
            assert!(guard
                .client
                .as_ref()
                .unwrap()
                .pending_vault_request()
                .is_some());
        }
    }

    #[test]
    fn vault_request_checkpoint_key_is_origin_specific_and_nonrevealing() {
        let first = vault_request_checkpoint_key(ORIGIN);
        let second = vault_request_checkpoint_key("https://other.example");
        assert_ne!(first, second);
        assert!(!first.contains("community.example"));
        assert!(first.starts_with(VAULT_REQUEST_CHECKPOINT_PREFIX));
    }

    #[test]
    fn restored_unaccepted_request_reuses_the_exact_submission_until_accepted() {
        let mut client =
            EnrollmentClient::new(ORIGIN.into(), ClientKind::Desktop, DEVICE_NAME.into()).unwrap();
        let first = client
            .begin_vault_request(ORIGIN.into(), "A".repeat(43), "Taylor".into())
            .unwrap();
        let restored = EnrollmentClient::restore(client.checkpoint().unwrap()).unwrap();
        assert!(!restored.vault_request_accepted());
        let mut restored = restored;
        let replay = restored
            .begin_vault_request(ORIGIN.into(), String::new(), String::new())
            .unwrap();
        assert_eq!(first, replay);
    }

    #[test]
    fn accepted_checkpoint_retains_prepared_creation_material_for_replay() {
        let mut client =
            EnrollmentClient::new(ORIGIN.into(), ClientKind::Desktop, DEVICE_NAME.into()).unwrap();
        client
            .begin_vault_request(ORIGIN.into(), "A".repeat(43), "Taylor".into())
            .unwrap();
        let (_, request_id, _) = client.pending_vault_request().unwrap();
        client
            .accept_vault_request_created(format!(
                "{{\"request_id\":\"{request_id}\",\"expires_in_seconds\":60}}"
            ))
            .unwrap();
        client
            .create_vault_request(
                "taylor".into(),
                "password long enough".into(),
                RECOVERY_SECRET.into(),
            )
            .unwrap();
        let restored = EnrollmentClient::restore(client.checkpoint().unwrap()).unwrap();
        assert!(restored.vault_request_accepted());
        assert!(restored.has_prepared_owner_material());
    }

    #[test]
    fn existing_device_rediscovery_keeps_the_client_and_credential() {
        let mut controller = unclaimed_controller().into_inner().unwrap();
        let before = controller.client.as_ref().unwrap().checkpoint().unwrap();
        let info = controller.info.clone().unwrap();

        apply_discovery(
            &mut controller,
            ORIGIN.into(),
            info,
            Some(EnrollmentRole::Device),
            None,
            false,
            None,
        );

        assert_eq!(
            controller.client.as_ref().unwrap().checkpoint().unwrap(),
            before,
            "re-discovering an existing device must not replace its client"
        );
        let view = controller.view();
        assert_eq!(view.stage, "ready");
        assert_eq!(view.owner, Some(false));
        assert!(!controller.owner_resume);
    }

    #[test]
    fn existing_device_discovery_on_another_origin_drops_the_stale_client() {
        let mut controller = unclaimed_controller().into_inner().unwrap();
        let info = controller.info.clone().unwrap();

        apply_discovery(
            &mut controller,
            "https://other.example".into(),
            info,
            Some(EnrollmentRole::Device),
            None,
            false,
            None,
        );

        assert!(controller.client.is_none());
        assert!(controller.ready);
        assert!(!controller.owner);
    }

    #[test]
    fn gateway_presence_is_none_until_the_registry_is_known() {
        assert_eq!(known_gateway_presence((vec![], true)), Some(false));
        assert_eq!(
            known_gateway_presence((vec![gateway("gw-1", true, false)], true)),
            Some(true)
        );
        assert_eq!(known_gateway_presence((vec![], false)), None);
        assert_eq!(
            known_gateway_presence((vec![gateway("gw-1", true, false)], false)),
            None
        );
    }

    #[tokio::test]
    async fn existing_identity_skips_an_active_binding_for_another_origin() {
        let root = tempfile::tempdir().unwrap();
        let store = Arc::new(CountingStore {
            reads: AtomicUsize::new(0),
        });
        let binding = active_binding();
        let state = state_with_active_binding(&root, store.clone(), &binding);

        assert!(existing_identity(&state, OTHER_ORIGIN)
            .await
            .unwrap()
            .is_none());
        assert_eq!(store.reads.load(Ordering::SeqCst), 0);
        assert!(!binding.data_dir(root.path()).exists());
        assert!(state.session.lock().await.is_none());
    }

    #[tokio::test]
    async fn gateway_presence_skips_an_active_binding_for_another_origin() {
        let root = tempfile::tempdir().unwrap();
        let store = Arc::new(CountingStore {
            reads: AtomicUsize::new(0),
        });
        let binding = active_binding();
        let state = state_with_active_binding(&root, store.clone(), &binding);

        assert_eq!(gateway_presence(&state, OTHER_ORIGIN).await.unwrap(), None);
        assert_eq!(store.reads.load(Ordering::SeqCst), 0);
        assert!(!binding.data_dir(root.path()).exists());
        assert!(state.session.lock().await.is_none());
    }

    #[tokio::test]
    async fn existing_identity_propagates_same_origin_secure_store_failures() {
        let root = tempfile::tempdir().unwrap();
        let binding = active_binding();
        let state = state_with_active_binding(&root, Arc::new(FailingStore), &binding);

        let error = existing_identity(&state, ORIGIN).await.err().unwrap();
        assert_eq!(error.code, "secure-store-unavailable");
    }

    #[tokio::test]
    async fn gateway_presence_propagates_same_origin_secure_store_failures() {
        let root = tempfile::tempdir().unwrap();
        let binding = active_binding();
        let state = state_with_active_binding(&root, Arc::new(FailingStore), &binding);

        let error = gateway_presence(&state, ORIGIN).await.err().unwrap();
        assert_eq!(error.code, "secure-store-unavailable");
    }

    #[test]
    fn device_discovery_reports_gateway_presence_without_changing_role() {
        let mut controller = unclaimed_controller().into_inner().unwrap();
        let info = controller.info.clone().unwrap();

        apply_discovery(
            &mut controller,
            ORIGIN.into(),
            info,
            Some(EnrollmentRole::Device),
            None,
            false,
            Some(true),
        );

        let view = controller.view();
        assert_eq!(view.has_gateway, Some(true));
        assert_eq!(view.owner, Some(false));
        assert_eq!(view.stage, "ready");
    }

    #[test]
    fn unknown_gateway_registry_stays_none_in_the_view() {
        let mut controller = ready_controller();
        set_identity_ready(&mut controller, Some(EnrollmentRole::Owner));
        controller.has_gateway = known_gateway_presence((vec![], false));
        assert_eq!(controller.view().has_gateway, None);
        assert_eq!(controller.view().owner, Some(true));

        controller.has_gateway = known_gateway_presence((vec![], true));
        assert_eq!(controller.view().has_gateway, Some(false));
    }
}
