use std::{net::SocketAddr, time::Duration};

use axum::{
    Router,
    body::Body,
    http::{Method, Request, Response, StatusCode},
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use http_body_util::BodyExt;
use peppy_crypto::KeyProfile;
use peppy_hosted_api::enrollment::VAULT_REQUEST_HEADER;
use peppy_server::{ServerBuilder, config::Config};
use serde_json::{Value, json};
use sqlx::{PgPool, postgres::PgPoolOptions};
use tower::ServiceExt;
use url::Url;
use uuid::Uuid;

const SETUP_TOKEN: &str = "vault-request-http-setup-token-at-least-32-bytes";
const ADMIN_PASSWORD: &str = "correct horse battery staple";

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
        let schema = format!("peppy_vault_requests_{}", Uuid::new_v4().simple());
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

    async fn server(&self) -> peppy_server::Server {
        let database_url = std::env::var("TEST_DATABASE_URL").unwrap();
        ServerBuilder::community(
            Config {
                bind_addr: "127.0.0.1:0".parse::<SocketAddr>().unwrap(),
                database_url,
                release_identity: "vault-requests-http-test".into(),
                s3: None,
                public_api_url: Some(Url::parse("http://localhost:3000").unwrap()),
                public_attachment_url: None,
                vault_attachment_quota_bytes: 123_456,
                trusted_proxy_cidrs: vec![],
                replay_retention: Duration::from_secs(7 * 86_400),
                relay_url: None,
                web_client: None,
            },
            self.pool.clone(),
        )
        .setup_token(SETUP_TOKEN)
        .build()
        .await
        .unwrap()
    }

    async fn app(&self) -> Router {
        self.server().await.router()
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

async fn body(response: Response<Body>) -> Value {
    serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap()
}

fn material(vault_id: Uuid) -> Value {
    let profile = KeyProfile::new(vault_id, 1).unwrap();
    let unique = vault_id.as_bytes();
    let mut check_header = [9_u8; 48];
    check_header[..16].copy_from_slice(unique);
    let mut admission_key = [11_u8; 32];
    admission_key[..16].copy_from_slice(unique);
    json!({"profile": profile, "check_header": URL_SAFE_NO_PAD.encode(check_header), "fingerprint": profile.fingerprint().unwrap(), "key_epoch": 1, "admission_public_key": URL_SAFE_NO_PAD.encode(admission_key)})
}

fn device() -> Value {
    let device_id = Uuid::new_v4();
    let mut signing_key = [7_u8; 32];
    signing_key[..16].copy_from_slice(device_id.as_bytes());
    json!({"device_id": device_id, "kind": "desktop", "name": "Primary device", "signing_public_key": URL_SAFE_NO_PAD.encode(signing_key)})
}

async fn claim(app: Router) -> String {
    let claimed = send(app, Method::POST, "/v1/instance/claim", Some(&json!({
        "operation_id": Uuid::new_v4(), "setup_token": SETUP_TOKEN, "username": "admin",
        "admin_password": ADMIN_PASSWORD, "device": device(), "material": material(Uuid::new_v4()),
        "credential_digest": "ab".repeat(32),
    })), &[]).await;
    assert_eq!(claimed.status(), StatusCode::OK);
    body(claimed).await["session"]["session_token"]
        .as_str()
        .unwrap()
        .to_owned()
}

async fn invite(app: Router, admin: &str) -> Value {
    let response = send(
        app,
        Method::POST,
        "/v1/instance/invites",
        None,
        &[("peppy-instance-session", admin)],
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    body(response).await
}

async fn submit(app: Router, token: &str, request_id: Uuid, secret: &str) -> Response<Body> {
    send(app, Method::POST, "/v1/instance/vault-requests", Some(&json!({
        "request_id": request_id, "invite_token": token, "display_name": "New owner",
        "client": "desktop", "request_secret_digest": hex::encode(peppy_server::instance::digest(secret.as_bytes())),
    })), &[]).await
}

fn create_request(request_id: Uuid, secret: &str, username: &str, operation_id: Uuid) -> Value {
    let credential_digest = format!(
        "{:032x}{:032x}",
        operation_id.as_u128(),
        operation_id.as_u128()
    );
    json!({
        "request_id": request_id, "request_secret": secret, "operation_id": operation_id,
        "username": username, "admin_password": "another correct horse battery staple",
        "device": device(), "material": material(Uuid::new_v4()), "credential_digest": credential_digest,
    })
}

async fn approved_request(app: Router, admin: &str, username: &str) -> (Uuid, String) {
    let invitation = invite(app.clone(), admin).await;
    let request_id = Uuid::new_v4();
    let secret = format!("secret-{username}");
    assert_eq!(
        submit(
            app.clone(),
            invitation["invite_token"].as_str().unwrap(),
            request_id,
            &secret
        )
        .await
        .status(),
        StatusCode::OK
    );
    assert_eq!(
        send(
            app,
            Method::POST,
            &format!("/v1/instance/vault-requests/{request_id}/approve"),
            None,
            &[("peppy-instance-session", admin)]
        )
        .await
        .status(),
        StatusCode::NO_CONTENT
    );
    (request_id, secret)
}

#[tokio::test]
async fn invite_quota_is_locked_across_concurrent_creators_and_expired_invites_reject_submission() {
    let fixture = Fixture::new().await;
    let app = fixture.app().await;
    let admin = claim(app.clone()).await;
    let mut creates = tokio::task::JoinSet::new();
    for _ in 0..10 {
        let app = app.clone();
        let admin = admin.clone();
        creates.spawn(async move {
            send(
                app,
                Method::POST,
                "/v1/instance/invites",
                None,
                &[("peppy-instance-session", &admin)],
            )
            .await
            .status()
        });
    }
    let mut created = 0;
    while let Some(status) = creates.join_next().await {
        match status.unwrap() {
            StatusCode::OK => created += 1,
            StatusCode::CONFLICT => {}
            status => panic!("unexpected invite response: {status}"),
        }
    }
    assert_eq!(created, 5);
    // Submission authenticates the plaintext token, so create a dedicated expired invitation
    // through the API before changing only its expiry timestamp.
    sqlx::query("DELETE FROM vault_invites")
        .execute(&fixture.pool)
        .await
        .unwrap();
    let expired = invite(app.clone(), &admin).await;
    sqlx::query("UPDATE vault_invites SET expires_at=now()-interval '1 second'")
        .execute(&fixture.pool)
        .await
        .unwrap();
    assert_eq!(
        submit(
            app,
            expired["invite_token"].as_str().unwrap(),
            Uuid::new_v4(),
            "expired-request-secret",
        )
        .await
        .status(),
        StatusCode::NOT_FOUND
    );
    fixture.cleanup().await;
}

#[tokio::test]
async fn empty_login_selects_the_admin_and_unknown_logins_match_wrong_password() {
    let fixture = Fixture::new().await;
    let app = fixture.app().await;
    let admin = claim(app.clone()).await;
    sqlx::query("UPDATE vault_owners SET username='alice' WHERE is_admin")
        .execute(&fixture.pool)
        .await
        .unwrap();
    let empty = send(
        app.clone(),
        Method::POST,
        "/v1/instance/session",
        Some(&json!({"username": "", "admin_password": ADMIN_PASSWORD, "client": "desktop"})),
        &[],
    )
    .await;
    assert_eq!(empty.status(), StatusCode::OK);
    let unknown = send(
        app.clone(),
        Method::POST,
        "/v1/instance/session",
        Some(&json!({"username": "nobody", "admin_password": ADMIN_PASSWORD, "client": "desktop"})),
        &[],
    )
    .await;
    let wrong = send(
        app,
        Method::POST,
        "/v1/instance/session",
        Some(&json!({"username": "alice", "admin_password": "wrong password that is long enough", "client": "desktop"})),
        &[],
    )
    .await;
    assert_eq!(unknown.status(), wrong.status());
    assert_eq!(body(unknown).await, body(wrong).await);
    drop(admin);
    fixture.cleanup().await;
}

#[tokio::test]
async fn create_vault_rejects_an_empty_username() {
    let fixture = Fixture::new().await;
    let app = fixture.app().await;
    let admin = claim(app.clone()).await;
    let (request_id, secret) = approved_request(app.clone(), &admin, "empty-name").await;
    let response = send(
        app,
        Method::POST,
        "/v1/instance/vaults",
        Some(&create_request(request_id, &secret, "   ", Uuid::new_v4())),
        &[],
    )
    .await;
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body(response).await["code"], "invalid_username");
    fixture.cleanup().await;
}

#[tokio::test]
async fn instance_maintenance_removes_terminal_records_after_thirty_days() {
    let fixture = Fixture::new().await;
    let app = fixture.app().await;
    let admin = claim(app).await;
    let owner: Uuid = sqlx::query_scalar("SELECT vault_id FROM vault_owners WHERE is_admin")
        .fetch_one(&fixture.pool)
        .await
        .unwrap();
    let invite_id = Uuid::new_v4();
    let request_id = Uuid::new_v4();
    sqlx::query("INSERT INTO vault_invites(id, token_digest, created_by, state, expires_at, created_at) VALUES($1,$2,$3,'used',now()-interval '31 days',now()-interval '31 days')")
        .bind(invite_id)
        .bind([1_u8; 32].as_slice())
        .bind(owner)
        .execute(&fixture.pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO vault_requests(id, invite_id, display_name, client_kind, secret_digest, state, expires_at, created_at) VALUES($1,$2,'Old request','desktop',$3,'denied',now()-interval '31 days',now()-interval '31 days')")
        .bind(request_id)
        .bind(invite_id)
        .bind([2_u8; 32].as_slice())
        .execute(&fixture.pool)
        .await
        .unwrap();
    // The first maintenance interval tick is immediate once the assembled server starts.
    let mut server = fixture.server().await;
    server.start_maintenance().unwrap();
    // Maintenance deletes the request and invite in separate statements, so wait until both
    // target rows are gone instead of assuming a fixed delay covers the whole sweep.
    let mut remaining = (true, true);
    let swept = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            remaining = sqlx::query_as::<_, (bool, bool)>(
                "SELECT EXISTS(SELECT 1 FROM vault_requests WHERE id=$1), EXISTS(SELECT 1 FROM vault_invites WHERE id=$2)",
            )
            .bind(request_id)
            .bind(invite_id)
            .fetch_one(&fixture.pool)
            .await
            .expect("query maintenance target rows");
            if remaining == (false, false) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    assert!(
        swept.is_ok(),
        "maintenance did not remove both target rows within 5s: request_present={}, invite_present={}",
        remaining.0,
        remaining.1
    );
    drop(admin);
    fixture.cleanup().await;
}

#[tokio::test]
async fn claim_invite_submit_approve_and_create_a_separate_owner_vault() {
    let fixture = Fixture::new().await;
    let app = fixture.app().await;
    let admin = claim(app.clone()).await;
    let invitation = invite(app.clone(), &admin).await;
    let request_id = Uuid::new_v4();
    let secret = "request-secret";
    let submitted = submit(
        app.clone(),
        invitation["invite_token"].as_str().unwrap(),
        request_id,
        secret,
    )
    .await;
    assert_eq!(submitted.status(), StatusCode::OK);
    let approved = send(
        app.clone(),
        Method::POST,
        &format!("/v1/instance/vault-requests/{request_id}/approve"),
        None,
        &[("peppy-instance-session", &admin)],
    )
    .await;
    assert_eq!(approved.status(), StatusCode::NO_CONTENT);

    let create = json!({"request_id": request_id, "request_secret": secret, "operation_id": Uuid::new_v4(), "username": "second", "admin_password": "another correct horse battery staple", "device": device(), "material": material(Uuid::new_v4()), "credential_digest": "cd".repeat(32)});
    let created = send(
        app.clone(),
        Method::POST,
        "/v1/instance/vaults",
        Some(&create),
        &[],
    )
    .await;
    assert_eq!(created.status(), StatusCode::OK);
    let owner_session = body(created).await["session"]["session_token"]
        .as_str()
        .unwrap()
        .to_owned();
    let owner_management = send(
        app.clone(),
        Method::GET,
        "/v1/instance/management",
        None,
        &[("peppy-instance-session", &owner_session)],
    )
    .await;
    assert_eq!(owner_management.status(), StatusCode::OK);
    let forbidden = send(
        app.clone(),
        Method::GET,
        "/v1/instance/vault-requests",
        None,
        &[("peppy-instance-session", &owner_session)],
    )
    .await;
    assert_eq!(forbidden.status(), StatusCode::FORBIDDEN);

    for (username, password, expected_admin) in [
        ("admin", ADMIN_PASSWORD, true),
        ("second", "another correct horse battery staple", false),
    ] {
        let login = send(
            app.clone(),
            Method::POST,
            "/v1/instance/session",
            Some(&json!({"username": username, "admin_password": password, "client": "desktop"})),
            &[],
        )
        .await;
        assert_eq!(login.status(), StatusCode::OK);
        let session = body(login).await["session_token"]
            .as_str()
            .unwrap()
            .to_owned();
        let account = send(
            app.clone(),
            Method::GET,
            "/v1/instance/management",
            None,
            &[("peppy-instance-session", &session)],
        )
        .await;
        assert_eq!(body(account).await["is_admin"], expected_admin);
    }
    fixture.cleanup().await;
}

#[tokio::test]
async fn submit_replays_exactly_and_rejects_wrong_invite_or_secret() {
    let fixture = Fixture::new().await;
    let app = fixture.app().await;
    let admin = claim(app.clone()).await;
    let invitation = invite(app.clone(), &admin).await;
    let token = invitation["invite_token"].as_str().unwrap();
    let id = Uuid::new_v4();
    let first = submit(app.clone(), token, id, "secret").await;
    assert_eq!(first.status(), StatusCode::OK);
    let replay = submit(app.clone(), token, id, "secret").await;
    assert_eq!(replay.status(), StatusCode::OK);
    assert_eq!(body(first).await, body(replay).await);
    assert_eq!(
        submit(app.clone(), token, id, "wrong").await.status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        submit(app.clone(), "not-an-invite", Uuid::new_v4(), "secret")
            .await
            .status(),
        StatusCode::NOT_FOUND
    );
    fixture.cleanup().await;
}

#[tokio::test]
async fn revoke_denies_an_approved_request_and_keeps_it_uncreatable() {
    let fixture = Fixture::new().await;
    let app = fixture.app().await;
    let admin = claim(app.clone()).await;
    let invitation = invite(app.clone(), &admin).await;
    let request_id = Uuid::new_v4();
    submit(
        app.clone(),
        invitation["invite_token"].as_str().unwrap(),
        request_id,
        "secret",
    )
    .await;
    assert_eq!(
        send(
            app.clone(),
            Method::POST,
            &format!("/v1/instance/vault-requests/{request_id}/approve"),
            None,
            &[("peppy-instance-session", &admin)]
        )
        .await
        .status(),
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        send(
            app.clone(),
            Method::DELETE,
            &format!(
                "/v1/instance/invites/{}",
                invitation["invite_id"].as_str().unwrap()
            ),
            None,
            &[("peppy-instance-session", &admin)]
        )
        .await
        .status(),
        StatusCode::NO_CONTENT
    );
    let status = send(
        app.clone(),
        Method::GET,
        &format!("/v1/instance/vault-requests/{request_id}"),
        None,
        &[(VAULT_REQUEST_HEADER, "secret")],
    )
    .await;
    assert_eq!(status.status(), StatusCode::OK);
    assert_eq!(body(status).await["state"], "denied");
    assert_eq!(
        send(
            app.clone(),
            Method::POST,
            "/v1/instance/vaults",
            Some(&create_request(
                request_id,
                "secret",
                "revoked",
                Uuid::new_v4()
            )),
            &[]
        )
        .await
        .status(),
        StatusCode::CONFLICT
    );
    fixture.cleanup().await;
}

#[tokio::test]
async fn concurrent_create_replays_and_username_conflict_keeps_request_approved() {
    let fixture = Fixture::new().await;
    let app = fixture.app().await;
    let admin = claim(app.clone()).await;
    let (request_id, secret) = approved_request(app.clone(), &admin, "concurrent").await;
    let create = create_request(request_id, &secret, "concurrent", Uuid::new_v4());
    let (first, second) = tokio::join!(
        send(
            app.clone(),
            Method::POST,
            "/v1/instance/vaults",
            Some(&create),
            &[]
        ),
        send(
            app.clone(),
            Method::POST,
            "/v1/instance/vaults",
            Some(&create),
            &[]
        )
    );
    assert_eq!(first.status(), StatusCode::OK);
    assert_eq!(second.status(), StatusCode::OK);
    let replayed = [
        body(first).await["replayed"].as_bool().unwrap(),
        body(second).await["replayed"].as_bool().unwrap(),
    ];
    assert_eq!(replayed.iter().filter(|value| **value).count(), 1);

    let (conflict_id, conflict_secret) = approved_request(app.clone(), &admin, "admin").await;
    let conflict = send(
        app.clone(),
        Method::POST,
        "/v1/instance/vaults",
        Some(&create_request(
            conflict_id,
            &conflict_secret,
            "admin",
            Uuid::new_v4(),
        )),
        &[],
    )
    .await;
    let conflict_status = conflict.status();
    let conflict_body = body(conflict).await;
    assert_eq!(conflict_status, StatusCode::CONFLICT, "{conflict_body}");
    assert_eq!(conflict_body["code"], "username_taken");
    let status = send(
        app.clone(),
        Method::GET,
        &format!("/v1/instance/vault-requests/{conflict_id}"),
        None,
        &[(VAULT_REQUEST_HEADER, &conflict_secret)],
    )
    .await;
    assert_eq!(status.status(), StatusCode::OK);
    assert_eq!(body(status).await["state"], "approved");
    fixture.cleanup().await;
}

#[tokio::test]
async fn denied_expired_and_wrong_secret_requests_cannot_create_vaults() {
    let fixture = Fixture::new().await;
    let app = fixture.app().await;
    let admin = claim(app.clone()).await;
    let (denied_id, denied_secret) = approved_request(app.clone(), &admin, "denied").await;
    assert_eq!(
        send(
            app.clone(),
            Method::POST,
            &format!("/v1/instance/vault-requests/{denied_id}/deny"),
            None,
            &[("peppy-instance-session", &admin)]
        )
        .await
        .status(),
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        send(
            app.clone(),
            Method::POST,
            "/v1/instance/vaults",
            Some(&create_request(
                denied_id,
                &denied_secret,
                "denied",
                Uuid::new_v4()
            )),
            &[]
        )
        .await
        .status(),
        StatusCode::CONFLICT
    );

    let invitation = invite(app.clone(), &admin).await;
    let expired_id = Uuid::new_v4();
    assert_eq!(
        submit(
            app.clone(),
            invitation["invite_token"].as_str().unwrap(),
            expired_id,
            "expired-secret"
        )
        .await
        .status(),
        StatusCode::OK
    );
    sqlx::query("UPDATE vault_requests SET expires_at=now()-interval '1 second' WHERE id=$1")
        .bind(expired_id)
        .execute(&fixture.pool)
        .await
        .unwrap();
    assert_eq!(
        send(
            app.clone(),
            Method::POST,
            &format!("/v1/instance/vault-requests/{expired_id}/approve"),
            None,
            &[("peppy-instance-session", &admin)]
        )
        .await
        .status(),
        StatusCode::CONFLICT
    );
    assert_eq!(
        send(
            app.clone(),
            Method::POST,
            "/v1/instance/vaults",
            Some(&create_request(
                expired_id,
                "wrong",
                "expired",
                Uuid::new_v4()
            )),
            &[]
        )
        .await
        .status(),
        StatusCode::NOT_FOUND
    );
    fixture.cleanup().await;
}
