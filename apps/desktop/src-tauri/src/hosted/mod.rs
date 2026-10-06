//! Production Peppy Hosted account sign-in, entitlement and first-vault provisioning.
//!
//! This module deliberately keeps hosted credentials on the native side.  The
//! webview receives only `HostedAccountView`.

use crate::{
    error::{BridgeError, BridgeResult},
    net::{read_bounded, NetError},
    secure_store::SecretStore,
    AppState,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use peppy_hosted_api::routes;
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

const PRODUCTION_ORIGIN: &str = "https://peppy.pro";
const MAX_BODY: usize = 64 * 1024;
const SESSION_PREFIX: &str = "hosted-session:";
const CHECKPOINT_PREFIX: &str = "hosted-provisioning:";

/// Managed by L8 with `app.manage(HostedState::default())`.
#[derive(Default)]
pub struct HostedState {
    session: Mutex<Option<HostedSessionRecord>>,
    sign_in_generation: AtomicU64,
    /// Tests send requests to a local fake server while keeping an HTTPS credential origin.
    #[cfg(test)]
    base_override: std::sync::Mutex<Option<String>>,
}

impl HostedState {
    fn api_base(&self, origin: &str) -> String {
        #[cfg(test)]
        if let Some(base) = self.base_override.lock().unwrap().clone() {
            return base;
        }
        origin.to_owned()
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
fn hosted_origin() -> BridgeResult<String> {
    #[cfg(debug_assertions)]
    if let Ok(value) = std::env::var("PEPPY_HOSTED_ORIGIN") {
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
fn hosted_error() -> BridgeError {
    BridgeError::new(
        "hosted-unavailable",
        "Peppy Hosted sign-in is not available in this build.",
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
    reqwest::Client::builder()
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
    crate::require_main(window.label())?;
    if client_id().is_none() {
        return Ok(HostedAccountView::unavailable());
    }
    account_view(&*state.store, &hosted, &hosted_origin()?).await
}

#[tauri::command]
pub async fn hosted_sign_out(
    window: tauri::WebviewWindow,
    state: tauri::State<'_, AppState>,
    hosted: tauri::State<'_, HostedState>,
) -> BridgeResult<()> {
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
    crate::require_main(window.label())?;
    if provider != "google" || client_id().is_none() {
        return Err(hosted_error());
    }
    let generation = hosted.sign_in_generation.fetch_add(1, Ordering::SeqCst) + 1;
    let origin = hosted_origin()?;
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
    let redirect_uri = redirect.uri.clone();
    let mut verifier = Zeroizing::new(random_b64url(32)?);
    let state_token = Zeroizing::new(random_b64url(32)?);
    let challenge = pkce_challenge(&verifier);
    let auth = google_url(
        client_id().expect("checked"),
        &redirect_uri,
        &challenge,
        &state_token,
        &attempt.nonce(),
    );
    app.opener()
        .open_url(auth, None::<String>)
        .map_err(|_| BridgeError::new("open-url", "Could not open Google sign-in."))?;
    let code = redirect.receive(&state_token).await?;
    if hosted.sign_in_generation.load(Ordering::SeqCst) != generation {
        return Err(BridgeError::new(
            "hosted-sign-in-cancelled",
            "The sign-in attempt was replaced.",
        ));
    }
    let mut form = vec![
        ("code", code.to_string()),
        ("client_id", client_id().unwrap().to_owned()),
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
    let encoded = Zeroizing::new(
        serde_json::to_vec(&HostedSessionWire {
            account_id: record.account_id.clone(),
            bearer: record.bearer.to_string(),
            expires_at_unix: record.expires_at_unix,
        })
        .map_err(|_| BridgeError::new("host-state", "Could not save the hosted session."))?,
    );
    state.store.set(&session_key(&origin), &encoded)?;
    *hosted.session.lock().await = Some(HostedSessionRecord {
        account_id: record.account_id.clone(),
        bearer: Zeroizing::new(record.bearer.to_string()),
        expires_at_unix: record.expires_at_unix,
    });
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
    let (app, state) = (&app, &*state);
    provision_core(
        &*state.store,
        &hosted,
        &hosted_origin()?,
        |need| async move {
            let purpose = match need {
                PassphraseNeed::Create => crate::dialogs::PassphrasePurpose::Create,
                PassphraseNeed::Resume => crate::dialogs::PassphrasePurpose::Unlock,
            };
            crate::dialogs::passphrase(app, purpose).await
        },
        |credential, passphrase| activate_and_unlock(app, state, credential, passphrase),
    )
    .await
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

/// Pure seam: load session from store with expiry/tombstone handling.
/// Assumes client_id is already checked by the caller.
pub(crate) async fn account_view(
    store: &dyn SecretStore,
    hosted: &HostedState,
    origin: &str,
) -> BridgeResult<HostedAccountView> {
    let Some(session) = load_session_from_store(store, hosted, origin).await? else {
        return Ok(HostedAccountView::signed_out());
    };
    let base = hosted.api_base(origin);
    let client = http(&base)?;
    let bytes = match authenticated(&client, &base, routes::ACCOUNT, &session.bearer).await {
        Ok(bytes) => bytes,
        Err(NetError::Revoked) => {
            tombstone_session(store, hosted, origin).await?;
            return Ok(HostedAccountView::signed_out());
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
        available: true,
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
    P: FnOnce(PassphraseNeed) -> PF,
    PF: std::future::Future<Output = BridgeResult<Option<Zeroizing<String>>>>,
    A: FnOnce(Zeroizing<Vec<u8>>, Zeroizing<String>) -> AF,
    AF: std::future::Future<Output = BridgeResult<()>>,
{
    let Some(session) = load_session_from_store(store, hosted, origin).await? else {
        return Err(BridgeError::new(
            "credentials-required",
            "Sign in to Peppy Hosted first.",
        ));
    };

    let base = hosted.api_base(origin);
    let client = http(&base)?;
    let account_bytes = match authenticated(&client, &base, routes::ACCOUNT, &session.bearer).await
    {
        Ok(bytes) => bytes,
        Err(NetError::Revoked) => return Err(session_expired(store, hosted, origin).await),
        Err(error) => return Err(error.into()),
    };
    let account = parse_hosted_account(
        String::from_utf8(account_bytes).map_err(|_| invalid_response())?,
        session.account_id.clone(),
    )
    .map_err(client_error)?;

    let key = checkpoint_key(origin, &session.account_id);
    let stored = store.get(&key)?.filter(|value| !value.is_empty());
    let restored = stored
        .as_ref()
        .and_then(|value| restore(value.to_vec(), origin, &session.account_id).ok());
    if stored.is_some() && restored.is_none() {
        // An unreadable or foreign checkpoint is discarded; a fresh operation starts instead.
        tombstone(store, &key)?;
    }
    // A vault owned by this account is only recoverable here when it is the one this desktop
    // was creating: the server replays a consumed operation for the same grant and payload.
    let replaying = match (&account.vault_id, &restored) {
        (Some(vault), Some(pending)) if *vault == pending.view().vault_id => true,
        (Some(_), _) => {
            return Err(BridgeError::new(
                "has-vault",
                "This Peppy Hosted account already has a vault.",
            ))
        }
        (None, _) => false,
    };
    if !replaying && account.access != "read_write" {
        return Err(BridgeError::new(
            "entitlement-required",
            "An active Peppy Hosted subscription is required.",
        ));
    }

    let need = if restored.is_some() {
        PassphraseNeed::Resume
    } else {
        PassphraseNeed::Create
    };
    let Some(passphrase) = passphrase(need).await? else {
        return Ok(());
    };
    let provisioning = match restored {
        Some(value) => {
            if !value
                .passphrase_matches(&passphrase)
                .map_err(client_error)?
            {
                return Err(BridgeError::new(
                    "wrong-passphrase",
                    "That passphrase does not match the pending vault.",
                ));
            }
            value
        }
        None => prepare(origin, &session.account_id, &passphrase).map_err(client_error)?,
    };
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
    activate_and_unlock(Zeroizing::new(credential.into_bytes()), passphrase).await?;
    tombstone(store, &key)
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
    let grant =
        match post_bearer_status(client, base, routes::PROVISIONING, &session.bearer, body).await {
            Ok(grant) => grant,
            Err((401, _)) => return Err(session_expired(store, hosted, origin).await),
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
) -> BridgeError {
    if let Err(error) = tombstone_session(store, hosted, origin).await {
        return error;
    }
    BridgeError::new(
        "credentials-required",
        "Your Peppy Hosted sign-in expired. Sign in again to continue.",
    )
}

#[derive(Debug, Clone, Copy)]
pub enum PassphraseNeed {
    Create,
    Resume,
}

async fn load_session_from_store(
    store: &dyn SecretStore,
    hosted: &HostedState,
    origin: &str,
) -> BridgeResult<Option<HostedSessionRecord>> {
    if let Some(value) = hosted.session.lock().await.as_ref() {
        if value.expires_at_unix > now_unix() {
            return Ok(Some(HostedSessionRecord {
                account_id: value.account_id.clone(),
                bearer: Zeroizing::new(value.bearer.to_string()),
                expires_at_unix: value.expires_at_unix,
            }));
        }
    }
    let key = session_key(origin);
    let Some(bytes) = store.get(&key)? else {
        return Ok(None);
    };
    let Some(record) = decode_session(&bytes) else {
        return Ok(None);
    };
    if record.expires_at_unix <= now_unix() {
        tombstone(store, &key)?;
        return Ok(None);
    }
    *hosted.session.lock().await = Some(HostedSessionRecord {
        account_id: record.account_id.clone(),
        bearer: Zeroizing::new(record.bearer.to_string()),
        expires_at_unix: record.expires_at_unix,
    });
    Ok(Some(record))
}

async fn tombstone_session(
    store: &dyn SecretStore,
    hosted: &HostedState,
    origin: &str,
) -> BridgeResult<()> {
    *hosted.session.lock().await = None;
    tombstone(store, &session_key(origin))
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
        let page = if cancelled {
            "Sign-in cancelled. You can return to Peppy."
        } else {
            "You can return to Peppy."
        };
        let _ = stream.write_all(format!("HTTP/1.1 200 OK\r\ncontent-type: text/html; charset=utf-8\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}", page.len(), page).as_bytes()).await;
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
