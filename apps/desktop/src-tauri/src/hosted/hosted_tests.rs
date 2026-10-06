//! Comprehensive tests for hosted account and provisioning seams.

use super::*;
use crate::secure_store::MemoryStore;
use axum::{
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde_json::json;
use std::sync::{Arc, Mutex};
use uuid::Uuid;

// ============================================================================
// Fake Server and Test Fixtures
// ============================================================================

/// Credential origin for provisioning tests; requests go to the fake server via `base_override`.
const HTTPS_ORIGIN: &str = "https://peppy.test";

struct FakeServerState {
    account_revoked: bool,
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

impl Default for FakeServerState {
    fn default() -> Self {
        Self {
            account_revoked: false,
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

async fn start_fake_server(
    server_state: Arc<Mutex<FakeServerState>>,
) -> (String, tokio::task::JoinHandle<()>) {
    let router = Router::new()
        .route(
            "/hosted/v1/account",
            get(fake_account_handler).with_state(server_state.clone()),
        )
        .route(
            "/hosted/v1/provisioning",
            post(fake_provisioning_handler).with_state(server_state.clone()),
        )
        .route(
            "/hosted/v1/provisioning/complete",
            post(fake_complete_handler).with_state(server_state.clone()),
        );

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

    let view = super::account_view(&store, &hosted, &uri).await.unwrap();

    assert!(!view.signed_in, "Should be signed out after 401");
    assert!(view.available, "Should still be available");
    let tombstoned = store.get(&session_key).unwrap();
    assert!(
        tombstoned.is_some_and(|v| v.is_empty()),
        "Session should be tombstoned"
    );
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

    let view = super::account_view(&store, &hosted, &uri).await.unwrap();

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

    let view = super::account_view(&store, &hosted, &uri).await.unwrap();

    assert!(
        view.resumable,
        "Should show resumable when checkpoint exists"
    );
}

// ============================================================================
// Provision Core Tests
// ============================================================================

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

    let passphrase_fn = |_need: super::PassphraseNeed| async {
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

    let passphrase_fn = |_need: super::PassphraseNeed| async {
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
async fn provision_passphrase_none_returns_ok_no_checkpoint() {
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

    let passphrase_fn = |_need: super::PassphraseNeed| async { Ok(None) };
    let activate_fn = |_cred: Zeroizing<Vec<u8>>, _pass: Zeroizing<String>| async { Ok(()) };

    let result = super::provision_core(&store, &hosted, &uri, passphrase_fn, activate_fn).await;

    assert!(
        result.is_ok(),
        "Provision should succeed when passphrase is None"
    );
    let checkpoint_key = super::checkpoint_key(&uri, account_id);
    assert!(
        store.get(&checkpoint_key).unwrap().is_none(),
        "No checkpoint should be created"
    );
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
    let prov = peppy_hosted_client::prepare(origin, account_id, correct_phrase).unwrap();
    let checkpoint_bytes = prov.checkpoint().unwrap();
    let checkpoint_key = super::checkpoint_key(origin, account_id);
    store.set(&checkpoint_key, &checkpoint_bytes).unwrap();

    let passphrase_fn = |need: super::PassphraseNeed| async move {
        match need {
            super::PassphraseNeed::Resume => Ok(Some(Zeroizing::new("wrong passphrase".into()))),
            super::PassphraseNeed::Create => panic!("Should not ask for Create on resume"),
        }
    };
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

    let passphrase_fn = |_need: super::PassphraseNeed| async {
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
    let phrase = "alpha bravo charlie delta echo foxtrot";
    let first = super::provision_core(
        &store,
        &hosted,
        &uri,
        |need| async move {
            assert!(matches!(need, super::PassphraseNeed::Create));
            Ok(Some(Zeroizing::new(phrase.into())))
        },
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
    assert!(
        super::account_view(&store, &hosted, &uri)
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
        |need| async move {
            assert!(matches!(need, super::PassphraseNeed::Resume));
            Ok(Some(Zeroizing::new(phrase.into())))
        },
        move |_, _| async move {
            *sink.lock().unwrap() += 1;
            Ok(())
        },
    )
    .await
    .unwrap();
    assert_eq!(*activated.lock().unwrap(), 1);
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
    let pending = peppy_hosted_client::prepare(HTTPS_ORIGIN, ACCOUNT, RESUME_PHRASE).unwrap();
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
        |need| async move {
            assert!(matches!(need, super::PassphraseNeed::Resume));
            Ok(Some(Zeroizing::new(RESUME_PHRASE.into())))
        },
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
        |_| async { panic!("no passphrase is needed for another vault") },
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
        |_| async { Ok(Some(Zeroizing::new(RESUME_PHRASE.into()))) },
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
async fn rejected_bearer_on_grant_ends_the_hosted_session() {
    let (store, hosted, _server, _vault, _account) = resumable_fixture(
        FakeServerState {
            grant_status: Some(401),
            ..FakeServerState::default()
        },
        false,
    )
    .await;
    let error = super::provision_core(
        &store,
        &hosted,
        HTTPS_ORIGIN,
        |_| async { Ok(Some(Zeroizing::new(RESUME_PHRASE.into()))) },
        |_, _| async { panic!("no activation without a grant") },
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, "credentials-required");
    assert!(store
        .get(&super::session_key(HTTPS_ORIGIN))
        .unwrap()
        .is_some_and(|value| value.is_empty()));
}
