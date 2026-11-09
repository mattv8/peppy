use base64::{
    Engine as _,
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
};
use futures_util::TryStreamExt;
use peppy_crypto::{
    KeyProfile, create_vault_check_header, derive_root_key,
    vault_identity::{
        VaultIdentityAnchor, VaultIdentityRecoveryRecord, derive_vault_identity_recovery_key,
    },
};
use peppy_protocol::VAULT_IDENTITY_PROTOCOL_VERSION;
use peppy_server::api::{TransportOptions, create_owner, router_with_options};
use reqwest::{Client, StatusCode};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{
    PgPool,
    postgres::{PgPoolCopyExt, PgPoolOptions},
};
use std::time::Duration;
use tokio::{net::TcpListener, task::JoinHandle};
use uuid::Uuid;

const PHRASE: &str = "correct horse battery staple";

struct TestVault {
    vault: Uuid,
    owner: Uuid,
    token: String,
    profile: KeyProfile,
}

impl TestVault {
    fn request(&self, operation: Uuid) -> Value {
        identity_request(self.vault, self.owner, &self.profile, operation)
    }
}

struct Server {
    admin: PgPool,
    pool: PgPool,
    schema: String,
    scoped_url: String,
    application_name: String,
    base_url: String,
    task: JoinHandle<()>,
    vault: Uuid,
    owner: Uuid,
    token: String,
    profile: KeyProfile,
}

impl Server {
    async fn start() -> Self {
        let database_url =
            std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL is required");
        let admin = PgPoolOptions::new()
            .max_connections(2)
            .connect(&database_url)
            .await
            .unwrap();
        let schema = format!("vault_identity_test_{}", Uuid::new_v4().simple());
        sqlx::query(&format!("CREATE SCHEMA {schema}"))
            .execute(&admin)
            .await
            .unwrap();
        let application_name = format!("vault_identity_{}", Uuid::new_v4().simple());
        let mut url: url::Url = database_url.parse().unwrap();
        url.query_pairs_mut()
            .append_pair("options", &format!("-csearch_path={schema}"))
            .append_pair("application_name", &application_name);
        let pool = PgPoolOptions::new()
            .max_connections(12)
            .connect(url.as_str())
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        let vault = Uuid::new_v4();
        let profile = KeyProfile::new(vault, 1).unwrap();
        let root = derive_root_key(PHRASE, &profile).unwrap();
        let header = create_vault_check_header(&root, profile.clone()).unwrap();
        let owner = create_owner(
            &pool,
            serde_json::to_value(&profile).unwrap(),
            serde_json::to_vec(&header).unwrap(),
            profile.fingerprint().unwrap(),
            1,
        )
        .await
        .unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn({
            let pool = pool.clone();
            async move {
                axum::serve(
                    listener,
                    router_with_options(pool, TransportOptions::default()),
                )
                .await
                .unwrap();
            }
        });
        Self {
            admin,
            pool,
            schema,
            scoped_url: url.to_string(),
            application_name,
            base_url: format!("http://{address}"),
            task,
            vault,
            owner: owner.device_id,
            token: owner.device_token,
            profile,
        }
    }

    fn request(&self, operation: Uuid) -> Value {
        identity_request(self.vault, self.owner, &self.profile, operation)
    }

    async fn create_vault(&self) -> TestVault {
        let vault = Uuid::new_v4();
        let profile = KeyProfile::new(vault, 1).unwrap();
        let root = derive_root_key(PHRASE, &profile).unwrap();
        let header = create_vault_check_header(&root, profile.clone()).unwrap();
        let owner = create_owner(
            &self.pool,
            serde_json::to_value(&profile).unwrap(),
            serde_json::to_vec(&header).unwrap(),
            profile.fingerprint().unwrap(),
            1,
        )
        .await
        .unwrap();
        TestVault {
            vault,
            owner: owner.device_id,
            token: owner.device_token,
            profile,
        }
    }

    fn auth(&self, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        request.bearer_auth(&self.token)
    }
    async fn post(&self, token: &str, value: &Value) -> reqwest::Response {
        Client::new()
            .post(format!("{}/v1/vault/identity", self.base_url))
            .bearer_auth(token)
            .json(value)
            .send()
            .await
            .unwrap()
    }
    async fn shutdown(self) {
        self.task.abort();
        let _ = self.task.await;
        self.pool.close().await;
        terminate_backends(&self.admin, &self.application_name).await;
        sqlx::query(&format!("DROP SCHEMA {} CASCADE", self.schema))
            .execute(&self.admin)
            .await
            .unwrap();
    }
}

// A closed client connection's backend can still be finishing a maintenance
// statement; wait for it so DROP SCHEMA cannot deadlock.
async fn terminate_backends(admin: &PgPool, application_name: &str) {
    sqlx::query(
        "SELECT pg_terminate_backend(pid, 10000) FROM pg_stat_activity WHERE application_name=$1",
    )
    .bind(application_name)
    .execute(admin)
    .await
    .unwrap();
}

fn identity_request(vault: Uuid, owner: Uuid, profile: &KeyProfile, operation: Uuid) -> Value {
    let root = derive_root_key(PHRASE, profile).unwrap();
    let key = derive_vault_identity_recovery_key(&root, profile).unwrap();
    let anchor = VaultIdentityAnchor::generate(vault, operation, owner).unwrap();
    let recovery = VaultIdentityRecoveryRecord::wrap(&anchor, &key, profile).unwrap();
    json!({"protocol_version": VAULT_IDENTITY_PROTOCOL_VERSION, "operation_id": operation, "key_epoch": 1, "profile_fingerprint": profile.fingerprint().unwrap(), "recovery_record": STANDARD.encode(recovery.encode())})
}

async fn register_next_profile(server: &Server) {
    let profile = KeyProfile::new(server.vault, 2).unwrap();
    let root = derive_root_key(PHRASE, &profile).unwrap();
    let header = create_vault_check_header(&root, profile.clone()).unwrap();
    let body = json!({
        "key_epoch": 2,
        "public_key_profile": profile,
        "encrypted_vault_check_header": STANDARD.encode(serde_json::to_vec(&header).unwrap()),
        "profile_fingerprint": profile.fingerprint().unwrap(),
        "admission_public_key": URL_SAFE_NO_PAD.encode([7_u8; 32]),
    });
    let register = server
        .auth(Client::new().post(format!("{}/v1/vault/key-profiles", server.base_url)))
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(register.status(), StatusCode::CREATED);
}

async fn activate_next_profile(server: &Server) {
    register_next_profile(server).await;
    let activate = server
        .auth(Client::new().post(format!(
            "{}/v1/vault/key-profiles/2/activate",
            server.base_url
        )))
        .send()
        .await
        .unwrap();
    assert_eq!(activate.status(), StatusCode::OK);
}

async fn copy_vault_rows(source: &PgPool, destination: &PgPool, table: &str, vault: Uuid) {
    let statement =
        format!("COPY (SELECT * FROM {table} WHERE vault_id='{vault}') TO STDOUT (FORMAT BINARY)");
    let mut output = source.copy_out_raw(&statement).await.unwrap();
    let mut bytes = Vec::new();
    while let Some(chunk) = output.try_next().await.unwrap() {
        bytes.extend_from_slice(&chunk);
    }
    drop(output);
    let mut input = destination
        .copy_in_raw(&format!("COPY {table} FROM STDIN (FORMAT BINARY)"))
        .await
        .unwrap();
    input.send(bytes).await.unwrap();
    input.finish().await.unwrap();
}

async fn wait_for_blocked_query(server: &Server, query_prefix: &str) {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let blocked: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE application_name=$1 AND state='active' AND wait_event_type='Lock' AND query LIKE $2)",
            )
            .bind(&server.application_name)
            .bind(format!("{query_prefix}%"))
            .fetch_one(&server.admin)
            .await
            .unwrap();
            if blocked {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("request reached the vault row lock");
}

#[tokio::test]
async fn establish_is_absent_then_sticky_and_conflicts_on_different_value() {
    let server = Server::start().await;
    let client = Client::new();
    let absent: Value = server
        .auth(client.get(format!("{}/v1/vault/identity", server.base_url)))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(absent, json!({"state":"absent","protocol_version":1}));
    let request = server.request(Uuid::new_v4());
    assert_eq!(
        server.post(&server.token, &request).await.status(),
        StatusCode::CREATED
    );
    assert_eq!(
        server.post(&server.token, &request).await.status(),
        StatusCode::OK
    );
    let conflict = server
        .post(&server.token, &server.request(Uuid::new_v4()))
        .await;
    assert_eq!(conflict.status(), StatusCode::CONFLICT);
    assert_eq!(
        conflict.json::<Value>().await.unwrap()["code"],
        "vault_identity_conflict"
    );
    let established: Value = server
        .auth(client.get(format!("{}/v1/vault/identity", server.base_url)))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(established["state"], "established");
    assert_eq!(established["operation_id"], request["operation_id"]);
    server.shutdown().await;
}

#[tokio::test]
async fn exact_retry_remains_idempotent_after_profile_activation() {
    let server = Server::start().await;
    let request = server.request(Uuid::new_v4());
    assert_eq!(
        server.post(&server.token, &request).await.status(),
        StatusCode::CREATED
    );
    activate_next_profile(&server).await;

    let retry = server.post(&server.token, &request).await;
    assert_eq!(retry.status(), StatusCode::OK);
    assert_eq!(
        retry.json::<Value>().await.unwrap()["operation_id"],
        request["operation_id"]
    );

    let conflict = server
        .post(&server.token, &server.request(Uuid::new_v4()))
        .await;
    assert_eq!(conflict.status(), StatusCode::CONFLICT);
    assert_eq!(
        conflict.json::<Value>().await.unwrap(),
        json!({"code":"vault_identity_conflict"})
    );

    let established = server
        .auth(Client::new().get(format!("{}/v1/vault/identity", server.base_url)))
        .send()
        .await
        .unwrap();
    assert_eq!(established.status(), StatusCode::OK);
    let established = established.json::<Value>().await.unwrap();
    assert_eq!(established["operation_id"], request["operation_id"]);
    assert_eq!(established["recovery_record"], request["recovery_record"]);
    server.shutdown().await;
}

#[tokio::test]
async fn all_json_extraction_failures_use_the_fixed_identity_error() {
    let server = Server::start().await;
    let valid = server.request(Uuid::new_v4());
    let mut unknown = valid.clone();
    unknown["caller_controlled_secret"] = json!(true);
    let mut wrong_type = valid.clone();
    wrong_type["key_epoch"] = json!("caller-controlled-type");
    let mut bad_uuid = valid.clone();
    bad_uuid["operation_id"] = json!("caller-controlled-uuid");
    let mut bad_base64 = valid;
    bad_base64["recovery_record"] = json!("caller-controlled-base64");
    let bodies = [
        unknown.to_string(),
        wrong_type.to_string(),
        bad_uuid.to_string(),
        bad_base64.to_string(),
        r#"{"protocol_version":1,"caller-controlled-syntax":]"#.to_owned(),
    ];

    for body in bodies {
        let response = Client::new()
            .post(format!("{}/v1/vault/identity", server.base_url))
            .bearer_auth(&server.token)
            .header("content-type", "application/json")
            .body(body)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(
            response.bytes().await.unwrap().as_ref(),
            br#"{"code":"invalid_vault_identity"}"#
        );
    }
    server.shutdown().await;
}

#[tokio::test]
async fn concurrent_establishes_store_exactly_one_value() {
    let server = Server::start().await;
    let requests = (0..8)
        .map(|_| server.request(Uuid::new_v4()))
        .collect::<Vec<_>>();
    let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(requests.len()));
    let mut tasks = Vec::new();
    for request in requests {
        let base = server.base_url.clone();
        let token = server.token.clone();
        let barrier = barrier.clone();
        tasks.push(tokio::spawn(async move {
            barrier.wait().await;
            Client::new()
                .post(format!("{base}/v1/vault/identity"))
                .bearer_auth(token)
                .json(&request)
                .send()
                .await
                .unwrap()
        }));
    }
    let statuses = futures_util::future::join_all(tasks)
        .await
        .into_iter()
        .map(|task| task.unwrap().status())
        .collect::<Vec<_>>();
    assert_eq!(
        statuses
            .iter()
            .filter(|&&status| status == StatusCode::CREATED)
            .count(),
        1
    );
    assert!(
        statuses
            .iter()
            .all(|&status| status == StatusCode::CREATED || status == StatusCode::CONFLICT)
    );
    let rows: i64 =
        sqlx::query_scalar("SELECT count(*) FROM vault_identity_establishments WHERE vault_id=$1")
            .bind(server.vault)
            .fetch_one(&server.pool)
            .await
            .unwrap();
    assert_eq!(rows, 1);
    server.shutdown().await;
}

#[tokio::test]
async fn identity_rows_are_immutable_but_vault_cascade_removes_them() {
    let server = Server::start().await;
    let request = server.request(Uuid::new_v4());
    assert_eq!(
        server.post(&server.token, &request).await.status(),
        StatusCode::CREATED
    );
    assert!(
        sqlx::query("UPDATE vault_identity_establishments SET operation_id=$2 WHERE vault_id=$1")
            .bind(server.vault)
            .bind(Uuid::new_v4())
            .execute(&server.pool)
            .await
            .is_err()
    );
    assert!(
        sqlx::query("DELETE FROM vault_identity_establishments WHERE vault_id=$1")
            .bind(server.vault)
            .execute(&server.pool)
            .await
            .is_err()
    );
    sqlx::query("TRUNCATE vaults CASCADE")
        .execute(&server.pool)
        .await
        .unwrap();
    let rows: i64 =
        sqlx::query_scalar("SELECT count(*) FROM vault_identity_establishments WHERE vault_id=$1")
            .bind(server.vault)
            .fetch_one(&server.pool)
            .await
            .unwrap();
    assert_eq!(rows, 0);
    server.shutdown().await;
}

#[tokio::test]
async fn vault_identity_http_and_rls_are_cross_vault_isolated() {
    let server = Server::start().await;
    let vault_b = server.create_vault().await;
    let role = format!("vault_identity_runtime_{}", Uuid::new_v4().simple());
    sqlx::raw_sql(&format!(
        "CREATE ROLE {role} NOLOGIN NOSUPERUSER NOBYPASSRLS NOCREATEROLE; \
         GRANT USAGE ON SCHEMA {} TO {role}; \
         GRANT SELECT, INSERT ON TABLE {}.vault_identity_establishments TO {role}",
        server.schema, server.schema
    ))
    .execute(&server.admin)
    .await
    .unwrap();

    let mut own_insert = server.pool.begin().await.unwrap();
    sqlx::query(&format!("SET LOCAL ROLE {role}"))
        .execute(&mut *own_insert)
        .await
        .unwrap();
    sqlx::query("SELECT set_config('peppy.vault_id',$1,true)")
        .bind(vault_b.vault.to_string())
        .execute(&mut *own_insert)
        .await
        .unwrap();
    sqlx::query("INSERT INTO vault_identity_establishments(vault_id,operation_id,key_epoch,profile_fingerprint,recovery_record,established_by_device_id) VALUES($1,$2,1,$3,$4,$5)")
        .bind(vault_b.vault)
        .bind(Uuid::new_v4())
        .bind(vault_b.profile.fingerprint().unwrap())
        .bind(vec![0_u8; 266])
        .bind(vault_b.owner)
        .execute(&mut *own_insert)
        .await
        .unwrap();
    own_insert.rollback().await.unwrap();

    let mut cross_vault_insert = server.pool.begin().await.unwrap();
    sqlx::query(&format!("SET LOCAL ROLE {role}"))
        .execute(&mut *cross_vault_insert)
        .await
        .unwrap();
    sqlx::query("SELECT set_config('peppy.vault_id',$1,true)")
        .bind(vault_b.vault.to_string())
        .execute(&mut *cross_vault_insert)
        .await
        .unwrap();
    let insert_error = sqlx::query("INSERT INTO vault_identity_establishments(vault_id,operation_id,key_epoch,profile_fingerprint,recovery_record,established_by_device_id) VALUES($1,$2,1,$3,$4,$5)")
        .bind(server.vault)
        .bind(Uuid::new_v4())
        .bind(server.profile.fingerprint().unwrap())
        .bind(vec![0_u8; 266])
        .bind(server.owner)
        .execute(&mut *cross_vault_insert)
        .await
        .unwrap_err();
    assert_eq!(
        insert_error.as_database_error().unwrap().code().as_deref(),
        Some("42501")
    );
    cross_vault_insert.rollback().await.unwrap();

    let request_a = server.request(Uuid::new_v4());
    assert_eq!(
        server.post(&server.token, &request_a).await.status(),
        StatusCode::CREATED
    );
    let client = Client::new();
    let absent_b: Value = client
        .get(format!("{}/v1/vault/identity", server.base_url))
        .bearer_auth(&vault_b.token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(absent_b, json!({"state":"absent","protocol_version":1}));

    let request_b = vault_b.request(Uuid::new_v4());
    assert_eq!(
        server.post(&vault_b.token, &request_b).await.status(),
        StatusCode::CREATED
    );
    let established_b: Value = client
        .get(format!("{}/v1/vault/identity", server.base_url))
        .bearer_auth(&vault_b.token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(established_b["operation_id"], request_b["operation_id"]);
    let stored_a: Uuid = sqlx::query_scalar(
        "SELECT operation_id FROM vault_identity_establishments WHERE vault_id=$1",
    )
    .bind(server.vault)
    .fetch_one(&server.pool)
    .await
    .unwrap();
    assert_eq!(stored_a.to_string(), request_a["operation_id"]);

    let mut scoped_select = server.pool.begin().await.unwrap();
    sqlx::query(&format!("SET LOCAL ROLE {role}"))
        .execute(&mut *scoped_select)
        .await
        .unwrap();
    sqlx::query("SELECT set_config('peppy.vault_id',$1,true)")
        .bind(vault_b.vault.to_string())
        .execute(&mut *scoped_select)
        .await
        .unwrap();
    let hidden_a: Option<Uuid> = sqlx::query_scalar(
        "SELECT operation_id FROM vault_identity_establishments WHERE vault_id=$1",
    )
    .bind(server.vault)
    .fetch_optional(&mut *scoped_select)
    .await
    .unwrap();
    assert_eq!(hidden_a, None);
    scoped_select.rollback().await.unwrap();

    sqlx::raw_sql(&format!("DROP OWNED BY {role}; DROP ROLE {role}"))
        .execute(&server.admin)
        .await
        .unwrap();
    server.shutdown().await;
}

#[tokio::test]
async fn blocked_establish_revalidates_revocation_and_owner_role() {
    let server = Server::start().await;
    let demoted = server.create_vault().await;

    let mut lock = server.pool.begin().await.unwrap();
    sqlx::query("SELECT key_epoch FROM vaults WHERE vault_id=$1 FOR UPDATE")
        .bind(server.vault)
        .fetch_one(&mut *lock)
        .await
        .unwrap();
    let base_url = server.base_url.clone();
    let token = server.token.clone();
    let request = server.request(Uuid::new_v4());
    let establish = tokio::spawn(async move {
        Client::new()
            .post(format!("{base_url}/v1/vault/identity"))
            .bearer_auth(token)
            .json(&request)
            .send()
            .await
            .unwrap()
    });
    wait_for_blocked_query(
        &server,
        "SELECT key_epoch,profile_fingerprint FROM vaults WHERE vault_id",
    )
    .await;
    sqlx::query("UPDATE device_credentials SET revoked_at=clock_timestamp() WHERE vault_id=$1 AND device_id=$2")
        .bind(server.vault)
        .bind(server.owner)
        .execute(&server.pool)
        .await
        .unwrap();
    lock.commit().await.unwrap();
    let revoked = establish.await.unwrap();
    assert_eq!(revoked.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        revoked.json::<Value>().await.unwrap(),
        json!({"code":"invalid_bearer"})
    );

    let mut lock = server.pool.begin().await.unwrap();
    sqlx::query("SELECT key_epoch FROM vaults WHERE vault_id=$1 FOR UPDATE")
        .bind(demoted.vault)
        .fetch_one(&mut *lock)
        .await
        .unwrap();
    let base_url = server.base_url.clone();
    let token = demoted.token.clone();
    let request = demoted.request(Uuid::new_v4());
    let establish = tokio::spawn(async move {
        Client::new()
            .post(format!("{base_url}/v1/vault/identity"))
            .bearer_auth(token)
            .json(&request)
            .send()
            .await
            .unwrap()
    });
    wait_for_blocked_query(
        &server,
        "SELECT key_epoch,profile_fingerprint FROM vaults WHERE vault_id",
    )
    .await;
    sqlx::query("UPDATE devices SET role='device' WHERE vault_id=$1 AND device_id=$2")
        .bind(demoted.vault)
        .bind(demoted.owner)
        .execute(&server.pool)
        .await
        .unwrap();
    lock.commit().await.unwrap();
    let demoted_response = establish.await.unwrap();
    assert_eq!(demoted_response.status(), StatusCode::FORBIDDEN);
    assert_eq!(
        demoted_response.json::<Value>().await.unwrap(),
        json!({"code":"owner_required"})
    );

    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM vault_identity_establishments")
        .fetch_one(&server.pool)
        .await
        .unwrap();
    assert_eq!(rows, 0);
    server.shutdown().await;
}

#[tokio::test]
async fn blocked_absent_establish_rejects_profile_activated_ahead_of_it() {
    let server = Server::start().await;
    register_next_profile(&server).await;
    let mut lock = server.pool.begin().await.unwrap();
    sqlx::query("SELECT key_epoch FROM vaults WHERE vault_id=$1 FOR UPDATE")
        .bind(server.vault)
        .fetch_one(&mut *lock)
        .await
        .unwrap();

    let base_url = server.base_url.clone();
    let token = server.token.clone();
    let activate = tokio::spawn(async move {
        Client::new()
            .post(format!("{base_url}/v1/vault/key-profiles/2/activate"))
            .bearer_auth(token)
            .send()
            .await
            .unwrap()
    });
    wait_for_blocked_query(&server, "SELECT key_epoch FROM vaults WHERE vault_id").await;

    let base_url = server.base_url.clone();
    let token = server.token.clone();
    let request = server.request(Uuid::new_v4());
    let establish = tokio::spawn(async move {
        Client::new()
            .post(format!("{base_url}/v1/vault/identity"))
            .bearer_auth(token)
            .json(&request)
            .send()
            .await
            .unwrap()
    });
    wait_for_blocked_query(
        &server,
        "SELECT key_epoch,profile_fingerprint FROM vaults WHERE vault_id",
    )
    .await;
    lock.commit().await.unwrap();

    assert_eq!(activate.await.unwrap().status(), StatusCode::OK);
    let stale = establish.await.unwrap();
    assert_eq!(stale.status(), StatusCode::CONFLICT);
    assert_eq!(
        stale.json::<Value>().await.unwrap(),
        json!({"code":"vault_identity_stale_context"})
    );
    let rows: i64 =
        sqlx::query_scalar("SELECT count(*) FROM vault_identity_establishments WHERE vault_id=$1")
            .bind(server.vault)
            .fetch_one(&server.pool)
            .await
            .unwrap();
    assert_eq!(rows, 0);
    server.shutdown().await;
}

#[tokio::test]
async fn independent_router_pools_share_the_identity_cas() {
    let server = Server::start().await;
    let replica_pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(&server.scoped_url)
        .await
        .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let replica_address = listener.local_addr().unwrap();
    let replica_task = tokio::spawn({
        let pool = replica_pool.clone();
        async move {
            axum::serve(
                listener,
                router_with_options(pool, TransportOptions::default()),
            )
            .await
            .unwrap();
        }
    });
    let requests = [
        server.request(Uuid::new_v4()),
        server.request(Uuid::new_v4()),
    ];
    let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(2));
    let mut tasks = Vec::new();
    for (base_url, request) in [
        (server.base_url.clone(), requests[0].clone()),
        (format!("http://{replica_address}"), requests[1].clone()),
    ] {
        let barrier = barrier.clone();
        let token = server.token.clone();
        tasks.push(tokio::spawn(async move {
            barrier.wait().await;
            Client::new()
                .post(format!("{base_url}/v1/vault/identity"))
                .bearer_auth(token)
                .json(&request)
                .send()
                .await
                .unwrap()
                .status()
        }));
    }
    let statuses = futures_util::future::join_all(tasks)
        .await
        .into_iter()
        .map(Result::unwrap)
        .collect::<Vec<_>>();
    assert_eq!(
        statuses
            .iter()
            .filter(|&&s| s == StatusCode::CREATED)
            .count(),
        1
    );
    assert_eq!(
        statuses
            .iter()
            .filter(|&&s| s == StatusCode::CONFLICT)
            .count(),
        1
    );
    let rows: i64 =
        sqlx::query_scalar("SELECT count(*) FROM vault_identity_establishments WHERE vault_id=$1")
            .bind(server.vault)
            .fetch_one(&server.pool)
            .await
            .unwrap();
    assert_eq!(rows, 1);

    replica_task.abort();
    let _ = replica_task.await;
    replica_pool.close().await;
    server.shutdown().await;
}

#[tokio::test]
async fn binary_database_restore_preserves_the_established_identity() {
    let server = Server::start().await;
    let request = server.request(Uuid::new_v4());
    assert_eq!(
        server.post(&server.token, &request).await.status(),
        StatusCode::CREATED
    );

    let restore_schema = format!("vault_identity_restore_{}", Uuid::new_v4().simple());
    sqlx::query(&format!("CREATE SCHEMA {restore_schema}"))
        .execute(&server.admin)
        .await
        .unwrap();
    let database_url = std::env::var("TEST_DATABASE_URL").unwrap();
    let restore_application_name = format!("vault_identity_restore_{}", Uuid::new_v4().simple());
    let mut restore_url: url::Url = database_url.parse().unwrap();
    restore_url
        .query_pairs_mut()
        .append_pair("options", &format!("-csearch_path={restore_schema}"))
        .append_pair("application_name", &restore_application_name);
    let restore_pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(restore_url.as_str())
        .await
        .unwrap();
    sqlx::migrate!("./migrations")
        .run(&restore_pool)
        .await
        .unwrap();
    for table in [
        "vaults",
        "devices",
        "device_credentials",
        "vault_identity_establishments",
    ] {
        copy_vault_rows(&server.pool, &restore_pool, table, server.vault).await;
    }

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn({
        let pool = restore_pool.clone();
        async move {
            axum::serve(
                listener,
                router_with_options(pool, TransportOptions::default()),
            )
            .await
            .unwrap();
        }
    });
    let response = Client::new()
        .get(format!("http://{address}/v1/vault/identity"))
        .bearer_auth(&server.token)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.json::<Value>().await.unwrap(),
        json!({
            "state": "established",
            "protocol_version": VAULT_IDENTITY_PROTOCOL_VERSION,
            "operation_id": request["operation_id"],
            "key_epoch": request["key_epoch"],
            "profile_fingerprint": request["profile_fingerprint"],
            "recovery_record": request["recovery_record"],
        })
    );

    task.abort();
    let _ = task.await;
    restore_pool.close().await;
    terminate_backends(&server.admin, &restore_application_name).await;
    sqlx::query(&format!("DROP SCHEMA {restore_schema} CASCADE"))
        .execute(&server.admin)
        .await
        .unwrap();
    server.shutdown().await;
}

#[tokio::test]
async fn malformed_and_non_owner_requests_do_not_write() {
    let server = Server::start().await;
    let device = Uuid::new_v4();
    let token = (0..3)
        .map(|_| Uuid::new_v4().simple().to_string())
        .collect::<String>();
    sqlx::query("INSERT INTO devices(vault_id,device_id,role,public_key,profile_fingerprint,key_epoch) VALUES($1,$2,'device','{}',$3,1)").bind(server.vault).bind(device).bind(server.profile.fingerprint().unwrap()).execute(&server.pool).await.unwrap();
    sqlx::query("INSERT INTO device_credentials(token_digest,vault_id,device_id) VALUES($1,$2,$3)")
        .bind(Sha256::digest(token.as_bytes()).as_slice())
        .bind(server.vault)
        .bind(device)
        .execute(&server.pool)
        .await
        .unwrap();
    let mut malformed = server.request(Uuid::new_v4());
    malformed["recovery_record"] = "AAAA".into();
    assert_eq!(
        server.post(&server.token, &malformed).await.status(),
        StatusCode::UNPROCESSABLE_ENTITY
    );
    let mut unknown = server.request(Uuid::new_v4());
    unknown["extra"] = true.into();
    assert_eq!(
        server.post(&server.token, &unknown).await.status(),
        StatusCode::UNPROCESSABLE_ENTITY
    );
    assert_eq!(
        server
            .post(&token, &server.request(Uuid::new_v4()))
            .await
            .status(),
        StatusCode::FORBIDDEN
    );
    let rows: i64 =
        sqlx::query_scalar("SELECT count(*) FROM vault_identity_establishments WHERE vault_id=$1")
            .bind(server.vault)
            .fetch_one(&server.pool)
            .await
            .unwrap();
    assert_eq!(rows, 0);
    server.shutdown().await;
}
