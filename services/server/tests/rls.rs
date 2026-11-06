use std::{net::SocketAddr, time::Duration};

use axum::{
    Router,
    body::Body,
    http::{Method, Request, Response, StatusCode},
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use http_body_util::BodyExt;
use libsodium_rs::crypto_sign;
use peppy_crypto::KeyProfile;
use peppy_server::{ServerBuilder, config::Config, rls};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{PgPool, postgres::PgPoolOptions};
use tower::ServiceExt;
use url::Url;
use uuid::Uuid;

const SETUP_TOKEN: &str = "rls-integration-setup-token-at-least-32-bytes";
const ADMIN_PASSWORD: &str = "correct horse battery staple";

struct Fixture {
    admin_url: String,
    database: String,
    migration_role: String,
    runtime_role: String,
    maintenance_role: String,
    migration: PgPool,
    runtime: PgPool,
    maintenance: PgPool,
    runtime_url: String,
}

impl Fixture {
    async fn new() -> Self {
        let admin_url = std::env::var("TEST_DATABASE_URL")
            .expect("TEST_DATABASE_URL must identify the disposable PostgreSQL fixture");
        let suffix = Uuid::new_v4().simple().to_string();
        let database = format!("rls_{suffix}");
        let migration_role = format!("rls_m_{suffix}");
        let runtime_role = format!("rls_r_{suffix}");
        let maintenance_role = format!("rls_x_{suffix}");
        let migration_password = format!("m_{suffix}");
        let runtime_password = format!("r_{suffix}");
        let maintenance_password = format!("x_{suffix}");
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&admin_url)
            .await
            .unwrap();

        sqlx::raw_sql(&format!(
            "CREATE ROLE {migration_role} LOGIN PASSWORD '{migration_password}' NOSUPERUSER NOBYPASSRLS NOCREATEROLE; \
             CREATE ROLE {runtime_role} LOGIN PASSWORD '{runtime_password}' NOSUPERUSER NOBYPASSRLS NOCREATEROLE; \
             CREATE ROLE {maintenance_role} LOGIN PASSWORD '{maintenance_password}' NOSUPERUSER BYPASSRLS NOCREATEROLE"
        ))
        .execute(&admin)
        .await
        .unwrap();
        sqlx::query(&format!(
            "CREATE DATABASE {database} OWNER {migration_role}"
        ))
        .execute(&admin)
        .await
        .unwrap();
        admin.close().await;

        let migration_url = role_url(&admin_url, &database, &migration_role, &migration_password);
        let runtime_url = role_url(&admin_url, &database, &runtime_role, &runtime_password);
        let maintenance_url = role_url(
            &admin_url,
            &database,
            &maintenance_role,
            &maintenance_password,
        );
        let migration = PgPoolOptions::new()
            .max_connections(2)
            .connect(&migration_url)
            .await
            .unwrap();
        peppy_server::MIGRATOR.run(&migration).await.unwrap();
        let runtime = PgPoolOptions::new()
            .max_connections(8)
            .connect(&runtime_url)
            .await
            .unwrap();
        let maintenance = PgPoolOptions::new()
            .max_connections(2)
            .connect(&maintenance_url)
            .await
            .unwrap();
        rls::grant_runtime_privileges(&migration, &runtime_role)
            .await
            .unwrap();
        rls::grant_maintenance_privileges(&migration, &maintenance_role)
            .await
            .unwrap();

        Self {
            admin_url,
            database,
            migration_role,
            runtime_role,
            maintenance_role,
            migration,
            runtime,
            maintenance,
            runtime_url,
        }
    }

    fn config(&self) -> Config {
        Config {
            bind_addr: "127.0.0.1:0".parse::<SocketAddr>().unwrap(),
            database_url: self.runtime_url.clone(),
            release_identity: "rls-integration-test".into(),
            s3: None,
            public_api_url: Some(Url::parse("http://localhost:3000").unwrap()),
            public_attachment_url: None,
            vault_attachment_quota_bytes: 123_456,
            trusted_proxy_cidrs: vec![],
            replay_retention: Duration::from_secs(7 * 86_400),
            relay_url: None,
            web_client: None,
            web_live_trusted_origin: None,
        }
    }

    async fn app(&self) -> Router {
        let server = ServerBuilder::community(self.config(), self.runtime.clone())
            .maintenance_database(self.maintenance.clone())
            .rls_enforced(true)
            .setup_token(SETUP_TOKEN)
            .build()
            .await
            .unwrap();
        server.router()
    }

    async fn cleanup(self) {
        self.runtime.close().await;
        self.maintenance.close().await;
        self.migration.close().await;
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&self.admin_url)
            .await
            .unwrap();
        sqlx::query(&format!("DROP DATABASE {} WITH (FORCE)", self.database))
            .execute(&admin)
            .await
            .unwrap();
        sqlx::query(&format!(
            "DROP ROLE {}, {}, {}",
            self.runtime_role, self.maintenance_role, self.migration_role
        ))
        .execute(&admin)
        .await
        .unwrap();
        admin.close().await;
    }
}

fn role_url(base: &str, database: &str, role: &str, password: &str) -> String {
    let mut url: Url = base.parse().unwrap();
    url.set_username(role).unwrap();
    url.set_password(Some(password)).unwrap();
    url.set_path(&format!("/{database}"));
    url.to_string()
}

async fn send(app: Router, method: Method, path: &str, body: Option<&Value>) -> Response<Body> {
    let mut request = Request::builder().method(method).uri(path);
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

async fn response_body(response: Response<Body>) -> Value {
    serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap()
}

fn material(vault_id: Uuid) -> Value {
    let profile = KeyProfile::new(vault_id, 1).unwrap();
    json!({
        "profile": profile,
        "check_header": URL_SAFE_NO_PAD.encode([9_u8; 48]),
        "fingerprint": profile.fingerprint().unwrap(),
        "key_epoch": 1,
        "admission_public_key": URL_SAFE_NO_PAD.encode([11_u8; 32]),
    })
}

fn device(device_id: Uuid) -> Value {
    json!({
        "device_id": device_id,
        "kind": "desktop",
        "name": "Primary device",
        "signing_public_key": URL_SAFE_NO_PAD.encode([7_u8; 32]),
    })
}

#[tokio::test]
async fn community_runtime_is_rls_scoped_and_control_plane_http_flows_work() {
    let fixture = Fixture::new().await;

    rls::validate_runtime_role(&fixture.runtime, true)
        .await
        .unwrap();
    rls::validate_maintenance_role(&fixture.maintenance)
        .await
        .unwrap();
    let role_flags: (bool, bool, bool) = sqlx::query_as(
        "SELECT rolsuper, rolbypassrls, EXISTS (SELECT 1 FROM pg_auth_members WHERE member = pg_roles.oid) FROM pg_roles WHERE rolname = current_user",
    )
    .fetch_one(&fixture.runtime)
    .await
    .unwrap();
    assert_eq!(role_flags, (false, false, false));
    let marker: i16 = sqlx::query_scalar("SELECT version FROM peppy_schema_marker")
        .fetch_one(&fixture.runtime)
        .await
        .unwrap();
    assert_eq!(marker, 1_i16);

    let app = fixture.app().await;
    assert_eq!(
        send(app.clone(), Method::GET, "/readyz", None)
            .await
            .status(),
        StatusCode::OK
    );

    let vault_id = Uuid::new_v4();
    let device_id = Uuid::new_v4();
    let bearer = "a1".repeat(48);
    let credential_digest = hex::encode(Sha256::digest(bearer.as_bytes()));
    let claimed = send(
        app.clone(),
        Method::POST,
        "/v1/instance/claim",
        Some(&json!({
            "operation_id": Uuid::new_v4(),
            "setup_token": SETUP_TOKEN,
            "username": "admin",
            "admin_password": ADMIN_PASSWORD,
            "device": device(device_id),
            "material": material(vault_id),
            "credential_digest": credential_digest,
        })),
    )
    .await;
    assert_eq!(
        claimed.status(),
        StatusCode::OK,
        "{}",
        response_body(claimed).await
    );

    let login = send(
        app.clone(),
        Method::POST,
        "/v1/instance/session",
        Some(&json!({
            "username": "admin",
            "admin_password": ADMIN_PASSWORD,
            "client": "desktop",
        })),
    )
    .await;
    assert_eq!(login.status(), StatusCode::OK);
    let login_body = response_body(login).await;
    let admin_session = login_body["session_token"].as_str().unwrap().to_owned();
    for path in [
        "/v1/instance/account",
        "/v1/instance/management",
        "/v1/instance/vaults",
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::get(path)
                    .header("peppy-instance-session", &admin_session)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{path}");
    }

    let vault = app
        .clone()
        .oneshot(
            Request::get("/v1/vault")
                .header("authorization", format!("Bearer {bearer}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        vault.status(),
        StatusCode::OK,
        "{}",
        response_body(vault).await
    );

    let invitation_token_digest = URL_SAFE_NO_PAD.encode(Sha256::digest([4_u8; 32]));
    let invitation = app
        .clone()
        .oneshot(
            Request::post("/v1/enrollment/invitations")
                .header("authorization", format!("Bearer {bearer}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "method": "transfer",
                        "transfer_public_key": URL_SAFE_NO_PAD.encode([7_u8; 32]),
                        "invitation_token_digest": invitation_token_digest.clone(),
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(invitation.status(), StatusCode::OK);
    let invitation_body = response_body(invitation).await;
    let invitation_id = invitation_body["invitation_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let joining_device = crypto_sign::KeyPair::generate().unwrap();
    let invitation_claim = send(
        app.clone(),
        Method::POST,
        "/v1/enrollment/invitations/claim",
        Some(&json!({
            "invitation_token_digest": invitation_token_digest,
            "claim_authenticator": URL_SAFE_NO_PAD.encode([5_u8; 32]),
            "device": {
                "device_id": Uuid::new_v4(),
                "kind": "android",
                "name": "Joining phone",
                "signing_public_key": URL_SAFE_NO_PAD.encode(joining_device.public_key.as_bytes()),
            },
            "transfer_public_key": URL_SAFE_NO_PAD.encode([8_u8; 32]),
            "credential_digest": "22".repeat(32),
        })),
    )
    .await;
    assert_eq!(invitation_claim.status(), StatusCode::OK);
    let invitation_claim_body = response_body(invitation_claim).await;
    let claim_secret = invitation_claim_body["claim_secret"].as_str().unwrap();
    let invitation_status = app
        .clone()
        .oneshot(
            Request::get(format!("/v1/enrollment/invitations/{invitation_id}"))
                .header("peppy-claim-secret", claim_secret)
                .header("origin", "http://localhost:3000")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        invitation_status.status(),
        StatusCode::OK,
        "{}",
        response_body(invitation_status).await
    );

    let computer_id = Uuid::new_v4();
    let computer_claim_token = [6_u8; 32];
    let computer_create = app
        .clone()
        .oneshot(
            Request::post("/v1/enrollment/computers")
                .header("authorization", format!("Bearer {bearer}"))
                .header("origin", "http://localhost:3000")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "invitation_id": computer_id,
                        "recipient": {
                            "device": {
                                "device_id": Uuid::new_v4(),
                                "kind": "desktop",
                                "name": "Joining computer",
                                "signing_public_key": URL_SAFE_NO_PAD.encode([2_u8; 32]),
                            },
                            "credential_digest": "33".repeat(32),
                            "requested_role": "device",
                        },
                        "recipient_transfer_public_key": URL_SAFE_NO_PAD.encode([3_u8; 32]),
                        "recipient_commitment": URL_SAFE_NO_PAD.encode([4_u8; 32]),
                        "claim_token_digest": URL_SAFE_NO_PAD.encode(Sha256::digest(computer_claim_token)),
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        computer_create.status(),
        StatusCode::OK,
        "{}",
        response_body(computer_create).await
    );
    let computer_status = app
        .clone()
        .oneshot(
            Request::post(format!("/v1/enrollment/computers/{computer_id}/status"))
                .header("origin", "http://localhost:3000")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"claim_token": URL_SAFE_NO_PAD.encode(computer_claim_token)})
                        .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        computer_status.status(),
        StatusCode::OK,
        "{}",
        response_body(computer_status).await
    );

    let manual_credential = "44".repeat(32);
    let manual_create = app
        .clone()
        .oneshot(
            Request::post("/v1/enrollment/challenges")
                .header("authorization", format!("Bearer {bearer}"))
                .header("origin", "http://localhost:3000")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "device": {
                            "device_id": Uuid::new_v4(),
                            "kind": "desktop",
                            "name": "Manual computer",
                            "signing_public_key": URL_SAFE_NO_PAD.encode([9_u8; 32]),
                        },
                        "vault_id": vault_id,
                        "vault_owner": "admin",
                        "credential_digest": manual_credential,
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(manual_create.status(), StatusCode::OK);
    let manual_body = response_body(manual_create).await;
    let manual_id = manual_body["enrollment_id"].as_str().unwrap();
    let manual_complete = send(
        app.clone(),
        Method::POST,
        &format!("/v1/enrollment/challenges/{manual_id}/complete"),
        Some(&json!({
            "admission_signature": URL_SAFE_NO_PAD.encode([0_u8; 64]),
            "device_signature": URL_SAFE_NO_PAD.encode([0_u8; 64]),
            "credential_digest": manual_credential,
        })),
    )
    .await;
    assert_eq!(manual_complete.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        response_body(manual_complete).await["code"],
        "challenge_proof_mismatch"
    );

    let other_vault = Uuid::new_v4();
    sqlx::query("INSERT INTO vaults(vault_id,public_key_profile,encrypted_vault_check_header,key_epoch,profile_fingerprint) VALUES($1,'{}',$2,1,$3)")
        .bind(other_vault)
        .bind([1_u8].as_slice())
        .bind("f".repeat(64))
        .execute(&fixture.migration)
        .await
        .unwrap();
    sqlx::query("INSERT INTO vault_owners(vault_id,username,password_hash,is_admin,operation_id,request_digest) VALUES($1,'secondary','hash',FALSE,$2,$3)")
        .bind(other_vault)
        .bind(Uuid::new_v4())
        .bind([8_u8; 32].as_slice())
        .execute(&fixture.migration)
        .await
        .unwrap();
    let summaries = app
        .clone()
        .oneshot(
            Request::get("/v1/instance/vaults")
                .header("peppy-instance-session", &admin_session)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(summaries.status(), StatusCode::OK);
    assert_eq!(response_body(summaries).await.as_array().unwrap().len(), 2);

    let unscoped: i64 = sqlx::query_scalar("SELECT count(*) FROM vaults")
        .fetch_one(&fixture.runtime)
        .await
        .unwrap();
    assert_eq!(unscoped, 0);
    let resolved: Option<Uuid> =
        sqlx::query_scalar("SELECT vault_id FROM community_resolve_device_credential($1)")
            .bind(Sha256::digest(bearer.as_bytes()).as_slice())
            .fetch_optional(&fixture.runtime)
            .await
            .unwrap();
    assert_eq!(resolved, Some(vault_id));
    let unknown: Option<Uuid> =
        sqlx::query_scalar("SELECT vault_id FROM community_resolve_device_credential($1)")
            .bind([0_u8; 32].as_slice())
            .fetch_optional(&fixture.runtime)
            .await
            .unwrap();
    assert_eq!(unknown, None);

    let mut scoped = fixture.runtime.begin().await.unwrap();
    sqlx::query("SELECT set_config('peppy.vault_id', $1, true)")
        .bind(vault_id.to_string())
        .execute(&mut *scoped)
        .await
        .unwrap();
    let scoped_count: i64 = sqlx::query_scalar("SELECT count(*) FROM vaults")
        .fetch_one(&mut *scoped)
        .await
        .unwrap();
    assert_eq!(scoped_count, 1);
    let cross_vault_write =
        sqlx::query("UPDATE vaults SET key_epoch=key_epoch+1 WHERE vault_id=$1")
            .bind(other_vault)
            .execute(&mut *scoped)
            .await
            .unwrap();
    assert_eq!(cross_vault_write.rows_affected(), 0);
    scoped.rollback().await.unwrap();
    let maintenance_count: i64 = sqlx::query_scalar("SELECT count(*) FROM vaults")
        .fetch_one(&fixture.maintenance)
        .await
        .unwrap();
    assert_eq!(maintenance_count, 2);

    let control_plane_rows: i64 = sqlx::query_scalar("SELECT count(*) FROM vault_owners")
        .fetch_one(&fixture.runtime)
        .await
        .unwrap();
    assert_eq!(control_plane_rows, 2);
    assert!(
        sqlx::query("DELETE FROM instance_sessions WHERE false")
            .execute(&fixture.runtime)
            .await
            .is_err()
    );
    sqlx::query("DELETE FROM instance_sessions WHERE false")
        .execute(&fixture.maintenance)
        .await
        .unwrap();
    assert!(
        sqlx::query("DELETE FROM instance_admission_windows WHERE false")
            .execute(&fixture.runtime)
            .await
            .is_err()
    );
    sqlx::query("DELETE FROM instance_admission_windows WHERE false")
        .execute(&fixture.maintenance)
        .await
        .unwrap();
    sqlx::query("INSERT INTO instance_admission_windows(window_started,count) VALUES(date_trunc('minute',now())-interval '3 hours',1)")
        .execute(&fixture.migration)
        .await
        .unwrap();
    let swept = sqlx::query("DELETE FROM instance_admission_windows WHERE ctid IN (SELECT ctid FROM instance_admission_windows WHERE window_started < date_trunc('minute', now()) - interval '2 hours' ORDER BY window_started LIMIT 1000 FOR UPDATE SKIP LOCKED)")
        .execute(&fixture.maintenance)
        .await
        .unwrap();
    assert_eq!(swept.rows_affected(), 1);

    fixture.cleanup().await;
}

#[tokio::test]
async fn live_ticket_resolver_and_consume_are_runtime_scoped() {
    let fixture = Fixture::new().await;
    let vault_a = Uuid::new_v4();
    let vault_b = Uuid::new_v4();
    let device = Uuid::new_v4();
    let session = [31_u8; 16];
    let secret = [32_u8; 32];
    let session_hash = Sha256::digest(session).to_vec();
    let ticket_digest = Sha256::digest(secret).to_vec();
    for (vault, username) in [(vault_a, "ownera"), (vault_b, "ownerb")] {
        sqlx::query("INSERT INTO vaults(vault_id,public_key_profile,encrypted_vault_check_header,key_epoch,profile_fingerprint) VALUES($1,'{}',$2,1,$3)")
            .bind(vault).bind([1_u8].as_slice()).bind("f".repeat(64)).execute(&fixture.migration).await.unwrap();
        sqlx::query("INSERT INTO vault_owners(vault_id,username,password_hash,is_admin,operation_id,request_digest) VALUES($1,$2,'hash',FALSE,$3,$4)")
            .bind(vault).bind(username).bind(Uuid::new_v4()).bind([8_u8; 32].as_slice()).execute(&fixture.migration).await.unwrap();
    }
    sqlx::query("INSERT INTO devices(vault_id,device_id,role,public_key,profile_fingerprint,key_epoch) VALUES($1,$2,'device','{}',$3,1)")
        .bind(vault_a).bind(device).bind("f".repeat(64)).execute(&fixture.migration).await.unwrap();
    sqlx::query("INSERT INTO device_credentials(token_digest,vault_id,device_id) VALUES($1,$2,$3)")
        .bind([9_u8; 32].as_slice())
        .bind(vault_a)
        .bind(device)
        .execute(&fixture.migration)
        .await
        .unwrap();
    sqlx::query("INSERT INTO live_ticket_sessions(vault_id,device_id,session_id_hash,state,lease_until) VALUES($1,$2,$3,'open',clock_timestamp()+interval '90 seconds')")
        .bind(vault_a).bind(device).bind(&session_hash).execute(&fixture.migration).await.unwrap();
    sqlx::query("INSERT INTO live_tickets(ticket_digest,vault_id,device_id,session_id_hash,issued_at,expires_at) VALUES($1,$2,$3,$4,clock_timestamp(),clock_timestamp()+interval '30 seconds')")
        .bind(&ticket_digest).bind(vault_a).bind(device).bind(&session_hash).execute(&fixture.migration).await.unwrap();
    let resolved: Option<Uuid> =
        sqlx::query_scalar("SELECT vault_id FROM community_resolve_live_ticket_vault($1)")
            .bind(&ticket_digest)
            .fetch_optional(&fixture.runtime)
            .await
            .unwrap();
    assert_eq!(resolved, Some(vault_a));
    let mut b = fixture.runtime.begin().await.unwrap();
    sqlx::query("SELECT set_config('peppy.vault_id', $1, true)")
        .bind(vault_b.to_string())
        .execute(&mut *b)
        .await
        .unwrap();
    let hidden: Option<i32> =
        sqlx::query_scalar("SELECT 1 FROM live_tickets WHERE ticket_digest=$1")
            .bind(&ticket_digest)
            .fetch_optional(&mut *b)
            .await
            .unwrap();
    assert_eq!(hidden, None);
    b.commit().await.unwrap();
    let consumed = peppy_server::__test_consume_live_ticket(&fixture.runtime, session, secret)
        .await
        .unwrap();
    assert_eq!(
        (consumed.vault_id, consumed.device_id, consumed.session_id),
        (vault_a, device, session)
    );

    let maintenance_device = Uuid::new_v4();
    sqlx::query("INSERT INTO devices(vault_id,device_id,role,public_key,profile_fingerprint,key_epoch) VALUES($1,$2,'device','{}',$3,1)")
        .bind(vault_b)
        .bind(maintenance_device)
        .bind("f".repeat(64))
        .execute(&fixture.migration)
        .await
        .unwrap();
    for (vault, device, session_hash, ticket_digest) in [
        (vault_a, device, vec![41_u8; 32], vec![42_u8; 32]),
        (
            vault_b,
            maintenance_device,
            vec![43_u8; 32],
            vec![44_u8; 32],
        ),
    ] {
        sqlx::query("INSERT INTO live_ticket_sessions(vault_id,device_id,session_id_hash,state,lease_until) VALUES($1,$2,$3,'open',clock_timestamp()-interval '1 second')")
            .bind(vault)
            .bind(device)
            .bind(&session_hash)
            .execute(&fixture.migration)
            .await
            .unwrap();
        sqlx::query("INSERT INTO live_tickets(ticket_digest,vault_id,device_id,session_id_hash,issued_at,expires_at) VALUES($1,$2,$3,$4,clock_timestamp()-interval '2 seconds',clock_timestamp()-interval '1 second')")
            .bind(ticket_digest)
            .bind(vault)
            .bind(device)
            .bind(session_hash)
            .execute(&fixture.migration)
            .await
            .unwrap();
    }
    let mut server = ServerBuilder::community(fixture.config(), fixture.runtime.clone())
        .maintenance_database(fixture.maintenance.clone())
        .rls_enforced(true)
        .setup_token(SETUP_TOKEN)
        .build()
        .await
        .unwrap();
    server.start_maintenance().unwrap();
    for _ in 0..50 {
        let remaining: i64 =
            sqlx::query_scalar("SELECT count(*) FROM live_tickets WHERE ticket_digest = ANY($1)")
                .bind(vec![vec![42_u8; 32], vec![44_u8; 32]])
                .fetch_one(&fixture.migration)
                .await
                .unwrap();
        if remaining == 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let remaining: i64 =
        sqlx::query_scalar("SELECT count(*) FROM live_tickets WHERE ticket_digest = ANY($1)")
            .bind(vec![vec![42_u8; 32], vec![44_u8; 32]])
            .fetch_one(&fixture.migration)
            .await
            .unwrap();
    assert_eq!(remaining, 0, "maintenance reaper must cross vault RLS");
    fixture.cleanup().await;
}

#[tokio::test]
async fn startup_validation_rejects_grant_policy_and_execute_drift() {
    let fixture = Fixture::new().await;

    sqlx::query("CREATE TABLE unexpected_runtime_table(id integer)")
        .execute(&fixture.migration)
        .await
        .unwrap();
    sqlx::query(&format!(
        "GRANT SELECT ON unexpected_runtime_table TO {}",
        fixture.runtime_role
    ))
    .execute(&fixture.migration)
    .await
    .unwrap();
    assert!(
        rls::validate_runtime_role(&fixture.runtime, true)
            .await
            .is_err()
    );
    sqlx::raw_sql(&format!(
        "REVOKE ALL ON unexpected_runtime_table FROM {}; DROP TABLE unexpected_runtime_table",
        fixture.runtime_role
    ))
    .execute(&fixture.migration)
    .await
    .unwrap();

    sqlx::query("DROP POLICY community_vaults_scope ON vaults")
        .execute(&fixture.migration)
        .await
        .unwrap();
    assert!(
        rls::validate_runtime_role(&fixture.runtime, true)
            .await
            .is_err()
    );
    sqlx::query("CREATE POLICY community_vaults_scope ON vaults USING (vault_id::text = nullif(current_setting('peppy.vault_id', true), '')) WITH CHECK (vault_id::text = nullif(current_setting('peppy.vault_id', true), ''))")
        .execute(&fixture.migration)
        .await
        .unwrap();

    sqlx::query(&format!(
        "GRANT DELETE ON vault_owners TO {}",
        fixture.runtime_role
    ))
    .execute(&fixture.migration)
    .await
    .unwrap();
    assert!(
        rls::validate_runtime_role(&fixture.runtime, true)
            .await
            .is_err()
    );
    rls::grant_runtime_privileges(&fixture.migration, &fixture.runtime_role)
        .await
        .unwrap();

    sqlx::query(&format!(
        "GRANT TRUNCATE ON vaults TO {}",
        fixture.runtime_role
    ))
    .execute(&fixture.migration)
    .await
    .unwrap();
    assert!(
        rls::validate_runtime_role(&fixture.runtime, true)
            .await
            .is_err()
    );
    sqlx::query(&format!(
        "REVOKE TRUNCATE ON vaults FROM {}",
        fixture.runtime_role
    ))
    .execute(&fixture.migration)
    .await
    .unwrap();

    sqlx::query(&format!(
        "GRANT CREATE ON SCHEMA public TO {}",
        fixture.runtime_role
    ))
    .execute(&fixture.migration)
    .await
    .unwrap();
    assert!(
        rls::validate_runtime_role(&fixture.runtime, true)
            .await
            .is_err()
    );
    sqlx::query(&format!(
        "REVOKE CREATE ON SCHEMA public FROM {}",
        fixture.runtime_role
    ))
    .execute(&fixture.migration)
    .await
    .unwrap();

    sqlx::query("GRANT EXECUTE ON FUNCTION community_resolve_device_credential(bytea) TO PUBLIC")
        .execute(&fixture.migration)
        .await
        .unwrap();
    assert!(
        rls::validate_runtime_role(&fixture.runtime, true)
            .await
            .is_err()
    );
    sqlx::query(
        "REVOKE EXECUTE ON FUNCTION community_resolve_device_credential(bytea) FROM PUBLIC",
    )
    .execute(&fixture.migration)
    .await
    .unwrap();
    rls::validate_runtime_role(&fixture.runtime, true)
        .await
        .unwrap();

    sqlx::query(&format!(
        "REVOKE UPDATE ON instance_admission_windows FROM {}",
        fixture.maintenance_role
    ))
    .execute(&fixture.migration)
    .await
    .unwrap();
    assert!(
        rls::validate_maintenance_role(&fixture.maintenance)
            .await
            .is_err()
    );
    rls::grant_maintenance_privileges(&fixture.migration, &fixture.maintenance_role)
        .await
        .unwrap();
    rls::validate_maintenance_role(&fixture.maintenance)
        .await
        .unwrap();

    sqlx::query(&format!(
        "GRANT TRUNCATE ON devices TO {}",
        fixture.maintenance_role
    ))
    .execute(&fixture.migration)
    .await
    .unwrap();
    assert!(
        rls::validate_maintenance_role(&fixture.maintenance)
            .await
            .is_err()
    );
    rls::grant_maintenance_privileges(&fixture.migration, &fixture.maintenance_role)
        .await
        .unwrap();
    rls::validate_maintenance_role(&fixture.maintenance)
        .await
        .unwrap();

    fixture.cleanup().await;
}

#[tokio::test]
async fn hosted_mode_missing_runtime_admission_grants_fails_closed_and_correct_grants_succeed() {
    let fixture = Fixture::new().await;
    let admission_tables = "instance_admission_windows, instance_admission_sources";

    sqlx::raw_sql(&format!(
        "REVOKE SELECT, INSERT, UPDATE ON TABLE {admission_tables} FROM {}",
        fixture.runtime_role
    ))
    .execute(&fixture.migration)
    .await
    .unwrap();
    let missing_grants = ServerBuilder::new(fixture.config(), fixture.runtime.clone())
        .setup_token(SETUP_TOKEN)
        .row_security(true)
        .maintenance_database(fixture.maintenance.clone())
        .build()
        .await
        .err()
        .expect("hosted startup must reject unsafe admission grants");
    assert!(
        missing_grants
            .to_string()
            .contains("admission ledger grant")
    );

    sqlx::raw_sql(&format!(
        "GRANT SELECT, INSERT, UPDATE ON TABLE {admission_tables} TO {}",
        fixture.runtime_role
    ))
    .execute(&fixture.migration)
    .await
    .unwrap();
    let server = ServerBuilder::new(fixture.config(), fixture.runtime.clone())
        .setup_token(SETUP_TOKEN)
        .row_security(true)
        .maintenance_database(fixture.maintenance.clone())
        .build()
        .await
        .unwrap();

    drop(server);

    sqlx::raw_sql(&format!(
        "REVOKE EXECUTE ON FUNCTION community_resolve_live_ticket_vault(bytea) FROM {}",
        fixture.runtime_role
    ))
    .execute(&fixture.migration)
    .await
    .unwrap();
    let error = ServerBuilder::new(fixture.config(), fixture.runtime.clone())
        .setup_token(SETUP_TOKEN)
        .row_security(true)
        .maintenance_database(fixture.maintenance.clone())
        .build()
        .await
        .err()
        .expect("hosted startup must reject a missing ticket resolver grant");
    assert!(error.to_string().contains("live ticket resolver execute"));
    rls::grant_runtime_privileges(&fixture.migration, &fixture.runtime_role)
        .await
        .unwrap();

    sqlx::raw_sql(&format!(
        "REVOKE DELETE ON TABLE live_tickets FROM {}",
        fixture.runtime_role
    ))
    .execute(&fixture.migration)
    .await
    .unwrap();
    let error = ServerBuilder::new(fixture.config(), fixture.runtime.clone())
        .setup_token(SETUP_TOKEN)
        .row_security(true)
        .maintenance_database(fixture.maintenance.clone())
        .build()
        .await
        .err()
        .expect("hosted startup must reject missing live-ticket runtime DML");
    assert!(error.to_string().contains("live ticket runtime DML"));
    rls::grant_runtime_privileges(&fixture.migration, &fixture.runtime_role)
        .await
        .unwrap();

    sqlx::raw_sql(&format!(
        "REVOKE DELETE ON TABLE live_tickets FROM {}",
        fixture.maintenance_role
    ))
    .execute(&fixture.migration)
    .await
    .unwrap();
    let error = ServerBuilder::new(fixture.config(), fixture.runtime.clone())
        .setup_token(SETUP_TOKEN)
        .row_security(true)
        .maintenance_database(fixture.maintenance.clone())
        .build()
        .await
        .err()
        .expect("hosted startup must reject missing live-ticket maintenance DML");
    assert!(error.to_string().contains("requires tenant DML"));
    rls::grant_maintenance_privileges(&fixture.migration, &fixture.maintenance_role)
        .await
        .unwrap();
    fixture.cleanup().await;
}

#[tokio::test]
async fn hosted_mode_missing_maintenance_update_fails_closed() {
    let fixture = Fixture::new().await;

    sqlx::raw_sql(&format!(
        "REVOKE UPDATE ON TABLE instance_admission_windows, instance_admission_sources FROM {}",
        fixture.maintenance_role
    ))
    .execute(&fixture.migration)
    .await
    .unwrap();
    let error = ServerBuilder::new(fixture.config(), fixture.runtime.clone())
        .setup_token(SETUP_TOKEN)
        .row_security(true)
        .maintenance_database(fixture.maintenance.clone())
        .build()
        .await
        .err()
        .expect("hosted startup must reject unsafe admission grants");

    assert!(error.to_string().contains("admission ledger grant"));
    fixture.cleanup().await;
}

#[tokio::test]
async fn hosted_mode_rejects_runtime_delete_on_admission_tables() {
    let fixture = Fixture::new().await;

    sqlx::raw_sql(&format!(
        "GRANT DELETE ON TABLE instance_admission_windows, instance_admission_sources TO {}",
        fixture.runtime_role
    ))
    .execute(&fixture.migration)
    .await
    .unwrap();
    let error = ServerBuilder::new(fixture.config(), fixture.runtime.clone())
        .setup_token(SETUP_TOKEN)
        .row_security(true)
        .maintenance_database(fixture.maintenance.clone())
        .build()
        .await
        .err()
        .expect("hosted startup must reject unsafe admission grants");

    assert!(error.to_string().contains("admission ledger grant"));
    fixture.cleanup().await;
}

#[tokio::test]
async fn policy_rewritten_true_fails_closed() {
    let fixture = Fixture::new().await;

    sqlx::raw_sql(
        "DROP POLICY community_vaults_scope ON vaults; \
         CREATE POLICY community_vaults_scope ON vaults \
         USING (true) WITH CHECK (true)",
    )
    .execute(&fixture.migration)
    .await
    .unwrap();

    assert!(
        rls::validate_runtime_role(&fixture.runtime, true)
            .await
            .is_err()
    );
    fixture.cleanup().await;
}

#[tokio::test]
async fn extra_true_policy_fails_closed() {
    let fixture = Fixture::new().await;

    sqlx::query(
        "CREATE POLICY unexpected_vaults_allow_all ON vaults \
         USING (true) WITH CHECK (true)",
    )
    .execute(&fixture.migration)
    .await
    .unwrap();

    assert!(
        rls::validate_runtime_role(&fixture.runtime, true)
            .await
            .is_err()
    );
    fixture.cleanup().await;
}

#[tokio::test]
async fn valid_source_community_policies_pass() {
    let fixture = Fixture::new().await;

    rls::validate_runtime_role(&fixture.runtime, true)
        .await
        .unwrap();
    fixture.cleanup().await;
}

#[tokio::test]
async fn membership_versions_are_trigger_owned_and_rls_read_only() {
    let fixture = Fixture::new().await;
    let vault = Uuid::new_v4();
    let device = Uuid::new_v4();
    sqlx::query("INSERT INTO vaults(vault_id,public_key_profile,encrypted_vault_check_header,key_epoch,profile_fingerprint) VALUES($1,'{}',$2,1,$3)")
        .bind(vault)
        .bind([1_u8].as_slice())
        .bind("f".repeat(64))
        .execute(&fixture.migration)
        .await
        .unwrap();
    let initial: i64 =
        sqlx::query_scalar("SELECT version FROM membership_versions WHERE vault_id=$1")
            .bind(vault)
            .fetch_one(&fixture.migration)
            .await
            .unwrap();
    assert_eq!(
        initial, 1,
        "vault creation initializes the version atomically"
    );

    let mut shadow_runtime = fixture.runtime.begin().await.unwrap();
    sqlx::query("SELECT set_config('peppy.vault_id', $1, true)")
        .bind(vault.to_string())
        .execute(&mut *shadow_runtime)
        .await
        .unwrap();
    sqlx::query("CREATE TEMP TABLE membership_versions(vault_id uuid, version bigint)")
        .execute(&mut *shadow_runtime)
        .await
        .unwrap();
    sqlx::query("INSERT INTO membership_versions VALUES($1, 100)")
        .bind(vault)
        .execute(&mut *shadow_runtime)
        .await
        .unwrap();
    sqlx::query("INSERT INTO devices(vault_id,device_id,role,public_key,profile_fingerprint,key_epoch) VALUES($1,$2,'device','{}',$3,1)")
        .bind(vault)
        .bind(Uuid::new_v4())
        .bind("f".repeat(64))
        .execute(&mut *shadow_runtime)
        .await
        .unwrap();
    let shadow_version: i64 =
        sqlx::query_scalar("SELECT version FROM membership_versions WHERE vault_id=$1")
            .bind(vault)
            .fetch_one(&mut *shadow_runtime)
            .await
            .unwrap();
    assert_eq!(shadow_version, 100, "temporary shadow is never touched");
    sqlx::query("DROP TABLE membership_versions")
        .execute(&mut *shadow_runtime)
        .await
        .unwrap();
    shadow_runtime.commit().await.unwrap();
    let after_shadow: i64 =
        sqlx::query_scalar("SELECT version FROM membership_versions WHERE vault_id=$1")
            .bind(vault)
            .fetch_one(&fixture.migration)
            .await
            .unwrap();
    assert_eq!(
        after_shadow,
        initial + 1,
        "real version table is invalidated"
    );

    let mut runtime = fixture.runtime.begin().await.unwrap();
    sqlx::query("SELECT set_config('peppy.vault_id', $1, true)")
        .bind(vault.to_string())
        .execute(&mut *runtime)
        .await
        .unwrap();
    sqlx::query("INSERT INTO devices(vault_id,device_id,role,public_key,profile_fingerprint,key_epoch,name) VALUES($1,$2,'device','{}',$3,1,'first')")
        .bind(vault)
        .bind(device)
        .bind("f".repeat(64))
        .execute(&mut *runtime)
        .await
        .unwrap();
    sqlx::query("INSERT INTO device_credentials(token_digest,vault_id,device_id) VALUES($1,$2,$3)")
        .bind([2_u8; 32].as_slice())
        .bind(vault)
        .bind(device)
        .execute(&mut *runtime)
        .await
        .unwrap();
    let bumped: i64 =
        sqlx::query_scalar("SELECT version FROM membership_versions WHERE vault_id=$1")
            .bind(vault)
            .fetch_one(&mut *runtime)
            .await
            .unwrap();
    assert_eq!(
        bumped,
        after_shadow + 2,
        "device and active credential mutations invalidate membership"
    );
    sqlx::query("UPDATE devices SET name='renamed' WHERE vault_id=$1 AND device_id=$2")
        .bind(vault)
        .bind(device)
        .execute(&mut *runtime)
        .await
        .unwrap();
    let unchanged: i64 =
        sqlx::query_scalar("SELECT version FROM membership_versions WHERE vault_id=$1")
            .bind(vault)
            .fetch_one(&mut *runtime)
            .await
            .unwrap();
    assert_eq!(
        unchanged, bumped,
        "display-name changes do not invalidate membership"
    );
    runtime.commit().await.unwrap();

    let mut forbidden_runtime = fixture.runtime.begin().await.unwrap();
    sqlx::query("SELECT set_config('peppy.vault_id', $1, true)")
        .bind(vault.to_string())
        .execute(&mut *forbidden_runtime)
        .await
        .unwrap();
    assert!(
        sqlx::query("UPDATE membership_versions SET version=0 WHERE vault_id=$1")
            .bind(vault)
            .execute(&mut *forbidden_runtime)
            .await
            .is_err()
    );
    forbidden_runtime.rollback().await.unwrap();

    assert!(
        sqlx::query("UPDATE membership_versions SET version=0 WHERE vault_id=$1")
            .bind(vault)
            .execute(&fixture.maintenance)
            .await
            .is_err()
    );
    assert!(
        sqlx::query("ALTER TABLE devices DISABLE TRIGGER devices_invalidate_membership_version")
            .execute(&fixture.runtime)
            .await
            .is_err()
    );
    assert!(
        sqlx::query("ALTER TABLE devices DISABLE TRIGGER devices_invalidate_membership_version")
            .execute(&fixture.maintenance)
            .await
            .is_err()
    );
    assert!(
        sqlx::query("TRUNCATE membership_versions")
            .execute(&fixture.runtime)
            .await
            .is_err()
    );

    sqlx::query(
        "UPDATE devices SET revoked_at=clock_timestamp() WHERE vault_id=$1 AND device_id=$2",
    )
    .bind(vault)
    .bind(device)
    .execute(&fixture.maintenance)
    .await
    .unwrap();
    let after_maintenance: i64 =
        sqlx::query_scalar("SELECT version FROM membership_versions WHERE vault_id=$1")
            .bind(vault)
            .fetch_one(&fixture.maintenance)
            .await
            .unwrap();
    assert_eq!(
        after_maintenance,
        bumped + 1,
        "BYPASSRLS maintenance still fires triggers"
    );

    sqlx::query("DELETE FROM device_credentials WHERE vault_id=$1")
        .bind(vault)
        .execute(&fixture.migration)
        .await
        .unwrap();
    sqlx::query("DELETE FROM devices WHERE vault_id=$1")
        .bind(vault)
        .execute(&fixture.migration)
        .await
        .unwrap();
    sqlx::query("DELETE FROM vaults WHERE vault_id=$1")
        .bind(vault)
        .execute(&fixture.migration)
        .await
        .unwrap();
    let remaining: i64 =
        sqlx::query_scalar("SELECT count(*) FROM membership_versions WHERE vault_id=$1")
            .bind(vault)
            .fetch_one(&fixture.migration)
            .await
            .unwrap();
    assert_eq!(
        remaining, 0,
        "vault cascade removes the version without trigger failures"
    );
    fixture.cleanup().await;
}

#[tokio::test]
async fn admission_verifier_updates_invalidate_membership() {
    let fixture = Fixture::new().await;
    let vault = Uuid::new_v4();
    sqlx::query("INSERT INTO vaults(vault_id,public_key_profile,encrypted_vault_check_header,key_epoch,profile_fingerprint) VALUES($1,'{}',$2,1,$3)")
        .bind(vault)
        .bind([1_u8].as_slice())
        .bind("f".repeat(64))
        .execute(&fixture.migration)
        .await
        .unwrap();
    let mut runtime = fixture.runtime.begin().await.unwrap();
    sqlx::query("SELECT set_config('peppy.vault_id', $1, true)")
        .bind(vault.to_string())
        .execute(&mut *runtime)
        .await
        .unwrap();
    sqlx::query("INSERT INTO vault_admission_keys(vault_id,key_epoch,profile_fingerprint,admission_public_key) VALUES($1,1,$2,$3)")
        .bind(vault)
        .bind("f".repeat(64))
        .bind([2_u8; 32].as_slice())
        .execute(&mut *runtime)
        .await
        .unwrap();
    let before: i64 =
        sqlx::query_scalar("SELECT version FROM membership_versions WHERE vault_id=$1")
            .bind(vault)
            .fetch_one(&mut *runtime)
            .await
            .unwrap();
    sqlx::query(
        "UPDATE vault_admission_keys SET admission_public_key=$2 WHERE vault_id=$1 AND key_epoch=1",
    )
    .bind(vault)
    .bind([3_u8; 32].as_slice())
    .execute(&mut *runtime)
    .await
    .unwrap();
    let after: i64 =
        sqlx::query_scalar("SELECT version FROM membership_versions WHERE vault_id=$1")
            .bind(vault)
            .fetch_one(&mut *runtime)
            .await
            .unwrap();
    assert_eq!(after, before + 1);
    runtime.commit().await.unwrap();
    fixture.cleanup().await;
}

#[tokio::test]
async fn cross_vault_credential_rebind_invalidates_both_vaults() {
    let fixture = Fixture::new().await;
    let vault_a = Uuid::new_v4();
    let vault_b = Uuid::new_v4();
    let device_a = Uuid::new_v4();
    let device_b = Uuid::new_v4();
    for (vault, device) in [(vault_a, device_a), (vault_b, device_b)] {
        sqlx::query("INSERT INTO vaults(vault_id,public_key_profile,encrypted_vault_check_header,key_epoch,profile_fingerprint) VALUES($1,'{}',$2,1,$3)")
            .bind(vault)
            .bind([1_u8].as_slice())
            .bind("f".repeat(64))
            .execute(&fixture.migration)
            .await
            .unwrap();
        sqlx::query("INSERT INTO devices(vault_id,device_id,role,public_key,profile_fingerprint,key_epoch) VALUES($1,$2,'device','{}',$3,1)")
            .bind(vault)
            .bind(device)
            .bind("f".repeat(64))
            .execute(&fixture.migration)
            .await
            .unwrap();
    }
    sqlx::query("INSERT INTO device_credentials(token_digest,vault_id,device_id) VALUES($1,$2,$3)")
        .bind([4_u8; 32].as_slice())
        .bind(vault_a)
        .bind(device_a)
        .execute(&fixture.maintenance)
        .await
        .unwrap();
    let before_a: i64 =
        sqlx::query_scalar("SELECT version FROM membership_versions WHERE vault_id=$1")
            .bind(vault_a)
            .fetch_one(&fixture.maintenance)
            .await
            .unwrap();
    let before_b: i64 =
        sqlx::query_scalar("SELECT version FROM membership_versions WHERE vault_id=$1")
            .bind(vault_b)
            .fetch_one(&fixture.maintenance)
            .await
            .unwrap();
    sqlx::query("UPDATE device_credentials SET vault_id=$2, device_id=$3 WHERE token_digest=$1")
        .bind([4_u8; 32].as_slice())
        .bind(vault_b)
        .bind(device_b)
        .execute(&fixture.maintenance)
        .await
        .unwrap();
    let after_a: i64 =
        sqlx::query_scalar("SELECT version FROM membership_versions WHERE vault_id=$1")
            .bind(vault_a)
            .fetch_one(&fixture.maintenance)
            .await
            .unwrap();
    let after_b: i64 =
        sqlx::query_scalar("SELECT version FROM membership_versions WHERE vault_id=$1")
            .bind(vault_b)
            .fetch_one(&fixture.maintenance)
            .await
            .unwrap();
    assert_eq!(after_a, before_a + 1);
    assert_eq!(after_b, before_b + 1);
    fixture.cleanup().await;
}

#[tokio::test]
async fn hosted_duplicate_equivalent_policies_pass() {
    let fixture = Fixture::new().await;

    for table in rls::TENANT_TABLES {
        let statement = format!(
            "CREATE POLICY hosted_{table}_scope ON {table} \
             USING (vault_id::text = nullif(current_setting('peppy.vault_id', true), '')) \
             WITH CHECK (vault_id::text = nullif(current_setting('peppy.vault_id', true), ''))"
        );
        sqlx::query(&statement)
            .execute(&fixture.migration)
            .await
            .unwrap();
    }

    rls::validate_runtime_role(&fixture.runtime, false)
        .await
        .unwrap();
    fixture.cleanup().await;
}

#[test]
fn community_migration_revokes_all_prescope_resolvers_from_public() {
    let migration = include_str!("../migrations/0021_community_rls.sql");
    for signature in [
        "community_resolve_device_credential(bytea)",
        "community_resolve_public_copy_vault(bytea)",
        "community_resolve_phone_invitation(bytea)",
        "community_resolve_computer_enrollment(uuid, bytea)",
        "community_resolve_phone_invitation_claim(uuid, bytea)",
        "community_resolve_device_admission(uuid)",
        "community_admin_vault_summaries()",
    ] {
        assert!(migration.contains(signature));
        assert!(migration.contains(&format!(
            "REVOKE ALL ON FUNCTION %I.{signature} FROM PUBLIC"
        )));
    }
}

#[test]
fn role_identifiers_cannot_escape_dynamic_grants() {
    assert!(rls::role_identifier("peppy_runtime").is_ok());
    assert!(rls::role_identifier("peppy_runtime; GRANT ALL").is_err());
}
