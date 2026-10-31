use std::{io::Cursor, net::SocketAddr, sync::Arc, time::Duration};

use axum::{
    body::Body,
    http::{Request, StatusCode, header},
};
use base64::Engine as _;
use http_body_util::BodyExt;
use peppy_hosted_api::{ApnsEnvironment, PushConfigAdvertisement, PushProvider};
use peppy_server::{
    ServerBuilder,
    api::{
        AccessPolicy, AccessPrincipal, AttachmentStoragePolicy, Operation, PolicyFuture,
        create_owner,
    },
    config::Config,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{PgPool, postgres::PgPoolOptions};
use tower::ServiceExt;
use uuid::Uuid;

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

struct CleanupPolicy {
    limit_bytes: i64,
}

impl AccessPolicy for CleanupPolicy {
    fn authorize(
        &self,
        _principal: AccessPrincipal,
        _operation: Operation,
    ) -> PolicyFuture<'_, bool> {
        Box::pin(async { true })
    }

    fn attachment_storage_policy_in_transaction<'a>(
        &'a self,
        _principal: AccessPrincipal,
        _configured_limit_bytes: i64,
        _connection: &'a mut sqlx::PgConnection,
    ) -> PolicyFuture<'a, Result<AttachmentStoragePolicy, sqlx::Error>> {
        Box::pin(async move {
            Ok(AttachmentStoragePolicy {
                allow_upload: true,
                limit_bytes: self.limit_bytes,
                cleanup_authorized: true,
                policy_version: Some("test-policy".into()),
            })
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
        Self::start_with_policy_and_push_config(policy, 5, PushConfigAdvertisement::default()).await
    }

    async fn start_with_policy(policy: Arc<dyn AccessPolicy>, max_connections: u32) -> Self {
        Self::start_with_policy_and_push_config(
            policy,
            max_connections,
            PushConfigAdvertisement::default(),
        )
        .await
    }

    async fn start_with_policy_and_push_config(
        policy: Arc<dyn AccessPolicy>,
        max_connections: u32,
        push_config: PushConfigAdvertisement,
    ) -> Self {
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
            .push_config(push_config)
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

    async fn add_device(
        &self,
        owner: &peppy_server::api::Credential,
    ) -> peppy_server::api::Credential {
        let device_id = Uuid::new_v4();
        let device_token = (0..3)
            .map(|_| Uuid::new_v4().simple().to_string())
            .collect::<String>();
        let fingerprint: String =
            sqlx::query_scalar("SELECT profile_fingerprint FROM vaults WHERE vault_id=$1")
                .bind(owner.vault_id)
                .fetch_one(&self.pool)
                .await
                .unwrap();
        sqlx::query("INSERT INTO devices(vault_id,device_id,role,public_key,profile_fingerprint,key_epoch) VALUES($1,$2,'device','{}',$3,1)")
            .bind(owner.vault_id)
            .bind(device_id)
            .bind(fingerprint)
            .execute(&self.pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO device_credentials(token_digest,vault_id,device_id) VALUES($1,$2,$3)",
        )
        .bind(Sha256::digest(device_token.as_bytes()).as_slice())
        .bind(owner.vault_id)
        .bind(device_id)
        .execute(&self.pool)
        .await
        .unwrap();
        peppy_server::api::Credential {
            device_token,
            vault_id: owner.vault_id,
            device_id,
            role: "device".into(),
        }
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

/// Reads the roster entry for this credential's device, not an arbitrary index.
async fn device_roster(server: &TestServer, credential: &peppy_server::api::Credential) -> Value {
    let (status, roster) = server
        .request("GET", "/v1/devices", &credential.device_token, json!(null))
        .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "device roster must accept the credential"
    );
    roster["devices"]
        .as_array()
        .expect("roster devices array")
        .iter()
        .find(|device| device["device_id"] == json!(credential.device_id))
        .cloned()
        .expect("roster contains the credential's device")
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
async fn attachment_usage_uses_policy_quota_and_requires_read_permission() {
    let policy = Arc::new(Policy {
        allowed: vec![Operation::Read],
        quota: Some(123_456),
        ..Policy::default()
    });
    let server = TestServer::start(policy.clone()).await;
    let owner = server.owner().await;
    let (status, usage) = server
        .request(
            "GET",
            "/v1/attachments/usage",
            &owner.device_token,
            json!(null),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(usage, json!({"used_bytes": 0, "limit_bytes": 123_456}));
    let calls = policy.calls().await;
    assert_called_by(&calls, &owner, Operation::Read);
    assert_called_by(&calls, &owner, Operation::Upload);
    server.shutdown().await;

    let denied = Arc::new(Policy {
        quota: Some(123_456),
        ..Policy::default()
    });
    let server = TestServer::start(denied).await;
    let owner = server.owner().await;
    assert_eq!(
        server
            .request(
                "GET",
                "/v1/attachments/usage",
                &owner.device_token,
                json!(null),
            )
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    server.shutdown().await;
}

#[tokio::test]
async fn policy_receives_verified_principals_and_cannot_be_tricked_by_foreign_envelopes() {
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
            format!("/v1/attachments/{}/public-copies", Uuid::new_v4()),
            json!({"not": "an image"}),
        ),
        (
            "POST",
            "/v1/vault/key-profiles".to_owned(),
            json!({"key_epoch": 2, "public_key_profile": {}, "encrypted_vault_check_header": "", "profile_fingerprint": "invalid", "admission_public_key": "not-a-key"}),
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
        Operation::Publish,
        Operation::Revoke,
    ] {
        assert_called_by(&calls, &owner, operation);
    }
    assert_called_by(&calls, &deletable, Operation::DeleteVault);
    server.shutdown().await;
}

#[tokio::test]
async fn wake_routes_use_sync_and_revoke_policy_and_delete_matching_jobs() {
    let policy = Arc::new(Policy::allowing([Operation::Sync, Operation::Revoke]));
    let server = TestServer::start(policy.clone()).await;
    let owner = server.owner().await;
    let route_id = Uuid::new_v4();

    assert_eq!(
        server
            .request(
                "PUT",
                "/v1/devices/self/wake-route",
                &owner.device_token,
                json!({"route_id": route_id, "wake_credential": "a".repeat(64)}),
            )
            .await
            .0,
        StatusCode::NO_CONTENT
    );
    sqlx::query("INSERT INTO device_wake_jobs(vault_id,device_id,cursor,job_id,opaque_nonce) VALUES($1,$2,1,$3,'opaque')")
        .bind(owner.vault_id)
        .bind(owner.device_id)
        .bind(Uuid::new_v4())
        .execute(&server.pool)
        .await
        .unwrap();

    assert_eq!(
        server
            .request(
                "DELETE",
                &format!("/v1/devices/self/wake-route?route_id={route_id}"),
                &owner.device_token,
                json!(null),
            )
            .await
            .0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM device_wake_jobs WHERE vault_id=$1 AND device_id=$2",
        )
        .bind(owner.vault_id)
        .bind(owner.device_id)
        .fetch_one(&server.pool)
        .await
        .unwrap(),
        0
    );
    assert_eq!(
        server
            .request(
                "DELETE",
                &format!("/v1/devices/self/wake-route?route_id={route_id}"),
                &owner.device_token,
                json!(null),
            )
            .await
            .0,
        StatusCode::NO_CONTENT
    );
    let calls = policy.calls().await;
    assert_called_by(&calls, &owner, Operation::Sync);
    assert_called_by(&calls, &owner, Operation::Revoke);
    server.shutdown().await;
}

#[tokio::test]
async fn push_config_and_wake_routes_require_device_auth_and_honor_route_fences() {
    let policy = Arc::new(Policy::allowing([
        Operation::Read,
        Operation::Sync,
        Operation::Revoke,
    ]));
    let server = TestServer::start_with_policy_and_push_config(
        policy.clone(),
        5,
        PushConfigAdvertisement {
            available: true,
            providers: vec![PushProvider::Fcm, PushProvider::Apns],
            apns_environment: Some(ApnsEnvironment::Production),
        },
    )
    .await;
    let first = server.owner().await;
    let second = server.owner().await;
    let route_id = Uuid::new_v4();

    let (status, config) = server
        .request(
            "GET",
            "/v1/devices/self/push-config",
            &first.device_token,
            json!(null),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        config,
        json!({"version": 1, "available": true, "providers": ["fcm", "apns"], "apns_environment": "production"})
    );
    assert_eq!(
        server
            .raw_request(
                "GET",
                "/v1/devices/self/push-config",
                "not-a-device-token",
                Body::empty(),
                &[("peppy-account-session", "account-only")],
            )
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        server
            .request(
                "PUT",
                "/v1/devices/self/wake-route",
                &first.device_token,
                json!({"route_id": route_id, "wake_credential": "a".repeat(64)}),
            )
            .await
            .0,
        StatusCode::NO_CONTENT
    );
    sqlx::query("INSERT INTO device_wake_jobs(vault_id,device_id,cursor,job_id,opaque_nonce) VALUES($1,$2,1,$3,'opaque')")
        .bind(first.vault_id)
        .bind(first.device_id)
        .bind(Uuid::new_v4())
        .execute(&server.pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO device_sockets(connection_id,vault_id,device_id,lease_until,responsive) VALUES($1,$2,$3,now() + interval '60 seconds',TRUE)",
    )
    .bind(Uuid::new_v4())
    .bind(first.vault_id)
    .bind(first.device_id)
    .execute(&server.pool)
    .await
    .unwrap();
    assert_eq!(
        server
            .request(
                "DELETE",
                &format!("/v1/devices/self/wake-route?route_id={}", Uuid::new_v4()),
                &first.device_token,
                json!(null),
            )
            .await
            .0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM device_wake_jobs WHERE vault_id=$1 AND device_id=$2"
        )
        .bind(first.vault_id)
        .bind(first.device_id)
        .fetch_one(&server.pool)
        .await
        .unwrap(),
        1
    );
    let roster = device_roster(&server, &first).await;
    assert_eq!(roster["wake_route"], true);
    assert_eq!(roster["presence"]["state"], "connected");
    assert_eq!(
        server
            .request(
                "DELETE",
                &format!("/v1/devices/self/wake-route?route_id={route_id}"),
                &second.device_token,
                json!(null),
            )
            .await
            .0,
        StatusCode::NO_CONTENT
    );
    assert!(!sqlx::query_scalar::<_, bool>("SELECT revoked_at IS NOT NULL FROM device_wake_routes WHERE vault_id=$1 AND device_id=$2")
        .bind(first.vault_id).bind(first.device_id).fetch_one(&server.pool).await.unwrap());
    assert_eq!(
        server
            .request(
                "DELETE",
                &format!("/v1/devices/self/wake-route?route_id={route_id}"),
                &first.device_token,
                json!(null),
            )
            .await
            .0,
        StatusCode::NO_CONTENT
    );
    assert!(sqlx::query_scalar::<_, bool>("SELECT revoked_at IS NOT NULL FROM device_wake_routes WHERE vault_id=$1 AND device_id=$2")
        .bind(first.vault_id).bind(first.device_id).fetch_one(&server.pool).await.unwrap());
    let roster = device_roster(&server, &first).await;
    assert_eq!(
        roster["revoked"], false,
        "wake-route deletion must not revoke the device"
    );
    assert_eq!(roster["wake_route"], false);
    assert_eq!(roster["presence"]["state"], "connected");
    sqlx::query(
        "UPDATE device_credentials SET revoked_at=now() WHERE vault_id=$1 AND device_id=$2",
    )
    .bind(first.vault_id)
    .bind(first.device_id)
    .execute(&server.pool)
    .await
    .unwrap();
    for method in ["PUT", "DELETE"] {
        assert_eq!(
            server
                .request(
                    method,
                    "/v1/devices/self/wake-route",
                    &first.device_token,
                    json!({"route_id": Uuid::new_v4(), "wake_credential": "a".repeat(64)})
                )
                .await
                .0,
            StatusCode::UNAUTHORIZED,
        );
    }
    let calls = policy.calls().await;
    assert_called_by(&calls, &first, Operation::Read);
    assert_called_by(&calls, &first, Operation::Sync);
    assert_called_by(&calls, &first, Operation::Revoke);
    server.shutdown().await;
}

#[tokio::test]
async fn default_push_config_is_unavailable_and_uses_sync_policy() {
    let policy = Arc::new(Policy::allowing([Operation::Sync]));
    let server = TestServer::start(policy.clone()).await;
    let owner = server.owner().await;

    let (status, config) = server
        .request(
            "GET",
            "/v1/devices/self/push-config",
            &owner.device_token,
            json!(null),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        config,
        json!({"version": 1, "available": false, "providers": [], "apns_environment": null})
    );
    assert_called_by(&policy.calls().await, &owner, Operation::Sync);
    server.shutdown().await;
}

#[tokio::test]
async fn policy_attachment_quota_overrides_the_server_default_for_reservations() {
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
    let key = peppy_crypto::FileKey::generate().unwrap();
    let mut ciphertext = Vec::new();
    peppy_crypto::encrypt_stream(Cursor::new(b"x"), &mut ciphertext, &key, b"attachment").unwrap();
    let png = b"\x89PNG\r\n\x1a\n";
    let policy = Arc::new(Policy {
        allowed: vec![Operation::Upload, Operation::Publish],
        quota: Some(ciphertext.len() as i64 + png.len() as i64 - 1),
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
                "declared_ciphertext_bytes": ciphertext.len(),
                "declared_ciphertext_sha256": hex::encode(Sha256::digest(&ciphertext)),
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
                Body::from(ciphertext),
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

async fn insert_retention_candidate(
    pool: &PgPool,
    owner: &peppy_server::api::Credential,
    attachment_id: Uuid,
    object_key: &str,
    bytes: i64,
    finalized_offset_seconds: i64,
) {
    sqlx::query("UPDATE attachment_retention_devices SET cleanup_ready=TRUE,reconciliation_needed=FALSE WHERE vault_id=$1 AND device_id=$2")
        .bind(owner.vault_id)
        .bind(owner.device_id)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO attachments(vault_id,attachment_id,object_key,ciphertext_bytes,ciphertext_sha256,created_by_device_id,retention_class,finalized_at) VALUES($1,$2,$3,$4,$5,$6,'message_media',now()+($7 * interval '1 second'))")
        .bind(owner.vault_id)
        .bind(attachment_id)
        .bind(object_key)
        .bind(bytes)
        .bind(vec![0_u8; 32])
        .bind(owner.device_id)
        .bind(finalized_offset_seconds)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO attachment_retention_uses(vault_id,use_id,attachment_id,device_id,kind) VALUES($1,$2,$3,$4,'message_history')")
        .bind(owner.vault_id)
        .bind(Uuid::new_v4())
        .bind(attachment_id)
        .bind(owner.device_id)
        .execute(pool)
        .await
        .unwrap();
}

async fn cleanup_reserve(
    server: &TestServer,
    owner: &peppy_server::api::Credential,
    bytes: i64,
) -> (StatusCode, Value) {
    server
        .raw_request(
            "POST",
            "/v1/attachments/reserve",
            &owner.device_token,
            Body::from(
                json!({
                    "declared_ciphertext_bytes": bytes,
                    "declared_ciphertext_sha256": hex::encode(Sha256::digest(vec![1_u8; bytes as usize])),
                    "retention_class": "message_media",
                    "reference_tracking": true,
                })
                .to_string(),
            ),
            &[
                (header::CONTENT_TYPE.as_str(), "application/json"),
                ("peppy-attachment-retention-protocol", "1"),
            ],
        )
        .await
}

#[tokio::test]
async fn cleanup_authorized_admission_selects_only_earliest_sufficient_victims() {
    let server =
        TestServer::start_with_policy(Arc::new(CleanupPolicy { limit_bytes: 10 }), 5).await;

    let owner = server.owner().await;
    let earliest = Uuid::new_v4();
    let later = Uuid::new_v4();
    insert_retention_candidate(&server.pool, &owner, earliest, "private/earliest", 4, -10).await;
    insert_retention_candidate(&server.pool, &owner, later, "private/later", 6, -5).await;

    let (status, body) = cleanup_reserve(&server, &owner, 3).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "storage_cleanup_pending");
    let removed: Vec<Uuid> = sqlx::query_scalar(
        "SELECT attachment_id FROM attachment_removals WHERE vault_id=$1 ORDER BY removed_at,attachment_id",
    )
    .bind(owner.vault_id)
    .fetch_all(&server.pool)
    .await
    .unwrap();
    assert_eq!(removed, vec![earliest]);
    let policy_version: Option<String> = sqlx::query_scalar(
        "SELECT policy_version FROM attachment_removals WHERE vault_id=$1 AND attachment_id=$2",
    )
    .bind(owner.vault_id)
    .bind(earliest)
    .fetch_one(&server.pool)
    .await
    .unwrap();
    assert_eq!(policy_version.as_deref(), Some("test-policy"));
    assert!(
        sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM attachments WHERE vault_id=$1 AND attachment_id=$2)"
        )
        .bind(owner.vault_id)
        .bind(later)
        .fetch_one(&server.pool)
        .await
        .unwrap()
    );

    let protected_uses = server.owner().await;
    for (index, kind) in ["pending_send", "contact_photo", "restore_critical"]
        .into_iter()
        .enumerate()
    {
        let attachment_id = Uuid::new_v4();
        insert_retention_candidate(
            &server.pool,
            &protected_uses,
            attachment_id,
            &format!("private/protected-use-{index}"),
            3,
            index as i64,
        )
        .await;
        sqlx::query("INSERT INTO attachment_retention_uses(vault_id,use_id,attachment_id,device_id,kind) VALUES($1,$2,$3,$4,$5)")
            .bind(protected_uses.vault_id).bind(Uuid::new_v4()).bind(attachment_id).bind(protected_uses.device_id).bind(kind)
            .execute(&server.pool).await.unwrap();
    }
    assert_eq!(
        cleanup_reserve(&server, &protected_uses, 2).await.0,
        StatusCode::PAYLOAD_TOO_LARGE
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM attachment_removals WHERE vault_id=$1")
            .bind(protected_uses.vault_id)
            .fetch_one(&server.pool)
            .await
            .unwrap(),
        0
    );

    let oversized = server.owner().await;
    let oversized_candidate = Uuid::new_v4();
    insert_retention_candidate(
        &server.pool,
        &oversized,
        oversized_candidate,
        "private/oversized",
        4,
        -1,
    )
    .await;
    assert_eq!(
        cleanup_reserve(&server, &oversized, 11).await.0,
        StatusCode::PAYLOAD_TOO_LARGE
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM attachment_removals WHERE vault_id=$1")
            .bind(oversized.vault_id)
            .fetch_one(&server.pool)
            .await
            .unwrap(),
        0
    );

    let insufficient = server.owner().await;
    let insufficient_candidate = Uuid::new_v4();
    insert_retention_candidate(
        &server.pool,
        &insufficient,
        insufficient_candidate,
        "private/insufficient",
        4,
        -1,
    )
    .await;
    sqlx::query("INSERT INTO attachments(vault_id,attachment_id,object_key,ciphertext_bytes,ciphertext_sha256,created_by_device_id) VALUES($1,$2,'private/protected',8,$3,$4)")
        .bind(insufficient.vault_id).bind(Uuid::new_v4()).bind(vec![0_u8; 32]).bind(insufficient.device_id).execute(&server.pool).await.unwrap();
    assert_eq!(
        cleanup_reserve(&server, &insufficient, 3).await.0,
        StatusCode::PAYLOAD_TOO_LARGE
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM attachment_removals WHERE vault_id=$1")
            .bind(insufficient.vault_id)
            .fetch_one(&server.pool)
            .await
            .unwrap(),
        0
    );

    server.shutdown().await;
}

#[tokio::test]
async fn retention_capability_requires_explicit_reconciliation_and_legacy_events_withdraw_it() {
    let server = TestServer::start(Arc::new(Policy::allowing([Operation::Send]))).await;
    let owner = server.owner().await;
    let device = server.add_device(&owner).await;
    let (_, fingerprint) = profile(device.vault_id, 1);

    let initial: (bool, bool) = sqlx::query_as("SELECT cleanup_ready,reconciliation_needed FROM attachment_retention_devices WHERE vault_id=$1 AND device_id=$2")
        .bind(device.vault_id).bind(device.device_id).fetch_one(&server.pool).await.unwrap();
    assert_eq!(initial, (false, true));

    assert_eq!(
        server
            .request(
                "PUT",
                "/v1/devices/self/retention-capability",
                &device.device_token,
                json!({"protocol_version":1,"cleanup_ready":true})
            )
            .await
            .0,
        StatusCode::NO_CONTENT
    );
    let ready: (bool, bool) = sqlx::query_as("SELECT cleanup_ready,reconciliation_needed FROM attachment_retention_devices WHERE vault_id=$1 AND device_id=$2")
        .bind(device.vault_id).bind(device.device_id).fetch_one(&server.pool).await.unwrap();
    assert_eq!(ready, (true, false));

    assert_eq!(
        server
            .request(
                "POST",
                "/v1/events",
                &device.device_token,
                event(device.vault_id, device.device_id, &fingerprint)
            )
            .await
            .0,
        StatusCode::OK
    );
    let withdrawn: (bool, bool) = sqlx::query_as("SELECT cleanup_ready,reconciliation_needed FROM attachment_retention_devices WHERE vault_id=$1 AND device_id=$2")
        .bind(device.vault_id).bind(device.device_id).fetch_one(&server.pool).await.unwrap();
    assert_eq!(withdrawn, (false, true));

    server.shutdown().await;
}

async fn insert_finalized_attachment(
    server: &TestServer,
    credential: &peppy_server::api::Credential,
) -> Uuid {
    let attachment_id = Uuid::new_v4();
    sqlx::query("INSERT INTO attachments(vault_id,attachment_id,object_key,ciphertext_bytes,ciphertext_sha256,created_by_device_id,retention_class) VALUES($1,$2,$3,1,$4,$5,'message_media')")
        .bind(credential.vault_id)
        .bind(attachment_id)
        .bind(format!("private/{attachment_id}"))
        .bind(vec![0_u8; 32])
        .bind(credential.device_id)
        .execute(&server.pool)
        .await
        .unwrap();
    attachment_id
}

#[tokio::test]
async fn retention_use_routes_enforce_identity_immutability_and_terminal_release() {
    let server = TestServer::start(Arc::new(Policy::allowing([Operation::Upload]))).await;
    let owner = server.owner().await;
    let device = server.add_device(&owner).await;
    let foreign = server.add_device(&owner).await;
    let attachment_id = insert_finalized_attachment(&server, &device).await;
    assert_eq!(
        server
            .request(
                "PUT",
                "/v1/devices/self/retention-capability",
                &device.device_token,
                json!({"protocol_version":1,"cleanup_ready":true})
            )
            .await
            .0,
        StatusCode::NO_CONTENT
    );
    let use_id = Uuid::new_v4();
    let path = format!("/v1/attachments/{attachment_id}/retention-uses");
    let register = json!({"use_id":use_id,"attachment_id":attachment_id,"device_id":device.device_id,"kind":"pending_send"});

    let response = server
        .raw_request(
            "POST",
            &path,
            &device.device_token,
            Body::from(register.to_string()),
            &[(header::CONTENT_TYPE.as_str(), "application/json")],
        )
        .await;
    assert_eq!(response.0, StatusCode::OK, "{}", response.1);
    assert_eq!(response.1["state"], "registered");
    let readiness: (bool, bool) = sqlx::query_as("SELECT cleanup_ready,reconciliation_needed FROM attachment_retention_devices WHERE vault_id=$1 AND device_id=$2")
        .bind(device.vault_id).bind(device.device_id).fetch_one(&server.pool).await.unwrap();
    assert_eq!(readiness, (false, true));

    let conflict = json!({"use_id":use_id,"attachment_id":attachment_id,"device_id":device.device_id,"kind":"contact_photo"});
    assert_eq!(
        server
            .raw_request(
                "POST",
                &path,
                &device.device_token,
                Body::from(conflict.to_string()),
                &[
                    (header::CONTENT_TYPE.as_str(), "application/json"),
                    ("peppy-attachment-retention-protocol", "1")
                ]
            )
            .await
            .0,
        StatusCode::CONFLICT
    );

    let wrong_path = format!("/v1/attachments/{}/retention-uses", Uuid::new_v4());
    assert_eq!(
        server
            .raw_request(
                "POST",
                &wrong_path,
                &device.device_token,
                Body::from(register.to_string()),
                &[(header::CONTENT_TYPE.as_str(), "application/json")]
            )
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    let foreign_body =
        json!({"use_id":use_id,"attachment_id":attachment_id,"device_id":device.device_id});
    let release_path = format!("{path}/{use_id}/release");
    assert_eq!(
        server
            .raw_request(
                "POST",
                &release_path,
                &foreign.device_token,
                Body::from(foreign_body.to_string()),
                &[(header::CONTENT_TYPE.as_str(), "application/json")]
            )
            .await
            .0,
        StatusCode::FORBIDDEN
    );

    let release =
        json!({"use_id":use_id,"attachment_id":attachment_id,"device_id":device.device_id});
    assert_eq!(
        server
            .raw_request(
                "POST",
                &release_path,
                &device.device_token,
                Body::from(release.to_string()),
                &[
                    (header::CONTENT_TYPE.as_str(), "application/json"),
                    ("peppy-attachment-retention-protocol", "1")
                ]
            )
            .await
            .0,
        StatusCode::OK
    );
    let replay = server
        .raw_request(
            "POST",
            &path,
            &device.device_token,
            Body::from(register.to_string()),
            &[
                (header::CONTENT_TYPE.as_str(), "application/json"),
                ("peppy-attachment-retention-protocol", "1"),
            ],
        )
        .await;
    assert_eq!(replay.0, StatusCode::OK);
    assert_eq!(replay.1["state"], "released");
    server.shutdown().await;
}

#[tokio::test]
async fn removed_attachment_id_is_gone_on_every_authorization_path() {
    let server = TestServer::start(Arc::new(Policy::allowing([
        Operation::Read,
        Operation::Upload,
        Operation::Publish,
        Operation::DeleteVault,
    ])))
    .await;
    let owner = server.owner().await;
    let attachment_id = Uuid::new_v4();
    sqlx::query("INSERT INTO attachment_removals(vault_id,attachment_id,object_key,ciphertext_bytes,policy_version,purged_at) VALUES($1,$2,'private/removed',1,'test',now())")
        .bind(owner.vault_id).bind(attachment_id).execute(&server.pool).await.unwrap();
    let protocol = [("peppy-attachment-retention-protocol", "1")];
    let json_headers = [
        (header::CONTENT_TYPE.as_str(), "application/json"),
        ("peppy-attachment-retention-protocol", "1"),
    ];
    let hash = hex::encode(Sha256::digest(b"x"));
    let cases = [
        (
            "POST",
            "/v1/attachments/reserve".to_owned(),
            json!({"attachment_id":attachment_id,"declared_ciphertext_bytes":1,"declared_ciphertext_sha256":hash}),
            json_headers.as_slice(),
        ),
        (
            "POST",
            format!("/v1/attachments/{attachment_id}/finalize"),
            Value::Null,
            protocol.as_slice(),
        ),
        (
            "GET",
            format!("/v1/attachments/{attachment_id}"),
            Value::Null,
            protocol.as_slice(),
        ),
        (
            "POST",
            format!("/v1/attachments/{attachment_id}/references"),
            json!({"references":[{"producer_device_id":owner.device_id.to_string(),"producer_sequence":"1"}]}),
            json_headers.as_slice(),
        ),
        (
            "POST",
            format!("/v1/attachments/{attachment_id}/public-copies"),
            Value::Null,
            protocol.as_slice(),
        ),
        (
            "POST",
            format!("/v1/attachments/{attachment_id}/retention-uses"),
            json!({"use_id":Uuid::new_v4(),"attachment_id":attachment_id,"device_id":owner.device_id,"kind":"message_history"}),
            json_headers.as_slice(),
        ),
    ];
    for (method, path, body, headers) in cases {
        let response = server
            .raw_request(
                method,
                &path,
                &owner.device_token,
                Body::from(body.to_string()),
                headers,
            )
            .await;
        assert_eq!(
            response.0,
            StatusCode::GONE,
            "{method} {path}: {}",
            response.1
        );
    }
    let upload = server
        .raw_request(
            "PUT",
            &format!("/v1/attachments/{attachment_id}/upload"),
            &owner.device_token,
            Body::from("x"),
            &protocol,
        )
        .await;
    assert_eq!(upload.0, StatusCode::GONE, "{}", upload.1);
    let deleted = server
        .request(
            "DELETE",
            "/v1/vault",
            &owner.device_token,
            json!({"vault_id":owner.vault_id}),
        )
        .await;
    assert_eq!(deleted.0, StatusCode::NO_CONTENT, "{}", deleted.1);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM attachment_removals WHERE vault_id=$1")
            .bind(owner.vault_id)
            .fetch_one(&server.pool)
            .await
            .unwrap(),
        0
    );
    server.shutdown().await;
}

#[tokio::test]
async fn concurrent_cleanup_admissions_select_one_stable_prefix() {
    let server =
        TestServer::start_with_policy(Arc::new(CleanupPolicy { limit_bytes: 12 }), 5).await;
    let owner = server.owner().await;
    let candidates = [Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4()];
    for (index, attachment_id) in candidates.into_iter().enumerate() {
        insert_retention_candidate(
            &server.pool,
            &owner,
            attachment_id,
            &format!("private/concurrent-{index}"),
            4,
            index as i64,
        )
        .await;
    }
    let (small, large) = tokio::join!(
        cleanup_reserve(&server, &owner, 3),
        cleanup_reserve(&server, &owner, 5)
    );
    assert_eq!(small.0, StatusCode::CONFLICT, "{}", small.1);
    assert_eq!(large.0, StatusCode::CONFLICT, "{}", large.1);
    assert_eq!(small.1["code"], "storage_cleanup_pending");
    assert_eq!(large.1["code"], "storage_cleanup_pending");
    let removed: Vec<Uuid> = sqlx::query_scalar(
        "SELECT attachment_id FROM attachment_removals WHERE vault_id=$1 ORDER BY object_key",
    )
    .bind(owner.vault_id)
    .fetch_all(&server.pool)
    .await
    .unwrap();
    assert!(
        removed == candidates[..1] || removed == candidates[..2],
        "only the winning admission's sufficient prefix may be selected: {removed:?}"
    );
    server.shutdown().await;
}

#[tokio::test]
async fn community_device_attachment_notice_routes_are_explicitly_unsupported() {
    let server =
        TestServer::start_with_policy(Arc::new(peppy_server::api::CommunityAccessPolicy), 5).await;
    let device = server.owner().await;
    let (get_status, get_body) = server
        .request(
            "GET",
            "/v1/attachments/storage/status",
            &device.device_token,
            json!(null),
        )
        .await;
    assert_eq!(get_status, StatusCode::NOT_IMPLEMENTED);
    assert_eq!(
        get_body["code"],
        peppy_hosted_api::attachments::error_codes::UNSUPPORTED
    );
    let (post_status, post_body) = server
        .request(
            "POST",
            "/v1/attachments/storage/notice/acknowledgement",
            &device.device_token,
            json!({"policy_version": "hosted_attachment_retention_v1"}),
        )
        .await;
    assert_eq!(post_status, StatusCode::NOT_IMPLEMENTED);
    assert_eq!(
        post_body["code"],
        peppy_hosted_api::attachments::error_codes::UNSUPPORTED
    );
    server.shutdown().await;
}

struct DeviceRetentionErrorPolicy;

impl AccessPolicy for DeviceRetentionErrorPolicy {
    fn authorize(
        &self,
        _principal: AccessPrincipal,
        _operation: Operation,
    ) -> PolicyFuture<'_, bool> {
        Box::pin(async { true })
    }

    fn device_attachment_storage_status(
        &self,
        _principal: AccessPrincipal,
    ) -> PolicyFuture<
        '_,
        Result<
            peppy_hosted_api::attachments::AttachmentStorageStatusResponse,
            peppy_server::api::DeviceAttachmentRetentionError,
        >,
    > {
        Box::pin(async { Err(peppy_server::api::DeviceAttachmentRetentionError::Forbidden) })
    }

    fn acknowledge_device_attachment_retention(
        &self,
        _principal: AccessPrincipal,
        _request: peppy_hosted_api::attachments::AcknowledgeRetentionNoticeRequest,
    ) -> PolicyFuture<'_, Result<(), peppy_server::api::DeviceAttachmentRetentionError>> {
        Box::pin(async { Err(peppy_server::api::DeviceAttachmentRetentionError::Unavailable) })
    }
}

#[tokio::test]
async fn device_attachment_notice_http_errors_use_canonical_forbidden_and_unavailable_codes() {
    let server = TestServer::start_with_policy(Arc::new(DeviceRetentionErrorPolicy), 5).await;
    let device = server.owner().await;
    let (get_status, get_body) = server
        .request(
            "GET",
            "/v1/attachments/storage/status",
            &device.device_token,
            json!(null),
        )
        .await;
    assert_eq!(get_status, StatusCode::FORBIDDEN);
    assert_eq!(
        get_body["code"],
        peppy_hosted_api::attachments::error_codes::FORBIDDEN
    );
    let (post_status, post_body) = server
        .request(
            "POST",
            "/v1/attachments/storage/notice/acknowledgement",
            &device.device_token,
            json!({"policy_version": "hosted_attachment_retention_v1"}),
        )
        .await;
    assert_eq!(post_status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        post_body["code"],
        peppy_hosted_api::attachments::error_codes::UNAVAILABLE
    );
    server.shutdown().await;
}

#[tokio::test]
async fn full_quota_restart_preregisters_protection_before_reserve_and_finalize() {
    let key = peppy_crypto::FileKey::generate().unwrap();
    let mut ciphertext = Vec::new();
    peppy_crypto::encrypt_stream(
        Cursor::new(b"future attachment"),
        &mut ciphertext,
        &key,
        b"attachment",
    )
    .unwrap();
    let limit = ciphertext.len() as i64;
    let server =
        TestServer::start_with_policy(Arc::new(CleanupPolicy { limit_bytes: limit }), 5).await;
    let owner = server.owner().await;
    let old = Uuid::new_v4();
    insert_retention_candidate(&server.pool, &owner, old, "private/full-old", limit, -10).await;

    assert_eq!(
        server
            .request(
                "PUT",
                "/v1/devices/self/retention-capability",
                &owner.device_token,
                json!({"protocol_version":1,"cleanup_ready":false})
            )
            .await
            .0,
        StatusCode::NO_CONTENT
    );
    let future = Uuid::new_v4();
    let history_use = Uuid::new_v4();
    let pending_use = Uuid::new_v4();
    for (use_id, kind) in [
        (history_use, "message_history"),
        (pending_use, "pending_send"),
    ] {
        let response = server.raw_request(
            "POST",
            &format!("/v1/attachments/{future}/retention-uses"),
            &owner.device_token,
            Body::from(json!({"use_id":use_id,"attachment_id":future,"device_id":owner.device_id,"kind":kind}).to_string()),
            &[(header::CONTENT_TYPE.as_str(), "application/json"), ("peppy-attachment-retention-protocol", "1")],
        ).await;
        assert_eq!(response.0, StatusCode::OK, "{kind}: {}", response.1);
        assert_eq!(response.1["state"], "registered");
    }
    assert!(!sqlx::query_scalar::<_, bool>("SELECT EXISTS(SELECT 1 FROM attachments WHERE vault_id=$1 AND attachment_id=$2) OR EXISTS(SELECT 1 FROM upload_reservations WHERE vault_id=$1 AND attachment_id=$2)")
        .bind(owner.vault_id).bind(future).fetch_one(&server.pool).await.unwrap());

    assert_eq!(
        server
            .request(
                "PUT",
                "/v1/devices/self/retention-capability",
                &owner.device_token,
                json!({"protocol_version":1,"cleanup_ready":true})
            )
            .await
            .0,
        StatusCode::NO_CONTENT
    );
    let reserve_body = json!({
        "attachment_id": future,
        "declared_ciphertext_bytes": ciphertext.len(),
        "declared_ciphertext_sha256": hex::encode(Sha256::digest(&ciphertext)),
        "retention_class": "message_media",
        "reference_tracking": true,
    });
    let first = server
        .raw_request(
            "POST",
            "/v1/attachments/reserve",
            &owner.device_token,
            Body::from(reserve_body.to_string()),
            &[
                (header::CONTENT_TYPE.as_str(), "application/json"),
                ("peppy-attachment-retention-protocol", "1"),
            ],
        )
        .await;
    assert_eq!(first.0, StatusCode::CONFLICT, "{}", first.1);
    assert_eq!(first.1["code"], "storage_cleanup_pending");

    sqlx::query(
        "UPDATE attachment_removals SET purged_at=now() WHERE vault_id=$1 AND attachment_id=$2",
    )
    .bind(owner.vault_id)
    .bind(old)
    .execute(&server.pool)
    .await
    .unwrap();
    sqlx::query(
        "DELETE FROM storage_deletions WHERE vault_id=$1 AND object_key='private/full-old'",
    )
    .bind(owner.vault_id)
    .execute(&server.pool)
    .await
    .unwrap();
    let retry = server
        .raw_request(
            "POST",
            "/v1/attachments/reserve",
            &owner.device_token,
            Body::from(reserve_body.to_string()),
            &[
                (header::CONTENT_TYPE.as_str(), "application/json"),
                ("peppy-attachment-retention-protocol", "1"),
            ],
        )
        .await;
    assert_eq!(retry.0, StatusCode::OK, "{}", retry.1);
    assert_eq!(
        server
            .raw_request(
                "PUT",
                &format!("/v1/attachments/{future}/upload"),
                &owner.device_token,
                Body::from(ciphertext.clone()),
                &[("peppy-attachment-retention-protocol", "1")]
            )
            .await
            .0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        server
            .raw_request(
                "POST",
                &format!("/v1/attachments/{future}/finalize"),
                &owner.device_token,
                Body::empty(),
                &[("peppy-attachment-retention-protocol", "1")]
            )
            .await
            .0,
        StatusCode::OK
    );

    let legacy = Uuid::new_v4();
    sqlx::query("INSERT INTO attachments(vault_id,attachment_id,object_key,ciphertext_bytes,ciphertext_sha256,created_by_device_id) VALUES($1,$2,'private/legacy-protected',1,$3,$4)")
        .bind(owner.vault_id).bind(legacy).bind(vec![0_u8;32]).bind(owner.device_id).execute(&server.pool).await.unwrap();
    let blocked = cleanup_reserve(&server, &owner, 1).await;
    assert_eq!(blocked.0, StatusCode::PAYLOAD_TOO_LARGE, "{}", blocked.1);
    let classes: Vec<String> = sqlx::query_scalar("SELECT retention_class FROM attachments WHERE vault_id=$1 AND attachment_id=ANY($2) ORDER BY attachment_id")
        .bind(owner.vault_id).bind([future, legacy]).fetch_all(&server.pool).await.unwrap();
    assert!(classes.contains(&"message_media".to_owned()));
    assert!(classes.contains(&"protected".to_owned()));
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM attachment_removals WHERE vault_id=$1 AND attachment_id=$2"
        )
        .bind(owner.vault_id)
        .bind(future)
        .fetch_one(&server.pool)
        .await
        .unwrap(),
        0
    );

    let tombstone_retry = server.raw_request("POST", "/v1/attachments/reserve", &owner.device_token, Body::from(json!({"attachment_id":old,"declared_ciphertext_bytes":1,"declared_ciphertext_sha256":hex::encode(Sha256::digest(b"x"))}).to_string()), &[(header::CONTENT_TYPE.as_str(), "application/json"), ("peppy-attachment-retention-protocol", "1")]).await;
    assert_eq!(tombstone_retry.0, StatusCode::GONE);

    let object_key: String = sqlx::query_scalar(
        "SELECT object_key FROM attachments WHERE vault_id=$1 AND attachment_id=$2",
    )
    .bind(owner.vault_id)
    .bind(future)
    .fetch_one(&server.pool)
    .await
    .unwrap();
    let storage =
        peppy_server::storage::Storage::new(Config::from_env().unwrap().s3.as_ref().unwrap());
    storage.delete(&object_key).await.unwrap();
    server.shutdown().await;
}
