//! Regression tests for the DU security review (reports/DU-security-review.md).
use crate::{
    credentials::{parse_credential, tests::credential_json, tests::TOKEN},
    secure_store::{MemoryStore, SecretStore},
    session::Session,
    sync::{self, classify, next_backoff, Failure, MediaOutcome, STABLE_CONNECTION},
    tests::{fixture_at, gateway, input, noop, open, routed, PHRASE},
    AppState, PreparedPublicCopy,
};
use axum::{
    extract::{Path as AxumPath, Query},
    http::{HeaderMap, StatusCode},
    routing::{get, post},
    Json, Router,
};
use peppy_client_core::{Cursor, RawSnapshotRecord, SnapshotPurpose};
use serde_json::{json, Value};
use std::{
    collections::{HashMap, HashSet},
    sync::{
        atomic::{AtomicU16, AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};

const TOKEN2: &str = "fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210";

struct CountingStore {
    inner: MemoryStore,
    gets: AtomicUsize,
}

impl SecretStore for CountingStore {
    fn get(
        &self,
        account: &str,
    ) -> crate::error::BridgeResult<Option<zeroize::Zeroizing<Vec<u8>>>> {
        self.gets.fetch_add(1, Ordering::SeqCst);
        self.inner.get(account)
    }

    fn set(&self, account: &str, secret: &[u8]) -> crate::error::BridgeResult<()> {
        self.inner.set(account, secret)
    }
}

struct FailingStore;

impl SecretStore for FailingStore {
    fn get(
        &self,
        _account: &str,
    ) -> crate::error::BridgeResult<Option<zeroize::Zeroizing<Vec<u8>>>> {
        Err(crate::error::BridgeError::new(
            "secure-store-unavailable",
            "unavailable",
        ))
    }

    fn set(&self, _account: &str, _secret: &[u8]) -> crate::error::BridgeResult<()> {
        Err(crate::error::BridgeError::new(
            "secure-store-unavailable",
            "unavailable",
        ))
    }
}

#[tokio::test]
async fn cached_native_session_does_not_reread_the_store() {
    let root = tempfile::tempdir().unwrap();
    let store = Arc::new(CountingStore {
        inner: MemoryStore::default(),
        gets: AtomicUsize::new(0),
    });
    let vault = uuid::Uuid::new_v4().to_string();
    let device = uuid::Uuid::new_v4().to_string();
    let state = AppState::new(root.path().to_path_buf(), store.clone(), noop());
    crate::activate_import(
        &state,
        credential("http://127.0.0.1:9", &vault, &device, TOKEN),
    )
    .await
    .unwrap();
    let before = store.gets.load(Ordering::SeqCst);
    assert!(state.session().await.unwrap().is_some());
    assert_eq!(store.gets.load(Ordering::SeqCst), before);
}

#[tokio::test]
async fn snapshot_loading_propagates_secure_store_errors() {
    let root = tempfile::tempdir().unwrap();
    let binding = crate::credentials::Binding {
        origin: "https://example.test".into(),
        vault_id: uuid::Uuid::new_v4().to_string(),
        device_id: uuid::Uuid::new_v4().to_string(),
    };
    let state = AppState::new(root.path().to_path_buf(), Arc::new(FailingStore), noop());
    state
        .update_config(|config| {
            config.select_origin(&binding.origin);
            config.remember(&binding);
            config.active = Some(binding);
        })
        .unwrap();

    assert_eq!(
        crate::snapshot_session(&state)
            .await
            .err()
            .expect("expected store error")
            .code,
        "secure-store-unavailable"
    );
}

async fn serve(router: Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    format!("http://{address}")
}

// ---- 1. Per-window app-command ACL -------------------------------------------------------

fn permission_entries(json: &str) -> Vec<Value> {
    let value: Value = serde_json::from_str(json).unwrap();
    value["permissions"].as_array().unwrap().clone()
}

// A capability entry is either a bare identifier or a scoped object carrying `identifier`
// and `allow`. Anything else fails closed: a malformed entry is never dropped from the ACL.
fn permission_id(entry: &Value) -> String {
    match entry {
        Value::String(id) => id.clone(),
        Value::Object(scoped) => scoped
            .get("identifier")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("scoped permission lacks a string identifier: {entry}"))
            .to_owned(),
        other => panic!("unsupported permission entry: {other}"),
    }
}

fn permissions(json: &str) -> HashSet<String> {
    permission_entries(json).iter().map(permission_id).collect()
}

#[test]
fn permission_helper_keeps_string_and_scoped_identifiers() {
    let json = r#"{"permissions":["allow-load-state",{"identifier":"opener:allow-open-url","allow":[{"url":"https://peppy.pro/privacy"}]}]}"#;
    assert_eq!(
        permissions(json),
        HashSet::from([
            "allow-load-state".to_owned(),
            "opener:allow-open-url".to_owned()
        ])
    );
}

#[test]
#[should_panic(expected = "scoped permission lacks a string identifier")]
fn permission_helper_rejects_scoped_entry_without_identifier() {
    permissions(r#"{"permissions":[{"allow":[{"url":"https://peppy.pro/privacy"}]}]}"#);
}

#[test]
#[should_panic(expected = "scoped permission lacks a string identifier")]
fn permission_helper_rejects_non_string_identifier() {
    permissions(r#"{"permissions":[{"identifier":7,"allow":[]}]}"#);
}

#[test]
#[should_panic(expected = "unsupported permission entry")]
fn permission_helper_rejects_unsupported_entry_shapes() {
    permissions(r#"{"permissions":[42]}"#);
}

#[test]
fn main_window_has_exactly_the_privacy_opener() {
    let main_json = include_str!("../capabilities/default.json");
    let manifest: Value = serde_json::from_str(main_json).unwrap();
    assert_eq!(manifest["windows"], json!(["main"]));
    let openers: Vec<Value> = permission_entries(main_json)
        .into_iter()
        .filter(|entry| permission_id(entry).starts_with("opener:"))
        .collect();
    assert_eq!(
        openers,
        [json!({
            "identifier": "opener:allow-open-url",
            "allow": [{"url": "https://peppy.pro/privacy"}]
        })]
    );
    let composer = permissions(include_str!("../capabilities/composer.json"));
    assert!(
        composer.iter().all(|id| !id.starts_with("opener:")),
        "composer windows must not open URLs"
    );
}

fn declared_commands() -> Vec<String> {
    let build = include_str!("../build.rs");
    let list = &build[build.find("const COMMANDS").unwrap()..build.find("];").unwrap()];
    list.split('"')
        .skip(1)
        .step_by(2)
        .map(str::to_owned)
        .collect()
}

fn allow(command: &str) -> String {
    format!("allow-{}", command.replace('_', "-"))
}

#[test]
fn app_acl_restricts_composer_windows_to_conversation_operations() {
    let commands = declared_commands();
    let registered = include_str!("lib.rs");
    let hosted = include_str!("hosted/mod.rs");
    let enrollment = include_str!("enrollment.rs");
    let history_search = include_str!("search_history.rs");
    for command in &commands {
        assert!(
            registered.contains(&format!("enrollment::{command},"))
                || !command.starts_with("enrollment_"),
            "{command} is declared but not registered in generate_handler"
        );
        assert!(
            registered.contains(&format!("fn {command}("))
                || [hosted, enrollment, history_search].iter().any(|source| {
                    source.contains(&format!("pub async fn {command}("))
                        || source.contains(&format!("pub fn {command}("))
                }),
            "{command} declared in the app manifest but not defined"
        );
    }

    let composer = permissions(include_str!("../capabilities/composer.json"));
    let main = permissions(include_str!("../capabilities/default.json"));
    assert!(commands.contains(&"load_history".to_owned()));
    assert!(registered.contains("fn load_history("));
    let handler = &registered[registered.find("tauri::generate_handler![").unwrap()..];
    assert!(handler.contains("\n        load_history,"));
    assert!(crate::COMPOSER_COMMANDS.contains(&"load_history"));
    assert!(main.contains("allow-load-history"));
    assert!(composer.contains("allow-load-history"));
    assert!(!composer.contains("allow-load-message-window"));
    assert!(main.contains("allow-load-message-window"));
    assert!(!crate::COMPOSER_COMMANDS.contains(&"load_message_window"));
    for command in [
        "get_history_search",
        "start_history_search",
        "cancel_history_search",
    ] {
        assert!(commands.contains(&command.to_owned()));
        assert!(handler.contains(&format!("\n        search_history::{command},")));
        assert!(main.contains(&allow(command)));
        assert!(!composer.contains(&allow(command)));
    }
    for command in [
        "request_attachment_download",
        "media_page",
        "open_media",
        "load_media",
        "close_media",
    ] {
        assert!(commands.contains(&command.to_owned()));
        assert!(handler.contains(&format!("\n        {command},")));
        assert!(crate::COMPOSER_COMMANDS.contains(&command));
        assert!(main.contains(&allow(command)));
        assert!(composer.contains(&allow(command)));
    }
    for command in ["mark_all_read", "set_conversation_pinned"] {
        assert!(commands.contains(&command.to_owned()));
        assert!(handler.contains(&format!("\n        {command},")));
        assert!(main.contains(&allow(command)));
        assert!(!composer.contains(&allow(command)));
    }
    for command in [
        "enrollment_begin_vault_request",
        "enrollment_vault_request_status",
        "enrollment_create_vault",
        "enrollment_rotate_phone",
        "enrollment_cancel_phone",
        "reveal_vault_secret",
        "enrollment_start_over",
    ] {
        assert!(registered.contains(&format!("enrollment::{command},")));
        assert!(commands.contains(&command.to_owned()));
        assert!(main.contains(&allow(command)));
    }
    let app_permissions = |set: &HashSet<String>| {
        set.iter()
            .filter(|p| !p.contains(':'))
            .cloned()
            .collect::<HashSet<_>>()
    };
    let expected: HashSet<String> = crate::COMPOSER_COMMANDS
        .iter()
        .map(|command| allow(command))
        .chain(std::iter::once("allow-report-update-activity".into()))
        .collect();
    assert!(expected.contains("allow-close-head-panel"));
    assert_eq!(app_permissions(&composer), expected);
    for main_only in [
        "configure_server",
        "unlock_sync",
        "publish_attachment",
        "discard_draft",
        "open_composer",
        "set_start_at_login",
        "popout_conversation",
        "hide_head",
        "mark_all_read",
        "get_history_search",
        "start_history_search",
        "cancel_history_search",
        "set_conversation_pinned",
        "hosted_account",
        "hosted_sign_in",
        "hosted_sign_out",
        "hosted_open_billing",
        "hosted_provision",
        "check_for_updates",
        "restart_for_update",
        "set_automatic_updates",
        "defer_update",
    ] {
        assert!(
            !composer.contains(&allow(main_only)),
            "composer can call {main_only}"
        );
        assert!(main.contains(&allow(main_only)), "main lacks {main_only}");
    }
    for command in commands
        .iter()
        .filter(|c| **c != "close_composer" && **c != "close_head_panel")
    {
        assert!(
            main.contains(&allow(command)),
            "main window cannot call {command}"
        );
    }
    // Composer windows cannot minimize/maximize the main shell either.
    assert!(!composer.contains("core:window:allow-minimize"));
    assert!(!main.contains("allow-close-head-panel"));
}

#[test]
fn device_management_commands_are_registered_for_the_main_window_only() {
    let commands = declared_commands();
    let registered = include_str!("lib.rs");
    let main = permissions(include_str!("../capabilities/default.json"));
    let composer = permissions(include_str!("../capabilities/composer.json"));
    for command in [
        "list_devices",
        "revoke_device",
        "request_contact_sync",
        "get_contact_sync_request",
    ] {
        assert!(
            commands.contains(&command.to_owned()),
            "{command} is not declared"
        );
        assert!(
            registered.contains(&format!("\n        {command},")),
            "{command} is not registered"
        );
        assert!(main.contains(&allow(command)), "main lacks {command}");
        assert!(
            !composer.contains(&allow(command)),
            "composer can call {command}"
        );
    }
}

#[test]
fn runtime_window_checks_back_the_acl() {
    assert!(crate::require_main("main").is_ok());
    assert_eq!(
        crate::require_main("composer-any").unwrap_err().code,
        "window-context"
    );
}

#[test]
fn mark_all_read_requires_a_visible_focused_main_window() {
    assert!(crate::require_mark_all_read_window("main", true, false, true).is_ok());
    assert_eq!(
        crate::require_mark_all_read_window("composer-any", true, false, true)
            .unwrap_err()
            .code,
        "window-context"
    );
    for (visible, minimized, focused) in [
        (false, false, true),
        (true, true, true),
        (true, false, false),
    ] {
        assert_eq!(
            crate::require_mark_all_read_window("main", visible, minimized, focused)
                .unwrap_err()
                .code,
            "window-not-visible"
        );
    }
}

#[test]
fn external_url_validation_accepts_only_safe_http_destinations() {
    for value in [
        "https://example.com/path",
        "http://localhost:8080",
        "www.example.com",
        "https://[::1]/",
    ] {
        assert!(crate::validate_external_url(value).is_ok(), "{value}");
    }
    for value in [
        "javascript:alert(1)",
        "https://user@example.com",
        "https://@example.com",
        "https://example.com\n",
        "https://exa\u{200b}mple.com",
        "https://example\\.com",
        "https://example.com:",
        "https://0x7f.0.0.1",
        "https://example.com\u{e0001}",
    ] {
        assert_eq!(
            crate::validate_external_url(value).unwrap_err().code,
            "invalid-request",
            "{value}"
        );
    }
}

#[test]
fn webview_navigation_allows_trusted_origins_with_any_route() {
    let dev_url = url::Url::parse("http://localhost:1420/base").unwrap();
    for value in [
        "tauri://localhost/index.html?window=composer&conversationId=abc",
        "tauri://localhost/reload/path?cache=2#message",
        "http://tauri.localhost/index.html?window=composer&conversationId=abc",
        "http://tauri.localhost/reload/path?cache=2#message",
    ] {
        assert!(
            crate::allows_app_navigation(&url::Url::parse(value).unwrap(), Some(&dev_url)),
            "{value}"
        );
    }
    if cfg!(debug_assertions) {
        for value in [
            "http://localhost:1420/index.html?window=composer&conversationId=abc",
            "http://localhost:1420/reload/path?cache=2#message",
        ] {
            assert!(
                crate::allows_app_navigation(&url::Url::parse(value).unwrap(), Some(&dev_url)),
                "{value}"
            );
        }
        let secure_dev_url = url::Url::parse("https://dev.peppy.test:4443/root").unwrap();
        assert!(crate::allows_app_navigation(
            &url::Url::parse("https://dev.peppy.test:4443/index.html?window=composer").unwrap(),
            Some(&secure_dev_url),
        ));
    }
}

#[test]
fn webview_navigation_denies_untrusted_origins_and_credentials() {
    let dev_url = url::Url::parse("http://localhost:1420").unwrap();
    for value in [
        "https://example.com",
        "http://tauri.localhost.evil.test/index.html",
        "http://localhost:1421/index.html",
        "http://user@tauri.localhost/index.html",
        "http://user@localhost:1420/index.html",
    ] {
        assert!(
            !crate::allows_app_navigation(&url::Url::parse(value).unwrap(), Some(&dev_url)),
            "{value}"
        );
    }
}

#[test]
fn app_acl_cannot_create_windows_or_webviews() {
    for manifest in [
        include_str!("../capabilities/default.json"),
        include_str!("../capabilities/composer.json"),
    ] {
        let granted = permissions(manifest);
        for denied in [
            "core:window:allow-create",
            "core:webview:allow-create",
            "core:webview:allow-create-webview",
            "core:webview:allow-create-webview-window",
        ] {
            assert!(!granted.contains(denied), "unexpected capability: {denied}");
        }
    }
}

#[tokio::test]
async fn search_index_ready_latch_prevents_reentry_and_notifies_only_on_transition() {
    let fixture = fixture_at("http://127.0.0.1:9");
    let notifications = Arc::new(AtomicUsize::new(0));
    let notifier_count = notifications.clone();
    let session = Arc::new(
        crate::session::open_session(
            fixture.dir.path(),
            &fixture.store,
            &fixture.binding,
            &[],
            Arc::new(move || {
                notifier_count.fetch_add(1, Ordering::SeqCst);
            }),
        )
        .unwrap(),
    );

    assert!(crate::claim_search_index_driver(&session));
    let action = crate::observe_search_index_state(
        &session,
        None,
        peppy_client_core::SearchIndexState::Ready,
    );
    let gate = crate::update_safety::SafetyGate::default();
    assert!(!crate::complete_search_index_batch(&session, gate.enter().unwrap(), action).await);
    session.index_driver_running.store(false, Ordering::Release);
    assert_eq!(notifications.load(Ordering::SeqCst), 0);
    assert!(!crate::claim_search_index_driver(&session));
    assert!(!crate::claim_search_index_driver(&session));

    let fixture = fixture_at("http://127.0.0.1:9");
    let notifications = Arc::new(AtomicUsize::new(0));
    let notifier_count = notifications.clone();
    let session = Arc::new(
        crate::session::open_session(
            fixture.dir.path(),
            &fixture.store,
            &fixture.binding,
            &[],
            Arc::new(move || {
                notifier_count.fetch_add(1, Ordering::SeqCst);
            }),
        )
        .unwrap(),
    );
    assert_eq!(
        crate::observe_search_index_state(
            &session,
            None,
            peppy_client_core::SearchIndexState::Indexing,
        ),
        crate::SearchIndexDriverAction::Wait
    );
    // The transition state belongs to the session, so a transient worker restart cannot lose it.
    session.index_driver_running.store(false, Ordering::Release);
    assert!(crate::claim_search_index_driver(&session));
    let action = crate::observe_search_index_state(
        &session,
        None,
        peppy_client_core::SearchIndexState::Ready,
    );
    assert!(!crate::complete_search_index_batch(&session, gate.enter().unwrap(), action).await);
    assert_eq!(notifications.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn search_index_first_batch_ready_notifies_only_after_initial_indexing() {
    let fixture = fixture_at("http://127.0.0.1:9");
    let notifications = Arc::new(AtomicUsize::new(0));
    let notifier_count = notifications.clone();
    let session = Arc::new(
        crate::session::open_session(
            fixture.dir.path(),
            &fixture.store,
            &fixture.binding,
            &[],
            Arc::new(move || {
                notifier_count.fetch_add(1, Ordering::SeqCst);
            }),
        )
        .unwrap(),
    );
    let gate = crate::update_safety::SafetyGate::default();

    let action = crate::observe_search_index_state(
        &session,
        Some(peppy_client_core::SearchIndexState::Indexing),
        peppy_client_core::SearchIndexState::Ready,
    );
    assert!(!crate::complete_search_index_batch(&session, gate.enter().unwrap(), action).await);
    assert_eq!(notifications.load(Ordering::SeqCst), 1);

    let action = crate::observe_search_index_state(
        &session,
        None,
        peppy_client_core::SearchIndexState::Ready,
    );
    assert!(!crate::complete_search_index_batch(&session, gate.enter().unwrap(), action).await);
    assert_eq!(notifications.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn search_index_wait_releases_update_admission_and_honors_cancellation() {
    let fixture = fixture_at("http://127.0.0.1:9");
    let session = Arc::new(open(&fixture, &fixture.binding, &[]));
    let gate = crate::update_safety::SafetyGate::default();
    let operation = gate.enter().unwrap();
    let waiting_session = session.clone();
    let waiter = tokio::spawn(async move {
        crate::complete_search_index_batch(
            &waiting_session,
            operation,
            crate::SearchIndexDriverAction::Wait,
        )
        .await
    });

    tokio::task::yield_now().await;
    let restart = gate
        .reserve_now()
        .expect("the completed batch must release update admission before its wait");
    drop(restart);
    session.cancel.cancel();
    assert!(
        !waiter.await.unwrap(),
        "cancellation must prevent the next batch"
    );
}

// ---- 2. Serialized import, token rotation and session cancellation -----------------------

fn credential(
    origin: &str,
    vault: &str,
    device: &str,
    token: &str,
) -> Arc<crate::credentials::ImportedCredential> {
    Arc::new(parse_credential(&credential_json(origin, vault, device, token)).unwrap())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn token_rotation_replaces_and_cancels_the_session_and_keeps_the_key() {
    let root = tempfile::tempdir().unwrap();
    let store = Arc::new(MemoryStore::default());
    let state = AppState::new(root.path().to_path_buf(), store.clone(), noop());
    let (vault, device) = (
        uuid::Uuid::new_v4().to_string(),
        uuid::Uuid::new_v4().to_string(),
    );
    let origin = "http://127.0.0.1:9";
    // Two concurrent first imports of the same device serialize: one key, one session.
    let (a, b) = tokio::join!(
        crate::activate_import(&state, credential(origin, &vault, &device, TOKEN)),
        crate::activate_import(&state, credential(origin, &vault, &device, TOKEN)),
    );
    a.unwrap();
    b.unwrap();
    let first = state.session().await.unwrap().unwrap();
    let key = store
        .get(&first.binding.db_key_account())
        .unwrap()
        .unwrap()
        .to_vec();
    first
        .client
        .save_compose_draft(
            first.client.create_compose_draft(None).unwrap().draft_id,
            0,
            Default::default(),
        )
        .unwrap();

    let head_generation = state.head_generation.lock().unwrap().current();
    crate::activate_import(&state, credential(origin, &vault, &device, TOKEN2))
        .await
        .unwrap();
    assert_eq!(
        state.head_generation.lock().unwrap().current(),
        head_generation,
        "same-binding restart preserves head topology generation"
    );
    let second = state.session().await.unwrap().unwrap();
    assert!(
        !Arc::ptr_eq(&first, &second),
        "a rotated token gets a fresh session"
    );
    assert!(
        first.cancel.is_cancelled(),
        "the old session's networking is cancelled"
    );
    assert_eq!(second.api.bearer(), TOKEN2);
    assert_eq!(
        store
            .get(&second.binding.db_key_account())
            .unwrap()
            .unwrap()
            .to_vec(),
        key,
        "database key preserved"
    );
    assert_eq!(
        second.client.compose_drafts().unwrap().len(),
        1,
        "database still readable"
    );

    // Switching origin deactivates (and cancels) the session without deleting anything.
    state
        .update_config(|config| config.select_origin("https://other.test"))
        .unwrap();
    assert!(state.session().await.unwrap().is_none());
    assert!(second.cancel.is_cancelled());
    state
        .update_config(|config| config.select_origin(origin))
        .unwrap();
    let restored = state.session().await.unwrap().unwrap();
    assert_eq!(restored.binding, second.binding);
    assert_eq!(restored.client.compose_drafts().unwrap().len(), 1);
    // A credential for another origin is refused while this server is configured.
    let foreign = crate::activate_import(
        &state,
        credential(
            "https://elsewhere.test",
            &vault,
            &uuid::Uuid::new_v4().to_string(),
            TOKEN,
        ),
    )
    .await;
    assert_eq!(foreign.unwrap_err().code, "origin-binding");
    // The full import path refuses before any network request (otherwise this would be `offline`).
    let network_path = crate::verify_and_activate_test_credential(
        &state,
        credential(
            "https://elsewhere.invalid",
            &vault,
            &uuid::Uuid::new_v4().to_string(),
            TOKEN,
        ),
    )
    .await;
    assert_eq!(network_path.unwrap_err().code, "origin-binding");
    state.close_session().await;
    assert!(restored.cancel.is_cancelled());
}

// ---- 3. Impossible snapshot cuts are abandoned -------------------------------------------

async fn snapshot_mock(stale_reply: &'static str) -> (String, Arc<AtomicUsize>) {
    let stale_hits = Arc::new(AtomicUsize::new(0));
    let hits = stale_hits.clone();
    let router = Router::new()
        .route("/v1/snapshot", get(|| async { Json(json!({"snapshot_version":1,"high_water_cursor":"2","record_count":"2","max_page_size":200})) }))
        .route(
            "/v1/snapshot/records",
            get(move |Query(query): Query<HashMap<String, String>>| {
                let hits = hits.clone();
                async move {
                    if query.get("high_water").map(String::as_str) == Some("5") {
                        hits.fetch_add(1, Ordering::SeqCst);
                        return if stale_reply == "409" {
                            (StatusCode::CONFLICT, Json(json!({"code":"resync_required","reason":"cursor_ahead"})))
                        } else {
                            (StatusCode::OK, Json(json!({"high_water_cursor":"5","records":[],"next_after":null})))
                        };
                    }
                    let after: u64 = query.get("after").and_then(|a| a.parse().ok()).unwrap_or(0);
                    let records: Vec<Value> = (1..=2u64).filter(|c| *c > after).map(|c| json!({"cursor":c.to_string(),"envelope":{"malformed":c}})).collect();
                    (StatusCode::OK, Json(json!({"high_water_cursor":"2","records":records,"next_after":null})))
                }
            }),
        );
    (serve(router).await, stale_hits)
}

async fn stale_cut_is_restarted(stale_reply: &'static str) {
    let (origin, stale_hits) = snapshot_mock(stale_reply).await;
    let f = fixture_at(&origin);
    let session = Arc::new(open(&f, &f.binding, &[]));
    session
        .client
        .unlock(&f.profile, &f.header, PHRASE)
        .unwrap();
    // A persisted, partially staged cut (H=5) that the server can no longer serve.
    let stale = session
        .client
        .begin_snapshot(Cursor(5), 5, SnapshotPurpose::Resync)
        .unwrap();
    let raw = |c: u64| RawSnapshotRecord {
        cursor: Cursor(c),
        envelope_json: format!("{{\"malformed\":{c}}}").into_bytes(),
    };
    session
        .client
        .append_snapshot_raw_page(stale.generation, &[raw(1), raw(2)])
        .unwrap();

    sync::snapshot_resync(&session).await.unwrap();
    assert_eq!(
        stale_hits.load(Ordering::SeqCst),
        1,
        "the impossible cut is tried once, then abandoned"
    );
    assert_eq!(
        *session.abandoned_snapshot.lock().unwrap(),
        Some(stale.generation)
    );
    assert_eq!(session.client.receive_cursor().unwrap(), Cursor(2));
    assert_eq!(
        session.client.quarantined().unwrap().len(),
        2,
        "raw malformed records reach core quarantine"
    );
    assert!(!session.client.restore_guarded().unwrap());
    // A later resync never resumes the abandoned generation.
    sync::snapshot_resync(&session).await.unwrap();
    assert_eq!(stale_hits.load(Ordering::SeqCst), 1);
    session.cancel.cancel();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn snapshot_cut_rejected_by_server_is_abandoned_and_restarted() {
    stale_cut_is_restarted("409").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn snapshot_cut_that_ends_early_is_abandoned_and_restarted() {
    stale_cut_is_restarted("empty").await;
}

// ---- 4. Media retry policy ----------------------------------------------------------------

#[test]
fn media_failures_are_classified() {
    use crate::net::NetError;
    let status = |status| Failure::Net(NetError::Status { status, code: None });
    assert_eq!(
        classify(&Failure::Net(NetError::Offline)),
        MediaOutcome::Fatal
    );
    assert_eq!(
        classify(&Failure::Net(NetError::Revoked)),
        MediaOutcome::Fatal
    );
    for transient in [408, 429, 500, 502, 503, 504] {
        assert_eq!(
            classify(&status(transient)),
            MediaOutcome::Retry,
            "{transient}"
        );
    }
    for permanent in [400, 403, 404, 413, 415, 422] {
        assert_eq!(
            classify(&status(permanent)),
            MediaOutcome::Permanent,
            "{permanent}"
        );
    }
    assert_eq!(
        classify(&Failure::Net(NetError::Invalid)),
        MediaOutcome::Permanent
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn transient_media_errors_are_retried_and_not_cached() {
    let reply = Arc::new(AtomicU16::new(503));
    let reserves = Arc::new(AtomicUsize::new(0));
    let (r, n) = (reply.clone(), reserves.clone());
    let router = Router::new().route(
        "/v1/attachments/reserve",
        post(move || {
            let (r, n) = (r.clone(), n.clone());
            async move {
                n.fetch_add(1, Ordering::SeqCst);
                (
                    StatusCode::from_u16(r.load(Ordering::SeqCst)).unwrap(),
                    Json(json!({"code":"busy"})),
                )
            }
        }),
    );
    let origin = serve(router).await;
    let f = fixture_at(&origin);
    let session = Arc::new(open(&f, &f.binding, &[]));
    session
        .client
        .unlock(&f.profile, &f.header, PHRASE)
        .unwrap();
    let gateway_id = uuid::Uuid::new_v4().to_string();
    session.set_status(|s| {
        s.gateways = vec![gateway(&gateway_id, true, true)];
        s.gateways_known = true;
    });
    let png = f.dir.path().join("p.png");
    std::fs::write(&png, crate::media::tests::sample_png(8, 8)).unwrap();
    let picked = session.prepare_attachment(&png).unwrap();
    let mut draft = input("draft-new", "", "pic", &["+15555550100"], "0");
    draft.attachment_ids = vec![picked.id.clone()];
    let draft = session.save_draft(&draft).unwrap();
    assert!(
        session
            .send_draft(&routed(
                input(&draft.id, "", "", &[], &draft.revision),
                &gateway_id,
                "sim-1"
            ))
            .unwrap()
            .accepted
    );

    for status in [503, 429, 502] {
        reply.store(status, Ordering::SeqCst);
        sync::work_round(&session).await.unwrap();
        assert!(
            session.transfer_errors.lock().unwrap().is_empty(),
            "{status} must not be cached as permanent"
        );
        assert_eq!(
            session.status.lock().unwrap().work_error,
            Some("media-retry")
        );
        assert_eq!(session.client.pending_uploads().unwrap().len(), 1);
    }
    assert_eq!(reserves.load(Ordering::SeqCst), 3, "retried every round");
    reply.store(400, Ordering::SeqCst);
    sync::work_round(&session).await.unwrap();
    assert_eq!(
        session.transfer_errors.lock().unwrap().len(),
        1,
        "deterministic rejection is shown"
    );
    sync::work_round(&session).await.unwrap();
    assert_eq!(
        reserves.load(Ordering::SeqCst),
        4,
        "permanent failures are not hammered"
    );
    session.cancel.cancel();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reserve_conflict_recovers_only_from_authenticated_duplicate_finalize_proof() {
    let finalized = Arc::new(AtomicUsize::new(0));
    let finalized_count = finalized.clone();
    let router = Router::new()
        .route(
            "/v1/attachments/reserve",
            post(|headers: HeaderMap| async move {
                assert_eq!(
                    headers
                        .get("authorization")
                        .and_then(|value| value.to_str().ok()),
                    Some(format!("Bearer {TOKEN}").as_str())
                );
                (
                    StatusCode::CONFLICT,
                    Json(json!({"code":"attachment_reservation_conflict"})),
                )
            }),
        )
        .route(
            "/v1/attachments/{id}/finalize",
            post(move |AxumPath(id): AxumPath<String>, headers: HeaderMap| {
                let finalized_count = finalized_count.clone();
                async move {
                    assert_eq!(
                        headers
                            .get("authorization")
                            .and_then(|value| value.to_str().ok()),
                        Some(format!("Bearer {TOKEN}").as_str())
                    );
                    finalized_count.fetch_add(1, Ordering::SeqCst);
                    (
                        StatusCode::OK,
                        Json(json!({"attachment_id":id,"duplicate":true})),
                    )
                }
            }),
        );
    let origin = serve(router).await;
    let f = fixture_at(&origin);
    let session = Arc::new(open(&f, &f.binding, &[]));
    session
        .client
        .unlock(&f.profile, &f.header, PHRASE)
        .unwrap();
    let file = f.dir.path().join("expired-finalized.png");
    std::fs::write(&file, crate::media::tests::sample_png(8, 8)).unwrap();
    let attachment = session.prepare_attachment(&file).unwrap();
    let gateway_id = uuid::Uuid::new_v4().to_string();
    session.set_status(|status| {
        status.gateways = vec![gateway(&gateway_id, true, true)];
        status.gateways_known = true;
    });
    let mut draft = input("draft-new", "", "upload", &["+15555550100"], "0");
    draft.attachment_ids = vec![attachment.id.clone()];
    let draft = session.save_draft(&draft).unwrap();
    session
        .send_draft(&routed(
            input(&draft.id, "", "", &[], &draft.revision),
            &gateway_id,
            "sim-1",
        ))
        .unwrap();
    let object = session.client.pending_uploads().unwrap().remove(0);

    assert!(sync::upload_one(&session, object).await.is_ok());

    assert_eq!(finalized.load(Ordering::SeqCst), 1);
    assert!(session.client.pending_uploads().unwrap().is_empty());
    assert!(session
        .client
        .attachment_info(attachment.id.parse().unwrap())
        .unwrap()
        .state
        .is_local());
    session.cancel.cancel();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reserve_conflict_does_not_mark_upload_without_duplicate_finalize_proof() {
    let router = Router::new()
        .route(
            "/v1/attachments/reserve",
            post(|| async {
                (
                    StatusCode::CONFLICT,
                    Json(json!({"code":"attachment_reservation_conflict"})),
                )
            }),
        )
        .route(
            "/v1/attachments/{id}/finalize",
            post(|AxumPath(id): AxumPath<String>| async move {
                (
                    StatusCode::OK,
                    Json(json!({"attachment_id":id,"duplicate":false})),
                )
            }),
        );
    let origin = serve(router).await;
    let f = fixture_at(&origin);
    let session = Arc::new(open(&f, &f.binding, &[]));
    session
        .client
        .unlock(&f.profile, &f.header, PHRASE)
        .unwrap();
    let file = f.dir.path().join("not-duplicate.png");
    std::fs::write(&file, crate::media::tests::sample_png(8, 8)).unwrap();
    let attachment = session.prepare_attachment(&file).unwrap();
    let gateway_id = uuid::Uuid::new_v4().to_string();
    session.set_status(|status| {
        status.gateways = vec![gateway(&gateway_id, true, true)];
        status.gateways_known = true;
    });
    let mut draft = input("draft-new", "", "upload", &["+15555550100"], "0");
    draft.attachment_ids = vec![attachment.id];
    let draft = session.save_draft(&draft).unwrap();
    session
        .send_draft(&routed(
            input(&draft.id, "", "", &[], &draft.revision),
            &gateway_id,
            "sim-1",
        ))
        .unwrap();
    let object = session.client.pending_uploads().unwrap().remove(0);

    assert!(sync::upload_one(&session, object).await.is_err());
    assert_eq!(session.client.pending_uploads().unwrap().len(), 1);
    session.cancel.cancel();
}

// ---- 5. Wake separation and stable backoff ------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn work_requests_wake_only_the_worker() {
    let f = fixture_at("http://127.0.0.1:9");
    let session: Arc<Session> = Arc::new(open(&f, &f.binding, &[]));
    let (worker_session, live_session) = (session.clone(), session.clone());
    let live =
        tokio::spawn(async move { sync::pause_live(&live_session, Duration::from_secs(30)).await });
    let worker =
        tokio::spawn(
            async move { sync::pause_worker(&worker_session, Duration::from_secs(30)).await },
        );
    tokio::time::sleep(Duration::from_millis(100)).await;
    session.request_work();
    assert!(tokio::time::timeout(Duration::from_secs(2), worker)
        .await
        .expect("worker woke")
        .unwrap());
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        !live.is_finished(),
        "a reconnect wait must not consume the worker's wake-up"
    );
    session.reconnect_wake.notify_one();
    assert!(tokio::time::timeout(Duration::from_secs(2), live)
        .await
        .unwrap()
        .unwrap());
    // Cancellation ends both waits with `false`.
    let waiter = session.clone();
    let cancelled =
        tokio::spawn(async move { sync::pause_worker(&waiter, Duration::from_secs(30)).await });
    session.cancel.cancel();
    assert!(!tokio::time::timeout(Duration::from_secs(2), cancelled)
        .await
        .unwrap()
        .unwrap());
}

#[test]
fn reconnect_backoff_resets_only_after_a_stable_connection() {
    let mut backoff = Duration::from_secs(1);
    for _ in 0..10 {
        backoff = next_backoff(backoff, false);
    }
    assert_eq!(
        backoff,
        Duration::from_secs(60),
        "flapping connections back off to the cap"
    );
    assert_eq!(next_backoff(backoff, false), Duration::from_secs(60));
    assert_eq!(next_backoff(backoff, true), Duration::from_secs(1));
    assert!(STABLE_CONNECTION >= Duration::from_secs(30));
}

// ---- 6. Public-copy confirmation names the image ------------------------------------------

#[test]
fn public_copy_confirmation_names_the_exposed_image() {
    let prepared = PreparedPublicCopy {
        bytes: vec![0; 3000],
        name: "holidayphoto.jpg".into(),
        width: 640,
        height: 480,
    };
    let prompt = crate::public_copy_prompt("holiday photo\u{0007}.jpg", &prepared);
    assert!(prompt.contains("\"holiday photo.jpg\""), "{prompt}");
    assert!(
        prompt.contains("holidayphoto.jpg")
            && prompt.contains("640×480")
            && prompt.contains("3 KiB")
    );
    assert!(prompt.contains("SEPARATE, server-readable"));
    assert!(!prompt.contains('/'), "no local path in the prompt");
}

// ---- 7. Failed sends never strand the UI on a stale revision ------------------------------

#[test]
fn refused_sends_keep_the_revision_and_results_report_it() {
    let f = fixture_at("http://127.0.0.1:9");
    let session = open(&f, &f.binding, &[]);
    session
        .client
        .unlock(&f.profile, &f.header, PHRASE)
        .unwrap();
    let gateway_id = uuid::Uuid::new_v4().to_string();
    session.set_status(|s| {
        s.gateways = vec![gateway(&gateway_id, true, false)];
        s.gateways_known = true;
    });
    let draft = session
        .save_draft(&input(
            "draft-new",
            "",
            "hi",
            &["+15555550100", "+15555550101"],
            "0",
        ))
        .unwrap();
    let id = draft.id.parse().unwrap();
    // Two recipients require capability-v2 MMS: refusal preserves the draft revision.
    let error = session
        .send_draft(&routed(
            input(&draft.id, "", "", &[], &draft.revision),
            &gateway_id,
            "sim-1",
        ))
        .unwrap_err();
    assert_eq!(error.code, "mms-unsupported");
    assert_eq!(
        session
            .client
            .compose_draft(id)
            .unwrap()
            .unwrap()
            .revision
            .to_string(),
        draft.revision
    );
    // A stale send reports the current revision so the UI can continue.
    let stale = session
        .send_draft(&routed(
            input(&draft.id, "", "", &[], "1"),
            &gateway_id,
            "sim-1",
        ))
        .unwrap_err();
    assert_eq!(
        (stale.code, stale.current_revision.as_deref()),
        ("stale-draft", Some(draft.revision.as_str()))
    );
    assert!(serde_json::to_string(&stale)
        .unwrap()
        .contains("\"currentRevision\""));
    // A successful send (route persisted + atomic send) reports the stored revision.
    let fixed = session
        .save_draft(&input(
            &draft.id,
            "",
            "hi",
            &["+15555550100"],
            &draft.revision,
        ))
        .unwrap();
    let sent = session
        .send_draft(&routed(
            input(&fixed.id, "", "", &[], &fixed.revision),
            &gateway_id,
            "sim-1",
        ))
        .unwrap();
    assert_eq!(
        sent.revision,
        Some(
            session
                .client
                .compose_draft(id)
                .unwrap()
                .unwrap()
                .revision
                .to_string()
        )
    );
}

// ---- 8. A composer cannot send another conversation's draft --------------------------------

#[test]
fn send_rejects_a_draft_from_another_conversation_without_mutation() {
    let f = fixture_at("http://127.0.0.1:9");
    let session = open(&f, &f.binding, &[]);
    session
        .client
        .unlock(&f.profile, &f.header, PHRASE)
        .unwrap();
    let gateway_id = uuid::Uuid::new_v4().to_string();
    session.set_status(|s| {
        s.gateways = vec![gateway(&gateway_id, true, false)];
        s.gateways_known = true;
    });
    let victim = session
        .save_draft(&input("draft-new", "", "secret", &["+15555550100"], "0"))
        .unwrap();
    let own = session
        .save_draft(&input("draft-new", "", "mine", &["+15555550101"], "0"))
        .unwrap();
    assert_ne!(victim.conversation_id, own.conversation_id);
    // A request that names `own` but carries the other draft must not mutate it.
    let forged = routed(
        input(&victim.id, &own.conversation_id, "", &[], &victim.revision),
        &gateway_id,
        "sim-1",
    );
    assert_eq!(
        session.send_draft(&forged).unwrap_err().code,
        "invalid-draft"
    );
    let stored = session
        .client
        .compose_draft(victim.id.parse().unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(
        stored.revision.to_string(),
        victim.revision,
        "no revision bump"
    );
    assert_eq!(stored.text, "secret");
    assert!(stored.route.is_none(), "route not persisted");
    assert!(
        session.client.pending_outbox_batch(10).unwrap().is_empty(),
        "nothing queued"
    );
    // The legitimate send for the matching conversation still works.
    let ok = routed(
        input(&own.id, &own.conversation_id, "", &[], &own.revision),
        &gateway_id,
        "sim-1",
    );
    assert!(session.send_draft(&ok).unwrap().accepted);
}
