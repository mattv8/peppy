use std::{net::SocketAddr, time::Duration};

use axum::{
    Router,
    body::Body,
    http::{Method, Request, Response, StatusCode},
};
use base64::{
    Engine as _,
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
};
use http_body_util::BodyExt;
use libsodium_rs::crypto_sign;
use peppy_crypto::{
    KeyProfile, create_vault_check_header, derive_root_key,
    vault_identity::{
        VaultIdentityAnchor, VaultIdentityRecoveryRecord, derive_vault_identity_recovery_key,
    },
};
use peppy_protocol::VAULT_IDENTITY_PROTOCOL_VERSION;
use peppy_server::{ServerBuilder, api::create_owner, config::Config, rls};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{PgConnection, PgPool, postgres::PgPoolOptions};
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
            vault_attachment_quota_bytes: 123_456,
            trusted_proxy_cidrs: vec![],
            replay_retention: Duration::from_secs(7 * 86_400),
            relay_url: None,
            mcp_worker_url: None,
            mcp_worker_token_file: None,
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
    rls::validate_runtime_role(&fixture.runtime, false)
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
async fn vault_identity_routes_use_the_runtime_role_and_survive_vault_cascade() {
    let fixture = Fixture::new().await;
    rls::validate_runtime_role(&fixture.runtime, true)
        .await
        .unwrap();
    rls::validate_maintenance_role(&fixture.maintenance)
        .await
        .unwrap();
    let runtime_privileges: (bool, bool, bool, bool) = sqlx::query_as(
        "SELECT has_table_privilege($1, 'vault_identity_establishments', 'SELECT'), has_table_privilege($1, 'vault_identity_establishments', 'INSERT'), has_table_privilege($1, 'vault_identity_establishments', 'UPDATE'), has_table_privilege($1, 'vault_identity_establishments', 'DELETE')",
    )
    .bind(&fixture.runtime_role)
    .fetch_one(&fixture.migration)
    .await
    .unwrap();
    assert_eq!(runtime_privileges, (true, true, false, false));
    let maintenance_privileges: (bool, bool, bool) = sqlx::query_as(
        "SELECT has_table_privilege($1, 'vault_identity_establishments', 'SELECT'), has_table_privilege($1, 'vault_identity_establishments', 'INSERT'), has_table_privilege($1, 'vault_identity_establishments', 'DELETE')",
    )
    .bind(&fixture.maintenance_role)
    .fetch_one(&fixture.migration)
    .await
    .unwrap();
    assert_eq!(maintenance_privileges, (true, false, false));

    let vault = Uuid::new_v4();
    let profile = KeyProfile::new(vault, 1).unwrap();
    let root = derive_root_key(ADMIN_PASSWORD, &profile).unwrap();
    let header = create_vault_check_header(&root, profile.clone()).unwrap();
    let owner = create_owner(
        &fixture.migration,
        serde_json::to_value(&profile).unwrap(),
        serde_json::to_vec(&header).unwrap(),
        profile.fingerprint().unwrap(),
        1,
    )
    .await
    .unwrap();
    let operation = Uuid::new_v4();
    let recovery_key = derive_vault_identity_recovery_key(&root, &profile).unwrap();
    let anchor = VaultIdentityAnchor::generate(vault, operation, owner.device_id).unwrap();
    let recovery = VaultIdentityRecoveryRecord::wrap(&anchor, &recovery_key, &profile).unwrap();
    let request = json!({
        "protocol_version": VAULT_IDENTITY_PROTOCOL_VERSION,
        "operation_id": operation,
        "key_epoch": 1,
        "profile_fingerprint": profile.fingerprint().unwrap(),
        "recovery_record": STANDARD.encode(recovery.encode()),
    });
    let app = fixture.app().await;

    let establish = app
        .clone()
        .oneshot(
            Request::post("/v1/vault/identity")
                .header("authorization", format!("Bearer {}", owner.device_token))
                .header("content-type", "application/json")
                .body(Body::from(request.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(establish.status(), StatusCode::CREATED);

    let get = app
        .clone()
        .oneshot(
            Request::get("/v1/vault/identity")
                .header("authorization", format!("Bearer {}", owner.device_token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(get.status(), StatusCode::OK);
    assert_eq!(
        response_body(get).await["operation_id"],
        operation.to_string()
    );

    let delete = app
        .oneshot(
            Request::delete("/v1/vault")
                .header("authorization", format!("Bearer {}", owner.device_token))
                .header("content-type", "application/json")
                .body(Body::from(json!({"vault_id": vault}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(delete.status(), StatusCode::NO_CONTENT);
    let remaining: i64 =
        sqlx::query_scalar("SELECT count(*) FROM vault_identity_establishments WHERE vault_id=$1")
            .bind(vault)
            .fetch_one(&fixture.migration)
            .await
            .unwrap();
    assert_eq!(
        remaining, 0,
        "vault cascade removes the established identity"
    );

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
async fn mcp_cleanup_queue_is_maintenance_only_and_oauth_sweep_is_executable() {
    let fixture = Fixture::new().await;
    for statement in [
        "SELECT 1 FROM mcp_cleanup_jobs",
        "INSERT INTO mcp_cleanup_jobs(vault_id,worker_device_id) VALUES(gen_random_uuid(),gen_random_uuid())",
        "UPDATE mcp_cleanup_jobs SET attempts=attempts",
        "DELETE FROM mcp_cleanup_jobs",
    ] {
        assert!(
            sqlx::query(statement)
                .execute(&fixture.runtime)
                .await
                .is_err()
        );
    }
    let maintenance_privileges: (bool, bool, bool, bool, bool, bool, bool) = sqlx::query_as(
        "SELECT has_table_privilege(current_user, 'mcp_cleanup_jobs', 'SELECT'), has_table_privilege(current_user, 'mcp_cleanup_jobs', 'UPDATE'), has_table_privilege(current_user, 'mcp_cleanup_jobs', 'DELETE'), has_table_privilege(current_user, 'mcp_cleanup_jobs', 'INSERT'), has_table_privilege(current_user, 'mcp_cleanup_jobs', 'TRUNCATE'), has_table_privilege(current_user, 'mcp_cleanup_jobs', 'REFERENCES'), has_table_privilege(current_user, 'mcp_cleanup_jobs', 'TRIGGER')",
    )
    .fetch_one(&fixture.maintenance)
    .await
    .unwrap();
    assert_eq!(
        maintenance_privileges,
        (true, true, true, false, false, false, false)
    );
    sqlx::query("SELECT mcp_sweep_oauth()")
        .execute(&fixture.maintenance)
        .await
        .unwrap();
    fixture.cleanup().await;
}

#[tokio::test]
async fn runtime_cleanup_triggers_ignore_a_temporary_queue_shadow() {
    let fixture = Fixture::new().await;
    let device_vault = Uuid::new_v4();
    let operation_vault = Uuid::new_v4();
    let vault_vault = Uuid::new_v4();
    let device_id = Uuid::new_v4();
    let operation_owner = Uuid::new_v4();
    let operation_worker = Uuid::new_v4();
    let vault_device = Uuid::new_v4();

    for (vault, device) in [
        (device_vault, device_id),
        (operation_vault, operation_owner),
        (vault_vault, vault_device),
    ] {
        sqlx::query("INSERT INTO vaults(vault_id,public_key_profile,encrypted_vault_check_header,key_epoch,profile_fingerprint) VALUES($1,'{}',$2,1,$3)")
            .bind(vault)
            .bind([1_u8].as_slice())
            .bind("f".repeat(64))
            .execute(&fixture.migration)
            .await
            .unwrap();
        sqlx::query("INSERT INTO devices(vault_id,device_id,role,public_key,profile_fingerprint,key_epoch,name,kind) VALUES($1,$2,'owner','{}',$3,1,'owner','desktop')")
            .bind(vault)
            .bind(device)
            .bind("f".repeat(64))
            .execute(&fixture.migration)
            .await
            .unwrap();
    }
    sqlx::query("INSERT INTO mcp_devices(vault_id,device_id) VALUES($1,$2),($3,$4)")
        .bind(device_vault)
        .bind(device_id)
        .bind(vault_vault)
        .bind(vault_device)
        .execute(&fixture.migration)
        .await
        .unwrap();
    sqlx::query("INSERT INTO mcp_operations(id,vault_id,owner_device_id,worker_device_id,state) VALUES($1,$2,$3,$4,'activated')")
        .bind(Uuid::new_v4())
        .bind(operation_vault)
        .bind(operation_owner)
        .bind(operation_worker)
        .execute(&fixture.migration)
        .await
        .unwrap();

    let mut runtime = fixture.runtime.begin().await.unwrap();
    sqlx::raw_sql(
        "CREATE TEMP TABLE mcp_cleanup_jobs(vault_id uuid, worker_device_id uuid, created_at timestamptz, attempts integer, last_attempt_at timestamptz); \
         CREATE FUNCTION pg_temp.shadow_cleanup_job() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'temporary shadow trigger executed'; END $$; \
         CREATE TRIGGER shadow_cleanup_job BEFORE INSERT ON mcp_cleanup_jobs FOR EACH ROW EXECUTE FUNCTION pg_temp.shadow_cleanup_job();",
    )
    .execute(&mut *runtime)
    .await
    .unwrap();
    for (vault, statement, id) in [
        (
            device_vault,
            "DELETE FROM devices WHERE vault_id=$1 AND device_id=$2",
            device_id,
        ),
        (
            operation_vault,
            "DELETE FROM mcp_operations WHERE vault_id=$1",
            operation_owner,
        ),
        (
            vault_vault,
            "DELETE FROM vaults WHERE vault_id=$1",
            vault_device,
        ),
    ] {
        sqlx::query("SELECT set_config('peppy.vault_id', $1, true)")
            .bind(vault.to_string())
            .execute(&mut *runtime)
            .await
            .unwrap();
        if statement.contains("device_id") {
            sqlx::query(statement)
                .bind(vault)
                .bind(id)
                .execute(&mut *runtime)
                .await
                .unwrap();
        } else {
            if vault == vault_vault {
                sqlx::query("DELETE FROM devices WHERE vault_id=$1 AND device_id=$2")
                    .bind(vault)
                    .bind(id)
                    .execute(&mut *runtime)
                    .await
                    .unwrap();
            }
            sqlx::query(statement)
                .bind(vault)
                .execute(&mut *runtime)
                .await
                .unwrap();
        }
    }
    let shadow_rows: i64 = sqlx::query_scalar("SELECT count(*) FROM mcp_cleanup_jobs")
        .fetch_one(&mut *runtime)
        .await
        .unwrap();
    assert_eq!(shadow_rows, 0);
    runtime.commit().await.unwrap();

    let real_jobs: i64 =
        sqlx::query_scalar("SELECT count(*) FROM mcp_cleanup_jobs WHERE vault_id IN ($1,$2,$3)")
            .bind(device_vault)
            .bind(operation_vault)
            .bind(vault_vault)
            .fetch_one(&fixture.maintenance)
            .await
            .unwrap();
    assert_eq!(real_jobs, 3);
    fixture.cleanup().await;
}

#[tokio::test]
async fn startup_validation_rejects_grant_policy_and_execute_drift() {
    let fixture = Fixture::new().await;

    sqlx::query(&format!(
        "GRANT UPDATE ON vault_identity_establishments TO {}",
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
        "REVOKE INSERT ON vault_identity_establishments FROM {}",
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
        "GRANT INSERT ON vault_identity_establishments TO {}",
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
async fn live_ticket_admission_events_are_runtime_scoped_and_independent_of_ticket_retention() {
    let fixture = Fixture::new().await;
    let vault_a = Uuid::new_v4();
    let vault_b = Uuid::new_v4();
    let device_a = Uuid::new_v4();
    let device_b = Uuid::new_v4();
    fixture.run_owner(&vault_insert(vault_a)).await;
    fixture.run_owner(&vault_insert(vault_b)).await;
    fixture.run_owner(&device_insert(vault_a, device_a)).await;
    fixture.run_owner(&device_insert(vault_b, device_b)).await;

    rls::validate_runtime_role(&fixture.runtime, true)
        .await
        .unwrap();
    rls::validate_maintenance_role(&fixture.maintenance)
        .await
        .unwrap();

    let event_id = Uuid::new_v4();
    fixture
        .apply(
            Writer::Runtime,
            vault_a,
            &format!(
                "INSERT INTO live_ticket_admission_events(event_id, vault_id, device_id, event_kind, created_at) VALUES('{event_id}', '{vault_a}', '{device_a}', 'ticket_issue', clock_timestamp())"
            ),
        )
        .await
        .unwrap();
    assert!(fixture
        .apply(
            Writer::Runtime,
            vault_a,
            &format!(
                "INSERT INTO live_ticket_admission_events(event_id, vault_id, device_id, event_kind, created_at) VALUES('{}', '{vault_b}', '{device_b}', 'ticket_issue', clock_timestamp())",
                Uuid::new_v4()
            ),
        )
        .await
        .is_err());
    for sql in [
        format!(
            "INSERT INTO live_ticket_admission_events(event_id, vault_id, device_id, event_kind, created_at) VALUES('{}', '{vault_a}', '{device_a}', 'invalid', clock_timestamp())",
            Uuid::new_v4()
        ),
        format!(
            "INSERT INTO live_ticket_admission_events(event_id, vault_id, device_id, event_kind, created_at) VALUES('{}', '{vault_a}', '{}', 'ticket_issue', clock_timestamp())",
            Uuid::new_v4(),
            Uuid::new_v4()
        ),
        format!(
            "INSERT INTO live_ticket_admission_events(event_id, vault_id, device_id, event_kind) VALUES('{}', '{vault_a}', '{device_a}', 'ticket_issue')",
            Uuid::new_v4()
        ),
    ] {
        assert!(fixture.apply(Writer::Runtime, vault_a, &sql).await.is_err());
    }

    let session_hash = vec![7_u8; 32];
    fixture
        .run_owner(&format!(
            "INSERT INTO live_ticket_sessions(vault_id, device_id, session_id_hash, state, closed_at) VALUES('{vault_a}', '{device_a}', decode('{}', 'hex'), 'closed', now())",
            hex::encode(&session_hash)
        ))
        .await;
    fixture
        .run_owner(&format!(
            "INSERT INTO live_tickets(ticket_digest, vault_id, device_id, session_id_hash, issued_at, expires_at) VALUES(decode('{}', 'hex'), '{vault_a}', '{device_a}', decode('{}', 'hex'), now(), now()+interval '1 minute')",
            "01".repeat(32),
            hex::encode(&session_hash)
        ))
        .await;
    fixture
        .run_owner(&format!(
            "DELETE FROM live_tickets WHERE vault_id='{vault_a}'; DELETE FROM live_ticket_sessions WHERE vault_id='{vault_a}'"
        ))
        .await;
    let retained: i64 =
        sqlx::query_scalar("SELECT count(*) FROM live_ticket_admission_events WHERE event_id=$1")
            .bind(event_id)
            .fetch_one(&fixture.migration)
            .await
            .unwrap();
    assert_eq!(retained, 1);

    let mut other_vault = fixture.runtime.begin().await.unwrap();
    sqlx::query("SELECT set_config('peppy.vault_id', $1, true)")
        .bind(vault_b.to_string())
        .execute(&mut *other_vault)
        .await
        .unwrap();
    let visible: i64 = sqlx::query_scalar("SELECT count(*) FROM live_ticket_admission_events")
        .fetch_one(&mut *other_vault)
        .await
        .unwrap();
    assert_eq!(visible, 0);
    other_vault.commit().await.unwrap();
    for privilege in ["SELECT", "INSERT"] {
        sqlx::raw_sql(&format!(
            "REVOKE {privilege} ON TABLE live_ticket_admission_events FROM {}",
            fixture.runtime_role
        ))
        .execute(&fixture.migration)
        .await
        .unwrap();
        assert!(
            rls::validate_runtime_role(&fixture.runtime, false)
                .await
                .is_err()
        );
        assert!(
            rls::validate_runtime_role(&fixture.runtime, true)
                .await
                .is_err()
        );
        sqlx::raw_sql(&format!(
            "GRANT {privilege} ON TABLE live_ticket_admission_events TO {}",
            fixture.runtime_role
        ))
        .execute(&fixture.migration)
        .await
        .unwrap();
    }
    fixture.cleanup().await;
}

#[tokio::test]
async fn live_ticket_admission_event_migration_round_trips() {
    let fixture = Fixture::new().await;
    sqlx::raw_sql(include_str!(
        "../migrations/down/0033_live_ticket_admission_events.down.sql"
    ))
    .execute(&fixture.migration)
    .await
    .unwrap();
    let absent: Option<String> =
        sqlx::query_scalar("SELECT to_regclass('live_ticket_admission_events')::text")
            .fetch_one(&fixture.migration)
            .await
            .unwrap();
    assert!(absent.is_none());
    sqlx::raw_sql(include_str!(
        "../migrations/0033_live_ticket_admission_events.sql"
    ))
    .execute(&fixture.migration)
    .await
    .unwrap();
    let restored: Option<String> =
        sqlx::query_scalar("SELECT to_regclass('live_ticket_admission_events')::text")
            .fetch_one(&fixture.migration)
            .await
            .unwrap();
    assert_eq!(restored.as_deref(), Some("live_ticket_admission_events"));
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
async fn column_privilege_drift_is_detected_and_repaired_by_grants() {
    let fixture = Fixture::new().await;

    sqlx::query(&format!(
        "GRANT UPDATE (version) ON membership_versions TO {}",
        fixture.runtime_role
    ))
    .execute(&fixture.migration)
    .await
    .unwrap();
    assert!(matches!(
        rls::validate_runtime_role(&fixture.runtime, true).await,
        Err(rls::RlsError::UnsafeRuntimeRole(
            "membership version grant is missing or unsafe"
        ))
    ));
    rls::grant_runtime_privileges(&fixture.migration, &fixture.runtime_role)
        .await
        .unwrap();
    rls::validate_runtime_role(&fixture.runtime, true)
        .await
        .unwrap();

    sqlx::query(&format!(
        "GRANT UPDATE (key_epoch) ON vault_identity_establishments TO {}",
        fixture.runtime_role
    ))
    .execute(&fixture.migration)
    .await
    .unwrap();
    assert!(matches!(
        rls::validate_runtime_role(&fixture.runtime, true).await,
        Err(rls::RlsError::UnsafeRuntimeRole(
            "vault identity grant is missing or unsafe"
        ))
    ));
    rls::grant_runtime_privileges(&fixture.migration, &fixture.runtime_role)
        .await
        .unwrap();
    rls::validate_runtime_role(&fixture.runtime, true)
        .await
        .unwrap();

    sqlx::query(&format!(
        "GRANT UPDATE (checksum) ON _sqlx_migrations TO {}",
        fixture.runtime_role
    ))
    .execute(&fixture.migration)
    .await
    .unwrap();
    assert!(matches!(
        rls::validate_runtime_role(&fixture.runtime, true).await,
        Err(rls::RlsError::UnsafeRuntimeRole(
            "schema metadata grant is unsafe"
        ))
    ));
    rls::grant_runtime_privileges(&fixture.migration, &fixture.runtime_role)
        .await
        .unwrap();
    rls::validate_runtime_role(&fixture.runtime, true)
        .await
        .unwrap();

    sqlx::query(&format!(
        "GRANT SELECT (vault_id) ON mcp_cleanup_jobs TO {}",
        fixture.runtime_role
    ))
    .execute(&fixture.migration)
    .await
    .unwrap();
    assert!(matches!(
        rls::validate_runtime_role(&fixture.runtime, true).await,
        Err(rls::RlsError::UnsafeRuntimeRole(
            "unexpected public-table privilege"
        ))
    ));
    sqlx::query(&format!(
        "REVOKE SELECT (vault_id) ON mcp_cleanup_jobs FROM {}",
        fixture.runtime_role
    ))
    .execute(&fixture.migration)
    .await
    .unwrap();

    sqlx::query(&format!(
        "GRANT INSERT (recovery_record) ON vault_identity_establishments TO {}",
        fixture.maintenance_role
    ))
    .execute(&fixture.migration)
    .await
    .unwrap();
    assert!(matches!(
        rls::validate_maintenance_role(&fixture.maintenance).await,
        Err(rls::RlsError::UnsafeMaintenanceRole(
            "vault identity grant is unsafe"
        ))
    ));
    rls::grant_maintenance_privileges(&fixture.migration, &fixture.maintenance_role)
        .await
        .unwrap();
    rls::validate_maintenance_role(&fixture.maintenance)
        .await
        .unwrap();

    sqlx::query(&format!(
        "GRANT UPDATE (version) ON membership_versions TO {}",
        fixture.maintenance_role
    ))
    .execute(&fixture.migration)
    .await
    .unwrap();
    assert!(matches!(
        rls::validate_maintenance_role(&fixture.maintenance).await,
        Err(rls::RlsError::UnsafeMaintenanceRole(
            "membership version grant is unsafe"
        ))
    ));
    rls::grant_maintenance_privileges(&fixture.migration, &fixture.maintenance_role)
        .await
        .unwrap();
    rls::validate_maintenance_role(&fixture.maintenance)
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

#[derive(Clone, Copy)]
enum Writer {
    Owner,
    Runtime,
}

struct MembershipCase {
    label: &'static str,
    writer: Writer,
    sql: String,
    bump: i64,
}

fn vault_insert(vault: Uuid) -> String {
    format!(
        "INSERT INTO vaults(vault_id,public_key_profile,encrypted_vault_check_header,key_epoch,profile_fingerprint) \
         VALUES('{vault}','{{}}',decode('01','hex'),1,repeat('f',64))"
    )
}

fn device_insert(vault: Uuid, device: Uuid) -> String {
    format!(
        "INSERT INTO devices(vault_id,device_id,role,public_key,profile_fingerprint,key_epoch) \
         VALUES('{vault}','{device}','device','{{}}',repeat('f',64),1)"
    )
}

async fn token_in(connection: &mut PgConnection, vault: Uuid) -> i64 {
    sqlx::query_scalar("SELECT version FROM membership_versions WHERE vault_id=$1")
        .bind(vault)
        .fetch_one(connection)
        .await
        .unwrap()
}

impl Fixture {
    fn writer(&self, writer: Writer) -> &PgPool {
        match writer {
            Writer::Owner => &self.migration,
            Writer::Runtime => &self.runtime,
        }
    }

    async fn run_owner(&self, sql: &str) {
        sqlx::raw_sql(sql).execute(&self.migration).await.unwrap();
    }

    async fn token(&self, vault: Uuid) -> i64 {
        let mut connection = self.migration.acquire().await.unwrap();
        token_in(&mut connection, vault).await
    }

    /// Commits `sql` as `writer`, scoped to `vault` the way request handlers are.
    async fn apply(&self, writer: Writer, vault: Uuid, sql: &str) -> Result<(), sqlx::Error> {
        let mut transaction = self.writer(writer).begin().await?;
        sqlx::query("SELECT set_config('peppy.vault_id', $1, true)")
            .bind(vault.to_string())
            .execute(&mut *transaction)
            .await?;
        sqlx::raw_sql(sql).execute(&mut *transaction).await?;
        transaction.commit().await
    }

    /// Runs `sql` as the owner with the named triggers dropped, returns the token delta, then rolls back.
    async fn token_delta_without(&self, vault: Uuid, dropped: &[(&str, &str)], sql: &str) -> i64 {
        let mut transaction = self.migration.begin().await.unwrap();
        for (trigger, table) in dropped {
            sqlx::raw_sql(&format!("DROP TRIGGER {trigger} ON {table}"))
                .execute(&mut *transaction)
                .await
                .unwrap();
        }
        let before = token_in(&mut transaction, vault).await;
        sqlx::raw_sql(sql).execute(&mut *transaction).await.unwrap();
        let after = token_in(&mut transaction, vault).await;
        transaction.rollback().await.unwrap();
        after - before
    }

    /// Each mutation must bump the token exactly as declared, and must not bump
    /// once its protected trigger is absent. Discrimination runs before the
    /// committed run so it never observes a state the commit has already changed.
    async fn assert_protected(
        &self,
        vault: Uuid,
        dropped: &[(&str, &str)],
        cases: &[MembershipCase],
    ) {
        for case in cases {
            assert_eq!(
                self.token_delta_without(vault, dropped, &case.sql).await,
                0,
                "{}: token must not move without the protected trigger",
                case.label
            );
            let before = self.token(vault).await;
            self.apply(case.writer, vault, &case.sql)
                .await
                .unwrap_or_else(|error| panic!("{}: {error}", case.label));
            assert_eq!(
                self.token(vault).await - before,
                case.bump,
                "{}: token delta",
                case.label
            );
        }
    }

    async fn assert_unchanged(&self, vault: Uuid, writer: Writer, label: &str, sql: &str) {
        let before = self.token(vault).await;
        self.apply(writer, vault, sql)
            .await
            .unwrap_or_else(|error| panic!("{label}: {error}"));
        assert_eq!(self.token(vault).await, before, "{label}");
    }
}

#[tokio::test]
async fn membership_vault_triggers_bump_and_discriminate() {
    let fixture = Fixture::new().await;
    let vault = Uuid::new_v4();

    let mut transaction = fixture.migration.begin().await.unwrap();
    sqlx::raw_sql("DROP TRIGGER vaults_initialize_membership_version ON vaults")
        .execute(&mut *transaction)
        .await
        .unwrap();
    sqlx::raw_sql(&vault_insert(vault))
        .execute(&mut *transaction)
        .await
        .unwrap();
    let rows: i64 =
        sqlx::query_scalar("SELECT count(*) FROM membership_versions WHERE vault_id=$1")
            .bind(vault)
            .fetch_one(&mut *transaction)
            .await
            .unwrap();
    transaction.rollback().await.unwrap();
    assert_eq!(rows, 0, "without the initializer no token row is created");

    fixture.run_owner(&vault_insert(vault)).await;
    assert_eq!(
        fixture.token(vault).await,
        1,
        "vault creation initializes the token"
    );

    fixture
        .assert_protected(
            vault,
            &[("vaults_invalidate_membership_version", "vaults")],
            &[
                MembershipCase {
                    label: "vault profile fingerprint",
                    writer: Writer::Owner,
                    sql: format!(
                        "UPDATE vaults SET profile_fingerprint=repeat('e',64) WHERE vault_id='{vault}'"
                    ),
                    bump: 1,
                },
                MembershipCase {
                    label: "vault check header",
                    writer: Writer::Owner,
                    sql: format!(
                        "UPDATE vaults SET encrypted_vault_check_header=decode('02','hex') WHERE vault_id='{vault}'"
                    ),
                    bump: 1,
                },
                MembershipCase {
                    label: "vault public key profile",
                    writer: Writer::Owner,
                    sql: format!(
                        "UPDATE vaults SET public_key_profile='{{\"epoch\":2}}' WHERE vault_id='{vault}'"
                    ),
                    bump: 1,
                },
                MembershipCase {
                    label: "vault key epoch",
                    writer: Writer::Owner,
                    sql: format!("UPDATE vaults SET key_epoch=2 WHERE vault_id='{vault}'"),
                    bump: 1,
                },
            ],
        )
        .await;
    fixture.cleanup().await;
}

#[tokio::test]
async fn membership_device_triggers_bump_and_discriminate() {
    let fixture = Fixture::new().await;
    let vault = Uuid::new_v4();
    let device = Uuid::new_v4();
    let added = Uuid::new_v4();
    let removed = Uuid::new_v4();
    fixture.run_owner(&vault_insert(vault)).await;
    fixture.run_owner(&device_insert(vault, device)).await;
    fixture.run_owner(&device_insert(vault, removed)).await;

    fixture
        .assert_protected(
            vault,
            &[("devices_invalidate_membership_version", "devices")],
            &[
                MembershipCase {
                    label: "device insert",
                    writer: Writer::Runtime,
                    sql: device_insert(vault, added),
                    bump: 1,
                },
                MembershipCase {
                    label: "device role",
                    writer: Writer::Runtime,
                    sql: format!(
                        "UPDATE devices SET role='gateway' WHERE vault_id='{vault}' AND device_id='{device}'"
                    ),
                    bump: 1,
                },
                MembershipCase {
                    label: "device profile fingerprint",
                    writer: Writer::Runtime,
                    sql: format!(
                        "UPDATE devices SET profile_fingerprint=repeat('e',64) WHERE vault_id='{vault}' AND device_id='{device}'"
                    ),
                    bump: 1,
                },
                MembershipCase {
                    label: "device revocation",
                    writer: Writer::Runtime,
                    sql: format!(
                        "UPDATE devices SET revoked_at=clock_timestamp() WHERE vault_id='{vault}' AND device_id='{added}'"
                    ),
                    bump: 1,
                },
                MembershipCase {
                    label: "device delete",
                    writer: Writer::Runtime,
                    sql: format!(
                        "DELETE FROM devices WHERE vault_id='{vault}' AND device_id='{removed}'"
                    ),
                    bump: 1,
                },
            ],
        )
        .await;
    fixture.cleanup().await;
}

#[tokio::test]
async fn membership_device_credential_triggers_bump_and_discriminate() {
    let fixture = Fixture::new().await;
    let vault = Uuid::new_v4();
    let device = Uuid::new_v4();
    fixture.run_owner(&vault_insert(vault)).await;
    fixture.run_owner(&device_insert(vault, device)).await;

    fixture
        .assert_protected(
            vault,
            &[(
                "device_credentials_invalidate_membership_version",
                "device_credentials",
            )],
            &[
                MembershipCase {
                    label: "credential insert",
                    writer: Writer::Runtime,
                    sql: format!(
                        "INSERT INTO device_credentials(token_digest,vault_id,device_id) VALUES(decode(repeat('02',32),'hex'),'{vault}','{device}')"
                    ),
                    bump: 1,
                },
                MembershipCase {
                    label: "credential digest rotation",
                    writer: Writer::Runtime,
                    sql: format!(
                        "UPDATE device_credentials SET token_digest=decode(repeat('03',32),'hex') WHERE vault_id='{vault}' AND token_digest=decode(repeat('02',32),'hex')"
                    ),
                    bump: 1,
                },
                MembershipCase {
                    label: "credential revocation",
                    writer: Writer::Runtime,
                    sql: format!(
                        "UPDATE device_credentials SET revoked_at=clock_timestamp() WHERE vault_id='{vault}' AND token_digest=decode(repeat('03',32),'hex')"
                    ),
                    bump: 1,
                },
                MembershipCase {
                    label: "credential delete",
                    writer: Writer::Runtime,
                    sql: format!(
                        "DELETE FROM device_credentials WHERE vault_id='{vault}' AND token_digest=decode(repeat('03',32),'hex')"
                    ),
                    bump: 1,
                },
            ],
        )
        .await;
    fixture.cleanup().await;
}

#[tokio::test]
async fn membership_key_profile_triggers_bump_and_discriminate() {
    let fixture = Fixture::new().await;
    let vault = Uuid::new_v4();
    fixture.run_owner(&vault_insert(vault)).await;

    let profile_insert = format!(
        "INSERT INTO vault_key_profiles(vault_id,key_epoch,public_key_profile,encrypted_vault_check_header,profile_fingerprint) \
         VALUES('{vault}',2,'{{}}',decode('03','hex'),repeat('e',64))"
    );
    fixture
        .assert_protected(
            vault,
            &[(
                "vault_key_profiles_invalidate_membership_version",
                "vault_key_profiles",
            )],
            &[MembershipCase {
                label: "key profile insert",
                writer: Writer::Owner,
                sql: profile_insert,
                bump: 1,
            }],
        )
        .await;

    // The immutability guard rejects profile deletion; the membership trigger
    // must still bump on delete if that guard is ever absent.
    let profile_delete =
        format!("DELETE FROM vault_key_profiles WHERE vault_id='{vault}' AND key_epoch=2");
    let rejected = fixture
        .apply(Writer::Owner, vault, &profile_delete)
        .await
        .expect_err("guarded profile delete must fail");
    assert!(rejected.to_string().contains("immutable"), "{rejected}");
    let guard = ("vault_key_profiles_immutable", "vault_key_profiles");
    let membership = (
        "vault_key_profiles_invalidate_membership_version",
        "vault_key_profiles",
    );
    assert_eq!(
        fixture
            .token_delta_without(vault, &[guard], &profile_delete)
            .await,
        1,
        "profile delete bumps the token when only the guard is absent"
    );
    assert_eq!(
        fixture
            .token_delta_without(vault, &[guard, membership], &profile_delete)
            .await,
        0,
        "profile delete does not bump the token without the membership trigger"
    );
    assert_eq!(
        fixture.token(vault).await,
        2,
        "rejected delete leaves the token unchanged"
    );
    fixture.cleanup().await;
}

#[tokio::test]
async fn membership_admission_key_triggers_bump_and_discriminate() {
    let fixture = Fixture::new().await;
    let vault = Uuid::new_v4();
    fixture.run_owner(&vault_insert(vault)).await;

    fixture
        .assert_protected(
            vault,
            &[(
                "vault_admission_keys_invalidate_membership_version",
                "vault_admission_keys",
            )],
            &[
                MembershipCase {
                    label: "admission key insert",
                    writer: Writer::Runtime,
                    sql: format!(
                        "INSERT INTO vault_admission_keys(vault_id,key_epoch,profile_fingerprint,admission_public_key) VALUES('{vault}',1,repeat('f',64),decode(repeat('02',32),'hex'))"
                    ),
                    bump: 1,
                },
                MembershipCase {
                    label: "admission verifier update",
                    writer: Writer::Runtime,
                    sql: format!(
                        "UPDATE vault_admission_keys SET admission_public_key=decode(repeat('03',32),'hex') WHERE vault_id='{vault}' AND key_epoch=1"
                    ),
                    bump: 1,
                },
                MembershipCase {
                    label: "admission key epoch rebind",
                    writer: Writer::Runtime,
                    sql: format!(
                        "UPDATE vault_admission_keys SET key_epoch=2 WHERE vault_id='{vault}' AND key_epoch=1"
                    ),
                    bump: 1,
                },
                MembershipCase {
                    label: "admission profile fingerprint rebind",
                    writer: Writer::Runtime,
                    sql: format!(
                        "UPDATE vault_admission_keys SET profile_fingerprint=repeat('e',64) WHERE vault_id='{vault}' AND key_epoch=2"
                    ),
                    bump: 1,
                },
                MembershipCase {
                    label: "admission key delete",
                    writer: Writer::Runtime,
                    sql: format!(
                        "DELETE FROM vault_admission_keys WHERE vault_id='{vault}' AND key_epoch=2"
                    ),
                    bump: 1,
                },
            ],
        )
        .await;
    fixture.cleanup().await;
}

#[tokio::test]
async fn membership_excluded_mutations_leave_token_unchanged() {
    let fixture = Fixture::new().await;
    let vault = Uuid::new_v4();
    let device = Uuid::new_v4();
    let phone = Uuid::new_v4();
    let computer = Uuid::new_v4();
    let recipient = Uuid::new_v4();
    fixture.run_owner(&vault_insert(vault)).await;
    fixture.run_owner(&device_insert(vault, device)).await;
    fixture
        .run_owner(&format!(
            "INSERT INTO vault_key_profiles(vault_id,key_epoch,public_key_profile,encrypted_vault_check_header,profile_fingerprint) \
             VALUES('{vault}',1,'{{}}',decode('01','hex'),repeat('f',64))"
        ))
        .await;
    let before = fixture.token(vault).await;

    fixture
        .assert_unchanged(
            vault,
            Writer::Runtime,
            "device display name",
            &format!(
                "UPDATE devices SET name='renamed' WHERE vault_id='{vault}' AND device_id='{device}'"
            ),
        )
        .await;
    fixture
        .assert_unchanged(
            vault,
            Writer::Runtime,
            "device presence insert",
            &format!(
                "INSERT INTO device_presence(vault_id,device_id,last_seen_at) VALUES('{vault}','{device}',clock_timestamp())"
            ),
        )
        .await;
    fixture
        .assert_unchanged(
            vault,
            Writer::Runtime,
            "device presence update",
            &format!(
                "UPDATE device_presence SET last_seen_at=clock_timestamp() WHERE vault_id='{vault}' AND device_id='{device}'"
            ),
        )
        .await;
    fixture
        .assert_unchanged(
            vault,
            Writer::Runtime,
            "waiting phone invitation create",
            &format!(
                "INSERT INTO phone_invitations(id,vault_id,owner_device_id,origin,method,token_digest,owner_transfer_public_key,profile_fingerprint,key_epoch,state,expires_at) \
                 VALUES('{phone}','{vault}','{device}','https://peppy.test','transfer',decode(repeat('0a',32),'hex'),decode(repeat('0b',32),'hex'),repeat('f',64),1,'waiting',clock_timestamp()+interval '1 hour')"
            ),
        )
        .await;
    fixture
        .assert_unchanged(
            vault,
            Writer::Runtime,
            "waiting phone invitation cleanup",
            &format!("DELETE FROM phone_invitations WHERE vault_id='{vault}' AND id='{phone}'"),
        )
        .await;
    fixture
        .assert_unchanged(
            vault,
            Writer::Runtime,
            "waiting computer enrollment create",
            &format!(
                "INSERT INTO computer_enrollments(id,vault_id,owner_device_id,origin,claim_token_digest,server_nonce,recipient_device_id,recipient_kind,recipient_name,recipient_signing_public_key,recipient_transfer_public_key,recipient_credential_digest,recipient_commitment,profile_fingerprint,key_epoch,state,expires_at) \
                 VALUES('{computer}','{vault}','{device}','https://peppy.test',decode(repeat('0c',32),'hex'),decode(repeat('0d',32),'hex'),'{recipient}','browser','Laptop',decode(repeat('0e',32),'hex'),decode(repeat('0f',32),'hex'),decode(repeat('10',32),'hex'),decode(repeat('11',32),'hex'),repeat('f',64),1,'waiting',clock_timestamp()+interval '1 hour')"
            ),
        )
        .await;
    fixture
        .assert_unchanged(
            vault,
            Writer::Runtime,
            "waiting computer enrollment cleanup",
            &format!(
                "DELETE FROM computer_enrollments WHERE vault_id='{vault}' AND id='{computer}'"
            ),
        )
        .await;
    fixture
        .assert_unchanged(
            vault,
            Writer::Owner,
            "key profile activation",
            &format!(
                "UPDATE vault_key_profiles SET activated_at=clock_timestamp() WHERE vault_id='{vault}' AND key_epoch=1"
            ),
        )
        .await;
    fixture
        .assert_unchanged(
            vault,
            Writer::Owner,
            "vault no-op epoch assignment",
            &format!("UPDATE vaults SET key_epoch=key_epoch WHERE vault_id='{vault}'"),
        )
        .await;
    assert_eq!(
        fixture.token(vault).await,
        before,
        "excluded mutations never move the token"
    );
    fixture.cleanup().await;
}

#[tokio::test]
async fn membership_mutations_fail_closed_without_token_row_or_counter() {
    let fixture = Fixture::new().await;
    let vault = Uuid::new_v4();
    fixture.run_owner(&vault_insert(vault)).await;
    fixture
        .run_owner(&device_insert(vault, Uuid::new_v4()))
        .await;
    let token_before = fixture.token(vault).await;

    let device_mutation = device_insert(vault, Uuid::new_v4());
    let vault_mutation = format!("UPDATE vaults SET key_epoch=2 WHERE vault_id='{vault}'");
    let preludes = [
        (
            "missing token row",
            format!("DELETE FROM membership_versions WHERE vault_id='{vault}'"),
        ),
        (
            "exhausted token",
            format!(
                "UPDATE membership_versions SET version=9223372036854775807 WHERE vault_id='{vault}'"
            ),
        ),
    ];
    for (label, prelude) in &preludes {
        for mutation in [&device_mutation, &vault_mutation] {
            let mut transaction = fixture.migration.begin().await.unwrap();
            sqlx::raw_sql(prelude)
                .execute(&mut *transaction)
                .await
                .unwrap();
            let error = sqlx::raw_sql(mutation)
                .execute(&mut *transaction)
                .await
                .expect_err(label);
            assert!(
                error
                    .to_string()
                    .contains("membership version missing or exhausted"),
                "{label}: {error}"
            );
            transaction.rollback().await.unwrap();
        }
    }
    assert_eq!(
        fixture.token(vault).await,
        token_before,
        "failed mutations leave the committed token unchanged"
    );
    fixture.cleanup().await;
}

#[tokio::test]
async fn membership_vault_delete_cascades_children_without_token_errors() {
    let fixture = Fixture::new().await;
    let vault = Uuid::new_v4();
    fixture.run_owner(&vault_insert(vault)).await;
    fixture
        .run_owner(&format!(
            "INSERT INTO vault_admission_keys(vault_id,key_epoch,profile_fingerprint,admission_public_key) \
             VALUES('{vault}',1,repeat('f',64),decode(repeat('02',32),'hex'))"
        ))
        .await;
    assert_eq!(
        fixture.token(vault).await,
        2,
        "admission child bumped before cascade"
    );

    fixture
        .run_owner(&format!("DELETE FROM vaults WHERE vault_id='{vault}'"))
        .await;

    let remaining: i64 = sqlx::query_scalar(
        "SELECT (SELECT count(*) FROM membership_versions WHERE vault_id=$1) \
              + (SELECT count(*) FROM vault_admission_keys WHERE vault_id=$1)",
    )
    .bind(vault)
    .fetch_one(&fixture.migration)
    .await
    .unwrap();
    assert_eq!(
        remaining, 0,
        "vault delete cascades token and admission children"
    );
    fixture.cleanup().await;
}

#[tokio::test]
async fn membership_cross_vault_device_move_invalidates_both_vaults() {
    let fixture = Fixture::new().await;
    let source = Uuid::new_v4();
    let destination = Uuid::new_v4();
    let device = Uuid::new_v4();
    fixture.run_owner(&vault_insert(source)).await;
    fixture.run_owner(&vault_insert(destination)).await;
    fixture.run_owner(&device_insert(source, device)).await;
    // The device insert trigger creates a retention dependent that blocks the move.
    fixture
        .run_owner(&format!(
            "DELETE FROM attachment_retention_devices WHERE vault_id='{source}' AND device_id='{device}'"
        ))
        .await;
    let source_before = fixture.token(source).await;
    let destination_before = fixture.token(destination).await;

    fixture
        .run_owner(&format!(
            "UPDATE devices SET vault_id='{destination}' WHERE vault_id='{source}' AND device_id='{device}'"
        ))
        .await;

    assert_eq!(
        fixture.token(source).await - source_before,
        1,
        "source loses the device"
    );
    assert_eq!(
        fixture.token(destination).await - destination_before,
        1,
        "destination gains the device"
    );
    fixture.cleanup().await;
}

#[tokio::test]
async fn membership_token_is_tenant_scoped_and_not_publicly_executable() {
    let fixture = Fixture::new().await;
    let vault = Uuid::new_v4();
    let other = Uuid::new_v4();
    fixture.run_owner(&vault_insert(vault)).await;
    fixture.run_owner(&vault_insert(other)).await;

    let mut transaction = fixture.runtime.begin().await.unwrap();
    sqlx::query("SELECT set_config('peppy.vault_id', $1, true)")
        .bind(vault.to_string())
        .execute(&mut *transaction)
        .await
        .unwrap();
    let own: i64 = sqlx::query_scalar("SELECT count(*) FROM membership_versions WHERE vault_id=$1")
        .bind(vault)
        .fetch_one(&mut *transaction)
        .await
        .unwrap();
    let foreign: i64 =
        sqlx::query_scalar("SELECT count(*) FROM membership_versions WHERE vault_id=$1")
            .bind(other)
            .fetch_one(&mut *transaction)
            .await
            .unwrap();
    transaction.rollback().await.unwrap();
    assert_eq!(own, 1, "runtime reads its own token");
    assert_eq!(foreign, 0, "runtime cannot read another vault's token");

    let executable: bool = sqlx::query_scalar(
        "SELECT has_function_privilege('public', 'membership_version_invalidate()', 'EXECUTE') \
             OR has_function_privilege('public', 'membership_version_initialize()', 'EXECUTE') \
             OR has_function_privilege($1, 'membership_version_invalidate()', 'EXECUTE') \
             OR has_function_privilege($1, 'membership_version_initialize()', 'EXECUTE')",
    )
    .bind(&fixture.runtime_role)
    .fetch_one(&fixture.migration)
    .await
    .unwrap();
    assert!(
        !executable,
        "token trigger functions are not directly executable by PUBLIC or runtime"
    );
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

#[tokio::test]
async fn membership_exchange_uses_transaction_local_scope_with_runtime_role() {
    use futures_util::{SinkExt, StreamExt};
    use peppy_protocol::{MEMBERSHIP_PROTOCOL_VERSION, MembershipChallengeV1, MembershipControlV1};
    use tokio::net::TcpListener;
    use tokio_tungstenite::{
        connect_async,
        tungstenite::{Message, client::IntoClientRequest},
    };

    async fn text(
        socket: &mut tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
    ) -> String {
        loop {
            match socket.next().await.unwrap().unwrap() {
                Message::Text(value) => return value.to_string(),
                Message::Ping(value) => socket.send(Message::Pong(value)).await.unwrap(),
                Message::Pong(_) => {}
                frame => panic!("expected text frame, got {frame:?}"),
            }
        }
    }

    let fixture = Fixture::new().await;
    let vault = Uuid::new_v4();
    let profile = KeyProfile::new(vault, 1).unwrap();
    let owner = create_owner(
        &fixture.migration,
        serde_json::to_value(&profile).unwrap(),
        vec![9_u8; 48],
        profile.fingerprint().unwrap(),
        1,
    )
    .await
    .unwrap();
    let foreign_vault = Uuid::new_v4();
    let foreign_profile = KeyProfile::new(foreign_vault, 1).unwrap();
    let foreign = create_owner(
        &fixture.migration,
        serde_json::to_value(&foreign_profile).unwrap(),
        vec![8_u8; 48],
        foreign_profile.fingerprint().unwrap(),
        1,
    )
    .await
    .unwrap();

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = fixture.app().await;
    let task = tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .unwrap();
    });
    let connect = async {
        loop {
            let mut request = format!("ws://{address}/v1/live")
                .into_client_request()
                .unwrap();
            request.headers_mut().insert(
                "authorization",
                format!("Bearer {}", owner.device_token).parse().unwrap(),
            );
            request.headers_mut().insert(
                "x-peppy-live-capabilities",
                "membership-version-v1".parse().unwrap(),
            );
            match connect_async(request).await {
                Ok(connected) => break connected,
                Err(tokio_tungstenite::tungstenite::Error::Http(response))
                    if response.status() == StatusCode::SERVICE_UNAVAILABLE
                        && response.body().as_deref()
                            == Some(br#"{"code":"live_unavailable"}"#) =>
                {
                    tokio::task::yield_now().await;
                }
                Err(error) => panic!("live websocket upgrade failed: {error:?}"),
            }
        }
    };
    let (mut socket, _) = tokio::time::timeout(Duration::from_secs(5), connect)
        .await
        .expect("PostgreSQL live backend did not become ready");
    let hello: Value = serde_json::from_str(&text(&mut socket).await).unwrap();
    assert_eq!(hello["type"], "hello");
    let MembershipControlV1::Bootstrap(bootstrap) =
        MembershipControlV1::parse(text(&mut socket).await.as_bytes()).unwrap()
    else {
        panic!("runtime role did not return membership bootstrap");
    };
    assert!(
        bootstrap
            .roster
            .iter()
            .any(|device| device.device_id.0 == owner.device_id)
    );
    assert!(
        !bootstrap
            .roster
            .iter()
            .any(|device| device.device_id.0 == foreign.device_id)
    );

    let challenge = MembershipControlV1::Challenge(MembershipChallengeV1 {
        r#type: "membership_challenge".into(),
        version: MEMBERSHIP_PROTOCOL_VERSION,
        connection_id: bootstrap.connection_id,
        challenge_id: URL_SAFE_NO_PAD.encode([77_u8; 16]),
    });
    socket
        .send(Message::Text(
            serde_json::to_string(&challenge).unwrap().into(),
        ))
        .await
        .unwrap();
    let MembershipControlV1::Response(response) =
        MembershipControlV1::parse(text(&mut socket).await.as_bytes()).unwrap()
    else {
        panic!("runtime role did not return membership response");
    };
    assert!(
        response
            .roster
            .iter()
            .any(|device| device.device_id.0 == owner.device_id)
    );
    assert!(
        !response
            .roster
            .iter()
            .any(|device| device.device_id.0 == foreign.device_id)
    );

    task.abort();
    let _ = task.await;
    fixture.cleanup().await;
}
