use std::{net::SocketAddr, time::Duration};

use axum::{
    Router,
    body::{Body, to_bytes},
    extract::ConnectInfo,
    http::{Method, Request, StatusCode},
};
use base64::{
    Engine as _,
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
};
use peppy_crypto::KeyProfile;
use peppy_server::{ServerBuilder, config::Config};
use serde_json::{Value, json};
use sqlx::{PgPool, postgres::PgPoolOptions};
use tower::ServiceExt;
use url::Url;
use uuid::Uuid;

const ORIGIN: &str = "https://admission-security.test";

struct Fixture {
    admin: PgPool,
    pool: PgPool,
    pool_url: String,
    schema: String,
}

impl Fixture {
    async fn new() -> Self {
        let database_url = std::env::var("TEST_DATABASE_URL")
            .expect("TEST_DATABASE_URL must identify a disposable PostgreSQL fixture");
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .unwrap();
        let schema = format!("peppy_admission_security_{}", Uuid::new_v4().simple());
        sqlx::query(&format!("CREATE SCHEMA {schema}"))
            .execute(&admin)
            .await
            .unwrap();
        let mut url: Url = database_url.parse().unwrap();
        url.query_pairs_mut()
            .append_pair("options", &format!("-csearch_path={schema}"));
        let pool = PgPoolOptions::new()
            .max_connections(12)
            .connect(url.as_str())
            .await
            .unwrap();
        peppy_server::MIGRATOR.run(&pool).await.unwrap();
        Self {
            admin,
            pool,
            pool_url: url.to_string(),
            schema,
        }
    }

    async fn app(&self) -> Router {
        self.app_for_pool(self.pool.clone()).await
    }

    async fn independent_pool(&self) -> PgPool {
        PgPoolOptions::new()
            .max_connections(6)
            .connect(&self.pool_url)
            .await
            .unwrap()
    }

    async fn constrained_pool(&self) -> PgPool {
        PgPoolOptions::new()
            .max_connections(1)
            .acquire_timeout(Duration::from_secs(1))
            .connect(&self.pool_url)
            .await
            .unwrap()
    }

    async fn app_for_pool(&self, pool: PgPool) -> Router {
        ServerBuilder::community(config(), pool)
            .setup_token("a".repeat(32))
            .build()
            .await
            .unwrap()
            .router()
    }

    async fn cleanup(self) {
        self.pool.close().await;
        sqlx::query(&format!("DROP SCHEMA {} CASCADE", self.schema))
            .execute(&self.admin)
            .await
            .unwrap();
        self.admin.close().await;
    }
}

fn source_for(attempt: usize) -> SocketAddr {
    SocketAddr::from((
        [192, 0, (attempt / 250) as u8, (attempt % 250 + 1) as u8],
        10_000,
    ))
}

fn claim_request(operation_id: Uuid, vault_id: Uuid, device_id: Uuid) -> Value {
    let profile = KeyProfile::new(vault_id, 1).unwrap();
    json!({
        "operation_id": operation_id,
        "setup_token": "a".repeat(32),
        "admin_password": "valid admin password",
        "device": {
            "device_id": device_id,
            "kind": "desktop",
            "name": "Primary device",
            "signing_public_key": URL_SAFE_NO_PAD.encode([7_u8; 32]),
        },
        "material": {
            "profile": serde_json::to_value(&profile).unwrap(),
            "check_header": URL_SAFE_NO_PAD.encode([9_u8; 48]),
            "fingerprint": profile.fingerprint().unwrap(),
            "key_epoch": 1,
            "admission_public_key": URL_SAFE_NO_PAD.encode([11_u8; 32]),
        },
        "credential_digest": "ab".repeat(32),
    })
}

fn config() -> Config {
    Config {
        bind_addr: "127.0.0.1:0".parse::<SocketAddr>().unwrap(),
        database_url: "postgres://unused".into(),
        release_identity: "admission-security-test".into(),
        s3: None,
        public_api_url: Some(Url::parse(ORIGIN).unwrap()),
        public_attachment_url: None,
        vault_attachment_quota_bytes: 1,
        trusted_proxy_cidrs: Vec::new(),
        replay_retention: Duration::from_secs(60),
        relay_url: None,
        mcp_worker_url: None,
        mcp_worker_token_file: None,
        web_client: None,
    }
}

async fn request_from(
    app: Router,
    method: Method,
    path: &str,
    body: Value,
    peer: Option<SocketAddr>,
) -> StatusCode {
    let mut request = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    if let Some(peer) = peer {
        request.extensions_mut().insert(ConnectInfo(peer));
    }
    app.oneshot(request).await.unwrap().status()
}

async fn authority_request(
    app: Router,
    method: Method,
    path: &str,
    authorization: Option<&str>,
) -> (StatusCode, String) {
    let body = if method == Method::DELETE && path.starts_with("/v1/attachments/") {
        json!({"compaction_generation": "0", "release_before_cursor": "0"}).to_string()
    } else {
        "{}".to_owned()
    };
    authority_request_body(app, method, path, authorization, body).await
}

async fn authority_request_body(
    app: Router,
    method: Method,
    path: &str,
    authorization: Option<&str>,
    body: String,
) -> (StatusCode, String) {
    let mut builder = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json");
    if let Some(authorization) = authorization {
        builder = builder.header("authorization", authorization);
    }
    let response = app
        .oneshot(builder.body(Body::from(body)).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let body = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    (status, String::from_utf8(body.to_vec()).unwrap())
}

async fn instance_authority_request(
    app: Router,
    method: Method,
    path: &str,
    session: Option<&str>,
) -> (StatusCode, String) {
    let cookie_restore = method == Method::GET && path == "/v1/instance/session";
    let mut builder = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json");
    if let Some(session) = session {
        builder = if cookie_restore {
            builder.header("cookie", format!("__Host-Peppy-Instance-Session={session}"))
        } else {
            builder.header("peppy-instance-session", session)
        };
    }
    let response = app
        .oneshot(builder.body(Body::from("{}")).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let body = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    (status, String::from_utf8(body.to_vec()).unwrap())
}

#[tokio::test]
async fn admission_global_limit_is_shared_by_independent_routers() {
    let fixture = Fixture::new().await;
    let first = fixture.app().await;
    let second = fixture.app().await;
    let request_body = json!({
        "username": "",
        "admin_password": "short",
        "client": "desktop",
    });

    for attempt in 0..100 {
        let app = if attempt % 2 == 0 {
            first.clone()
        } else {
            second.clone()
        };
        let peer = source_for(attempt);
        assert_eq!(
            request_from(
                app,
                Method::POST,
                "/v1/instance/session",
                request_body.clone(),
                Some(peer),
            )
            .await,
            StatusCode::UNAUTHORIZED,
        );
    }
    assert_eq!(
        request_from(
            first,
            Method::POST,
            "/v1/instance/session",
            request_body,
            Some(SocketAddr::from(([192, 0, 2, 251], 10_000))),
        )
        .await,
        StatusCode::TOO_MANY_REQUESTS,
    );
    fixture.cleanup().await;
}

#[tokio::test]
async fn concurrent_independent_pools_preserve_the_shared_global_admission_limit() {
    let fixture = Fixture::new().await;
    let first = fixture.app_for_pool(fixture.independent_pool().await).await;
    let second = fixture.app_for_pool(fixture.independent_pool().await).await;
    let body = json!({
        "username": "",
        "admin_password": "short",
        "client": "desktop",
    });
    let mut tasks = tokio::task::JoinSet::new();
    for attempt in 0..100 {
        let app = if attempt % 2 == 0 {
            first.clone()
        } else {
            second.clone()
        };
        let request_body = body.clone();
        tasks.spawn(async move {
            request_from(
                app,
                Method::POST,
                "/v1/instance/session",
                request_body,
                Some(source_for(attempt)),
            )
            .await
        });
    }
    while let Some(result) = tasks.join_next().await {
        assert_eq!(result.unwrap(), StatusCode::UNAUTHORIZED);
    }
    assert_eq!(
        request_from(
            first,
            Method::POST,
            "/v1/instance/session",
            body,
            Some(source_for(101)),
        )
        .await,
        StatusCode::TOO_MANY_REQUESTS,
    );
    fixture.cleanup().await;
}

#[tokio::test]
async fn claim_replay_does_not_acquire_a_second_connection_while_holding_the_owner_lock() {
    let fixture = Fixture::new().await;
    let request_body = claim_request(Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    assert_eq!(
        request_from(
            fixture.app().await,
            Method::POST,
            "/v1/instance/claim",
            request_body.clone(),
            Some(source_for(1)),
        )
        .await,
        StatusCode::OK,
    );
    let replay_app = fixture.app_for_pool(fixture.constrained_pool().await).await;
    assert_eq!(
        tokio::time::timeout(
            Duration::from_secs(3),
            request_from(
                replay_app,
                Method::POST,
                "/v1/instance/claim",
                request_body,
                Some(source_for(2)),
            ),
        )
        .await
        .expect("replay must not wait for a second connection"),
        StatusCode::OK,
    );
    fixture.cleanup().await;
}

#[tokio::test]
async fn route_authority_inventory_is_complete_and_device_routes_reject_bad_bearers() {
    let fixture = Fixture::new().await;
    let app = fixture.app().await;
    let sources = production_route_sources();

    // These handlers take their authentication from headers before consulting
    // state. Their valid paths avoid extractor failures, so a 422/404 can never
    // be mistaken for authorization evidence.
    let attachment = Uuid::new_v4();
    let device = Uuid::new_v4();
    let share = Uuid::new_v4();
    let mut covered = std::collections::BTreeSet::new();
    let device_routes = [
        (Method::GET, "/v1/vault".to_owned()),
        (Method::GET, "/v1/devices".to_owned()),
        (Method::GET, "/v1/mcp/device".to_owned()),
        (Method::GET, "/v1/devices/self/push-config".to_owned()),
        (Method::GET, "/v1/attachments/storage/status".to_owned()),
        (Method::GET, "/v1/account/access".to_owned()),
        (Method::GET, "/v1/capabilities".to_owned()),
        (Method::GET, "/v1/events".to_owned()),
        (Method::GET, "/v1/snapshot".to_owned()),
        (Method::GET, "/v1/snapshot/records?high_water=0".to_owned()),
        (Method::GET, "/v1/commands/pending".to_owned()),
        (Method::GET, "/v1/vault/key-profiles".to_owned()),
        (Method::GET, "/v1/attachments/usage".to_owned()),
        (Method::GET, "/v1/drafts?after=0".to_owned()),
        (Method::GET, format!("/v1/attachments/{attachment}")),
        (Method::POST, format!("/v1/devices/{device}/revoke")),
        (Method::POST, "/v1/compaction/capability".to_owned()),
        (
            Method::DELETE,
            format!("/v1/devices/self/wake-route?route_id={device}"),
        ),
        (Method::PUT, format!("/v1/attachments/{attachment}/upload")),
        (Method::DELETE, format!("/v1/attachments/{attachment}")),
        (Method::POST, format!("/v1/public-copies/{share}/revoke")),
    ];
    for (method, path) in device_routes {
        covered.insert(route_key(method.as_str(), &path));
        for (credential, code) in [
            (None, "missing_bearer"),
            (Some("Bearer malformed"), "invalid_bearer"),
        ] {
            let (status, body) =
                authority_request(app.clone(), method.clone(), &path, credential).await;
            assert_eq!(status, StatusCode::UNAUTHORIZED, "{method} {path}");
            assert_eq!(
                serde_json::from_str::<Value>(&body).unwrap()["code"],
                code,
                "{method} {path}"
            );
        }
    }
    let profile = KeyProfile::new(attachment, 1).unwrap();
    let profile_json = serde_json::to_value(&profile).unwrap();
    let envelope = |purpose: &str| {
        json!({
            "protocol_version": 1, "envelope_id": attachment, "vault_id": attachment,
            "producer_device_id": attachment, "producer_sequence": "1", "key_epoch": 1,
            "crypto_suite": 1, "profile_fingerprint": "0".repeat(64), "purpose": purpose,
            "command_id": if purpose == "command" { json!(attachment) } else { Value::Null },
            "route": if purpose == "command" { json!({"gateway_device_id": attachment, "subscription_id": "1"}) } else { Value::Null },
            "ciphertext": base64::engine::general_purpose::STANDARD.encode([9_u8; 64]), "compaction": Value::Null,
        })
    };
    let mutation_cases = vec![
        (
            Method::PUT,
            "/v1/devices/self/wake-route".to_owned(),
            json!({"route_id": attachment, "wake_credential": ""}),
        ),
        (
            Method::PUT,
            "/v1/devices/self/retention-capability".to_owned(),
            json!({"protocol_version": 1, "cleanup_ready": false}),
        ),
        (
            Method::POST,
            "/v1/capabilities".to_owned(),
            json!({"simulator": false, "capabilities": {}}),
        ),
        (
            Method::POST,
            format!("/v1/commands/{attachment}/receipts"),
            json!({"receipt": {}}),
        ),
        (
            Method::DELETE,
            "/v1/vault".to_owned(),
            json!({"vault_id": attachment}),
        ),
        (
            Method::POST,
            "/v1/vault/key-profiles".to_owned(),
            json!({"key_epoch": 2, "public_key_profile": profile_json, "encrypted_vault_check_header": "", "profile_fingerprint": "", "admission_public_key": ""}),
        ),
        (
            Method::POST,
            "/v1/vault/key-profiles/1/activate".to_owned(),
            json!({}),
        ),
        (
            Method::POST,
            "/v1/attachments/reserve".to_owned(),
            json!({"attachment_id": attachment, "declared_ciphertext_bytes": 64, "declared_ciphertext_sha256": "0".repeat(64)}),
        ),
        (
            Method::POST,
            format!("/v1/attachments/{attachment}/finalize"),
            json!({}),
        ),
        (
            Method::POST,
            format!("/v1/attachments/{attachment}/references"),
            json!({"references": [{"producer_device_id": attachment, "producer_sequence": "1"}]}),
        ),
        (
            Method::POST,
            format!("/v1/attachments/{attachment}/retention-uses"),
            json!({"use_id": device, "attachment_id": attachment, "device_id": attachment, "kind": "message_history"}),
        ),
        (
            Method::POST,
            format!("/v1/attachments/{attachment}/retention-uses/{device}/release"),
            json!({"use_id": device, "attachment_id": attachment, "device_id": attachment}),
        ),
        (
            Method::POST,
            format!("/v1/attachments/{attachment}/public-copies"),
            Value::String(String::new()),
        ),
        (
            Method::PUT,
            format!("/v1/drafts/{attachment}/revisions/{device}"),
            json!({
                "base_revision_id": Value::Null,
                "sealed_record": "",
                "attachment_ids": [],
                "authored": false,
                "resolves_branch_ids": [],
            }),
        ),
        (
            Method::POST,
            format!("/v1/drafts/{attachment}/send"),
            json!({"revision_id": device, "command_id": share}),
        ),
        (
            Method::POST,
            format!("/v1/drafts/{attachment}/discard"),
            json!({}),
        ),
        (Method::POST, "/v1/events".to_owned(), envelope("event")),
        (Method::POST, "/v1/commands".to_owned(), envelope("command")),
    ];
    for (method, path, body) in mutation_cases {
        covered.insert(route_key(method.as_str(), &path));
        let body = if let Value::String(body) = body {
            body
        } else {
            body.to_string()
        };
        for (credential, code) in [
            (None, "missing_bearer"),
            (Some("Bearer malformed"), "invalid_bearer"),
        ] {
            let (status, response) = authority_request_body(
                app.clone(),
                method.clone(),
                &path,
                credential,
                body.clone(),
            )
            .await;
            assert_eq!(status, StatusCode::UNAUTHORIZED, "{method} {path}");
            assert_eq!(
                serde_json::from_str::<Value>(&response).unwrap()["code"],
                code,
                "{method} {path}"
            );
        }
    }
    let recipient = json!({"device": {"device_id": attachment, "kind": "browser", "name": "test", "signing_public_key": ""}, "credential_digest": "", "requested_role": "device"});
    let owner_mutations = vec![
        (
            Method::POST,
            "/v1/enrollment/invitations".to_owned(),
            json!({"method": "transfer", "transfer_public_key": "", "invitation_token_digest": ""}),
        ),
        (
            Method::POST,
            format!("/v1/enrollment/invitations/{attachment}/approve"),
            json!({"transcript_hash": "", "encrypted_key_package": Value::Null, "transfer_nonce": Value::Null}),
        ),
        (
            Method::DELETE,
            format!("/v1/enrollment/invitations/{attachment}"),
            json!({}),
        ),
        (
            Method::POST,
            "/v1/enrollment/computers".to_owned(),
            json!({"invitation_id": attachment, "recipient": recipient, "recipient_transfer_public_key": "", "recipient_commitment": "", "claim_token_digest": ""}),
        ),
        (
            Method::DELETE,
            format!("/v1/enrollment/computers/{attachment}"),
            json!({}),
        ),
        (
            Method::POST,
            format!("/v1/enrollment/computers/{attachment}/approve"),
            json!({
                "transcript": {
                    "origin": ORIGIN, "vault_id": attachment, "fingerprint": "", "key_epoch": 1,
                    "invitation_id": attachment, "server_nonce": "", "owner_transfer_public_key": "",
                    "owner_device_id": device, "recipient": recipient,
                    "recipient_transfer_public_key": "", "granted_role": "device"
                },
                "encrypted_key_package": "", "transfer_nonce": "", "owner_proof": ""
            }),
        ),
    ];
    for (method, path, body) in owner_mutations {
        covered.insert(route_key(method.as_str(), &path));
        for credential in [None, Some("Bearer malformed")] {
            let (status, response) = authority_request_body(
                app.clone(),
                method.clone(),
                &path,
                credential,
                body.to_string(),
            )
            .await;
            assert_eq!(status, StatusCode::UNAUTHORIZED, "{method} {path}");
            let response = serde_json::from_str::<Value>(&response).unwrap();
            let field = if path.starts_with("/v1/enrollment/computers") {
                "error"
            } else {
                "code"
            };
            assert_eq!(response[field], "invalid_bearer", "{method} {path}");
        }
    }
    let webhook_input = json!({
        "name": null, "url": "https://example.com/hook", "events": ["message.received"],
        "filterMode": "all", "filter": [], "paused": false,
    });
    let owner_mcp_routes = [
        (Method::GET, "/v1/mcp/grants".to_owned(), json!({})),
        (
            Method::GET,
            format!("/v1/mcp/authorizations/{attachment}"),
            json!({}),
        ),
        (Method::POST, "/v1/mcp/device/prepare".to_owned(), json!({})),
        (
            Method::POST,
            "/v1/mcp/device/activate".to_owned(),
            json!({
                "operation_id": attachment,
                "status": {
                    "state": "approved", "expires_in_seconds": 60,
                    "transcript": null, "encrypted_key_package": null,
                    "transfer_nonce": null, "owner_proof": null,
                },
            }),
        ),
        (
            Method::POST,
            "/v1/mcp/device/complete".to_owned(),
            json!({
                "operation_id": attachment,
                "admission": {
                    "vault_id": attachment, "device_id": device, "role": "device",
                    "material": {
                        "profile": {}, "check_header": "check", "fingerprint": "11".repeat(32),
                        "key_epoch": 1, "admission_public_key": "key",
                    },
                },
            }),
        ),
        (
            Method::POST,
            format!("/v1/mcp/grants/{attachment}/revoke"),
            json!({}),
        ),
        (
            Method::POST,
            format!("/v1/mcp/authorizations/{attachment}/approve"),
            json!({"scopes": ["conversations:read"]}),
        ),
        (
            Method::POST,
            format!("/v1/mcp/authorizations/{attachment}/deny"),
            json!({}),
        ),
        (Method::GET, "/v1/mcp/webhooks".to_owned(), json!({})),
        (
            Method::POST,
            "/v1/mcp/webhooks".to_owned(),
            webhook_input.clone(),
        ),
        (
            Method::PUT,
            format!("/v1/mcp/webhooks/{attachment}"),
            webhook_input,
        ),
        (
            Method::DELETE,
            format!("/v1/mcp/webhooks/{attachment}"),
            json!({}),
        ),
        (
            Method::POST,
            format!("/v1/mcp/webhooks/{attachment}/secret"),
            json!({}),
        ),
        (
            Method::POST,
            format!("/v1/mcp/webhooks/{attachment}/rotate-secret"),
            json!({}),
        ),
        (
            Method::POST,
            format!("/v1/mcp/webhooks/{attachment}/test"),
            json!({}),
        ),
    ];
    for (method, path, body) in owner_mcp_routes {
        covered.insert(route_key(method.as_str(), &path));
        for credential in [None, Some("Bearer malformed")] {
            let (status, _) = authority_request_body(
                app.clone(),
                method.clone(),
                &path,
                credential,
                body.to_string(),
            )
            .await;
            assert_eq!(status, StatusCode::UNAUTHORIZED, "{method} {path}");
        }
    }
    // The Axum `any` route authenticates before delegating to rmcp. The SDK is
    // configured without legacy sessions or replay, so POST is its only
    // supported method; other methods would be its intentional 405 response.
    for credential in [None, Some("Bearer malformed")] {
        let (status, response) = authority_request_body(
            app.clone(),
            Method::POST,
            "/mcp",
            credential,
            json!({"jsonrpc": "2.0", "id": 1, "method": "initialize"}).to_string(),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "POST /mcp");
        assert_eq!(
            serde_json::from_str::<Value>(&response).unwrap()["error"],
            "invalid_token",
            "POST /mcp"
        );
    }
    covered.insert(route_key("POST", "/mcp"));
    let instance_routes = [
        (
            "/v1/instance/session",
            StatusCode::UNAUTHORIZED,
            "invalid_session",
        ),
        (
            "/v1/instance/account",
            StatusCode::UNAUTHORIZED,
            "invalid_session",
        ),
        (
            "/v1/instance/management",
            StatusCode::FORBIDDEN,
            "admin_session_required",
        ),
        (
            "/v1/instance/vaults",
            StatusCode::UNAUTHORIZED,
            "invalid_session",
        ),
        (
            "/v1/instance/vault-requests",
            StatusCode::UNAUTHORIZED,
            "invalid_session",
        ),
        (
            "/v1/instance/invites",
            StatusCode::UNAUTHORIZED,
            "invalid_session",
        ),
        (
            "/v1/instance/events",
            StatusCode::UNAUTHORIZED,
            "invalid_session",
        ),
    ];
    for (path, expected_status, expected_code) in instance_routes {
        covered.insert(route_key("GET", path));
        for session in [None, Some("malformed")] {
            let (status, body) =
                instance_authority_request(app.clone(), Method::GET, path, session).await;
            assert_eq!(status, expected_status, "GET {path}");
            assert_eq!(
                serde_json::from_str::<Value>(&body).unwrap()["code"],
                expected_code,
                "GET {path}"
            );
        }
    }
    for (method, path) in [
        (Method::DELETE, "/v1/instance/session".to_owned()),
        (
            Method::POST,
            format!("/v1/instance/devices/{device}/revoke"),
        ),
        (Method::POST, "/v1/instance/invites".to_owned()),
        (Method::DELETE, format!("/v1/instance/invites/{device}")),
        (
            Method::POST,
            format!("/v1/instance/vault-requests/{device}/approve"),
        ),
        (
            Method::POST,
            format!("/v1/instance/vault-requests/{device}/deny"),
        ),
    ] {
        covered.insert(route_key(method.as_str(), &path));
        for session in [None, Some("malformed")] {
            let (status, body) =
                instance_authority_request(app.clone(), method.clone(), &path, session).await;
            assert_eq!(status, StatusCode::UNAUTHORIZED, "{method} {path}");
            assert_eq!(
                serde_json::from_str::<Value>(&body).unwrap()["code"],
                "invalid_session",
                "{method} {path}"
            );
        }
    }
    // A real connection supplies Hyper's OnUpgrade extension, so the WebSocket
    // probe cannot mistake a 426 extractor rejection for an authentication check.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(peppy_server::serve(listener, app.clone(), async {
        let _ = stopped.await;
    }));
    let client = reqwest::Client::new();
    for (credential, code) in [
        (None, "missing_bearer"),
        (Some("Bearer malformed"), "invalid_bearer"),
    ] {
        let mut request = client
            .get(format!("http://{address}/v1/ws"))
            .header("connection", "upgrade")
            .header("upgrade", "websocket")
            .header("sec-websocket-version", "13")
            .header("sec-websocket-key", STANDARD.encode([0x42_u8; 16]));
        if let Some(credential) = credential {
            request = request.header("authorization", credential);
        }
        let response = request.send().await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(response.json::<Value>().await.unwrap()["code"], code);
    }
    drop(client);
    stop.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    covered.insert(route_key("GET", "/v1/ws"));

    for (method, path, reason) in [
        ("GET", "/healthz", "public liveness"),
        ("GET", "/readyz", "public readiness"),
        ("GET", "/__release", "public revision"),
        ("GET", "/v1/instance", "public setup discovery"),
        (
            "POST",
            "/v1/instance/claim",
            "setup token or bounded first-claim window",
        ),
        ("POST", "/v1/instance/session", "password authentication"),
        (
            "POST",
            "/v1/instance/vault-requests",
            "single-use invite capability",
        ),
        (
            "GET",
            "/v1/instance/vault-requests/{request_id}",
            "request-secret capability",
        ),
        (
            "POST",
            "/v1/instance/vaults",
            "approved request secret and password",
        ),
        (
            "GET",
            "/file/mms-usercontent/{token}/{safe_name}",
            "public-copy possession token",
        ),
        (
            "POST",
            "/v1/enrollment/challenges",
            "public challenge; no admission without proof",
        ),
        (
            "POST",
            "/v1/enrollment/challenges/{enrollment_id}/complete",
            "admission and device signatures",
        ),
        (
            "POST",
            "/v1/enrollment/invitations/claim",
            "QR invitation claim authenticator",
        ),
        (
            "GET",
            "/v1/enrollment/invitations/{invitation_id}",
            "owner bearer or claim-secret capability",
        ),
        (
            "POST",
            "/v1/enrollment/invitations/{invitation_id}/confirm",
            "claim secret and signatures",
        ),
        (
            "POST",
            "/v1/enrollment/computers/{id}/status",
            "computer claim-token capability",
        ),
        (
            "POST",
            "/v1/enrollment/computers/{id}/confirm",
            "computer claim token and signatures",
        ),
        (
            "GET",
            "/.well-known/oauth-protected-resource",
            "public OAuth protected-resource metadata",
        ),
        (
            "GET",
            "/.well-known/oauth-authorization-server",
            "public OAuth authorization-server metadata",
        ),
        (
            "POST",
            "/oauth/register",
            "public dynamic client registration admission",
        ),
        (
            "GET",
            "/oauth/authorize",
            "public PKCE authorization request before owner consent",
        ),
        (
            "POST",
            "/oauth/token",
            "authorization-code or refresh-token proof",
        ),
        ("POST", "/oauth/revoke", "opaque token proof"),
    ] {
        assert!(!reason.is_empty());
        assert!(
            covered.insert(route_key(method, path)),
            "duplicate exception {method} {path}"
        );
    }
    let declared: std::collections::BTreeSet<_> = sources
        .iter()
        .flat_map(|(_, source)| declared_route_pairs(source))
        .collect();
    assert_eq!(
        declared, covered,
        "every method/path needs an authentication probe or explicit capability exception"
    );
    fixture.cleanup().await;
}

fn route_key(method: &str, path: &str) -> (String, String) {
    let path = path.split('?').next().unwrap();
    let normalized = path
        .split('/')
        .map(|part| {
            if part.starts_with('{') || Uuid::parse_str(part).is_ok() || part.parse::<u32>().is_ok()
            {
                "{}"
            } else {
                part
            }
        })
        .collect::<Vec<_>>()
        .join("/");
    (method.to_owned(), normalized)
}

fn production_route_sources() -> Vec<(std::path::PathBuf, String)> {
    let mut directories = vec![std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src")];
    let mut sources = Vec::new();
    while let Some(directory) = directories.pop() {
        for entry in std::fs::read_dir(directory).unwrap() {
            let entry = entry.unwrap();
            let kind = entry.file_type().unwrap();
            let path = entry.path();
            if kind.is_dir() {
                directories.push(path);
            } else if kind.is_file() && path.extension().is_some_and(|extension| extension == "rs")
            {
                let content = std::fs::read_to_string(&path).unwrap();
                sources.push((path, content));
            }
        }
    }
    sources
}

fn declared_route_pairs(source: &str) -> Vec<(String, String)> {
    let mut remaining = source.split("#[cfg(test)]").next().unwrap();
    let mut pairs = Vec::new();
    while let Some(start) = remaining.find(".route(") {
        let arguments = &remaining[start + ".route(".len()..];
        let mut depth = 1;
        let mut quoted = false;
        let mut escaped = false;
        let mut end = None;
        for (index, character) in arguments.char_indices() {
            if quoted {
                if escaped {
                    escaped = false;
                } else if character == '\\' {
                    escaped = true;
                } else if character == '"' {
                    quoted = false;
                }
            } else {
                match character {
                    '"' => quoted = true,
                    '(' => depth += 1,
                    ')' => {
                        depth -= 1;
                        if depth == 0 {
                            end = Some(index);
                            break;
                        }
                    }
                    _ => {}
                }
            }
        }
        let end = end.expect("route declaration must have balanced parentheses");
        let (path, handlers) = arguments[..end].split_once(',').expect("route arguments");
        let path = path.trim();
        let path = if path.starts_with('"') && path.ends_with('"') {
            &path[1..path.len() - 1]
        } else if path.ends_with("DEVICE_ACCOUNT_ACCESS") {
            peppy_hosted_api::account_access::routes::DEVICE_ACCOUNT_ACCESS
        } else if path.ends_with("DEVICE_ATTACHMENT_STORAGE_STATUS") {
            peppy_hosted_api::attachments::routes::DEVICE_ATTACHMENT_STORAGE_STATUS
        } else {
            panic!("classify new route constant: {path}");
        };
        let declared_path = path
            .split('/')
            .map(|part| {
                if part.starts_with('{') && part.ends_with('}') {
                    "{}"
                } else {
                    part
                }
            })
            .collect::<Vec<_>>()
            .join("/");
        let before = pairs.len();
        if handlers.trim() == "any(handle)" {
            assert_eq!(
                path, "/mcp",
                "classify the supported method for new any route"
            );
            // rmcp only accepts POST with the configured NeverSessionManager.
            pairs.push(("POST".to_owned(), declared_path));
        } else {
            assert!(
                !handlers.trim_start().starts_with("any("),
                "classify new method-router expression for {path}"
            );
            for method in [
                "get", "post", "put", "patch", "delete", "head", "options", "trace",
            ] {
                let needle = format!("{method}(");
                if handlers.match_indices(&needle).any(|(position, _)| {
                    position == 0
                        || !handlers.as_bytes()[position - 1].is_ascii_alphanumeric()
                            && handlers.as_bytes()[position - 1] != b'_'
                }) {
                    pairs.push((method.to_ascii_uppercase(), declared_path.clone()));
                }
            }
        }
        assert!(
            pairs.len() > before,
            "classify new method-router expression for {path}"
        );
        remaining = &arguments[end + 1..];
    }
    pairs
}

#[test]
fn route_inventory_distinguishes_methods_added_to_an_existing_path() {
    let pairs = declared_route_pairs(r#"Router::new().route("/one", get(read).post(write))"#);
    assert_eq!(
        pairs,
        vec![route_key("GET", "/one"), route_key("POST", "/one")]
    );
}

#[test]
fn route_inventory_classifies_sdk_any_route_by_its_supported_method() {
    let pairs = declared_route_pairs(r#"Router::new().route("/mcp", any(handle))"#);
    assert_eq!(pairs, vec![route_key("POST", "/mcp")]);
}

#[test]
#[should_panic(expected = "classify new method-router expression")]
fn route_inventory_rejects_chained_sdk_any_routes() {
    declared_route_pairs(r#"Router::new().route("/mcp", any(handle).get(other_handler))"#);
}

#[test]
fn route_inventory_does_not_collapse_literal_paths_into_parameters() {
    let pairs = declared_route_pairs(
        r#"Router::new().route("/one/{id}", get(read)).route("/one/2", get(other))"#,
    );
    assert_eq!(
        pairs,
        vec![
            ("GET".into(), "/one/{}".into()),
            ("GET".into(), "/one/2".into())
        ]
    );
}
