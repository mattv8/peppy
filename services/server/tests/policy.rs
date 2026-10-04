use std::{net::SocketAddr, sync::Arc, time::Duration};

use axum::{
    body::Body,
    http::{Request, StatusCode, header},
};
use base64::Engine as _;
use http_body_util::BodyExt;
use peppy_server::{
    ServerBuilder,
    api::{AccessPolicy, AccessPrincipal, Operation, PolicyFuture, create_owner},
    config::Config,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{PgPool, postgres::PgPoolOptions};
use tower::ServiceExt;
use uuid::Uuid;

static TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[derive(Default)]
struct Policy {
    allowed: Vec<Operation>,
    quota: Option<i64>,
    seen: tokio::sync::Mutex<Vec<(AccessPrincipal, Operation)>>,
}

impl Policy {
    fn allowing(allowed: impl IntoIterator<Item = Operation>) -> Self {
        Self {
            allowed: allowed.into_iter().collect(),
            ..Self::default()
        }
    }

    async fn calls(&self) -> Vec<(AccessPrincipal, Operation)> {
        self.seen.lock().await.clone()
    }
}

impl AccessPolicy for Policy {
    fn authorize(
        &self,
        principal: AccessPrincipal,
        operation: Operation,
    ) -> PolicyFuture<'_, bool> {
        Box::pin(async move {
            self.seen.lock().await.push((principal, operation));
            self.allowed.contains(&operation)
        })
    }

    fn attachment_quota_bytes(&self, principal: AccessPrincipal) -> PolicyFuture<'_, Option<i64>> {
        Box::pin(async move {
            self.seen.lock().await.push((principal, Operation::Upload));
            self.quota
        })
    }
}

struct DatabasePolicy {
    pool: tokio::sync::OnceCell<PgPool>,
    quota: i64,
}

impl AccessPolicy for DatabasePolicy {
    fn authorize(
        &self,
        _principal: AccessPrincipal,
        _operation: Operation,
    ) -> PolicyFuture<'_, bool> {
        Box::pin(async move {
            sqlx::query_scalar::<_, i32>("SELECT 1")
                .fetch_one(self.pool.get().expect("test policy pool initialized"))
                .await
                .is_ok()
        })
    }

    fn attachment_quota_bytes(&self, _principal: AccessPrincipal) -> PolicyFuture<'_, Option<i64>> {
        Box::pin(async move {
            sqlx::query_scalar::<_, i32>("SELECT 1")
                .fetch_one(self.pool.get().expect("test policy pool initialized"))
                .await
                .ok()
                .map(|_| self.quota)
        })
    }
}

struct TestServer {
    pool: PgPool,
    admin: PgPool,
    schema: String,
    router: axum::Router,
}

impl TestServer {
    async fn start(policy: Arc<Policy>) -> Self {
        Self::start_with_policy(policy, 5).await
    }

    async fn start_with_policy(policy: Arc<dyn AccessPolicy>, max_connections: u32) -> Self {
        let database_url = std::env::var("TEST_DATABASE_URL")
            .expect("TEST_DATABASE_URL is required; integration tests never silently skip");
        let admin = PgPoolOptions::new()
            .max_connections(2)
            .connect(&database_url)
            .await
            .unwrap();
        let schema = format!("peppy_policy_test_{}", Uuid::new_v4().simple());
        sqlx::query(&format!("CREATE SCHEMA {schema}"))
            .execute(&admin)
            .await
            .unwrap();
        let mut isolated_url: url::Url = database_url.parse().unwrap();
        isolated_url
            .query_pairs_mut()
            .append_pair("options", &format!("-csearch_path={schema}"));
        let pool = PgPoolOptions::new()
            .max_connections(max_connections)
            .connect(isolated_url.as_str())
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();

        let mut config = Config::from_env().expect("complete S3 test configuration is required");
        config.bind_addr = "127.0.0.1:0".parse::<SocketAddr>().unwrap();
        config.database_url = isolated_url.to_string();
        config.release_identity = "policy-test".into();
        config.replay_retention = Duration::from_secs(86_400);
        let server = ServerBuilder::new(config, pool.clone())
            .access_policy(policy)
            .build()
            .await
            .unwrap();

        Self {
            pool,
            admin,
            schema,
            router: server.router(),
        }
    }

    async fn owner(&self) -> peppy_server::api::Credential {
        let vault = Uuid::new_v4();
        let (profile, fingerprint) = profile(vault, 1);
        create_owner(&self.pool, profile, vec![7, 8, 9], fingerprint, 1)
            .await
            .unwrap()
    }

    async fn request(
        &self,
        method: &str,
        path: &str,
        token: &str,
        body: Value,
    ) -> (StatusCode, Value) {
        let response = self
            .router
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(path)
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    async fn raw_request(
        &self,
        method: &str,
        path: &str,
        token: &str,
        body: impl Into<Body>,
        extra_headers: &[(&str, &str)],
    ) -> (StatusCode, Value) {
        let mut request = Request::builder()
            .method(method)
            .uri(path)
            .header(header::AUTHORIZATION, format!("Bearer {token}"));
        for (name, value) in extra_headers {
            request = request.header(*name, *value);
        }
        let response = self
            .router
            .clone()
            .oneshot(request.body(body.into()).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    async fn shutdown(self) {
        self.pool.close().await;
        sqlx::query(&format!("DROP SCHEMA {} CASCADE", self.schema))
            .execute(&self.admin)
            .await
            .unwrap();
        self.admin.close().await;
    }
}

fn profile(vault: Uuid, epoch: u32) -> (Value, String) {
    let salt: Vec<u8> = (0..16).collect();
    let mut digest = Sha256::new();
    digest.update(b"peppy-key-profile-v1\0");
    digest.update(1_u16.to_be_bytes());
    digest.update(&salt);
    digest.update(vault.as_bytes());
    digest.update(epoch.to_be_bytes());
    (
        json!({"crypto_suite": 1, "salt": salt, "vault_id": vault, "key_epoch": epoch}),
        hex::encode(digest.finalize()),
    )
}

fn event(vault: Uuid, device: Uuid, fingerprint: &str) -> Value {
    json!({
        "protocol_version": 1,
        "envelope_id": Uuid::new_v4(),
        "command_id": null,
        "vault_id": vault,
        "producer_device_id": device,
        "producer_sequence": "1",
        "key_epoch": 1,
        "crypto_suite": 1,
        "profile_fingerprint": fingerprint,
        "purpose": "event",
        "route": null,
        "ciphertext": base64::engine::general_purpose::STANDARD.encode([1_u8]),
    })
}

fn assert_called_by(
    calls: &[(AccessPrincipal, Operation)],
    credential: &peppy_server::api::Credential,
    operation: Operation,
) {
    assert!(calls.iter().any(|(principal, actual)| {
        *actual == operation
            && principal.vault_id == credential.vault_id
            && principal.device_id == credential.device_id
            && principal.role == credential.role
    }));
}

#[tokio::test]
async fn policy_receives_verified_principals_and_cannot_be_tricked_by_foreign_envelopes() {
    let _guard = TEST_LOCK.lock().await;
    let policy = Arc::new(Policy::allowing([
        Operation::Read,
        Operation::Sync,
        Operation::Export,
        Operation::Revoke,
        Operation::DeleteVault,
        Operation::AttachmentDelete,
        Operation::Send,
        Operation::Upload,
        Operation::Publish,
        Operation::Pair,
    ]));
    let server = TestServer::start(policy.clone()).await;
    let first = server.owner().await;
    let second = server.owner().await;

    assert_eq!(
        server
            .request("GET", "/v1/vault", &first.device_token, json!(null))
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        server
            .request("GET", "/v1/vault", &second.device_token, json!(null))
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        server
            .request(
                "POST",
                "/v1/events",
                &first.device_token,
                event(second.vault_id, first.device_id, "not-a-real-fingerprint")
            )
            .await
            .0,
        StatusCode::FORBIDDEN
    );

    let calls = policy.calls().await;
    assert_called_by(&calls, &first, Operation::Read);
    assert_called_by(&calls, &second, Operation::Read);
    assert_called_by(&calls, &first, Operation::Send);
    assert!(
        !calls
            .iter()
            .any(|(principal, _)| principal.vault_id == second.vault_id
                && principal.device_id == first.device_id)
    );
    server.shutdown().await;
}

#[tokio::test]
async fn readonly_policy_denies_mutations_before_semantic_validation_but_permits_safe_operations() {
    let _guard = TEST_LOCK.lock().await;
    let policy = Arc::new(Policy::allowing([
        Operation::Read,
        Operation::Sync,
        Operation::Revoke,
        Operation::DeleteVault,
        Operation::AttachmentDelete,
    ]));
    let server = TestServer::start(policy.clone()).await;
    let owner = server.owner().await;
    let target = Uuid::new_v4();
    sqlx::query("INSERT INTO devices(vault_id,device_id,role,public_key,profile_fingerprint,key_epoch) VALUES($1,$2,'device',$3,$4,1)")
        .bind(owner.vault_id).bind(target).bind(json!({"test": true})).bind(profile(owner.vault_id, 1).1)
        .execute(&server.pool).await.unwrap();

    assert_eq!(
        server
            .request("GET", "/v1/vault", &owner.device_token, json!(null))
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        server
            .request(
                "GET",
                "/v1/events?after=0",
                &owner.device_token,
                json!(null)
            )
            .await
            .0,
        StatusCode::OK
    );
    let denied = vec![
        (
            "POST",
            "/v1/events".to_owned(),
            event(owner.vault_id, owner.device_id, "not-a-real-fingerprint"),
        ),
        (
            "POST",
            "/v1/attachments/reserve".to_owned(),
            json!({"declared_ciphertext_bytes": 1, "declared_ciphertext_sha256": "not-a-hash"}),
        ),
        (
            "PUT",
            format!("/v1/attachments/{}/upload", Uuid::new_v4()),
            json!({"not": "ciphertext"}),
        ),
        (
            "POST",
            "/v1/pairing".to_owned(),
            json!({"device_id": Uuid::new_v4(), "public_key": {}, "profile_fingerprint": "invalid", "key_epoch": 1, "requested_role": "invalid"}),
        ),
        (
            "POST",
            format!("/v1/attachments/{}/public-copies", Uuid::new_v4()),
            json!({"not": "an image"}),
        ),
        (
            "POST",
            "/v1/vault/key-profiles".to_owned(),
            json!({"key_epoch": 2, "public_key_profile": {}, "encrypted_vault_check_header": "", "profile_fingerprint": "invalid"}),
        ),
    ];
    for (method, path, body) in denied {
        assert_eq!(
            server
                .request(method, &path, &owner.device_token, body)
                .await
                .0,
            StatusCode::FORBIDDEN,
            "{path}"
        );
    }
    assert_eq!(
        server
            .request(
                "POST",
                &format!("/v1/devices/{target}/revoke"),
                &owner.device_token,
                json!(null)
            )
            .await
            .0,
        StatusCode::NO_CONTENT
    );

    let deletable = server.owner().await;
    assert_eq!(
        server
            .request(
                "DELETE",
                "/v1/vault",
                &deletable.device_token,
                json!({"vault_id": deletable.vault_id})
            )
            .await
            .0,
        StatusCode::NO_CONTENT
    );
    let calls = policy.calls().await;
    for operation in [
        Operation::Read,
        Operation::Sync,
        Operation::Send,
        Operation::Upload,
        Operation::Pair,
        Operation::Publish,
        Operation::Revoke,
    ] {
        assert_called_by(&calls, &owner, operation);
    }
    assert_called_by(&calls, &deletable, Operation::DeleteVault);
    server.shutdown().await;
}

#[tokio::test]
async fn policy_attachment_quota_overrides_the_server_default_for_reservations() {
    let _guard = TEST_LOCK.lock().await;
    let policy = Arc::new(Policy {
        allowed: vec![Operation::Upload],
        quota: Some(3),
        ..Policy::default()
    });
    let server = TestServer::start(policy.clone()).await;
    let owner = server.owner().await;
    for (bytes, expected) in [
        (b"abc".as_slice(), StatusCode::OK),
        (b"abcd".as_slice(), StatusCode::PAYLOAD_TOO_LARGE),
    ] {
        let (status, body) = server
            .request(
                "POST",
                "/v1/attachments/reserve",
                &owner.device_token,
                json!({
                    "declared_ciphertext_bytes": bytes.len(),
                    "declared_ciphertext_sha256": hex::encode(Sha256::digest(bytes)),
                }),
            )
            .await;
        assert_eq!(status, expected, "{body}");
    }
    let calls = policy.calls().await;
    assert_called_by(&calls, &owner, Operation::Upload);
    server.shutdown().await;
}

#[tokio::test]
async fn quota_policy_callback_does_not_reacquire_a_one_connection_pool_inside_the_transaction() {
    let _guard = TEST_LOCK.lock().await;
    let policy = Arc::new(DatabasePolicy {
        pool: tokio::sync::OnceCell::new(),
        quota: 64 * 1024 * 1024,
    });
    let server = TestServer::start_with_policy(policy.clone(), 1).await;
    policy.pool.set(server.pool.clone()).unwrap();
    let owner = server.owner().await;

    let result = tokio::time::timeout(
        Duration::from_secs(2),
        server.request(
            "POST",
            "/v1/attachments/reserve",
            &owner.device_token,
            json!({
                "declared_ciphertext_bytes": 1,
                "declared_ciphertext_sha256": hex::encode(Sha256::digest(b"x")),
            }),
        ),
    )
    .await
    .expect("request deadlocked by reacquiring its one-connection runtime pool");
    assert_eq!(result.0, StatusCode::OK, "{}", result.1);
    server.shutdown().await;
}

#[tokio::test]
async fn public_copy_uses_the_policy_quota_instead_of_the_community_default() {
    let _guard = TEST_LOCK.lock().await;
    let policy = Arc::new(Policy {
        allowed: vec![Operation::Upload, Operation::Publish],
        quota: Some(8),
        ..Policy::default()
    });
    let server = TestServer::start(policy).await;
    let owner = server.owner().await;
    let (_, reserved) = server
        .request(
            "POST",
            "/v1/attachments/reserve",
            &owner.device_token,
            json!({
                "declared_ciphertext_bytes": 1,
                "declared_ciphertext_sha256": hex::encode(Sha256::digest(b"x")),
            }),
        )
        .await;
    let attachment = Uuid::parse_str(reserved["attachment_id"].as_str().unwrap()).unwrap();
    assert_eq!(
        server
            .raw_request(
                "PUT",
                &format!("/v1/attachments/{attachment}/upload"),
                &owner.device_token,
                Body::from("x"),
                &[],
            )
            .await
            .0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        server
            .raw_request(
                "POST",
                &format!("/v1/attachments/{attachment}/finalize"),
                &owner.device_token,
                Body::empty(),
                &[],
            )
            .await
            .0,
        StatusCode::OK
    );
    let png = b"\x89PNG\r\n\x1a\n";
    let response = server
        .raw_request(
            "POST",
            &format!("/v1/attachments/{attachment}/public-copies"),
            &owner.device_token,
            Body::from(png.as_slice()),
            &[("x-file-name", "copy.png")],
        )
        .await;
    assert_eq!(response.0, StatusCode::PAYLOAD_TOO_LARGE, "{}", response.1);
    server.shutdown().await;
}

#[tokio::test]
async fn pairing_policy_callback_does_not_reacquire_a_one_connection_pool_inside_the_transaction() {
    let _guard = TEST_LOCK.lock().await;
    let policy = Arc::new(DatabasePolicy {
        pool: tokio::sync::OnceCell::new(),
        quota: 64 * 1024 * 1024,
    });
    let server = TestServer::start_with_policy(policy.clone(), 1).await;
    policy.pool.set(server.pool.clone()).unwrap();
    let owner = server.owner().await;
    let (status, intent) = server
        .request(
            "POST",
            "/v1/pairing/intents",
            &owner.device_token,
            json!({"https_origin": "https://example.test/"}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{intent}");
    let intent_token = intent["intent_token"].as_str().unwrap();
    let signing = ed25519_dalek::SigningKey::from_bytes(&[7_u8; 32]);
    let public_key =
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(signing.verifying_key().as_bytes());

    let result = tokio::time::timeout(
        Duration::from_secs(2),
        server.request(
            "POST",
            &format!("/v1/pairing/intents/{intent_token}/claim"),
            &owner.device_token,
            json!({
                "device_id": Uuid::new_v4(),
                "public_key": {"ed25519_public_key": public_key},
                "requested_role": "device",
            }),
        ),
    )
    .await
    .expect("pairing policy deadlocked by reacquiring its one-connection runtime pool");
    assert_eq!(result.0, StatusCode::OK, "{}", result.1);
    server.shutdown().await;
}
