//! Production Peppy Hosted account sign-in, entitlement and first-vault provisioning.
//!
//! This module deliberately keeps hosted credentials on the native side.  The
//! webview receives only `HostedAccountView`.

use crate::{
    credentials::Binding,
    error::{BridgeError, BridgeResult},
    net::{read_bounded, NetError},
    secure_store::SecretStore,
    AppState,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use peppy_hosted_api::{
    auth_web::routes as web_routes, routes, NativeAuthConfigResponse, NativeSessionExchangeRequest,
    SESSION_LIFETIME_SECONDS,
};
use peppy_hosted_client::{
    parse_hosted_account, parse_hosted_session, parse_hosted_session_renewal, prepare, restore,
    HostedProvisioning,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fmt,
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tauri_plugin_opener::OpenerExt;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::Mutex,
};
use zeroize::{Zeroize, Zeroizing};

#[cfg_attr(target_os = "macos", allow(dead_code))]
mod callback_page;

const PRODUCTION_ORIGIN: &str = "https://peppy.pro";
const MAX_BODY: usize = 64 * 1024;
const SESSION_PREFIX: &str = "hosted-session:";
const CHECKPOINT_PREFIX: &str = "hosted-provisioning:";
const PENDING_SECRET_PREFIX: &str = "hosted-pending-secret:v1:";
const RECOVERY_INDEX_PREFIX: &str = "hosted-provisioning-index:v1:";
const MAX_PENDING_SECRET_BYTES: usize = 16 * 1024;
const RENEWAL_SUCCESS_INTERVAL_SECONDS: u64 = 15 * 60;
const RENEWAL_FAILURE_RETRY_SECONDS: u64 = 30;

/// Managed by L8 with `app.manage(HostedState::default())`.
#[derive(Default)]
pub struct HostedState {
    session: Mutex<Option<HostedSessionRecord>>,
    provisioning: Mutex<()>,
    sign_in_generation: AtomicU64,
    account_session_revision: AtomicU64,
    renewal: Mutex<()>,
    last_renewal_failure_at: Mutex<Option<u64>>,
    /// Tests send requests to a local fake server while keeping an HTTPS credential origin.
    #[cfg(test)]
    base_override: std::sync::Mutex<Option<String>>,
    /// Tests isolate provider discovery failures from a valid account endpoint.
    #[cfg(test)]
    auth_config_base_override: std::sync::Mutex<Option<String>>,
}

/// Native-only account session for hosted enrollment headers. It is never an IPC value.
pub(crate) async fn enrollment_account_session(
    state: &AppState,
    hosted: &HostedState,
    origin: &str,
) -> BridgeResult<Zeroizing<String>> {
    if origin != hosted_origin()? {
        return Err(BridgeError::new(
            "invalid-origin",
            "Hosted enrollment is only available at Peppy Hosted.",
        ));
    }
    let session = load_session_from_store(&*state.store, hosted, origin)
        .await?
        .ok_or_else(|| {
            BridgeError::new(
                "hosted-session-required",
                "Sign in again to Peppy Hosted before enrolling.",
            )
        })?;
    if session.expires_at_unix <= now_unix() {
        return Err(BridgeError::new(
            "hosted-session-required",
            "Sign in again to Peppy Hosted before enrolling.",
        ));
    }
    Ok(Zeroizing::new(session.bearer.to_string()))
}

/// Whether hosted enrollment at `origin` has a usable account session. An unreadable session
/// counts as absent: sign-in is then offered again, and device-credential flows are not blocked.
pub(crate) async fn has_enrollment_account_session(
    state: &AppState,
    hosted: &HostedState,
    origin: &str,
) -> bool {
    enrollment_account_session(state, hosted, origin)
        .await
        .is_ok()
}

/// Reuses the reviewed hosted grant/checkpoint/protected-import flow for first owner creation.
pub(crate) async fn provision_enrollment(
    app: &tauri::AppHandle,
    state: &AppState,
    hosted: &HostedState,
) -> BridgeResult<()> {
    let operation = crate::admit_update_operation(state)?;
    let origin = hosted_origin()?;
    provision_core(
        &*state.store,
        hosted,
        &origin,
        || async move {
            crate::dialogs::passphrase(
                app,
                crate::dialogs::PassphrasePurpose::ResumeHostedPassphrase,
            )
            .await
        },
        |credential, passphrase| {
            activate_and_unlock(app, state, &operation, credential, passphrase)
        },
    )
    .await
}

impl HostedState {
    pub(crate) fn begin_sign_in_attempt(&self) -> u64 {
        self.sign_in_generation.fetch_add(1, Ordering::SeqCst) + 1
    }

    pub(crate) async fn capture_mcp_account_session(&self, bearer: &str) -> BridgeResult<u64> {
        let session = self.session.lock().await;
        if session
            .as_ref()
            .is_none_or(|current| current.bearer.as_str() != bearer)
        {
            return Err(BridgeError::new(
                "hosted-account-changed",
                "The Peppy Hosted account changed during MCP enablement.",
            ));
        }
        Ok(self.account_session_revision.load(Ordering::SeqCst))
    }

    pub(crate) fn ensure_mcp_account_session_current(&self, revision: u64) -> BridgeResult<()> {
        if self.account_session_revision.load(Ordering::SeqCst) != revision {
            return Err(BridgeError::new(
                "hosted-account-changed",
                "The Peppy Hosted account changed during MCP enablement.",
            ));
        }
        Ok(())
    }

    fn api_base(&self, origin: &str) -> String {
        #[cfg(test)]
        if let Some(base) = self.base_override.lock().unwrap().clone() {
            return base;
        }
        origin.to_owned()
    }

    fn auth_config_base(&self, origin: &str) -> String {
        #[cfg(test)]
        if let Some(base) = self.auth_config_base_override.lock().unwrap().clone() {
            return base;
        }
        self.api_base(origin)
    }
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HostedAccountView {
    pub available: bool,
    pub providers: Vec<String>,
    pub deletion_pending: bool,
    pub signed_in: bool,
    pub account_label: Option<String>,
    pub classification: Option<String>,
    pub entitlement: Option<String>,
    pub access: Option<String>,
    pub has_vault: bool,
    pub resumable: bool,
}

impl HostedAccountView {
    fn signed_out(providers: Vec<String>) -> Self {
        Self {
            available: !providers.is_empty(),
            providers,
            deletion_pending: false,
            signed_in: false,
            account_label: None,
            classification: None,
            entitlement: None,
            access: None,
            has_vault: false,
            resumable: false,
        }
    }
}

/// Deliberately no `Debug`: bearer tokens must never reach diagnostics.
pub(crate) struct HostedSessionRecord {
    pub(crate) account_id: String,
    pub(crate) bearer: Zeroizing<String>,
    pub(crate) expires_at_unix: u64,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct HostedSessionWire {
    account_id: String,
    bearer: String,
    expires_at_unix: u64,
}
impl fmt::Debug for HostedSessionRecord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HostedSessionRecord")
            .field("account_id", &self.account_id)
            .field("expires_at_unix", &self.expires_at_unix)
            .finish()
    }
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
pub(crate) fn hosted_origin() -> BridgeResult<String> {
    resolve_hosted_origin(std::env::var("PEPPY_HOSTED_ORIGIN").ok().as_deref())
}

fn resolve_hosted_origin(override_origin: Option<&str>) -> BridgeResult<String> {
    #[cfg(debug_assertions)]
    if let Some(value) = override_origin {
        let url = url::Url::parse(value).map_err(|_| {
            BridgeError::new(
                "invalid-origin",
                "The hosted development origin is invalid.",
            )
        })?;
        let loopback = matches!(url.host_str(), Some("127.0.0.1" | "::1" | "[::1]"));
        if (url.scheme() == "https" || url.scheme() == "http" && loopback)
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none()
            && matches!(url.path(), "" | "/")
        {
            return Ok(url.origin().ascii_serialization());
        }
        return Err(BridgeError::new(
            "invalid-origin",
            "The hosted development origin must be HTTPS or loopback HTTP.",
        ));
    }
    #[cfg(not(debug_assertions))]
    let _ = override_origin;
    Ok(PRODUCTION_ORIGIN.into())
}
fn session_key(origin: &str) -> String {
    format!("{SESSION_PREFIX}{origin}")
}
fn checkpoint_key(origin: &str, account_id: &str) -> String {
    format!("{CHECKPOINT_PREFIX}{origin}:{account_id}")
}
fn scoped_key(prefix: &str, parts: &[&str]) -> String {
    let mut digest = Sha256::new();
    for part in parts {
        digest.update(part.as_bytes());
        digest.update([0]);
    }
    format!("{prefix}{:x}", digest.finalize())
}
fn pending_secret_key(origin: &str, account_id: &str) -> String {
    scoped_key(PENDING_SECRET_PREFIX, &[origin, account_id])
}
fn recovery_index_key(binding: &Binding) -> String {
    scoped_key(
        RECOVERY_INDEX_PREFIX,
        &[&binding.origin, &binding.vault_id, &binding.device_id],
    )
}
fn read_text_secret(store: &dyn SecretStore, key: &str) -> BridgeResult<Option<Zeroizing<String>>> {
    let Some(value) = store.get(key)?.filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    if value.len() > MAX_PENDING_SECRET_BYTES {
        return Err(BridgeError::new(
            "vault-secret-unavailable",
            "The stored vault shared secret is invalid.",
        ));
    }
    match String::from_utf8(value.to_vec()) {
        Ok(value) => Ok(Some(Zeroizing::new(value))),
        Err(error) => {
            let mut bytes = error.into_bytes();
            bytes.zeroize();
            Err(BridgeError::new(
                "vault-secret-unavailable",
                "The stored vault shared secret is invalid.",
            ))
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RecoveryIndex {
    version: u8,
    origin: String,
    account_id: String,
    operation_id: String,
    vault_id: String,
    device_id: String,
    generated: bool,
}

fn binding_for(provisioning: &HostedProvisioning) -> Binding {
    let view = provisioning.view();
    Binding {
        origin: view.origin,
        vault_id: view.vault_id,
        device_id: view.device_id,
    }
}

fn recovery_index(provisioning: &HostedProvisioning, generated: bool) -> RecoveryIndex {
    let view = provisioning.view();
    RecoveryIndex {
        version: 1,
        origin: view.origin,
        account_id: view.account_id,
        operation_id: view.operation_id,
        vault_id: view.vault_id,
        device_id: view.device_id,
        generated,
    }
}

fn matching_index(index: &RecoveryIndex, provisioning: &HostedProvisioning) -> bool {
    let view = provisioning.view();
    index.version == 1
        && index.origin == view.origin
        && index.account_id == view.account_id
        && index.operation_id == view.operation_id
        && index.vault_id == view.vault_id
        && index.device_id == view.device_id
}

fn read_recovery_index(
    store: &dyn SecretStore,
    provisioning: &HostedProvisioning,
) -> BridgeResult<Option<RecoveryIndex>> {
    let key = recovery_index_key(&binding_for(provisioning));
    let Some(value) = store.get(&key)?.filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    let index: RecoveryIndex = serde_json::from_slice(&value).map_err(|_| {
        BridgeError::new(
            "vault-secret-unavailable",
            "The stored provisioning recovery record is invalid.",
        )
    })?;
    if !matching_index(&index, provisioning) {
        return Err(BridgeError::new(
            "vault-secret-unavailable",
            "The stored provisioning recovery record does not match the pending vault.",
        ));
    }
    Ok(Some(index))
}
fn hosted_error() -> BridgeError {
    BridgeError::new(
        "hosted-unavailable",
        "Hosted sign-in is currently unavailable.",
    )
}
fn invalid_response() -> BridgeError {
    BridgeError::new(
        "server-response",
        "Peppy Hosted returned an invalid response.",
    )
}
fn client_error<T>(_: T) -> BridgeError {
    invalid_response()
}

fn tombstone(store: &dyn SecretStore, key: &str) -> BridgeResult<()> {
    store.set(key, b"")
}
fn decode_session(bytes: &[u8]) -> Option<HostedSessionRecord> {
    let wire: HostedSessionWire = (!bytes.is_empty())
        .then(|| serde_json::from_slice(bytes).ok())
        .flatten()?;
    Some(HostedSessionRecord {
        account_id: wire.account_id,
        bearer: Zeroizing::new(wire.bearer),
        expires_at_unix: wire.expires_at_unix,
    })
}
fn http(origin: &str) -> BridgeResult<reqwest::Client> {
    crate::tls::http_builder(origin)?
        .redirect(reqwest::redirect::Policy::none())
        .https_only(!crate::origin::is_loopback_http(origin))
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(20))
        .user_agent("Peppy-Desktop")
        .build()
        .map_err(|_| BridgeError::new("network", "Could not create the native network client."))
}
async fn read_response(response: reqwest::Response) -> Result<Vec<u8>, NetError> {
    if !response.status().is_success() {
        return Err(NetError::Status {
            status: response.status().as_u16(),
            code: None,
        });
    }
    read_bounded(response, MAX_BODY).await
}
async fn sign_in_providers(hosted: &HostedState, origin: &str) -> BridgeResult<Vec<String>> {
    let base = hosted.auth_config_base(origin);
    let client = http(&base)?;
    let response = client
        .get(format!("{base}{}", routes::AUTH_CONFIG))
        .send()
        .await
        .map_err(|_| NetError::Offline)?;
    let response = read_response(response).await?;
    let config: NativeAuthConfigResponse =
        serde_json::from_slice(&response).map_err(|_| NetError::Invalid)?;
    Ok(config
        .available_providers
        .into_iter()
        .map(|provider| provider.to_string())
        .collect())
}

async fn begin_sign_in(hosted: &HostedState, origin: &str, provider: &str) -> BridgeResult<u64> {
    let generation = hosted.begin_sign_in_attempt();
    if !sign_in_providers(hosted, origin)
        .await?
        .iter()
        .any(|available| available == provider)
    {
        return Err(hosted_error());
    }
    ensure_sign_in_current(hosted, generation)?;
    Ok(generation)
}

fn ensure_sign_in_current(hosted: &HostedState, generation: u64) -> BridgeResult<()> {
    if hosted.sign_in_generation.load(Ordering::SeqCst) != generation {
        return Err(BridgeError::new(
            "hosted-sign-in-cancelled",
            "The sign-in attempt was replaced.",
        ));
    }
    Ok(())
}

async fn signed_out_view(hosted: &HostedState, origin: &str) -> BridgeResult<HostedAccountView> {
    Ok(HostedAccountView::signed_out(
        sign_in_providers(hosted, origin).await?,
    ))
}
async fn authenticated(
    client: &reqwest::Client,
    origin: &str,
    path: &str,
    bearer: &str,
) -> Result<Vec<u8>, NetError> {
    let response = client
        .get(format!("{origin}{path}"))
        .bearer_auth(bearer)
        .send()
        .await
        .map_err(|_| NetError::Offline)?;
    if response.status() == reqwest::StatusCode::UNAUTHORIZED {
        return Err(NetError::Revoked);
    }
    read_response(response).await
}

async fn renew_session(
    store: &dyn SecretStore,
    hosted: &HostedState,
    origin: &str,
) -> BridgeResult<()> {
    let _renewal = hosted.renewal.lock().await;
    let generation = hosted.sign_in_generation.load(Ordering::SeqCst);
    let session = match load_session_from_store(store, hosted, origin).await {
        Ok(session) => session,
        Err(error) => {
            *hosted.last_renewal_failure_at.lock().await = Some(now_unix());
            return Err(error);
        }
    };
    let Some(session) = session else {
        *hosted.last_renewal_failure_at.lock().await = Some(now_unix());
        return Err(BridgeError::new(
            "hosted-session-required",
            "Sign in again to Peppy Hosted.",
        ));
    };
    let now = now_unix();
    if session.expires_at_unix.saturating_sub(now)
        > u64::from(SESSION_LIFETIME_SECONDS).saturating_sub(RENEWAL_SUCCESS_INTERVAL_SECONDS)
    {
        return Ok(());
    }
    if hosted
        .last_renewal_failure_at
        .lock()
        .await
        .is_some_and(|failed_at| now.saturating_sub(failed_at) < RENEWAL_FAILURE_RETRY_SECONDS)
    {
        return Ok(());
    }

    let base = hosted.api_base(origin);
    let client = http(&base)?;
    let response = client
        .post(format!("{base}{}", routes::AUTH_SESSION_RENEW))
        .bearer_auth(&*session.bearer)
        .send()
        .await
        .map_err(|_| BridgeError::new("offline", "Could not reach Peppy Hosted."));
    let response = match response {
        Ok(response) => response,
        Err(error) => {
            *hosted.last_renewal_failure_at.lock().await = Some(now);
            return Err(error);
        }
    };
    if response.status() == reqwest::StatusCode::UNAUTHORIZED {
        if let Err(error) =
            tombstone_observed_session(store, hosted, origin, &session.bearer, generation).await
        {
            *hosted.last_renewal_failure_at.lock().await = Some(now);
            return Err(error);
        }
        *hosted.last_renewal_failure_at.lock().await = Some(now);
        return Err(BridgeError::new(
            "hosted-session-required",
            "Your Peppy Hosted sign-in expired. Sign in again to continue.",
        ));
    }
    let bytes = match read_response(response).await {
        Ok(bytes) => bytes,
        Err(error) => {
            *hosted.last_renewal_failure_at.lock().await = Some(now);
            return Err(error.into());
        }
    };
    let expires_in_seconds = match String::from_utf8(bytes)
        .map_err(|_| invalid_response())
        .and_then(|body| {
            parse_hosted_session_renewal(body, session.account_id.clone()).map_err(client_error)
        }) {
        Ok(expires) => expires,
        Err(error) => {
            *hosted.last_renewal_failure_at.lock().await = Some(now);
            return Err(error);
        }
    };
    if let Err(error) = replace_observed_session_expiry(
        store,
        hosted,
        origin,
        &session.bearer,
        generation,
        now.saturating_add(u64::from(expires_in_seconds)),
    )
    .await
    {
        *hosted.last_renewal_failure_at.lock().await = Some(now);
        return Err(error);
    }
    *hosted.last_renewal_failure_at.lock().await = None;
    Ok(())
}

#[tauri::command]
pub async fn hosted_account(
    window: tauri::WebviewWindow,
    state: tauri::State<'_, AppState>,
    hosted: tauri::State<'_, HostedState>,
) -> BridgeResult<HostedAccountView> {
    let _operation = crate::admit_update_operation(&state)?;
    crate::require_main(window.label())?;
    account_view(&*state.store, &hosted, &hosted_origin()?).await
}

#[tauri::command]
pub async fn hosted_session_renew(
    window: tauri::WebviewWindow,
    state: tauri::State<'_, AppState>,
    hosted: tauri::State<'_, HostedState>,
) -> BridgeResult<()> {
    let _operation = crate::admit_update_operation(&state)?;
    crate::require_main(window.label())?;
    renew_session(&*state.store, &hosted, &hosted_origin()?).await
}

/// Returns the native-validated Peppy Hosted public origin without accepting renderer input.
#[tauri::command]
pub fn hosted_origin_url(window: tauri::WebviewWindow) -> BridgeResult<String> {
    crate::require_main(window.label())?;
    hosted_origin()
}

#[tauri::command]
pub async fn hosted_sign_out(
    window: tauri::WebviewWindow,
    state: tauri::State<'_, AppState>,
    hosted: tauri::State<'_, HostedState>,
) -> BridgeResult<()> {
    let _operation = crate::admit_update_operation(&state)?;
    crate::require_main(window.label())?;
    tombstone_session(&*state.store, &hosted, &hosted_origin()?).await
}

#[tauri::command]
pub(crate) async fn hosted_delete_account(
    window: tauri::WebviewWindow,
    state: tauri::State<'_, AppState>,
    hosted: tauri::State<'_, HostedState>,
) -> BridgeResult<String> {
    let _operation = crate::admit_update_operation(&state)?;
    crate::require_main(window.label())?;
    delete_account_core(&*state.store, &hosted, &hosted_origin()?).await
}

/// Deletes the current Hosted account and discards only the observed account session on success.
pub(crate) async fn delete_account_core(
    store: &dyn SecretStore,
    hosted: &HostedState,
    origin: &str,
) -> BridgeResult<String> {
    let Some(session) = load_session_from_store(store, hosted, origin).await? else {
        return Err(BridgeError::new(
            "hosted-session-required",
            "Sign in again to Peppy Hosted before deleting your account.",
        ));
    };
    let generation = hosted.sign_in_generation.load(Ordering::SeqCst);
    let base = hosted.api_base(origin);
    let response = http(&base)?
        .delete(format!("{base}{}", routes::ACCOUNT))
        .bearer_auth(&*session.bearer)
        .send()
        .await
        .map_err(|_| BridgeError::new("offline", "Could not reach Peppy Hosted."))?;

    match response.status() {
        reqwest::StatusCode::NO_CONTENT => {
            tombstone_observed_session(store, hosted, origin, &session.bearer, generation).await?;
            Ok("deleted".into())
        }
        reqwest::StatusCode::ACCEPTED => Ok("pending".into()),
        _ => Err(BridgeError::new(
            "account-deletion-failed",
            "Peppy Hosted could not delete your account. Please try again.",
        )),
    }
}

#[tauri::command]
pub fn hosted_open_billing(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
) -> BridgeResult<()> {
    crate::require_main(window.label())?;
    app.opener()
        .open_url(
            format!("{}/account/subscribe", hosted_origin()?),
            None::<String>,
        )
        .map_err(|_| BridgeError::new("open-url", "Could not open the subscription page."))
}

#[tauri::command]
pub async fn hosted_sign_in(
    window: tauri::WebviewWindow,
    #[cfg_attr(target_os = "macos", allow(unused_variables))] app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    hosted: tauri::State<'_, HostedState>,
    provider: String,
) -> BridgeResult<HostedAccountView> {
    let _operation = crate::admit_update_operation(&state)?;
    crate::require_main(window.label())?;
    let origin = hosted_origin()?;
    let generation = begin_sign_in(&hosted, &origin, &provider).await?;
    ensure_sign_in_current(&hosted, generation)?;
    let mut verifier = Zeroizing::new(random_b64url(32)?);
    let state_token = Zeroizing::new(random_b64url(32)?);
    let challenge = pkce_challenge(&verifier);
    #[cfg(target_os = "macos")]
    let (return_to, _loopback): (String, Option<Loopback>) =
        ("org.peppy.desktop:/auth/callback".to_owned(), None);
    #[cfg(not(target_os = "macos"))]
    let (return_to, loopback) = {
        let loopback = Loopback::bind().await?;
        (format!("{}/callback", loopback.uri), Some(loopback))
    };
    let auth = app_authorize_url(&origin, &challenge, &state_token, &provider, &return_to)?;
    #[cfg(target_os = "macos")]
    let code = authenticate_with_system_browser(&window, auth, &state_token).await?;
    #[cfg(not(target_os = "macos"))]
    let code = {
        app.opener()
            .open_url(auth, None::<String>)
            .map_err(|_| BridgeError::new("open-url", "Could not open Hosted sign-in."))?;
        loopback
            .expect("loopback exists off macOS")
            .receive(&state_token)
            .await?
    };
    ensure_sign_in_current(&hosted, generation)?;
    let session_request = native_session_request(&code, &verifier)?;
    verifier.zeroize();
    let client = http(&origin)?;
    let session_body = read_response(
        client
            .post(format!("{origin}{}", routes::AUTH_NATIVE_SESSION))
            .header("content-type", "application/json")
            .body(session_request)
            .send()
            .await
            .map_err(|_| BridgeError::new("offline", "Could not reach Peppy Hosted."))?,
    )
    .await
    .map_err(BridgeError::from)?;
    let parsed =
        parse_hosted_session(String::from_utf8(session_body).map_err(|_| invalid_response())?)
            .map_err(client_error)?;
    let record = HostedSessionRecord {
        account_id: parsed.account_id(),
        bearer: Zeroizing::new(parsed.bearer_token()),
        expires_at_unix: now_unix().saturating_add(parsed.expires_in_seconds() as u64),
    };
    commit_sign_in_session(&*state.store, &hosted, &origin, generation, record).await?;
    account_view(&*state.store, &hosted, &origin).await
}

#[tauri::command]
pub async fn hosted_provision(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    hosted: tauri::State<'_, HostedState>,
) -> BridgeResult<()> {
    crate::require_main(window.label())?;
    provision_enrollment(&app, &state, &hosted).await
}

/// Activates the provisioned credential through the shared import path, then unlocks with the
/// passphrase the person just chose so they are not prompted twice.
async fn activate_and_unlock(
    app: &tauri::AppHandle,
    state: &AppState,
    operation: &crate::update_safety::OperationGuard,
    credential: Zeroizing<Vec<u8>>,
    passphrase: Zeroizing<String>,
) -> BridgeResult<()> {
    crate::activate_native_credential(app, state, operation, credential).await?;
    let native = state.require_session_admitted(operation).await?;
    let vault = crate::fetch_vault(
        &native.api,
        &native.binding.vault_id,
        &native.binding.device_id,
    )
    .await?;
    let (profile, header) = crate::vault_header(&vault)?;
    crate::unlock_with(
        state,
        &native,
        profile,
        header,
        passphrase,
        vault.profile_fingerprint,
    )
    .await
}

fn persist_checkpoint(
    store: &dyn SecretStore,
    key: &str,
    value: &HostedProvisioning,
) -> BridgeResult<()> {
    let bytes = Zeroizing::new(value.checkpoint().map_err(client_error)?);
    store.set(key, &bytes)
}
async fn post_bearer_status(
    client: &reqwest::Client,
    origin: &str,
    path: &str,
    bearer: &str,
    body: String,
) -> Result<Vec<u8>, (u16, BridgeError)> {
    let response = client
        .post(format!("{origin}{path}"))
        .bearer_auth(bearer)
        .header("content-type", "application/json")
        .body(body)
        .send()
        .await
        .map_err(|_| {
            (
                0,
                BridgeError::new("offline", "Could not reach Peppy Hosted."),
            )
        })?;
    if response.status() == reqwest::StatusCode::FORBIDDEN {
        return Err((
            403,
            BridgeError::new(
                "entitlement-required",
                "An active Peppy Hosted subscription is required.",
            ),
        ));
    }
    let status = response.status().as_u16();
    read_response(response)
        .await
        .map_err(|error| (status, BridgeError::from(error)))
}

/// Pure seam: load account state without coupling existing sessions to sign-in availability.
pub(crate) async fn account_view(
    store: &dyn SecretStore,
    hosted: &HostedState,
    origin: &str,
) -> BridgeResult<HostedAccountView> {
    let Some(session) = load_session_from_store(store, hosted, origin).await? else {
        return signed_out_view(hosted, origin).await;
    };
    let base = hosted.api_base(origin);
    let client = http(&base)?;
    let generation = hosted.sign_in_generation.load(Ordering::SeqCst);
    let bytes = match authenticated(&client, &base, routes::ACCOUNT, &session.bearer).await {
        Ok(bytes) => bytes,
        Err(NetError::Revoked) => {
            tombstone_observed_session(store, hosted, origin, &session.bearer, generation).await?;
            return signed_out_view(hosted, origin).await;
        }
        Err(error) => return Err(error.into()),
    };
    let account = parse_hosted_account(
        String::from_utf8(bytes).map_err(|_| invalid_response())?,
        session.account_id.clone(),
    )
    .map_err(client_error)?;
    let resumable = store
        .get(&checkpoint_key(origin, &session.account_id))?
        .is_some_and(|value| !value.is_empty());
    let providers = sign_in_providers(hosted, origin).await.unwrap_or_default();
    Ok(HostedAccountView {
        available: !providers.is_empty(),
        providers,
        // The current native account DTO does not expose deletion state yet.
        deletion_pending: false,
        signed_in: true,
        account_label: None,
        classification: Some(account.classification),
        entitlement: Some(account.entitlement),
        access: Some(account.access),
        has_vault: account.vault_id.is_some(),
        resumable,
    })
}

/// Pure seam: provisioning with pluggable passphrase and activation callbacks.
pub(crate) async fn provision_core<P, PF, A, AF>(
    store: &dyn SecretStore,
    hosted: &HostedState,
    origin: &str,
    passphrase: P,
    activate_and_unlock: A,
) -> BridgeResult<()>
where
    P: FnOnce() -> PF,
    PF: std::future::Future<Output = BridgeResult<Option<Zeroizing<String>>>>,
    A: FnOnce(Zeroizing<Vec<u8>>, Zeroizing<String>) -> AF,
    AF: std::future::Future<Output = BridgeResult<()>>,
{
    let _provisioning = hosted.provisioning.lock().await;
    let Some(session) = load_session_from_store(store, hosted, origin).await? else {
        return Err(BridgeError::new(
            "hosted-session-required",
            "Sign in again to Peppy Hosted.",
        ));
    };

    let base = hosted.api_base(origin);
    let client = http(&base)?;
    let account_bytes = match authenticated(&client, &base, routes::ACCOUNT, &session.bearer).await
    {
        Ok(bytes) => bytes,
        Err(NetError::Revoked) => {
            return Err(session_expired(store, hosted, origin, &session.bearer).await)
        }
        Err(error) => return Err(error.into()),
    };
    let account = parse_hosted_account(
        String::from_utf8(account_bytes).map_err(|_| invalid_response())?,
        session.account_id.clone(),
    )
    .map_err(client_error)?;

    let key = checkpoint_key(origin, &session.account_id);
    let pending_key = pending_secret_key(origin, &session.account_id);
    let stored = store.get(&key)?.filter(|value| !value.is_empty());
    let restored = match stored.as_ref() {
        Some(value) => Some(
            restore(value.to_vec(), origin, &session.account_id).map_err(|_| {
                BridgeError::new(
                    "hosted-checkpoint-invalid",
                    "The pending Hosted provisioning record is invalid.",
                )
            })?,
        ),
        None => None,
    };
    // A vault owned by this account is only recoverable here when it is the one this desktop
    // was creating: the server replays a consumed operation for the same grant and payload.
    let replaying = match (&account.vault_id, &restored) {
        (Some(vault), Some(pending)) if *vault == pending.view().vault_id => true,
        (Some(_), _) => {
            return Err(BridgeError::new(
                "has-vault",
                "This Peppy Hosted account already has a vault.",
            ));
        }
        (None, _) => false,
    };
    if !replaying && account.access != "read_write" {
        return Err(BridgeError::new(
            "entitlement-required",
            "An active Peppy Hosted subscription is required.",
        ));
    }

    let mut recovered_legacy_secret = false;
    let mut has_recovery_index = false;
    let (provisioning, passphrase, generated) = match restored {
        Some(value) => {
            let binding = binding_for(&value);
            let index = read_recovery_index(store, &value)?;
            has_recovery_index = index.is_some();
            let stored_secret = match read_text_secret(store, &pending_key)? {
                Some(secret) => Some(secret),
                None => read_text_secret(store, &binding.vault_secret_account())?,
            };
            let secret = match stored_secret {
                Some(secret) => secret,
                None if index.is_some_and(|index| index.generated) => {
                    return Err(BridgeError::new(
                        "vault-secret-unavailable",
                        "The generated vault shared secret is unavailable on this device.",
                    ));
                }
                None => {
                    recovered_legacy_secret = true;
                    let Some(secret) = passphrase().await? else {
                        return Ok(());
                    };
                    secret
                }
            };
            if !value.passphrase_matches(&secret).map_err(client_error)? {
                return Err(BridgeError::new(
                    "wrong-passphrase",
                    "That passphrase does not match the pending vault.",
                ));
            }
            (value, secret, false)
        }
        None => {
            let secret = match read_text_secret(store, &pending_key)? {
                Some(secret) => secret,
                None => Zeroizing::new(peppy_crypto::passphrase::generate_passphrase()),
            };
            if !peppy_crypto::passphrase::passphrase_acceptable(&secret) {
                return Err(BridgeError::new(
                    "vault-secret-unavailable",
                    "The stored vault shared secret is invalid.",
                ));
            }
            let provisioning = prepare(
                origin,
                &session.account_id,
                &secret,
                peppy_hosted_api::enrollment::ClientKind::Desktop,
                "Peppy Desktop",
            )
            .map_err(client_error)?;
            (provisioning, secret, true)
        }
    };
    if generated || recovered_legacy_secret {
        // This must happen before either the checkpoint or any replayable request.
        store.set(&pending_key, passphrase.as_bytes())?;
    }
    if !has_recovery_index {
        let binding = binding_for(&provisioning);
        let index = Zeroizing::new(
            serde_json::to_vec(&recovery_index(&provisioning, generated)).map_err(|_| {
                BridgeError::new(
                    "vault-secret-unavailable",
                    "Could not save provisioning recovery data.",
                )
            })?,
        );
        store.set(&recovery_index_key(&binding), &index)?;
    }
    persist_checkpoint(store, &key, &provisioning)?;

    let complete = |provisioning: &HostedProvisioning| {
        let body = provisioning
            .complete_request()
            .map_err(|error| (0, client_error(error)));
        let (client, base, bearer) = (&client, &base, &session.bearer);
        async move {
            post_bearer_status(client, base, routes::PROVISIONING_COMPLETE, bearer, body?).await
        }
    };
    request_grant(
        store,
        hosted,
        origin,
        &client,
        &base,
        &session,
        &key,
        &provisioning,
    )
    .await?;
    let done = match complete(&provisioning).await {
        Ok(done) => done,
        // An expired or replaced grant is rejected as unauthorized or conflicting. Retry once
        // with a fresh grant for the same operation and material; never more than once.
        Err((401 | 409 | 410, _)) if !replaying => {
            provisioning.clear_grant().map_err(client_error)?;
            request_grant(
                store,
                hosted,
                origin,
                &client,
                &base,
                &session,
                &key,
                &provisioning,
            )
            .await?;
            complete(&provisioning).await.map_err(|(_, error)| error)?
        }
        Err((_, error)) => return Err(error),
    };

    let credential = provisioning
        .credential_json(String::from_utf8(done).map_err(|_| invalid_response())?)
        .map_err(client_error)?;
    let cleanup_secret = passphrase.clone();
    activate_and_unlock(Zeroizing::new(credential.into_bytes()), passphrase).await?;
    hand_off_and_cleanup(store, &key, &pending_key, &provisioning, &cleanup_secret);
    Ok(())
}

/// A completed activation is never turned into a retry error by secure-store trouble. Cleanup is
/// permitted only after the verified binding contains exactly the secret used for activation.
fn hand_off_and_cleanup(
    store: &dyn SecretStore,
    checkpoint_key: &str,
    pending_key: &str,
    provisioning: &HostedProvisioning,
    secret: &Zeroizing<String>,
) {
    let binding_account = binding_for(provisioning).vault_secret_account();
    let handoff_complete =
        crate::enrollment::save_verified_vault_secret_if_empty(store, &binding_account, secret)
            .is_ok()
            && read_text_secret(store, &binding_account)
                .ok()
                .flatten()
                .is_some_and(|stored| stored.as_str() == secret.as_str());
    if !handoff_complete || tombstone(store, checkpoint_key).is_err() {
        return;
    }

    let pending = match read_text_secret(store, pending_key) {
        Ok(pending) => pending,
        Err(_) => return,
    };
    if pending.is_some_and(|pending| pending.as_str() == secret.as_str())
        && tombstone(store, pending_key).is_err()
    {
        return;
    }

    let index_key = recovery_index_key(&binding_for(provisioning));
    if read_recovery_index(store, provisioning)
        .ok()
        .flatten()
        .is_some()
    {
        let _ = tombstone(store, &index_key);
    }
}

/// Repairs only the recovery record bound to an already unlocked owner identity. This does not
/// consult an OAuth account session: the owner header verifier is the authority for the move.
pub(crate) fn sweep_verified_pending_secret(
    store: &dyn SecretStore,
    binding: &Binding,
    verify: impl Fn(&str) -> bool,
) -> BridgeResult<()> {
    let index_key = recovery_index_key(binding);
    let Some(bytes) = store.get(&index_key)?.filter(|value| !value.is_empty()) else {
        return Ok(());
    };
    let index: RecoveryIndex = serde_json::from_slice(&bytes).map_err(|_| {
        BridgeError::new(
            "vault-secret-unavailable",
            "The stored provisioning recovery record is invalid.",
        )
    })?;
    if index.version != 1
        || index.origin != binding.origin
        || index.vault_id != binding.vault_id
        || index.device_id != binding.device_id
    {
        return Ok(());
    }
    let checkpoint_key = checkpoint_key(&index.origin, &index.account_id);
    if let Some(checkpoint) = store
        .get(&checkpoint_key)?
        .filter(|value| !value.is_empty())
    {
        let provisioning =
            restore(checkpoint.to_vec(), &index.origin, &index.account_id).map_err(|_| {
                BridgeError::new(
                    "vault-secret-unavailable",
                    "The pending Hosted provisioning record is invalid.",
                )
            })?;
        if !matching_index(&index, &provisioning) || binding_for(&provisioning) != *binding {
            return Ok(());
        }
    }
    let pending_key = pending_secret_key(&index.origin, &index.account_id);
    let Some(secret) = read_text_secret(store, &pending_key)? else {
        return Ok(());
    };
    let binding_account = binding.vault_secret_account();
    if let Some(stored) = read_text_secret(store, &binding_account)? {
        if stored.as_str() != secret.as_str() {
            return Ok(());
        }
    } else if verify(&secret) {
        crate::enrollment::save_verified_vault_secret_if_empty(store, &binding_account, &secret)?;
        if !read_text_secret(store, &binding_account)?
            .is_some_and(|stored| stored.as_str() == secret.as_str())
        {
            return Ok(());
        }
    } else {
        return Ok(());
    }
    // Durable confirmation exists; cleanup only this exact recovery tuple.
    tombstone_if_matching_checkpoint(store, &checkpoint_key, &index)?;
    if read_text_secret(store, &pending_key)?
        .is_some_and(|stored| stored.as_str() == secret.as_str())
    {
        tombstone(store, &pending_key)?;
    }
    tombstone(store, &index_key)
}

fn tombstone_if_matching_checkpoint(
    store: &dyn SecretStore,
    key: &str,
    index: &RecoveryIndex,
) -> BridgeResult<()> {
    let Some(checkpoint) = store.get(key)?.filter(|value| !value.is_empty()) else {
        return Ok(());
    };
    let provisioning =
        restore(checkpoint.to_vec(), &index.origin, &index.account_id).map_err(|_| {
            BridgeError::new(
                "hosted-checkpoint-invalid",
                "The pending Hosted provisioning record is invalid.",
            )
        })?;
    if matching_index(index, &provisioning) {
        tombstone(store, key)?;
    }
    Ok(())
}

/// Requests a grant when the operation has none and records it in the durable checkpoint. A
/// rejected bearer means the hosted session ended, so it is discarded and sign-in is required.
#[allow(clippy::too_many_arguments)]
async fn request_grant(
    store: &dyn SecretStore,
    hosted: &HostedState,
    origin: &str,
    client: &reqwest::Client,
    base: &str,
    session: &HostedSessionRecord,
    key: &str,
    provisioning: &HostedProvisioning,
) -> BridgeResult<()> {
    if provisioning.has_grant() {
        return Ok(());
    }
    let body = provisioning.grant_request().map_err(client_error)?;
    let grant = match post_bearer_status(client, base, routes::PROVISIONING, &session.bearer, body)
        .await
    {
        Ok(grant) => grant,
        Err((401, _)) => return Err(session_expired(store, hosted, origin, &session.bearer).await),
        Err((_, error)) => return Err(error),
    };
    provisioning
        .accept_grant(String::from_utf8(grant).map_err(|_| invalid_response())?)
        .map_err(client_error)?;
    persist_checkpoint(store, key, provisioning)
}

async fn session_expired(
    store: &dyn SecretStore,
    hosted: &HostedState,
    origin: &str,
    observed_bearer: &str,
) -> BridgeError {
    let generation = hosted.sign_in_generation.load(Ordering::SeqCst);
    if let Err(error) =
        tombstone_observed_session(store, hosted, origin, observed_bearer, generation).await
    {
        return error;
    }
    BridgeError::new(
        "hosted-session-required",
        "Your Peppy Hosted sign-in expired. Sign in again to continue.",
    )
}

async fn load_session_from_store(
    store: &dyn SecretStore,
    hosted: &HostedState,
    origin: &str,
) -> BridgeResult<Option<HostedSessionRecord>> {
    let mut cached = hosted.session.lock().await;
    if cached
        .as_ref()
        .is_some_and(|value| value.expires_at_unix > now_unix())
    {
        let value = cached.as_ref().expect("checked cached session");
        return Ok(Some(copy_session(value)));
    }
    let key = session_key(origin);
    let Some(bytes) = store.get(&key)? else {
        *cached = None;
        return Ok(None);
    };
    let Some(record) = decode_session(&bytes) else {
        *cached = None;
        return Ok(None);
    };
    *cached = Some(copy_session(&record));
    Ok(Some(record))
}

pub(crate) async fn tombstone_session(
    store: &dyn SecretStore,
    hosted: &HostedState,
    origin: &str,
) -> BridgeResult<()> {
    let mut cached = hosted.session.lock().await;
    tombstone(store, &session_key(origin))?;
    hosted.sign_in_generation.fetch_add(1, Ordering::SeqCst);
    hosted
        .account_session_revision
        .fetch_add(1, Ordering::SeqCst);
    *cached = None;
    Ok(())
}

fn copy_session(record: &HostedSessionRecord) -> HostedSessionRecord {
    HostedSessionRecord {
        account_id: record.account_id.clone(),
        bearer: Zeroizing::new(record.bearer.to_string()),
        expires_at_unix: record.expires_at_unix,
    }
}

pub(crate) async fn commit_sign_in_session(
    store: &dyn SecretStore,
    hosted: &HostedState,
    origin: &str,
    generation: u64,
    record: HostedSessionRecord,
) -> BridgeResult<()> {
    let mut cached = hosted.session.lock().await;
    if hosted.sign_in_generation.load(Ordering::SeqCst) != generation {
        return Err(BridgeError::new(
            "hosted-sign-in-cancelled",
            "The sign-in attempt was replaced.",
        ));
    }
    let encoded = Zeroizing::new(
        serde_json::to_vec(&HostedSessionWire {
            account_id: record.account_id.clone(),
            bearer: record.bearer.to_string(),
            expires_at_unix: record.expires_at_unix,
        })
        .map_err(|_| BridgeError::new("host-state", "Could not save the hosted session."))?,
    );
    store.set(&session_key(origin), &encoded)?;
    *cached = Some(record);
    hosted
        .account_session_revision
        .fetch_add(1, Ordering::SeqCst);
    Ok(())
}

async fn tombstone_observed_session(
    store: &dyn SecretStore,
    hosted: &HostedState,
    origin: &str,
    observed_bearer: &str,
    observed_generation: u64,
) -> BridgeResult<bool> {
    let mut cached = hosted.session.lock().await;
    if hosted.sign_in_generation.load(Ordering::SeqCst) != observed_generation {
        return Ok(false);
    }
    let stored = store.get(&session_key(origin))?;
    let Some(stored) = stored.and_then(|value| decode_session(&value)) else {
        return Ok(false);
    };
    if stored.bearer.as_str() != observed_bearer
        || cached
            .as_ref()
            .is_some_and(|session| session.bearer.as_str() != observed_bearer)
    {
        return Ok(false);
    }
    tombstone(store, &session_key(origin))?;
    *cached = None;
    hosted
        .account_session_revision
        .fetch_add(1, Ordering::SeqCst);
    Ok(true)
}

async fn replace_observed_session_expiry(
    store: &dyn SecretStore,
    hosted: &HostedState,
    origin: &str,
    observed_bearer: &str,
    observed_generation: u64,
    expires_at_unix: u64,
) -> BridgeResult<bool> {
    let mut cached = hosted.session.lock().await;
    if hosted.sign_in_generation.load(Ordering::SeqCst) != observed_generation {
        return Ok(false);
    }
    let Some(stored) = store
        .get(&session_key(origin))?
        .and_then(|value| decode_session(&value))
    else {
        return Ok(false);
    };
    if stored.bearer.as_str() != observed_bearer
        || cached
            .as_ref()
            .is_some_and(|session| session.bearer.as_str() != observed_bearer)
    {
        return Ok(false);
    }
    let renewed = HostedSessionRecord {
        account_id: stored.account_id,
        bearer: Zeroizing::new(observed_bearer.to_owned()),
        expires_at_unix,
    };
    let encoded = Zeroizing::new(
        serde_json::to_vec(&HostedSessionWire {
            account_id: renewed.account_id.clone(),
            bearer: renewed.bearer.to_string(),
            expires_at_unix: renewed.expires_at_unix,
        })
        .map_err(|_| BridgeError::new("host-state", "Could not save the hosted session."))?,
    );
    store.set(&session_key(origin), &encoded)?;
    *cached = Some(renewed);
    Ok(true)
}

#[cfg_attr(target_os = "macos", allow(dead_code))]
struct Loopback {
    listener: TcpListener,
    uri: String,
}
#[cfg_attr(target_os = "macos", allow(dead_code))]
impl Loopback {
    async fn bind() -> BridgeResult<Self> {
        let listener = TcpListener::bind("127.0.0.1:0").await.map_err(|_| {
            BridgeError::new(
                "hosted-sign-in",
                "Could not start the secure sign-in callback.",
            )
        })?;
        Ok(Self {
            uri: format!(
                "http://127.0.0.1:{}",
                listener
                    .local_addr()
                    .map_err(|_| BridgeError::new(
                        "hosted-sign-in",
                        "Could not start the secure sign-in callback."
                    ))?
                    .port()
            ),
            listener,
        })
    }
    async fn receive(self, expected: &str) -> BridgeResult<Zeroizing<String>> {
        const CALLBACK_DEADLINE: Duration = Duration::from_secs(300);
        const REQUEST_LINE_DEADLINE: Duration = Duration::from_secs(10);
        const MAX_REQUEST_LINE_BYTES: usize = 4096;
        let deadline = tokio::time::Instant::now() + CALLBACK_DEADLINE;
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            let Ok(accepted) = tokio::time::timeout(remaining, self.listener.accept()).await else {
                return Err(BridgeError::new(
                    "hosted-sign-in-timeout",
                    "Hosted sign-in timed out.",
                ));
            };
            let Ok((mut stream, _)) = accepted else {
                return Err(BridgeError::new(
                    "hosted-sign-in",
                    "Could not receive the sign-in callback.",
                ));
            };
            let line = match read_callback_request_line(
                &mut stream,
                REQUEST_LINE_DEADLINE.min(remaining),
                MAX_REQUEST_LINE_BYTES,
            )
            .await
            {
                Ok(line) => line,
                // Browsers and local software can connect before the callback; do not let one
                // malformed, fragmented, or oversized request consume the sign-in listener.
                Err(()) => continue,
            };
            let (page, result) = match classify_callback_request(&line, expected) {
                // Probes, favicon fetches and stale or forged callbacks must not end the sign-in.
                CallbackRequest::Unrelated => continue,
                CallbackRequest::Cancelled => (
                    callback_page::CallbackOutcome::Cancelled,
                    Err(cancelled_error()),
                ),
                CallbackRequest::Code(code) => (callback_page::CallbackOutcome::Received, Ok(code)),
                CallbackRequest::MissingCode => (
                    callback_page::CallbackOutcome::Received,
                    Err(BridgeError::new(
                        "hosted-sign-in",
                        "Hosted sign-in did not return a code.",
                    )),
                ),
            };
            let _ = stream
                .write_all(callback_page::response(page).as_bytes())
                .await;
            return result;
        }
    }
}

/// What one loopback request line means for the pending sign-in.
#[derive(Debug, PartialEq, Eq)]
#[cfg_attr(target_os = "macos", allow(dead_code))]
enum CallbackRequest {
    /// Not this sign-in's callback: malformed, another path, or without the expected `state`.
    Unrelated,
    Cancelled,
    Code(Zeroizing<String>),
    MissingCode,
}

/// Only a request to `/callback` carrying this sign-in's `state` may finish it, so an unrelated local
/// connection can neither cancel the sign-in nor substitute a code.
#[cfg_attr(target_os = "macos", allow(dead_code))]
fn classify_callback_request(line: &str, expected_state: &str) -> CallbackRequest {
    let Some(target) = line.split_whitespace().nth(1) else {
        return CallbackRequest::Unrelated;
    };
    let Ok(url) = url::Url::parse(&format!("http://127.0.0.1{target}")) else {
        return CallbackRequest::Unrelated;
    };
    if url.path() != "/callback" {
        return CallbackRequest::Unrelated;
    }
    let query: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
    let state_matches = query
        .get("state")
        .is_some_and(|state| constant_time_eq(state.as_bytes(), expected_state.as_bytes()));
    if !state_matches {
        return CallbackRequest::Unrelated;
    }
    if query.contains_key("error") {
        return CallbackRequest::Cancelled;
    }
    query
        .get("code")
        .filter(|value| !value.is_empty() && value.len() <= 8192)
        .map_or(CallbackRequest::MissingCode, |value| {
            CallbackRequest::Code(Zeroizing::new(value.to_owned()))
        })
}

#[cfg_attr(target_os = "macos", allow(dead_code))]
async fn read_callback_request_line(
    stream: &mut (impl tokio::io::AsyncRead + Unpin),
    deadline: Duration,
    max_bytes: usize,
) -> Result<String, ()> {
    // A close with unread HTTP headers can reset the connection and discard the callback page.
    // Drain through the blank line, not EOF: browsers keep their write side open for the response.
    const MAX_REQUEST_BYTES: usize = 16 * 1024;
    let mut request = Vec::with_capacity(256);
    let mut line_end = None;
    let expires_at = tokio::time::Instant::now() + deadline;
    loop {
        if request.len() >= MAX_REQUEST_BYTES || (line_end.is_none() && request.len() >= max_bytes)
        {
            return Err(());
        }
        let mut byte = [0u8; 1];
        let remaining = expires_at.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return Err(());
        }
        let read = tokio::time::timeout(remaining, stream.read(&mut byte))
            .await
            .map_err(|_| ())?
            .map_err(|_| ())?;
        if read == 0 {
            return Err(());
        }
        request.push(byte[0]);
        if line_end.is_none() && request.ends_with(b"\r\n") {
            line_end = Some(request.len() - 2);
        }
        if let Some(end) = line_end {
            if request.ends_with(b"\r\n\r\n") {
                let line = std::str::from_utf8(&request[..end]).map_err(|_| ())?;
                return Ok(line.to_owned());
            }
        }
    }
}
fn random_b64url(length: usize) -> BridgeResult<String> {
    let mut bytes = Zeroizing::new(vec![0; length]);
    getrandom::fill(&mut bytes)
        .map_err(|_| BridgeError::new("hosted-sign-in", "Could not create secure sign-in data."))?;
    Ok(URL_SAFE_NO_PAD.encode(&*bytes))
}
fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    let mut difference = left.len() ^ right.len();
    for index in 0..left.len().max(right.len()) {
        difference |= usize::from(*left.get(index).unwrap_or(&0) ^ *right.get(index).unwrap_or(&0));
    }
    difference == 0
}
fn app_authorize_url(
    origin: &str,
    challenge: &str,
    state: &str,
    provider: &str,
    return_to: &str,
) -> BridgeResult<String> {
    let mut url = url::Url::parse(&format!("{origin}{}", web_routes::APP_AUTHORIZE))
        .map_err(|_| invalid_response())?;
    url.query_pairs_mut()
        .append_pair("challenge", challenge)
        .append_pair("state", state)
        .append_pair("provider", provider)
        .append_pair("return_to", return_to);
    Ok(url.into())
}

fn native_session_request(code: &str, verifier: &str) -> BridgeResult<String> {
    serde_json::to_string(&NativeSessionExchangeRequest {
        code: code.to_owned(),
        verifier: verifier.to_owned(),
    })
    .map_err(|_| invalid_response())
}

const SYSTEM_BROWSER_DEADLINE: Duration = Duration::from_secs(300);

fn is_system_browser_cancellation(error_domain: &str, code: isize) -> bool {
    error_domain == "ASWebAuthenticationSessionErrorDomain" && code == 1
}

async fn await_system_browser_completion<T>(
    receiver: tokio::sync::oneshot::Receiver<BridgeResult<T>>,
    deadline: Duration,
    release_session: impl FnOnce(),
) -> BridgeResult<T> {
    match tokio::time::timeout(deadline, receiver).await {
        Ok(Ok(result)) => result,
        Ok(Err(_)) => Err(BridgeError::new(
            "hosted-sign-in",
            "Hosted sign-in did not complete.",
        )),
        Err(_) => {
            release_session();
            Err(BridgeError::new(
                "hosted-sign-in-timeout",
                "Hosted sign-in timed out.",
            ))
        }
    }
}

#[cfg(target_os = "macos")]
#[allow(deprecated, unexpected_cfgs)]
async fn authenticate_with_system_browser(
    window: &tauri::WebviewWindow,
    auth: String,
    state: &str,
) -> BridgeResult<Zeroizing<String>> {
    use std::sync::OnceLock;

    use cocoa::{
        base::{id, nil},
        foundation::NSString,
    };
    use objc::{
        class,
        declare::ClassDecl,
        msg_send,
        runtime::{Class, Object, Sel},
        sel, sel_impl,
    };
    use objc2_authentication_services as _;
    use objc2_foundation::{NSError, NSURL};

    fn release_session_objects(
        objects: &std::sync::Arc<std::sync::Mutex<Option<(usize, usize)>>>,
        cancel: bool,
    ) {
        let Some((session, provider)) = objects.lock().unwrap().take() else {
            return;
        };
        unsafe {
            let session = session as id;
            let provider = provider as id;
            if cancel {
                let _: () = msg_send![session, cancel];
            }
            let _: () = msg_send![session, release];
            let _: () = msg_send![provider, release];
        }
    }

    extern "C" fn presentation_anchor(provider: &Object, _: Sel, _: id) -> id {
        unsafe { *provider.get_ivar("anchor") }
    }

    fn presentation_provider_class() -> *const Class {
        static CLASS: OnceLock<usize> = OnceLock::new();
        *CLASS.get_or_init(|| {
            let superclass = class!(NSObject);
            let mut declaration =
                ClassDecl::new("PeppyAuthenticationPresentationContext", superclass)
                    .expect("presentation provider class is registered once");
            declaration.add_ivar::<id>("anchor");
            unsafe {
                declaration.add_method(
                    sel!(presentationAnchorForWebAuthenticationSession:),
                    presentation_anchor as extern "C" fn(&Object, Sel, id) -> id,
                );
            }
            declaration.register() as *const Class as usize
        }) as *const Class
    }

    let expected_state = state.to_owned();
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let sender = std::sync::Arc::new(std::sync::Mutex::new(Some(sender)));
    let session_objects = std::sync::Arc::new(std::sync::Mutex::new(None));
    let auth_for_main = auth.clone();
    let presentation_window = window.clone();
    let completion_sender = sender.clone();
    let completion_objects = session_objects.clone();
    let session_objects_for_main = session_objects.clone();
    window
        .run_on_main_thread(move || unsafe {
            let session_class = class!(ASWebAuthenticationSession);
            let Ok(anchor) = presentation_window.ns_window() else {
                let _ = sender.lock().unwrap().take().map(|sender| {
                    sender.send(Err(BridgeError::new(
                        "hosted-sign-in",
                        "Could not present the secure sign-in window.",
                    )))
                });
                return;
            };
            let provider: id = msg_send![presentation_provider_class(), alloc];
            let provider: id = msg_send![provider, init];
            (*provider).set_ivar("anchor", anchor.cast::<Object>());
            let url = NSString::alloc(nil).init_str(&auth_for_main);
            let scheme = NSString::alloc(nil).init_str("org.peppy.desktop");
            let url: id = msg_send![class!(NSURL), URLWithString: url];
            let completion: block2::RcBlock<dyn Fn(*mut NSURL, *mut NSError)> =
                block2::RcBlock::new(move |callback_url: *mut NSURL, error: *mut NSError| {
                    let callback_url = callback_url.cast::<Object>();
                    let error = error.cast::<Object>();
                    let result = if callback_url != nil {
                        let value: id = msg_send![callback_url, absoluteString];
                        let bytes: *const std::ffi::c_char = msg_send![value, UTF8String];
                        if bytes.is_null() {
                            Err(BridgeError::new(
                                "hosted-sign-in",
                                "The sign-in callback was invalid.",
                            ))
                        } else {
                            let callback = std::ffi::CStr::from_ptr(bytes).to_string_lossy();
                            classify_native_callback(&callback, &expected_state)
                        }
                    } else if error != nil {
                        let code: isize = msg_send![error, code];
                        let domain: id = msg_send![error, domain];
                        let domain_bytes: *const std::ffi::c_char = msg_send![domain, UTF8String];
                        let is_cancelled = !domain_bytes.is_null()
                            && is_system_browser_cancellation(
                                &std::ffi::CStr::from_ptr(domain_bytes).to_string_lossy(),
                                code,
                            );
                        if is_cancelled {
                            Err(cancelled_error())
                        } else {
                            Err(BridgeError::new(
                                "hosted-sign-in",
                                "Hosted sign-in did not complete.",
                            ))
                        }
                    } else {
                        Err(BridgeError::new(
                            "hosted-sign-in",
                            "Hosted sign-in did not complete.",
                        ))
                    };
                    release_session_objects(&completion_objects, false);
                    let _ = completion_sender
                        .lock()
                        .unwrap()
                        .take()
                        .map(|sender| sender.send(result));
                });
            let session: id = msg_send![session_class, alloc];
            let session: id = msg_send![session,
                initWithURL: url
                callbackURLScheme: scheme
                completionHandler: block2::RcBlock::as_ptr(&completion)
            ];
            let _: () = msg_send![session, setPresentationContextProvider: provider];
            let _: () = msg_send![session, setPrefersEphemeralWebBrowserSession: false];
            *session_objects_for_main.lock().unwrap() = Some((session as usize, provider as usize));
            let started: bool = msg_send![session, start];
            if !started {
                release_session_objects(&session_objects_for_main, false);
                let _ = sender.lock().unwrap().take().map(|sender| {
                    sender.send(Err(BridgeError::new(
                        "hosted-sign-in",
                        "Could not present the secure sign-in window.",
                    )))
                });
            }
        })
        .map_err(|_| {
            BridgeError::new(
                "hosted-sign-in",
                "Could not present the secure sign-in window.",
            )
        })?;
    let timeout_window = window.clone();
    await_system_browser_completion(receiver, SYSTEM_BROWSER_DEADLINE, move || {
        let session_objects = session_objects.clone();
        let _ = timeout_window.run_on_main_thread(move || {
            release_session_objects(&session_objects, true);
        });
    })
    .await
}

fn cancelled_error() -> BridgeError {
    BridgeError::new("cancelled", "Hosted sign-in was cancelled.")
}

fn classify_native_callback(
    callback: &str,
    expected_state: &str,
) -> BridgeResult<Zeroizing<String>> {
    let url = url::Url::parse(callback)
        .map_err(|_| BridgeError::new("hosted-sign-in", "The sign-in callback was invalid."))?;
    if url.scheme() != "org.peppy.desktop" || url.path() != "/auth/callback" {
        return Err(BridgeError::new(
            "hosted-sign-in",
            "The sign-in callback was invalid.",
        ));
    }
    let query: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
    if !query
        .get("state")
        .is_some_and(|state| constant_time_eq(state.as_bytes(), expected_state.as_bytes()))
    {
        return Err(BridgeError::new(
            "hosted-sign-in",
            "The sign-in callback was invalid.",
        ));
    }
    if query.contains_key("error") {
        return Err(cancelled_error());
    }
    query
        .get("code")
        .filter(|code| !code.is_empty() && code.len() <= 8192)
        .map(|code| Zeroizing::new(code.to_owned()))
        .ok_or_else(|| BridgeError::new("hosted-sign-in", "The sign-in callback was invalid."))
}
fn pkce_challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_is_s256() {
        assert_eq!(
            pkce_challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }
    #[test]
    fn session_debug_redacts_bearer() {
        let session = HostedSessionRecord {
            account_id: "a".into(),
            bearer: Zeroizing::new("secret".into()),
            expires_at_unix: 1,
        };
        assert!(!format!("{session:?}").contains("secret"));
    }

    const CALLBACK_STATE: &str = "expected-state";

    #[test]
    fn only_a_root_callback_with_the_expected_state_is_classified() {
        for unrelated in [
            "",
            "GET",
            "GET :99999/ HTTP/1.1",
            "GET /favicon.ico HTTP/1.1",
            "GET / HTTP/1.1",
            "GET /callback?error=access_denied HTTP/1.1",
            "GET /callback?error=access_denied&state=other HTTP/1.1",
            "GET /callback?code=forged&state=other HTTP/1.1",
            "GET /callback?code=forged&state=expected-stat HTTP/1.1",
            "GET /favicon.ico?code=x&state=expected-state HTTP/1.1",
        ] {
            assert_eq!(
                classify_callback_request(unrelated, CALLBACK_STATE),
                CallbackRequest::Unrelated,
                "{unrelated:?}"
            );
        }
        assert_eq!(
            classify_callback_request(
                "GET /callback?error=access_denied&state=expected-state HTTP/1.1",
                CALLBACK_STATE
            ),
            CallbackRequest::Cancelled
        );
        assert_eq!(
            classify_callback_request(
                "GET /callback?state=expected-state HTTP/1.1",
                CALLBACK_STATE
            ),
            CallbackRequest::MissingCode
        );
        assert_eq!(
            classify_callback_request(
                &format!(
                    "GET /callback?code={}&state=expected-state HTTP/1.1",
                    "c".repeat(8193)
                ),
                CALLBACK_STATE
            ),
            CallbackRequest::MissingCode
        );
        assert_eq!(
            classify_callback_request(
                "GET /callback?code=auth-code&state=expected-state HTTP/1.1",
                CALLBACK_STATE
            ),
            CallbackRequest::Code(Zeroizing::new("auth-code".into()))
        );
    }

    #[test]
    fn native_callback_requires_matching_state_and_maps_provider_errors_to_cancelled() {
        assert_eq!(
            classify_native_callback(
                "org.peppy.desktop:/auth/callback?code=forged&state=other",
                CALLBACK_STATE,
            )
            .unwrap_err()
            .code,
            "hosted-sign-in"
        );
        assert_eq!(
            classify_native_callback(
                "org.peppy.desktop:/auth/callback?error=access_denied&state=expected-state",
                CALLBACK_STATE,
            )
            .unwrap_err()
            .code,
            "cancelled"
        );
        assert_eq!(
            classify_native_callback(
                "org.peppy.desktop:/other?code=forged&state=expected-state",
                CALLBACK_STATE,
            )
            .unwrap_err()
            .code,
            "hosted-sign-in"
        );
    }

    #[test]
    fn system_browser_cancellation_requires_the_authentication_session_domain() {
        assert!(is_system_browser_cancellation(
            "ASWebAuthenticationSessionErrorDomain",
            1
        ));
        assert!(!is_system_browser_cancellation("NSURLErrorDomain", 1));
        assert!(!is_system_browser_cancellation(
            "ASWebAuthenticationSessionErrorDomain",
            2
        ));
    }

    #[tokio::test]
    async fn system_browser_timeout_releases_the_session_and_returns_timeout() {
        let (_sender, receiver) = tokio::sync::oneshot::channel::<BridgeResult<()>>();
        let released = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let release_session = released.clone();

        let error =
            await_system_browser_completion(receiver, Duration::from_millis(1), move || {
                release_session.store(true, Ordering::SeqCst)
            })
            .await
            .unwrap_err();

        assert_eq!(error.code, "hosted-sign-in-timeout");
        assert!(released.load(Ordering::SeqCst));
    }

    #[test]
    fn native_session_exchange_uses_the_attempt_verifier() {
        let request: serde_json::Value = serde_json::from_str(
            &native_session_request("returned-code", "attempt-verifier").unwrap(),
        )
        .unwrap();
        assert_eq!(request["code"], "returned-code");
        assert_eq!(request["verifier"], "attempt-verifier");
    }

    #[tokio::test]
    async fn loopback_is_ipv4_only_and_releases_its_port_after_use() {
        let loopback = Loopback::bind().await.unwrap();
        let address = loopback.listener.local_addr().unwrap();
        assert_eq!(address.ip(), std::net::Ipv4Addr::LOCALHOST);
        drop(loopback);
        TcpListener::bind(address).await.unwrap();
    }

    #[tokio::test]
    async fn callback_reader_consumes_headers_before_the_connection_can_close() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut client = tokio::net::TcpStream::connect(listener.local_addr().unwrap())
            .await
            .unwrap();
        let (mut server, _) = listener.accept().await.unwrap();
        client
            .write_all(
                b"GET /callback?code=auth-code&state=expected-state HTTP/1.1\r\nHost: localhost\r\n\r\n",
            )
            .await
            .unwrap();
        client.shutdown().await.unwrap();

        let line = read_callback_request_line(&mut server, Duration::from_secs(1), 4096)
            .await
            .unwrap();
        assert_eq!(
            classify_callback_request(&line, CALLBACK_STATE),
            CallbackRequest::Code(Zeroizing::new("auth-code".into()))
        );
        let mut unread = Vec::new();
        server.read_to_end(&mut unread).await.unwrap();
        assert!(
            unread.is_empty(),
            "closing with unread request bytes: {unread:?}"
        );
    }

    #[tokio::test]
    async fn loopback_delivers_the_complete_page_without_resetting_the_browser() {
        let loopback = Loopback::bind().await.unwrap();
        let address = loopback.uri.trim_start_matches("http://").to_owned();
        let receiver = tokio::spawn(async move { loopback.receive(CALLBACK_STATE).await });
        let mut stream = tokio::net::TcpStream::connect(address).await.unwrap();
        stream
            .write_all(
                b"GET /callback?code=auth-code&state=expected-state HTTP/1.1\r\nHost: localhost\r\n\r\n",
            )
            .await
            .unwrap();
        // A browser waits for the response without first half-closing its request socket.
        let mut response = Vec::new();
        stream.read_to_end(&mut response).await.unwrap();
        let response = String::from_utf8(response).unwrap();
        let (headers, body) = response.split_once("\r\n\r\n").unwrap();
        assert!(headers.starts_with("HTTP/1.1 200 OK\r\n"));
        let length: usize = headers
            .lines()
            .find_map(|line| line.strip_prefix("content-length: "))
            .unwrap()
            .parse()
            .unwrap();
        assert_eq!(body.len(), length);
        assert_eq!(receiver.await.unwrap().unwrap().as_str(), "auth-code");
    }

    #[tokio::test]
    async fn callback_reader_rejects_incomplete_oversized_and_expired_requests() {
        for request in [
            b"GET / HTTP/1.1\r\nHost: localhost\r\n".to_vec(),
            [b"A".repeat(4096), b"\r\n\r\n".to_vec()].concat(),
            [
                b"GET / HTTP/1.1\r\nHost: ".to_vec(),
                b"A".repeat(16 * 1024),
                b"\r\n\r\n".to_vec(),
            ]
            .concat(),
        ] {
            assert!(read_callback_request_line(
                &mut request.as_slice(),
                Duration::from_secs(1),
                4096
            )
            .await
            .is_err());
        }
        let mut complete = b"GET / HTTP/1.1\r\n\r\n".as_slice();
        assert!(
            read_callback_request_line(&mut complete, Duration::ZERO, 4096)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn callback_reader_waits_for_fragmented_headers_but_not_for_eof() {
        let (mut client, mut server) = tokio::io::duplex(256);
        client
            .write_all(b"GET / HTTP/1.1\r\nHost: local")
            .await
            .unwrap();
        let read = read_callback_request_line(&mut server, Duration::from_secs(1), 4096);
        tokio::pin!(read);
        std::future::poll_fn(|cx| {
            assert!(std::future::Future::poll(read.as_mut(), cx).is_pending());
            std::task::Poll::Ready(())
        })
        .await;
        client.write_all(b"host\r\n\r\n").await.unwrap();
        assert_eq!(read.await.unwrap(), "GET / HTTP/1.1");
    }

    #[tokio::test]
    async fn callback_reader_deadline_also_bounds_stalled_headers() {
        let (mut client, mut server) = tokio::io::duplex(256);
        client
            .write_all(b"GET / HTTP/1.1\r\nHost: local")
            .await
            .unwrap();
        let result = tokio::time::timeout(
            Duration::from_secs(1),
            read_callback_request_line(&mut server, Duration::from_millis(20), 4096),
        )
        .await
        .expect("the request deadline must close a stalled header read");
        assert!(result.is_err());
    }

    /// Sends `chunks` on one loopback connection, then waits until the listener closes it.
    async fn send_callback_connection(address: &str, chunks: &[&[u8]]) -> Vec<u8> {
        let mut stream = tokio::net::TcpStream::connect(address).await.unwrap();
        for chunk in chunks {
            // The listener may drop an oversized request before every byte is written.
            if stream.write_all(chunk).await.is_err() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let _ = stream.shutdown().await;
        let mut response = Vec::new();
        let _ = stream.read_to_end(&mut response).await;
        response
    }

    #[tokio::test]
    async fn loopback_ignores_unrelated_requests_until_the_fragmented_callback() {
        let loopback = Loopback::bind().await.unwrap();
        let address = loopback.uri.trim_start_matches("http://").to_owned();
        let receiver = tokio::spawn(async move { loopback.receive(CALLBACK_STATE).await });

        let oversized = vec![b'A'; 5000];
        for unrelated in [
            &[b"GET /favicon.ico HTTP/1.1\r\n\r\n".as_slice()][..],
            &[b"GET / HTTP/1.1\r\n\r\n".as_slice()],
            &[b"GARBAGE\r\n\r\n".as_slice()],
            &[b"GET :99999/ HTTP/1.1\r\n\r\n".as_slice()],
            &[b"GET /callback?error=access_denied HTTP/1.1\r\n\r\n".as_slice()],
            &[b"GET /callback?code=forged&state=other HTTP/1.1\r\n\r\n".as_slice()],
            &[b"GET /callback?code=cut-off&state=expected-state".as_slice()],
            &[b"\xff\xfe\r\n\r\n".as_slice()],
            &[oversized.as_slice()],
        ] {
            assert!(send_callback_connection(&address, unrelated)
                .await
                .is_empty());
            assert!(!receiver.is_finished());
        }

        let response = send_callback_connection(
            &address,
            &[
                b"GET /callback?code=auth-code&st",
                b"ate=expected-state HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n",
            ],
        )
        .await;

        assert!(!response.is_empty());
        let code = receiver.await.unwrap().unwrap();
        assert_eq!(code.as_str(), "auth-code");
    }

    #[tokio::test]
    async fn loopback_cancels_only_for_an_error_with_the_expected_state() {
        let loopback = Loopback::bind().await.unwrap();
        let address = loopback.uri.trim_start_matches("http://").to_owned();
        let receiver = tokio::spawn(async move { loopback.receive(CALLBACK_STATE).await });

        send_callback_connection(
            &address,
            &[b"GET /callback?error=access_denied HTTP/1.1\r\n\r\n"],
        )
        .await;
        assert!(!receiver.is_finished());
        send_callback_connection(
            &address,
            &[b"GET /callback?error=access_denied&state=expected-state HTTP/1.1\r\n\r\n"],
        )
        .await;

        let error = receiver.await.unwrap().unwrap_err();
        assert_eq!(error.code, "cancelled");
    }
}

#[cfg(test)]
mod hosted_tests;
