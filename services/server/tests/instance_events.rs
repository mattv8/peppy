use std::{
    net::SocketAddr,
    time::{Duration, Instant},
};

use axum::{
    Router,
    body::Body,
    http::{Method, Request, Response, StatusCode},
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use http_body_util::BodyExt;
use peppy_crypto::KeyProfile;
use peppy_server::{ServerBuilder, config::Config};
use serde_json::{Value, json};
use sqlx::{PgPool, postgres::PgPoolOptions};
use tokio::{net::TcpListener, sync::oneshot, task::JoinHandle, time::timeout};
use tower::ServiceExt;
use url::Url;
use uuid::Uuid;

const SETUP_TOKEN: &str = "instance-events-setup-token-at-least-32-bytes";
const ADMIN_PASSWORD: &str = "correct horse battery staple";
const REQUEST_SECRET: &str = "request-secret";

struct Fixture {
    admin: PgPool,
    pool: PgPool,
    schema: String,
}

impl Fixture {
    async fn new() -> Self {
        let database_url =
            std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL is required");
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .unwrap();
        let schema = format!("peppy_instance_events_{}", Uuid::new_v4().simple());
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
            schema,
        }
    }

    async fn app(&self) -> Router {
        ServerBuilder::community(
            Config {
                bind_addr: "127.0.0.1:0".parse().unwrap(),
                database_url: std::env::var("TEST_DATABASE_URL").unwrap(),
                release_identity: "instance-events-test".into(),
                s3: None,
                public_api_url: Some(Url::parse("http://localhost:3000").unwrap()),
                public_attachment_url: None,
                vault_attachment_quota_bytes: 123_456,
                trusted_proxy_cidrs: vec![],
                replay_retention: Duration::from_secs(604_800),
                relay_url: None,
                web_client: None,
            },
            self.pool.clone(),
        )
        .setup_token(SETUP_TOKEN)
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

async fn send(
    app: Router,
    method: Method,
    path: &str,
    body: Option<&Value>,
    headers: &[(&str, &str)],
) -> Response<Body> {
    let mut request = Request::builder().method(method).uri(path);
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    if body.is_some() {
        request = request.header("content-type", "application/json");
    }
    app.oneshot(
        request
            .body(body.map_or_else(Body::empty, |value| Body::from(value.to_string())))
            .unwrap(),
    )
    .await
    .unwrap()
}

async fn json_body(response: Response<Body>) -> Value {
    serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap()
}

fn device() -> Value {
    json!({"device_id": Uuid::new_v4(), "kind": "desktop", "name": "Primary device", "signing_public_key": URL_SAFE_NO_PAD.encode([7_u8; 32])})
}

fn material(vault_id: Uuid) -> Value {
    let profile = KeyProfile::new(vault_id, 1).unwrap();
    json!({"profile": profile, "check_header": URL_SAFE_NO_PAD.encode([9_u8; 48]), "fingerprint": profile.fingerprint().unwrap(), "key_epoch": 1, "admission_public_key": URL_SAFE_NO_PAD.encode([11_u8; 32])})
}

async fn claim(app: Router) -> String {
    let response = send(app, Method::POST, "/v1/instance/claim", Some(&json!({
        "operation_id": Uuid::new_v4(), "setup_token": SETUP_TOKEN, "username": "admin", "admin_password": ADMIN_PASSWORD,
        "device": device(), "material": material(Uuid::new_v4()), "credential_digest": "ab".repeat(32),
    })), &[]).await;
    assert_eq!(response.status(), StatusCode::OK);
    json_body(response).await["session"]["session_token"]
        .as_str()
        .unwrap()
        .to_owned()
}

async fn request(app: Router, admin: &str, secret: &str) -> Uuid {
    let invitation = send(
        app.clone(),
        Method::POST,
        "/v1/instance/invites",
        None,
        &[("peppy-instance-session", admin)],
    )
    .await;
    assert_eq!(invitation.status(), StatusCode::OK);
    let token = json_body(invitation).await["invite_token"]
        .as_str()
        .unwrap()
        .to_owned();
    let request_id = Uuid::new_v4();
    let submitted = send(app, Method::POST, "/v1/instance/vault-requests", Some(&json!({
        "request_id": request_id, "invite_token": token, "display_name": "New owner", "client": "desktop",
        "request_secret_digest": hex::encode(peppy_server::instance::digest(secret.as_bytes())),
    })), &[]).await;
    assert_eq!(submitted.status(), StatusCode::OK);
    request_id
}

async fn spawn(
    app: Router,
) -> (
    SocketAddr,
    oneshot::Sender<()>,
    JoinHandle<std::io::Result<()>>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (shutdown, receiver) = oneshot::channel();
    let server = tokio::spawn(peppy_server::serve(listener, app, async move {
        let _ = receiver.await;
    }));
    (address, shutdown, server)
}

fn url(address: SocketAddr, path: &str) -> String {
    format!("http://{address}{path}")
}

async fn sse(client: &reqwest::Client, address: SocketAddr, admin: &str) -> reqwest::Response {
    timeout(
        Duration::from_secs(2),
        client
            .get(url(address, "/v1/instance/events"))
            .header("peppy-instance-session", admin)
            .send(),
    )
    .await
    .unwrap()
    .unwrap()
}

async fn next_sse(response: &mut reqwest::Response) -> String {
    let chunk = timeout(Duration::from_secs(7), response.chunk())
        .await
        .unwrap()
        .unwrap()
        .expect("SSE stream ended unexpectedly");
    String::from_utf8_lossy(&chunk).into_owned()
}

async fn wait_for_sse_request(response: &mut reqwest::Response, request_id: Uuid) {
    const WAIT: Duration = Duration::from_secs(7);
    let target = request_id.to_string();
    let mut body = Vec::new();
    let outcome = timeout(WAIT, async {
        loop {
            let chunk = response
                .chunk()
                .await
                .expect("SSE transport error while waiting for request event")
                .expect("SSE stream ended before request event");
            body.extend_from_slice(&chunk);
            if String::from_utf8_lossy(&body).contains(&target) {
                break;
            }
        }
    })
    .await;
    if outcome.is_err() {
        panic!(
            "timed out after {WAIT:?} waiting for SSE event containing request {target}; received {} bytes",
            body.len()
        );
    }
}

#[tokio::test]
async fn sse_snapshots_enforce_session_cap_and_release_permits() {
    let fixture = Fixture::new().await;
    let app = fixture.app().await;
    let admin = claim(app.clone()).await;
    let (address, shutdown, server) = spawn(app.clone()).await;
    let client = reqwest::Client::new();
    let mut streams = Vec::new();
    for _ in 0..4 {
        let mut stream = sse(&client, address, &admin).await;
        assert_eq!(stream.status(), StatusCode::OK);
        assert!(next_sse(&mut stream).await.contains("snapshot"));
        streams.push(stream);
    }
    let limited = sse(&client, address, &admin).await;
    assert_eq!(limited.status(), StatusCode::TOO_MANY_REQUESTS);
    let request_id = request(app, &admin, REQUEST_SECRET).await;
    wait_for_sse_request(&mut streams[0], request_id).await;
    drop(streams);
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(sse(&client, address, &admin).await.status(), StatusCode::OK);
    shutdown.send(()).unwrap();
    timeout(Duration::from_secs(2), server)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    fixture.cleanup().await;
}

#[tokio::test]
async fn sse_rechecks_revoked_and_expired_sessions() {
    let fixture = Fixture::new().await;
    let app = fixture.app().await;
    let admin = claim(app.clone()).await;
    let (address, shutdown, server) = spawn(app).await;
    let client = reqwest::Client::new();
    for column in ["revoked_at=now()", "expires_at=now()-interval '1 second'"] {
        let mut stream = sse(&client, address, &admin).await;
        assert_eq!(stream.status(), StatusCode::OK);
        next_sse(&mut stream).await;
        sqlx::query(&format!(
            "UPDATE instance_sessions SET {column} WHERE token_digest=$1"
        ))
        .bind(peppy_server::instance::digest(admin.as_bytes()).as_slice())
        .execute(&fixture.pool)
        .await
        .unwrap();
        assert!(
            timeout(Duration::from_secs(7), stream.chunk())
                .await
                .unwrap()
                .unwrap()
                .is_none()
        );
        if column.starts_with("revoked") {
            sqlx::query("UPDATE instance_sessions SET revoked_at=NULL, expires_at=now()+interval '1 hour' WHERE token_digest=$1")
                .bind(peppy_server::instance::digest(admin.as_bytes()).as_slice()).execute(&fixture.pool).await.unwrap();
        }
    }
    shutdown.send(()).unwrap();
    timeout(Duration::from_secs(2), server)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    fixture.cleanup().await;
}

#[tokio::test]
async fn sse_closes_when_the_streaming_owner_is_demoted() {
    let fixture = Fixture::new().await;
    let app = fixture.app().await;
    let admin = claim(app.clone()).await;
    let (address, shutdown, server) = spawn(app).await;
    let client = reqwest::Client::new();
    let mut stream = sse(&client, address, &admin).await;
    assert_eq!(stream.status(), StatusCode::OK);
    next_sse(&mut stream).await;
    sqlx::query("UPDATE vault_owners SET is_admin=FALSE WHERE is_admin")
        .execute(&fixture.pool)
        .await
        .unwrap();
    assert!(
        timeout(Duration::from_secs(7), stream.chunk())
            .await
            .unwrap()
            .unwrap()
            .is_none()
    );
    shutdown.send(()).unwrap();
    timeout(Duration::from_secs(2), server)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    fixture.cleanup().await;
}

#[tokio::test]
async fn longpoll_observes_immediate_local_remote_and_bounded_wait_states() {
    let fixture = Fixture::new().await;
    let app_a = fixture.app().await;
    let app_b = fixture.app().await;
    let admin = claim(app_a.clone()).await;
    let (address_a, shutdown_a, server_a) = spawn(app_a.clone()).await;
    let (address_b, shutdown_b, server_b) = spawn(app_b).await;
    let client = reqwest::Client::new();
    let request_id = request(app_a.clone(), &admin, "first-request-secret").await;
    let status_url = move |wait| {
        url(
            address_a,
            &format!("/v1/instance/vault-requests/{request_id}?wait={wait}"),
        )
    };
    let immediate = client
        .get(status_url(15))
        .header("peppy-vault-request", "first-request-secret")
        .header("peppy-known-state", "approved")
        .send()
        .await
        .unwrap();
    assert_eq!(immediate.json::<Value>().await.unwrap()["state"], "pending");
    let poll = tokio::spawn({
        let client = client.clone();
        async move {
            client
                .get(status_url(15))
                .header("peppy-vault-request", "first-request-secret")
                .header("peppy-known-state", "pending")
                .send()
                .await
                .unwrap()
        }
    });
    tokio::time::sleep(Duration::from_millis(80)).await;
    assert_eq!(
        client
            .post(url(
                address_a,
                &format!("/v1/instance/vault-requests/{request_id}/approve")
            ))
            .header("peppy-instance-session", &admin)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        timeout(Duration::from_secs(2), poll)
            .await
            .unwrap()
            .unwrap()
            .json::<Value>()
            .await
            .unwrap()["state"],
        "approved"
    );

    let second_id = request(app_a.clone(), &admin, "second-request-secret").await;
    let remote_url = url(
        address_a,
        &format!("/v1/instance/vault-requests/{second_id}?wait=15"),
    );
    let remote_poll = tokio::spawn({
        let client = client.clone();
        async move {
            client
                .get(remote_url)
                .header("peppy-vault-request", "second-request-secret")
                .header("peppy-known-state", "pending")
                .send()
                .await
                .unwrap()
        }
    });
    tokio::time::sleep(Duration::from_millis(80)).await;
    assert_eq!(
        client
            .post(url(
                address_b,
                &format!("/v1/instance/vault-requests/{second_id}/approve")
            ))
            .header("peppy-instance-session", &admin)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        timeout(Duration::from_secs(6), remote_poll)
            .await
            .unwrap()
            .unwrap()
            .json::<Value>()
            .await
            .unwrap()["state"],
        "approved"
    );

    let third_id = request(app_a, &admin, "third-request-secret").await;
    let started = Instant::now();
    let waiting = client
        .get(url(
            address_a,
            &format!("/v1/instance/vault-requests/{third_id}?wait=1"),
        ))
        .header("peppy-vault-request", "third-request-secret")
        .header("peppy-known-state", "pending")
        .send();
    let unrelated = client
        .post(url(address_a, "/v1/instance/invites"))
        .header("peppy-instance-session", &admin)
        .send();
    let (waited, unrelated) = tokio::join!(waiting, unrelated);
    assert_eq!(unrelated.unwrap().status(), StatusCode::OK);
    assert_eq!(waited.unwrap().status(), StatusCode::OK);
    assert!(started.elapsed() >= Duration::from_millis(850));
    shutdown_a.send(()).unwrap();
    shutdown_b.send(()).unwrap();
    timeout(Duration::from_secs(2), server_a)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    timeout(Duration::from_secs(2), server_b)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    fixture.cleanup().await;
}

#[tokio::test]
async fn shutdown_ends_only_its_own_sse_stream() {
    let fixture = Fixture::new().await;
    let app_a = fixture.app().await;
    let app_b = fixture.app().await;
    let admin = claim(app_a).await;
    let (address_a, shutdown_a, server_a) = spawn(fixture.app().await).await;
    let (address_b, shutdown_b, server_b) = spawn(app_b).await;
    let client = reqwest::Client::new();
    let mut first = sse(&client, address_a, &admin).await;
    let mut second = sse(&client, address_b, &admin).await;
    next_sse(&mut first).await;
    next_sse(&mut second).await;
    shutdown_a.send(()).unwrap();
    assert!(
        timeout(Duration::from_secs(2), first.chunk())
            .await
            .unwrap()
            .unwrap()
            .is_none()
    );
    timeout(Duration::from_secs(2), server_a)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let request_id = request(fixture.app().await, &admin, REQUEST_SECRET).await;
    wait_for_sse_request(&mut second, request_id).await;
    shutdown_b.send(()).unwrap();
    timeout(Duration::from_secs(2), server_b)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    fixture.cleanup().await;
}
