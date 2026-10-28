//! Comprehensive tests for hosted account and provisioning seams.

use super::*;
use crate::secure_store::MemoryStore;
use axum::{
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use peppy_hosted_api::enrollment::ClientKind;
use serde_json::json;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};
use uuid::Uuid;
use zeroize::Zeroizing;

// ============================================================================
// Fake Server and Test Fixtures
// ============================================================================

/// Credential origin for provisioning tests; requests go to the fake server via `base_override`.
const HTTPS_ORIGIN: &str = "https://peppy.test";
const CLIENT_KIND: ClientKind = ClientKind::Desktop;
const DEVICE_NAME: &str = "Peppy Desktop";

struct FakeServerState {
    auth_config: FakeAuthConfig,
    auth_config_calls: usize,
    auth_attempt_calls: usize,
    auth_config_gate: Option<DeferredAuthConfig>,
    account_revoked: bool,
    renew_response: FakeRenewalResponse,
    renew_calls: usize,
    renew_gate: Option<DeferredRenewal>,
    has_vault: bool,
    access: String,
    grant_expired: bool,
    grant_call_count: usize,
    complete_fails: bool,
    grant_bodies: Vec<serde_json::Value>,
    complete_calls: usize,
    vault_id: Option<String>,
    grant_status: Option<u16>,
}

#[derive(Clone, Copy)]
enum FakeAuthConfig {
    Google,
    MissingGoogle,
    Malformed,
    Oversize,
    Status(u16),
}

#[derive(Clone, Copy, Default)]
enum FakeRenewalResponse {
    #[default]
    Success,
    Invalid200,
    Error(StatusCode),
}

#[derive(Clone)]
struct DeferredAuthConfig {
    reached: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
}

#[derive(Clone)]
struct DeferredRenewal {
    reached: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
}

impl Default for FakeServerState {
    fn default() -> Self {
        Self {
            auth_config: FakeAuthConfig::Google,
            auth_config_calls: 0,
            auth_attempt_calls: 0,
            auth_config_gate: None,
            account_revoked: false,
            renew_response: FakeRenewalResponse::Success,
            renew_calls: 0,
            renew_gate: None,
            has_vault: false,
            access: "read_write".into(),
            grant_expired: false,
            grant_call_count: 0,
            complete_fails: false,
            grant_bodies: Vec::new(),
            complete_calls: 0,
            vault_id: None,
            grant_status: None,
        }
    }
}

async fn fake_auth_config_handler(
    axum::extract::State(state): axum::extract::State<Arc<Mutex<FakeServerState>>>,
) -> impl IntoResponse {
    let (config, gate) = {
        let mut state = state.lock().unwrap();
        state.auth_config_calls += 1;
        (state.auth_config, state.auth_config_gate.clone())
    };
    if let Some(gate) = gate {
        gate.reached.notify_one();
        gate.release.notified().await;
    }
    let (status, body) = match config {
        FakeAuthConfig::Google => (
            StatusCode::OK,
            r#"{"available_providers":["google"]}"#.to_owned(),
        ),
        FakeAuthConfig::MissingGoogle => {
            (StatusCode::OK, r#"{"available_providers":[]}"#.to_owned())
        }
        FakeAuthConfig::Malformed => (StatusCode::OK, "{".to_owned()),
        FakeAuthConfig::Oversize => (StatusCode::OK, "x".repeat(MAX_BODY + 1)),
        FakeAuthConfig::Status(status) => (StatusCode::from_u16(status).unwrap(), String::new()),
    };
    (status, body)
}

async fn fake_auth_attempt_handler(
    axum::extract::State(state): axum::extract::State<Arc<Mutex<FakeServerState>>>,
) -> impl IntoResponse {
    state.lock().unwrap().auth_attempt_calls += 1;
    StatusCode::INTERNAL_SERVER_ERROR
}

async fn fake_account_handler(
    axum::extract::State(state): axum::extract::State<Arc<Mutex<FakeServerState>>>,
) -> impl IntoResponse {
    let state = state.lock().unwrap();
    if state.account_revoked {
        return (StatusCode::UNAUTHORIZED, "").into_response();
    }
    let vault_id = if let Some(vault) = &state.vault_id {
        Some(Uuid::parse_str(vault).unwrap())
    } else if state.has_vault {
        Some(Uuid::new_v4())
    } else {
        None
    };
    let response = json!({
        "account_id": "550e8400-e29b-41d4-a716-446655440000",
        "classification": "provisioning",
        "entitlement": "active",
        "access": state.access,
        "vault_id": vault_id,
        "operation_id": "550e8400-e29b-41d4-a716-446655440001"
    });
    (StatusCode::OK, Json(response)).into_response()
}

async fn fake_session_renew_handler(
    axum::extract::State(state): axum::extract::State<Arc<Mutex<FakeServerState>>>,
) -> impl IntoResponse {
    let (response, gate) = {
        let mut state = state.lock().unwrap();
        state.renew_calls += 1;
        (state.renew_response, state.renew_gate.clone())
    };
    if let Some(gate) = gate {
        gate.reached.notify_one();
        gate.release.notified().await;
    }
    match response {
        FakeRenewalResponse::Success => (
            StatusCode::OK,
            Json(json!({
                "account_id": ACCOUNT,
                "expires_in_seconds": peppy_hosted_api::SESSION_LIFETIME_SECONDS,
            })),
        )
            .into_response(),
        FakeRenewalResponse::Invalid200 => (StatusCode::OK, "{not-json").into_response(),
        FakeRenewalResponse::Error(status) => status.into_response(),
    }
}

async fn fake_provisioning_handler(
    axum::extract::State(state): axum::extract::State<Arc<Mutex<FakeServerState>>>,
    body: String,
) -> impl IntoResponse {
    let mut state = state.lock().unwrap();
    state
        .grant_bodies
        .push(serde_json::from_str(&body).unwrap());
    if let Some(status) = state.grant_status {
        return StatusCode::from_u16(status).unwrap().into_response();
    }
    drop(state);
    let response = json!({
        "grant": format!("pgr_{}", "0".repeat(64)),
        "expires_in_seconds": 300
    });
    (StatusCode::OK, Json(response)).into_response()
}

async fn fake_complete_handler(
    axum::extract::State(state): axum::extract::State<Arc<Mutex<FakeServerState>>>,
    body: String,
) -> impl IntoResponse {
    let mut state = state.lock().unwrap();
    state.complete_calls += 1;
    if state.complete_fails {
        return (StatusCode::INTERNAL_SERVER_ERROR, "").into_response();
    }
    if state.grant_expired && state.grant_call_count == 0 {
        state.grant_call_count += 1;
        return (StatusCode::UNAUTHORIZED, "").into_response();
    }
    let request: serde_json::Value = serde_json::from_str(&body).unwrap();
    let response = json!({
        "operation_id": request["operation_id"],
        "vault_id": request["public_key_profile"]["vault_id"],
        "device_id": request["device_id"],
        "already_provisioned": false
    });
    (StatusCode::OK, Json(response)).into_response()
}

fn encode_session(account_id: &str, bearer: &str, expires_at: u64) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "account_id": account_id,
        "bearer": bearer,
        "expires_at_unix": expires_at
    }))
    .unwrap()
}

#[derive(Default)]
struct ControlledStore {
    values: Mutex<HashMap<String, Vec<u8>>>,
    fail_get: Mutex<bool>,
    fail_nonempty_write: Mutex<bool>,
    fail_write_prefixes: Mutex<Vec<String>>,
    fail_binding_write: Mutex<bool>,
    fail_empty_write: Mutex<bool>,
}

impl ControlledStore {
    fn value(&self, key: &str) -> Option<Vec<u8>> {
        self.values.lock().unwrap().get(key).cloned()
    }

    fn fail_writes_with_prefix(&self, prefix: &str) {
        self.fail_write_prefixes.lock().unwrap().push(prefix.into());
    }

    fn has_key_with_prefix(&self, prefix: &str) -> bool {
        self.values
            .lock()
            .unwrap()
            .keys()
            .any(|key| key.starts_with(prefix))
    }
}

impl crate::secure_store::SecretStore for ControlledStore {
    fn get(&self, key: &str) -> BridgeResult<Option<Zeroizing<Vec<u8>>>> {
        if *self.fail_get.lock().unwrap() {
            return Err(BridgeError::new(
                "secure-store-unavailable",
                "test read failure",
            ));
        }
        Ok(self.value(key).map(Zeroizing::new))
    }

    fn set(&self, key: &str, value: &[u8]) -> BridgeResult<()> {
        if self
            .fail_write_prefixes
            .lock()
            .unwrap()
            .iter()
            .any(|prefix| key.starts_with(prefix))
        {
            return Err(BridgeError::new(
                "secure-store-unavailable",
                "test write failure",
            ));
        }
        if key.starts_with("vault-secret:") && *self.fail_binding_write.lock().unwrap() {
            return Err(BridgeError::new(
                "secure-store-unavailable",
                "test binding write failure",
            ));
        }
        if !value.is_empty() && *self.fail_nonempty_write.lock().unwrap() {
            return Err(BridgeError::new(
                "secure-store-unavailable",
                "test write failure",
            ));
        }
        if value.is_empty() && *self.fail_empty_write.lock().unwrap() {
            return Err(BridgeError::new(
                "secure-store-unavailable",
                "test write failure",
            ));
        }
        self.values.lock().unwrap().insert(key.into(), value.into());
        Ok(())
    }
}

async fn start_fake_server(
    server_state: Arc<Mutex<FakeServerState>>,
) -> (String, tokio::task::JoinHandle<()>) {
    let router = Router::new()
        .route("/hosted/v1/auth/config", get(fake_auth_config_handler))
        .route("/hosted/v1/auth/attempts", post(fake_auth_attempt_handler))
        .route("/hosted/v1/account", get(fake_account_handler))
        .route(
            "/hosted/v1/auth/session/renew",
            post(fake_session_renew_handler),
        )
        .route("/hosted/v1/provisioning", post(fake_provisioning_handler))
        .route(
            "/hosted/v1/provisioning/complete",
            post(fake_complete_handler),
        )
        .with_state(server_state);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let uri = format!("http://{}", addr);

    let handle = tokio::spawn(async move {
        let _ = axum::serve(
            listener,
            router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await;
    });

    (uri, handle)
}

// ============================================================================
// Account View Tests
// ============================================================================

#[tokio::test]
async fn configured_google_is_available_only_when_server_advertises_it() {
    let server_state = Arc::new(Mutex::new(FakeServerState::default()));
    let (uri, _handle) = start_fake_server(server_state.clone()).await;
    let hosted = HostedState::default();

    assert!(
        super::sign_in_available(&hosted, &uri, Some("desktop-client-id"))
            .await
            .unwrap()
    );

    server_state.lock().unwrap().auth_config = FakeAuthConfig::MissingGoogle;
    assert!(
        !super::sign_in_available(&hosted, &uri, Some("desktop-client-id"))
            .await
            .unwrap()
    );
    let server = server_state.lock().unwrap();
    assert_eq!(
        server.auth_attempt_calls, 0,
        "a missing provider must not create an attempt"
    );
}

#[tokio::test]
async fn missing_client_id_is_unavailable_without_fetching_auth_config() {
    let server_state = Arc::new(Mutex::new(FakeServerState::default()));
    let (uri, _handle) = start_fake_server(server_state.clone()).await;
    let hosted = HostedState::default();

    assert!(!super::sign_in_available(&hosted, &uri, None).await.unwrap());
    assert_eq!(server_state.lock().unwrap().auth_config_calls, 0);
}

#[tokio::test]
async fn logout_during_provider_preflight_cancels_the_reserved_sign_in_attempt() {
    let store = MemoryStore::default();
    let hosted = HostedState::default();
    let gate = DeferredAuthConfig {
        reached: Arc::new(tokio::sync::Notify::new()),
        release: Arc::new(tokio::sync::Notify::new()),
    };
    let server_state = Arc::new(Mutex::new(FakeServerState {
        auth_config_gate: Some(gate.clone()),
        ..FakeServerState::default()
    }));
    let (origin, _handle) = start_fake_server(server_state.clone()).await;

    let preflight = super::begin_sign_in(&hosted, &origin, Some("desktop-client-id"));
    tokio::pin!(preflight);
    let reached = gate.reached.notified();
    tokio::pin!(reached);
    tokio::select! {
        _ = &mut reached => {}
        result = &mut preflight => panic!("preflight completed before the deferred provider response: {result:?}"),
    }

    super::tombstone_session(&store, &hosted, &origin)
        .await
        .unwrap();
    gate.release.notify_one();

    let error = preflight.await.unwrap_err();
    assert_eq!(error.code, "hosted-sign-in-cancelled");
    assert!(store
        .get(&super::session_key(&origin))
        .unwrap()
        .is_some_and(|value| value.is_empty()));
    assert!(hosted.session.lock().await.is_none());
    assert_eq!(
        server_state.lock().unwrap().auth_attempt_calls,
        0,
        "the production sign-in flow returns before its auth attempt, loopback, or browser open"
    );
}

#[tokio::test]
async fn invalid_auth_config_responses_return_typed_retryable_errors() {
    for (config, expected_code) in [
        (FakeAuthConfig::Malformed, "server-response"),
        (FakeAuthConfig::Oversize, "response-too-large"),
        (FakeAuthConfig::Status(503), "server-rejected"),
    ] {
        let server_state = Arc::new(Mutex::new(FakeServerState {
            auth_config: config,
            ..FakeServerState::default()
        }));
        let (uri, _handle) = start_fake_server(server_state).await;
        let error =
            super::sign_in_available(&HostedState::default(), &uri, Some("desktop-client-id"))
                .await
                .unwrap_err();
        assert_eq!(error.code, expected_code);
    }
}

#[tokio::test]
async fn loaded_account_survives_missing_google_advertisement() {
    let store = MemoryStore::default();
    let hosted = HostedState::default();
    let server_state = Arc::new(Mutex::new(FakeServerState {
        auth_config: FakeAuthConfig::MissingGoogle,
        ..FakeServerState::default()
    }));
    let (base, _handle) = start_fake_server(server_state).await;
    hosted.base_override.lock().unwrap().replace(base);
    store
        .set(
            &super::session_key(HTTPS_ORIGIN),
            &encode_session(
                ACCOUNT,
                "pst_validtoken123456789012345678901234567",
                super::now_unix() + 3600,
            ),
        )
        .unwrap();

    let view = super::account_view(&store, &hosted, HTTPS_ORIGIN, Some("desktop-client-id"))
        .await
        .unwrap();
    assert!(view.signed_in);
    assert!(!view.available);
    assert!(store
        .get(&super::session_key(HTTPS_ORIGIN))
        .unwrap()
        .is_some_and(|value| !value.is_empty()));
}

#[tokio::test]
async fn loaded_account_survives_malformed_or_unavailable_provider_discovery() {
    for config in [
        FakeAuthConfig::Malformed,
        FakeAuthConfig::Oversize,
        FakeAuthConfig::Status(503),
    ] {
        let store = MemoryStore::default();
        let hosted = HostedState::default();
        let server_state = Arc::new(Mutex::new(FakeServerState {
            auth_config: config,
            ..FakeServerState::default()
        }));
        let (base, _handle) = start_fake_server(server_state).await;
        hosted.base_override.lock().unwrap().replace(base);
        store
            .set(
                &super::session_key(HTTPS_ORIGIN),
                &encode_session(
                    ACCOUNT,
                    "pst_validtoken123456789012345678901234567",
                    super::now_unix() + 3600,
                ),
            )
            .unwrap();

        let view = super::account_view(&store, &hosted, HTTPS_ORIGIN, Some("desktop-client-id"))
            .await
            .unwrap();

        assert!(view.signed_in);
        assert!(!view.available);
        assert!(store
            .get(&super::session_key(HTTPS_ORIGIN))
            .unwrap()
            .is_some_and(|value| !value.is_empty()));
    }
}

#[tokio::test]
async fn loaded_account_survives_refused_provider_discovery() {
    let store = MemoryStore::default();
    let hosted = HostedState::default();
    let server_state = Arc::new(Mutex::new(FakeServerState::default()));
    let (base, _handle) = start_fake_server(server_state).await;
    hosted.base_override.lock().unwrap().replace(base);
    store
        .set(
            &super::session_key(HTTPS_ORIGIN),
            &encode_session(
                ACCOUNT,
                "pst_validtoken123456789012345678901234567",
                super::now_unix() + 3600,
            ),
        )
        .unwrap();
    let refused_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let refused = format!("http://{}", refused_listener.local_addr().unwrap());
    drop(refused_listener);
    hosted
        .auth_config_base_override
        .lock()
        .unwrap()
        .replace(refused);

    let view = super::account_view(&store, &hosted, HTTPS_ORIGIN, Some("desktop-client-id"))
        .await
        .unwrap();

    assert!(view.signed_in);
    assert!(!view.available);
    assert!(store
        .get(&super::session_key(HTTPS_ORIGIN))
        .unwrap()
        .is_some_and(|value| !value.is_empty()));
}

#[tokio::test]
async fn signed_out_provider_discovery_failure_remains_retryable() {
    let server_state = Arc::new(Mutex::new(FakeServerState {
        auth_config: FakeAuthConfig::Status(503),
        ..FakeServerState::default()
    }));
    let (uri, _handle) = start_fake_server(server_state).await;

    let result = super::account_view(
        &MemoryStore::default(),
        &HostedState::default(),
        &uri,
        Some("desktop-client-id"),
    )
    .await;
    let Err(error) = result else {
        panic!("a signed-out discovery failure must remain retryable");
    };

    assert_eq!(error.code, "server-rejected");
}

#[test]
fn hosted_origin_uses_only_native_validated_configuration() {
    assert_eq!(
        super::resolve_hosted_origin(None).unwrap(),
        super::PRODUCTION_ORIGIN
    );
    #[cfg(debug_assertions)]
    {
        assert_eq!(
            super::resolve_hosted_origin(Some("http://127.0.0.1:8787")).unwrap(),
            "http://127.0.0.1:8787"
        );
        assert_eq!(
            super::resolve_hosted_origin(Some("http://peppy.test"))
                .unwrap_err()
                .code,
            "invalid-origin"
        );
    }
    #[cfg(not(debug_assertions))]
    assert_eq!(
        super::resolve_hosted_origin(Some("http://127.0.0.1:8787")).unwrap(),
        super::PRODUCTION_ORIGIN
    );
}

#[test]
fn hosted_origin_url_is_main_window_command_with_main_window_permission() {
    let manifest = include_str!("../../build.rs");
    let capability = include_str!("../../capabilities/default.json");
    let registered = include_str!("../lib.rs");
    let hosted = include_str!("mod.rs");

    assert!(manifest.contains("\"hosted_origin_url\","));
    assert!(capability.contains("\"allow-hosted-origin-url\","));
    assert!(registered.contains("hosted::hosted_origin_url,"));
    assert!(hosted.contains("pub fn hosted_origin_url(window: tauri::WebviewWindow)"));
    assert!(hosted.contains("crate::require_main(window.label())?;"));
}

#[tokio::test]
async fn account_401_signs_out_and_tombstones_session() {
    let store = MemoryStore::default();
    let hosted = HostedState::default();

    let server_state = Arc::new(Mutex::new(FakeServerState {
        account_revoked: true,
        has_vault: false,
        access: "read_write".into(),
        grant_expired: false,
        grant_call_count: 0,
        ..FakeServerState::default()
    }));
    let (uri, _handle) = start_fake_server(server_state).await;

    // Seed a signed-in session for this server URI.
    let session_key = super::session_key(&uri);
    store
        .set(
            &session_key,
            &encode_session(
                "550e8400-e29b-41d4-a716-446655440000",
                "pst_validtoken123456789012345678901234567",
                super::now_unix() + 3600,
            ),
        )
        .unwrap();

    let view = super::account_view(&store, &hosted, &uri, Some("desktop-client-id"))
        .await
        .unwrap();

    assert!(!view.signed_in, "Should be signed out after 401");
    assert!(view.available, "Should still be available");
    let tombstoned = store.get(&session_key).unwrap();
    assert!(
        tombstoned.is_some_and(|v| v.is_empty()),
        "Session should be tombstoned"
    );
}

#[tokio::test]
async fn account_401_uses_current_sign_in_availability_after_tombstoning() {
    for (auth_config, client_id) in [
        (FakeAuthConfig::Google, None),
        (FakeAuthConfig::MissingGoogle, Some("desktop-client-id")),
    ] {
        let store = MemoryStore::default();
        let hosted = HostedState::default();
        let server_state = Arc::new(Mutex::new(FakeServerState {
            account_revoked: true,
            auth_config,
            ..FakeServerState::default()
        }));
        let (uri, _handle) = start_fake_server(server_state).await;
        let session_key = super::session_key(&uri);
        store
            .set(
                &session_key,
                &encode_session(
                    "550e8400-e29b-41d4-a716-446655440000",
                    "pst_validtoken123456789012345678901234567",
                    super::now_unix() + 3600,
                ),
            )
            .unwrap();

        let view = super::account_view(&store, &hosted, &uri, client_id)
            .await
            .unwrap();

        assert!(!view.signed_in);
        assert!(!view.available);
        assert!(store
            .get(&session_key)
            .unwrap()
            .is_some_and(|value| value.is_empty()));
    }
}

#[tokio::test]
async fn final_sign_in_commit_after_logout_is_rejected_and_keeps_tombstone() {
    let store = MemoryStore::default();
    let hosted = HostedState::default();
    let origin = HTTPS_ORIGIN;
    let generation = hosted.sign_in_generation.fetch_add(1, Ordering::SeqCst) + 1;
    let checkpoint_key = super::checkpoint_key(origin, ACCOUNT);
    let device_credential_key = "device-credential-test";
    let checkpoint = b"provisioning-checkpoint";
    let device_credential = b"device-credential";
    store.set(&checkpoint_key, checkpoint).unwrap();
    store.set(device_credential_key, device_credential).unwrap();

    super::tombstone_session(&store, &hosted, origin)
        .await
        .unwrap();

    let error = super::commit_sign_in_session(
        &store,
        &hosted,
        origin,
        generation,
        HostedSessionRecord {
            account_id: ACCOUNT.into(),
            bearer: Zeroizing::new("new-bearer".into()),
            expires_at_unix: super::now_unix() + 3600,
        },
    )
    .await
    .unwrap_err();

    assert_eq!(error.code, "hosted-sign-in-cancelled");
    assert!(store
        .get(&super::session_key(origin))
        .unwrap()
        .is_some_and(|value| value.is_empty()));
    assert_eq!(
        store
            .get(&checkpoint_key)
            .unwrap()
            .as_ref()
            .map(|value| value.as_slice()),
        Some(checkpoint.as_slice())
    );
    assert_eq!(
        store
            .get(device_credential_key)
            .unwrap()
            .as_ref()
            .map(|value| value.as_slice()),
        Some(device_credential.as_slice())
    );
    assert!(hosted.session.lock().await.is_none());
}

#[tokio::test]
async fn failed_sign_out_preserves_the_pending_sign_in_generation_and_cache() {
    let store = ControlledStore::default();
    let hosted = HostedState::default();
    let origin = HTTPS_ORIGIN;
    let key = super::session_key(origin);
    let generation = hosted.sign_in_generation.fetch_add(1, Ordering::SeqCst) + 1;
    store
        .set(
            &key,
            &encode_session(ACCOUNT, "old-bearer", super::now_unix() + 3600),
        )
        .unwrap();
    *hosted.session.lock().await = Some(HostedSessionRecord {
        account_id: ACCOUNT.into(),
        bearer: Zeroizing::new("old-bearer".into()),
        expires_at_unix: super::now_unix() + 3600,
    });
    *store.fail_empty_write.lock().unwrap() = true;

    assert!(super::tombstone_session(&store, &hosted, origin)
        .await
        .is_err());
    assert_eq!(hosted.sign_in_generation.load(Ordering::SeqCst), generation);
    assert_eq!(
        hosted
            .session
            .lock()
            .await
            .as_ref()
            .unwrap()
            .bearer
            .as_str(),
        "old-bearer"
    );

    *store.fail_empty_write.lock().unwrap() = false;
    super::commit_sign_in_session(
        &store,
        &hosted,
        origin,
        generation,
        HostedSessionRecord {
            account_id: ACCOUNT.into(),
            bearer: Zeroizing::new("new-bearer".into()),
            expires_at_unix: super::now_unix() + 3600,
        },
    )
    .await
    .unwrap();

    assert_eq!(
        super::decode_session(&store.value(&key).unwrap())
            .unwrap()
            .bearer
            .as_str(),
        "new-bearer"
    );
}

#[tokio::test]
async fn matching_401_tombstones_the_observed_session() {
    let store = MemoryStore::default();
    let hosted = HostedState::default();
    let key = super::session_key(HTTPS_ORIGIN);
    store
        .set(
            &key,
            &encode_session(ACCOUNT, "old-bearer", super::now_unix() + 3600),
        )
        .unwrap();
    super::load_session_from_store(&store, &hosted, HTTPS_ORIGIN)
        .await
        .unwrap();

    assert!(
        super::tombstone_observed_session(&store, &hosted, HTTPS_ORIGIN, "old-bearer", 0)
            .await
            .unwrap()
    );
    assert!(store
        .get(&key)
        .unwrap()
        .is_some_and(|value| value.is_empty()));
}

#[tokio::test]
async fn stale_401_does_not_tombstone_a_new_bearer_for_the_same_account() {
    let store = MemoryStore::default();
    let hosted = HostedState::default();
    super::commit_sign_in_session(
        &store,
        &hosted,
        HTTPS_ORIGIN,
        0,
        HostedSessionRecord {
            account_id: ACCOUNT.into(),
            bearer: Zeroizing::new("new-bearer".into()),
            expires_at_unix: super::now_unix() + 3600,
        },
    )
    .await
    .unwrap();

    assert!(
        !super::tombstone_observed_session(&store, &hosted, HTTPS_ORIGIN, "old-bearer", 0)
            .await
            .unwrap()
    );
    assert_eq!(
        super::decode_session(
            &store
                .get(&super::session_key(HTTPS_ORIGIN))
                .unwrap()
                .unwrap()
        )
        .unwrap()
        .bearer
        .as_str(),
        "new-bearer"
    );
}

#[tokio::test]
async fn expired_cached_session_is_replaced_by_the_current_stored_session() {
    let store = MemoryStore::default();
    let hosted = HostedState::default();
    store
        .set(
            &super::session_key(HTTPS_ORIGIN),
            &encode_session(ACCOUNT, "stored-bearer", super::now_unix() + 3600),
        )
        .unwrap();
    *hosted.session.lock().await = Some(HostedSessionRecord {
        account_id: ACCOUNT.into(),
        bearer: Zeroizing::new("expired-bearer".into()),
        expires_at_unix: super::now_unix().saturating_sub(1),
    });

    let session = super::load_session_from_store(&store, &hosted, HTTPS_ORIGIN)
        .await
        .unwrap()
        .unwrap();

    assert_eq!(session.bearer.as_str(), "stored-bearer");
    assert_eq!(
        hosted
            .session
            .lock()
            .await
            .as_ref()
            .unwrap()
            .bearer
            .as_str(),
        "stored-bearer"
    );
}

#[tokio::test]
async fn locally_expired_session_stays_available_for_authoritative_renewal() {
    let store = MemoryStore::default();
    let hosted = HostedState::default();
    let server = Arc::new(Mutex::new(FakeServerState::default()));
    let (origin, _handle) = start_fake_server(server.clone()).await;
    hosted.base_override.lock().unwrap().replace(origin.clone());
    let key = super::session_key(&origin);
    store
        .set(
            &key,
            &encode_session(ACCOUNT, "old-bearer", super::now_unix().saturating_sub(1)),
        )
        .unwrap();

    super::renew_session(&store, &hosted, &origin)
        .await
        .unwrap();

    assert_eq!(server.lock().unwrap().renew_calls, 1);
    assert!(
        super::decode_session(&store.get(&key).unwrap().unwrap())
            .unwrap()
            .expires_at_unix
            > super::now_unix()
    );
}

#[tokio::test]
async fn fresh_thirty_day_session_skips_renewal_post() {
    let store = MemoryStore::default();
    let hosted = HostedState::default();
    let server = Arc::new(Mutex::new(FakeServerState::default()));
    let (origin, _handle) = start_fake_server(server.clone()).await;
    hosted.base_override.lock().unwrap().replace(origin.clone());
    store
        .set(
            &super::session_key(&origin),
            &encode_session(
                ACCOUNT,
                "fresh-bearer",
                super::now_unix() + u64::from(peppy_hosted_api::SESSION_LIFETIME_SECONDS),
            ),
        )
        .unwrap();

    super::renew_session(&store, &hosted, &origin)
        .await
        .unwrap();

    assert_eq!(server.lock().unwrap().renew_calls, 0);
}

#[tokio::test]
async fn concurrent_renewals_share_one_post() {
    let store = MemoryStore::default();
    let hosted = HostedState::default();
    let gate = DeferredRenewal {
        reached: Arc::new(tokio::sync::Notify::new()),
        release: Arc::new(tokio::sync::Notify::new()),
    };
    let server = Arc::new(Mutex::new(FakeServerState {
        renew_gate: Some(gate.clone()),
        ..FakeServerState::default()
    }));
    let (origin, _handle) = start_fake_server(server.clone()).await;
    hosted.base_override.lock().unwrap().replace(origin.clone());
    store
        .set(
            &super::session_key(&origin),
            &encode_session(ACCOUNT, "old-bearer", super::now_unix() + 1),
        )
        .unwrap();

    let first = super::renew_session(&store, &hosted, &origin);
    tokio::pin!(first);
    let reached = gate.reached.notified();
    tokio::pin!(reached);
    tokio::select! {
        _ = &mut reached => {}
        result = &mut first => panic!("first renewal completed before the deferred response: {result:?}"),
    }
    let second = super::renew_session(&store, &hosted, &origin);
    gate.release.notify_one();
    let (first, second) = tokio::join!(&mut first, second);

    first.unwrap();
    second.unwrap();
    assert_eq!(server.lock().unwrap().renew_calls, 1);
}

#[tokio::test]
async fn malformed_renewal_response_preserves_bearer_and_uses_failure_backoff() {
    let store = MemoryStore::default();
    let hosted = HostedState::default();
    let server = Arc::new(Mutex::new(FakeServerState {
        renew_response: FakeRenewalResponse::Invalid200,
        ..FakeServerState::default()
    }));
    let (origin, _handle) = start_fake_server(server.clone()).await;
    hosted.base_override.lock().unwrap().replace(origin.clone());
    let key = super::session_key(&origin);
    store
        .set(
            &key,
            &encode_session(ACCOUNT, "original-bearer", super::now_unix() + 1),
        )
        .unwrap();

    assert_eq!(
        super::renew_session(&store, &hosted, &origin)
            .await
            .unwrap_err()
            .code,
        "server-response"
    );
    super::renew_session(&store, &hosted, &origin)
        .await
        .unwrap();

    assert_eq!(server.lock().unwrap().renew_calls, 1);
    assert_eq!(
        super::decode_session(&store.get(&key).unwrap().unwrap())
            .unwrap()
            .bearer
            .as_str(),
        "original-bearer"
    );
}

#[tokio::test]
async fn renewal_write_failure_preserves_bearer_and_uses_failure_backoff() {
    let store = ControlledStore::default();
    let hosted = HostedState::default();
    let server = Arc::new(Mutex::new(FakeServerState::default()));
    let (origin, _handle) = start_fake_server(server.clone()).await;
    hosted.base_override.lock().unwrap().replace(origin.clone());
    let key = super::session_key(&origin);
    store
        .set(
            &key,
            &encode_session(ACCOUNT, "original-bearer", super::now_unix() + 1),
        )
        .unwrap();
    store.fail_writes_with_prefix(&key);

    assert_eq!(
        super::renew_session(&store, &hosted, &origin)
            .await
            .unwrap_err()
            .code,
        "secure-store-unavailable"
    );
    super::renew_session(&store, &hosted, &origin)
        .await
        .unwrap();

    assert_eq!(server.lock().unwrap().renew_calls, 1);
    assert_eq!(
        super::decode_session(&store.value(&key).unwrap())
            .unwrap()
            .bearer
            .as_str(),
        "original-bearer"
    );
}

#[tokio::test]
async fn late_renewal_success_cannot_restore_a_signed_out_session() {
    let store = MemoryStore::default();
    let hosted = HostedState::default();
    let gate = DeferredRenewal {
        reached: Arc::new(tokio::sync::Notify::new()),
        release: Arc::new(tokio::sync::Notify::new()),
    };
    let server = Arc::new(Mutex::new(FakeServerState {
        renew_gate: Some(gate.clone()),
        ..FakeServerState::default()
    }));
    let (origin, _handle) = start_fake_server(server).await;
    hosted.base_override.lock().unwrap().replace(origin.clone());
    let key = super::session_key(&origin);
    store
        .set(
            &key,
            &encode_session(ACCOUNT, "old-bearer", super::now_unix() + 1),
        )
        .unwrap();

    let renewal = super::renew_session(&store, &hosted, &origin);
    tokio::pin!(renewal);
    let reached = gate.reached.notified();
    tokio::pin!(reached);
    tokio::select! {
        _ = &mut reached => {}
        result = &mut renewal => panic!("renewal completed before the deferred response: {result:?}"),
    }
    super::tombstone_session(&store, &hosted, &origin)
        .await
        .unwrap();
    gate.release.notify_one();
    renewal.await.unwrap();

    assert!(store
        .get(&key)
        .unwrap()
        .is_some_and(|value| value.is_empty()));
    assert!(hosted.session.lock().await.is_none());
}

#[tokio::test]
async fn late_renewal_success_cannot_overwrite_a_same_account_replacement_bearer() {
    let store = MemoryStore::default();
    let hosted = HostedState::default();
    let gate = DeferredRenewal {
        reached: Arc::new(tokio::sync::Notify::new()),
        release: Arc::new(tokio::sync::Notify::new()),
    };
    let server = Arc::new(Mutex::new(FakeServerState {
        renew_gate: Some(gate.clone()),
        ..FakeServerState::default()
    }));
    let (origin, _handle) = start_fake_server(server).await;
    hosted.base_override.lock().unwrap().replace(origin.clone());
    let key = super::session_key(&origin);
    store
        .set(
            &key,
            &encode_session(ACCOUNT, "old-bearer", super::now_unix() + 1),
        )
        .unwrap();

    let renewal = super::renew_session(&store, &hosted, &origin);
    tokio::pin!(renewal);
    let reached = gate.reached.notified();
    tokio::pin!(reached);
    tokio::select! {
        _ = &mut reached => {}
        result = &mut renewal => panic!("renewal completed before the deferred response: {result:?}"),
    }
    super::commit_sign_in_session(
        &store,
        &hosted,
        &origin,
        0,
        HostedSessionRecord {
            account_id: ACCOUNT.into(),
            bearer: Zeroizing::new("replacement-bearer".into()),
            expires_at_unix: super::now_unix() + 3600,
        },
    )
    .await
    .unwrap();
    gate.release.notify_one();
    renewal.await.unwrap();

    assert_eq!(
        super::decode_session(&store.get(&key).unwrap().unwrap())
            .unwrap()
            .bearer
            .as_str(),
        "replacement-bearer"
    );
}

#[tokio::test]
async fn renewal_401_only_tombstones_its_observed_bearer() {
    let store = MemoryStore::default();
    let hosted = HostedState::default();
    let server = Arc::new(Mutex::new(FakeServerState {
        renew_response: FakeRenewalResponse::Error(StatusCode::UNAUTHORIZED),
        ..FakeServerState::default()
    }));
    let (origin, _handle) = start_fake_server(server).await;
    hosted.base_override.lock().unwrap().replace(origin.clone());
    let key = super::session_key(&origin);
    store
        .set(
            &key,
            &encode_session(ACCOUNT, "old-bearer", super::now_unix() + 1),
        )
        .unwrap();

    let error = super::renew_session(&store, &hosted, &origin)
        .await
        .unwrap_err();

    assert_eq!(error.code, "hosted-session-required");
    assert!(store
        .get(&key)
        .unwrap()
        .is_some_and(|value| value.is_empty()));
}

#[tokio::test]
async fn session_store_read_failure_fails_closed() {
    let store = ControlledStore::default();
    let hosted = HostedState::default();
    *store.fail_get.lock().unwrap() = true;

    let error = super::load_session_from_store(&store, &hosted, HTTPS_ORIGIN)
        .await
        .unwrap_err();

    assert_eq!(error.code, "secure-store-unavailable");
}

#[tokio::test]
async fn account_read_write_no_vault_shows_fields() {
    let store = MemoryStore::default();
    let hosted = HostedState::default();

    let server_state = Arc::new(Mutex::new(FakeServerState {
        account_revoked: false,
        has_vault: false,
        access: "read_write".into(),
        grant_expired: false,
        grant_call_count: 0,
        ..FakeServerState::default()
    }));
    let (uri, _handle) = start_fake_server(server_state).await;

    let session_key = super::session_key(&uri);
    store
        .set(
            &session_key,
            &encode_session(
                "550e8400-e29b-41d4-a716-446655440000",
                "pst_validtoken123456789012345678901234567",
                super::now_unix() + 3600,
            ),
        )
        .unwrap();

    let view = super::account_view(&store, &hosted, &uri, Some("desktop-client-id"))
        .await
        .unwrap();

    assert!(view.signed_in);
    assert!(view.available);
    assert_eq!(view.access, Some("read_write".into()));
    assert_eq!(view.entitlement, Some("active".into()));
    assert!(!view.has_vault);
    assert!(!view.resumable);
}

#[tokio::test]
async fn account_resumable_when_checkpoint_exists() {
    let store = MemoryStore::default();
    let hosted = HostedState::default();

    let server_state = Arc::new(Mutex::new(FakeServerState {
        account_revoked: false,
        has_vault: false,
        access: "read_write".into(),
        grant_expired: false,
        grant_call_count: 0,
        ..FakeServerState::default()
    }));
    let (uri, _handle) = start_fake_server(server_state).await;

    let account_id = "550e8400-e29b-41d4-a716-446655440000";
    let session_key = super::session_key(&uri);
    store
        .set(
            &session_key,
            &encode_session(
                account_id,
                "pst_validtoken123456789012345678901234567",
                super::now_unix() + 3600,
            ),
        )
        .unwrap();

    let checkpoint_key = super::checkpoint_key(&uri, account_id);
    store.set(&checkpoint_key, b"checkpoint-data").unwrap();

    let view = super::account_view(&store, &hosted, &uri, Some("desktop-client-id"))
        .await
        .unwrap();

    assert!(
        view.resumable,
        "Should show resumable when checkpoint exists"
    );
}

// ============================================================================
// Provision Core Tests
// ============================================================================

#[tokio::test]
async fn absent_hosted_session_requires_sign_in_without_claiming_expiry() {
    let store = MemoryStore::default();
    let hosted = HostedState::default();

    let error = super::provision_core(
        &store,
        &hosted,
        HTTPS_ORIGIN,
        || async { panic!("an absent Hosted session must not prompt") },
        |_, _| async { panic!("an absent Hosted session must not activate") },
    )
    .await
    .unwrap_err();

    assert_eq!(error.code, "hosted-session-required");
    assert!(error.message.contains("Sign in again"));
    assert!(!error.message.contains("expired"));
}

#[tokio::test]
async fn absent_hosted_session_for_enrollment_requires_sign_in_without_claiming_expiry() {
    let root = tempfile::tempdir().unwrap();
    let store = Arc::new(MemoryStore::default());
    let state = crate::AppState::new(root.path().to_path_buf(), store, crate::tests::noop());
    let origin = super::hosted_origin().unwrap();

    let error = super::enrollment_account_session(&state, &HostedState::default(), &origin)
        .await
        .unwrap_err();

    assert_eq!(error.code, "hosted-session-required");
    assert!(error.message.contains("Sign in again"));
    assert!(!error.message.contains("expired"));
}

#[tokio::test]
async fn fresh_provision_generates_a_vault_secret_without_prompting() {
    let store = MemoryStore::default();
    let hosted = HostedState::default();
    let server_state = Arc::new(Mutex::new(FakeServerState::default()));
    let (base, _handle) = start_fake_server(server_state).await;
    hosted.base_override.lock().unwrap().replace(base);
    let origin = HTTPS_ORIGIN;
    let prompts = Arc::new(Mutex::new(0));
    let prompt_count = prompts.clone();
    store
        .set(
            &super::session_key(origin),
            &encode_session(
                ACCOUNT,
                "pst_validtoken123456789012345678901234567",
                super::now_unix() + 3600,
            ),
        )
        .unwrap();

    let result = super::provision_core(
        &store,
        &hosted,
        origin,
        move || {
            let prompt_count = prompt_count.clone();
            async move {
                *prompt_count.lock().unwrap() += 1;
                Ok(Some(Zeroizing::new("twelve-chars".into())))
            }
        },
        |_, passphrase| async move {
            assert!(peppy_crypto::passphrase::passphrase_acceptable(&passphrase));
            Ok(())
        },
    )
    .await;
    result.unwrap();
    assert_eq!(*prompts.lock().unwrap(), 0);
    assert!(store
        .get(&super::checkpoint_key(origin, ACCOUNT))
        .unwrap()
        .is_some_and(|value| value.is_empty()));
}

#[tokio::test]
async fn pending_secret_write_failure_prevents_checkpoint_and_posts() {
    let hosted = HostedState::default();
    let server = Arc::new(Mutex::new(FakeServerState::default()));
    let (base, _handle) = start_fake_server(server.clone()).await;
    hosted.base_override.lock().unwrap().replace(base);
    let origin = HTTPS_ORIGIN;
    for prefix in [super::PENDING_SECRET_PREFIX, super::RECOVERY_INDEX_PREFIX] {
        let store = ControlledStore::default();
        store
            .set(
                &super::session_key(origin),
                &encode_session(
                    ACCOUNT,
                    "pst_validtoken123456789012345678901234567",
                    super::now_unix() + 3600,
                ),
            )
            .unwrap();
        store.fail_writes_with_prefix(prefix);

        let error = super::provision_core(
            &store,
            &hosted,
            origin,
            || async { panic!("fresh Hosted provisioning must not prompt") },
            |_, _| async { panic!("activation must not run") },
        )
        .await
        .unwrap_err();

        assert_eq!(error.code, "secure-store-unavailable");
        assert!(store
            .value(&super::checkpoint_key(origin, ACCOUNT))
            .is_none());
        assert!(!store.has_key_with_prefix(super::RECOVERY_INDEX_PREFIX));
    }
    assert!(server.lock().unwrap().grant_bodies.is_empty());
    assert_eq!(server.lock().unwrap().complete_calls, 0);
}

#[tokio::test]
async fn concurrent_provision_calls_share_one_operation_and_secret() {
    let store = MemoryStore::default();
    let hosted = HostedState::default();
    let server = Arc::new(Mutex::new(FakeServerState {
        complete_fails: true,
        ..FakeServerState::default()
    }));
    let (base, _handle) = start_fake_server(server.clone()).await;
    hosted.base_override.lock().unwrap().replace(base);
    store
        .set(
            &super::session_key(HTTPS_ORIGIN),
            &encode_session(
                ACCOUNT,
                "pst_validtoken123456789012345678901234567",
                super::now_unix() + 3600,
            ),
        )
        .unwrap();

    let first = super::provision_core(
        &store,
        &hosted,
        HTTPS_ORIGIN,
        || async { panic!("fresh Hosted provisioning must not prompt") },
        |_, _| async { panic!("completion failure must not activate") },
    );
    let second = super::provision_core(
        &store,
        &hosted,
        HTTPS_ORIGIN,
        || async { panic!("serialized resume must use the saved secret") },
        |_, _| async { panic!("completion failure must not activate") },
    );
    let (first, second) = tokio::join!(first, second);
    assert!(first.is_err() && second.is_err());
    let server = server.lock().unwrap();
    assert_eq!(
        server.grant_bodies.len(),
        1,
        "the second call reuses the saved grant"
    );
    assert_eq!(server.complete_calls, 2);
}

#[tokio::test]
async fn failed_postactivation_binding_handoff_is_repaired_by_verified_discovery() {
    let store = ControlledStore::default();
    let hosted = HostedState::default();
    let server = Arc::new(Mutex::new(FakeServerState::default()));
    let (base, _handle) = start_fake_server(server).await;
    hosted.base_override.lock().unwrap().replace(base);
    store
        .set(
            &super::session_key(HTTPS_ORIGIN),
            &encode_session(
                ACCOUNT,
                "pst_validtoken123456789012345678901234567",
                super::now_unix() + 3600,
            ),
        )
        .unwrap();
    *store.fail_binding_write.lock().unwrap() = true;

    super::provision_core(
        &store,
        &hosted,
        HTTPS_ORIGIN,
        || async { panic!("fresh Hosted provisioning must not prompt") },
        |_, _| async { Ok(()) },
    )
    .await
    .unwrap();

    assert!(store
        .value(&super::checkpoint_key(HTTPS_ORIGIN, ACCOUNT))
        .is_some_and(|value| !value.is_empty()));
    assert!(store
        .value(&super::pending_secret_key(HTTPS_ORIGIN, ACCOUNT))
        .is_some_and(|value| !value.is_empty()));

    let checkpoint = super::checkpoint_key(HTTPS_ORIGIN, ACCOUNT);
    let provisioning =
        peppy_hosted_client::restore(store.value(&checkpoint).unwrap(), HTTPS_ORIGIN, ACCOUNT)
            .unwrap();
    let binding = super::binding_for(&provisioning);
    *store.fail_binding_write.lock().unwrap() = false;
    super::sweep_verified_pending_secret(&store, &binding, |secret| {
        provisioning.passphrase_matches(secret).unwrap()
    })
    .unwrap();

    for key in [
        checkpoint,
        super::pending_secret_key(HTTPS_ORIGIN, ACCOUNT),
        super::recovery_index_key(&binding),
    ] {
        assert!(store.value(&key).is_some_and(|value| value.is_empty()));
    }
}

#[tokio::test]
async fn invalid_checkpoint_is_preserved_and_never_replaced() {
    let store = MemoryStore::default();
    let hosted = HostedState::default();
    let server = Arc::new(Mutex::new(FakeServerState::default()));
    let (origin, _handle) = start_fake_server(server.clone()).await;
    let checkpoint = super::checkpoint_key(&origin, ACCOUNT);
    store.set(&checkpoint, b"invalid checkpoint").unwrap();
    store
        .set(
            &super::session_key(&origin),
            &encode_session(
                ACCOUNT,
                "pst_validtoken123456789012345678901234567",
                super::now_unix() + 3600,
            ),
        )
        .unwrap();

    let error = super::provision_core(
        &store,
        &hosted,
        &origin,
        || async { panic!("invalid checkpoint must not prompt") },
        |_, _| async { panic!("invalid checkpoint must not activate") },
    )
    .await
    .unwrap_err();

    assert_eq!(error.code, "hosted-checkpoint-invalid");
    assert_eq!(
        store.get(&checkpoint).unwrap().unwrap().as_slice(),
        b"invalid checkpoint"
    );
    assert!(server.lock().unwrap().grant_bodies.is_empty());
}

#[tokio::test]
async fn legacy_recovery_persists_before_post_and_retries_without_prompting() {
    let (store, hosted, server, _vault, _account) = resumable_fixture(
        FakeServerState {
            complete_fails: true,
            ..FakeServerState::default()
        },
        false,
    )
    .await;
    let prompts = Arc::new(Mutex::new(0));
    let prompt_count = prompts.clone();

    assert!(super::provision_core(
        &store,
        &hosted,
        HTTPS_ORIGIN,
        move || {
            let prompt_count = prompt_count.clone();
            async move {
                *prompt_count.lock().unwrap() += 1;
                Ok(Some(Zeroizing::new(RESUME_PHRASE.into())))
            }
        },
        |_, _| async { panic!("failed POST must not activate") },
    )
    .await
    .is_err());

    let checkpoint = super::checkpoint_key(HTTPS_ORIGIN, ACCOUNT);
    let provisioning = peppy_hosted_client::restore(
        store.get(&checkpoint).unwrap().unwrap().to_vec(),
        HTTPS_ORIGIN,
        ACCOUNT,
    )
    .unwrap();
    let index = super::read_recovery_index(&store, &provisioning)
        .unwrap()
        .unwrap();
    assert!(!index.generated);
    assert_eq!(
        super::read_text_secret(&store, &super::pending_secret_key(HTTPS_ORIGIN, ACCOUNT))
            .unwrap()
            .unwrap()
            .as_str(),
        RESUME_PHRASE
    );

    server.lock().unwrap().complete_fails = false;
    super::provision_core(
        &store,
        &hosted,
        HTTPS_ORIGIN,
        || async { panic!("saved legacy secret must not prompt again") },
        |_, _| async { Ok(()) },
    )
    .await
    .unwrap();
    assert_eq!(*prompts.lock().unwrap(), 1);
}

#[tokio::test]
async fn restored_checkpoint_uses_binding_secret_when_pending_secret_is_gone() {
    let (store, hosted, _server, _vault, _account) =
        resumable_fixture(FakeServerState::default(), false).await;
    let checkpoint = super::checkpoint_key(HTTPS_ORIGIN, ACCOUNT);
    let provisioning = peppy_hosted_client::restore(
        store.get(&checkpoint).unwrap().unwrap().to_vec(),
        HTTPS_ORIGIN,
        ACCOUNT,
    )
    .unwrap();
    let binding = super::binding_for(&provisioning);
    store
        .set(&binding.vault_secret_account(), RESUME_PHRASE.as_bytes())
        .unwrap();
    let activated = Arc::new(Mutex::new(false));
    let activation = activated.clone();

    super::provision_core(
        &store,
        &hosted,
        HTTPS_ORIGIN,
        || async { panic!("binding secret must avoid a recovery prompt") },
        move |_, secret| async move {
            assert_eq!(secret.as_str(), RESUME_PHRASE);
            *activation.lock().unwrap() = true;
            Ok(())
        },
    )
    .await
    .unwrap();
    assert!(*activated.lock().unwrap());
}

#[tokio::test]
async fn secure_store_read_failure_preserves_checkpoint() {
    let store = ControlledStore::default();
    let hosted = HostedState::default();
    let origin = HTTPS_ORIGIN;
    let checkpoint = super::checkpoint_key(origin, ACCOUNT);
    store.set(&checkpoint, b"preserve this checkpoint").unwrap();
    store
        .set(
            &super::session_key(origin),
            &encode_session(
                ACCOUNT,
                "pst_validtoken123456789012345678901234567",
                super::now_unix() + 3600,
            ),
        )
        .unwrap();
    *store.fail_get.lock().unwrap() = true;

    let error = super::provision_core(
        &store,
        &hosted,
        origin,
        || async { panic!("read failure must not prompt") },
        |_, _| async { panic!("read failure must not activate") },
    )
    .await
    .unwrap_err();

    assert_eq!(error.code, "secure-store-unavailable");
    *store.fail_get.lock().unwrap() = false;
    assert_eq!(
        store.value(&checkpoint).unwrap(),
        b"preserve this checkpoint"
    );
}

#[test]
fn pending_secret_keys_are_account_isolated() {
    let first = super::pending_secret_key(HTTPS_ORIGIN, ACCOUNT);
    let second = super::pending_secret_key(HTTPS_ORIGIN, "660e8400-e29b-41d4-a716-446655440000");
    assert_ne!(first, second);
    assert!(!first.contains(ACCOUNT));
}

#[test]
fn verified_sweep_moves_only_a_matching_recovery_tuple() {
    let store = MemoryStore::default();
    let provisioning = peppy_hosted_client::prepare(
        HTTPS_ORIGIN,
        ACCOUNT,
        RESUME_PHRASE,
        CLIENT_KIND,
        DEVICE_NAME,
    )
    .unwrap();
    let binding = super::binding_for(&provisioning);
    let checkpoint = super::checkpoint_key(HTTPS_ORIGIN, ACCOUNT);
    let pending = super::pending_secret_key(HTTPS_ORIGIN, ACCOUNT);
    let index = super::recovery_index_key(&binding);
    store
        .set(&checkpoint, &provisioning.checkpoint().unwrap())
        .unwrap();
    store.set(&pending, RESUME_PHRASE.as_bytes()).unwrap();
    store
        .set(
            &index,
            &serde_json::to_vec(&super::recovery_index(&provisioning, true)).unwrap(),
        )
        .unwrap();

    super::sweep_verified_pending_secret(&store, &binding, |secret| secret == RESUME_PHRASE)
        .unwrap();

    assert_eq!(
        super::read_text_secret(&store, &binding.vault_secret_account())
            .unwrap()
            .unwrap()
            .as_str(),
        RESUME_PHRASE
    );
    for key in [&checkpoint, &pending, &index] {
        assert!(store
            .get(key)
            .unwrap()
            .is_some_and(|value| value.is_empty()));
    }
}

#[test]
fn verified_sweep_repairs_pending_secret_after_checkpoint_cleanup() {
    let store = MemoryStore::default();
    let provisioning = peppy_hosted_client::prepare(
        HTTPS_ORIGIN,
        ACCOUNT,
        RESUME_PHRASE,
        CLIENT_KIND,
        DEVICE_NAME,
    )
    .unwrap();
    let binding = super::binding_for(&provisioning);
    let checkpoint = super::checkpoint_key(HTTPS_ORIGIN, ACCOUNT);
    let pending = super::pending_secret_key(HTTPS_ORIGIN, ACCOUNT);
    let index = super::recovery_index_key(&binding);
    store.set(&checkpoint, b"").unwrap();
    store.set(&pending, RESUME_PHRASE.as_bytes()).unwrap();
    store
        .set(
            &index,
            &serde_json::to_vec(&super::recovery_index(&provisioning, true)).unwrap(),
        )
        .unwrap();

    super::sweep_verified_pending_secret(&store, &binding, |secret| secret == RESUME_PHRASE)
        .unwrap();

    assert!(store
        .get(&binding.vault_secret_account())
        .unwrap()
        .is_some_and(|value| value.as_slice() == RESUME_PHRASE.as_bytes()));
    assert!(store
        .get(&pending)
        .unwrap()
        .is_some_and(|value| value.is_empty()));
    assert!(store
        .get(&index)
        .unwrap()
        .is_some_and(|value| value.is_empty()));
}

#[test]
fn verified_sweep_preserves_conflicting_binding_and_recovery_data() {
    let store = MemoryStore::default();
    let provisioning = peppy_hosted_client::prepare(
        HTTPS_ORIGIN,
        ACCOUNT,
        RESUME_PHRASE,
        CLIENT_KIND,
        DEVICE_NAME,
    )
    .unwrap();
    let binding = super::binding_for(&provisioning);
    let checkpoint = super::checkpoint_key(HTTPS_ORIGIN, ACCOUNT);
    let pending = super::pending_secret_key(HTTPS_ORIGIN, ACCOUNT);
    let index = super::recovery_index_key(&binding);
    store
        .set(&checkpoint, &provisioning.checkpoint().unwrap())
        .unwrap();
    store.set(&pending, RESUME_PHRASE.as_bytes()).unwrap();
    store
        .set(
            &index,
            &serde_json::to_vec(&super::recovery_index(&provisioning, true)).unwrap(),
        )
        .unwrap();
    store
        .set(
            &binding.vault_secret_account(),
            b"different verified secret",
        )
        .unwrap();

    super::sweep_verified_pending_secret(&store, &binding, |_| {
        panic!("an existing binding must avoid repeated verification")
    })
    .unwrap();

    for key in [&checkpoint, &pending, &index] {
        assert!(store
            .get(key)
            .unwrap()
            .is_some_and(|value| !value.is_empty()));
    }
}

#[test]
fn verified_sweep_keeps_recovery_data_when_verification_fails() {
    let store = MemoryStore::default();
    let provisioning = peppy_hosted_client::prepare(
        HTTPS_ORIGIN,
        ACCOUNT,
        RESUME_PHRASE,
        CLIENT_KIND,
        DEVICE_NAME,
    )
    .unwrap();
    let binding = super::binding_for(&provisioning);
    let checkpoint = super::checkpoint_key(HTTPS_ORIGIN, ACCOUNT);
    let pending = super::pending_secret_key(HTTPS_ORIGIN, ACCOUNT);
    let index = super::recovery_index_key(&binding);
    store
        .set(&checkpoint, &provisioning.checkpoint().unwrap())
        .unwrap();
    store.set(&pending, RESUME_PHRASE.as_bytes()).unwrap();
    store
        .set(
            &index,
            &serde_json::to_vec(&super::recovery_index(&provisioning, true)).unwrap(),
        )
        .unwrap();

    super::sweep_verified_pending_secret(&store, &binding, |_| false).unwrap();

    for key in [&checkpoint, &pending, &index] {
        assert!(store
            .get(key)
            .unwrap()
            .is_some_and(|value| !value.is_empty()));
    }
}

#[test]
fn verified_sweep_keeps_recovery_data_for_a_nonmatching_index() {
    let store = MemoryStore::default();
    let provisioning = peppy_hosted_client::prepare(
        HTTPS_ORIGIN,
        ACCOUNT,
        RESUME_PHRASE,
        CLIENT_KIND,
        DEVICE_NAME,
    )
    .unwrap();
    let binding = super::binding_for(&provisioning);
    let checkpoint = super::checkpoint_key(HTTPS_ORIGIN, ACCOUNT);
    let pending = super::pending_secret_key(HTTPS_ORIGIN, ACCOUNT);
    let index = super::recovery_index_key(&binding);
    store
        .set(&checkpoint, &provisioning.checkpoint().unwrap())
        .unwrap();
    store.set(&pending, RESUME_PHRASE.as_bytes()).unwrap();
    let mut recovery_index = super::recovery_index(&provisioning, true);
    recovery_index.operation_id = "different-operation".into();
    store
        .set(&index, &serde_json::to_vec(&recovery_index).unwrap())
        .unwrap();

    super::sweep_verified_pending_secret(&store, &binding, |_| {
        panic!("a nonmatching index must not verify")
    })
    .unwrap();

    for key in [&checkpoint, &pending, &index] {
        assert!(store
            .get(key)
            .unwrap()
            .is_some_and(|value| !value.is_empty()));
    }
}

#[test]
fn cleanup_failures_retain_the_recovery_index() {
    for failed_key in [super::CHECKPOINT_PREFIX, super::PENDING_SECRET_PREFIX] {
        let store = ControlledStore::default();
        let provisioning = peppy_hosted_client::prepare(
            HTTPS_ORIGIN,
            ACCOUNT,
            RESUME_PHRASE,
            CLIENT_KIND,
            DEVICE_NAME,
        )
        .unwrap();
        let binding = super::binding_for(&provisioning);
        let checkpoint = super::checkpoint_key(HTTPS_ORIGIN, ACCOUNT);
        let pending = super::pending_secret_key(HTTPS_ORIGIN, ACCOUNT);
        let index = super::recovery_index_key(&binding);
        store
            .set(&checkpoint, &provisioning.checkpoint().unwrap())
            .unwrap();
        store.set(&pending, RESUME_PHRASE.as_bytes()).unwrap();
        store
            .set(
                &index,
                &serde_json::to_vec(&super::recovery_index(&provisioning, true)).unwrap(),
            )
            .unwrap();
        store.fail_writes_with_prefix(failed_key);

        super::hand_off_and_cleanup(
            &store,
            &checkpoint,
            &pending,
            &provisioning,
            &Zeroizing::new(RESUME_PHRASE.into()),
        );

        assert!(store.value(&index).is_some_and(|value| !value.is_empty()));
    }
}

#[test]
fn postactivation_handoff_never_overwrites_a_conflicting_binding() {
    let store = MemoryStore::default();
    let provisioning = peppy_hosted_client::prepare(
        HTTPS_ORIGIN,
        ACCOUNT,
        RESUME_PHRASE,
        CLIENT_KIND,
        DEVICE_NAME,
    )
    .unwrap();
    let binding = super::binding_for(&provisioning);
    let checkpoint = super::checkpoint_key(HTTPS_ORIGIN, ACCOUNT);
    let pending = super::pending_secret_key(HTTPS_ORIGIN, ACCOUNT);
    let index = super::recovery_index_key(&binding);
    store
        .set(&checkpoint, &provisioning.checkpoint().unwrap())
        .unwrap();
    store.set(&pending, RESUME_PHRASE.as_bytes()).unwrap();
    store
        .set(
            &index,
            &serde_json::to_vec(&super::recovery_index(&provisioning, true)).unwrap(),
        )
        .unwrap();
    store
        .set(
            &binding.vault_secret_account(),
            b"different verified secret",
        )
        .unwrap();

    super::hand_off_and_cleanup(
        &store,
        &checkpoint,
        &pending,
        &provisioning,
        &Zeroizing::new(RESUME_PHRASE.into()),
    );

    assert_eq!(
        store
            .get(&binding.vault_secret_account())
            .unwrap()
            .unwrap()
            .as_slice(),
        b"different verified secret"
    );
    for key in [&checkpoint, &pending, &index] {
        assert!(store
            .get(key)
            .unwrap()
            .is_some_and(|value| !value.is_empty()));
    }
}

#[test]
fn verified_sweep_binding_write_failure_keeps_recovery_data() {
    let store = ControlledStore::default();
    let provisioning = peppy_hosted_client::prepare(
        HTTPS_ORIGIN,
        ACCOUNT,
        RESUME_PHRASE,
        CLIENT_KIND,
        DEVICE_NAME,
    )
    .unwrap();
    let binding = super::binding_for(&provisioning);
    let checkpoint = super::checkpoint_key(HTTPS_ORIGIN, ACCOUNT);
    let pending = super::pending_secret_key(HTTPS_ORIGIN, ACCOUNT);
    let index = super::recovery_index_key(&binding);
    store
        .set(&checkpoint, &provisioning.checkpoint().unwrap())
        .unwrap();
    store.set(&pending, RESUME_PHRASE.as_bytes()).unwrap();
    store
        .set(
            &index,
            &serde_json::to_vec(&super::recovery_index(&provisioning, true)).unwrap(),
        )
        .unwrap();
    *store.fail_nonempty_write.lock().unwrap() = true;

    assert!(
        super::sweep_verified_pending_secret(&store, &binding, |secret| secret == RESUME_PHRASE)
            .is_err()
    );
    for key in [&checkpoint, &pending, &index] {
        assert!(store.value(key).is_some_and(|value| !value.is_empty()));
    }
}

#[tokio::test]
async fn provision_has_vault_returns_error() {
    let store = MemoryStore::default();
    let hosted = HostedState::default();

    let server_state = Arc::new(Mutex::new(FakeServerState {
        account_revoked: false,
        has_vault: true,
        access: "read_write".into(),
        grant_expired: false,
        grant_call_count: 0,
        ..FakeServerState::default()
    }));
    let (uri, _handle) = start_fake_server(server_state).await;

    let account_id = "550e8400-e29b-41d4-a716-446655440000";
    let session_key = super::session_key(&uri);
    store
        .set(
            &session_key,
            &encode_session(
                account_id,
                "pst_validtoken123456789012345678901234567",
                super::now_unix() + 3600,
            ),
        )
        .unwrap();

    let passphrase_fn = || async {
        Ok(Some(Zeroizing::new(
            "alpha bravo charlie delta echo foxtrot".into(),
        )))
    };
    let activate_fn = |_cred: Zeroizing<Vec<u8>>, _pass: Zeroizing<String>| async { Ok(()) };

    let result = super::provision_core(&store, &hosted, &uri, passphrase_fn, activate_fn).await;

    match result {
        Err(e) => assert_eq!(
            e.code, "has-vault",
            "Expected has-vault error, got {}",
            e.code
        ),
        Ok(_) => panic!("Expected has-vault error"),
    }
}

#[tokio::test]
async fn provision_read_only_returns_entitlement_error() {
    let store = MemoryStore::default();
    let hosted = HostedState::default();

    let server_state = Arc::new(Mutex::new(FakeServerState {
        account_revoked: false,
        has_vault: false,
        access: "read_only".into(),
        grant_expired: false,
        grant_call_count: 0,
        ..FakeServerState::default()
    }));
    let (uri, _handle) = start_fake_server(server_state).await;

    let account_id = "550e8400-e29b-41d4-a716-446655440000";
    let session_key = super::session_key(&uri);
    store
        .set(
            &session_key,
            &encode_session(
                account_id,
                "pst_validtoken123456789012345678901234567",
                super::now_unix() + 3600,
            ),
        )
        .unwrap();

    let passphrase_fn = || async {
        Ok(Some(Zeroizing::new(
            "alpha bravo charlie delta echo foxtrot".into(),
        )))
    };
    let activate_fn = |_cred: Zeroizing<Vec<u8>>, _pass: Zeroizing<String>| async { Ok(()) };

    let result = super::provision_core(&store, &hosted, &uri, passphrase_fn, activate_fn).await;

    match result {
        Err(e) => assert_eq!(
            e.code, "entitlement-required",
            "Expected entitlement-required error, got {}",
            e.code
        ),
        Ok(_) => panic!("Expected entitlement-required error"),
    }
}

#[tokio::test]
async fn provision_wrong_resume_passphrase_returns_error() {
    let store = MemoryStore::default();
    let hosted = HostedState::default();
    let server_state = Arc::new(Mutex::new(FakeServerState::default()));
    let (base, _handle) = start_fake_server(server_state.clone()).await;
    hosted.base_override.lock().unwrap().replace(base);
    let origin = HTTPS_ORIGIN;
    let account_id = "550e8400-e29b-41d4-a716-446655440000";

    let session_key = super::session_key(origin);
    store
        .set(
            &session_key,
            &encode_session(
                account_id,
                "pst_validtoken123456789012345678901234567",
                super::now_unix() + 3600,
            ),
        )
        .unwrap();

    // Create a valid checkpoint with a known passphrase.
    let correct_phrase = "alpha bravo charlie delta echo foxtrot";
    let prov =
        peppy_hosted_client::prepare(origin, account_id, correct_phrase, CLIENT_KIND, DEVICE_NAME)
            .unwrap();
    let checkpoint_bytes = prov.checkpoint().unwrap();
    let checkpoint_key = super::checkpoint_key(origin, account_id);
    store.set(&checkpoint_key, &checkpoint_bytes).unwrap();

    let passphrase_fn = || async move { Ok(Some(Zeroizing::new("wrong passphrase".into()))) };
    let activate_fn = |_cred: Zeroizing<Vec<u8>>, _pass: Zeroizing<String>| async { Ok(()) };

    let result = super::provision_core(&store, &hosted, origin, passphrase_fn, activate_fn).await;

    match result {
        Err(e) => assert_eq!(
            e.code, "wrong-passphrase",
            "Expected wrong-passphrase error, got {}",
            e.code
        ),
        Ok(_) => panic!("Expected wrong-passphrase error"),
    }
    assert!(
        server_state.lock().unwrap().grant_bodies.is_empty(),
        "A wrong passphrase must not reach the provisioning grant"
    );
}

#[tokio::test]
async fn generated_checkpoint_without_secret_fails_without_prompt_or_regeneration() {
    let store = MemoryStore::default();
    let hosted = HostedState::default();
    let server = Arc::new(Mutex::new(FakeServerState::default()));
    let (base, _handle) = start_fake_server(server.clone()).await;
    hosted.base_override.lock().unwrap().replace(base);
    let provisioning = peppy_hosted_client::prepare(
        HTTPS_ORIGIN,
        ACCOUNT,
        RESUME_PHRASE,
        CLIENT_KIND,
        DEVICE_NAME,
    )
    .unwrap();
    let binding = super::binding_for(&provisioning);
    let checkpoint = super::checkpoint_key(HTTPS_ORIGIN, ACCOUNT);
    store
        .set(&checkpoint, &provisioning.checkpoint().unwrap())
        .unwrap();
    store
        .set(
            &super::recovery_index_key(&binding),
            &serde_json::to_vec(&super::recovery_index(&provisioning, true)).unwrap(),
        )
        .unwrap();
    store
        .set(
            &super::session_key(HTTPS_ORIGIN),
            &encode_session(
                ACCOUNT,
                "pst_validtoken123456789012345678901234567",
                super::now_unix() + 3600,
            ),
        )
        .unwrap();

    let error = super::provision_core(
        &store,
        &hosted,
        HTTPS_ORIGIN,
        || async { panic!("generated secret loss must not prompt") },
        |_, _| async { panic!("generated secret loss must not activate") },
    )
    .await
    .unwrap_err();

    assert_eq!(error.code, "vault-secret-unavailable");
    assert!(store
        .get(&checkpoint)
        .unwrap()
        .is_some_and(|value| !value.is_empty()));
    assert!(server.lock().unwrap().grant_bodies.is_empty());
}

#[tokio::test]
async fn malformed_generated_pending_secret_fails_without_prompt_or_regeneration() {
    let store = MemoryStore::default();
    let hosted = HostedState::default();
    let provisioning = peppy_hosted_client::prepare(
        HTTPS_ORIGIN,
        ACCOUNT,
        RESUME_PHRASE,
        CLIENT_KIND,
        DEVICE_NAME,
    )
    .unwrap();
    let binding = super::binding_for(&provisioning);
    store
        .set(
            &super::checkpoint_key(HTTPS_ORIGIN, ACCOUNT),
            &provisioning.checkpoint().unwrap(),
        )
        .unwrap();
    store
        .set(&super::pending_secret_key(HTTPS_ORIGIN, ACCOUNT), &[0xff])
        .unwrap();
    store
        .set(
            &super::recovery_index_key(&binding),
            &serde_json::to_vec(&super::recovery_index(&provisioning, true)).unwrap(),
        )
        .unwrap();
    store
        .set(
            &super::session_key(HTTPS_ORIGIN),
            &encode_session(
                ACCOUNT,
                "pst_validtoken123456789012345678901234567",
                super::now_unix() + 3600,
            ),
        )
        .unwrap();
    let server = Arc::new(Mutex::new(FakeServerState::default()));
    let (base, _handle) = start_fake_server(server.clone()).await;
    hosted.base_override.lock().unwrap().replace(base);

    let error = super::provision_core(
        &store,
        &hosted,
        HTTPS_ORIGIN,
        || async { panic!("malformed generated material must not prompt") },
        |_, _| async { panic!("malformed generated material must not activate") },
    )
    .await
    .unwrap_err();

    assert_eq!(error.code, "vault-secret-unavailable");
    assert!(server.lock().unwrap().grant_bodies.is_empty());
}

#[tokio::test]
async fn legacy_checkpoint_cancel_keeps_its_identity_for_later_recovery() {
    let (store, hosted, server, _vault, _account) =
        resumable_fixture(FakeServerState::default(), false).await;
    let checkpoint = super::checkpoint_key(HTTPS_ORIGIN, ACCOUNT);

    super::provision_core(
        &store,
        &hosted,
        HTTPS_ORIGIN,
        || async move { Ok(None) },
        |_, _| async { panic!("cancelled recovery must not activate") },
    )
    .await
    .unwrap();

    assert!(store
        .get(&checkpoint)
        .unwrap()
        .is_some_and(|value| !value.is_empty()));
    assert!(server.lock().unwrap().grant_bodies.is_empty());
}

#[tokio::test]
async fn provision_expired_grant_retried_once() {
    let store = MemoryStore::default();
    let hosted = HostedState::default();

    let server_state = Arc::new(Mutex::new(FakeServerState {
        account_revoked: false,
        has_vault: false,
        access: "read_write".into(),
        grant_expired: true,
        grant_call_count: 0,
        ..FakeServerState::default()
    }));
    let (base, _handle) = start_fake_server(server_state.clone()).await;
    hosted.base_override.lock().unwrap().replace(base);
    let uri = HTTPS_ORIGIN.to_owned();

    let account_id = "550e8400-e29b-41d4-a716-446655440000";
    let session_key = super::session_key(&uri);
    store
        .set(
            &session_key,
            &encode_session(
                account_id,
                "pst_validtoken123456789012345678901234567",
                super::now_unix() + 3600,
            ),
        )
        .unwrap();

    let passphrase_fn = || async {
        Ok(Some(Zeroizing::new(
            "alpha bravo charlie delta echo foxtrot".into(),
        )))
    };

    let calls = Arc::new(Mutex::new(0));
    let calls_clone = calls.clone();
    let activate_fn = move |_cred: Zeroizing<Vec<u8>>, _pass: Zeroizing<String>| {
        let calls = calls_clone.clone();
        async move {
            *calls.lock().unwrap() += 1;
            Ok(())
        }
    };

    let result = super::provision_core(&store, &hosted, &uri, passphrase_fn, activate_fn).await;

    assert!(
        result.is_ok(),
        "Provision should succeed with grant retry: {:?}",
        result.err().map(|error| error.code)
    );
    assert_eq!(*calls.lock().unwrap(), 1, "Activate should be called once");
    let server = server_state.lock().unwrap();
    assert_eq!(server.grant_bodies.len(), 2, "exactly one grant retry");
    assert_eq!(server.complete_calls, 2);
    assert_eq!(
        server.grant_bodies[0]["operation_id"], server.grant_bodies[1]["operation_id"],
        "the retry keeps the same operation"
    );
    drop(server);
    assert!(
        store
            .get(&super::checkpoint_key(&uri, account_id))
            .unwrap()
            .is_some_and(|value| value.is_empty()),
        "the checkpoint is cleared only after success"
    );
}

#[tokio::test]
async fn failed_completion_keeps_checkpoint_and_resume_reuses_operation() {
    let store = MemoryStore::default();
    let hosted = HostedState::default();
    let server_state = Arc::new(Mutex::new(FakeServerState {
        complete_fails: true,
        ..FakeServerState::default()
    }));
    let (base, _handle) = start_fake_server(server_state.clone()).await;
    hosted.base_override.lock().unwrap().replace(base);
    let uri = HTTPS_ORIGIN.to_owned();
    let account_id = "550e8400-e29b-41d4-a716-446655440000";
    store
        .set(
            &super::session_key(&uri),
            &encode_session(
                account_id,
                "pst_validtoken123456789012345678901234567",
                super::now_unix() + 3600,
            ),
        )
        .unwrap();
    let first = super::provision_core(
        &store,
        &hosted,
        &uri,
        || async move { panic!("fresh Hosted provisioning must not prompt") },
        |_, _| async { panic!("activation must not run after a failed completion") },
    )
    .await;
    assert!(first.is_err());
    let checkpoint_key = super::checkpoint_key(&uri, account_id);
    assert!(
        store
            .get(&checkpoint_key)
            .unwrap()
            .is_some_and(|value| !value.is_empty()),
        "the checkpoint survives a failed completion"
    );
    let pending_key = super::pending_secret_key(&uri, account_id);
    assert!(store.get(&pending_key).unwrap().is_some_and(|value| {
        peppy_crypto::passphrase::passphrase_acceptable(std::str::from_utf8(&value).unwrap())
    }));
    assert!(
        super::account_view(&store, &hosted, &uri, Some("desktop-client-id"))
            .await
            .unwrap()
            .resumable
    );

    server_state.lock().unwrap().complete_fails = false;
    let activated = Arc::new(Mutex::new(0));
    let sink = activated.clone();
    super::provision_core(
        &store,
        &hosted,
        &uri,
        || async move { panic!("stored generated secret must resume without prompting") },
        move |_, _| async move {
            *sink.lock().unwrap() += 1;
            Ok(())
        },
    )
    .await
    .unwrap();
    assert_eq!(*activated.lock().unwrap(), 1);
    assert!(store
        .get(&pending_key)
        .unwrap()
        .is_some_and(|value| value.is_empty()));
    let server = server_state.lock().unwrap();
    let operations: Vec<_> = server
        .grant_bodies
        .iter()
        .map(|body| body["operation_id"].clone())
        .collect();
    assert!(
        operations.windows(2).all(|pair| pair[0] == pair[1]),
        "resume reuses the original operation: {operations:?}"
    );
}

const RESUME_PHRASE: &str = "alpha bravo charlie delta echo foxtrot";
const ACCOUNT: &str = "550e8400-e29b-41d4-a716-446655440000";

async fn resumable_fixture(
    server: FakeServerState,
    granted: bool,
) -> (
    MemoryStore,
    HostedState,
    Arc<Mutex<FakeServerState>>,
    String,
    String,
) {
    let store = MemoryStore::default();
    let hosted = HostedState::default();
    let pending = peppy_hosted_client::prepare(
        HTTPS_ORIGIN,
        ACCOUNT,
        RESUME_PHRASE,
        CLIENT_KIND,
        DEVICE_NAME,
    )
    .unwrap();
    if granted {
        pending
            .accept_grant(
                json!({"grant": format!("pgr_{}", "1".repeat(64)), "expires_in_seconds": 600})
                    .to_string(),
            )
            .unwrap();
    }
    let vault = pending.view().vault_id;
    store
        .set(
            &super::checkpoint_key(HTTPS_ORIGIN, ACCOUNT),
            &pending.checkpoint().unwrap(),
        )
        .unwrap();
    store
        .set(
            &super::session_key(HTTPS_ORIGIN),
            &encode_session(
                ACCOUNT,
                "pst_validtoken123456789012345678901234567",
                super::now_unix() + 3600,
            ),
        )
        .unwrap();
    let server = Arc::new(Mutex::new(server));
    let (base, _handle) = start_fake_server(server.clone()).await;
    hosted.base_override.lock().unwrap().replace(base);
    (store, hosted, server, vault, ACCOUNT.to_owned())
}

#[tokio::test]
async fn lost_completion_is_recovered_by_replaying_the_operation() {
    let (store, hosted, server, vault, account) =
        resumable_fixture(FakeServerState::default(), true).await;
    server.lock().unwrap().vault_id = Some(vault);
    // The account already owns the vault and lapsed billing must not block recovering it.
    server.lock().unwrap().access = "read_only".into();
    let activated = Arc::new(Mutex::new(0));
    let sink = activated.clone();
    super::provision_core(
        &store,
        &hosted,
        HTTPS_ORIGIN,
        || async move { Ok(Some(Zeroizing::new(RESUME_PHRASE.into()))) },
        move |_, _| async move {
            *sink.lock().unwrap() += 1;
            Ok(())
        },
    )
    .await
    .unwrap();
    assert_eq!(*activated.lock().unwrap(), 1);
    let server = server.lock().unwrap();
    assert!(
        server.grant_bodies.is_empty(),
        "replay reuses the saved grant"
    );
    assert_eq!(server.complete_calls, 1);
    assert!(store
        .get(&super::checkpoint_key(HTTPS_ORIGIN, &account))
        .unwrap()
        .is_some_and(|value| value.is_empty()));
}

#[tokio::test]
async fn foreign_vault_is_not_replayed() {
    let (store, hosted, server, _vault, _account) =
        resumable_fixture(FakeServerState::default(), true).await;
    server.lock().unwrap().vault_id = Some("77777777-2222-4333-8444-555555555555".into());
    let error = super::provision_core(
        &store,
        &hosted,
        HTTPS_ORIGIN,
        || async { panic!("no passphrase is needed for another vault") },
        |_, _| async { panic!("another vault must not be activated") },
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, "has-vault");
}

#[tokio::test]
async fn resume_with_an_expired_saved_grant_requests_a_new_one() {
    let (store, hosted, server, _vault, _account) = resumable_fixture(
        FakeServerState {
            grant_expired: true,
            ..FakeServerState::default()
        },
        true,
    )
    .await;
    super::provision_core(
        &store,
        &hosted,
        HTTPS_ORIGIN,
        || async { Ok(Some(Zeroizing::new(RESUME_PHRASE.into()))) },
        |_, _| async { Ok(()) },
    )
    .await
    .unwrap();
    let server = server.lock().unwrap();
    assert_eq!(
        server.grant_bodies.len(),
        1,
        "the expired saved grant is replaced once"
    );
    assert_eq!(server.complete_calls, 2);
}

#[tokio::test]
async fn expired_hosted_session_requires_sign_in_and_preserves_recovery_data() {
    let (store, hosted, server, _vault, _account) = resumable_fixture(
        FakeServerState {
            account_revoked: true,
            ..FakeServerState::default()
        },
        false,
    )
    .await;
    let expired_at = super::now_unix().saturating_sub(1);
    store
        .set(
            &super::session_key(HTTPS_ORIGIN),
            &encode_session(
                ACCOUNT,
                "pst_expiredtoken12345678901234567890123456",
                expired_at,
            ),
        )
        .unwrap();
    *hosted.session.lock().await = Some(HostedSessionRecord {
        account_id: ACCOUNT.into(),
        bearer: Zeroizing::new("pst_expiredtoken12345678901234567890123456".into()),
        expires_at_unix: expired_at,
    });
    let checkpoint = super::checkpoint_key(HTTPS_ORIGIN, ACCOUNT);
    let provisioning = peppy_hosted_client::restore(
        store.get(&checkpoint).unwrap().unwrap().to_vec(),
        HTTPS_ORIGIN,
        ACCOUNT,
    )
    .unwrap();
    let pending = super::pending_secret_key(HTTPS_ORIGIN, ACCOUNT);
    let index = super::recovery_index_key(&super::binding_for(&provisioning));
    store.set(&pending, RESUME_PHRASE.as_bytes()).unwrap();
    store
        .set(
            &index,
            &serde_json::to_vec(&super::recovery_index(&provisioning, false)).unwrap(),
        )
        .unwrap();
    let keys = [checkpoint.clone(), pending.clone(), index.clone()];
    let saved: Vec<Vec<u8>> = keys
        .iter()
        .map(|key| store.get(key).unwrap().unwrap().to_vec())
        .collect();

    let error = super::provision_core(
        &store,
        &hosted,
        HTTPS_ORIGIN,
        || async { panic!("an expired Hosted session must not prompt") },
        |_, _| async { panic!("an expired Hosted session must not activate") },
    )
    .await
    .unwrap_err();

    assert_eq!(error.code, "hosted-session-required");
    assert!(store
        .get(&super::session_key(HTTPS_ORIGIN))
        .unwrap()
        .is_some_and(|value| value.is_empty()));
    assert!(hosted.session.lock().await.is_none());
    for (key, bytes) in keys.iter().zip(&saved) {
        assert_eq!(
            store.get(key).unwrap().unwrap().as_slice(),
            bytes.as_slice()
        );
    }
    {
        let server = server.lock().unwrap();
        assert!(server.grant_bodies.is_empty());
        assert_eq!(server.complete_calls, 0);
    }

    server.lock().unwrap().account_revoked = false;
    store
        .set(
            &super::session_key(HTTPS_ORIGIN),
            &encode_session(
                ACCOUNT,
                "pst_validtoken123456789012345678901234567",
                super::now_unix() + 3600,
            ),
        )
        .unwrap();
    let activated = Arc::new(Mutex::new(0));
    let sink = activated.clone();
    super::provision_core(
        &store,
        &hosted,
        HTTPS_ORIGIN,
        || async { panic!("the saved pending secret must avoid a prompt") },
        move |_, secret| async move {
            assert_eq!(secret.as_str(), RESUME_PHRASE);
            *sink.lock().unwrap() += 1;
            Ok(())
        },
    )
    .await
    .unwrap();
    assert_eq!(*activated.lock().unwrap(), 1);
    assert!(store
        .get(&checkpoint)
        .unwrap()
        .is_some_and(|value| value.is_empty()));
    let server = server.lock().unwrap();
    assert_eq!(server.grant_bodies.len(), 1);
    assert_eq!(server.complete_calls, 1);
}

#[tokio::test]
async fn rejected_bearer_on_grant_ends_the_hosted_session() {
    let (store, hosted, _server, _vault, _account) = resumable_fixture(
        FakeServerState {
            grant_status: Some(401),
            ..FakeServerState::default()
        },
        false,
    )
    .await;
    let checkpoint = super::checkpoint_key(HTTPS_ORIGIN, ACCOUNT);
    let provisioning = peppy_hosted_client::restore(
        store.get(&checkpoint).unwrap().unwrap().to_vec(),
        HTTPS_ORIGIN,
        ACCOUNT,
    )
    .unwrap();
    let pending = super::pending_secret_key(HTTPS_ORIGIN, ACCOUNT);
    let index = super::recovery_index_key(&super::binding_for(&provisioning));
    store.set(&pending, RESUME_PHRASE.as_bytes()).unwrap();
    store
        .set(
            &index,
            &serde_json::to_vec(&super::recovery_index(&provisioning, false)).unwrap(),
        )
        .unwrap();
    let keys = [checkpoint.clone(), pending.clone(), index.clone()];
    let saved: Vec<Vec<u8>> = keys
        .iter()
        .map(|key| store.get(key).unwrap().unwrap().to_vec())
        .collect();
    let error = super::provision_core(
        &store,
        &hosted,
        HTTPS_ORIGIN,
        || async { Ok(Some(Zeroizing::new(RESUME_PHRASE.into()))) },
        |_, _| async { panic!("no activation without a grant") },
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, "hosted-session-required");
    assert!(store
        .get(&super::session_key(HTTPS_ORIGIN))
        .unwrap()
        .is_some_and(|value| value.is_empty()));
    for (key, bytes) in keys.iter().zip(&saved) {
        assert_eq!(
            store.get(key).unwrap().unwrap().as_slice(),
            bytes.as_slice()
        );
    }
}

#[tokio::test]
async fn rejected_bearer_while_loading_a_resumable_provisioning_keeps_recovery_data() {
    let (store, hosted, _server, _vault, _account) = resumable_fixture(
        FakeServerState {
            account_revoked: true,
            ..FakeServerState::default()
        },
        false,
    )
    .await;
    let checkpoint = super::checkpoint_key(HTTPS_ORIGIN, ACCOUNT);
    let provisioning = peppy_hosted_client::restore(
        store.get(&checkpoint).unwrap().unwrap().to_vec(),
        HTTPS_ORIGIN,
        ACCOUNT,
    )
    .unwrap();
    let pending = super::pending_secret_key(HTTPS_ORIGIN, ACCOUNT);
    let index = super::recovery_index_key(&super::binding_for(&provisioning));
    store.set(&pending, RESUME_PHRASE.as_bytes()).unwrap();
    store
        .set(
            &index,
            &serde_json::to_vec(&super::recovery_index(&provisioning, false)).unwrap(),
        )
        .unwrap();
    let keys = [checkpoint.clone(), pending.clone(), index.clone()];
    let saved: Vec<Vec<u8>> = keys
        .iter()
        .map(|key| store.get(key).unwrap().unwrap().to_vec())
        .collect();

    let error = super::provision_core(
        &store,
        &hosted,
        HTTPS_ORIGIN,
        || async { panic!("a rejected bearer must not prompt") },
        |_, _| async { panic!("a rejected bearer must not activate") },
    )
    .await
    .unwrap_err();

    assert_eq!(error.code, "hosted-session-required");
    assert!(store
        .get(&super::session_key(HTTPS_ORIGIN))
        .unwrap()
        .is_some_and(|value| value.is_empty()));
    for (key, bytes) in keys.iter().zip(&saved) {
        assert_eq!(
            store.get(key).unwrap().unwrap().as_slice(),
            bytes.as_slice()
        );
    }
}
