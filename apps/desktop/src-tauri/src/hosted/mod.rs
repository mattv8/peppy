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
use peppy_hosted_api::{routes, IdentityProvider, NativeAuthConfigResponse};
use peppy_hosted_client::{
    hosted_login_request, hosted_session_request, parse_hosted_account, parse_hosted_login_attempt,
    parse_hosted_session, prepare, restore, HostedProvisioning,
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

/// Managed by L8 with `app.manage(HostedState::default())`.
#[derive(Default)]
pub struct HostedState {
    session: Mutex<Option<HostedSessionRecord>>,
    provisioning: Mutex<()>,
    sign_in_generation: AtomicU64,
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
    Ok(Zeroizing::new(session.bearer.to_string()))
}

/// Reuses the reviewed hosted grant/checkpoint/protected-import flow for first owner creation.
pub(crate) async fn provision_enrollment(
    app: &tauri::AppHandle,
    state: &AppState,
    hosted: &HostedState,
) -> BridgeResult<()> {
    let _operation = crate::admit_update_operation(state)?;
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
        |credential, passphrase| activate_and_unlock(app, state, credential, passphrase),
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
        let url = url::Url::parse(&value).map_err(|_| {
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
    credential: Zeroizing<Vec<u8>>,
    passphrase: Zeroizing<String>,
) -> BridgeResult<()> {
    crate::activate_native_credential(app, state, credential).await?;
    let native = state.require_session().await?;
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
    let bytes = match authenticated(&client, &base, routes::ACCOUNT, &session.bearer).await {
        Ok(bytes) => bytes,
        Err(NetError::Revoked) => {
            tombstone_observed_session(store, hosted, origin, &session.bearer).await?;
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
    if let Err(error) = tombstone_observed_session(store, hosted, origin, observed_bearer).await {
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
    if let Some(value) = cached.as_ref() {
        if value.expires_at_unix > now_unix() {
            return Ok(Some(copy_session(value)));
        }
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
    if record.expires_at_unix <= now_unix() {
        tombstone(store, &key)?;
        *cached = None;
        return Ok(None);
    }
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
) -> BridgeResult<bool> {
    let mut cached = hosted.session.lock().await;
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
        let (mut stream, _) =
            tokio::time::timeout(Duration::from_secs(300), self.listener.accept())
                .await
                .map_err(|_| {
                    BridgeError::new("hosted-sign-in-timeout", "Google sign-in timed out.")
                })?
                .map_err(|_| {
                    BridgeError::new("hosted-sign-in", "Could not receive the sign-in callback.")
                })?;
        let mut request = [0u8; 4096];
        let length = tokio::time::timeout(Duration::from_secs(10), stream.read(&mut request))
            .await
            .map_err(|_| BridgeError::new("hosted-sign-in", "The sign-in callback timed out."))?
            .map_err(|_| {
                BridgeError::new("hosted-sign-in", "Could not read the sign-in callback.")
            })?;
        let line = std::str::from_utf8(&request[..length])
            .ok()
            .and_then(|v| v.split("\r\n").next())
            .ok_or_else(|| {
                BridgeError::new("hosted-sign-in", "The sign-in callback was invalid.")
            })?;
        let target = line.split_whitespace().nth(1).ok_or_else(|| {
            BridgeError::new("hosted-sign-in", "The sign-in callback was invalid.")
        })?;
        let url = url::Url::parse(&format!("http://127.0.0.1{target}"))
            .map_err(|_| BridgeError::new("hosted-sign-in", "The sign-in callback was invalid."))?;
        let query: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
        let cancelled = query.contains_key("error");
        let outcome = if cancelled {
            callback_page::CallbackOutcome::Cancelled
        } else {
            callback_page::CallbackOutcome::Received
        };
        let _ = stream
            .write_all(callback_page::response(outcome).as_bytes())
            .await;
        if cancelled {
            return Err(BridgeError::new(
                "hosted-sign-in-cancelled",
                "Google sign-in was cancelled.",
            ));
        }
        let state = query.get("state").ok_or_else(|| {
            BridgeError::new("hosted-sign-in", "The sign-in callback was invalid.")
        })?;
        if !constant_time_eq(state.as_bytes(), expected.as_bytes()) {
            return Err(BridgeError::new(
                "hosted-sign-in",
                "The sign-in callback did not match this request.",
            ));
        }
        query
            .get("code")
            .filter(|value| !value.is_empty() && value.len() <= 8192)
            .map(|value| Zeroizing::new(value.to_owned()))
            .ok_or_else(|| {
                BridgeError::new("hosted-sign-in", "Google did not return a sign-in code.")
            })
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
}

#[cfg(test)]
mod hosted_tests;
