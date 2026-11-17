use std::{
    sync::{
        Arc,
        atomic::{AtomicU16, Ordering},
    },
    time::Duration,
};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use futures_util::{SinkExt, StreamExt};
use peppy_crypto::{KeyProfile, derive_root_key, enrollment::derive_admission_keypair};
use peppy_hosted_api::PresenceState;
use peppy_protocol::Envelope;
use peppy_server::api::{
    TransportOptions, compact_records, create_owner, prune_replay_log, router_with_options,
    router_with_options_and_relay,
};
use peppy_server::{presence, scope};
use reqwest::{Client, StatusCode};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{PgPool, postgres::PgPoolOptions};
use tokio::{net::TcpListener, task::JoinHandle};
use tokio_tungstenite::{connect_async, tungstenite};
use url::Url;
use uuid::Uuid;

#[tokio::test]
async fn presence_leases_require_a_responsive_pong_and_cleanup_on_close() {
    let server = TestServer::start().await;
    let connection = Uuid::new_v4();
    let mut tx = scope::begin(&server.pool, server.vault).await.unwrap();
    presence::open(&mut tx, server.vault, server.owner_device, connection)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let responsive: bool = sqlx::query_scalar(
        "SELECT responsive FROM device_sockets WHERE vault_id=$1 AND device_id=$2 AND connection_id=$3",
    )
    .bind(server.vault)
    .bind(server.owner_device)
    .bind(connection)
    .fetch_one(&server.pool)
    .await
    .unwrap();
    assert!(!responsive);

    let mut tx = scope::begin(&server.pool, server.vault).await.unwrap();
    assert!(
        presence::renew(
            &mut tx,
            server.vault,
            server.owner_device,
            connection,
            presence::LEASE_SECONDS,
        )
        .await
        .unwrap()
    );
    tx.commit().await.unwrap();
    let touched: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM device_presence WHERE vault_id=$1 AND device_id=$2)",
    )
    .bind(server.vault)
    .bind(server.owner_device)
    .fetch_one(&server.pool)
    .await
    .unwrap();
    assert!(touched);
    sqlx::query("UPDATE device_sockets SET lease_until=now() - interval '1 second' WHERE vault_id=$1 AND device_id=$2 AND connection_id=$3")
        .bind(server.vault)
        .bind(server.owner_device)
        .bind(connection)
        .execute(&server.pool)
        .await
        .unwrap();
    let mut tx = scope::begin(&server.pool, server.vault).await.unwrap();
    assert!(
        !presence::renew(
            &mut tx,
            server.vault,
            server.owner_device,
            connection,
            presence::LEASE_SECONDS,
        )
        .await
        .unwrap()
    );
    tx.commit().await.unwrap();
    let renewed: bool = sqlx::query_scalar("SELECT lease_until > now() FROM device_sockets WHERE vault_id=$1 AND device_id=$2 AND connection_id=$3")
        .bind(server.vault)
        .bind(server.owner_device)
        .bind(connection)
        .fetch_one(&server.pool)
        .await
        .unwrap();
    assert!(
        renewed,
        "a non-broadcasting renewal must still extend the lease"
    );
    assert!(
        presence::close(&server.pool, server.vault, connection)
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn presence_caps_sockets_and_revocation_invalidates_renewal() {
    let server = TestServer::start().await;
    let mut connections = Vec::new();
    for _ in 0..32 {
        let connection = Uuid::new_v4();
        let mut tx = scope::begin(&server.pool, server.vault).await.unwrap();
        presence::open(&mut tx, server.vault, server.owner_device, connection)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        connections.push(connection);
    }
    let overflow = Uuid::new_v4();
    let mut tx = scope::begin(&server.pool, server.vault).await.unwrap();
    presence::open(&mut tx, server.vault, server.owner_device, overflow)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM device_sockets WHERE vault_id=$1 AND device_id=$2",
    )
    .bind(server.vault)
    .bind(server.owner_device)
    .fetch_one(&server.pool)
    .await
    .unwrap();
    assert_eq!(count, 32);

    sqlx::query("UPDATE devices SET revoked_at=now() WHERE vault_id=$1 AND device_id=$2")
        .bind(server.vault)
        .bind(server.owner_device)
        .execute(&server.pool)
        .await
        .unwrap();
    let mut tx = scope::begin(&server.pool, server.vault).await.unwrap();
    assert!(
        !presence::renew(
            &mut tx,
            server.vault,
            server.owner_device,
            connections[0],
            presence::LEASE_SECONDS,
        )
        .await
        .unwrap()
    );
}

#[tokio::test]
async fn presence_batch_and_direct_lookup_keep_never_seen_devices_never() {
    let server = TestServer::start().await;
    let batch = presence::lookup_many_best_effort(
        &server.pool,
        server.vault,
        &[(server.owner_device, false)],
    )
    .await;
    assert_eq!(batch[&server.owner_device].state, PresenceState::Never);
    assert_eq!(batch[&server.owner_device].last_seen_age_seconds, None);

    let mut tx = scope::begin(&server.pool, server.vault).await.unwrap();
    let direct = presence::lookup(&mut tx, server.vault, server.owner_device, false)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(direct.state, PresenceState::Never);
    assert_eq!(direct.last_seen_age_seconds, None);
}

struct TestServer {
    pool: PgPool,
    admin: PgPool,
    schema: String,
    base_url: String,
    ws_url: String,
    task: JoinHandle<()>,
    owner_token: String,
    owner_device: Uuid,
    vault: Uuid,
    fingerprint: String,
}

impl TestServer {
    async fn start() -> Self {
        Self::start_with(TransportOptions::default()).await
    }

    async fn start_with(options: TransportOptions) -> Self {
        Self::start_custom(options, None, None).await
    }

    /// A vault whose owner profile/header are real `peppy-crypto` values, so a real
    /// `peppy_client_core::Client` (device = owner) can encrypt for it.
    async fn start_real(
        profile: &peppy_crypto::KeyProfile,
        header: &peppy_crypto::VaultCheckHeader,
    ) -> Self {
        let real = (
            serde_json::to_value(profile).unwrap(),
            profile.fingerprint().unwrap(),
            serde_json::to_vec(header).unwrap(),
            profile.vault_id,
        );
        Self::start_custom(TransportOptions::default(), Some(real), None).await
    }

    async fn start_custom(
        options: TransportOptions,
        real: Option<(Value, String, Vec<u8>, Uuid)>,
        relay_url: Option<Url>,
    ) -> Self {
        Self::start_custom_mode(options, real, relay_url, true).await
    }

    async fn start_custom_mode(
        options: TransportOptions,
        real: Option<(Value, String, Vec<u8>, Uuid)>,
        relay_url: Option<Url>,
        start_maintenance: bool,
    ) -> Self {
        let database_url = std::env::var("TEST_DATABASE_URL")
            .expect("TEST_DATABASE_URL is required; integration tests never silently skip");
        let admin = PgPoolOptions::new()
            .max_connections(2)
            .connect(&database_url)
            .await
            .unwrap();
        let schema = format!("peppy_server_test_{}", Uuid::new_v4().simple());
        sqlx::query(&format!("CREATE SCHEMA {schema}"))
            .execute(&admin)
            .await
            .unwrap();

        let mut isolated_url: Url = database_url.parse().unwrap();
        isolated_url
            .query_pairs_mut()
            .append_pair("options", &format!("-csearch_path={schema}"));
        let pool = PgPoolOptions::new()
            .max_connections(5)
            .connect(isolated_url.as_str())
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();

        let (vault, profile, fingerprint, header) = match real {
            Some((profile, fingerprint, header, vault)) => (vault, profile, fingerprint, header),
            None => {
                let vault = Uuid::new_v4();
                let (profile, fingerprint) = profile(vault, 1);
                (vault, profile, fingerprint, vec![7, 8, 9])
            }
        };
        let owner = create_owner(&pool, profile, header, fingerprint.clone(), 1)
            .await
            .unwrap();

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server_pool = pool.clone();
        let task = tokio::spawn(async move {
            let router = if start_maintenance {
                match relay_url {
                    Some(relay_url) => {
                        router_with_options_and_relay(server_pool, options, relay_url)
                    }
                    None => router_with_options(server_pool, options),
                }
            } else {
                let config = peppy_server::config::Config {
                    bind_addr: addr,
                    database_url: isolated_url.into(),
                    release_identity: "test".into(),
                    s3: None,
                    public_api_url: None,
                    vault_attachment_quota_bytes: 512 * 1024 * 1024,
                    trusted_proxy_cidrs: Vec::new(),
                    replay_retention: options.replay_retention,
                    relay_url,
                    web_client: None,
                    web_live_trusted_origin: None,
                    mcp_worker_url: None,
                    mcp_worker_token_file: None,
                };
                peppy_server::ServerBuilder::new(config, server_pool)
                    .build()
                    .await
                    .unwrap()
                    .router()
            };
            axum::serve(listener, router).await.unwrap();
        });
        Self {
            pool,
            admin,
            schema,
            base_url: format!("http://{addr}"),
            ws_url: format!("ws://{addr}/v1/ws"),
            task,
            owner_token: owner.device_token,
            owner_device: owner.device_id,
            vault,
            fingerprint,
        }
    }

    async fn start_with_relay(relay_url: Url) -> Self {
        Self::start_custom(TransportOptions::default(), None, Some(relay_url)).await
    }

    fn auth(&self, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        request.bearer_auth(&self.owner_token)
    }

    async fn provision_device(&self, role: &str) -> TestDevice {
        self.provision_device_for(self.vault, &self.fingerprint, role)
            .await
    }

    async fn provision_device_for(&self, vault: Uuid, fingerprint: &str, role: &str) -> TestDevice {
        let id = Uuid::new_v4();
        let token = (0..3)
            .map(|_| Uuid::new_v4().simple().to_string())
            .collect::<String>();
        sqlx::query("INSERT INTO devices(vault_id,device_id,role,public_key,profile_fingerprint,key_epoch) VALUES($1,$2,$3,$4,$5,1)")
            .bind(vault)
            .bind(id)
            .bind(role)
            .bind(json!({"fixture": true}))
            .bind(fingerprint)
            .execute(&self.pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO device_credentials(token_digest,vault_id,device_id) VALUES($1,$2,$3)",
        )
        .bind(Sha256::digest(token.as_bytes()).as_slice())
        .bind(vault)
        .bind(id)
        .execute(&self.pool)
        .await
        .unwrap();
        TestDevice { id, token }
    }

    async fn shutdown(self) {
        self.task.abort();
        let _ = self.task.await;
        self.pool.close().await;
        let drop_schema = format!("DROP SCHEMA {} CASCADE", self.schema);
        let first_attempt = sqlx::query(&drop_schema).execute(&self.admin).await;
        if matches!(
            &first_attempt,
            Err(sqlx::Error::Database(error)) if error.code().as_deref() == Some("40P01")
        ) {
            sqlx::query(&drop_schema)
                .execute(&self.admin)
                .await
                .unwrap();
        } else {
            first_attempt.unwrap();
        }
        self.admin.close().await;
    }
}

struct TestDevice {
    id: Uuid,
    token: String,
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

fn admission_public_key(vault: Uuid, epoch: u32) -> String {
    let profile = KeyProfile::new(vault, epoch).unwrap();
    let root = derive_root_key("correct horse battery staple", &profile).unwrap();
    let admission = derive_admission_keypair(&root).unwrap();
    URL_SAFE_NO_PAD.encode(admission.public_key())
}

fn event(vault: Uuid, producer: Uuid, sequence: u64, envelope: Uuid, payload: u8) -> Value {
    json!({
        "protocol_version": 1,
        "envelope_id": envelope,
        "command_id": null,
        "vault_id": vault,
        "producer_device_id": producer,
        "producer_sequence": sequence.to_string(),
        "key_epoch": 1,
        "crypto_suite": 1,
        "profile_fingerprint": "PLACEHOLDER",
        "purpose": "event",
        "route": null,
        "ciphertext": base64::engine::general_purpose::STANDARD.encode([payload]),
    })
}

fn compacting_event(
    vault: Uuid,
    producer: Uuid,
    sequence: u64,
    envelope: Uuid,
    supersedes: Vec<(Uuid, u64)>,
) -> Value {
    let mut value = event(vault, producer, sequence, envelope, sequence as u8);
    value["compaction"] = json!({
        "key": base64::engine::general_purpose::STANDARD.encode([7_u8; 32]),
        "terminal": false,
        "supersedes": supersedes.into_iter().map(|(producer_device_id, producer_sequence)| json!({
            "producer_device_id": producer_device_id,
            "producer_sequence": producer_sequence.to_string(),
        })).collect::<Vec<_>>(),
    });
    value
}

fn command(
    vault: Uuid,
    producer: Uuid,
    sequence: u64,
    envelope: Uuid,
    command: Uuid,
    gateway: Uuid,
) -> Value {
    json!({
        "protocol_version": 1,
        "envelope_id": envelope,
        "command_id": command,
        "vault_id": vault,
        "producer_device_id": producer,
        "producer_sequence": sequence.to_string(),
        "key_epoch": 1,
        "crypto_suite": 1,
        "profile_fingerprint": "PLACEHOLDER",
        "purpose": "command",
        "route": {"gateway_device_id": gateway, "subscription_id": "sim:test"},
        "ciphertext": base64::engine::general_purpose::STANDARD.encode([9, 8, 7]),
    })
}

fn rcs_link_command(
    vault: Uuid,
    producer: Uuid,
    sequence: u64,
    envelope: Uuid,
    command_id: Uuid,
    gateway: Uuid,
) -> Value {
    let mut envelope = command(vault, producer, sequence, envelope, command_id, gateway);
    envelope["route"] = json!({"kind": "rcs_link", "gateway_device_id": gateway});
    envelope
}

#[test]
fn recorded_carrier_envelope_round_trips_at_server_boundary() {
    let recorded = json!({
        "protocol_version": 1,
        "envelope_id": "00000000-0000-0000-0000-000000000001",
        "command_id": "00000000-0000-0000-0000-000000000002",
        "vault_id": "00000000-0000-0000-0000-000000000003",
        "producer_device_id": "00000000-0000-0000-0000-000000000004",
        "producer_sequence": "9",
        "key_epoch": 2,
        "crypto_suite": 3,
        "profile_fingerprint": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "purpose": "command",
        "route": {
            "gateway_device_id": "00000000-0000-0000-0000-000000000005",
            "subscription_id": "sim:fixture"
        },
        "ciphertext": "AQID"
    });

    let envelope: Envelope = serde_json::from_value(recorded.clone()).unwrap();

    assert_eq!(serde_json::to_value(envelope).unwrap(), recorded);
}

fn set_fingerprint(mut envelope: Value, fingerprint: &str) -> Value {
    envelope["profile_fingerprint"] = json!(fingerprint);
    envelope
}

fn with_origin_tag(mut envelope: Value, tag: [u8; 32]) -> Value {
    envelope["origin_tag"] = json!(base64::engine::general_purpose::STANDARD.encode(tag));
    envelope
}

#[derive(Clone)]
struct FakeRelay {
    status: Arc<AtomicU16>,
    requests: Arc<tokio::sync::Mutex<Vec<Value>>>,
    hold_response: Arc<std::sync::atomic::AtomicBool>,
    release_response: Arc<tokio::sync::Notify>,
}

async fn fake_wake(
    axum::extract::State(relay): axum::extract::State<FakeRelay>,
    axum::Json(body): axum::Json<Value>,
) -> axum::http::StatusCode {
    relay.requests.lock().await.push(body);
    if relay.hold_response.load(Ordering::SeqCst) {
        relay.release_response.notified().await;
    }
    axum::http::StatusCode::from_u16(relay.status.load(Ordering::SeqCst)).unwrap()
}

async fn start_fake_relay() -> (Url, FakeRelay, JoinHandle<()>) {
    let relay = FakeRelay {
        status: Arc::new(AtomicU16::new(202)),
        requests: Arc::new(tokio::sync::Mutex::new(Vec::new())),
        hold_response: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        release_response: Arc::new(tokio::sync::Notify::new()),
    };
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let router = axum::Router::new()
        .route(
            "/relay/v1/routes/{route}/wake",
            axum::routing::post(fake_wake),
        )
        .with_state(relay.clone());
    let task = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    (
        format!("http://{address}/relay").parse().unwrap(),
        relay,
        task,
    )
}

#[tokio::test]
async fn newer_commit_during_inflight_wake_rotates_the_delivery_hint() {
    let (relay_url, relay, relay_task) = start_fake_relay().await;
    let server = TestServer::start_with_relay(relay_url).await;
    let gateway = server.provision_device("gateway").await;
    assert_eq!(
        Client::new()
            .put(format!("{}/v1/devices/self/wake-route", server.base_url))
            .bearer_auth(&gateway.token)
            .json(&json!({"route_id":Uuid::new_v4(),"wake_credential":"a".repeat(64)}))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NO_CONTENT
    );

    relay.hold_response.store(true, Ordering::SeqCst);
    server.commit(&server.owner_event(1)).await;
    wait_until(5, "in-flight wake", || {
        let relay = relay.clone();
        async move { relay.requests.lock().await.len() == 1 }
    })
    .await;
    server.commit(&server.owner_event(2)).await;
    relay.hold_response.store(false, Ordering::SeqCst);
    relay.release_response.notify_one();
    wait_until(5, "replacement wake", || {
        let relay = relay.clone();
        async move { relay.requests.lock().await.len() == 2 }
    })
    .await;
    let requests = relay.requests.lock().await;
    assert_ne!(requests[0]["idempotency_id"], requests[1]["idempotency_id"]);
    assert_ne!(requests[0]["opaque_nonce"], requests[1]["opaque_nonce"]);
    drop(requests);
    wait_until(3, "replacement wake deletion", || {
        let pool = server.pool.clone();
        async move {
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM device_wake_jobs")
                .fetch_one(&pool)
                .await
                .unwrap()
                == 0
        }
    })
    .await;
    server.shutdown().await;
    relay_task.abort();
}

#[tokio::test]
async fn durable_wakes_use_random_opaque_ids_coalesce_and_replace_invalid_routes() {
    let (relay_url, relay, relay_task) = start_fake_relay().await;
    let server = TestServer::start_with_relay(relay_url).await;
    let gateway = server.provision_device("gateway").await;
    let client = Client::new();
    let register = |route_id| {
        client
            .put(format!("{}/v1/devices/self/wake-route", server.base_url))
            .bearer_auth(&gateway.token)
            .json(&json!({"route_id":route_id,"wake_credential":"a".repeat(64)}))
    };
    assert_eq!(
        register(Uuid::new_v4()).send().await.unwrap().status(),
        StatusCode::NO_CONTENT
    );

    // Two committed records before the worker claims work are one durable wake.
    server.commit(&server.owner_event(1)).await;
    server.commit(&server.owner_event(2)).await;
    wait_until(5, "accepted coalesced wake", || {
        let relay = relay.clone();
        async move { relay.requests.lock().await.len() == 1 }
    })
    .await;
    let request = relay.requests.lock().await[0].clone();
    let encoded = request.to_string();
    assert!(!encoded.contains(&server.vault.to_string()));
    assert!(!encoded.contains(&gateway.id.to_string()));
    assert!(!encoded.contains("cursor"));
    assert_ne!(request["idempotency_id"], Value::Null);
    assert_ne!(request["opaque_nonce"], Value::Null);
    wait_until(3, "accepted wake deletion", || {
        let pool = server.pool.clone();
        async move {
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM device_wake_jobs")
                .fetch_one(&pool)
                .await
                .unwrap()
                == 0
        }
    })
    .await;

    // A transient response retries the immutable persisted idempotency pair.
    relay.status.store(503, Ordering::SeqCst);
    server.commit(&server.owner_event(3)).await;
    wait_until(5, "retryable wake", || {
        let relay = relay.clone();
        async move { relay.requests.lock().await.len() == 2 }
    })
    .await;
    let retry_id = relay.requests.lock().await[1]["idempotency_id"].clone();
    let retry_nonce = relay.requests.lock().await[1]["opaque_nonce"].clone();
    relay.status.store(202, Ordering::SeqCst);
    wait_until(5, "accepted wake retry", || {
        let relay = relay.clone();
        async move { relay.requests.lock().await.len() == 3 }
    })
    .await;
    assert_eq!(relay.requests.lock().await[2]["idempotency_id"], retry_id);
    assert_eq!(relay.requests.lock().await[2]["opaque_nonce"], retry_nonce);

    relay.status.store(401, Ordering::SeqCst);
    server.commit(&server.owner_event(4)).await;
    wait_until(5, "401 wake", || {
        let relay = relay.clone();
        async move { relay.requests.lock().await.len() == 4 }
    })
    .await;
    wait_until(3, "old route revocation", || {
        let pool = server.pool.clone();
        async move { sqlx::query_scalar::<_, bool>("SELECT revoked_at IS NOT NULL FROM device_wake_routes WHERE vault_id=$1 AND device_id=$2").bind(server.vault).bind(gateway.id).fetch_one(&pool).await.unwrap() }
    }).await;
    relay.status.store(202, Ordering::SeqCst);
    assert_eq!(
        register(Uuid::new_v4()).send().await.unwrap().status(),
        StatusCode::NO_CONTENT
    );
    server.commit(&server.owner_event(5)).await;
    wait_until(5, "replacement route wake", || {
        let relay = relay.clone();
        async move { relay.requests.lock().await.len() == 5 }
    })
    .await;
    server.shutdown().await;
    relay_task.abort();
}

#[tokio::test]
async fn legacy_pairing_routes_are_absent_while_instance_discovery_remains_available() {
    let server = TestServer::start().await;
    let client = Client::new();
    for path in [
        "/v1/pairing",
        "/v1/pairing/consume",
        "/v1/pairing/intents",
        "/v1/pairing/join-requests",
    ] {
        assert_eq!(
            client
                .post(format!("{}{}", server.base_url, path))
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::NOT_FOUND,
            "{path} must not remain as an enrollment alias"
        );
    }
    assert_eq!(
        client
            .get(format!("{}/v1/instance", server.base_url))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    server.shutdown().await;
}

#[tokio::test]
async fn device_revocation_enforces_owner_scope_last_owner_and_token_cleanup() {
    let server = TestServer::start().await;
    let device = server.provision_device("device").await;

    let (status, denied) = server
        .post(
            &device.token,
            &format!("/v1/devices/{}/revoke", server.owner_device),
            &Value::Null,
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(denied["code"], "owner_required");

    let (status, missing) = server
        .post(
            &server.owner_token,
            &format!("/v1/devices/{}/revoke", Uuid::new_v4()),
            &Value::Null,
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(missing["code"], "device_not_found");

    let (status, last_owner) = server
        .post(
            &server.owner_token,
            &format!("/v1/devices/{}/revoke", server.owner_device),
            &Value::Null,
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(last_owner["code"], "last_active_owner");

    let (status, body) = server
        .post(
            &server.owner_token,
            &format!("/v1/devices/{}/revoke", device.id),
            &Value::Null,
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    let (status, replayed) = server
        .post(
            &server.owner_token,
            &format!("/v1/devices/{}/revoke", device.id),
            &Value::Null,
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(replayed["code"], "device_already_revoked");
    let (status, rejected) = server.get(&device.token, "/v1/vault").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(rejected["code"], "invalid_bearer");
    server.shutdown().await;
}

#[tokio::test]
async fn devices_roster_includes_nullable_names_and_revoked_devices() {
    let server = TestServer::start().await;
    let device = server.provision_device("device").await;
    sqlx::query("UPDATE devices SET name=$1, kind=$2 WHERE vault_id=$3 AND device_id=$4")
        .bind("Legacy phone")
        .bind("android")
        .bind(server.vault)
        .bind(device.id)
        .execute(&server.pool)
        .await
        .unwrap();

    let (status, roster) = server.get(&device.token, "/v1/devices").await;
    assert_eq!(status, StatusCode::OK);
    let devices = roster["devices"].as_array().unwrap();
    assert_eq!(devices.len(), 2);
    let owner = devices
        .iter()
        .find(|candidate| candidate["device_id"] == json!(server.owner_device))
        .unwrap();
    assert_eq!(owner["name"], Value::Null);
    assert_eq!(owner["kind"], Value::Null);
    let legacy_phone = devices
        .iter()
        .find(|candidate| candidate["device_id"] == json!(device.id))
        .unwrap();
    assert_eq!(legacy_phone["name"], "Legacy phone");
    assert_eq!(legacy_phone["kind"], "android");
    assert_eq!(legacy_phone["role"], "device");
    assert_eq!(legacy_phone["revoked"], false);
    assert_eq!(legacy_phone["profile_fingerprint"], server.fingerprint);
    assert_eq!(legacy_phone["key_epoch"], 1);

    let (status, body) = server
        .post(
            &server.owner_token,
            &format!("/v1/devices/{}/revoke", device.id),
            &Value::Null,
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    let (status, roster) = server.get(&server.owner_token, "/v1/devices").await;
    assert_eq!(status, StatusCode::OK);
    let legacy_phone = roster["devices"]
        .as_array()
        .unwrap()
        .iter()
        .find(|candidate| candidate["device_id"] == json!(device.id))
        .unwrap();
    assert_eq!(legacy_phone["revoked"], true);

    server.shutdown().await;
}

#[tokio::test]
async fn vault_delete_is_owner_only_requires_echo_and_queues_all_object_cleanup() {
    let server = TestServer::start().await;
    let device = server.provision_device("device").await;
    let client = Client::new();

    let (status, denied) = server
        .call(
            client
                .delete(format!("{}/v1/vault", server.base_url))
                .bearer_auth(&device.token)
                .json(&json!({"vault_id":server.vault})),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(denied["code"], "owner_required");
    let (status, mismatch) = server
        .call(
            client
                .delete(format!("{}/v1/vault", server.base_url))
                .bearer_auth(&server.owner_token)
                .json(&json!({"vault_id":Uuid::new_v4()})),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(mismatch["code"], "vault_id_mismatch");

    let reservation = Uuid::new_v4();
    let attachment = Uuid::new_v4();
    sqlx::query("INSERT INTO upload_reservations(vault_id,attachment_id,device_id,object_key,declared_bytes,declared_sha256) VALUES($1,$2,$3,'reserved-key',1,$4)")
        .bind(server.vault).bind(reservation).bind(server.owner_device).bind(vec![1_u8; 32]).execute(&server.pool).await.unwrap();
    sqlx::query("INSERT INTO attachments(vault_id,attachment_id,object_key,ciphertext_bytes,ciphertext_sha256,created_by_device_id) VALUES($1,$2,'private-key',1,$3,$4)")
        .bind(server.vault).bind(attachment).bind(vec![2_u8; 32]).bind(server.owner_device).execute(&server.pool).await.unwrap();
    sqlx::query("INSERT INTO storage_deletions(object_key,vault_id,reason,not_before) VALUES('private-key',$1,'upload_attempt',now()+interval '1 hour')")
        .bind(server.vault)
        .execute(&server.pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO public_attachment_copies(share_id,vault_id,attachment_id,object_key,token_digest,safe_name,media_type,byte_count,created_by_device_id,ready_at) VALUES($1,$2,$3,'public-key',$4,'file.bin','application/octet-stream',1,$5,now())")
        .bind(Uuid::new_v4()).bind(server.vault).bind(attachment).bind(vec![3_u8; 32]).bind(server.owner_device).execute(&server.pool).await.unwrap();
    server.commit(&server.owner_event(1)).await;

    let (status, deleted) = server
        .call(
            client
                .delete(format!("{}/v1/vault", server.base_url))
                .bearer_auth(&server.owner_token)
                .json(&json!({"vault_id":server.vault})),
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{deleted}");
    let vault_count: i64 = sqlx::query_scalar("SELECT count(*) FROM vaults WHERE vault_id=$1")
        .bind(server.vault)
        .fetch_one(&server.pool)
        .await
        .unwrap();
    assert_eq!(vault_count, 0);
    let remaining_dependents: i64 = sqlx::query_scalar("SELECT (SELECT count(*) FROM devices WHERE vault_id=$1) + (SELECT count(*) FROM encrypted_records WHERE vault_id=$1) + (SELECT count(*) FROM public_attachment_copies WHERE vault_id=$1)")
        .bind(server.vault).fetch_one(&server.pool).await.unwrap();
    assert_eq!(remaining_dependents, 0);
    let queued: Vec<(String, String)> = sqlx::query_as(
        "SELECT object_key,reason FROM storage_deletions WHERE vault_id=$1 ORDER BY object_key",
    )
    .bind(server.vault)
    .fetch_all(&server.pool)
    .await
    .unwrap();
    assert_eq!(
        queued,
        vec![
            ("private-key".into(), "vault_deleted".into()),
            ("public-key".into(), "vault_deleted".into()),
            ("reserved-key".into(), "vault_deleted".into()),
        ]
    );
    let private_cleanup_still_deferred: bool = sqlx::query_scalar(
        "SELECT not_before > now()+interval '50 minutes' FROM storage_deletions WHERE object_key='private-key'",
    )
    .fetch_one(&server.pool)
    .await
    .unwrap();
    assert!(private_cleanup_still_deferred);
    server.shutdown().await;
}

async fn insert_phone_invitation(
    pool: &PgPool,
    vault: Uuid,
    owner_device: Uuid,
    fingerprint: &str,
    state: &str,
    expired: bool,
) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO phone_invitations(id,vault_id,owner_device_id,origin,method,token_digest,owner_transfer_public_key,profile_fingerprint,key_epoch,state,expires_at) VALUES($1,$2,$3,'https://peppy.test','transfer',$4,$5,$6,1,$7,CASE WHEN $8 THEN now()-interval '1 hour' ELSE now()+interval '1 hour' END)")
        .bind(id)
        .bind(vault)
        .bind(owner_device)
        .bind(Sha256::digest(id.as_bytes()).as_slice())
        .bind(vec![7_u8; 32])
        .bind(fingerprint)
        .bind(state)
        .bind(expired)
        .execute(pool)
        .await
        .unwrap();
    id
}

async fn insert_computer_enrollment(
    pool: &PgPool,
    vault: Uuid,
    owner_device: Uuid,
    fingerprint: &str,
) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO computer_enrollments(id,vault_id,owner_device_id,origin,claim_token_digest,server_nonce,recipient_device_id,recipient_kind,recipient_name,recipient_signing_public_key,recipient_transfer_public_key,recipient_credential_digest,recipient_commitment,profile_fingerprint,key_epoch,state,expires_at) VALUES($1,$2,$3,'https://peppy.test',$4,$5,$6,'browser','Computer',$7,$8,$9,$10,$11,1,'consumed',now()+interval '1 hour')")
        .bind(id)
        .bind(vault)
        .bind(owner_device)
        .bind(Sha256::digest(id.as_bytes()).as_slice())
        .bind(vec![1_u8; 32])
        .bind(Uuid::new_v4())
        .bind(vec![2_u8; 32])
        .bind(vec![3_u8; 32])
        .bind(vec![4_u8; 32])
        .bind(vec![5_u8; 32])
        .bind(fingerprint)
        .execute(pool)
        .await
        .unwrap();
    id
}

#[tokio::test]
async fn vault_delete_removes_device_owned_phone_invitations_and_keeps_other_vaults() {
    let server = TestServer::start().await;
    let device = server.provision_device("device").await;
    for (owner, state, expired) in [
        (server.owner_device, "waiting", false),
        (device.id, "consumed", false),
        (server.owner_device, "expired", true),
    ] {
        insert_phone_invitation(
            &server.pool,
            server.vault,
            owner,
            &server.fingerprint,
            state,
            expired,
        )
        .await;
    }
    let other_vault = Uuid::new_v4();
    let (other_profile, other_fingerprint) = profile(other_vault, 1);
    let other_owner = create_owner(
        &server.pool,
        other_profile,
        vec![4, 5, 6],
        other_fingerprint.clone(),
        1,
    )
    .await
    .unwrap();
    let other_invitation = insert_phone_invitation(
        &server.pool,
        other_vault,
        other_owner.device_id,
        &other_fingerprint,
        "waiting",
        false,
    )
    .await;
    let computer_enrollment = insert_computer_enrollment(
        &server.pool,
        server.vault,
        server.owner_device,
        &server.fingerprint,
    )
    .await;
    let other_computer_enrollment = insert_computer_enrollment(
        &server.pool,
        other_vault,
        other_owner.device_id,
        &other_fingerprint,
    )
    .await;

    let (status, deleted) = server
        .call(
            Client::new()
                .delete(format!("{}/v1/vault", server.base_url))
                .bearer_auth(&server.owner_token)
                .json(&json!({"vault_id":server.vault})),
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{deleted}");
    let (vaults, invitations, enrollments, devices, credentials): (i64, i64, i64, i64, i64) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM vaults WHERE vault_id=$1), (SELECT count(*) FROM phone_invitations WHERE vault_id=$1), (SELECT count(*) FROM computer_enrollments WHERE vault_id=$1 AND id=$2), (SELECT count(*) FROM devices WHERE vault_id=$1), (SELECT count(*) FROM device_credentials WHERE vault_id=$1)",
    )
    .bind(server.vault)
    .bind(computer_enrollment)
    .fetch_one(&server.pool)
    .await
    .unwrap();
    assert_eq!(
        (vaults, invitations, enrollments, devices, credentials),
        (0, 0, 0, 0, 0)
    );
    let (other_invitations, other_enrollments, other_devices): (i64, i64, i64) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM phone_invitations WHERE vault_id=$1 AND id=$2), (SELECT count(*) FROM computer_enrollments WHERE vault_id=$1 AND id=$3), (SELECT count(*) FROM devices WHERE vault_id=$1)",
    )
    .bind(other_vault)
    .bind(other_invitation)
    .bind(other_computer_enrollment)
    .fetch_one(&server.pool)
    .await
    .unwrap();
    assert_eq!(
        (other_invitations, other_enrollments, other_devices),
        (1, 1, 1)
    );
    server.shutdown().await;
}

async fn ingest_state(server: &TestServer) -> (i64, i64) {
    sqlx::query_as(
        "SELECT (SELECT next_cursor FROM vaults WHERE vault_id=$1), (SELECT count(*) FROM encrypted_records WHERE vault_id=$1)",
    )
    .bind(server.vault)
    .fetch_one(&server.pool)
    .await
    .unwrap()
}

#[tokio::test]
async fn ingest_rejects_sequences_beyond_bigint_without_advancing_the_cursor() {
    let server = TestServer::start().await;
    let beyond_bigint = i64::MAX as u64 + 1;
    assert_eq!(beyond_bigint.to_string(), "9223372036854775808");
    let overflowed_sequence = server.owner_event(beyond_bigint);
    let overflowed_supersedes = set_fingerprint(
        compacting_event(
            server.vault,
            server.owner_device,
            1,
            Uuid::new_v4(),
            vec![(Uuid::new_v4(), beyond_bigint)],
        ),
        &server.fingerprint,
    );
    for envelope in [overflowed_sequence, overflowed_supersedes] {
        let (status, rejected) = server
            .post(&server.owner_token, "/v1/events", &envelope)
            .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{rejected}");
        assert_eq!(rejected["code"], "invalid_envelope");
        assert_eq!(ingest_state(&server).await, (0, 0));
    }

    assert_eq!(server.commit(&server.owner_event(1)).await, "1");
    assert_eq!(ingest_state(&server).await, (1, 1));
    server.shutdown().await;
}

/// Holds the vault row lock in a test transaction and returns it with its backend pid.
async fn hold_vault_lock(server: &TestServer) -> (sqlx::Transaction<'static, sqlx::Postgres>, i32) {
    let mut holder = server.pool.begin().await.unwrap();
    let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *holder)
        .await
        .unwrap();
    sqlx::query("SELECT 1 FROM vaults WHERE vault_id=$1 FOR UPDATE")
        .bind(server.vault)
        .execute(&mut *holder)
        .await
        .unwrap();
    (holder, pid)
}

/// Waits until PostgreSQL reports a backend running `statement` blocked by `holder_pid`.
/// Reaching the vault lock proves the request already authenticated.
async fn wait_for_lock_waiter(server: &TestServer, holder_pid: i32, statement: &str) {
    let pattern = format!("%{statement}%");
    wait_until(5, "request blocked on the vault lock", || {
        let admin = server.admin.clone();
        let pattern = pattern.clone();
        async move {
            sqlx::query_scalar::<_, bool>(
                "SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE $1 = ANY(pg_blocking_pids(pid)) AND query LIKE $2)",
            )
            .bind(holder_pid)
            .bind(pattern)
            .fetch_one(&admin)
            .await
            .unwrap()
        }
    })
    .await;
}

async fn revoke_credential_and_release(
    mut holder: sqlx::Transaction<'static, sqlx::Postgres>,
    device: Uuid,
) {
    sqlx::query("UPDATE device_credentials SET revoked_at=now() WHERE device_id=$1")
        .bind(device)
        .execute(&mut *holder)
        .await
        .unwrap();
    holder.commit().await.unwrap();
}

async fn finish_blocked(task: JoinHandle<(StatusCode, Value)>) -> (StatusCode, Value) {
    tokio::time::timeout(Duration::from_secs(10), task)
        .await
        .expect("blocked request did not finish after the lock was released")
        .unwrap()
}

async fn send_detached(request: reqwest::RequestBuilder) -> (StatusCode, Value) {
    let response = request.send().await.unwrap();
    let status = response.status();
    let text = response.text().await.unwrap();
    let body = if text.is_empty() {
        Value::Null
    } else {
        serde_json::from_str(&text).unwrap()
    };
    (status, body)
}

#[tokio::test]
async fn ingest_rejects_a_credential_revoked_while_waiting_for_the_vault_lock() {
    let server = TestServer::start().await;
    let device = server.provision_device("device").await;
    let envelope = set_fingerprint(
        event(server.vault, device.id, 1, Uuid::new_v4(), 1),
        &server.fingerprint,
    );
    let (holder, holder_pid) = hold_vault_lock(&server).await;
    let task = tokio::spawn(send_detached(
        Client::new()
            .post(format!("{}/v1/events", server.base_url))
            .bearer_auth(&device.token)
            .json(&envelope),
    ));
    wait_for_lock_waiter(
        &server,
        holder_pid,
        "SELECT key_epoch,profile_fingerprint FROM vaults",
    )
    .await;
    revoke_credential_and_release(holder, device.id).await;

    let (status, rejected) = finish_blocked(task).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{rejected}");
    assert_eq!(rejected["code"], "invalid_bearer");
    assert_eq!(ingest_state(&server).await, (0, 0));
    assert_eq!(server.commit(&server.owner_event(1)).await, "1");
    server.shutdown().await;
}

#[tokio::test]
async fn attachment_release_rejects_a_credential_revoked_while_waiting_for_the_vault_lock() {
    let server = TestServer::start().await;
    let device = server.provision_device("device").await;
    let attachment = Uuid::new_v4();
    sqlx::query("INSERT INTO upload_reservations(vault_id,attachment_id,device_id,object_key,declared_bytes,declared_sha256,finalized_at,reference_tracked) VALUES($1,$2,$3,'release-key',1,$4,now(),TRUE)")
        .bind(server.vault).bind(attachment).bind(device.id).bind(vec![1_u8; 32]).execute(&server.pool).await.unwrap();
    sqlx::query("INSERT INTO attachments(vault_id,attachment_id,object_key,ciphertext_bytes,ciphertext_sha256,created_by_device_id) VALUES($1,$2,'release-key',1,$3,$4)")
        .bind(server.vault).bind(attachment).bind(vec![1_u8; 32]).bind(device.id).execute(&server.pool).await.unwrap();

    let (holder, holder_pid) = hold_vault_lock(&server).await;
    let task = tokio::spawn(send_detached(
        Client::new()
            .delete(format!("{}/v1/attachments/{attachment}", server.base_url))
            .bearer_auth(&device.token)
            .json(&json!({"compaction_generation":"1","release_before_cursor":"0"})),
    ));
    wait_for_lock_waiter(
        &server,
        holder_pid,
        "SELECT compaction_generation,replay_floor_cursor FROM vaults",
    )
    .await;
    revoke_credential_and_release(holder, device.id).await;

    let (status, rejected) = finish_blocked(task).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{rejected}");
    assert_eq!(rejected["code"], "invalid_bearer");
    let (attachments, deletions): (i64, i64) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM attachments WHERE vault_id=$1 AND attachment_id=$2), (SELECT count(*) FROM storage_deletions WHERE object_key='release-key')",
    )
    .bind(server.vault)
    .bind(attachment)
    .fetch_one(&server.pool)
    .await
    .unwrap();
    assert_eq!((attachments, deletions), (1, 0));
    server.shutdown().await;
}

#[tokio::test]
async fn cross_vault_foreign_gateway_and_non_target_receipts_are_denied() {
    let first = TestServer::start().await;
    let target = first.provision_device("gateway").await;
    let other = first.provision_device("gateway").await;
    let foreign_vault = Uuid::new_v4();
    let (foreign_profile, foreign_fingerprint) = profile(foreign_vault, 1);
    let foreign_owner = create_owner(
        &first.pool,
        foreign_profile,
        vec![1, 2, 3],
        foreign_fingerprint.clone(),
        1,
    )
    .await
    .unwrap();
    let foreign = first
        .provision_device_for(foreign_vault, &foreign_fingerprint, "gateway")
        .await;
    let client = Client::new();

    let foreign_envelope = set_fingerprint(
        event(first.vault, foreign_owner.device_id, 1, Uuid::new_v4(), 1),
        &first.fingerprint,
    );
    let denied = client
        .post(format!("{}/v1/events", first.base_url))
        .bearer_auth(&foreign_owner.device_token)
        .json(&foreign_envelope)
        .send()
        .await
        .unwrap();
    assert_eq!(denied.status(), StatusCode::FORBIDDEN);

    for foreign_route in [
        command(
            first.vault,
            first.owner_device,
            1,
            Uuid::new_v4(),
            Uuid::new_v4(),
            foreign.id,
        ),
        rcs_link_command(
            first.vault,
            first.owner_device,
            1,
            Uuid::new_v4(),
            Uuid::new_v4(),
            foreign.id,
        ),
    ] {
        let denied = first
            .auth(client.post(format!("{}/v1/commands", first.base_url)))
            .json(&set_fingerprint(foreign_route, &first.fingerprint))
            .send()
            .await
            .unwrap();
        assert_eq!(denied.status(), StatusCode::FORBIDDEN);
    }

    let command_id = Uuid::new_v4();
    let valid = set_fingerprint(
        command(
            first.vault,
            first.owner_device,
            1,
            Uuid::new_v4(),
            command_id,
            target.id,
        ),
        &first.fingerprint,
    );
    let accepted = first
        .auth(client.post(format!("{}/v1/commands", first.base_url)))
        .json(&valid)
        .send()
        .await
        .unwrap();
    assert_eq!(accepted.status(), StatusCode::OK);
    let receipt_url = format!("{}/v1/commands/{command_id}/receipts", first.base_url);
    let denied = client
        .post(&receipt_url)
        .bearer_auth(&other.token)
        .json(&json!({"receipt":{"state":"sent"}}))
        .send()
        .await
        .unwrap();
    assert_eq!(denied.status(), StatusCode::FORBIDDEN);
    let owner_denied = first
        .auth(client.post(&receipt_url))
        .json(&json!({"receipt":{}}))
        .send()
        .await
        .unwrap();
    assert_eq!(owner_denied.status(), StatusCode::FORBIDDEN);
    let accepted = client
        .post(&receipt_url)
        .bearer_auth(&target.token)
        .json(&json!({"receipt":{"state":"sent"}}))
        .send()
        .await
        .unwrap();
    assert_eq!(accepted.status(), StatusCode::NO_CONTENT);

    first.shutdown().await;
}

#[tokio::test]
async fn owner_capability_registration_limits_owner_to_its_own_gateway_work() {
    let server = TestServer::start().await;
    let gateway = server.provision_device("gateway").await;
    let ordinary = server.provision_device("device").await;
    let desktop_owner = server.provision_device("gateway").await;
    sqlx::query("UPDATE devices SET role='owner' WHERE vault_id=$1 AND device_id=$2")
        .bind(server.vault)
        .bind(desktop_owner.id)
        .execute(&server.pool)
        .await
        .unwrap();
    let capability = json!({"simulator":false,"capabilities":{"carrier":true}});

    let (status, _) = server
        .post(&ordinary.token, "/v1/capabilities", &capability)
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let before = set_fingerprint(
        command(
            server.vault,
            server.owner_device,
            1,
            Uuid::new_v4(),
            Uuid::new_v4(),
            server.owner_device,
        ),
        &server.fingerprint,
    );
    let (status, _) = server
        .post(&server.owner_token, "/v1/commands", &before)
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let gateway_command = Uuid::new_v4();
    let routed_to_gateway = set_fingerprint(
        command(
            server.vault,
            server.owner_device,
            2,
            Uuid::new_v4(),
            gateway_command,
            gateway.id,
        ),
        &server.fingerprint,
    );
    let (status, _) = server
        .post(&server.owner_token, "/v1/commands", &routed_to_gateway)
        .await;
    assert_eq!(status, StatusCode::OK);
    let (_, desktop_pending) = server
        .get(&server.owner_token, "/v1/commands/pending")
        .await;
    assert_eq!(
        desktop_pending["commands"][0]["command_id"],
        json!(gateway_command)
    );

    let (status, _) = server
        .post(&server.owner_token, "/v1/capabilities", &capability)
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let owner_role: String =
        sqlx::query_scalar("SELECT role FROM devices WHERE vault_id=$1 AND device_id=$2")
            .bind(server.vault)
            .bind(server.owner_device)
            .fetch_one(&server.pool)
            .await
            .unwrap();
    assert_eq!(owner_role, "owner");

    let owner_command = Uuid::new_v4();
    let routed_to_owner = set_fingerprint(
        command(
            server.vault,
            server.owner_device,
            3,
            Uuid::new_v4(),
            owner_command,
            server.owner_device,
        ),
        &server.fingerprint,
    );
    let (status, _) = server
        .post(&server.owner_token, "/v1/commands", &routed_to_owner)
        .await;
    assert_eq!(status, StatusCode::OK);
    let (_, mobile_pending) = server
        .get(&server.owner_token, "/v1/commands/pending")
        .await;
    assert_eq!(mobile_pending["commands"].as_array().unwrap().len(), 1);
    assert_eq!(
        mobile_pending["commands"][0]["command_id"],
        json!(owner_command)
    );
    let (_, desktop_owner_pending) = server
        .get(&desktop_owner.token, "/v1/commands/pending")
        .await;
    assert_eq!(
        desktop_owner_pending["commands"].as_array().unwrap().len(),
        2
    );
    assert!(
        desktop_owner_pending["commands"]
            .as_array()
            .unwrap()
            .iter()
            .any(|command| command["command_id"] == json!(gateway_command))
    );
    assert!(
        desktop_owner_pending["commands"]
            .as_array()
            .unwrap()
            .iter()
            .any(|command| command["command_id"] == json!(owner_command))
    );

    let own_receipt = format!("/v1/commands/{owner_command}/receipts");
    let (status, _) = server
        .post(
            &server.owner_token,
            &own_receipt,
            &json!({"receipt":{"state":"sent"}}),
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let foreign_receipt = format!("/v1/commands/{gateway_command}/receipts");
    let (status, _) = server
        .post(
            &server.owner_token,
            &foreign_receipt,
            &json!({"receipt":{"state":"sent"}}),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let foreign_vault = Uuid::new_v4();
    let (foreign_profile, foreign_fingerprint) = profile(foreign_vault, 1);
    let foreign_owner = create_owner(
        &server.pool,
        foreign_profile,
        vec![1, 2, 3],
        foreign_fingerprint,
        1,
    )
    .await
    .unwrap();
    let (status, foreign_capability) = server
        .post(&foreign_owner.device_token, "/v1/capabilities", &capability)
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{foreign_capability}");
    for foreign_target in [
        command(
            server.vault,
            server.owner_device,
            4,
            Uuid::new_v4(),
            Uuid::new_v4(),
            foreign_owner.device_id,
        ),
        rcs_link_command(
            server.vault,
            server.owner_device,
            4,
            Uuid::new_v4(),
            Uuid::new_v4(),
            foreign_owner.device_id,
        ),
    ] {
        let (status, foreign_target_rejected) = server
            .post(
                &server.owner_token,
                "/v1/commands",
                &set_fingerprint(foreign_target, &server.fingerprint),
            )
            .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(foreign_target_rejected["code"], "gateway_not_authorized");
    }

    let revoked_owner = server.provision_device("gateway").await;
    sqlx::query("UPDATE devices SET role='owner' WHERE vault_id=$1 AND device_id=$2")
        .bind(server.vault)
        .bind(revoked_owner.id)
        .execute(&server.pool)
        .await
        .unwrap();
    let (status, revoked_capability) = server
        .post(&revoked_owner.token, "/v1/capabilities", &capability)
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{revoked_capability}");
    let (status, revoked) = server
        .post(
            &server.owner_token,
            &format!("/v1/devices/{}/revoke", revoked_owner.id),
            &json!({}),
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{revoked}");
    for revoked_target in [
        command(
            server.vault,
            server.owner_device,
            5,
            Uuid::new_v4(),
            Uuid::new_v4(),
            revoked_owner.id,
        ),
        rcs_link_command(
            server.vault,
            server.owner_device,
            5,
            Uuid::new_v4(),
            Uuid::new_v4(),
            revoked_owner.id,
        ),
    ] {
        let (status, revoked_target_rejected) = server
            .post(
                &server.owner_token,
                "/v1/commands",
                &set_fingerprint(revoked_target, &server.fingerprint),
            )
            .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(revoked_target_rejected["code"], "gateway_not_authorized");
    }

    sqlx::query("UPDATE devices SET revoked_at=now() WHERE vault_id=$1 AND device_id=$2")
        .bind(server.vault)
        .bind(server.owner_device)
        .execute(&server.pool)
        .await
        .unwrap();
    let (status, _) = server
        .post(&server.owner_token, "/v1/capabilities", &capability)
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    server.shutdown().await;
}

#[tokio::test]
async fn capability_updates_skip_identical_values_and_timestamp_changed_values() {
    let server = TestServer::start().await;
    let initial = json!({"simulator":false,"capabilities":{"carrier":true}});
    let (status, _) = server
        .post(&server.owner_token, "/v1/capabilities", &initial)
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    sqlx::query("UPDATE device_capabilities SET updated_at=now()-interval '1 hour' WHERE vault_id=$1 AND device_id=$2")
        .bind(server.vault)
        .bind(server.owner_device)
        .execute(&server.pool)
        .await
        .unwrap();
    let unchanged_at: String = sqlx::query_scalar(
        "SELECT updated_at::text FROM device_capabilities WHERE vault_id=$1 AND device_id=$2",
    )
    .bind(server.vault)
    .bind(server.owner_device)
    .fetch_one(&server.pool)
    .await
    .unwrap();

    let (status, _) = server
        .post(&server.owner_token, "/v1/capabilities", &initial)
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let repeated_at: String = sqlx::query_scalar(
        "SELECT updated_at::text FROM device_capabilities WHERE vault_id=$1 AND device_id=$2",
    )
    .bind(server.vault)
    .bind(server.owner_device)
    .fetch_one(&server.pool)
    .await
    .unwrap();
    assert_eq!(repeated_at, unchanged_at);

    let changed = json!({"simulator":true,"capabilities":{"carrier":false}});
    let (status, _) = server
        .post(&server.owner_token, "/v1/capabilities", &changed)
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (simulator, capabilities, changed_at): (bool, Value, String) = sqlx::query_as("SELECT simulator, capabilities, updated_at::text FROM device_capabilities WHERE vault_id=$1 AND device_id=$2")
        .bind(server.vault)
        .bind(server.owner_device)
        .fetch_one(&server.pool)
        .await
        .unwrap();
    assert!(simulator);
    assert_eq!(capabilities, json!({"carrier":false}));
    assert_ne!(changed_at, unchanged_at);

    server.shutdown().await;
}

#[tokio::test]
async fn repeated_compaction_capability_registration_keeps_the_fence_set() {
    let server = TestServer::start().await;

    let (status, _) = server
        .post(&server.owner_token, "/v1/compaction/capability", &json!({}))
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let fence_xmin: String =
        sqlx::query_scalar("SELECT xmin::text FROM devices WHERE vault_id=$1 AND device_id=$2")
            .bind(server.vault)
            .bind(server.owner_device)
            .fetch_one(&server.pool)
            .await
            .unwrap();
    let (status, _) = server
        .post(&server.owner_token, "/v1/compaction/capability", &json!({}))
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (fenced, repeated_xmin): (bool, String) = sqlx::query_as(
        "SELECT compaction_generation_fence, xmin::text FROM devices WHERE vault_id=$1 AND device_id=$2",
    )
    .bind(server.vault)
    .bind(server.owner_device)
    .fetch_one(&server.pool)
    .await
    .unwrap();
    assert!(fenced);
    assert_eq!(repeated_xmin, fence_xmin);

    server.shutdown().await;
}

#[tokio::test]
async fn exact_retry_returns_cursor_and_changed_identity_or_sequence_conflicts() {
    let server = TestServer::start().await;
    let client = Client::new();
    let envelope_id = Uuid::new_v4();
    let original = set_fingerprint(
        event(server.vault, server.owner_device, 1, envelope_id, 1),
        &server.fingerprint,
    );
    let first: Value = server
        .auth(client.post(format!("{}/v1/events", server.base_url)))
        .json(&original)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(first, json!({"cursor":"1","duplicate":false}));
    let retry: Value = server
        .auth(client.post(format!("{}/v1/events", server.base_url)))
        .json(&original)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(retry, json!({"cursor":"1","duplicate":true}));

    let changed_envelope = set_fingerprint(
        event(server.vault, server.owner_device, 2, envelope_id, 2),
        &server.fingerprint,
    );
    let response = server
        .auth(client.post(format!("{}/v1/events", server.base_url)))
        .json(&changed_envelope)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT);
    let changed_sequence = set_fingerprint(
        event(server.vault, server.owner_device, 1, Uuid::new_v4(), 3),
        &server.fingerprint,
    );
    let response = server
        .auth(client.post(format!("{}/v1/events", server.base_url)))
        .json(&changed_sequence)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT);
    server.shutdown().await;
}

#[tokio::test]
async fn origin_tags_allow_distinct_same_producer_records_and_deduplicate_cross_producer_records() {
    let server = TestServer::start().await;
    let replacement = server.provision_device("device").await;
    let tag = [7; 32];
    let first = with_origin_tag(
        set_fingerprint(
            event(server.vault, server.owner_device, 1, Uuid::new_v4(), 1),
            &server.fingerprint,
        ),
        tag,
    );
    let (status, accepted) = server.post(&server.owner_token, "/v1/events", &first).await;
    assert_eq!(status, StatusCode::OK, "{accepted}");
    assert_eq!(accepted, json!({"cursor":"1","duplicate":false}));

    let same_producer_request = with_origin_tag(
        set_fingerprint(
            event(server.vault, server.owner_device, 2, Uuid::new_v4(), 2),
            &server.fingerprint,
        ),
        tag,
    );
    let same_producer_id: Uuid = same_producer_request["envelope_id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let (status, same_producer) = server
        .post(&server.owner_token, "/v1/events", &same_producer_request)
        .await;
    assert_eq!(status, StatusCode::OK, "{same_producer}");
    assert_eq!(same_producer, json!({"cursor":"2","duplicate":false}));
    let (indexed_origin_tag_is_null, serialized_origin_tag): (bool, Option<String>) =
        sqlx::query_as(
            "SELECT origin_tag IS NULL, envelope->>'origin_tag' FROM encrypted_records WHERE vault_id=$1 AND envelope_id=$2",
        )
        .bind(server.vault)
        .bind(same_producer_id)
        .fetch_one(&server.pool)
        .await
        .unwrap();
    assert!(indexed_origin_tag_is_null);
    assert_eq!(
        serialized_origin_tag,
        Some(base64::engine::general_purpose::STANDARD.encode(tag))
    );
    let (status, retry) = server
        .post(&server.owner_token, "/v1/events", &same_producer_request)
        .await;
    assert_eq!(status, StatusCode::OK, "{retry}");
    assert_eq!(retry, json!({"cursor":"2","duplicate":true}));
    let mut changed_same_producer = same_producer_request.clone();
    changed_same_producer["ciphertext"] = json!("AQIE");
    let (status, changed) = server
        .post(&server.owner_token, "/v1/events", &changed_same_producer)
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{changed}");

    let cross_producer = with_origin_tag(
        set_fingerprint(
            event(server.vault, replacement.id, 1, Uuid::new_v4(), 3),
            &server.fingerprint,
        ),
        tag,
    );
    let (status, cross_producer) = server
        .post(&replacement.token, "/v1/events", &cross_producer)
        .await;
    assert_eq!(status, StatusCode::OK, "{cross_producer}");
    assert_eq!(
        cross_producer,
        json!({"cursor":"1","duplicate":true,"origin_duplicate":true})
    );
    let event_count: i64 = sqlx::query_scalar("SELECT count(*) FROM event_log WHERE vault_id=$1")
        .bind(server.vault)
        .fetch_one(&server.pool)
        .await
        .unwrap();
    assert_eq!(event_count, 2);

    let (status, retry) = server.post(&server.owner_token, "/v1/events", &first).await;
    assert_eq!(status, StatusCode::OK, "{retry}");
    assert_eq!(retry, json!({"cursor":"1","duplicate":true}));

    let untagged = set_fingerprint(
        event(server.vault, replacement.id, 2, Uuid::new_v4(), 3),
        &server.fingerprint,
    );
    let (status, untagged) = server
        .post(&replacement.token, "/v1/events", &untagged)
        .await;
    assert_eq!(status, StatusCode::OK, "{untagged}");
    assert_eq!(untagged, json!({"cursor":"3","duplicate":false}));

    let other_vault = Uuid::new_v4();
    let (profile, fingerprint) = profile(other_vault, 1);
    let other_owner = create_owner(&server.pool, profile, vec![1, 2, 3], fingerprint.clone(), 1)
        .await
        .unwrap();
    let other = with_origin_tag(
        set_fingerprint(
            event(other_vault, other_owner.device_id, 1, Uuid::new_v4(), 4),
            &fingerprint,
        ),
        tag,
    );
    let (status, other) = server
        .post(&other_owner.device_token, "/v1/events", &other)
        .await;
    assert_eq!(status, StatusCode::OK, "{other}");
    assert_eq!(other, json!({"cursor":"1","duplicate":false}));

    let compacted_tag = [8; 32];
    sqlx::query("INSERT INTO compacted_records(vault_id,producer_device_id,producer_sequence,envelope_id,command_id,cipher_digest,original_cursor,origin_tag) VALUES($1,$2,9,$3,NULL,$4,77,$5)")
        .bind(server.vault)
        .bind(server.owner_device)
        .bind(Uuid::new_v4())
        .bind(vec![9_u8; 32])
        .bind(compacted_tag.to_vec())
        .execute(&server.pool)
        .await
        .unwrap();
    let compacted_duplicate = with_origin_tag(
        set_fingerprint(
            event(server.vault, replacement.id, 3, Uuid::new_v4(), 5),
            &server.fingerprint,
        ),
        compacted_tag,
    );
    let (status, compacted_duplicate) = server
        .post(&replacement.token, "/v1/events", &compacted_duplicate)
        .await;
    assert_eq!(status, StatusCode::OK, "{compacted_duplicate}");
    assert_eq!(
        compacted_duplicate,
        json!({"cursor":"77","duplicate":true,"origin_duplicate":true})
    );

    let concurrent = [3_u64, 4]
        .into_iter()
        .map(|sequence| {
            with_origin_tag(
                set_fingerprint(
                    event(
                        server.vault,
                        server.owner_device,
                        sequence,
                        Uuid::new_v4(),
                        sequence as u8,
                    ),
                    &server.fingerprint,
                ),
                [10; 32],
            )
        })
        .map(|request| {
            let base_url = server.base_url.clone();
            let token = server.owner_token.clone();
            tokio::spawn(send_detached(
                Client::new()
                    .post(format!("{base_url}/v1/events"))
                    .bearer_auth(token)
                    .json(&request),
            ))
        })
        .collect::<Vec<_>>();
    for task in concurrent {
        let (status, body) = task.await.unwrap();
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["duplicate"], false);
    }

    let tagged_command = with_origin_tag(
        set_fingerprint(
            command(
                server.vault,
                server.owner_device,
                5,
                Uuid::new_v4(),
                Uuid::new_v4(),
                replacement.id,
            ),
            &server.fingerprint,
        ),
        [9; 32],
    );
    let (status, rejected) = server
        .post(&server.owner_token, "/v1/commands", &tagged_command)
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{rejected}");
    assert_eq!(rejected["code"], "invalid_envelope");
    server.shutdown().await;
}

#[tokio::test]
async fn compacted_same_producer_extra_origin_record_keeps_its_exact_retry_identity() {
    let server = TestServer::start().await;
    let tag = [11; 32];
    let tag_base64 = base64::engine::general_purpose::STANDARD.encode(tag);
    let representative = with_origin_tag(
        set_fingerprint(
            event(server.vault, server.owner_device, 1, Uuid::new_v4(), 1),
            &server.fingerprint,
        ),
        tag,
    );
    let representative_id: Uuid = representative["envelope_id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(server.commit(&representative).await, "1");
    let extra_id = Uuid::new_v4();
    let extra = with_origin_tag(
        set_fingerprint(
            event(server.vault, server.owner_device, 2, extra_id, 2),
            &server.fingerprint,
        ),
        tag,
    );
    assert_eq!(server.commit(&extra).await, "2");
    let extra_envelope: Envelope = serde_json::from_value(extra.clone()).unwrap();
    let extra_digest = extra_envelope.wire_digest().unwrap();
    let (retained_envelope, retained_digest): (Value, Vec<u8>) = sqlx::query_as(
        "SELECT envelope, cipher_digest FROM encrypted_records WHERE vault_id=$1 AND envelope_id=$2",
    )
    .bind(server.vault)
    .bind(extra_id)
    .fetch_one(&server.pool)
    .await
    .unwrap();
    assert_eq!(
        retained_envelope,
        serde_json::to_value(&extra_envelope).unwrap()
    );
    assert_eq!(retained_digest, extra_digest.as_slice());
    let compactor = set_fingerprint(
        compacting_event(
            server.vault,
            server.owner_device,
            3,
            Uuid::new_v4(),
            vec![(server.owner_device, 1), (server.owner_device, 2)],
        ),
        &server.fingerprint,
    );
    assert_eq!(server.commit(&compactor).await, "3");
    let (status, _) = server
        .post(&server.owner_token, "/v1/compaction/capability", &json!({}))
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    age_and_prune(&server).await;
    sqlx::query(
        "UPDATE record_compaction SET created_at=now()-interval '91 days' WHERE vault_id=$1",
    )
    .bind(server.vault)
    .execute(&server.pool)
    .await
    .unwrap();
    assert_eq!(compact_records(&server.pool).await.unwrap(), 2);
    let (indexed_tag_is_null, original_cursor, compacted_digest): (bool, i64, Vec<u8>) = sqlx::query_as(
        "SELECT origin_tag IS NULL, original_cursor, cipher_digest FROM compacted_records WHERE vault_id=$1 AND envelope_id=$2",
    )
    .bind(server.vault)
    .bind(extra_id)
    .fetch_one(&server.pool)
    .await
    .unwrap();
    assert!(indexed_tag_is_null);
    assert_eq!(original_cursor, 2);
    assert_eq!(compacted_digest, extra_digest.as_slice());
    let (representative_live, representative_compacted): (bool, bool) = sqlx::query_as(
        "SELECT EXISTS(SELECT 1 FROM encrypted_records WHERE vault_id=$1 AND envelope_id=$2), EXISTS(SELECT 1 FROM compacted_records WHERE vault_id=$1 AND envelope_id=$2)",
    )
    .bind(server.vault)
    .bind(representative_id)
    .fetch_one(&server.pool)
    .await
    .unwrap();
    assert_eq!(
        (representative_live, representative_compacted),
        (false, true)
    );
    let (status, retry) = server.post(&server.owner_token, "/v1/events", &extra).await;
    assert_eq!(status, StatusCode::OK, "{retry}");
    assert_eq!(retry, json!({"cursor":"2","duplicate":true}));
    let mut changed = extra.clone();
    changed["ciphertext"] = json!("AQIE");
    let (status, conflict) = server
        .post(&server.owner_token, "/v1/events", &changed)
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{conflict}");

    let third_same_producer = with_origin_tag(
        set_fingerprint(
            event(server.vault, server.owner_device, 4, Uuid::new_v4(), 4),
            &server.fingerprint,
        ),
        tag,
    );
    let (status, admitted) = server
        .post(&server.owner_token, "/v1/events", &third_same_producer)
        .await;
    assert_eq!(status, StatusCode::OK, "{admitted}");
    assert_eq!(admitted, json!({"cursor":"4","duplicate":false}));
    let third_id: Uuid = third_same_producer["envelope_id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let third_indexed_tag_is_null: bool = sqlx::query_scalar(
        "SELECT origin_tag IS NULL FROM encrypted_records WHERE vault_id=$1 AND envelope_id=$2",
    )
    .bind(server.vault)
    .bind(third_id)
    .fetch_one(&server.pool)
    .await
    .unwrap();
    assert!(third_indexed_tag_is_null);
    let (status, retry) = server
        .post(&server.owner_token, "/v1/events", &third_same_producer)
        .await;
    assert_eq!(status, StatusCode::OK, "{retry}");
    assert_eq!(retry, json!({"cursor":"4","duplicate":true}));
    let representative_indexes: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM (SELECT origin_tag FROM encrypted_records WHERE vault_id=$1 UNION ALL SELECT origin_tag FROM compacted_records WHERE vault_id=$1) records WHERE origin_tag=$2",
    )
    .bind(server.vault)
    .bind(tag.to_vec())
    .fetch_one(&server.pool)
    .await
    .unwrap();
    assert_eq!(
        representative_indexes, 1,
        "tag {tag_base64} must retain one indexed representative"
    );
    server.shutdown().await;
}

#[tokio::test]
async fn concurrent_commits_have_gapless_high_water_and_ordered_replay() {
    let server = Arc::new(TestServer::start().await);
    let mut tasks = Vec::new();
    for sequence in 1..=16_u64 {
        let server = Arc::clone(&server);
        tasks.push(tokio::spawn(async move {
            let body = set_fingerprint(
                event(
                    server.vault,
                    server.owner_device,
                    sequence,
                    Uuid::new_v4(),
                    sequence as u8,
                ),
                &server.fingerprint,
            );
            let response = server
                .auth(Client::new().post(format!("{}/v1/events", server.base_url)))
                .json(&body)
                .send()
                .await
                .unwrap();
            let status = response.status();
            let body = response.text().await.unwrap();
            (status, body)
        }));
    }
    for task in tasks {
        let (status, body) = task.await.unwrap();
        assert_eq!(status, StatusCode::OK, "{body}");
    }
    let replay: Value = server
        .auth(Client::new().get(format!("{}/v1/events?after=0&limit=100", server.base_url)))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(replay["high_water_cursor"], "16");
    let cursors: Vec<u64> = replay["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["cursor"].as_str().unwrap().parse().unwrap())
        .collect();
    assert_eq!(cursors, (1..=16).collect::<Vec<_>>());
    Arc::try_unwrap(server).ok().unwrap().shutdown().await;
}

#[tokio::test]
async fn websocket_presence_hint_is_vault_scoped_and_tracks_pong_and_close() {
    let server = TestServer::start().await;
    let device = server.provision_device("device").await;

    let foreign_vault = Uuid::new_v4();
    let (foreign_profile, foreign_fingerprint) = profile(foreign_vault, 1);
    let foreign = create_owner(
        &server.pool,
        foreign_profile,
        vec![1],
        foreign_fingerprint,
        1,
    )
    .await
    .unwrap();

    let mut observer = server.connect_ws(&server.owner_token).await;
    send_hello(&mut observer, "0").await;
    let ready = next_json(&mut observer, 3).await;
    assert_eq!(ready["type"], "ready");
    assert!(
        ready["frame_types"]
            .as_array()
            .unwrap()
            .contains(&json!("drafts_changed"))
    );
    assert_eq!(
        next_json(&mut observer, 3).await["type"],
        "presence_changed"
    );

    let mut foreign_observer = server.connect_ws(&foreign.device_token).await;
    send_hello(&mut foreign_observer, "0").await;
    assert_eq!(next_json(&mut foreign_observer, 3).await["type"], "ready");
    assert_eq!(
        next_json(&mut foreign_observer, 3).await["type"],
        "presence_changed"
    );

    let mut subject = server.connect_ws(&device.token).await;
    send_hello(&mut subject, "0").await;
    assert_eq!(next_json(&mut subject, 3).await["type"], "ready");
    assert_eq!(next_json(&mut subject, 3).await["type"], "presence_changed");
    assert_eq!(
        next_json(&mut observer, 3).await["type"],
        "presence_changed"
    );
    assert_no_text_frame(&mut foreign_observer, Duration::from_millis(500)).await;

    let (_, connected_roster) = server.get(&device.token, "/v1/devices").await;
    let connected = connected_roster["devices"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["device_id"] == device.id.to_string())
        .unwrap();
    assert_eq!(connected["presence"]["state"], "connected");

    subject.close(None).await.unwrap();
    assert_eq!(
        next_json(&mut observer, 3).await["type"],
        "presence_changed"
    );
    assert_no_text_frame(&mut foreign_observer, Duration::from_millis(500)).await;

    let (_, seen_roster) = server.get(&device.token, "/v1/devices").await;
    let seen = seen_roster["devices"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["device_id"] == device.id.to_string())
        .unwrap();
    assert_eq!(seen["presence"]["state"], "seen");
    server.shutdown().await;
}

#[tokio::test]
async fn websocket_pongs_commit_renewals_even_when_another_socket_is_responsive() {
    let server = TestServer::start().await;
    let device = server.provision_device("device").await;
    let mut observer = server.connect_ws(&server.owner_token).await;
    send_hello(&mut observer, "0").await;
    assert_eq!(next_json(&mut observer, 3).await["type"], "ready");
    answer_ping(&mut observer, "observer initial challenge", 5).await;
    wait_for_responsive_sockets(&server, server.owner_device, 1).await;
    assert_eq!(
        next_json(&mut observer, 5).await["type"],
        "presence_changed"
    );
    let mut first = server.connect_ws(&device.token).await;
    send_hello(&mut first, "0").await;
    assert_eq!(next_json(&mut first, 3).await["type"], "ready");
    let mut second = server.connect_ws(&device.token).await;
    send_hello(&mut second, "0").await;
    assert_eq!(next_json(&mut second, 3).await["type"], "ready");

    answer_ping(&mut first, "first socket initial challenge", 5).await;
    wait_for_responsive_sockets(&server, device.id, 1).await;
    assert_eq!(
        next_json(&mut observer, 5).await["type"],
        "presence_changed"
    );
    let (first_connection, first_lease_ms): (Uuid, i64) = sqlx::query_as(
        "SELECT connection_id, (EXTRACT(EPOCH FROM lease_until) * 1000)::bigint FROM device_sockets WHERE vault_id=$1 AND device_id=$2 AND responsive",
    )
    .bind(server.vault)
    .bind(device.id)
    .fetch_one(&server.pool)
    .await
    .unwrap();
    answer_ping(&mut second, "second socket initial challenge", 5).await;
    wait_for_responsive_sockets(&server, device.id, 2).await;
    assert_no_text_frame(&mut observer, Duration::from_millis(500)).await;
    sqlx::query("UPDATE device_sockets SET lease_until=now()-interval '1 second' WHERE vault_id=$1 AND connection_id=$2")
        .bind(server.vault)
        .bind(first_connection)
        .execute(&server.pool)
        .await
        .unwrap();
    answer_ping(&mut first, "first socket renewal challenge", 25).await;
    wait_until(5, "first socket renewed beyond its original lease", || {
        let pool = server.pool.clone();
        async move {
            sqlx::query_scalar::<_, bool>(
                "SELECT (EXTRACT(EPOCH FROM lease_until) * 1000)::bigint > $3 FROM device_sockets WHERE vault_id=$1 AND connection_id=$2",
            )
            .bind(server.vault)
            .bind(first_connection)
            .bind(first_lease_ms)
            .fetch_one(&pool)
            .await
            .unwrap()
        }
    })
    .await;
    first.close(None).await.unwrap();
    wait_for_responsive_sockets(&server, device.id, 1).await;
    second.close(None).await.unwrap();
    wait_for_responsive_sockets(&server, device.id, 0).await;
    observer.close(None).await.unwrap();
    server.shutdown().await;
}

#[tokio::test]
async fn websocket_draft_hints_are_ready_gated_vault_scoped_and_coalesced() {
    let server = TestServer::start().await;
    let foreign_vault = Uuid::new_v4();
    let (foreign_profile, foreign_fingerprint) = profile(foreign_vault, 1);
    let foreign = create_owner(
        &server.pool,
        foreign_profile,
        vec![1],
        foreign_fingerprint,
        1,
    )
    .await
    .unwrap();

    let mut observer = server.connect_ws(&server.owner_token).await;
    send_hello(&mut observer, "0").await;
    assert_eq!(next_json(&mut observer, 3).await["type"], "ready");
    assert_eq!(
        next_json(&mut observer, 3).await["type"],
        "presence_changed"
    );

    let mut foreign_observer = server.connect_ws(&foreign.device_token).await;
    send_hello(&mut foreign_observer, "0").await;
    assert_eq!(next_json(&mut foreign_observer, 3).await["type"], "ready");
    assert_eq!(
        next_json(&mut foreign_observer, 3).await["type"],
        "presence_changed"
    );

    let mut unready = server.connect_ws(&server.owner_token).await;
    let (writes_complete, mut observe_complete) = tokio::sync::watch::channel(false);
    let burst_started = tokio::time::Instant::now();
    let send_burst = async {
        for _ in 0..4 {
            server.put_draft(Uuid::new_v4(), Uuid::new_v4()).await;
        }
        writes_complete.send_replace(true);
    };
    let observe_burst = async {
        let mut hints = Vec::new();
        let mut writes_done = false;
        let mut settle = Box::pin(tokio::time::sleep(Duration::from_secs(3_600)));
        loop {
            tokio::select! {
                frame = next_json_frame(&mut observer) => match frame["type"].as_str() {
                    Some("drafts_changed") => {
                        assert_eq!(frame, json!({"type":"drafts_changed"}));
                        hints.push(tokio::time::Instant::now());
                    }
                    Some("presence_changed") => {}
                    other => panic!("expected a draft hint or presence hint, got {other:?}: {frame}"),
                },
                changed = observe_complete.changed(), if !writes_done => {
                    assert!(changed.is_ok(), "draft writes ended without completion");
                    writes_done = true;
                    settle
                        .as_mut()
                        .reset(tokio::time::Instant::now() + Duration::from_millis(600));
                }
                _ = &mut settle, if writes_done => break,
            }
        }
        hints
    };
    let ((), hint_times) = tokio::join!(send_burst, observe_burst);
    assert!(
        hint_times.len() >= 2,
        "a burst must produce leading and trailing hints"
    );
    for (index, received_at) in hint_times.into_iter().enumerate() {
        let minimum_elapsed = Duration::from_millis(500 * index as u64);
        assert!(
            received_at.duration_since(burst_started) >= minimum_elapsed,
            "draft hint {index} arrived before its 500 ms rate-limit window"
        );
    }
    assert_no_text_frame(&mut foreign_observer, Duration::from_millis(600)).await;
    assert_no_text_frame(&mut unready, Duration::from_millis(600)).await;
    server.shutdown().await;
}

#[tokio::test]
async fn websocket_adaptive_keepalive_ready_frame_is_opt_in() {
    let server = TestServer::start().await;
    let mut adaptive = server.connect_ws(&server.owner_token).await;
    send_adaptive_hello(&mut adaptive, "0").await;
    let ready = next_json(&mut adaptive, 3).await;
    assert_eq!(ready["type"], "ready");
    assert_eq!(
        ready["keepalive"],
        json!({"interval_ms": 50_000, "peers": false})
    );
    assert_eq!(
        ready["frame_types"].as_array().unwrap().last(),
        Some(&json!("keepalive"))
    );

    for value in [json!("fast"), json!(true), json!(7)] {
        let mut legacy = server.connect_ws(&server.owner_token).await;
        legacy
            .send(tungstenite::Message::Text(
                json!({"protocol_version":1,"resume_cursor":"0","keepalive":value})
                    .to_string()
                    .into(),
            ))
            .await
            .unwrap();
        let ready = next_json(&mut legacy, 3).await;
        assert!(ready.get("keepalive").is_none());
        assert!(
            !ready["frame_types"]
                .as_array()
                .unwrap()
                .contains(&json!("keepalive"))
        );
    }
    server.shutdown().await;
}

#[tokio::test]
async fn websocket_legacy_keepalive_is_unchanged() {
    let server = TestServer::start_with(keepalive_options()).await;
    let (mut ws, ready) = open_socket(&server, &server.owner_token, false).await;
    assert!(ready.get("keepalive").is_none());
    assert_eq!(
        ready["frame_types"],
        json!([
            "ready",
            "event",
            "resync_required",
            "presence_changed",
            "drafts_changed"
        ])
    );

    let first = next_ping(&mut ws, Duration::from_millis(750)).await;
    assert!(first.texts.iter().all(|frame| frame["type"] != "keepalive"));
    let mut previous = first.at;
    pong(&mut ws, first.payload).await;
    wait_for_responsive_sockets(&server, server.owner_device, 1).await;
    let lease = lease_seconds_left(&server.pool, server.vault, server.owner_device).await;
    assert!(
        (50.0..=61.0).contains(&lease),
        "legacy Pong lease: {lease}s"
    );

    for _ in 0..3 {
        let ping = next_ping(&mut ws, FAST + Duration::from_millis(600)).await;
        let gap = ping.at.duration_since(previous);
        assert!(
            gap >= FAST / 2 && gap < IDLE,
            "legacy Ping gap {gap:?} is not the fast cadence"
        );
        assert!(
            ping.texts.iter().all(|frame| frame["type"] != "keepalive"),
            "legacy socket received a keepalive frame: {:?}",
            ping.texts
        );
        previous = ping.at;
        pong(&mut ws, ping.payload).await;
    }
    ws.close(None).await.unwrap();
    server.shutdown().await;
}

/// A lone adaptive socket: the first challenge is immediate and its keepalive reports FAST
/// (no idle-length lease exists yet), the second is one FAST later and reports IDLE, and
/// later challenges follow the idle cadence.
#[tokio::test]
async fn websocket_adaptive_keepalive_idles_when_alone() {
    let server = TestServer::start_with(keepalive_options()).await;
    let android = provision_kind(&server, "android").await;
    let (mut ws, ready) = open_socket(&server, &android.token, true).await;
    assert_eq!(
        ready["keepalive"],
        json!({"interval_ms": IDLE.as_millis() as u64, "peers": false})
    );

    let ready_at = tokio::time::Instant::now();
    let first = next_ping(&mut ws, Duration::from_millis(750)).await;
    assert!(
        first.at.duration_since(ready_at) < IDLE / 2,
        "the first Ping waited for the idle interval"
    );
    let first_at = first.at;
    let (frame, remaining) =
        answer_with_lease_check(&server, android.id, &mut ws, first, false).await;
    assert_eq!(frame, keepalive_frame(FAST, false));
    assert!(
        (110.0..=121.0).contains(&remaining),
        "idle Pong lease: {remaining}s"
    );
    wait_for_responsive_sockets(&server, android.id, 1).await;

    let second = next_ping(&mut ws, FAST + Duration::from_millis(600)).await;
    assert!(
        second.at.duration_since(first_at) >= FAST / 2,
        "the second challenge must follow one FAST interval"
    );
    let second_at = second.at;
    let (frame, _) = answer_with_lease_check(&server, android.id, &mut ws, second, false).await;
    assert_eq!(frame, keepalive_frame(IDLE, false));

    let base = tokio::time::Instant::now();
    assert_quiet(&mut ws, FAST * 2).await;
    wait_for_responsive_sockets(&server, android.id, 1).await;
    let third = next_ping(
        &mut ws,
        (second_at + IDLE + Duration::from_secs(1)).saturating_duration_since(base),
    )
    .await;
    assert!(
        third.at.duration_since(second_at) >= IDLE / 2,
        "the idle cadence must follow the second challenge"
    );
    let (frame, _) = answer_with_lease_check(&server, android.id, &mut ws, third, false).await;
    assert_eq!(frame, keepalive_frame(IDLE, false));
    ws.close(None).await.unwrap();
    server.shutdown().await;
}

#[tokio::test]
async fn websocket_adaptive_keepalive_speeds_up_when_a_peer_connects() {
    let server = TestServer::start_with(keepalive_options()).await;
    let (_, mut android, desktop) = speed_up_with_desktop(&server).await;
    desktop.abort();
    let _ = desktop.await;
    android.close(None).await.unwrap();
    server.shutdown().await;
}

#[tokio::test]
async fn websocket_adaptive_keepalive_idles_again_when_the_peer_leaves() {
    let server = TestServer::start_with(keepalive_options()).await;
    let (android_id, mut android, desktop) = speed_up_with_desktop(&server).await;

    // The peer leaves right after a fast-mode Ping and its keepalive, so the next scheduled tick is
    // one FAST interval away: the changed frame must come first, with no Ping, well inside that.
    let trigger = tokio::time::Instant::now();
    desktop.abort();
    let (frame, left) =
        expect_change_without_ping(&mut android, Duration::from_secs(1), false).await;
    assert_eq!(frame, keepalive_frame(IDLE, false));
    assert!(
        left.duration_since(trigger) < FAST / 2,
        "the leave must be announced well before the next scheduled tick"
    );
    let _ = desktop.await;

    // The tick armed while the peer was present still fires, and it arms one FAST interval:
    // the last renewal was at the fast length, so no idle-length lease exists yet.
    let first_after = next_ping(&mut android, FAST + Duration::from_millis(600)).await;
    let first_after_at = first_after.at;
    let (frame, remaining) =
        answer_with_lease_check(&server, android_id, &mut android, first_after, false).await;
    assert_eq!(frame, keepalive_frame(FAST, false));
    assert!(remaining > 100.0, "idle renewal lease: {remaining}s");

    // Now the renewal is at the idle length, so the cadence is IDLE.
    let second = next_ping(&mut android, FAST + Duration::from_millis(600)).await;
    assert!(
        second.at.duration_since(first_after_at) >= FAST / 2,
        "the second challenge must follow one FAST interval"
    );
    let second_at = second.at;
    let (frame, _) =
        answer_with_lease_check(&server, android_id, &mut android, second, false).await;
    assert_eq!(frame, keepalive_frame(IDLE, false));

    let base = tokio::time::Instant::now();
    assert_quiet(&mut android, FAST * 2).await;
    let third = next_ping(
        &mut android,
        (second_at + IDLE + Duration::from_secs(1)).saturating_duration_since(base),
    )
    .await;
    assert!(
        third.at.duration_since(second_at) >= IDLE / 2,
        "the idle cadence must follow the second challenge"
    );
    let (frame, _) = answer_with_lease_check(&server, android_id, &mut android, third, false).await;
    assert_eq!(frame, keepalive_frame(IDLE, false));
    android.close(None).await.unwrap();
    server.shutdown().await;
}

#[tokio::test]
async fn websocket_adaptive_keepalive_counts_only_non_android_peers() {
    let server = TestServer::start_with(keepalive_options()).await;
    let (android, mut ws) = adaptive_android_alone(&server).await;

    let other_android = provision_kind(&server, "android").await;
    let other_android_responder = connect_responsive(&server, &other_android).await;
    let frames = keepalives_for(&mut ws, FAST * 2 + IDLE).await;
    assert!(
        !frames.is_empty(),
        "an idle socket still sends keepalive frames"
    );
    assert!(
        frames.iter().all(|frame| frame["peers"] == false),
        "a second Android device must not count as a peer: {frames:?}"
    );
    assert!(
        !presence::peer_connected(&server.pool, server.vault, android.id)
            .await
            .unwrap()
    );
    other_android_responder.abort();
    let _ = other_android_responder.await;

    for kind in ["browser", "ios"] {
        let peer = provision_kind(&server, kind).await;
        let responder = connect_responsive(&server, &peer).await;
        let (frame, _) = expect_peers_change(&mut ws, Duration::from_secs(1), true).await;
        assert_eq!(
            frame,
            keepalive_frame(FAST, true),
            "{kind} peer must switch to fast mode"
        );
        responder.abort();
        let _ = responder.await;
        let (frame, _) = expect_peers_change(&mut ws, Duration::from_secs(1), false).await;
        assert_eq!(
            frame,
            keepalive_frame(IDLE, false),
            "{kind} peer leaving must return to idle mode"
        );
    }

    // Direct complement: each kind's responsive lease row, then the same row after revocation.
    let plain = server.provision_device("device").await;
    let mut peers = vec![(plain.id, true)];
    for (kind, counts) in [
        ("desktop", true),
        ("browser", true),
        ("ios", true),
        ("android", false),
    ] {
        peers.push((provision_kind(&server, kind).await.id, counts));
    }
    for (device, counts) in peers {
        insert_responsive_socket(&server, device).await;
        assert_eq!(
            presence::peer_connected(&server.pool, server.vault, android.id)
                .await
                .unwrap(),
            counts,
            "device {device} must count: {counts}"
        );
        sqlx::query("UPDATE devices SET revoked_at=now() WHERE vault_id=$1 AND device_id=$2")
            .bind(server.vault)
            .bind(device)
            .execute(&server.pool)
            .await
            .unwrap();
        assert!(
            !presence::peer_connected(&server.pool, server.vault, android.id)
                .await
                .unwrap(),
            "a revoked device {device} must not count"
        );
    }
    ws.close(None).await.unwrap();
    server.shutdown().await;
}

/// Without an idle-length lease the first challenge is followed by one FAST tick, so an
/// unanswered first challenge disconnects after about FAST, not IDLE.
#[tokio::test]
async fn websocket_unanswered_challenge_disconnects_in_both_keepalive_modes() {
    let server = TestServer::start_with(keepalive_options()).await;

    let idle = provision_kind(&server, "android").await;
    let (mut ws, _) = open_socket(&server, &idle.token, true).await;
    let challenge = next_ping(&mut ws, Duration::from_millis(750)).await;
    let closed_at =
        wait_for_socket_removal(&server, idle.id, FAST + Duration::from_millis(1_500)).await;
    let elapsed = closed_at.duration_since(challenge.at);
    assert!(
        elapsed >= FAST / 2 && elapsed <= FAST + Duration::from_millis(1_500),
        "idle socket closed after {elapsed:?}, expected about one fast tick"
    );
    wait_for_close(&mut ws, Duration::from_secs(2)).await;

    // Fast: a responsive desktop peer makes the next tick FAST after the challenge.
    let desktop = server.provision_device("device").await;
    let responder = connect_responsive(&server, &desktop).await;
    wait_for_responsive_sockets(&server, desktop.id, 1).await;
    let fast = provision_kind(&server, "android").await;
    let (mut ws, ready) = open_socket(&server, &fast.token, true).await;
    assert_eq!(
        ready["keepalive"],
        json!({"interval_ms": FAST.as_millis() as u64, "peers": true})
    );
    let challenge = next_ping(&mut ws, Duration::from_millis(750)).await;
    let closed_at =
        wait_for_socket_removal(&server, fast.id, FAST + Duration::from_millis(1_500)).await;
    let elapsed = closed_at.duration_since(challenge.at);
    assert!(
        elapsed >= FAST / 2 && elapsed <= FAST + Duration::from_millis(1_500),
        "fast socket closed after {elapsed:?}, expected about one fast tick"
    );
    wait_for_close(&mut ws, Duration::from_secs(2)).await;
    responder.abort();
    let _ = responder.await;
    server.shutdown().await;
}

/// Pins the pull-in rule: an idle-cadence challenge left unanswered when a peer connects is
/// disconnected at the pulled-in tick, one FAST interval after the switch, long before the
/// original idle deadline. The anchor is the desktop's first Pong, which this test sends itself;
/// the android socket is not read, so its challenge stays unanswered. No lower bound is asserted:
/// the close cannot be anchored more tightly than the presence notice's latency.
#[tokio::test]
async fn websocket_outstanding_idle_challenge_closes_one_fast_interval_after_the_switch() {
    let server = TestServer::start_with(keepalive_options()).await;
    let (android, mut ws) = adaptive_android_alone(&server).await;
    // Left unread: the socket is not read again until it is closed, so its Pong is never sent.
    let held = next_ping(&mut ws, IDLE + Duration::from_secs(1)).await;

    let desktop = server.provision_device("device").await;
    let (mut desktop_ws, _) = open_socket(&server, &desktop.token, false).await;
    let first = next_ping(&mut desktop_ws, Duration::from_secs(3)).await;
    let anchor = tokio::time::Instant::now();
    pong(&mut desktop_ws, first.payload).await;
    let responder = spawn_responder(desktop_ws);

    let closed_at =
        wait_for_socket_removal(&server, android.id, IDLE + Duration::from_secs(1)).await;
    let after_switch = closed_at.duration_since(anchor);
    assert!(
        after_switch <= FAST + Duration::from_secs(1),
        "closed {after_switch:?} after the peer's Pong; the pulled-in tick is one FAST interval after the switch"
    );
    assert!(
        closed_at.duration_since(held.at) < IDLE / 2,
        "closed near the original idle deadline: the pull-in did not happen"
    );
    responder.abort();
    let _ = responder.await;
    wait_for_close(&mut ws, Duration::from_secs(2)).await;
    server.shutdown().await;
}

/// The scheduled-discovery path, made deterministic: a desktop row becomes responsive by direct SQL,
/// so no presence notice reaches the android socket. The next scheduled tick must find `peers:true`
/// itself: its keepalive reports the new state, and the Ping after it comes one FAST interval later.
#[tokio::test]
async fn websocket_scheduled_tick_discovers_a_peer_without_a_notice() {
    let server = TestServer::start_with(keepalive_options()).await;
    let (android, mut ws) = adaptive_android_alone(&server).await;
    let desktop = server.provision_device("device").await;
    insert_responsive_socket(&server, desktop.id).await;

    // The idle tick after the second challenge is still IDLE away, so the row is in place first.
    let discovered = next_ping(&mut ws, IDLE + Duration::from_secs(1)).await;
    assert!(
        discovered
            .texts
            .iter()
            .all(|frame| frame["type"] != "keepalive"),
        "the change must not be announced before the tick: {:?}",
        discovered.texts
    );
    let discovered_at = discovered.at;
    let (frame, _) = answer_with_lease_check(&server, android.id, &mut ws, discovered, true).await;
    assert_eq!(frame, keepalive_frame(FAST, true));

    let next = next_ping(&mut ws, FAST + Duration::from_millis(600)).await;
    assert!(
        next.at.duration_since(discovered_at) >= FAST / 2,
        "the Ping after the discovery tick must follow one FAST interval"
    );
    let (frame, _) = answer_with_lease_check(&server, android.id, &mut ws, next, true).await;
    assert_eq!(frame, keepalive_frame(FAST, true));
    ws.close(None).await.unwrap();
    server.shutdown().await;
}

#[tokio::test]
async fn websocket_handshake_replays_and_revocation_closes_active_socket() {
    let server = TestServer::start().await;
    let device = server.provision_device("device").await;
    let body = set_fingerprint(
        event(server.vault, server.owner_device, 1, Uuid::new_v4(), 4),
        &server.fingerprint,
    );
    assert_eq!(
        server
            .auth(Client::new().post(format!("{}/v1/events", server.base_url)))
            .json(&body)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );

    let mut ws = server.connect_ws(&device.token).await;
    send_hello(&mut ws, "0").await;
    let ready = next_json(&mut ws, 2).await;
    assert_eq!(ready["type"], "ready");
    assert_eq!(ready["protocol_version"], 1);
    assert_eq!(ready["resume_cursor"], "0");
    assert_eq!(ready["high_water_cursor"], "1");
    let frame = next_json(&mut ws, 2).await;
    assert_eq!(frame["type"], "event");
    assert_eq!(frame["cursor"], "1");

    // This committed row deliberately bypasses the API broadcast channel.  A
    // connected peer must still receive it from its bounded durable scan.
    sqlx::query("INSERT INTO event_log(vault_id,cursor,envelope_id,producer_device_id,producer_sequence,purpose,cipher_digest,envelope) VALUES($1,2,$2,$3,2,'event',$4,$5)")
        .bind(server.vault)
        .bind(Uuid::new_v4())
        .bind(server.owner_device)
        .bind(vec![9_u8; 32])
        .bind(json!({"protocol_version":1}))
        .execute(&server.pool)
        .await
        .unwrap();
    sqlx::query("UPDATE vaults SET next_cursor=2 WHERE vault_id=$1")
        .bind(server.vault)
        .execute(&server.pool)
        .await
        .unwrap();
    let replayed = next_durable_event(&mut ws, 7).await;
    assert_eq!(replayed["cursor"], "2");

    let revoked = server
        .auth(Client::new().post(format!(
            "{}/v1/devices/{}/revoke",
            server.base_url, device.id
        )))
        .send()
        .await
        .unwrap();
    assert_eq!(revoked.status(), StatusCode::NO_CONTENT);
    assert_eq!(
        close_code(&mut ws, 22).await,
        4401,
        "revoked active websocket must close with the revoked code"
    );
    server.shutdown().await;
}

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

#[tokio::test]
async fn draft_merge_push_rebases_on_target_and_closes_only_other_live_branches() {
    let server = TestServer::start().await;
    let draft = Uuid::new_v4();
    let revision_a = Uuid::new_v4();
    let revision_b = Uuid::new_v4();
    let merged_revision = Uuid::new_v4();
    let path = |revision| format!("/v1/drafts/{draft}/revisions/{revision}");
    let put = |revision, base: Option<Uuid>, resolves: Vec<Uuid>| {
        Client::new()
            .put(format!("{}{}", server.base_url, path(revision)))
            .bearer_auth(&server.owner_token)
            .json(&json!({
                "base_revision_id": base,
                "sealed_record": URL_SAFE_NO_PAD.encode(b"draft"),
                "attachment_ids": [],
                "authored": true,
                "resolves_branch_ids": resolves,
            }))
    };
    let (status, first) = server.call(put(revision_a, None, vec![])).await;
    assert_eq!(status, StatusCode::OK, "{first}");
    let branch_a: Uuid = serde_json::from_value(first["branch_id"].clone()).unwrap();
    let (status, second) = server.call(put(revision_b, None, vec![])).await;
    assert_eq!(status, StatusCode::OK, "{second}");
    let branch_b: Uuid = serde_json::from_value(second["branch_id"].clone()).unwrap();
    assert_ne!(branch_a, branch_b);

    let (status, merged) = server
        .call(put(merged_revision, Some(revision_b), vec![branch_a]))
        .await;
    assert_eq!(status, StatusCode::OK, "{merged}");
    assert_eq!(merged["branch_id"], second["branch_id"]);
    let live: Vec<Uuid> = sqlx::query_scalar(
        "SELECT branch_id FROM draft_branches WHERE vault_id=$1 AND draft_id=$2 AND state='live'",
    )
    .bind(server.vault)
    .bind(draft)
    .fetch_all(&server.pool)
    .await
    .unwrap();
    assert_eq!(live, vec![branch_b]);
    server.shutdown().await;
}

impl TestServer {
    async fn connect_ws(&self, token: &str) -> Ws {
        let request = tungstenite::http::Request::builder()
            .uri(&self.ws_url)
            .header(
                "Host",
                Url::parse(&self.ws_url).unwrap().host_str().unwrap(),
            )
            .header("Authorization", format!("Bearer {token}"))
            .header("Connection", "Upgrade")
            .header("Upgrade", "websocket")
            .header("Sec-WebSocket-Version", "13")
            .header(
                "Sec-WebSocket-Key",
                tungstenite::handshake::client::generate_key(),
            )
            .body(())
            .unwrap();
        connect_async(request).await.unwrap().0
    }

    /// Sends a request and returns the status plus JSON body (`null` when empty).
    async fn call(&self, request: reqwest::RequestBuilder) -> (StatusCode, Value) {
        let response = request.send().await.unwrap();
        let status = response.status();
        let text = response.text().await.unwrap();
        let body = if text.is_empty() {
            Value::Null
        } else {
            serde_json::from_str(&text).unwrap_or_else(|_| panic!("non-JSON body: {text}"))
        };
        (status, body)
    }

    async fn get(&self, token: &str, path: &str) -> (StatusCode, Value) {
        self.call(
            Client::new()
                .get(format!("{}{path}", self.base_url))
                .bearer_auth(token),
        )
        .await
    }

    async fn post(&self, token: &str, path: &str, body: &Value) -> (StatusCode, Value) {
        self.call(
            Client::new()
                .post(format!("{}{path}", self.base_url))
                .bearer_auth(token)
                .json(body),
        )
        .await
    }

    async fn put_draft(&self, draft: Uuid, revision: Uuid) {
        let (status, body) = self
            .call(
                Client::new()
                    .put(format!(
                        "{}/v1/drafts/{draft}/revisions/{revision}",
                        self.base_url
                    ))
                    .bearer_auth(&self.owner_token)
                    .json(&json!({
                        "base_revision_id": null,
                        "sealed_record": URL_SAFE_NO_PAD.encode(b"draft"),
                        "attachment_ids": [],
                        "authored": true,
                    })),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }

    /// A valid owner-produced event at the vault's initial epoch.
    fn owner_event(&self, sequence: u64) -> Value {
        set_fingerprint(
            event(
                self.vault,
                self.owner_device,
                sequence,
                Uuid::new_v4(),
                sequence as u8,
            ),
            &self.fingerprint,
        )
    }

    async fn commit(&self, body: &Value) -> String {
        let (status, accepted) = self.post(&self.owner_token, "/v1/events", body).await;
        assert_eq!(status, StatusCode::OK, "{accepted}");
        accepted["cursor"].as_str().unwrap().to_owned()
    }
}

async fn send_hello(ws: &mut Ws, resume_cursor: &str) {
    ws.send(tungstenite::Message::Text(
        json!({"protocol_version":1,"resume_cursor":resume_cursor})
            .to_string()
            .into(),
    ))
    .await
    .unwrap();
}

async fn send_adaptive_hello(ws: &mut Ws, resume_cursor: &str) {
    ws.send(tungstenite::Message::Text(
        json!({"protocol_version":1,"resume_cursor":resume_cursor,"keepalive":"adaptive"})
            .to_string()
            .into(),
    ))
    .await
    .unwrap();
}

async fn answer_ping(ws: &mut Ws, label: &str, seconds: u64) {
    tokio::time::timeout(Duration::from_secs(seconds), async {
        loop {
            match ws.next().await {
                Some(Ok(tungstenite::Message::Ping(payload))) => {
                    ws.send(tungstenite::Message::Pong(payload)).await.unwrap();
                    return;
                }
                Some(Ok(tungstenite::Message::Text(_)))
                | Some(Ok(tungstenite::Message::Pong(_))) => {}
                other => panic!("{label} stream ended before its Ping: {other:?}"),
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for {label}"));
}

async fn wait_for_responsive_sockets(server: &TestServer, device: Uuid, expected: i64) {
    wait_until(5, "responsive socket commits", || {
        let pool = server.pool.clone();
        async move {
            let actual: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM device_sockets WHERE vault_id=$1 AND device_id=$2 AND responsive AND lease_until > now()",
            )
            .bind(server.vault)
            .bind(device)
            .fetch_one(&pool)
            .await
            .unwrap();
            actual == expected
        }
    })
    .await;
}

/// Reads the next text frame as JSON, responding to control pings.
async fn next_json_frame(ws: &mut Ws) -> Value {
    loop {
        match ws.next().await {
            Some(Ok(tungstenite::Message::Text(text))) => {
                return serde_json::from_str::<Value>(&text).unwrap();
            }
            Some(Ok(tungstenite::Message::Ping(payload))) => {
                ws.send(tungstenite::Message::Pong(payload)).await.unwrap();
            }
            Some(Ok(tungstenite::Message::Pong(_))) => {}
            other => panic!("expected a text frame, got {other:?}"),
        }
    }
}

async fn next_json(ws: &mut Ws, seconds: u64) -> Value {
    tokio::time::timeout(Duration::from_secs(seconds), next_json_frame(ws))
        .await
        .expect("timed out waiting for a websocket frame")
}

/// Waits for a durable event while allowing the advertised non-durable presence hint.
async fn next_durable_event(ws: &mut Ws, seconds: u64) -> Value {
    tokio::time::timeout(Duration::from_secs(seconds), async {
        loop {
            let frame = next_json_frame(ws).await;
            match frame["type"].as_str() {
                Some("event") => return frame,
                Some("presence_changed") => {}
                other => panic!("expected an event or presence hint, got {other:?}: {frame}"),
            }
        }
    })
    .await
    .expect("timed out waiting for a durable websocket event")
}

async fn assert_no_text_frame(ws: &mut Ws, duration: Duration) {
    let received = tokio::time::timeout(duration, next_json_frame(ws)).await;
    assert!(
        received.is_err(),
        "unexpected websocket frame: {received:?}"
    );
}

async fn assert_no_durable_event(ws: &mut Ws, duration: Duration) {
    let received = tokio::time::timeout(duration, async {
        loop {
            let frame = next_json_frame(ws).await;
            match frame["type"].as_str() {
                Some("presence_changed") => {}
                Some("event") => return frame,
                other => panic!("unexpected websocket frame {other:?}: {frame}"),
            }
        }
    })
    .await;
    assert!(
        received.is_err(),
        "unexpected duplicate event: {received:?}"
    );
}

/// Waits for the server close frame and returns its code. Text frames that
/// precede it (for example `resync_required`) are skipped.
async fn close_code(ws: &mut Ws, seconds: u64) -> u16 {
    tokio::time::timeout(Duration::from_secs(seconds), async {
        loop {
            match ws.next().await {
                Some(Ok(tungstenite::Message::Close(Some(frame)))) => return u16::from(frame.code),
                Some(Ok(tungstenite::Message::Close(None))) => return 1005,
                Some(Ok(_)) => {}
                other => panic!("socket ended without a close frame: {other:?}"),
            }
        }
    })
    .await
    .expect("timed out waiting for a websocket close")
}

fn cursors(items: &Value) -> Vec<u64> {
    items
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["cursor"].as_str().unwrap().parse().unwrap())
        .collect()
}

async fn wait_until<F, Fut>(seconds: u64, what: &str, mut condition: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let deadline = tokio::time::Instant::now() + Duration::from_secs(seconds);
    while !condition().await {
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {what}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Adaptive keepalive cadences: `FAST` while a responsive non-android peer is present, `IDLE` when alone.
// Wide enough that every lower bound keeps at least FAST/2 = 400 ms of slack on a loaded runner.
const FAST: Duration = Duration::from_millis(800);
const IDLE: Duration = Duration::from_secs(3);

fn keepalive_options() -> TransportOptions {
    TransportOptions {
        heartbeat_interval: FAST,
        idle_heartbeat_interval: IDLE,
        ..Default::default()
    }
}

fn keepalive_frame(interval: Duration, peers: bool) -> Value {
    json!({"type": "keepalive", "interval_ms": interval.as_millis() as u64, "peers": peers})
}

/// Provisions a device and sets its `devices.kind`, which the harness leaves NULL (desktop).
async fn provision_kind(server: &TestServer, kind: &str) -> TestDevice {
    let device = server.provision_device("device").await;
    sqlx::query("UPDATE devices SET kind=$1 WHERE vault_id=$2 AND device_id=$3")
        .bind(kind)
        .bind(server.vault)
        .bind(device.id)
        .execute(&server.pool)
        .await
        .unwrap();
    device
}

/// Connects with a legacy or adaptive hello and returns the socket with its ready frame.
async fn open_socket(server: &TestServer, token: &str, adaptive: bool) -> (Ws, Value) {
    let mut ws = server.connect_ws(token).await;
    if adaptive {
        send_adaptive_hello(&mut ws, "0").await;
    } else {
        send_hello(&mut ws, "0").await;
    }
    let ready = next_json(&mut ws, 3).await;
    assert_eq!(ready["type"], "ready");
    (ws, ready)
}

/// Connects a legacy desktop-style socket, answers its first Ping, waits until it is responsive,
/// and keeps answering in the background.
async fn connect_responsive(server: &TestServer, device: &TestDevice) -> JoinHandle<()> {
    let (mut ws, _) = open_socket(server, &device.token, false).await;
    let first = next_ping(&mut ws, Duration::from_secs(3)).await;
    pong(&mut ws, first.payload).await;
    wait_for_responsive_sockets(server, device.id, 1).await;
    spawn_responder(ws)
}

/// Answers every Ping in the background so a socket that is not under test stays responsive.
fn spawn_responder(mut ws: Ws) -> JoinHandle<()> {
    tokio::spawn(async move {
        while let Some(Ok(message)) = ws.next().await {
            if let tungstenite::Message::Ping(payload) = message {
                let _ = ws.send(tungstenite::Message::Pong(payload)).await;
            }
        }
    })
}

/// An android-kind adaptive socket alone and responsive. Its first challenge is immediate, its
/// second follows one FAST interval later (the first renewal was at the fast length), and its
/// keepalive frames report FAST then IDLE. Returns with no challenge outstanding.
async fn adaptive_android_alone(server: &TestServer) -> (TestDevice, Ws) {
    let android = provision_kind(server, "android").await;
    let (mut ws, _) = open_socket(server, &android.token, true).await;
    let first = next_ping(&mut ws, Duration::from_millis(750)).await;
    let first_at = first.at;
    let (frame, _) = answer_with_lease_check(server, android.id, &mut ws, first, false).await;
    assert_eq!(frame, keepalive_frame(FAST, false));
    let second = next_ping(&mut ws, FAST + Duration::from_millis(600)).await;
    assert!(second.at.duration_since(first_at) >= FAST / 2);
    let (frame, _) = answer_with_lease_check(server, android.id, &mut ws, second, false).await;
    assert_eq!(frame, keepalive_frame(IDLE, false));
    wait_for_responsive_sockets(server, android.id, 1).await;
    (android, ws)
}

/// Connects a desktop peer to an idle adaptive android socket and checks the switch to fast mode.
///
/// The android socket's idle challenge stays outstanding while the peer connects: the socket is
/// not read until the peer is responsive. The first read flushes tungstenite's automatic Pong,
/// which then reaches the server after the switch and must not disconnect the socket. Returns the
/// android device id, its socket and the peer's responder.
async fn speed_up_with_desktop(server: &TestServer) -> (Uuid, Ws, JoinHandle<()>) {
    let (android, mut ws) = adaptive_android_alone(server).await;
    // The switch is triggered a few milliseconds after this challenge, far inside its IDLE
    // window, so no scheduled tick can precede the changed frame: the strict reader applies.
    let held = next_ping(&mut ws, IDLE + Duration::from_secs(1)).await;

    let desktop = server.provision_device("device").await;
    let responder = connect_responsive(server, &desktop).await;
    let (frame, switched) = expect_change_without_ping(&mut ws, Duration::from_secs(1), true).await;
    assert_eq!(frame, keepalive_frame(FAST, true));
    assert!(
        switched.duration_since(held.at) < IDLE / 2,
        "the switch must land well inside the idle window of the held challenge"
    );

    let fast = next_ping(&mut ws, FAST + Duration::from_millis(600)).await;
    assert!(
        fast.at.duration_since(switched) >= FAST / 2,
        "the switch did not pull the next Ping to the fast cadence"
    );
    let (frame, remaining) = answer_with_lease_check(server, android.id, &mut ws, fast, true).await;
    assert_eq!(frame, keepalive_frame(FAST, true));
    assert!(
        (50.0..=61.0).contains(&remaining),
        "fast lease after the switch: {remaining}s"
    );
    (android.id, ws, responder)
}

/// Seconds left on the lease of `device`'s responsive socket row.
async fn lease_seconds_left(pool: &PgPool, vault: Uuid, device: Uuid) -> f64 {
    sqlx::query_scalar::<_, f64>(
        "SELECT EXTRACT(EPOCH FROM lease_until - now())::float8 FROM device_sockets WHERE vault_id=$1 AND device_id=$2 AND responsive",
    )
    .bind(vault)
    .bind(device)
    .fetch_one(pool)
    .await
    .unwrap()
}

/// The device's socket lease as (`lease_until` in microseconds since the epoch, seconds left).
async fn lease_snapshot(pool: &PgPool, vault: Uuid, device: Uuid) -> (i64, f64) {
    sqlx::query_as::<_, (i64, f64)>(
        "SELECT (EXTRACT(EPOCH FROM lease_until) * 1000000)::bigint, EXTRACT(EPOCH FROM lease_until - now())::float8 FROM device_sockets WHERE vault_id=$1 AND device_id=$2 ORDER BY lease_until DESC LIMIT 1",
    )
    .bind(vault)
    .bind(device)
    .fetch_one(pool)
    .await
    .unwrap()
}

/// Waits until a Pong has replaced the lease observed as `before`, then returns its seconds left.
async fn renewed_lease_seconds_left(pool: &PgPool, vault: Uuid, device: Uuid, before: i64) -> f64 {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let (until, remaining) = lease_snapshot(pool, vault, device).await;
        if until != before {
            return remaining;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the Pong did not renew the lease"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// A responsive, unexpired socket row for `device`, inserted directly.
async fn insert_responsive_socket(server: &TestServer, device: Uuid) {
    sqlx::query("INSERT INTO device_sockets(connection_id,vault_id,device_id,lease_until,responsive) VALUES($1,$2,$3,now()+make_interval(secs => 60),TRUE)")
        .bind(Uuid::new_v4())
        .bind(server.vault)
        .bind(device)
        .execute(&server.pool)
        .await
        .unwrap();
}

/// Answers `ping`, reads the keepalive frame that follows it, and checks the lease its Pong writes:
/// the lease must outlive the interval that frame reports (`remaining > 2 × interval`), so the
/// socket cannot be hidden between two checks. Returns the frame and the lease seconds left.
async fn answer_with_lease_check(
    server: &TestServer,
    device: Uuid,
    ws: &mut Ws,
    ping: ControlPing,
    peers: bool,
) -> (Value, f64) {
    let (before, _) = lease_snapshot(&server.pool, server.vault, device).await;
    pong(ws, ping.payload).await;
    let frame = next_keepalive(ws, Duration::from_secs(1), peers).await;
    let reported = frame["interval_ms"].as_u64().unwrap() as f64 / 1000.0;
    let remaining = renewed_lease_seconds_left(&server.pool, server.vault, device, before).await;
    assert!(
        remaining > 2.0 * reported,
        "the lease ({remaining}s) must outlive the reported interval ({reported}s)"
    );
    (frame, remaining)
}

async fn pong(ws: &mut Ws, payload: Vec<u8>) {
    ws.send(tungstenite::Message::Pong(payload.into()))
        .await
        .unwrap();
}

/// A control Ping, the text frames that arrived before it, and when it was read.
struct ControlPing {
    payload: Vec<u8>,
    at: tokio::time::Instant,
    texts: Vec<Value>,
}

/// The inbound frames the keepalive tests distinguish. Pongs and binary frames are skipped.
enum Wire {
    Ping(Vec<u8>),
    Text(Value),
    Closed,
}

/// The next distinguished frame, or `None` when `deadline` passes first.
async fn next_wire(ws: &mut Ws, deadline: tokio::time::Instant) -> Option<Wire> {
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        match tokio::time::timeout(remaining, ws.next()).await {
            Err(_) => return None,
            Ok(Some(Ok(tungstenite::Message::Ping(payload)))) => {
                return Some(Wire::Ping(payload.to_vec()));
            }
            Ok(Some(Ok(tungstenite::Message::Text(text)))) => {
                return Some(Wire::Text(serde_json::from_str(&text).unwrap()));
            }
            Ok(Some(Ok(tungstenite::Message::Close(_)))) | Ok(None) | Ok(Some(Err(_))) => {
                return Some(Wire::Closed);
            }
            Ok(Some(Ok(_))) => {}
        }
    }
}

/// Waits for the next Ping without answering it. Presence hints are skipped; other text is kept.
/// tokio-tungstenite queues a Pong for the Ping and sends it on the next read, write or flush,
/// so a challenge stays unanswered only while the socket is not read.
async fn next_ping(ws: &mut Ws, within: Duration) -> ControlPing {
    let deadline = tokio::time::Instant::now() + within;
    let mut texts = Vec::new();
    loop {
        match next_wire(ws, deadline).await {
            Some(Wire::Ping(payload)) => {
                return ControlPing {
                    payload,
                    at: tokio::time::Instant::now(),
                    texts,
                };
            }
            Some(Wire::Text(frame)) if frame["type"] == "presence_changed" => {}
            Some(Wire::Text(frame)) => texts.push(frame),
            Some(Wire::Closed) => panic!("socket closed while waiting for a Ping"),
            None => panic!("no Ping within {within:?}"),
        }
    }
}

/// Waits for a keepalive frame reporting `peers`, answering Pings on the way.
async fn next_keepalive(ws: &mut Ws, within: Duration, peers: bool) -> Value {
    let deadline = tokio::time::Instant::now() + within;
    loop {
        match next_wire(ws, deadline).await {
            Some(Wire::Ping(payload)) => pong(ws, payload).await,
            Some(Wire::Text(frame)) if frame["type"] == "keepalive" && frame["peers"] == peers => {
                return frame;
            }
            Some(Wire::Text(_)) => {}
            Some(Wire::Closed) => panic!("socket closed while waiting for keepalive peers={peers}"),
            None => panic!("no keepalive with peers={peers} within {within:?}"),
        }
    }
}

/// Waits for the keepalive frame reporting the new `peers` value after a change, and when it was
/// received. Both legal orders are accepted: the changed frame alone, or a scheduled Ping followed
/// by a frame that already reports the new state (a tick that discovered the change itself). Pings
/// are answered on the way. Use this where the change is not synchronized with the heartbeat.
async fn expect_peers_change(
    ws: &mut Ws,
    within: Duration,
    peers: bool,
) -> (Value, tokio::time::Instant) {
    let deadline = tokio::time::Instant::now() + within;
    loop {
        match next_wire(ws, deadline).await {
            Some(Wire::Ping(payload)) => pong(ws, payload).await,
            Some(Wire::Text(frame)) if frame["type"] == "keepalive" && frame["peers"] == peers => {
                return (frame, tokio::time::Instant::now());
            }
            Some(Wire::Text(_)) => {}
            Some(Wire::Closed) => panic!("socket closed while waiting for peers={peers}"),
            None => panic!("no peers={peers} keepalive within {within:?}"),
        }
    }
}

/// Strict reader for a change the test triggers at a controlled moment, far from any heartbeat
/// deadline: the changed keepalive frame must arrive with no Ping before it. Callers also bound the
/// time from their trigger, so a late frame cannot hide behind a legitimately scheduled tick.
async fn expect_change_without_ping(
    ws: &mut Ws,
    within: Duration,
    peers: bool,
) -> (Value, tokio::time::Instant) {
    let deadline = tokio::time::Instant::now() + within;
    loop {
        match next_wire(ws, deadline).await {
            Some(Wire::Ping(_)) => {
                panic!("a Ping was sent before the peers={peers} keepalive frame")
            }
            Some(Wire::Text(frame)) if frame["type"] == "keepalive" && frame["peers"] == peers => {
                return (frame, tokio::time::Instant::now());
            }
            Some(Wire::Text(_)) => {}
            Some(Wire::Closed) => panic!("socket closed while waiting for peers={peers}"),
            None => panic!("no peers={peers} keepalive within {within:?}"),
        }
    }
}

/// Collects every keepalive frame for `within`, answering Pings on the way.
async fn keepalives_for(ws: &mut Ws, within: Duration) -> Vec<Value> {
    let deadline = tokio::time::Instant::now() + within;
    let mut frames = Vec::new();
    loop {
        match next_wire(ws, deadline).await {
            Some(Wire::Ping(payload)) => pong(ws, payload).await,
            Some(Wire::Text(frame)) if frame["type"] == "keepalive" => frames.push(frame),
            Some(Wire::Text(_)) => {}
            Some(Wire::Closed) => panic!("socket closed while collecting keepalive frames"),
            None => return frames,
        }
    }
}

/// Asserts that no Ping and no keepalive frame arrives for `within`.
async fn assert_quiet(ws: &mut Ws, within: Duration) {
    let deadline = tokio::time::Instant::now() + within;
    loop {
        match next_wire(ws, deadline).await {
            Some(Wire::Ping(_)) => panic!("unexpected Ping within {within:?}"),
            Some(Wire::Text(frame)) if frame["type"] == "keepalive" => {
                panic!("unexpected keepalive frame within {within:?}: {frame}")
            }
            Some(Wire::Text(_)) => {}
            Some(Wire::Closed) => panic!("socket closed within {within:?}"),
            None => return,
        }
    }
}

/// Waits until the server deletes `device`'s socket row, which happens when the socket closes.
/// The socket is not read meanwhile, so a queued automatic Pong cannot answer the challenge.
async fn wait_for_socket_removal(
    server: &TestServer,
    device: Uuid,
    within: Duration,
) -> tokio::time::Instant {
    let deadline = tokio::time::Instant::now() + within;
    loop {
        let rows: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM device_sockets WHERE vault_id=$1 AND device_id=$2",
        )
        .bind(server.vault)
        .bind(device)
        .fetch_one(&server.pool)
        .await
        .unwrap();
        if rows == 0 {
            return tokio::time::Instant::now();
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "socket row still present after {within:?}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Waits for the server to end the socket and returns when it did. Any Ping is a failure:
/// the unanswered challenge must end the socket at the next tick, not start another.
async fn wait_for_close(ws: &mut Ws, within: Duration) -> tokio::time::Instant {
    let deadline = tokio::time::Instant::now() + within;
    loop {
        match next_wire(ws, deadline).await {
            Some(Wire::Closed) => return tokio::time::Instant::now(),
            Some(Wire::Ping(_)) => panic!("a Ping followed the unanswered challenge"),
            Some(Wire::Text(_)) => {}
            None => panic!("socket still open {within:?} after its challenge"),
        }
    }
}

#[tokio::test]
async fn snapshot_pages_are_stable_under_concurrent_writes_and_converge_with_tail() {
    let server = Arc::new(TestServer::start().await);
    for sequence in 1..=25 {
        server.commit(&server.owner_event(sequence)).await;
    }
    let (status, start) = server.get(&server.owner_token, "/v1/snapshot").await;
    assert_eq!(status, StatusCode::OK, "{start}");
    assert_eq!(start["high_water_cursor"], "25");
    assert_eq!(start["record_count"], "25");
    assert_eq!(start["vault_id"], json!(server.vault));

    let mut writers = Vec::new();
    for sequence in 26..=45_u64 {
        let server = Arc::clone(&server);
        writers.push(tokio::spawn(async move {
            server.commit(&server.owner_event(sequence)).await
        }));
    }
    let mut snapshot = Vec::new();
    let mut after = "0".to_owned();
    loop {
        let (status, page) = server
            .get(
                &server.owner_token,
                &format!("/v1/snapshot/records?high_water=25&after={after}&limit=7"),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{page}");
        assert_eq!(page["high_water_cursor"], "25");
        for record in page["records"].as_array().unwrap() {
            assert_eq!(record["envelope"]["vault_id"], json!(server.vault));
        }
        snapshot.extend(cursors(&page["records"]));
        match page["next_after"].as_str() {
            Some(next) => after = next.to_owned(),
            None => break,
        }
    }
    assert_eq!(snapshot, (1..=25).collect::<Vec<_>>());
    for writer in writers {
        writer.await.unwrap();
    }
    let (_, again) = server
        .get(
            &server.owner_token,
            "/v1/snapshot/records?high_water=25&after=7&limit=7",
        )
        .await;
    assert_eq!(cursors(&again["records"]), (8..=14).collect::<Vec<_>>());

    let mut tail = Vec::new();
    let mut after = 25_u64;
    loop {
        let (status, page) = server
            .get(
                &server.owner_token,
                &format!("/v1/events?after={after}&limit=200"),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{page}");
        let batch = cursors(&page["events"]);
        let Some(last) = batch.last().copied() else {
            break;
        };
        tail.extend(batch);
        after = last;
    }
    snapshot.extend(tail);
    assert_eq!(
        snapshot,
        (1..=45).collect::<Vec<_>>(),
        "cut plus tail must converge without gaps or duplicates"
    );

    let (status, ahead) = server
        .get(&server.owner_token, "/v1/snapshot/records?high_water=46")
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(ahead["code"], "resync_required");
    assert_eq!(ahead["reason"], "cursor_ahead");
    assert_eq!(ahead["high_water_cursor"], "45");
    let (status, _) = server
        .get(
            &server.owner_token,
            "/v1/snapshot/records?high_water=5&after=6",
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let foreign_vault = Uuid::new_v4();
    let (foreign_profile, foreign_fingerprint) = profile(foreign_vault, 1);
    let foreign = create_owner(
        &server.pool,
        foreign_profile,
        vec![1],
        foreign_fingerprint,
        1,
    )
    .await
    .unwrap();
    let (_, foreign_start) = server.get(&foreign.device_token, "/v1/snapshot").await;
    assert_eq!(foreign_start["record_count"], "0");
    let (_, foreign_page) = server
        .get(&foreign.device_token, "/v1/snapshot/records?high_water=0")
        .await;
    assert_eq!(foreign_page["records"], json!([]));
    Arc::try_unwrap(server).ok().unwrap().shutdown().await;
}

#[tokio::test]
async fn snapshot_manifest_matches_served_records_and_pages_with_the_cut() {
    let server = TestServer::start().await;
    let compacted = set_fingerprint(
        compacting_event(server.vault, server.owner_device, 1, Uuid::new_v4(), vec![]),
        &server.fingerprint,
    );
    let tagged = with_origin_tag(
        set_fingerprint(
            event(server.vault, server.owner_device, 2, Uuid::new_v4(), 2),
            &server.fingerprint,
        ),
        [7; 32],
    );
    server.commit(&compacted).await;
    server.commit(&tagged).await;
    server.commit(&server.owner_event(3)).await;

    let (status, start) = server.get(&server.owner_token, "/v1/snapshot").await;
    assert_eq!(status, StatusCode::OK, "{start}");
    assert_eq!(start["manifest_supported"], true);
    assert_eq!(start["max_manifest_page_size"], 5_000);

    let (status, records) = server
        .get(
            &server.owner_token,
            "/v1/snapshot/records?high_water=3&compaction_generation=0&after=0&limit=3",
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{records}");
    let (status, manifest) = server
        .get(
            &server.owner_token,
            "/v1/snapshot/manifest?high_water=3&compaction_generation=0&after=0&limit=3",
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{manifest}");
    assert_eq!(cursors(&manifest["entries"]), cursors(&records["records"]));
    for (entry, record) in manifest["entries"]
        .as_array()
        .expect("manifest entries")
        .iter()
        .zip(records["records"].as_array().expect("snapshot records"))
    {
        let envelope: Envelope =
            serde_json::from_value(record["envelope"].clone()).expect("served envelope");
        assert_eq!(
            entry["digest"],
            hex::encode(envelope.wire_digest().expect("digest"))
        );
        let flags = entry["flags"].as_u64().expect("flags");
        assert_eq!(flags & 1 != 0, envelope.compaction.is_some());
        assert_eq!(flags & 2 != 0, envelope.origin_tag.is_some());
    }
    assert_eq!(manifest["entries"][2]["flags"], 0);

    let (status, first) = server
        .get(
            &server.owner_token,
            "/v1/snapshot/manifest?high_water=3&after=0&limit=1",
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{first}");
    assert_eq!(cursors(&first["entries"]), vec![1]);
    assert_eq!(first["next_after"], "1");
    let (status, last) = server
        .get(
            &server.owner_token,
            "/v1/snapshot/manifest?high_water=3&after=2&limit=0",
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{last}");
    assert_eq!(cursors(&last["entries"]), vec![3]);
    assert_eq!(last["next_after"], Value::Null);
    let (status, default_page) = server
        .get(
            &server.owner_token,
            "/v1/snapshot/manifest?high_water=3&after=0",
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{default_page}");
    assert_eq!(cursors(&default_page["entries"]), vec![1, 2, 3]);

    let (status, invalid) = server
        .get(
            &server.owner_token,
            "/v1/snapshot/manifest?high_water=3&after=4",
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{invalid}");
    assert_eq!(invalid["code"], "invalid_cursor");
    let (records_status, records_resync) = server
        .get(&server.owner_token, "/v1/snapshot/records?high_water=4")
        .await;
    let (manifest_status, manifest_resync) = server
        .get(&server.owner_token, "/v1/snapshot/manifest?high_water=4")
        .await;
    assert_eq!(records_status, StatusCode::CONFLICT);
    assert_eq!(manifest_status, StatusCode::CONFLICT);
    assert_eq!(manifest_resync, records_resync);

    let other_vault = Uuid::new_v4();
    let (profile, fingerprint) = profile(other_vault, 1);
    let foreign = create_owner(&server.pool, profile, vec![1], fingerprint.clone(), 1)
        .await
        .expect("foreign owner");
    let foreign_event = set_fingerprint(
        event(other_vault, foreign.device_id, 1, Uuid::new_v4(), 99),
        &fingerprint,
    );
    let (status, body) = server
        .post(&foreign.device_token, "/v1/events", &foreign_event)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, foreign_manifest) = server
        .get(
            &foreign.device_token,
            "/v1/snapshot/manifest?high_water=1&after=0",
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{foreign_manifest}");
    assert_eq!(cursors(&foreign_manifest["entries"]), vec![1]);
    let foreign_envelope: Envelope =
        serde_json::from_value(foreign_event).expect("foreign envelope");
    assert_eq!(
        foreign_manifest["entries"][0]["digest"],
        hex::encode(foreign_envelope.wire_digest().expect("foreign digest"))
    );

    sqlx::query("UPDATE devices SET revoked_at=now() WHERE vault_id=$1 AND device_id=$2")
        .bind(server.vault)
        .bind(server.owner_device)
        .execute(&server.pool)
        .await
        .expect("revoke device");
    let (status, _) = server
        .get(
            &server.owner_token,
            "/v1/snapshot/manifest?high_water=3&after=0",
        )
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    server.shutdown().await;
}

#[tokio::test]
async fn snapshot_manifest_clamps_pages_at_five_thousand_entries() {
    let server = TestServer::start().await;
    sqlx::query(
        "INSERT INTO encrypted_records(vault_id,producer_device_id,envelope_id,cursor,producer_sequence,purpose,command_id,cipher_digest,envelope)
         SELECT $1,$2,md5(cursor::text)::uuid,cursor,cursor,'event',NULL,decode(repeat('00',32),'hex'),
                CASE WHEN cursor=1 THEN '{\"compaction\":null,\"origin_tag\":null}'::jsonb ELSE '{}'::jsonb END
         FROM generate_series(1,5001) AS cursor",
    )
    .bind(server.vault)
    .bind(server.owner_device)
    .execute(&server.pool)
    .await
    .expect("insert retained records");
    sqlx::query("UPDATE vaults SET next_cursor=5001 WHERE vault_id=$1")
        .bind(server.vault)
        .execute(&server.pool)
        .await
        .expect("advance high water");

    let (status, default_page) = server
        .get(
            &server.owner_token,
            "/v1/snapshot/manifest?high_water=5001&after=0",
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{default_page}");
    assert_eq!(
        default_page["entries"]
            .as_array()
            .expect("default entries")
            .len(),
        1_000
    );
    let (status, minimum_page) = server
        .get(
            &server.owner_token,
            "/v1/snapshot/manifest?high_water=5001&after=0&limit=0",
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{minimum_page}");
    assert_eq!(
        minimum_page["entries"]
            .as_array()
            .expect("minimum entries")
            .len(),
        1
    );
    assert_eq!(minimum_page["entries"][0]["flags"], 0);
    assert_eq!(default_page["entries"][1]["flags"], 0);
    let digest = minimum_page["entries"][0]["digest"]
        .as_str()
        .expect("manifest digest");
    assert_eq!(digest.len(), 64);
    assert!(
        digest
            .bytes()
            .all(|byte| byte.is_ascii_digit()
                || (byte.is_ascii_lowercase() && byte.is_ascii_hexdigit())),
        "digest must be lowercase hexadecimal: {digest}"
    );

    let (status, page) = server
        .get(
            &server.owner_token,
            "/v1/snapshot/manifest?high_water=5001&after=0&limit=65535",
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert_eq!(page["entries"].as_array().expect("entries").len(), 5_000);
    assert_eq!(page["next_after"], "5000");
    let (status, tail) = server
        .get(
            &server.owner_token,
            "/v1/snapshot/manifest?high_water=5001&after=5000&limit=65535",
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{tail}");
    assert_eq!(cursors(&tail["entries"]), vec![5001]);
    assert_eq!(tail["next_after"], Value::Null);
    let (status, empty) = server
        .get(
            &server.owner_token,
            "/v1/snapshot/manifest?high_water=5001&after=5001",
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{empty}");
    assert_eq!(empty["entries"], json!([]));
    assert_eq!(empty["next_after"], Value::Null);
    server.shutdown().await;
}

#[tokio::test]
async fn snapshot_manifest_observes_compaction_generation_and_deletion() {
    let server = TestServer::start().await;
    let first = set_fingerprint(
        compacting_event(server.vault, server.owner_device, 1, Uuid::new_v4(), vec![]),
        &server.fingerprint,
    );
    let second = set_fingerprint(
        compacting_event(
            server.vault,
            server.owner_device,
            2,
            Uuid::new_v4(),
            vec![(server.owner_device, 1)],
        ),
        &server.fingerprint,
    );
    server.commit(&first).await;
    server.commit(&second).await;
    let gateway = server.provision_device("gateway").await;
    for token in [&server.owner_token, &gateway.token] {
        let (status, body) = server
            .get(
                token,
                "/v1/snapshot/manifest?high_water=2&compaction_generation=0&after=0",
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }
    let gateway_fences: bool = sqlx::query_scalar(
        "SELECT compaction_generation_fence FROM devices WHERE vault_id=$1 AND device_id=$2",
    )
    .bind(server.vault)
    .bind(gateway.id)
    .fetch_one(&server.pool)
    .await
    .expect("read fence");
    assert!(gateway_fences);
    sqlx::query("UPDATE event_log SET created_at=now()-interval '31 days' WHERE vault_id=$1")
        .bind(server.vault)
        .execute(&server.pool)
        .await
        .expect("age events");
    assert_eq!(
        prune_replay_log(&server.pool, Duration::from_secs(30 * 86_400))
            .await
            .expect("prune"),
        2
    );
    assert_eq!(compact_records(&server.pool).await.expect("compact"), 1);

    let (status, manifest) = server
        .get(
            &server.owner_token,
            "/v1/snapshot/manifest?high_water=2&compaction_generation=1&after=0",
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{manifest}");
    assert_eq!(cursors(&manifest["entries"]), vec![2]);
    let (records_status, records_resync) = server
        .get(
            &server.owner_token,
            "/v1/snapshot/records?high_water=2&compaction_generation=0&after=0",
        )
        .await;
    let (manifest_status, manifest_resync) = server
        .get(
            &server.owner_token,
            "/v1/snapshot/manifest?high_water=2&compaction_generation=0&after=0",
        )
        .await;
    assert_eq!(records_status, StatusCode::CONFLICT);
    assert_eq!(manifest_status, StatusCode::CONFLICT);
    assert_eq!(manifest_resync, records_resync);
    server.shutdown().await;
}

// Verified resync end to end: the real client core drives the real snapshot routes, following
// the reference host loop of spec 4.3. The helpers below parse the real JSON; nothing is mocked.

use peppy_client_core::{
    Client as CoreClient, ClientConfig, Cursor, DatabaseKey, DeviceId, IngestResult,
    NotificationCapture, RawSnapshotRecord, SnapshotManifestEntry, SnapshotPurpose, VaultId,
};
use std::time::Instant;

const RESYNC_PASSPHRASE: &str = "correct horse battery staple";

/// A real vault (owner profile and header from `peppy-crypto`) plus a directory for client databases.
struct RealVault {
    server: TestServer,
    dir: tempfile::TempDir,
    profile: KeyProfile,
    header: peppy_crypto::VaultCheckHeader,
}

async fn real_vault() -> RealVault {
    let profile = KeyProfile::new(Uuid::new_v4(), 1).unwrap();
    let root = derive_root_key(RESYNC_PASSPHRASE, &profile).unwrap();
    let header = peppy_crypto::create_vault_check_header(&root, profile.clone()).unwrap();
    let server = TestServer::start_real(&profile, &header).await;
    RealVault {
        server,
        dir: tempfile::TempDir::new().unwrap(),
        profile,
        header,
    }
}

impl RealVault {
    /// A client database for `device`, unlocked with the vault passphrase.
    fn open(&self, name: &str, device: Uuid) -> CoreClient {
        let client = CoreClient::open(
            ClientConfig {
                database_path: self.dir.path().join(name),
                vault_id: VaultId(self.server.vault),
                device_id: DeviceId(device),
            },
            DatabaseKey::new(&[5; 32]).unwrap(),
        )
        .unwrap();
        client
            .unlock(&self.profile, &self.header, RESYNC_PASSPHRASE)
            .unwrap();
        client
    }
}

fn cursor_of(value: &Value) -> u64 {
    match value {
        Value::String(text) => text.parse().expect("decimal cursor"),
        other => other.as_u64().expect("numeric cursor"),
    }
}

fn capture(key: &str, text: &str) -> NotificationCapture {
    NotificationCapture {
        notification_key: key.into(),
        instance: "i".into(),
        package_name: "com.example".into(),
        app_name: "App".into(),
        app_icon_png_base64: None,
        title: "Download".into(),
        text: text.into(),
        category: None,
        posted_at: 1,
        dismissible: true,
    }
}

/// The app's upgrade handshake: declare the capability, then record the snapshot's fields.
async fn enable_compaction(server: &TestServer, client: &CoreClient) {
    let (status, _) = server
        .post(&server.owner_token, "/v1/compaction/capability", &json!({}))
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, start) = server.get(&server.owner_token, "/v1/snapshot").await;
    client
        .set_server_compaction_state(
            start["compaction_supported"].as_bool().unwrap(),
            start["compaction_active"].as_bool().unwrap(),
        )
        .unwrap();
}

/// Uploads everything the producer queued, as the app's outbox does, acking each row.
async fn publish(server: &TestServer, producer: &CoreClient) {
    for envelope in producer.pending_outbox().unwrap() {
        let (status, body) = server
            .post(
                &server.owner_token,
                "/v1/events",
                &serde_json::to_value(&envelope).unwrap(),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        producer.ack_outbox(envelope.envelope_id).unwrap();
    }
}

/// Pulls `/v1/events` from `after` to the end, as a device does: `(cursor, envelope JSON)`.
async fn fetch_events(server: &TestServer, after: u64) -> Vec<(u64, Vec<u8>)> {
    let mut events = Vec::new();
    let mut cursor = after;
    loop {
        let (status, page) = server
            .get(&server.owner_token, &format!("/v1/events?after={cursor}"))
            .await;
        assert_eq!(status, StatusCode::OK, "{page}");
        for record in page["events"].as_array().expect("events") {
            events.push((
                cursor_of(&record["cursor"]),
                serde_json::to_vec(&record["envelope"]).unwrap(),
            ));
        }
        match page["next_after"].as_str() {
            Some(next) => cursor = next.parse().unwrap(),
            None => return events,
        }
    }
}

/// Journals the fetched records whose cursor `keep` accepts, as the device's replay does.
fn journal(device: &CoreClient, events: &[(u64, Vec<u8>)], keep: impl Fn(u64) -> bool) {
    for (cursor, envelope_json) in events {
        if keep(*cursor) {
            let result = device.ingest_raw(envelope_json, Cursor(*cursor)).unwrap();
            assert!(
                matches!(result, IngestResult::Journaled),
                "cursor {cursor} was not journaled"
            );
        }
    }
}

/// Makes a device cursor `held` fall below the replay floor, as the prune tests do: every
/// transport row up to `high` is aged past retention and pruned.
async fn expire_cursor(server: &TestServer, held: i64, high: i64) {
    sqlx::query(
        "UPDATE event_log SET created_at=now()-interval '31 days' WHERE vault_id=$1 AND cursor<=$2",
    )
    .bind(server.vault)
    .bind(high)
    .execute(&server.pool)
    .await
    .unwrap();
    let retention = Duration::from_secs(30 * 86_400);
    assert!(prune_replay_log(&server.pool, retention).await.unwrap() > 0);
    let (status, body) = server
        .get(&server.owner_token, &format!("/v1/events?after={held}"))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["reason"], "cursor_expired");
}

fn hex_digest(text: &str) -> [u8; 32] {
    assert!(
        text.len() == 64
            && text
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
        "manifest digest must be 64 lowercase hex: {text}"
    );
    hex::decode(text)
        .unwrap()
        .try_into()
        .expect("32-byte digest")
}

/// Parses a manifest page exactly as the wire contract describes it (spec 3.2).
fn manifest_entries(page: &Value) -> Vec<SnapshotManifestEntry> {
    page["entries"]
        .as_array()
        .expect("manifest entries")
        .iter()
        .map(|entry| {
            let flags = entry["flags"].as_u64().expect("flags");
            assert!(flags <= 3, "unknown manifest flag bits: {flags}");
            SnapshotManifestEntry {
                cursor: Cursor(cursor_of(&entry["cursor"])),
                digest: hex_digest(entry["digest"].as_str().expect("digest")),
                has_compaction: flags & 1 != 0,
                has_origin_tag: flags & 2 != 0,
            }
        })
        .collect()
}

fn raw_records(records: &Value) -> Vec<RawSnapshotRecord> {
    records
        .as_array()
        .expect("snapshot records")
        .iter()
        .map(|record| RawSnapshotRecord {
            cursor: Cursor(cursor_of(&record["cursor"])),
            envelope_json: serde_json::to_vec(&record["envelope"]).unwrap(),
        })
        .collect()
}

/// The envelopes the records route serves for a cut: the oracle for what the manifest must flag.
async fn served_cut(server: &TestServer, high: u64, generation: u64) -> Vec<(u64, Value)> {
    let (status, page) = server
        .get(
            &server.owner_token,
            &format!("/v1/snapshot/records?high_water={high}&compaction_generation={generation}&after=0&limit=200"),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{page}");
    page["records"]
        .as_array()
        .expect("snapshot records")
        .iter()
        .map(|record| (cursor_of(&record["cursor"]), record["envelope"].clone()))
        .collect()
}

/// Full import of the cut described by `start`: the reference the manifest path must match.
async fn full_import(server: &TestServer, device: &CoreClient, start: &Value) {
    let high = cursor_of(&start["high_water_cursor"]);
    let generation = cursor_of(&start["compaction_generation"]);
    let count = cursor_of(&start["record_count"]);
    let mut progress = device
        .begin_snapshot_with_compaction(
            Cursor(high),
            count,
            SnapshotPurpose::Resync,
            Some(generation),
        )
        .unwrap();
    while progress.received_records < progress.expected_records {
        let (status, page) = server
            .get(
                &server.owner_token,
                &format!(
                    "/v1/snapshot/records?high_water={high}&compaction_generation={generation}&after={}&limit=200",
                    progress.last_cursor.0
                ),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{page}");
        let records = raw_records(&page["records"]);
        assert!(!records.is_empty(), "full import stalled: {page}");
        progress = device
            .append_snapshot_raw_page(progress.generation, &records)
            .unwrap();
    }
    device.finish_snapshot(progress.generation).unwrap();
    while device.apply_pending(100).unwrap().snapshot_remaining > 0 {}
}

#[derive(Default)]
struct ResyncTrace {
    /// `(after, missing)` of every manifest page the host read, in order.
    pages: Vec<(u64, u32)>,
    /// `(cursor, has_compaction, has_origin_tag)` of every manifest entry listed, in order.
    entries: Vec<(u64, bool, bool)>,
    /// Cursors downloaded through `/v1/snapshot/records`, in order.
    downloaded: Vec<u64>,
}

impl ResyncTrace {
    fn listed(&self) -> Vec<u64> {
        self.entries.iter().map(|entry| entry.0).collect()
    }
}

/// The reference host loop of spec 4.3 against the real routes, driven by the real core.
async fn resync_through_manifest(
    server: &TestServer,
    device: &CoreClient,
    limit: u16,
) -> ResyncTrace {
    let (status, start) = server.get(&server.owner_token, "/v1/snapshot").await;
    assert_eq!(status, StatusCode::OK, "{start}");
    assert_eq!(start["manifest_supported"], true);
    let high = cursor_of(&start["high_water_cursor"]);
    let generation = cursor_of(&start["compaction_generation"]);
    let count = cursor_of(&start["record_count"]);
    let mut trace = ResyncTrace::default();
    let mut progress = device
        .begin_snapshot_with_manifest(Cursor(high), count, Some(generation))
        .unwrap();
    while progress.received_records < progress.expected_records {
        let after = progress.last_cursor.0;
        let (status, page) = server
            .get(
                &server.owner_token,
                &format!(
                    "/v1/snapshot/manifest?high_water={high}&compaction_generation={generation}&after={after}&limit={limit}"
                ),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{page}");
        let entries = manifest_entries(&page);
        assert!(
            !entries.is_empty(),
            "server sent no manifest progress: {page}"
        );
        trace.entries.extend(
            entries
                .iter()
                .map(|entry| (entry.cursor.0, entry.has_compaction, entry.has_origin_tag)),
        );
        let manifest = device
            .append_snapshot_manifest(progress.generation, &entries)
            .unwrap();
        trace.pages.push((after, manifest.missing));
        progress = manifest.progress;
        let mut remaining = manifest.missing;
        while remaining > 0 {
            let after = progress.last_cursor.0;
            let page_size = remaining.min(200);
            let (status, records) = server
                .get(
                    &server.owner_token,
                    &format!(
                        "/v1/snapshot/records?high_water={high}&compaction_generation={generation}&after={after}&limit={page_size}"
                    ),
                )
                .await;
            assert_eq!(status, StatusCode::OK, "{records}");
            let raw = raw_records(&records["records"]);
            assert!(
                !raw.is_empty() && raw.len() <= remaining as usize,
                "download must supply exactly the missing run: {records}"
            );
            trace
                .downloaded
                .extend(raw.iter().map(|record| record.cursor.0));
            progress = device
                .append_snapshot_raw_page(progress.generation, &raw)
                .unwrap();
            remaining -= raw.len() as u32;
        }
    }
    device.finish_snapshot(progress.generation).unwrap();
    while device.apply_pending(100).unwrap().snapshot_remaining > 0 {}
    trace
}

/// What a user can observe on a device after sync: cursors, quarantine and notifications.
fn observed(device: &CoreClient) -> Value {
    let progress = device.replay_progress().unwrap();
    json!({
        "receive_cursor": device.receive_cursor().unwrap().0,
        "received_through": progress.received_through.0,
        "processed_through": progress.processed_through.0,
        "waiting_for_keys": progress.waiting_for_keys,
        "quarantined": device.quarantine_count().unwrap(),
        "notifications": serde_json::to_value(device.notification_snapshot().unwrap()).unwrap(),
    })
}

fn is_flagged(envelope: &Value) -> bool {
    !envelope["compaction"].is_null() || !envelope["origin_tag"].is_null()
}

#[tokio::test]
async fn verified_resync_downloads_only_the_records_the_journal_lacks() {
    let real = real_vault().await;
    let server = &real.server;
    let producer = real.open("producer.db", server.owner_device);
    let held_device = real.open("held.db", Uuid::new_v4());
    let fresh = real.open("fresh.db", Uuid::new_v4());

    for i in 0..6 {
        producer
            .capture_notification(capture(&format!("n{i}"), "held"))
            .unwrap();
    }
    publish(server, &producer).await;
    let held = fetch_events(server, 0).await;
    assert_eq!(
        held.iter().map(|event| event.0).collect::<Vec<_>>(),
        (1..=6).collect::<Vec<_>>()
    );
    journal(&held_device, &held, |_| true);
    held_device.apply_pending(100).unwrap();

    // Four more records the device never fetches.
    for i in 6..10 {
        producer
            .capture_notification(capture(&format!("n{i}"), "missing"))
            .unwrap();
    }
    publish(server, &producer).await;
    expire_cursor(server, 6, 10).await;

    let (status, start) = server.get(&server.owner_token, "/v1/snapshot").await;
    assert_eq!(status, StatusCode::OK, "{start}");
    assert_eq!(start["manifest_supported"], true);
    assert_eq!(start["max_manifest_page_size"], 5_000);
    assert_eq!(start["high_water_cursor"], "10");
    assert_eq!(start["record_count"], "10");

    let trace = resync_through_manifest(server, &held_device, 4).await;
    assert_eq!(trace.listed(), (1..=10).collect::<Vec<_>>());
    assert!(trace.entries.iter().all(|entry| !entry.1 && !entry.2));
    assert_eq!(trace.pages, vec![(0, 0), (4, 2), (8, 2)]);
    assert_eq!(
        trace.downloaded,
        vec![7, 8, 9, 10],
        "exactly the M records the journal lacks"
    );

    full_import(server, &fresh, &start).await;
    assert_eq!(observed(&held_device), observed(&fresh));
    assert_eq!(observed(&held_device)["receive_cursor"], 10);
    assert_eq!(
        observed(&held_device)["notifications"]["notifications"]
            .as_array()
            .unwrap()
            .len(),
        10
    );
    real.server.shutdown().await;
}

#[tokio::test]
async fn verified_resync_downloads_flagged_held_records_and_matches_a_full_import() {
    let real = real_vault().await;
    let server = &real.server;
    let producer = real.open("producer.db", server.owner_device);
    let held_device = real.open("held.db", Uuid::new_v4());
    let fresh = real.open("fresh.db", Uuid::new_v4());
    enable_compaction(server, &producer).await;

    // Held: a superseding chain, its compaction removal (flag bit 0), and an origin-tagged
    // record (flag bit 1). This core never produces origin tags, so the server fixture supplies
    // one; the envelope is journaled like any other record.
    for text in ["10%", "50%", "90%"] {
        producer
            .capture_notification(capture("progress", text))
            .unwrap();
    }
    publish(server, &producer).await;
    producer.remove_notification("progress", "i").unwrap();
    publish(server, &producer).await;
    // A second producer, so the fixture does not take the owner's next producer sequence.
    let gateway = server.provision_device("gateway").await;
    let tagged = with_origin_tag(
        set_fingerprint(
            event(server.vault, gateway.id, 1, Uuid::new_v4(), 5),
            &server.fingerprint,
        ),
        [7; 32],
    );
    let (status, accepted) = server.post(&gateway.token, "/v1/events", &tagged).await;
    assert_eq!(status, StatusCode::OK, "{accepted}");
    assert_eq!(accepted["cursor"], "5");

    let held = fetch_events(server, 0).await;
    assert_eq!(held.len(), 5);
    journal(&held_device, &held, |_| true);
    held_device.apply_pending(100).unwrap();

    for i in 0..2 {
        producer
            .capture_notification(capture(&format!("n{i}"), "missing"))
            .unwrap();
    }
    publish(server, &producer).await;
    expire_cursor(server, 5, 7).await;

    let (_, start) = server.get(&server.owner_token, "/v1/snapshot").await;
    let served = served_cut(server, 7, cursor_of(&start["compaction_generation"])).await;
    let flagged: Vec<u64> = served
        .iter()
        .filter(|(_, envelope)| is_flagged(envelope))
        .map(|(cursor, _)| *cursor)
        .collect();
    assert!(
        flagged.contains(&4),
        "removal must carry compaction: {flagged:?}"
    );
    assert!(
        flagged.contains(&5),
        "tagged record must carry origin_tag: {flagged:?}"
    );
    let held_cursors: Vec<u64> = held.iter().map(|event| event.0).collect();
    let expected_downloads: Vec<u64> = (1..=7)
        .filter(|cursor| !held_cursors.contains(cursor) || flagged.contains(cursor))
        .collect();

    let trace = resync_through_manifest(server, &held_device, 3).await;
    for (cursor, has_compaction, has_origin_tag) in &trace.entries {
        let envelope = &served.iter().find(|(c, _)| c == cursor).unwrap().1;
        assert_eq!(
            *has_compaction,
            !envelope["compaction"].is_null(),
            "cursor {cursor}"
        );
        assert_eq!(
            *has_origin_tag,
            !envelope["origin_tag"].is_null(),
            "cursor {cursor}"
        );
    }
    assert_eq!(trace.downloaded, expected_downloads);

    full_import(server, &fresh, &start).await;
    assert_eq!(observed(&held_device), observed(&fresh));
    assert_eq!(observed(&held_device)["receive_cursor"], 7);
    real.server.shutdown().await;
}

#[tokio::test]
async fn verified_resync_after_compaction_omits_compacted_cursors_and_fences_the_old_generation() {
    let real = real_vault().await;
    let server = &real.server;
    let producer = real.open("producer.db", server.owner_device);
    let held_device = real.open("held.db", Uuid::new_v4());
    let fresh = real.open("fresh.db", Uuid::new_v4());
    enable_compaction(server, &producer).await;

    for text in ["10%", "50%", "90%"] {
        producer
            .capture_notification(capture("progress", text))
            .unwrap();
    }
    publish(server, &producer).await;
    producer.remove_notification("progress", "i").unwrap();
    publish(server, &producer).await;
    let held = fetch_events(server, 0).await;
    assert_eq!(held.len(), 4);
    journal(&held_device, &held, |_| true);
    held_device.apply_pending(100).unwrap();

    for i in 0..2 {
        producer
            .capture_notification(capture(&format!("n{i}"), "later"))
            .unwrap();
    }
    publish(server, &producer).await;
    expire_cursor(server, 4, 6).await;

    // Cursors 1..=3 are superseded by the removal and sit below the floor: compaction deletes them.
    assert!(compact_records(&server.pool).await.unwrap() > 0);
    let remaining: Vec<i64> = sqlx::query_scalar(
        "SELECT cursor FROM encrypted_records WHERE vault_id=$1 ORDER BY cursor",
    )
    .bind(server.vault)
    .fetch_all(&server.pool)
    .await
    .unwrap();
    assert_eq!(remaining, vec![4, 5, 6]);

    let (_, start) = server.get(&server.owner_token, "/v1/snapshot").await;
    assert_eq!(start["compaction_generation"], "1");
    assert_eq!(start["record_count"], "3");
    // A request carrying the old generation is refused with the same resync body as records.
    let (records_status, records_resync) = server
        .get(
            &server.owner_token,
            "/v1/snapshot/records?high_water=6&compaction_generation=0&after=0",
        )
        .await;
    let (manifest_status, manifest_resync) = server
        .get(
            &server.owner_token,
            "/v1/snapshot/manifest?high_water=6&compaction_generation=0&after=0",
        )
        .await;
    assert_eq!(records_status, StatusCode::CONFLICT);
    assert_eq!(manifest_status, StatusCode::CONFLICT);
    assert_eq!(manifest_resync, records_resync);
    assert_eq!(manifest_resync["reason"], "compaction_generation_changed");

    let trace = resync_through_manifest(server, &held_device, 2).await;
    assert_eq!(
        trace.listed(),
        vec![4, 5, 6],
        "compacted cursors 1..=3 must be absent"
    );
    assert_eq!(trace.pages, vec![(0, 2), (5, 1)]);
    assert_eq!(trace.downloaded, vec![4, 5, 6]);

    full_import(server, &fresh, &start).await;
    assert_eq!(observed(&held_device), observed(&fresh));
    assert_eq!(observed(&held_device)["receive_cursor"], 6);
    real.server.shutdown().await;
}

#[tokio::test]
async fn verified_resync_pages_missing_runs_across_manifest_boundaries() {
    let real = real_vault().await;
    let server = &real.server;
    let producer = real.open("producer.db", server.owner_device);
    let held_device = real.open("held.db", Uuid::new_v4());
    let fresh = real.open("fresh.db", Uuid::new_v4());

    for i in 0..12 {
        producer
            .capture_notification(capture(&format!("n{i:02}"), "x"))
            .unwrap();
    }
    publish(server, &producer).await;
    let events = fetch_events(server, 0).await;
    assert_eq!(events.len(), 12);
    // Held cursors leave gaps: missing 2..=4, 7 and 12.
    let held = [1, 5, 6, 8, 9, 10, 11];
    journal(&held_device, &events, |cursor| held.contains(&cursor));
    expire_cursor(server, 1, 12).await;

    // With limit 3, the page after 0 is [1 held, 2 and 3 missing]: its missing run 2..=3 ends at
    // the page boundary. The page after 3 is [4 missing, 5 and 6 held]: the run 2..=4 spans two
    // manifest pages. Held 5 and 6 are ignored until the host asks again from cursor 4.
    let trace = resync_through_manifest(server, &held_device, 3).await;
    assert_eq!(trace.pages, vec![(0, 2), (3, 1), (4, 1), (7, 0), (10, 1)]);
    assert_eq!(
        trace.listed(),
        vec![1, 2, 3, 4, 5, 6, 5, 6, 7, 8, 9, 10, 11, 12]
    );
    assert_eq!(trace.downloaded, vec![2, 3, 4, 7, 12]);

    let (_, start) = server.get(&server.owner_token, "/v1/snapshot").await;
    full_import(server, &fresh, &start).await;
    assert_eq!(observed(&held_device), observed(&fresh));
    assert_eq!(observed(&held_device)["receive_cursor"], 12);
    real.server.shutdown().await;
}

/// Timing note, not an assertion: the server cost of one 5,000-entry manifest page against the
/// same range paged as records. Run with `-- --nocapture` to see the numbers.
#[tokio::test]
async fn manifest_cost_at_five_thousand_records() {
    let server = TestServer::start().await;
    let seeding = Instant::now();
    // About 1.4 KB per served envelope: an 800-byte ciphertext, base64-encoded.
    sqlx::query(
        "INSERT INTO encrypted_records(vault_id,producer_device_id,envelope_id,cursor,producer_sequence,purpose,command_id,cipher_digest,envelope)
         SELECT $1,$2,md5(cursor::text)::uuid,cursor,cursor,'event',NULL,decode(md5(cursor::text)||md5(cursor::text||'x'),'hex'),
                jsonb_build_object('protocol_version',1,'envelope_id',md5(cursor::text)::uuid,'command_id',NULL::text,
                    'vault_id',$1::text,'producer_device_id',$2::text,'producer_sequence',cursor::text,'key_epoch',1,
                    'crypto_suite',1,'profile_fingerprint',$3::text,'purpose','event','route',NULL::text,
                    'ciphertext',repeat('QUJD',267))
         FROM generate_series(1,5000) AS cursor",
    )
    .bind(server.vault)
    .bind(server.owner_device)
    .bind(&server.fingerprint)
    .execute(&server.pool)
    .await
    .expect("seed 5000 records");
    sqlx::query("UPDATE vaults SET next_cursor=5000 WHERE vault_id=$1")
        .bind(server.vault)
        .execute(&server.pool)
        .await
        .unwrap();
    let seeded = seeding.elapsed();

    let (_, start) = server.get(&server.owner_token, "/v1/snapshot").await;
    assert_eq!(start["record_count"], "5000");
    let timed = Instant::now();
    let (status, manifest) = server
        .get(
            &server.owner_token,
            "/v1/snapshot/manifest?high_water=5000&compaction_generation=0&after=0&limit=5000",
        )
        .await;
    let manifest_time = timed.elapsed();
    assert_eq!(status, StatusCode::OK, "{manifest}");
    assert_eq!(manifest["entries"].as_array().unwrap().len(), 5_000);
    let manifest_bytes = serde_json::to_vec(&manifest).unwrap().len();

    let timed = Instant::now();
    let (mut after, mut requests, mut served, mut envelope_bytes) = (0u64, 0u32, 0usize, 0usize);
    loop {
        let (status, page) = server
            .get(
                &server.owner_token,
                &format!("/v1/snapshot/records?high_water=5000&compaction_generation=0&after={after}&limit=200"),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{page}");
        requests += 1;
        let records = page["records"].as_array().unwrap();
        if envelope_bytes == 0 {
            envelope_bytes = serde_json::to_vec(&records[0]["envelope"]).unwrap().len();
        }
        served += records.len();
        match page["next_after"].as_str() {
            Some(next) => after = next.parse().unwrap(),
            None => break,
        }
    }
    let records_time = timed.elapsed();
    assert_eq!(served, 5_000);

    eprintln!(
        "verified-resync timing: served envelope {envelope_bytes} B; seeding 5000 rows {seeded:?}; \
         manifest 5000 entries in one request {manifest_time:?} ({manifest_bytes} B body); \
         records paging of the same 5000 in {requests} requests of 200 {records_time:?}"
    );
    server.shutdown().await;
}

#[tokio::test]
async fn compaction_only_prunes_replay_expired_explicit_targets_and_fences_snapshot_pages() {
    let server = TestServer::start().await;
    let first = set_fingerprint(
        compacting_event(server.vault, server.owner_device, 1, Uuid::new_v4(), vec![]),
        &server.fingerprint,
    );
    let second = set_fingerprint(
        compacting_event(
            server.vault,
            server.owner_device,
            2,
            Uuid::new_v4(),
            vec![(server.owner_device, 1)],
        ),
        &server.fingerprint,
    );
    server.commit(&first).await;
    server.commit(&second).await;
    // A producer cannot reference its own current or future sequence.
    let forward = set_fingerprint(
        compacting_event(
            server.vault,
            server.owner_device,
            3,
            Uuid::new_v4(),
            vec![(server.owner_device, 4)],
        ),
        &server.fingerprint,
    );
    let (status, body) = server
        .post(&server.owner_token, "/v1/events", &forward)
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    // A cross-producer cycle has no retained representative and must remain intact.
    let gateway = server.provision_device("gateway").await;
    let third = set_fingerprint(
        compacting_event(
            server.vault,
            server.owner_device,
            3,
            Uuid::new_v4(),
            vec![(gateway.id, 1)],
        ),
        &server.fingerprint,
    );
    let fourth = set_fingerprint(
        compacting_event(
            server.vault,
            gateway.id,
            1,
            Uuid::new_v4(),
            vec![(server.owner_device, 3)],
        ),
        &server.fingerprint,
    );
    server.commit(&third).await;
    let (status, body) = server.post(&gateway.token, "/v1/events", &fourth).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, _) = server
        .post(&gateway.token, "/v1/compaction/capability", &json!({}))
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    sqlx::query("UPDATE event_log SET created_at=now()-interval '31 days' WHERE vault_id=$1")
        .bind(server.vault)
        .execute(&server.pool)
        .await
        .unwrap();
    assert_eq!(
        prune_replay_log(&server.pool, Duration::from_secs(30 * 86_400))
            .await
            .unwrap(),
        4
    );
    // No device has declared generation fencing yet: nothing may be deleted.
    assert_eq!(compact_records(&server.pool).await.unwrap(), 0);
    let (status, _) = server
        .post(&server.owner_token, "/v1/compaction/capability", &json!({}))
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(compact_records(&server.pool).await.unwrap(), 1);
    let remaining: Vec<i64> = sqlx::query_scalar(
        "SELECT cursor FROM encrypted_records WHERE vault_id=$1 ORDER BY cursor",
    )
    .bind(server.vault)
    .fetch_all(&server.pool)
    .await
    .unwrap();
    assert_eq!(remaining, vec![2, 3, 4]);
    let (_, start) = server.get(&server.owner_token, "/v1/snapshot").await;
    assert_eq!(start["compaction_generation"], "1");
    let (status, restart) = server
        .get(
            &server.owner_token,
            "/v1/snapshot/records?high_water=4&after=0",
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(restart["code"], "resync_required");
    assert_eq!(restart["reason"], "compaction_generation_changed");
    let (status, page) = server
        .get(
            &server.owner_token,
            "/v1/snapshot/records?high_water=4&compaction_generation=1&after=0",
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert_eq!(cursors(&page["records"]), vec![2, 3, 4]);
    server.shutdown().await;
}

fn marked_event(
    vault: Uuid,
    producer: Uuid,
    sequence: u64,
    payload: u8,
    terminal: bool,
    supersedes: Vec<(Uuid, u64)>,
    fingerprint: &str,
) -> Value {
    let mut value = compacting_event(vault, producer, sequence, Uuid::new_v4(), supersedes);
    value["ciphertext"] = json!(base64::engine::general_purpose::STANDARD.encode([payload]));
    value["compaction"]["terminal"] = json!(terminal);
    set_fingerprint(value, fingerprint)
}

async fn age_and_prune(server: &TestServer) {
    sqlx::query("UPDATE event_log SET created_at=now()-interval '31 days' WHERE vault_id=$1")
        .bind(server.vault)
        .execute(&server.pool)
        .await
        .unwrap();
    prune_replay_log(&server.pool, Duration::from_secs(30 * 86_400))
        .await
        .unwrap();
    // Each vault is processed at most daily; tests rewind that clock explicitly.
    sqlx::query("UPDATE vaults SET last_compacted_at=NULL WHERE vault_id=$1")
        .bind(server.vault)
        .execute(&server.pool)
        .await
        .unwrap();
}

async fn retained(server: &TestServer) -> Vec<i64> {
    sqlx::query_scalar("SELECT cursor FROM encrypted_records WHERE vault_id=$1 ORDER BY cursor")
        .bind(server.vault)
        .fetch_all(&server.pool)
        .await
        .unwrap()
}

fn keyed(mut value: Value, group: u8) -> Value {
    value["compaction"]["key"] =
        json!(base64::engine::general_purpose::STANDARD.encode([group; 32]));
    value
}

#[tokio::test]
async fn expired_terminal_root_is_kept_while_a_same_key_record_survives_outside_its_ancestry() {
    let server = TestServer::start().await;
    let (o, v, f) = (
        server.owner_device,
        server.vault,
        server.fingerprint.clone(),
    );
    // c1 is an older same-group state that the terminal never referenced (a strand).
    server
        .commit(&keyed(marked_event(v, o, 1, 1, false, vec![], &f), 9))
        .await;
    server
        .commit(&keyed(marked_event(v, o, 2, 2, false, vec![], &f), 9))
        .await;
    server
        .commit(&keyed(marked_event(v, o, 3, 3, true, vec![(o, 2)], &f), 9))
        .await;
    let (status, _) = server
        .post(&server.owner_token, "/v1/compaction/capability", &json!({}))
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    age_and_prune(&server).await;
    sqlx::query(
        "UPDATE record_compaction SET created_at=now()-interval '91 days' WHERE vault_id=$1",
    )
    .bind(v)
    .execute(&server.pool)
    .await
    .unwrap();
    // c2 goes (superseded by c3); the terminal root stays because c1 survives.
    assert_eq!(compact_records(&server.pool).await.unwrap(), 1);
    assert_eq!(retained(&server).await, vec![1, 3]);
    server.shutdown().await;
}

#[tokio::test]
async fn expired_terminal_roots_self_expire_with_their_ancestry_and_keep_retry_identity() {
    let server = TestServer::start().await;
    let (o, v, f) = (
        server.owner_device,
        server.vault,
        server.fingerprint.clone(),
    );
    // Lifetime A: post (c1) -> removal (c2, terminal). Lifetime B: post (c3) -> removal (c4).
    server
        .commit(&keyed(marked_event(v, o, 1, 1, false, vec![], &f), 1))
        .await;
    server
        .commit(&keyed(marked_event(v, o, 2, 2, true, vec![(o, 1)], &f), 1))
        .await;
    server
        .commit(&keyed(marked_event(v, o, 3, 3, false, vec![], &f), 2))
        .await;
    server
        .commit(&keyed(marked_event(v, o, 4, 4, true, vec![(o, 3)], &f), 2))
        .await;
    // A standalone terminal (e.g. a dismissal) with no ancestry.
    server
        .commit(&keyed(marked_event(v, o, 5, 5, true, vec![], &f), 3))
        .await;
    // A non-terminal latest state never expires, however old.
    server
        .commit(&keyed(marked_event(v, o, 6, 6, false, vec![], &f), 4))
        .await;
    let (status, _) = server
        .post(&server.owner_token, "/v1/compaction/capability", &json!({}))
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    age_and_prune(&server).await;

    // Young terminals stay, but the posts they supersede go.
    assert_eq!(compact_records(&server.pool).await.unwrap(), 2);
    assert_eq!(retained(&server).await, vec![2, 4, 5, 6]);

    // 90 days later for lifetime A and the standalone terminal only.
    sqlx::query("UPDATE record_compaction SET created_at=now()-interval '91 days' WHERE vault_id=$1 AND cursor IN (2,5,6)")
        .bind(v).execute(&server.pool).await.unwrap();
    sqlx::query("UPDATE vaults SET last_compacted_at=NULL WHERE vault_id=$1")
        .bind(v)
        .execute(&server.pool)
        .await
        .unwrap();
    assert_eq!(compact_records(&server.pool).await.unwrap(), 2);
    assert_eq!(
        retained(&server).await,
        vec![4, 6],
        "younger root and non-terminal state retained"
    );
    let (_, start) = server.get(&server.owner_token, "/v1/snapshot").await;
    assert_eq!(start["compaction_generation"], "2");

    // A retry of the purged terminal stays a duplicate of its original cursor.
    let ledger: Uuid = sqlx::query_scalar("SELECT envelope_id FROM compacted_records WHERE vault_id=$1 AND producer_device_id=$2 AND producer_sequence=2")
        .bind(v).bind(o).fetch_one(&server.pool).await.unwrap();
    let mut retry = keyed(marked_event(v, o, 2, 2, true, vec![(o, 1)], &f), 1);
    retry["envelope_id"] = json!(ledger);
    let (status, accepted) = server.post(&server.owner_token, "/v1/events", &retry).await;
    assert_eq!(status, StatusCode::OK, "{accepted}");
    assert_eq!(accepted["duplicate"], true);
    assert_eq!(accepted["cursor"], "2");

    // Lifetime B expires later; only the latest non-terminal state remains.
    sqlx::query("UPDATE record_compaction SET created_at=now()-interval '91 days' WHERE vault_id=$1 AND cursor=4")
        .bind(v).execute(&server.pool).await.unwrap();
    sqlx::query("UPDATE vaults SET last_compacted_at=NULL WHERE vault_id=$1")
        .bind(v)
        .execute(&server.pool)
        .await
        .unwrap();
    assert_eq!(compact_records(&server.pool).await.unwrap(), 1);
    assert_eq!(retained(&server).await, vec![6]);
    let remaining_meta: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM record_supersessions WHERE vault_id=$1")
            .bind(v)
            .fetch_one(&server.pool)
            .await
            .unwrap();
    assert_eq!(remaining_meta, 0);
    server.shutdown().await;
}

#[tokio::test]
async fn unsaturated_compaction_pass_waits_the_full_daily_interval() {
    let server = TestServer::start().await;
    let (o, v, f) = (
        server.owner_device,
        server.vault,
        server.fingerprint.clone(),
    );
    server
        .commit(&marked_event(v, o, 1, 1, false, vec![], &f))
        .await;
    server
        .commit(&marked_event(v, o, 2, 2, false, vec![(o, 1)], &f))
        .await;
    let (status, _) = server
        .post(&server.owner_token, "/v1/compaction/capability", &json!({}))
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    age_and_prune(&server).await;
    assert_eq!(compact_records(&server.pool).await.unwrap(), 1);
    // An unsaturated pass waits the full interval.
    let next_due_hours: f64 = sqlx::query_scalar("SELECT (EXTRACT(EPOCH FROM (last_compacted_at + interval '24 hours' - now()))/3600)::float8 FROM vaults WHERE vault_id=$1")
        .bind(v).fetch_one(&server.pool).await.unwrap();
    assert!(next_due_hours > 23.0, "{next_due_hours}");
    server.shutdown().await;
}

#[tokio::test]
async fn compaction_collapses_chains_protects_targets_holds_pending_and_keeps_retry_identity() {
    let server = TestServer::start().await;
    let gateway = server.provision_device("gateway").await;
    let (o, v, f) = (
        server.owner_device,
        server.vault,
        server.fingerprint.clone(),
    );
    // 1 <- 2 <- 3(terminal): the whole superseded chain goes, not just its middle.
    server
        .commit(&marked_event(v, o, 1, 1, false, vec![], &f))
        .await; // c1
    server
        .commit(&marked_event(v, o, 2, 2, false, vec![(o, 1)], &f))
        .await; // c2
    server
        .commit(&marked_event(v, o, 3, 3, true, vec![(o, 2)], &f))
        .await; // c3
    // A gateway legacy row (no metadata) and a carrier command are not removable by the owner.
    let (status, _) = server
        .post(
            &gateway.token,
            "/v1/events",
            &set_fingerprint(event(v, gateway.id, 1, Uuid::new_v4(), 40), &f),
        )
        .await; // c4
    assert_eq!(status, StatusCode::OK);
    let (status, body) = server
        .post(
            &server.owner_token,
            "/v1/commands",
            &set_fingerprint(
                command(v, o, 4, Uuid::new_v4(), Uuid::new_v4(), gateway.id),
                &f,
            ),
        )
        .await; // c5
    assert_eq!(status, StatusCode::OK, "{body}");
    server
        .commit(&marked_event(
            v,
            o,
            5,
            5,
            false,
            vec![(gateway.id, 1), (o, 4)],
            &f,
        ))
        .await; // c6
    let supersessions: Vec<(Uuid, i64)> = sqlx::query_as(
        "SELECT target_producer_device_id,target_producer_sequence FROM record_supersessions WHERE vault_id=$1 AND by_cursor=6 ORDER BY target_producer_device_id,target_producer_sequence",
    )
    .bind(v)
    .fetch_all(&server.pool)
    .await
    .unwrap();
    let mut expected_supersessions = vec![(gateway.id, 1), (o, 4)];
    expected_supersessions.sort_unstable();
    assert_eq!(supersessions, expected_supersessions);
    // Young terminal 7 superseded by 8: kept for the terminal window.
    server
        .commit(&marked_event(v, o, 7, 7, true, vec![], &f))
        .await; // c7
    server
        .commit(&marked_event(v, o, 8, 8, false, vec![(o, 7)], &f))
        .await; // c8
    // 9 <- 10 <- 11, but 10 also names not-yet-uploaded (gateway,50): 10 is held, 9 still goes.
    server
        .commit(&marked_event(v, o, 9, 9, false, vec![], &f))
        .await; // c9
    server
        .commit(&marked_event(
            v,
            o,
            10,
            10,
            false,
            vec![(o, 9), (gateway.id, 50)],
            &f,
        ))
        .await; // c10
    server
        .commit(&marked_event(v, o, 11, 11, false, vec![(o, 10)], &f))
        .await; // c11
    age_and_prune(&server).await;

    // The gateway has not declared generation fencing yet.
    assert_eq!(compact_records(&server.pool).await.unwrap(), 0);
    let (_, start) = server.get(&server.owner_token, "/v1/snapshot").await;
    assert_eq!(start["compaction_supported"], true);
    assert_eq!(start["compaction_active"], false);
    let (status, _) = server
        .post(&server.owner_token, "/v1/compaction/capability", &json!({}))
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    // A fenced snapshot page is itself the gateway's declaration.
    let (status, _) = server
        .get(
            &gateway.token,
            "/v1/snapshot/records?high_water=11&compaction_generation=0&after=0",
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let (_, start) = server.get(&server.owner_token, "/v1/snapshot").await;
    assert_eq!(start["compaction_active"], true);

    assert_eq!(compact_records(&server.pool).await.unwrap(), 3);
    assert_eq!(retained(&server).await, vec![3, 4, 5, 6, 7, 8, 10, 11]);
    // Daily spacing: an immediate second pass does nothing even when eligible later.
    sqlx::query("UPDATE record_compaction SET created_at=now()-interval '91 days' WHERE vault_id=$1 AND cursor=7")
        .bind(v).execute(&server.pool).await.unwrap();
    assert_eq!(compact_records(&server.pool).await.unwrap(), 0);
    sqlx::query("UPDATE vaults SET last_compacted_at=NULL WHERE vault_id=$1")
        .bind(v)
        .execute(&server.pool)
        .await
        .unwrap();
    assert_eq!(
        compact_records(&server.pool).await.unwrap(),
        1,
        "expired terminal 7 is superseded by 8"
    );
    assert_eq!(retained(&server).await, vec![3, 4, 5, 6, 8, 10, 11]);
    let (_, start) = server.get(&server.owner_token, "/v1/snapshot").await;
    assert_eq!(start["compaction_generation"], "2");

    // Retry identity survives physical deletion.
    let original = marked_event(v, o, 1, 1, false, vec![], &f);
    let mut retry = original.clone();
    let ledger: Uuid = sqlx::query_scalar("SELECT envelope_id FROM compacted_records WHERE vault_id=$1 AND producer_sequence=1 AND producer_device_id=$2")
        .bind(v).bind(o).fetch_one(&server.pool).await.unwrap();
    retry["envelope_id"] = json!(ledger);
    let (status, accepted) = server.post(&server.owner_token, "/v1/events", &retry).await;
    assert_eq!(status, StatusCode::OK, "{accepted}");
    assert_eq!(accepted["duplicate"], true);
    assert_eq!(accepted["cursor"], "1");
    let reused = marked_event(v, o, 2, 99, false, vec![], &f);
    let (status, conflict) = server
        .post(&server.owner_token, "/v1/events", &reused)
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{conflict}");
    assert_eq!(
        retained(&server).await,
        vec![3, 4, 5, 6, 8, 10, 11],
        "no record was re-added"
    );

    // A newly paired device pauses physical compaction until it declares.
    let late = server.provision_device("device").await;
    let (_, start) = server.get(&late.token, "/v1/snapshot").await;
    assert_eq!(start["compaction_active"], false);
    server.shutdown().await;
}

#[tokio::test]
async fn replay_expiry_and_ahead_cursors_resync_while_immutable_records_survive_pruning() {
    let server = TestServer::start().await;
    let mut bodies = Vec::new();
    for sequence in 1..=10 {
        let body = server.owner_event(sequence);
        server.commit(&body).await;
        bodies.push(body);
    }
    sqlx::query(
        "UPDATE event_log SET created_at=now()-interval '31 days' WHERE vault_id=$1 AND cursor<=6",
    )
    .bind(server.vault)
    .execute(&server.pool)
    .await
    .unwrap();
    let retention = Duration::from_secs(30 * 86_400);
    assert_eq!(prune_replay_log(&server.pool, retention).await.unwrap(), 6);
    assert_eq!(prune_replay_log(&server.pool, retention).await.unwrap(), 0);

    for expired in ["0", "5"] {
        let (status, body) = server
            .get(&server.owner_token, &format!("/v1/events?after={expired}"))
            .await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body["code"], "resync_required");
        assert_eq!(body["reason"], "cursor_expired");
        assert_eq!(body["replay_floor_cursor"], "6");
        assert_eq!(body["high_water_cursor"], "10");
    }
    let (status, retained) = server.get(&server.owner_token, "/v1/events?after=6").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(cursors(&retained["events"]), vec![7, 8, 9, 10]);
    assert_eq!(retained["replay_floor_cursor"], "6");
    let (status, ahead) = server.get(&server.owner_token, "/v1/events?after=11").await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(ahead["reason"], "cursor_ahead");

    let (_, start) = server.get(&server.owner_token, "/v1/snapshot").await;
    assert_eq!(start["record_count"], "10");
    assert_eq!(start["replay_floor_cursor"], "6");
    let (_, page) = server
        .get(
            &server.owner_token,
            "/v1/snapshot/records?high_water=10&limit=200",
        )
        .await;
    assert_eq!(cursors(&page["records"]), (1..=10).collect::<Vec<_>>());

    // Identity outlives the pruned transport rows.
    let (status, retry) = server
        .post(&server.owner_token, "/v1/events", &bodies[0])
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(retry, json!({"cursor":"1","duplicate":true}));
    let (status, conflict) = server
        .post(&server.owner_token, "/v1/events", &server.owner_event(2))
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(conflict["code"], "idempotency_conflict");

    assert!(
        sqlx::query("DELETE FROM encrypted_records WHERE vault_id=$1")
            .bind(server.vault)
            .execute(&server.pool)
            .await
            .is_err()
    );
    assert!(
        sqlx::query("UPDATE encrypted_records SET envelope='{}'::jsonb WHERE vault_id=$1")
            .bind(server.vault)
            .execute(&server.pool)
            .await
            .is_err()
    );

    for (resume, reason) in [("0", "cursor_expired"), ("99", "cursor_ahead")] {
        let mut ws = server.connect_ws(&server.owner_token).await;
        send_hello(&mut ws, resume).await;
        let frame = next_json(&mut ws, 3).await;
        assert_eq!(frame["type"], "resync_required");
        assert_eq!(frame["reason"], reason);
        assert_eq!(close_code(&mut ws, 3).await, 4409);
    }
    server.shutdown().await;
}

#[tokio::test]
async fn websocket_negotiation_has_distinct_close_codes() {
    let server = TestServer::start_with(TransportOptions {
        hello_timeout: Duration::from_secs(1),
        ..TransportOptions::default()
    })
    .await;
    let token = server.owner_token.clone();

    let mut silent = server.connect_ws(&token).await;
    assert_eq!(close_code(&mut silent, 4).await, 4408);

    let mut old = server.connect_ws(&token).await;
    old.send(tungstenite::Message::Text(
        json!({"protocol_version":2,"resume_cursor":"0"})
            .to_string()
            .into(),
    ))
    .await
    .unwrap();
    assert_eq!(close_code(&mut old, 3).await, 4426);

    for invalid in [
        "not json",
        r#"{"protocol_version":1,"resume_cursor":"-1"}"#,
        r#"{"protocol_version":1}"#,
    ] {
        let mut ws = server.connect_ws(&token).await;
        ws.send(tungstenite::Message::Text(invalid.into()))
            .await
            .unwrap();
        assert_eq!(close_code(&mut ws, 3).await, 4400, "{invalid}");
    }

    let mut chatty = server.connect_ws(&token).await;
    send_hello(&mut chatty, "0").await;
    assert_eq!(next_json(&mut chatty, 3).await["type"], "ready");
    chatty
        .send(tungstenite::Message::Text("unexpected".into()))
        .await
        .unwrap();
    assert_eq!(close_code(&mut chatty, 3).await, 4400);
    server.shutdown().await;
}

#[tokio::test]
async fn outbox_drainer_recovers_a_lost_hint_and_retires_references() {
    // The durable rescan is pushed beyond the test window, so only the
    // drainer's hint can deliver the injected commit in time.
    let server = TestServer::start_with(TransportOptions {
        durable_replay_interval: Duration::from_secs(120),
        outbox_drain_interval: Duration::from_millis(100),
        outbox_delivered_retention: Duration::from_secs(1),
        ..TransportOptions::default()
    })
    .await;
    let device = server.provision_device("device").await;
    let mut ws = server.connect_ws(&device.token).await;
    send_hello(&mut ws, "0").await;
    assert_eq!(next_json(&mut ws, 3).await["type"], "ready");

    assert_eq!(server.commit(&server.owner_event(1)).await, "1");
    assert_eq!(next_durable_event(&mut ws, 3).await["cursor"], "1");
    let payload_is_null: bool = sqlx::query_scalar(
        "SELECT payload IS NULL FROM outbox_jobs WHERE vault_id=$1 AND cursor=1",
    )
    .bind(server.vault)
    .fetch_one(&server.pool)
    .await
    .unwrap();
    assert!(
        payload_is_null,
        "outbox rows are references, not envelope copies"
    );
    let outbox_state = |cursor: i64| {
        let pool = server.pool.clone();
        let vault = server.vault;
        async move {
            sqlx::query_as::<_, (bool, i32)>("SELECT delivered_at IS NOT NULL, attempts FROM outbox_jobs WHERE vault_id=$1 AND cursor=$2")
                .bind(vault)
                .bind(cursor)
                .fetch_optional(&pool)
                .await
                .unwrap()
        }
    };
    wait_until(5, "delivered outbox reference to be retired", || async {
        outbox_state(1).await.is_none()
    })
    .await;

    // Simulate a commit whose request-path hint was lost (crash or cancellation).
    sqlx::query("INSERT INTO event_log(vault_id,cursor,envelope_id,producer_device_id,producer_sequence,purpose,cipher_digest,envelope) VALUES($1,2,$2,$3,2,'event',$4,$5)")
        .bind(server.vault)
        .bind(Uuid::new_v4())
        .bind(server.owner_device)
        .bind(vec![9_u8; 32])
        .bind(json!({"protocol_version":1}))
        .execute(&server.pool)
        .await
        .unwrap();
    sqlx::query("UPDATE vaults SET next_cursor=2 WHERE vault_id=$1")
        .bind(server.vault)
        .execute(&server.pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO outbox_jobs(vault_id,cursor,kind) VALUES($1,2,'sync')")
        .bind(server.vault)
        .execute(&server.pool)
        .await
        .unwrap();
    assert_eq!(next_durable_event(&mut ws, 3).await["cursor"], "2");
    wait_until(3, "first delivery to be recorded", || async {
        outbox_state(2).await.is_none_or(|(delivered, _)| delivered)
    })
    .await;
    wait_until(5, "second reference to be retired", || async {
        outbox_state(2).await.is_none()
    })
    .await;

    // A redelivered reference is idempotent: the socket does not repeat cursor 2.
    sqlx::query("INSERT INTO outbox_jobs(vault_id,cursor,kind) VALUES($1,2,'sync')")
        .bind(server.vault)
        .execute(&server.pool)
        .await
        .unwrap();
    wait_until(3, "redelivery to be recorded", || async {
        outbox_state(2)
            .await
            .is_none_or(|(delivered, attempts)| delivered && attempts == 1)
    })
    .await;
    assert_no_durable_event(&mut ws, Duration::from_millis(1_500)).await;
    server.shutdown().await;
}

fn with_epoch(mut envelope: Value, epoch: u32, fingerprint: &str) -> Value {
    envelope["key_epoch"] = json!(epoch);
    envelope["profile_fingerprint"] = json!(fingerprint);
    envelope
}

#[tokio::test]
async fn key_profiles_are_owner_only_immutable_forward_and_gate_epochs() {
    let server = TestServer::start().await;
    sqlx::query("INSERT INTO vault_admission_keys(vault_id,key_epoch,profile_fingerprint,admission_public_key) VALUES($1,1,$2,$3)")
        .bind(server.vault)
        .bind(&server.fingerprint)
        .bind(URL_SAFE_NO_PAD.decode(admission_public_key(server.vault, 1)).unwrap())
        .execute(&server.pool)
        .await
        .unwrap();
    let owner = server.owner_token.clone();
    let gateway = server.provision_device("gateway").await;
    let device = server.provision_device("device").await;
    let (profile_two, fingerprint_two) = profile(server.vault, 2);
    let header_two = base64::engine::general_purpose::STANDARD.encode([2_u8; 80]);
    let admission_key_two = admission_public_key(server.vault, 2);
    let register = json!({
        "key_epoch": 2,
        "public_key_profile": profile_two,
        "encrypted_vault_check_header": header_two,
        "profile_fingerprint": fingerprint_two,
        "admission_public_key": admission_key_two,
    });

    let (status, _) = server
        .post(&device.token, "/v1/vault/key-profiles", &register)
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, created) = server
        .post(&owner, "/v1/vault/key-profiles", &register)
        .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(created["activated"], false);
    let admission_key: Vec<u8> = sqlx::query_scalar("SELECT admission_public_key FROM vault_admission_keys WHERE vault_id=$1 AND key_epoch=2 AND profile_fingerprint=$2")
        .bind(server.vault)
        .bind(&fingerprint_two)
        .fetch_one(&server.pool)
        .await
        .unwrap();
    assert_eq!(
        admission_key,
        URL_SAFE_NO_PAD.decode(&admission_key_two).unwrap()
    );
    let (status, duplicate) = server
        .post(&owner, "/v1/vault/key-profiles", &register)
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(duplicate["duplicate"], true);
    let mut changed = register.clone();
    changed["encrypted_vault_check_header"] = json!("AAAA");
    let (status, body) = server
        .post(&owner, "/v1/vault/key-profiles", &changed)
        .await;
    assert_eq!(
        (status, body["code"].clone()),
        (StatusCode::CONFLICT, json!("key_profile_conflict"))
    );
    let (foreign_profile, foreign_fingerprint) = profile(Uuid::new_v4(), 3);
    let (status, _) = server
        .post(&owner, "/v1/vault/key-profiles", &json!({"key_epoch":3,"public_key_profile":foreign_profile,"encrypted_vault_check_header":"AAAA","profile_fingerprint":foreign_fingerprint,"admission_public_key":admission_public_key(server.vault, 3)}))
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let (_, vault) = server.get(&owner, "/v1/vault").await;
    assert_eq!(vault["key_epoch"], 1);

    let early = with_epoch(server.owner_event(1), 2, &fingerprint_two);
    let (status, body) = server.post(&owner, "/v1/events", &early).await;
    assert_eq!(
        (status, body["code"].clone()),
        (StatusCode::CONFLICT, json!("key_epoch_not_active"))
    );

    let accepted_command = set_fingerprint(
        command(
            server.vault,
            server.owner_device,
            2,
            Uuid::new_v4(),
            Uuid::new_v4(),
            gateway.id,
        ),
        &server.fingerprint,
    );
    let (status, original) = server.post(&owner, "/v1/commands", &accepted_command).await;
    assert_eq!(status, StatusCode::OK, "{original}");

    let activate = "/v1/vault/key-profiles/2/activate";
    let (status, _) = server.post(&device.token, activate, &json!({})).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, activated) = server.post(&owner, activate, &json!({})).await;
    assert_eq!(status, StatusCode::OK, "{activated}");
    assert_eq!(activated["duplicate"], false);
    let (_, again) = server.post(&owner, activate, &json!({})).await;
    assert_eq!(again["duplicate"], true);
    let (status, body) = server
        .post(&owner, "/v1/vault/key-profiles/1/activate", &json!({}))
        .await;
    assert_eq!(
        (status, body["code"].clone()),
        (StatusCode::CONFLICT, json!("key_epoch_not_forward"))
    );
    let (status, _) = server
        .post(&owner, "/v1/vault/key-profiles/9/activate", &json!({}))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (_, vault) = server.get(&owner, "/v1/vault").await;
    assert_eq!(vault["key_epoch"], 2);
    assert_eq!(vault["profile_fingerprint"], json!(fingerprint_two));
    assert_eq!(
        vault["encrypted_vault_check_header"],
        json!(header_two),
        "header must be unwrapped base64"
    );
    let (_, history) = server.get(&device.token, "/v1/vault/key-profiles").await;
    let history = history["key_profiles"].as_array().unwrap().clone();
    assert_eq!(history.len(), 2);
    assert_eq!(history[0]["key_epoch"], 1);
    assert_eq!(history[0]["encrypted_vault_check_header"], "BwgJ");
    assert_eq!(history[0]["profile_fingerprint"], json!(server.fingerprint));
    assert_eq!(history[0]["current"], false);
    assert_eq!(history[1]["current"], true);

    let (status, retry) = server.post(&owner, "/v1/commands", &accepted_command).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(retry["cursor"], original["cursor"]);
    assert_eq!(retry["duplicate"], true);
    let retired = set_fingerprint(
        command(
            server.vault,
            server.owner_device,
            3,
            Uuid::new_v4(),
            Uuid::new_v4(),
            gateway.id,
        ),
        &server.fingerprint,
    );
    let (status, body) = server.post(&owner, "/v1/commands", &retired).await;
    assert_eq!(
        (status, body["code"].clone()),
        (StatusCode::CONFLICT, json!("retired_key_epoch"))
    );
    let (status, _) = server
        .post(&owner, "/v1/events", &server.owner_event(4))
        .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "historical activated epochs remain valid for events"
    );
    let (status, _) = server
        .post(
            &owner,
            "/v1/events",
            &with_epoch(server.owner_event(5), 2, &fingerprint_two),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let current_command = with_epoch(
        command(
            server.vault,
            server.owner_device,
            6,
            Uuid::new_v4(),
            Uuid::new_v4(),
            gateway.id,
        ),
        2,
        &fingerprint_two,
    );
    let (status, _) = server.post(&owner, "/v1/commands", &current_command).await;
    assert_eq!(status, StatusCode::OK);

    assert!(sqlx::query("UPDATE vault_key_profiles SET encrypted_vault_check_header='\\x00' WHERE vault_id=$1 AND key_epoch=1").bind(server.vault).execute(&server.pool).await.is_err());
    assert!(
        sqlx::query("DELETE FROM vault_key_profiles WHERE vault_id=$1")
            .bind(server.vault)
            .execute(&server.pool)
            .await
            .is_err()
    );
    server.shutdown().await;
}

#[tokio::test]
async fn key_profile_admission_revalidates_the_exact_owner_after_the_vault_lock() {
    let server = TestServer::start().await;
    let (profile_two, fingerprint_two) = profile(server.vault, 2);
    let request = json!({
        "key_epoch": 2,
        "public_key_profile": profile_two,
        "encrypted_vault_check_header": base64::engine::general_purpose::STANDARD.encode([2_u8; 80]),
        "profile_fingerprint": fingerprint_two,
        "admission_public_key": admission_public_key(server.vault, 2),
    });
    let (holder, holder_pid) = hold_vault_lock(&server).await;
    let base_url = server.base_url.clone();
    let token = server.owner_token.clone();
    let task = tokio::spawn(send_detached(
        Client::new()
            .post(format!("{base_url}/v1/vault/key-profiles"))
            .bearer_auth(token)
            .json(&request),
    ));
    wait_for_lock_waiter(&server, holder_pid, "SELECT key_epoch FROM vaults").await;
    revoke_credential_and_release(holder, server.owner_device).await;
    let (status, body) = finish_blocked(task).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    let profiles: i64 =
        sqlx::query_scalar("SELECT count(*) FROM vault_key_profiles WHERE vault_id=$1")
            .bind(server.vault)
            .fetch_one(&server.pool)
            .await
            .unwrap();
    assert_eq!(profiles, 1);
    server.shutdown().await;
}

#[tokio::test]
async fn key_profile_activation_revalidates_the_owner_role_after_the_vault_lock() {
    let server = TestServer::start().await;
    let (profile_two, fingerprint_two) = profile(server.vault, 2);
    let request = json!({
        "key_epoch": 2,
        "public_key_profile": profile_two,
        "encrypted_vault_check_header": base64::engine::general_purpose::STANDARD.encode([2_u8; 80]),
        "profile_fingerprint": fingerprint_two,
        "admission_public_key": admission_public_key(server.vault, 2),
    });
    let (status, body) = server
        .post(&server.owner_token, "/v1/vault/key-profiles", &request)
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let (mut holder, holder_pid) = hold_vault_lock(&server).await;
    let base_url = server.base_url.clone();
    let token = server.owner_token.clone();
    let task = tokio::spawn(send_detached(
        Client::new()
            .post(format!("{base_url}/v1/vault/key-profiles/2/activate"))
            .bearer_auth(token)
            .json(&json!({})),
    ));
    wait_for_lock_waiter(&server, holder_pid, "SELECT key_epoch FROM vaults").await;
    sqlx::query("UPDATE devices SET role='device' WHERE vault_id=$1 AND device_id=$2")
        .bind(server.vault)
        .bind(server.owner_device)
        .execute(&mut *holder)
        .await
        .unwrap();
    holder.commit().await.unwrap();
    let (status, body) = finish_blocked(task).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    let epoch: i32 = sqlx::query_scalar("SELECT key_epoch FROM vaults WHERE vault_id=$1")
        .bind(server.vault)
        .fetch_one(&server.pool)
        .await
        .unwrap();
    assert_eq!(epoch, 1);
    server.shutdown().await;
}

#[tokio::test]
async fn key_profile_activation_requires_a_matching_admission_verifier() {
    let server = TestServer::start().await;
    let (profile_two, fingerprint_two) = profile(server.vault, 2);
    let header_two = vec![2_u8; 80];
    sqlx::query("INSERT INTO vault_key_profiles(vault_id,key_epoch,public_key_profile,encrypted_vault_check_header,profile_fingerprint,created_by_device_id) VALUES($1,2,$2,$3,$4,$5)")
        .bind(server.vault)
        .bind(&profile_two)
        .bind(&header_two)
        .bind(&fingerprint_two)
        .bind(server.owner_device)
        .execute(&server.pool)
        .await
        .unwrap();

    let activate = "/v1/vault/key-profiles/2/activate";
    let (status, body) = server.post(&server.owner_token, activate, &json!({})).await;
    assert_eq!(
        (status, body["code"].clone()),
        (
            StatusCode::CONFLICT,
            json!("key_profile_admission_key_missing")
        )
    );

    sqlx::query("INSERT INTO vault_admission_keys(vault_id,key_epoch,profile_fingerprint,admission_public_key) VALUES($1,2,$2,$3)")
        .bind(server.vault)
        .bind(&fingerprint_two)
        .bind(URL_SAFE_NO_PAD.decode(admission_public_key(server.vault, 2)).unwrap())
        .execute(&server.pool)
        .await
        .unwrap();
    let (status, activated) = server.post(&server.owner_token, activate, &json!({})).await;
    assert_eq!(status, StatusCode::OK, "{activated}");
    assert_eq!(activated["duplicate"], false);
    server.shutdown().await;
}

#[tokio::test]
async fn pending_command_references_never_retarget_a_gateway() {
    let server = TestServer::start().await;
    let owner = server.owner_token.clone();
    let first = server.provision_device("gateway").await;
    let second = server.provision_device("gateway").await;
    let envelope_id = Uuid::new_v4();
    let command_id = Uuid::new_v4();
    let original = set_fingerprint(
        command(
            server.vault,
            server.owner_device,
            1,
            envelope_id,
            command_id,
            first.id,
        ),
        &server.fingerprint,
    );
    let (status, accepted) = server.post(&owner, "/v1/commands", &original).await;
    assert_eq!(status, StatusCode::OK);
    let delivered_id = Uuid::new_v4();
    let delivered = set_fingerprint(
        command(
            server.vault,
            server.owner_device,
            2,
            Uuid::new_v4(),
            delivered_id,
            second.id,
        ),
        &server.fingerprint,
    );
    assert_eq!(
        server.post(&owner, "/v1/commands", &delivered).await.0,
        StatusCode::OK
    );
    let (status, _) = server
        .post(
            &second.token,
            &format!("/v1/commands/{delivered_id}/receipts"),
            &json!({"receipt":{"state":"sent"}}),
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (_, pending) = server.get(&owner, "/v1/commands/pending").await;
    assert_eq!(
        pending["commands"],
        json!([{"command_id":command_id,"producer_device_id":server.owner_device,"gateway_device_id":first.id,"cursor":accepted["cursor"]}])
    );
    let (_, targeted) = server.get(&first.token, "/v1/commands/pending").await;
    assert_eq!(targeted["commands"].as_array().unwrap().len(), 1);
    let (_, other) = server.get(&second.token, "/v1/commands/pending").await;
    assert_eq!(other["commands"], json!([]));

    let mut retarget = original.clone();
    retarget["route"]["gateway_device_id"] = json!(second.id);
    let (status, _) = server.post(&owner, "/v1/commands", &retarget).await;
    assert_eq!(status, StatusCode::CONFLICT);
    retarget["envelope_id"] = json!(Uuid::new_v4());
    retarget["producer_sequence"] = json!("3");
    let (status, _) = server.post(&owner, "/v1/commands", &retarget).await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "a command id cannot move to another gateway"
    );

    let (status, _) = server
        .post(
            &owner,
            &format!("/v1/devices/{}/revoke", first.id),
            &json!({}),
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, retry) = server.post(&owner, "/v1/commands", &original).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(retry, json!({"cursor":accepted["cursor"],"duplicate":true}));
    let (_, pending) = server.get(&owner, "/v1/commands/pending").await;
    assert_eq!(pending["commands"][0]["gateway_device_id"], json!(first.id));
    let (status, _) = server
        .post(
            &second.token,
            &format!("/v1/commands/{command_id}/receipts"),
            &json!({"receipt":{"state":"sent"}}),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (_, page) = server
        .get(
            &owner,
            &format!(
                "/v1/snapshot/records?high_water={}",
                accepted["cursor"].as_str().unwrap()
            ),
        )
        .await;
    assert_eq!(
        page["records"][0]["envelope"]["route"]["gateway_device_id"],
        json!(first.id)
    );
    server.shutdown().await;
}

#[tokio::test]
async fn ingest_accepts_carrier_and_rcs_link_routes() {
    let server = TestServer::start().await;
    let gateway = server.provision_device("gateway").await;
    let carrier = set_fingerprint(
        command(
            server.vault,
            server.owner_device,
            1,
            Uuid::new_v4(),
            Uuid::new_v4(),
            gateway.id,
        ),
        &server.fingerprint,
    );
    let rcs_link = set_fingerprint(
        rcs_link_command(
            server.vault,
            server.owner_device,
            2,
            Uuid::new_v4(),
            Uuid::new_v4(),
            gateway.id,
        ),
        &server.fingerprint,
    );

    assert_eq!(
        server
            .post(&server.owner_token, "/v1/commands", &carrier)
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        server
            .post(&server.owner_token, "/v1/commands", &rcs_link)
            .await
            .0,
        StatusCode::OK
    );
    server.shutdown().await;
}

#[tokio::test]
async fn rcs_link_commands_are_delivered_only_to_the_addressed_gateway() {
    let server = TestServer::start().await;
    let addressed = server.provision_device("gateway").await;
    let other = server.provision_device("gateway").await;
    let command_id = Uuid::new_v4();
    let envelope = set_fingerprint(
        rcs_link_command(
            server.vault,
            server.owner_device,
            1,
            Uuid::new_v4(),
            command_id,
            addressed.id,
        ),
        &server.fingerprint,
    );

    assert_eq!(
        server
            .post(&server.owner_token, "/v1/commands", &envelope)
            .await
            .0,
        StatusCode::OK
    );
    let (_, delivered) = server.get(&addressed.token, "/v1/commands/pending").await;
    assert_eq!(delivered["commands"][0]["command_id"], json!(command_id));
    let (_, not_delivered) = server.get(&other.token, "/v1/commands/pending").await;
    assert_eq!(not_delivered["commands"], json!([]));
    server.shutdown().await;
}

#[tokio::test]
async fn legacy_local_schema_upgrades_without_losing_records_or_identity() {
    let database_url = std::env::var("TEST_DATABASE_URL")
        .expect("TEST_DATABASE_URL is required; integration tests never silently skip");
    let admin = PgPoolOptions::new()
        .max_connections(2)
        .connect(&database_url)
        .await
        .unwrap();
    let schema = format!("peppy_server_test_{}", Uuid::new_v4().simple());
    sqlx::query(&format!("CREATE SCHEMA {schema}"))
        .execute(&admin)
        .await
        .unwrap();
    let mut isolated_url: Url = database_url.parse().unwrap();
    isolated_url
        .query_pairs_mut()
        .append_pair("options", &format!("-csearch_path={schema}"));
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(isolated_url.as_str())
        .await
        .unwrap();

    let full = sqlx::migrate!("./migrations");
    let mut legacy = sqlx::migrate!("./migrations");
    legacy.migrations = std::borrow::Cow::Owned(
        full.migrations
            .iter()
            .filter(|m| m.version < 7)
            .cloned()
            .collect(),
    );
    legacy.run(&pool).await.unwrap();

    // Rows exactly as the pre-A4 server wrote them.
    let vault = Uuid::new_v4();
    let owner_device = Uuid::new_v4();
    let gateway = Uuid::new_v4();
    let (vault_profile, fingerprint) = profile(vault, 1);
    let token = "ab".repeat(48);
    sqlx::query("INSERT INTO vaults(vault_id,public_key_profile,encrypted_vault_check_header,key_epoch,profile_fingerprint,next_cursor) VALUES($1,$2,$3,1,$4,2)")
        .bind(vault).bind(&vault_profile).bind(vec![7_u8, 8, 9]).bind(&fingerprint)
        .execute(&pool).await.unwrap();
    for (device, role) in [(owner_device, "owner"), (gateway, "gateway")] {
        sqlx::query("INSERT INTO devices(vault_id,device_id,role,public_key,profile_fingerprint,key_epoch) VALUES($1,$2,$3,'{}'::jsonb,$4,1)")
            .bind(vault).bind(device).bind(role).bind(&fingerprint)
            .execute(&pool).await.unwrap();
    }
    sqlx::query("INSERT INTO device_credentials(token_digest,vault_id,device_id) VALUES($1,$2,$3)")
        .bind(Sha256::digest(token.as_bytes()).as_slice())
        .bind(vault)
        .bind(owner_device)
        .execute(&pool)
        .await
        .unwrap();
    let command_id = Uuid::new_v4();
    let legacy_event = set_fingerprint(
        event(vault, owner_device, 1, Uuid::new_v4(), 1),
        &fingerprint,
    );
    let legacy_command = set_fingerprint(
        command(vault, owner_device, 2, Uuid::new_v4(), command_id, gateway),
        &fingerprint,
    );
    for (cursor, body) in [(1_i64, &legacy_event), (2, &legacy_command)] {
        let envelope: peppy_protocol::Envelope = serde_json::from_value(body.clone()).unwrap();
        let digest = envelope.wire_digest().unwrap();
        let canonical = serde_json::to_value(&envelope).unwrap();
        let purpose = if envelope.command_id.is_some() {
            "command"
        } else {
            "event"
        };
        sqlx::query("INSERT INTO event_log(vault_id,cursor,envelope_id,producer_device_id,producer_sequence,purpose,command_id,cipher_digest,envelope) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9)")
            .bind(vault).bind(cursor).bind(envelope.envelope_id.0).bind(owner_device).bind(cursor)
            .bind(purpose).bind(envelope.command_id.map(|id| id.0)).bind(digest.as_slice()).bind(&canonical)
            .execute(&pool).await.unwrap();
        sqlx::query(
            "INSERT INTO encrypted_records(vault_id,envelope_id,envelope) VALUES($1,$2,$3)",
        )
        .bind(vault)
        .bind(envelope.envelope_id.0)
        .bind(&canonical)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO outbox_jobs(vault_id,cursor,kind,payload) VALUES($1,$2,'sync',$3)",
        )
        .bind(vault)
        .bind(cursor)
        .bind(&canonical)
        .execute(&pool)
        .await
        .unwrap();
    }
    sqlx::query("INSERT INTO commands(vault_id,producer_device_id,command_id,gateway_device_id,cipher_digest,cursor) SELECT vault_id,producer_device_id,command_id,$2,cipher_digest,cursor FROM event_log WHERE vault_id=$1 AND command_id IS NOT NULL")
        .bind(vault).bind(gateway).execute(&pool).await.unwrap();

    full.run(&pool).await.unwrap();

    let backfilled: Vec<(i64, Uuid, i64, String)> = sqlx::query_as("SELECT cursor,producer_device_id,producer_sequence,purpose FROM encrypted_records WHERE vault_id=$1 ORDER BY cursor")
        .bind(vault).fetch_all(&pool).await.unwrap();
    assert_eq!(
        backfilled,
        vec![
            (1, owner_device, 1, "event".into()),
            (2, owner_device, 2, "command".into())
        ]
    );
    let (epoch, activated): (i32, bool) = sqlx::query_as(
        "SELECT key_epoch, activated_at IS NOT NULL FROM vault_key_profiles WHERE vault_id=$1",
    )
    .bind(vault)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!((epoch, activated), (1, true));
    let preserved: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM outbox_jobs WHERE vault_id=$1 AND payload IS NOT NULL",
    )
    .bind(vault)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        preserved, 2,
        "legacy outbox rows are not rewritten by the migration"
    );

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let server_pool = pool.clone();
    let task = tokio::spawn(async move {
        axum::serve(
            listener,
            router_with_options(server_pool, TransportOptions::default()),
        )
        .await
        .unwrap();
    });
    let client = Client::new();
    let snapshot: Value = client
        .get(format!("{base_url}/v1/snapshot"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(snapshot["record_count"], "2");
    assert_eq!(snapshot["high_water_cursor"], "2");
    let retry: Value = client
        .post(format!("{base_url}/v1/commands"))
        .bearer_auth(&token)
        .json(&legacy_command)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(retry, json!({"cursor":"2","duplicate":true}));
    let accepted: Value = client
        .post(format!("{base_url}/v1/events"))
        .bearer_auth(&token)
        .json(&set_fingerprint(
            event(vault, owner_device, 3, Uuid::new_v4(), 3),
            &fingerprint,
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(accepted, json!({"cursor":"3","duplicate":false}));

    task.abort();
    let _ = task.await;
    pool.close().await;
    sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
        .execute(&admin)
        .await
        .unwrap();
    admin.close().await;
}

#[tokio::test]
async fn near_limit_envelopes_page_within_the_byte_budget_and_advance() {
    let server = TestServer::start().await;
    let budget = 8 * 1024 * 1024_usize;
    for sequence in 1..=8_u64 {
        let mut body = server.owner_event(sequence);
        body["ciphertext"] =
            json!(
                base64::engine::general_purpose::STANDARD.encode(vec![sequence as u8; 1_048_576])
            );
        assert_eq!(server.commit(&body).await, sequence.to_string());
    }

    for (path, items) in [
        ("/v1/events?limit=200&after=", "events"),
        (
            "/v1/snapshot/records?high_water=8&limit=200&after=",
            "records",
        ),
    ] {
        let mut seen: Vec<u64> = Vec::new();
        let mut after = "0".to_owned();
        let mut pages = 0;
        loop {
            let response = Client::new()
                .get(format!("{}{path}{after}", server.base_url))
                .bearer_auth(&server.owner_token)
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let bytes = response.bytes().await.unwrap();
            // One envelope (~1.4 MB) may exceed the remaining budget, never more.
            assert!(
                bytes.len() < budget + 1_500_000,
                "{path} page was {} bytes",
                bytes.len()
            );
            let page: Value = serde_json::from_slice(&bytes).unwrap();
            let batch = cursors(&page[items]);
            assert!(
                !batch.is_empty() && batch.len() < 8,
                "{path} page must be budget-limited"
            );
            seen.extend(&batch);
            pages += 1;
            match page["next_after"].as_str() {
                Some(next) => {
                    assert_eq!(next, batch.last().unwrap().to_string());
                    after = next.to_owned();
                }
                None => break,
            }
        }
        assert_eq!(seen, (1..=8).collect::<Vec<_>>(), "{path}");
        assert!(pages >= 2, "{path}");
    }

    let mut ws = server.connect_ws(&server.owner_token).await;
    send_hello(&mut ws, "0").await;
    assert_eq!(next_json(&mut ws, 5).await["type"], "ready");
    for cursor in 1..=8 {
        let frame = next_json(&mut ws, 10).await;
        assert_eq!(frame["cursor"], cursor.to_string());
        assert_eq!(
            frame["envelope"]["ciphertext"].as_str().unwrap().len(),
            1_398_104
        );
    }
    server.shutdown().await;
}

/// One real path: a real client publishes legacy history, upgrades, removes; the real server
/// ages, prunes and compacts it; a fresh real client imports the fenced snapshot and nothing
/// resurrects.
#[tokio::test]
async fn real_client_history_purges_on_the_real_server_and_fresh_snapshot_does_not_resurrect() {
    use peppy_client_core as core;
    let pass = "correct horse battery staple";
    let vault = Uuid::new_v4();
    let key_profile = peppy_crypto::KeyProfile::new(vault, 1).unwrap();
    let root = peppy_crypto::derive_root_key(pass, &key_profile).unwrap();
    let header = peppy_crypto::create_vault_check_header(&root, key_profile.clone()).unwrap();
    let server = TestServer::start_real(&key_profile, &header).await;
    let dir = tempfile::TempDir::new().unwrap();
    let open = |name: &str, device: Uuid| {
        let client = core::Client::open(
            core::ClientConfig {
                database_path: dir.path().join(name),
                vault_id: core::VaultId(vault),
                device_id: core::DeviceId(device),
            },
            core::DatabaseKey::new(&[5; 32]).unwrap(),
        )
        .unwrap();
        client.unlock(&key_profile, &header, pass).unwrap();
        client
    };
    let upload = |client: &core::Client| {
        let pending = client.pending_outbox().unwrap();
        let server = &server;
        async move {
            for envelope in &pending {
                let (status, body) = server
                    .post(
                        &server.owner_token,
                        "/v1/events",
                        &serde_json::to_value(envelope).unwrap(),
                    )
                    .await;
                assert_eq!(status, StatusCode::OK, "{body}");
            }
            pending
        }
    };
    let connect = |client: &core::Client, start: &Value| -> Value {
        serde_json::from_str(
            &client
                .set_server_compaction_state(
                    start["compaction_supported"].as_bool().unwrap(),
                    start["compaction_active"].as_bool().unwrap(),
                )
                .unwrap(),
        )
        .unwrap()
    };
    let phone = open("phone.db", server.owner_device);
    let capture = |text: &str| core::NotificationCapture {
        notification_key: "progress".into(),
        instance: "i".into(),
        package_name: "com.example".into(),
        app_name: "App".into(),
        app_icon_png_base64: None,
        title: "Download".into(),
        text: text.into(),
        category: None,
        posted_at: 1,
        dismissible: true,
    };
    // Pre-upgrade: three legacy posts (no server capability recorded yet).
    for text in ["10%", "50%", "90%"] {
        phone.capture_notification(capture(text)).unwrap();
    }
    let legacy = upload(&phone).await;
    assert_eq!(legacy.len(), 3);
    assert!(legacy.iter().all(|e| e.compaction.is_none()));
    for e in &legacy {
        phone.ack_outbox(e.envelope_id).unwrap();
    }

    // Upgrade handshake: declare, read the snapshot fields, record them.
    let (status, _) = server
        .post(&server.owner_token, "/v1/compaction/capability", &json!({}))
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, start) = server.get(&server.owner_token, "/v1/snapshot").await;
    let ready = connect(&phone, &start);
    assert_eq!(ready["state"], "ready", "{ready}");
    assert_eq!(ready["history_compacting"], true);
    phone.remove_notification("progress", "i").unwrap();
    let removal = upload(&phone).await;
    assert_eq!(removal.len(), 1);
    assert_eq!(removal[0].compaction.as_ref().unwrap().supersedes.len(), 3);

    // Age everything past the replay window and the terminal window, then a real pass.
    sqlx::query("UPDATE event_log SET created_at=now()-interval '31 days' WHERE vault_id=$1")
        .bind(vault)
        .execute(&server.pool)
        .await
        .unwrap();
    prune_replay_log(&server.pool, Duration::from_secs(30 * 86_400))
        .await
        .unwrap();
    sqlx::query(
        "UPDATE record_compaction SET created_at=now()-interval '91 days' WHERE vault_id=$1",
    )
    .bind(vault)
    .execute(&server.pool)
    .await
    .unwrap();
    assert_eq!(compact_records(&server.pool).await.unwrap(), 4);
    let retained: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM encrypted_records WHERE vault_id=$1")
            .bind(vault)
            .fetch_one(&server.pool)
            .await
            .unwrap();
    assert_eq!(retained, 0);

    // A fresh client imports the fenced snapshot.
    let fresh = open("fresh.db", Uuid::new_v4());
    let (_, start) = server.get(&server.owner_token, "/v1/snapshot").await;
    let generation: u64 = start["compaction_generation"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(generation, 1);
    let high_water: u64 = start["high_water_cursor"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let count: u64 = start["record_count"].as_str().unwrap().parse().unwrap();
    assert_eq!(count, 0);
    let mut progress = fresh
        .begin_snapshot_with_compaction(
            core::Cursor(high_water),
            count,
            core::SnapshotPurpose::Resync,
            Some(generation),
        )
        .unwrap();
    while progress.received_records < progress.expected_records {
        let (status, page) = server.get(&server.owner_token, &format!(
            "/v1/snapshot/records?high_water={high_water}&compaction_generation={generation}&after={}", progress.last_cursor.0)).await;
        assert_eq!(status, StatusCode::OK);
        let records = page["records"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| core::RawSnapshotRecord {
                cursor: core::Cursor(r["cursor"].as_str().unwrap().parse().unwrap()),
                envelope_json: serde_json::to_vec(&r["envelope"]).unwrap(),
            })
            .collect::<Vec<_>>();
        progress = fresh
            .append_snapshot_raw_page(progress.generation, &records)
            .unwrap();
    }
    fresh.finish_snapshot(progress.generation).unwrap();
    while fresh.apply_pending(100).unwrap().snapshot_remaining > 0 {}
    assert!(
        fresh
            .notification_snapshot()
            .unwrap()
            .notifications
            .is_empty()
    );
    // A stale pre-purge fence is refused rather than silently partial.
    let (status, restart) = server
        .get(
            &server.owner_token,
            &format!(
                "/v1/snapshot/records?high_water={high_water}&compaction_generation=0&after=0"
            ),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(restart["reason"], "compaction_generation_changed");
    // A retry of a purged legacy post stays a duplicate.
    let (status, again) = server
        .post(
            &server.owner_token,
            "/v1/events",
            &serde_json::to_value(&legacy[0]).unwrap(),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(again["duplicate"], true);
    server.shutdown().await;
}
