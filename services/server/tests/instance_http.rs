use std::{net::SocketAddr, time::Duration};

use axum::{
    Router,
    body::Body,
    extract::ConnectInfo,
    http::{Method, Request, Response, StatusCode},
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use http_body_util::BodyExt;
use peppy_crypto::KeyProfile;
use peppy_server::{
    ServerBuilder,
    api::{
        CommunityEnrollmentAuthorization, EnrollmentAuthorization, EnrollmentAuthorizationRequest,
        EnrollmentOperation,
    },
    config::Config,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{PgPool, postgres::PgPoolOptions};
use tower::ServiceExt;
use url::Url;
use uuid::Uuid;

const SETUP_TOKEN: &str = "fixed-instance-http-setup-token-at-least-32-bytes";
const ADMIN_PASSWORD: &str = "correct horse battery staple";
const ORIGIN: &str = "http://localhost:3000";

struct Fixture {
    admin: PgPool,
    pool: PgPool,
    schema: String,
}

fn test_database_url() -> String {
    std::env::var("TEST_DATABASE_URL")
        .expect("TEST_DATABASE_URL must identify a disposable PostgreSQL fixture")
}

impl Fixture {
    async fn new() -> Self {
        let database_url = test_database_url();
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .expect("disposable PostgreSQL fixture must be available");
        let schema = format!("peppy_instance_http_{}", Uuid::new_v4().simple());
        sqlx::query(&format!("CREATE SCHEMA {schema}"))
            .execute(&admin)
            .await
            .unwrap();
        let mut url: Url = database_url.parse().unwrap();
        url.query_pairs_mut()
            .append_pair("options", &format!("-csearch_path={schema}"));
        let pool = PgPoolOptions::new()
            .max_connections(8)
            .connect(url.as_str())
            .await
            .unwrap();
        peppy_server::MIGRATOR.run(&pool).await.unwrap();
        Self {
            admin,
            pool,
            schema,
        }
    }

    async fn community_app(&self) -> Router {
        self.community_app_with_trusted_proxies(Vec::new()).await
    }

    async fn community_app_with_trusted_proxies(
        &self,
        trusted_proxy_cidrs: Vec<ipnet::IpNet>,
    ) -> Router {
        let mut server_config = config(self.pool.clone());
        server_config.trusted_proxy_cidrs = trusted_proxy_cidrs;
        ServerBuilder::community(server_config, self.pool.clone())
            .setup_token(SETUP_TOKEN)
            .build()
            .await
            .unwrap()
            .router()
    }

    async fn restarted_community_app(&self) -> Router {
        ServerBuilder::community(config(self.pool.clone()), self.pool.clone())
            .build()
            .await
            .unwrap()
            .router()
    }

    async fn open_community_app(&self) -> Router {
        ServerBuilder::community(config(self.pool.clone()), self.pool.clone())
            .setup_token("")
            .build()
            .await
            .unwrap()
            .router()
    }

    async fn hosted_app(&self) -> Router {
        ServerBuilder::new(config(self.pool.clone()), self.pool.clone())
            .setup_token(SETUP_TOKEN)
            .row_security(true)
            .maintenance_database(self.pool.clone())
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

fn config(_pool: PgPool) -> Config {
    Config {
        bind_addr: "127.0.0.1:0".parse::<SocketAddr>().unwrap(),
        database_url: test_database_url(),
        release_identity: "instance-http-test".into(),
        s3: None,
        public_api_url: Some(Url::parse(ORIGIN).unwrap()),
        public_attachment_url: None,
        vault_attachment_quota_bytes: 123_456,
        trusted_proxy_cidrs: Vec::new(),
        replay_retention: Duration::from_secs(7 * 86_400),
        relay_url: None,
        web_client: None,
    }
}

fn claim_request(operation_id: Uuid, vault_id: Uuid, device_id: Uuid, kind: &str) -> Value {
    let profile = KeyProfile::new(vault_id, 1).unwrap();
    json!({
        "operation_id": operation_id,
        "setup_token": SETUP_TOKEN,
        "admin_password": ADMIN_PASSWORD,
        "device": {
            "device_id": device_id,
            "kind": kind,
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

async fn send(
    app: Router,
    method: Method,
    path: &str,
    body: Option<&Value>,
    headers: &[(&str, &str)],
) -> Response<Body> {
    send_from(app, method, path, body, headers, None).await
}

async fn send_from(
    app: Router,
    method: Method,
    path: &str,
    body: Option<&Value>,
    headers: &[(&str, &str)],
    peer: Option<SocketAddr>,
) -> Response<Body> {
    let mut request = Request::builder().method(method).uri(path);
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    if body.is_some() {
        request = request.header("content-type", "application/json");
    }
    let mut request = request
        .body(body.map_or_else(Body::empty, |value| Body::from(value.to_string())))
        .unwrap();
    if let Some(peer) = peer {
        request.extensions_mut().insert(ConnectInfo(peer));
    }
    app.oneshot(request).await.unwrap()
}

fn public_claim_digest(request: &Value) -> Vec<u8> {
    let mut public = request.clone();
    let object = public.as_object_mut().unwrap();
    object.remove("admin_password");
    object.remove("setup_token");
    Sha256::digest(serde_json::to_vec(&public).unwrap()).to_vec()
}

async fn json_body(response: Response<Body>) -> Value {
    serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap()
}

fn session_cookie(response: &Response<Body>) -> String {
    response
        .headers()
        .get("set-cookie")
        .unwrap()
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned()
}

async fn post_login_from(address: SocketAddr, forwarded_for: Option<&str>) -> reqwest::Response {
    let mut request = reqwest::Client::new()
        .post(format!("http://{address}/v1/instance/session"))
        .json(&json!({"admin_password": "wrong password value", "client": "desktop"}));
    if let Some(forwarded_for) = forwarded_for {
        request = request.header("x-forwarded-for", forwarded_for);
    }
    request.send().await.unwrap()
}

#[tokio::test]
async fn real_tcp_peers_have_independent_login_buckets_and_cannot_spoof_forwarded_for() {
    let fixture = Fixture::new().await;
    let app = fixture.community_app().await;
    let claim = claim_request(Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4(), "desktop");
    let claimed = send(
        app.clone(),
        Method::POST,
        "/v1/instance/claim",
        Some(&claim),
        &[],
    )
    .await;
    assert_eq!(claimed.status(), StatusCode::OK);

    let ipv4_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let ipv4_address = ipv4_listener.local_addr().unwrap();
    let ipv4_server = tokio::spawn(peppy_server::serve(
        ipv4_listener,
        app.clone(),
        std::future::pending(),
    ));
    let ipv6_listener = tokio::net::TcpListener::bind("[::1]:0").await.unwrap();
    let ipv6_address = ipv6_listener.local_addr().unwrap();
    let ipv6_server = tokio::spawn(peppy_server::serve(
        ipv6_listener,
        app,
        std::future::pending(),
    ));

    for attempt in 0..10 {
        let spoofed = format!("198.51.100.{attempt}");
        let response = post_login_from(ipv4_address, Some(&spoofed)).await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }
    let throttled = post_login_from(ipv4_address, Some("203.0.113.250")).await;
    assert_eq!(throttled.status(), StatusCode::TOO_MANY_REQUESTS);

    let independent = post_login_from(ipv6_address, Some("203.0.113.250")).await;
    assert_eq!(independent.status(), StatusCode::UNAUTHORIZED);

    ipv4_server.abort();
    ipv6_server.abort();
    fixture.cleanup().await;
}

#[tokio::test]
async fn trusted_proxy_uses_the_first_untrusted_forwarded_hop_for_login_buckets() {
    let fixture = Fixture::new().await;
    let app = fixture
        .community_app_with_trusted_proxies(vec!["127.0.0.0/8".parse().unwrap()])
        .await;
    let claim = claim_request(Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4(), "desktop");
    let claimed = send(
        app.clone(),
        Method::POST,
        "/v1/instance/claim",
        Some(&claim),
        &[],
    )
    .await;
    assert_eq!(claimed.status(), StatusCode::OK);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(peppy_server::serve(listener, app, std::future::pending()));

    for _ in 0..10 {
        let response = post_login_from(address, Some("198.51.100.1, 127.0.0.8")).await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }
    let first_limited = post_login_from(address, Some("198.51.100.1, 127.0.0.8")).await;
    assert_eq!(first_limited.status(), StatusCode::TOO_MANY_REQUESTS);
    let independent = post_login_from(address, Some("198.51.100.2, 127.0.0.8")).await;
    assert_eq!(independent.status(), StatusCode::UNAUTHORIZED);

    server.abort();
    fixture.cleanup().await;
}

#[tokio::test]
async fn trusted_proxy_malformed_missing_and_oversized_chains_share_peer_fallback_bucket() {
    let fixture = Fixture::new().await;
    let app = fixture
        .community_app_with_trusted_proxies(vec!["127.0.0.0/8".parse().unwrap()])
        .await;
    let claim = claim_request(Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4(), "desktop");
    let claimed = send(
        app.clone(),
        Method::POST,
        "/v1/instance/claim",
        Some(&claim),
        &[],
    )
    .await;
    assert_eq!(claimed.status(), StatusCode::OK);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(peppy_server::serve(listener, app, std::future::pending()));
    let too_many_hops = (0..17)
        .map(|index| format!("198.51.100.{index}"))
        .collect::<Vec<_>>()
        .join(",");
    let oversized = format!("{}1", "1".repeat(8192));

    for forwarded_for in [
        None,
        Some("not-an-ip"),
        Some("198.51.100.1,,127.0.0.8"),
        Some(too_many_hops.as_str()),
        Some(oversized.as_str()),
        None,
        Some("not-an-ip"),
        Some("198.51.100.1,,127.0.0.8"),
        Some(too_many_hops.as_str()),
        Some(oversized.as_str()),
    ] {
        let response = post_login_from(address, forwarded_for).await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }
    let throttled = post_login_from(address, None).await;
    assert_eq!(throttled.status(), StatusCode::TOO_MANY_REQUESTS);

    server.abort();
    fixture.cleanup().await;
}

#[tokio::test]
async fn native_claim_is_atomic_idempotent_rate_limited_and_authorizes_management() {
    let fixture = Fixture::new().await;
    let app = fixture.community_app().await;

    let discovery = send(app.clone(), Method::GET, "/v1/instance", None, &[]).await;
    assert_eq!(discovery.status(), StatusCode::OK);
    assert_eq!(
        json_body(discovery).await,
        json!({
            "mode": "community",
            "claimed": false,
            "setup_required": true,
            "password_auth": false,
            "qr_pairing": false,
            "shared_secret_pairing": false,
        })
    );

    let mut wrong_token = claim_request(Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4(), "desktop");
    wrong_token["setup_token"] = json!("not-the-operator-token");
    let mut absent_token = wrong_token.clone();
    absent_token.as_object_mut().unwrap().remove("setup_token");
    let absent_rejected = send(
        app.clone(),
        Method::POST,
        "/v1/instance/claim",
        Some(&absent_token),
        &[],
    )
    .await;
    assert_eq!(absent_rejected.status(), StatusCode::UNAUTHORIZED);
    let rejected = send_from(
        app.clone(),
        Method::POST,
        "/v1/instance/claim",
        Some(&wrong_token),
        &[("x-forwarded-for", "198.51.100.10")],
        Some("198.51.100.10:1234".parse().unwrap()),
    )
    .await;
    assert_eq!(rejected.status(), StatusCode::UNAUTHORIZED);

    let requests = [
        claim_request(Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4(), "desktop"),
        claim_request(Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4(), "android"),
    ];
    let first = send(
        app.clone(),
        Method::POST,
        "/v1/instance/claim",
        Some(&requests[0]),
        &[("x-forwarded-for", "198.51.100.11")],
    );
    let second = send(
        app.clone(),
        Method::POST,
        "/v1/instance/claim",
        Some(&requests[1]),
        &[("x-forwarded-for", "198.51.100.12")],
    );
    let (first, second) = tokio::join!(first, second);
    let statuses = [first.status(), second.status()];
    assert_eq!(
        statuses
            .iter()
            .filter(|status| **status == StatusCode::OK)
            .count(),
        1
    );
    assert_eq!(
        statuses
            .iter()
            .filter(|status| **status == StatusCode::CONFLICT)
            .count(),
        1
    );
    let (winner_request, winner_response) = if first.status() == StatusCode::OK {
        (&requests[0], first)
    } else {
        (&requests[1], second)
    };
    let winner_body = json_body(winner_response).await;
    assert_eq!(winner_body["replayed"], false);
    let native_token = winner_body["session"]["session_token"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(!native_token.is_empty());
    assert!(winner_body["session"]["csrf_token"].is_null());

    let owner_count: i64 = sqlx::query_scalar("SELECT count(*) FROM vault_owners")
        .fetch_one(&fixture.pool)
        .await
        .unwrap();
    let vault_count: i64 = sqlx::query_scalar("SELECT count(*) FROM vaults")
        .fetch_one(&fixture.pool)
        .await
        .unwrap();
    assert_eq!((owner_count, vault_count), (1, 1));
    let device_count: i64 = sqlx::query_scalar("SELECT count(*) FROM devices")
        .fetch_one(&fixture.pool)
        .await
        .unwrap();
    let credential_count: i64 = sqlx::query_scalar("SELECT count(*) FROM device_credentials")
        .fetch_one(&fixture.pool)
        .await
        .unwrap();
    assert_eq!((device_count, credential_count), (1, 1));
    let stored_request_digest: Vec<u8> =
        sqlx::query_scalar("SELECT request_digest FROM vault_owners")
            .fetch_one(&fixture.pool)
            .await
            .unwrap();
    assert_eq!(stored_request_digest, public_claim_digest(winner_request));
    assert_ne!(
        stored_request_digest,
        Sha256::digest(serde_json::to_vec(winner_request).unwrap()).to_vec()
    );
    let mut changed_secrets = winner_request.clone();
    changed_secrets["admin_password"] = json!("different administrator password");
    changed_secrets["setup_token"] = json!("different setup authority");
    assert_eq!(
        stored_request_digest,
        public_claim_digest(&changed_secrets),
        "private claim secrets must not affect the stored idempotency digest"
    );

    let replay = send(
        app.clone(),
        Method::POST,
        "/v1/instance/claim",
        Some(winner_request),
        &[("x-forwarded-for", "198.51.100.13")],
    )
    .await;
    assert_eq!(replay.status(), StatusCode::OK);
    assert_eq!(json_body(replay).await["replayed"], true);
    let mut omitted_token_replay = winner_request.clone();
    omitted_token_replay
        .as_object_mut()
        .unwrap()
        .remove("setup_token");
    let omitted_token_replay = send(
        app.clone(),
        Method::POST,
        "/v1/instance/claim",
        Some(&omitted_token_replay),
        &[],
    )
    .await;
    assert_eq!(omitted_token_replay.status(), StatusCode::OK);
    assert_eq!(json_body(omitted_token_replay).await["replayed"], true);
    let mut empty_token_replay = winner_request.clone();
    empty_token_replay["setup_token"] = json!("");
    let empty_token_replay = send(
        app.clone(),
        Method::POST,
        "/v1/instance/claim",
        Some(&empty_token_replay),
        &[],
    )
    .await;
    assert_eq!(empty_token_replay.status(), StatusCode::OK);
    assert_eq!(json_body(empty_token_replay).await["replayed"], true);

    let mut wrong_password_retry = winner_request.clone();
    wrong_password_retry["admin_password"] = json!("different administrator password");
    let rejected_retry = send(
        app.clone(),
        Method::POST,
        "/v1/instance/claim",
        Some(&wrong_password_retry),
        &[],
    )
    .await;
    assert_eq!(rejected_retry.status(), StatusCode::CONFLICT);
    let mut wrong_authority_retry = winner_request.clone();
    wrong_authority_retry["setup_token"] = json!("different setup authority");
    let rejected_authority = send_from(
        app.clone(),
        Method::POST,
        "/v1/instance/claim",
        Some(&wrong_authority_retry),
        &[],
        Some("192.0.2.19:1234".parse().unwrap()),
    )
    .await;
    assert_eq!(rejected_authority.status(), StatusCode::OK);
    assert_eq!(json_body(rejected_authority).await["replayed"], true);

    let mut wrong_operation = winner_request.clone();
    wrong_operation["operation_id"] = json!(Uuid::new_v4());
    let conflict = send(
        app.clone(),
        Method::POST,
        "/v1/instance/claim",
        Some(&wrong_operation),
        &[("x-forwarded-for", "198.51.100.14")],
    )
    .await;
    assert_eq!(conflict.status(), StatusCode::CONFLICT);

    let unauthorized = send(
        app.clone(),
        Method::GET,
        "/v1/instance/management",
        None,
        &[],
    )
    .await;
    assert_eq!(unauthorized.status(), StatusCode::FORBIDDEN);

    let native_login = send(
        app.clone(),
        Method::POST,
        "/v1/instance/session",
        Some(&json!({"admin_password": ADMIN_PASSWORD, "client": "desktop"})),
        &[],
    )
    .await;
    assert_eq!(native_login.status(), StatusCode::OK);
    let native_login = json_body(native_login).await;
    let native_token = native_login["session_token"].as_str().unwrap().to_owned();
    assert!(!native_token.is_empty());
    assert!(native_login["csrf_token"].is_null());

    let management = send(
        app.clone(),
        Method::GET,
        "/v1/instance/management",
        None,
        &[("peppy-instance-session", &native_token)],
    )
    .await;
    assert_eq!(management.status(), StatusCode::OK);
    let management = json_body(management).await;
    assert_eq!(management["quota_bytes"], 123_456);
    assert_eq!(management["replay_retention_days"], 7);
    assert_eq!(management["devices"].as_array().unwrap().len(), 1);

    let peer: SocketAddr = "192.0.2.44:4567".parse().unwrap();
    let wrong_login = json!({"admin_password": "wrong password value", "client": "desktop"});
    for attempt in 0..10 {
        let spoofed = format!("198.51.100.{attempt}");
        let response = send_from(
            app.clone(),
            Method::POST,
            "/v1/instance/session",
            Some(&wrong_login),
            &[("x-forwarded-for", &spoofed)],
            Some(peer),
        )
        .await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }
    let throttled = send_from(
        app.clone(),
        Method::POST,
        "/v1/instance/session",
        Some(&wrong_login),
        &[("x-forwarded-for", "203.0.113.250")],
        Some(peer),
    )
    .await;
    assert_eq!(throttled.status(), StatusCode::TOO_MANY_REQUESTS);

    fixture.cleanup().await;
}

#[tokio::test]
async fn exhausted_fresh_claim_admission_rejects_a_correct_setup_token() {
    let fixture = Fixture::new().await;
    let app = fixture.community_app().await;
    let mut wrong_token = claim_request(Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4(), "desktop");
    wrong_token["setup_token"] = json!("not-the-operator-token");

    for _ in 0..10 {
        let rejected = send(
            app.clone(),
            Method::POST,
            "/v1/instance/claim",
            Some(&wrong_token),
            &[("x-forwarded-for", "198.51.100.42")],
        )
        .await;
        assert_eq!(rejected.status(), StatusCode::UNAUTHORIZED);
    }
    let correct = send(
        app,
        Method::POST,
        "/v1/instance/claim",
        Some(&claim_request(
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            "desktop",
        )),
        &[],
    )
    .await;
    assert_eq!(correct.status(), StatusCode::TOO_MANY_REQUESTS);

    fixture.cleanup().await;
}

#[tokio::test]
async fn open_community_claim_accepts_an_omitted_setup_token() {
    let fixture = Fixture::new().await;
    let app = fixture.open_community_app().await;

    let discovery = send(app.clone(), Method::GET, "/v1/instance", None, &[]).await;
    assert_eq!(json_body(discovery).await["setup_required"], false);

    let mut claim = claim_request(Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4(), "desktop");
    claim.as_object_mut().unwrap().remove("setup_token");
    let claimed = send(app, Method::POST, "/v1/instance/claim", Some(&claim), &[]).await;
    assert_eq!(claimed.status(), StatusCode::OK);

    let discovery = send(
        fixture.open_community_app().await,
        Method::GET,
        "/v1/instance",
        None,
        &[],
    )
    .await;
    assert_eq!(
        json_body(discovery).await,
        json!({
            "mode": "community",
            "claimed": true,
            "setup_required": false,
            "password_auth": true,
            "qr_pairing": true,
            "shared_secret_pairing": true,
        })
    );

    fixture.cleanup().await;
}

#[tokio::test]
async fn exact_claim_replays_after_restart_without_setup_authority() {
    let fixture = Fixture::new().await;
    let app = fixture.community_app().await;
    let operation_id = Uuid::new_v4();
    let vault_id = Uuid::new_v4();
    let device_id = Uuid::new_v4();
    let claim = claim_request(operation_id, vault_id, device_id, "desktop");

    let initial = send(app, Method::POST, "/v1/instance/claim", Some(&claim), &[]).await;
    assert_eq!(initial.status(), StatusCode::OK);
    let initial = json_body(initial).await;
    let initial_session = initial["session"]["session_token"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(initial["vault_id"], json!(vault_id));
    assert_eq!(initial["device_id"], json!(device_id));
    assert_eq!(initial["replayed"], false);
    assert!(initial.get("credential").is_none());

    let restarted = fixture.restarted_community_app().await;

    let mut wrong_password = claim.clone();
    wrong_password["admin_password"] = json!("different administrator password");
    for attempt in 0..10 {
        let spoofed = format!("198.51.100.{attempt}");
        let rejected_password = send(
            restarted.clone(),
            Method::POST,
            "/v1/instance/claim",
            Some(&wrong_password),
            &[("x-forwarded-for", &spoofed)],
        )
        .await;
        assert_eq!(rejected_password.status(), StatusCode::CONFLICT);
    }
    let throttled_password = send(
        restarted.clone(),
        Method::POST,
        "/v1/instance/claim",
        Some(&wrong_password),
        &[("x-forwarded-for", "203.0.113.99")],
    )
    .await;
    assert_eq!(throttled_password.status(), StatusCode::TOO_MANY_REQUESTS);

    let mut different_operation = claim.clone();
    different_operation["operation_id"] = json!(Uuid::new_v4());
    let rejected_operation = send(
        restarted.clone(),
        Method::POST,
        "/v1/instance/claim",
        Some(&different_operation),
        &[],
    )
    .await;
    assert_eq!(rejected_operation.status(), StatusCode::CONFLICT);

    let mut different_digest = claim.clone();
    different_digest["credential_digest"] = json!("ef".repeat(32));
    let rejected_digest = send(
        restarted.clone(),
        Method::POST,
        "/v1/instance/claim",
        Some(&different_digest),
        &[],
    )
    .await;
    assert_eq!(rejected_digest.status(), StatusCode::CONFLICT);

    let replay = send_from(
        restarted,
        Method::POST,
        "/v1/instance/claim",
        Some(&claim),
        &[],
        Some("192.0.2.55:4444".parse().unwrap()),
    )
    .await;
    assert_eq!(replay.status(), StatusCode::OK);
    let replay = json_body(replay).await;
    assert_eq!(replay["vault_id"], json!(vault_id));
    assert_eq!(replay["device_id"], json!(device_id));
    assert_eq!(replay["replayed"], true);
    assert!(replay.get("credential").is_none());
    let replay_session = replay["session"]["session_token"].as_str().unwrap();
    assert_ne!(replay_session, initial_session);

    let owner_count: i64 = sqlx::query_scalar("SELECT count(*) FROM vault_owners")
        .fetch_one(&fixture.pool)
        .await
        .unwrap();
    let device_count: i64 = sqlx::query_scalar("SELECT count(*) FROM devices")
        .fetch_one(&fixture.pool)
        .await
        .unwrap();
    assert_eq!((owner_count, device_count), (1, 1));

    fixture.cleanup().await;
}

#[tokio::test]
async fn browser_claim_login_logout_and_revoke_enforce_cookie_origin_and_csrf_contracts() {
    let fixture = Fixture::new().await;
    let app = fixture.community_app().await;
    let claim = claim_request(Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4(), "browser");

    let cross_origin_claim = send(
        app.clone(),
        Method::POST,
        "/v1/instance/claim",
        Some(&claim),
        &[("origin", "https://attacker.example")],
    )
    .await;
    assert_eq!(cross_origin_claim.status(), StatusCode::FORBIDDEN);

    let claimed = send(
        app.clone(),
        Method::POST,
        "/v1/instance/claim",
        Some(&claim),
        &[("origin", ORIGIN)],
    )
    .await;
    assert_eq!(claimed.status(), StatusCode::OK);
    let set_cookie = claimed
        .headers()
        .get("set-cookie")
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();
    assert!(set_cookie.contains("Secure"));
    assert!(set_cookie.contains("HttpOnly"));
    assert!(set_cookie.contains("SameSite=Strict"));
    let initial_cookie = session_cookie(&claimed);
    let claimed_body = json_body(claimed).await;
    assert!(claimed_body["session"].get("session_token").is_none());
    assert!(claimed_body["session"]["csrf_token"].is_string());

    let login = json!({"admin_password": ADMIN_PASSWORD, "client": "browser"});
    let cross_origin_login = send(
        app.clone(),
        Method::POST,
        "/v1/instance/session",
        Some(&login),
        &[("origin", "https://attacker.example")],
    )
    .await;
    assert_eq!(cross_origin_login.status(), StatusCode::FORBIDDEN);
    let wrong_password = send(
        app.clone(),
        Method::POST,
        "/v1/instance/session",
        Some(&json!({"admin_password": "wrong password value", "client": "browser"})),
        &[("origin", ORIGIN)],
    )
    .await;
    assert_eq!(wrong_password.status(), StatusCode::UNAUTHORIZED);

    let login = send(
        app.clone(),
        Method::POST,
        "/v1/instance/session",
        Some(&login),
        &[("origin", ORIGIN)],
    )
    .await;
    assert_eq!(login.status(), StatusCode::OK);
    let cookie = session_cookie(&login);
    let login_body = json_body(login).await;
    assert!(login_body.get("session_token").is_none());
    let csrf = login_body["csrf_token"].as_str().unwrap().to_owned();

    let authorization = CommunityEnrollmentAuthorization::with_origin(
        fixture.pool.clone(),
        Some(&Url::parse(ORIGIN).unwrap()),
    );
    let mut authorization_headers = axum::http::HeaderMap::new();
    authorization_headers.insert("cookie", cookie.parse().unwrap());
    authorization_headers.insert("x-peppy-csrf-token", csrf.parse().unwrap());
    authorization_headers.insert("origin", "https://attacker.example".parse().unwrap());
    let rejected_authorization = authorization
        .authorize_enrollment(EnrollmentAuthorizationRequest {
            headers: authorization_headers,
            method: Method::POST,
            operation: EnrollmentOperation::ManualChallenge,
            vault_id: None,
        })
        .await;
    assert_eq!(rejected_authorization, Err(StatusCode::FORBIDDEN));

    let account = send(
        app.clone(),
        Method::GET,
        "/v1/instance/account",
        None,
        &[("cookie", &cookie)],
    )
    .await;
    assert_eq!(account.status(), StatusCode::OK);

    let restored_session = send(
        app.clone(),
        Method::GET,
        "/v1/instance/session",
        None,
        &[("cookie", &cookie)],
    )
    .await;
    assert_eq!(restored_session.status(), StatusCode::OK);
    assert_eq!(restored_session.headers()["cache-control"], "no-store");
    let restored_body = json_body(restored_session).await;
    assert_eq!(restored_body["csrf_token"], csrf);
    assert!(restored_body.get("session_token").is_none());

    let raw_browser_token = cookie
        .strip_prefix("Peppy-Instance-Session=")
        .expect("session cookie contains the browser token");
    let native_session_restore = send(
        app.clone(),
        Method::GET,
        "/v1/instance/session",
        None,
        &[("peppy-instance-session", raw_browser_token)],
    )
    .await;
    assert_eq!(native_session_restore.status(), StatusCode::UNAUTHORIZED);

    let cross_origin_restore = send(
        app.clone(),
        Method::GET,
        "/v1/instance/session",
        None,
        &[("origin", "https://attacker.example"), ("cookie", &cookie)],
    )
    .await;
    assert_eq!(cross_origin_restore.status(), StatusCode::FORBIDDEN);

    let manual = json!({
        "device": {
            "device_id": Uuid::new_v4(),
            "kind": "desktop",
            "name": "Manual desktop",
            "signing_public_key": URL_SAFE_NO_PAD.encode([17_u8; 32]),
        },
        "vault_id": claim["material"]["profile"]["vault_id"],
        "credential_digest": "cd".repeat(32),
    });
    let cross_origin_manual = send(
        app.clone(),
        Method::POST,
        "/v1/enrollment/challenges",
        Some(&manual),
        &[
            ("origin", "https://attacker.example"),
            ("cookie", &cookie),
            ("x-peppy-csrf-token", &csrf),
        ],
    )
    .await;
    assert_eq!(cross_origin_manual.status(), StatusCode::FORBIDDEN);

    let owner_device = claim["device"]["device_id"].as_str().unwrap();
    let revoke_path = format!("/v1/instance/devices/{owner_device}/revoke");
    let missing_csrf = send(
        app.clone(),
        Method::POST,
        &revoke_path,
        None,
        &[("origin", ORIGIN), ("cookie", &cookie)],
    )
    .await;
    assert_eq!(missing_csrf.status(), StatusCode::FORBIDDEN);
    let cross_origin_revoke = send(
        app.clone(),
        Method::POST,
        &revoke_path,
        None,
        &[
            ("origin", "https://attacker.example"),
            ("cookie", &cookie),
            ("x-peppy-csrf-token", &csrf),
        ],
    )
    .await;
    assert_eq!(cross_origin_revoke.status(), StatusCode::FORBIDDEN);
    let reached_revoke_policy = send(
        app.clone(),
        Method::POST,
        &revoke_path,
        None,
        &[
            ("origin", ORIGIN),
            ("cookie", &cookie),
            ("x-peppy-csrf-token", &csrf),
        ],
    )
    .await;
    assert_eq!(reached_revoke_policy.status(), StatusCode::CONFLICT);

    let cross_origin_logout = send(
        app.clone(),
        Method::DELETE,
        "/v1/instance/session",
        None,
        &[
            ("origin", "https://attacker.example"),
            ("cookie", &cookie),
            ("x-peppy-csrf-token", &csrf),
        ],
    )
    .await;
    assert_eq!(cross_origin_logout.status(), StatusCode::FORBIDDEN);
    let logout = send(
        app.clone(),
        Method::DELETE,
        "/v1/instance/session",
        None,
        &[
            ("origin", ORIGIN),
            ("cookie", &cookie),
            ("x-peppy-csrf-token", &csrf),
        ],
    )
    .await;
    assert_eq!(logout.status(), StatusCode::NO_CONTENT);
    let cleared_cookie = logout
        .headers()
        .get("set-cookie")
        .expect("browser logout must expire its session cookie")
        .to_str()
        .unwrap();
    assert!(cleared_cookie.contains("Max-Age=0"));
    assert!(cleared_cookie.contains("Secure"));
    assert!(cleared_cookie.contains("HttpOnly"));
    assert!(cleared_cookie.contains("SameSite=Strict"));
    let revoked = send(
        app.clone(),
        Method::GET,
        "/v1/instance/account",
        None,
        &[("cookie", &cookie)],
    )
    .await;
    assert_eq!(revoked.status(), StatusCode::UNAUTHORIZED);

    let initial_session_still_works = send(
        app,
        Method::GET,
        "/v1/instance/account",
        None,
        &[("cookie", &initial_cookie)],
    )
    .await;
    assert_eq!(initial_session_still_works.status(), StatusCode::OK);

    fixture.cleanup().await;
}

#[tokio::test]
async fn hosted_router_does_not_mount_community_instance_routes() {
    let fixture = Fixture::new().await;
    let app = fixture.hosted_app().await;
    for (method, path) in [
        (Method::GET, "/v1/instance"),
        (Method::POST, "/v1/instance/claim"),
        (Method::GET, "/v1/instance/session"),
        (Method::POST, "/v1/instance/session"),
        (Method::GET, "/v1/instance/vaults"),
        (Method::POST, "/v1/instance/invites"),
        (Method::GET, "/v1/instance/vault-requests"),
        (Method::POST, "/v1/instance/vault-requests"),
        (
            Method::GET,
            "/v1/instance/vault-requests/00000000-0000-0000-0000-000000000000",
        ),
        (
            Method::POST,
            "/v1/instance/vault-requests/00000000-0000-0000-0000-000000000000/approve",
        ),
        (
            Method::POST,
            "/v1/instance/vault-requests/00000000-0000-0000-0000-000000000000/deny",
        ),
        (Method::POST, "/v1/instance/vaults"),
        (Method::GET, "/v1/instance/events"),
    ] {
        let response = send(app.clone(), method, path, None, &[]).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
    }
    fixture.cleanup().await;
}
