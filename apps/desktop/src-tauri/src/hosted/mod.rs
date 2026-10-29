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
    routes, IdentityProvider, NativeAuthConfigResponse, SESSION_LIFETIME_SECONDS,
};
use peppy_hosted_client::{
    hosted_login_request, hosted_session_request, parse_hosted_account, parse_hosted_login_attempt,
    parse_hosted_session, parse_hosted_session_renewal, prepare, restore, HostedProvisioning,
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
    pub signed_in: bool,
    pub account_label: Option<String>,
    pub classification: Option<String>,
    pub entitlement: Option<String>,
    pub access: Option<String>,
    pub has_vault: bool,
    pub resumable: bool,
}

impl HostedAccountView {
    fn unavailable() -> Self {
        Self {
            available: false,
            signed_in: false,
            account_label: None,
            classification: None,
            entitlement: None,
            access: None,
            has_vault: false,
            resumable: false,
        }
    }
    fn signed_out() -> Self {
        Self {
            available: true,
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
struct HostedSessionRecord {
    account_id: String,
    bearer: Zeroizing<String>,
    expires_at_unix: u64,
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
fn client_id() -> Option<&'static str> {
    option_env!("PEPPY_DESKTOP_GOOGLE_CLIENT_ID").filter(|value| !value.is_empty())
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
        "Google sign-in is currently unavailable.",
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
async fn sign_in_available(
    hosted: &HostedState,
    origin: &str,
    configured_client_id: Option<&str>,
) -> BridgeResult<bool> {
    if configured_client_id.is_none() {
        return Ok(false);
    }
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
        .contains(&IdentityProvider::Google))
}

async fn begin_sign_in(
    hosted: &HostedState,
    origin: &str,
    configured_client_id: Option<&str>,
) -> BridgeResult<u64> {
    let generation = hosted.sign_in_generation.fetch_add(1, Ordering::SeqCst) + 1;
    if !sign_in_available(hosted, origin, configured_client_id).await? {
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

async fn signed_out_view(
    hosted: &HostedState,
    origin: &str,
    configured_client_id: Option<&str>,
) -> BridgeResult<HostedAccountView> {
    if sign_in_available(hosted, origin, configured_client_id).await? {
        Ok(HostedAccountView::signed_out())
    } else {
        Ok(HostedAccountView::unavailable())
    }
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
    account_view(&*state.store, &hosted, &hosted_origin()?, client_id()).await
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
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    hosted: tauri::State<'_, HostedState>,
    provider: String,
) -> BridgeResult<HostedAccountView> {
    let _operation = crate::admit_update_operation(&state)?;
    crate::require_main(window.label())?;
    let Some(client_id) = client_id() else {
        return Err(hosted_error());
    };
    if provider != "google" {
        return Err(hosted_error());
    }
    let origin = hosted_origin()?;
    let generation = begin_sign_in(&hosted, &origin, Some(client_id)).await?;
    let client = http(&origin)?;
    let request = hosted_login_request("google".into()).map_err(client_error)?;
    let attempt_body = read_response(
        client
            .post(format!("{origin}{}", routes::AUTH_ATTEMPTS))
            .header("content-type", "application/json")
            .body(request)
            .send()
            .await
            .map_err(|_| BridgeError::new("offline", "Could not reach Peppy Hosted."))?,
    )
    .await
    .map_err(BridgeError::from)?;
    let attempt = parse_hosted_login_attempt(
        String::from_utf8(attempt_body).map_err(|_| invalid_response())?,
    )
    .map_err(client_error)?;
    let redirect = Loopback::bind().await?;
    ensure_sign_in_current(&hosted, generation)?;
    let redirect_uri = redirect.uri.clone();
    let mut verifier = Zeroizing::new(random_b64url(32)?);
    let state_token = Zeroizing::new(random_b64url(32)?);
    let challenge = pkce_challenge(&verifier);
    let auth = google_url(
        client_id,
        &redirect_uri,
        &challenge,
        &state_token,
        &attempt.nonce(),
    );
    app.opener()
        .open_url(auth, None::<String>)
        .map_err(|_| BridgeError::new("open-url", "Could not open Google sign-in."))?;
    let code = redirect.receive(&state_token).await?;
    ensure_sign_in_current(&hosted, generation)?;
    let mut form = vec![
        ("code", code.to_string()),
        ("client_id", client_id.to_owned()),
        ("redirect_uri", redirect_uri),
        ("grant_type", "authorization_code".into()),
        ("code_verifier", verifier.to_string()),
    ];
    if let Some(secret) =
        option_env!("PEPPY_DESKTOP_GOOGLE_CLIENT_SECRET").filter(|value| !value.is_empty())
    {
        form.push(("client_secret", secret.into()));
    }
    let encoded_form = url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs(form.iter().map(|(key, value)| (*key, value.as_str())))
        .finish();
    let token_body = read_response(
        http("https://oauth2.googleapis.com")?
            .post("https://oauth2.googleapis.com/token")
            .header("content-type", "application/x-www-form-urlencoded")
            .body(encoded_form)
            .send()
            .await
            .map_err(|_| BridgeError::new("offline", "Could not reach Google sign-in."))?,
    )
    .await
    .map_err(BridgeError::from)?;
    verifier.zeroize();
    let mut id_token = Zeroizing::new(
        serde_json::from_slice::<serde_json::Value>(&token_body)
            .ok()
            .and_then(|v| v.get("id_token")?.as_str().map(str::to_owned))
            .filter(|value| !value.is_empty() && value.len() <= 16 * 1024)
            .ok_or_else(invalid_response)?,
    );
    let session_request =
        hosted_session_request(attempt.attempt_id(), id_token.to_string()).map_err(client_error)?;
    id_token.zeroize();
    let session_body = read_response(
        client
            .post(format!("{origin}{}", routes::AUTH_SESSION))
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
    account_view(&*state.store, &hosted, &origin, Some(client_id)).await
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
    configured_client_id: Option<&str>,
) -> BridgeResult<HostedAccountView> {
    let Some(session) = load_session_from_store(store, hosted, origin).await? else {
        return signed_out_view(hosted, origin, configured_client_id).await;
    };
    let base = hosted.api_base(origin);
    let client = http(&base)?;
    let generation = hosted.sign_in_generation.load(Ordering::SeqCst);
    let bytes = match authenticated(&client, &base, routes::ACCOUNT, &session.bearer).await {
        Ok(bytes) => bytes,
        Err(NetError::Revoked) => {
            tombstone_observed_session(store, hosted, origin, &session.bearer, generation).await?;
            return signed_out_view(hosted, origin, configured_client_id).await;
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
    Ok(HostedAccountView {
        available: sign_in_available(hosted, origin, configured_client_id)
            .await
            .unwrap_or(false),
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

async fn tombstone_session(
    store: &dyn SecretStore,
    hosted: &HostedState,
    origin: &str,
) -> BridgeResult<()> {
    let mut cached = hosted.session.lock().await;
    tombstone(store, &session_key(origin))?;
    hosted.sign_in_generation.fetch_add(1, Ordering::SeqCst);
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

async fn commit_sign_in_session(
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

struct Loopback {
    listener: TcpListener,
    uri: String,
}
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
                    "Google sign-in timed out.",
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
                    Err(BridgeError::new(
                        "hosted-sign-in-cancelled",
                        "Google sign-in was cancelled.",
                    )),
                ),
                CallbackRequest::Code(code) => (callback_page::CallbackOutcome::Received, Ok(code)),
                CallbackRequest::MissingCode => (
                    callback_page::CallbackOutcome::Received,
                    Err(BridgeError::new(
                        "hosted-sign-in",
                        "Google did not return a sign-in code.",
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
enum CallbackRequest {
    /// Not this sign-in's callback: malformed, another path, or without the expected `state`.
    Unrelated,
    Cancelled,
    Code(Zeroizing<String>),
    MissingCode,
}

/// Only a request to `/` carrying this sign-in's `state` may finish it, so an unrelated local
/// connection can neither cancel the sign-in nor substitute a code.
fn classify_callback_request(line: &str, expected_state: &str) -> CallbackRequest {
    let Some(target) = line.split_whitespace().nth(1) else {
        return CallbackRequest::Unrelated;
    };
    let Ok(url) = url::Url::parse(&format!("http://127.0.0.1{target}")) else {
        return CallbackRequest::Unrelated;
    };
    if url.path() != "/" {
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
fn google_url(
    client_id: &str,
    redirect: &str,
    challenge: &str,
    state: &str,
    nonce: &str,
) -> String {
    let mut url =
        url::Url::parse("https://accounts.google.com/o/oauth2/v2/auth").expect("constant URL");
    url.query_pairs_mut()
        .append_pair("response_type", "code")
        .append_pair("client_id", client_id)
        .append_pair("redirect_uri", redirect)
        .append_pair("scope", "openid email")
        .append_pair("code_challenge", challenge)
        .append_pair("code_challenge_method", "S256")
        .append_pair("state", state)
        .append_pair("nonce", nonce)
        .append_pair("prompt", "select_account");
    url.into()
}
fn pkce_challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absent_client_id_is_unavailable() {
        if client_id().is_none() {
            assert!(!HostedAccountView::unavailable().available);
        }
    }
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
            "GET /?error=access_denied HTTP/1.1",
            "GET /?error=access_denied&state=other HTTP/1.1",
            "GET /?code=forged&state=other HTTP/1.1",
            "GET /?code=forged&state=expected-stat HTTP/1.1",
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
                "GET /?error=access_denied&state=expected-state HTTP/1.1",
                CALLBACK_STATE
            ),
            CallbackRequest::Cancelled
        );
        assert_eq!(
            classify_callback_request("GET /?state=expected-state HTTP/1.1", CALLBACK_STATE),
            CallbackRequest::MissingCode
        );
        assert_eq!(
            classify_callback_request(
                &format!(
                    "GET /?code={}&state=expected-state HTTP/1.1",
                    "c".repeat(8193)
                ),
                CALLBACK_STATE
            ),
            CallbackRequest::MissingCode
        );
        assert_eq!(
            classify_callback_request(
                "GET /?code=auth-code&state=expected-state HTTP/1.1",
                CALLBACK_STATE
            ),
            CallbackRequest::Code(Zeroizing::new("auth-code".into()))
        );
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
                b"GET /?code=auth-code&state=expected-state HTTP/1.1\r\nHost: localhost\r\n\r\n",
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
                b"GET /?code=auth-code&state=expected-state HTTP/1.1\r\nHost: localhost\r\n\r\n",
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
            &[b"GET /?error=access_denied HTTP/1.1\r\n\r\n".as_slice()],
            &[b"GET /?code=forged&state=other HTTP/1.1\r\n\r\n".as_slice()],
            &[b"GET /?code=cut-off&state=expected-state".as_slice()],
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
                b"GET /?code=auth-code&st",
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

        send_callback_connection(&address, &[b"GET /?error=access_denied HTTP/1.1\r\n\r\n"]).await;
        assert!(!receiver.is_finished());
        send_callback_connection(
            &address,
            &[b"GET /?error=access_denied&state=expected-state HTTP/1.1\r\n\r\n"],
        )
        .await;

        let error = receiver.await.unwrap().unwrap_err();
        assert_eq!(error.code, "hosted-sign-in-cancelled");
    }
}

#[cfg(test)]
mod hosted_tests;
