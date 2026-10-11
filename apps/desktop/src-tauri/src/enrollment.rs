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
use serde::Serialize;
use std::time::Duration;
use tauri::{AppHandle, State, WebviewWindow};
use zeroize::Zeroizing;

const DEVICE_NAME: &str = "Peppy Desktop";
const HOSTED_ORIGIN: &str = "https://peppy.pro";
const OWNER_CHECKPOINT_PREFIX: &str = "enrollment-owner:v1:";

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
    owner_resume: bool,
    owner_passphrase: Option<Zeroizing<String>>,
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
}

impl Controller {
    fn view(&self) -> EnrollmentView {
        let mode = match self.info.as_ref().map(|info| info.mode) {
            Some(DeploymentMode::Hosted) => "hosted",
            _ => "community",
        };
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
            Some(_) if self.ready => "ready",
            Some(info) if !info.claimed || info.setup_required => "setup",
            Some(_) if self.admin_session.is_some() => "locked",
            Some(_) => "signin",
        };
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
            error_code: None,
        }
    }

    fn origin(&self) -> BridgeResult<&str> {
        self.origin
            .as_deref()
            .ok_or_else(|| BridgeError::new("server-required", "Choose a server before enrolling."))
    }
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

/// Known registry only: an unknown registry yields `None`, never a fabricated `false`.
fn known_gateway_presence((gateways, known): (Vec<crate::dto::GatewayView>, bool)) -> Option<bool> {
    known.then_some(!gateways.is_empty())
}

/// Reads gateway presence from the active session for `origin`. No network request is made.
async fn gateway_presence(state: &AppState, origin: &str) -> BridgeResult<Option<bool>> {
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

fn wrong_owner_passphrase() -> BridgeError {
    BridgeError::new(
        "wrong-passphrase",
        "That recovery secret does not match the pending vault.",
    )
}

#[allow(clippy::too_many_arguments)]
async fn create_owner_core<P, PF, C, CF, G, H, HF, A, AF>(
    controller: &std::sync::Mutex<Controller>,
    store: &dyn SecretStore,
    expected_binding: Option<crate::credentials::Binding>,
    mut prompt: P,
    confirm_generated: C,
    generate: G,
    post_claim: H,
    activate: A,
) -> BridgeResult<bool>
where
    P: FnMut(PassphrasePurpose) -> PF,
    PF: std::future::Future<Output = BridgeResult<Option<Zeroizing<String>>>>,
    C: FnOnce(String) -> CF,
    CF: std::future::Future<Output = BridgeResult<bool>>,
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
    let Some(setup_token) = prompt(PassphrasePurpose::SetupToken).await? else {
        return Ok(false);
    };
    if !owner_operation_current(controller, epoch)? {
        return Ok(false);
    }
    let Some(admin_password) = prompt(PassphrasePurpose::AdminPassword).await? else {
        return Ok(false);
    };
    if !owner_operation_current(controller, epoch)? {
        return Ok(false);
    }

    let (retained, resume) = {
        let controller = controller.lock().map_err(|_| BridgeError::host_state())?;
        (controller.owner_passphrase.clone(), controller.owner_resume)
    };
    let passphrase = if let Some(passphrase) = retained {
        passphrase
    } else if resume {
        let Some(passphrase) = prompt(PassphrasePurpose::Unlock).await? else {
            return Ok(false);
        };
        passphrase
    } else {
        let passphrase = generate();
        let message = format!(
            "Save this recovery secret before continuing. It is required to add devices:\n\n{}",
            passphrase.as_str()
        );
        if !confirm_generated(message).await? {
            return Ok(false);
        }
        passphrase
    };
    if !owner_operation_current(controller, epoch)? {
        return Ok(false);
    }

    let (origin, request, checkpoint) = {
        let mut controller = controller.lock().map_err(|_| BridgeError::host_state())?;
        if controller.operation_epoch != epoch {
            return Ok(false);
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
                admin_password.to_string(),
                passphrase.to_string(),
            )
            .map_err(|_| enrollment_error())?;
        let checkpoint = Zeroizing::new(client.checkpoint().map_err(|_| enrollment_error())?);
        controller.owner_passphrase = Some(Zeroizing::new(passphrase.to_string()));
        controller.owner_resume = false;
        (origin, request, checkpoint)
    };
    store.set(&owner_checkpoint_key(&origin), &checkpoint)?;
    if !owner_operation_current(controller, epoch)? {
        return Ok(false);
    }

    let response = post_claim(origin.clone(), request).await?;
    let identity = {
        let controller = controller.lock().map_err(|_| BridgeError::host_state())?;
        if controller.operation_epoch != epoch {
            return Ok(false);
        }
        controller
            .client
            .as_ref()
            .ok_or_else(enrollment_error)?
            .finish_owner_admission(response, passphrase.to_string())
            .map_err(|_| enrollment_error())?
    };
    activate(identity, passphrase, expected_binding).await?;
    if !owner_operation_current(controller, epoch)? {
        return Ok(false);
    }
    tombstone_owner_checkpoint(store, &origin)?;
    let mut controller = controller.lock().map_err(|_| BridgeError::host_state())?;
    if controller.operation_epoch != epoch {
        return Ok(false);
    }
    controller.owner_passphrase = None;
    controller.owner_resume = false;
    controller.ready = true;
    controller.owner = true;
    Ok(true)
}

fn enrollment_error() -> BridgeError {
    BridgeError::new(
        "enrollment",
        "The enrollment response was invalid or no longer matches this device.",
    )
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
        return Err(NetError::Status {
            status: response.status().as_u16(),
            code: None,
        }
        .into());
    }
    String::from_utf8(read_bounded(response, MAX_JSON_BYTES).await?).map_err(|_| enrollment_error())
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
        crate::sync::blocking(move || {
            activation
                .activate(&session.client)
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
    let response: InstanceInfo = serde_json::from_str(
        &body(
            http(&selected)?
                .get(format!("{selected}{discovery_path}"))
                .send()
                .await
                .map_err(|_| NetError::Offline)?,
        )
        .await?,
    )
    .map_err(|_| enrollment_error())?;
    if response.mode == DeploymentMode::Hosted && selected != HOSTED_ORIGIN {
        return Err(BridgeError::new(
            "invalid-origin",
            "Hosted enrollment is only available at Peppy Hosted.",
        ));
    }
    let existing = existing_identity(&state, &selected).await?;
    let existing_role = existing.as_ref().map(ExistingIdentity::role);
    let has_gateway = gateway_presence(&state, &selected).await?;
    let can_resume_pending =
        response.mode == DeploymentMode::Community && !response.claimed && existing.is_none();
    let pending_owner = if can_resume_pending {
        restore_pending_owner(&*state.store, &selected)?
    } else {
        None
    };
    let resumed_pending = pending_owner.is_some();
    // An existing device stays ready with its own credential; only an owner gets an invitation client.
    let client = match (existing, pending_owner) {
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
    controller.has_gateway = has_gateway;
    set_identity_ready(controller, existing_role);
}

/// Returns the unlocked identity for `selected`, if this app already holds one.
async fn existing_identity(
    state: &AppState,
    selected: &str,
) -> BridgeResult<Option<ExistingIdentity>> {
    if state.config()?.active_binding().is_none() {
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
        |purpose| dialogs::passphrase(app_ref, purpose),
        |message| async move {
            dialogs::confirm(
                app_ref,
                "Save vault recovery secret",
                &message,
                "I saved it",
            )
            .await
        },
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
    .await?;

    let has_gateway = if completed {
        refresh_gateway_presence(&state).await?
    } else {
        None
    };
    let mut controller = state
        .enrollment
        .lock()
        .map_err(|_| BridgeError::host_state())?;
    if completed {
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
        let controller = state
            .enrollment
            .lock()
            .map_err(|_| BridgeError::host_state())?;
        (
            controller.origin()?.to_owned(),
            controller
                .client
                .as_ref()
                .ok_or_else(enrollment_error)?
                .manual_challenge_request(None)
                .map_err(|_| enrollment_error())?,
        )
    };
    let client = http(&origin)?;
    let account_session = hosted_account_header(&state, &hosted).await?;
    let challenge_json = post(
        &client,
        &origin,
        enrollment::routes::ENROLLMENT_CHALLENGES,
        challenge_request,
        None,
        account_session.as_ref().map(|value| value.as_str()),
    )
    .await?;
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
pub async fn enrollment_add_phone(
    window: WebviewWindow,
    state: State<'_, AppState>,
    method: EnrollmentMethod,
    hosted: State<'_, crate::hosted::HostedState>,
) -> BridgeResult<EnrollmentView> {
    crate::require_main(window.label())?;
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
    let account = hosted_account_header(&state, &hosted).await?;
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
    controller.invitation = None;
    Ok(controller.view())
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

#[tauri::command]
pub async fn enrollment_approve(
    window: WebviewWindow,
    app: AppHandle,
    state: State<'_, AppState>,
    hosted: State<'_, crate::hosted::HostedState>,
) -> BridgeResult<EnrollmentView> {
    crate::require_main(window.label())?;
    refresh_invitation(&state, &hosted).await?;
    let Some(passphrase) = dialogs::passphrase(&app, PassphrasePurpose::Unlock).await? else {
        return Ok(state
            .enrollment
            .lock()
            .map_err(|_| BridgeError::host_state())?
            .view());
    };
    let (id, request) = {
        let controller = state
            .enrollment
            .lock()
            .map_err(|_| BridgeError::host_state())?;
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
        (
            controller.invitation_id.ok_or_else(enrollment_error)?,
            controller
                .client
                .as_ref()
                .ok_or_else(enrollment_error)?
                .approve_phone_invitation(
                    serde_json::to_string(status).map_err(|_| enrollment_error())?,
                    passphrase.to_string(),
                )
                .map_err(|_| enrollment_error())?,
        )
    };
    let session = state.require_session().await?;
    let origin = session.api.origin().to_owned();
    let account = hosted_account_header(&state, &hosted).await?;
    body(
        device_request(
            &session,
            &origin,
            http(&origin)?
                .post(format!(
                    "{origin}{}/{id}/approve",
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
    if state
        .enrollment
        .lock()
        .map_err(|_| BridgeError::host_state())?
        .invitation_id
        .is_some()
    {
        refresh_invitation(&state, &hosted).await?;
    }
    let has_gateway = refresh_gateway_presence(&state).await?;
    let mut controller = state
        .enrollment
        .lock()
        .map_err(|_| BridgeError::host_state())?;
    controller.has_gateway = has_gateway;
    Ok(controller.view())
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
    controller.client = None;
    controller.owner_passphrase = None;
    controller.owner_resume = false;
    controller.invitation_id = None;
    controller.invitation = None;
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
    use peppy_hosted_api::enrollment::{
        ClaimInstanceRequest, ClaimInstanceResponse, InstanceSession,
    };
    use std::sync::{Arc, Mutex};

    const ORIGIN: &str = "https://community.example";
    const RECOVERY_SECRET: &str = "alpha bravo charlie delta echo foxtrot";

    fn unclaimed_controller() -> Mutex<Controller> {
        let mut controller = Controller::default();
        controller.origin = Some(ORIGIN.into());
        controller.info = Some(InstanceInfo {
            mode: DeploymentMode::Community,
            claimed: false,
            setup_required: true,
            password_auth: true,
            qr_pairing: true,
            shared_secret_pairing: true,
        });
        controller.client = Some(
            EnrollmentClient::new(ORIGIN.into(), ClientKind::Desktop, DEVICE_NAME.into()).unwrap(),
        );
        Mutex::new(controller)
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
            PassphrasePurpose::AdminPassword => "password long enough",
            PassphrasePurpose::Unlock => RECOVERY_SECRET,
            _ => panic!("unexpected prompt: {purpose:?}"),
        };
        Ok(Some(Zeroizing::new(value.into())))
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
            |purpose| std::future::ready(prompt(purpose)),
            |_| std::future::ready(Ok(true)),
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
        assert!(create_owner_core(
            &controller,
            &store,
            None,
            |purpose| std::future::ready(prompt(purpose)),
            |_| std::future::ready(Ok(true)),
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
        .unwrap());

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
            |purpose| {
                std::future::ready(match purpose {
                    PassphrasePurpose::Unlock => Ok(Some(Zeroizing::new(
                        "wrong secret with enough separate words".into(),
                    ))),
                    _ => prompt(purpose),
                })
            },
            |_| std::future::ready(Ok(true)),
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
            move |purpose| {
                if purpose == PassphrasePurpose::SetupToken {
                    cancel_owner_operation(&prompt_controller).unwrap();
                }
                std::future::ready(prompt(purpose))
            },
            |_| std::future::ready(Ok(true)),
            || Zeroizing::new(RECOVERY_SECRET.into()),
            |_, _| std::future::ready(Ok(String::new())),
            move |_, _, _| {
                *activation.lock().unwrap() = "new-owner-profile";
                std::future::ready(Ok(()))
            },
        )
        .await
        .unwrap();

        assert!(!completed);
        assert_eq!(*active_profile.lock().unwrap(), "existing-profile");
        assert!(store.get(&owner_checkpoint_key(ORIGIN)).unwrap().is_none());
    }

    fn ready_controller() -> Controller {
        let mut controller = Controller::default();
        controller.origin = Some(ORIGIN.into());
        controller.info = Some(InstanceInfo {
            mode: DeploymentMode::Community,
            claimed: true,
            setup_required: false,
            password_auth: true,
            qr_pairing: true,
            shared_secret_pairing: true,
        });
        controller
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
