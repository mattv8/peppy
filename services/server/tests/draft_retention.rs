//! Draft retention coverage. Database invariants run against a fresh schema;
//! the HTTP route matrix runs through the real router with Postgres and S3
//! (SeaweedFS) storage. The HTTP harness deliberately does not start the
//! maintenance loop, so deadline and terminal cases observe the request path
//! before any worker pass; worker passes are invoked explicitly.

use std::{io::Cursor, time::Duration};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use peppy_server::{
    ServerBuilder,
    api::create_owner,
    config::Config,
    draft_retention::expire_due,
    storage::{Storage, StorageError},
};
use reqwest::{Client, Response, StatusCode};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{PgPool, postgres::PgPoolOptions};
use tokio::{net::TcpListener, task::JoinHandle};
use url::Url;
use uuid::Uuid;

async fn pool() -> (PgPool, PgPool, String) {
    let database_url = std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL is required");
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(&database_url)
        .await
        .unwrap();
    let schema = format!("draft_retention_{}", Uuid::new_v4().simple());
    sqlx::query(&format!("CREATE SCHEMA {schema}"))
        .execute(&admin)
        .await
        .unwrap();
    let mut url: Url = database_url.parse().unwrap();
    url.query_pairs_mut()
        .append_pair("options", &format!("-csearch_path={schema}"));
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(url.as_str())
        .await
        .unwrap();
    peppy_server::MIGRATOR.run(&pool).await.unwrap();
    (admin, pool, schema)
}

async fn seed(pool: &PgPool) -> (Uuid, Uuid, Uuid) {
    let vault = Uuid::new_v4();
    let device = Uuid::new_v4();
    let draft = Uuid::new_v4();
    seed_with_ids(pool, vault, device).await;
    (vault, device, draft)
}

async fn seed_with_ids(pool: &PgPool, vault: Uuid, device: Uuid) {
    sqlx::query("INSERT INTO vaults(vault_id,public_key_profile,encrypted_vault_check_header,key_epoch,profile_fingerprint) VALUES($1,'{}',$2,1,$3)").bind(vault).bind(vec![0_u8]).bind("0".repeat(64)).execute(pool).await.unwrap();
    sqlx::query("INSERT INTO devices(vault_id,device_id,role,public_key,profile_fingerprint,key_epoch) VALUES($1,$2,'device','{}',$3,1)").bind(vault).bind(device).bind("0".repeat(64)).execute(pool).await.unwrap();
}

#[tokio::test]
async fn expiry_closes_branches_nulls_ciphertext_lapses_holds_and_queues_unreferenced_draft_objects()
 {
    let (admin, pool, schema) = pool().await;
    let (vault, device, draft) = seed(&pool).await;
    let branch = Uuid::new_v4();
    let revision = Uuid::new_v4();
    let attachment = Uuid::new_v4();
    sqlx::query("INSERT INTO draft_incarnations(vault_id,draft_id) VALUES($1,$2)")
        .bind(vault)
        .bind(draft)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO draft_branches(vault_id,draft_id,branch_id,head_revision_id,state,expires_at,change_seq) VALUES($1,$2,$3,$4,'live',now()-interval '1 second',1)").bind(vault).bind(draft).bind(branch).bind(revision).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO draft_revisions(vault_id,draft_id,revision_id,branch_id,producer_device_id,sealed_record,attachment_ids,authored,conflict,expires_at_at_accept) VALUES($1,$2,$3,$4,$5,$6,'{}',true,false,now()-interval '1 second')").bind(vault).bind(draft).bind(revision).bind(branch).bind(device).bind(vec![1_u8]).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO attachments(vault_id,attachment_id,object_key,ciphertext_bytes,ciphertext_sha256,created_by_device_id,draft_id) VALUES($1,$2,'draft/expired',1,$3,$4,$5)").bind(vault).bind(attachment).bind(vec![0_u8;32]).bind(device).bind(draft).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO draft_attachment_holds(vault_id,draft_id,attachment_id,kind,expires_at) VALUES($1,$2,$3,'draft',now()-interval '1 second')").bind(vault).bind(draft).bind(attachment).execute(&pool).await.unwrap();
    expire_due(&pool, 10).await.unwrap();
    let state: (String, Option<String>, i64, i64) = sqlx::query_as("SELECT (SELECT state FROM draft_branches WHERE vault_id=$1 AND branch_id=$2),(SELECT sealed_record::text FROM draft_revisions WHERE vault_id=$1 AND draft_id=$3 AND revision_id=$4),(SELECT count(*) FROM attachments WHERE vault_id=$1 AND attachment_id=$5),(SELECT count(*) FROM storage_deletions WHERE object_key='draft/expired')").bind(vault).bind(branch).bind(draft).bind(revision).bind(attachment).fetch_one(&pool).await.unwrap();
    assert_eq!(state, ("closed".into(), None, 0, 1));
    let terminal: String =
        sqlx::query_scalar("SELECT reason FROM draft_terminals WHERE vault_id=$1 AND draft_id=$2")
            .bind(vault)
            .bind(draft)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(terminal, "expired");
    pool.close().await;
    sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
        .execute(&admin)
        .await
        .unwrap();
    admin.close().await;
}

#[tokio::test]
async fn live_hold_prevents_draft_attachment_expiry() {
    let (admin, pool, schema) = pool().await;
    let (vault, device, draft) = seed(&pool).await;
    let attachment = Uuid::new_v4();
    sqlx::query("INSERT INTO attachments(vault_id,attachment_id,object_key,ciphertext_bytes,ciphertext_sha256,created_by_device_id,draft_id) VALUES($1,$2,'draft/held',1,$3,$4,$5)").bind(vault).bind(attachment).bind(vec![0_u8;32]).bind(device).bind(draft).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO draft_attachment_holds(vault_id,draft_id,attachment_id,kind,expires_at) VALUES($1,$2,$3,'send',now()+interval '30 days')").bind(vault).bind(draft).bind(attachment).execute(&pool).await.unwrap();
    expire_due(&pool, 10).await.unwrap();
    assert!(
        sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM attachments WHERE vault_id=$1 AND attachment_id=$2)"
        )
        .bind(vault)
        .bind(attachment)
        .fetch_one(&pool)
        .await
        .unwrap()
    );
    pool.close().await;
    sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
        .execute(&admin)
        .await
        .unwrap();
    admin.close().await;
}

#[tokio::test]
async fn protected_attachment_does_not_starve_a_later_expired_vault() {
    let (admin, pool, schema) = pool().await;
    let earlier_vault = Uuid::from_u128(1);
    let later_vault = Uuid::from_u128(2);
    let earlier_device = Uuid::new_v4();
    let later_device = Uuid::new_v4();
    let protected_attachment = Uuid::new_v4();
    let expired_attachment = Uuid::new_v4();
    let later_draft = Uuid::new_v4();
    let branch = Uuid::new_v4();
    let revision = Uuid::new_v4();

    for (vault, device) in [(earlier_vault, earlier_device), (later_vault, later_device)] {
        seed_with_ids(&pool, vault, device).await;
    }

    sqlx::query("INSERT INTO attachments(vault_id,attachment_id,object_key,ciphertext_bytes,ciphertext_sha256,created_by_device_id,draft_id) VALUES($1,$2,'draft/protected',1,$3,$4,$5)")
        .bind(earlier_vault)
        .bind(protected_attachment)
        .bind(vec![0_u8; 32])
        .bind(earlier_device)
        .bind(Uuid::new_v4())
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO attachment_record_references(vault_id,attachment_id,producer_device_id,producer_sequence,registered_by_device_id) VALUES($1,$2,$3,1,$3)")
        .bind(earlier_vault)
        .bind(protected_attachment)
        .bind(earlier_device)
        .execute(&pool)
        .await
        .unwrap();

    sqlx::query("INSERT INTO draft_incarnations(vault_id,draft_id) VALUES($1,$2)")
        .bind(later_vault)
        .bind(later_draft)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO draft_branches(vault_id,draft_id,branch_id,head_revision_id,state,expires_at,change_seq) VALUES($1,$2,$3,$4,'live',now()-interval '1 second',1)")
        .bind(later_vault)
        .bind(later_draft)
        .bind(branch)
        .bind(revision)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO attachments(vault_id,attachment_id,object_key,ciphertext_bytes,ciphertext_sha256,created_by_device_id,draft_id) VALUES($1,$2,'draft/expired-later',1,$3,$4,$5)")
        .bind(later_vault)
        .bind(expired_attachment)
        .bind(vec![0_u8; 32])
        .bind(later_device)
        .bind(later_draft)
        .execute(&pool)
        .await
        .unwrap();

    expire_due(&pool, 1).await.unwrap();

    let attachments: (bool, bool) = sqlx::query_as("SELECT EXISTS(SELECT 1 FROM attachments WHERE vault_id=$1 AND attachment_id=$2), EXISTS(SELECT 1 FROM attachments WHERE vault_id=$3 AND attachment_id=$4)")
        .bind(earlier_vault)
        .bind(protected_attachment)
        .bind(later_vault)
        .bind(expired_attachment)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(attachments, (true, false));

    pool.close().await;
    sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
        .execute(&admin)
        .await
        .unwrap();
    admin.close().await;
}

#[tokio::test]
async fn live_hold_does_not_starve_later_draft_cleanup_at_quota() {
    let (admin, pool, schema) = pool().await;
    let earlier_vault = Uuid::from_u128(3);
    let later_vault = Uuid::from_u128(4);
    let earlier_device = Uuid::new_v4();
    let later_device = Uuid::new_v4();
    let held_attachment = Uuid::new_v4();
    let expired_attachment = Uuid::new_v4();

    seed_with_ids(&pool, earlier_vault, earlier_device).await;
    seed_with_ids(&pool, later_vault, later_device).await;
    sqlx::query("INSERT INTO attachments(vault_id,attachment_id,object_key,ciphertext_bytes,ciphertext_sha256,created_by_device_id,draft_id) VALUES($1,$2,'draft/held-quota',1,$3,$4,$5)")
        .bind(earlier_vault)
        .bind(held_attachment)
        .bind(vec![0_u8; 32])
        .bind(earlier_device)
        .bind(Uuid::new_v4())
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO draft_attachment_holds(vault_id,draft_id,attachment_id,kind,expires_at) VALUES($1,$2,$3,'send',now()+interval '30 days')")
        .bind(earlier_vault)
        .bind(Uuid::new_v4())
        .bind(held_attachment)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO attachments(vault_id,attachment_id,object_key,ciphertext_bytes,ciphertext_sha256,created_by_device_id,draft_id) VALUES($1,$2,'draft/expired-quota',1,$3,$4,$5)")
        .bind(later_vault)
        .bind(expired_attachment)
        .bind(vec![0_u8; 32])
        .bind(later_device)
        .bind(Uuid::new_v4())
        .execute(&pool)
        .await
        .unwrap();

    expire_due(&pool, 1).await.unwrap();

    let attachments: (bool, bool) = sqlx::query_as("SELECT EXISTS(SELECT 1 FROM attachments WHERE vault_id=$1 AND attachment_id=$2), EXISTS(SELECT 1 FROM attachments WHERE vault_id=$3 AND attachment_id=$4)")
        .bind(earlier_vault)
        .bind(held_attachment)
        .bind(later_vault)
        .bind(expired_attachment)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(attachments, (true, false));

    pool.close().await;
    sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
        .execute(&admin)
        .await
        .unwrap();
    admin.close().await;
}

// ---------------------------------------------------------------------------
// HTTP harness
// ---------------------------------------------------------------------------

struct Harness {
    pool: PgPool,
    admin: PgPool,
    schema: String,
    base_url: String,
    task: JoinHandle<()>,
    owner_token: String,
    owner_device: Uuid,
    vault: Uuid,
    storage: Storage,
}

impl Harness {
    async fn start() -> Self {
        Self::start_inner(false).await
    }

    async fn start_with_maintenance() -> Self {
        Self::start_inner(true).await
    }

    async fn start_inner(start_maintenance: bool) -> Self {
        let database_url = std::env::var("TEST_DATABASE_URL")
            .expect("TEST_DATABASE_URL is required; integration tests never silently skip");
        let mut config = Config::from_env().expect("complete S3 test configuration is required");
        let storage =
            Storage::initialize(config.s3.as_ref().expect("S3 is required for draft tests"))
                .await
                .unwrap();
        let admin = PgPoolOptions::new()
            .max_connections(2)
            .connect(&database_url)
            .await
            .unwrap();
        let schema = format!("draft_http_{}", Uuid::new_v4().simple());
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
        config.database_url = isolated_url.to_string();
        let mut server = ServerBuilder::community(config, pool.clone())
            .storage_cleanup_interval(Duration::from_millis(100))
            .build()
            .await
            .unwrap();
        if start_maintenance {
            server.start_maintenance().unwrap();
        }
        let router = server.router();

        let vault = Uuid::new_v4();
        let (profile, fingerprint) = profile(vault);
        let owner = create_owner(&pool, profile, vec![1, 2, 3], fingerprint, 1)
            .await
            .unwrap();
        let owner_device: Uuid =
            sqlx::query_scalar("SELECT device_id FROM devices WHERE vault_id=$1 AND role='owner'")
                .bind(vault)
                .fetch_one(&pool)
                .await
                .unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        Self {
            pool,
            admin,
            schema,
            base_url: format!("http://{addr}"),
            task,
            owner_token: owner.device_token,
            owner_device,
            vault,
            storage,
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base_url)
    }

    async fn put_as(&self, token: &str, draft: Uuid, revision: Uuid, body: Value) -> Response {
        Client::new()
            .put(self.url(&format!("/v1/drafts/{draft}/revisions/{revision}")))
            .bearer_auth(token)
            .json(&body)
            .send()
            .await
            .unwrap()
    }

    async fn put(&self, draft: Uuid, revision: Uuid, body: Value) -> Response {
        self.put_as(&self.owner_token, draft, revision, body).await
    }

    async fn put_ok(&self, draft: Uuid, revision: Uuid, body: Value) -> Value {
        read_ok(self.put(draft, revision, body).await).await
    }

    async fn send_as(&self, token: &str, draft: Uuid, revision: Uuid, command: Uuid) -> Response {
        Client::new()
            .post(self.url(&format!("/v1/drafts/{draft}/send")))
            .bearer_auth(token)
            .json(&json!({"revision_id": revision, "command_id": command}))
            .send()
            .await
            .unwrap()
    }

    async fn send(&self, draft: Uuid, revision: Uuid, command: Uuid) -> Response {
        self.send_as(&self.owner_token, draft, revision, command)
            .await
    }

    async fn discard_as(&self, token: &str, draft: Uuid) -> Response {
        Client::new()
            .post(self.url(&format!("/v1/drafts/{draft}/discard")))
            .bearer_auth(token)
            .send()
            .await
            .unwrap()
    }

    async fn discard(&self, draft: Uuid) -> Response {
        self.discard_as(&self.owner_token, draft).await
    }

    async fn changes_as(&self, token: &str, query: &str) -> Response {
        Client::new()
            .get(self.url(&format!("/v1/drafts?{query}")))
            .bearer_auth(token)
            .send()
            .await
            .unwrap()
    }

    async fn changes(&self, query: &str) -> Value {
        read_ok(self.changes_as(&self.owner_token, query).await).await
    }

    async fn reserve_draft(&self, token: &str, draft: Uuid, cipher: &[u8]) -> Response {
        Client::new()
            .post(self.url("/v1/attachments/reserve"))
            .bearer_auth(token)
            .json(&json!({
                "declared_ciphertext_bytes": cipher.len(),
                "declared_ciphertext_sha256": hex::encode(Sha256::digest(cipher)),
                "reference_tracking": true,
                "retention_class": "message_media",
                "draft_id": draft,
            }))
            .send()
            .await
            .unwrap()
    }

    async fn upload(&self, attachment: Uuid, bytes: Vec<u8>) -> Response {
        Client::new()
            .put(self.url(&format!("/v1/attachments/{attachment}/upload")))
            .bearer_auth(&self.owner_token)
            .body(bytes)
            .send()
            .await
            .unwrap()
    }

    async fn finalize(&self, attachment: Uuid) -> Response {
        Client::new()
            .post(self.url(&format!("/v1/attachments/{attachment}/finalize")))
            .bearer_auth(&self.owner_token)
            .send()
            .await
            .unwrap()
    }

    /// Reserves, uploads and finalizes one draft-owned image; returns its ID.
    async fn draft_attachment(&self, draft: Uuid, bytes: &[u8]) -> Uuid {
        let cipher = ciphertext(bytes);
        let response = self.reserve_draft(&self.owner_token, draft, &cipher).await;
        let attachment = attachment_id(response).await;
        assert_eq!(
            self.upload(attachment, cipher).await.status(),
            StatusCode::NO_CONTENT
        );
        assert_eq!(self.finalize(attachment).await.status(), StatusCode::OK);
        attachment
    }

    async fn object_key(&self, attachment: Uuid) -> String {
        sqlx::query_scalar(
            "SELECT object_key FROM upload_reservations WHERE vault_id=$1 AND attachment_id=$2",
        )
        .bind(self.vault)
        .bind(attachment)
        .fetch_one(&self.pool)
        .await
        .unwrap()
    }

    async fn expire_branch(&self, branch: Uuid) {
        sqlx::query("UPDATE draft_branches SET expires_at=now()-interval '1 second' WHERE vault_id=$1 AND branch_id=$2")
            .bind(self.vault)
            .bind(branch)
            .execute(&self.pool)
            .await
            .unwrap();
        expire_due(&self.pool, 10).await.unwrap();
    }

    async fn wait_for_storage_cleanup(&self, key: &str) {
        for _ in 0..200 {
            let queued: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM storage_deletions WHERE object_key=$1 AND vault_id=$2)",
            )
            .bind(key)
            .bind(self.vault)
            .fetch_one(&self.pool)
            .await
            .unwrap();
            if !queued && self.storage.head_bytes(key).await == Err(StorageError::NotFound) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("storage cleanup did not remove {key} within 10 seconds");
    }

    async fn shutdown(self) {
        let rows = sqlx::query("SELECT object_key FROM upload_reservations")
            .fetch_all(&self.pool)
            .await
            .unwrap();
        for row in rows {
            let key: String = sqlx::Row::get(&row, "object_key");
            let _ = self.storage.delete(&key).await;
        }
        let copies: Vec<String> =
            sqlx::query_scalar("SELECT object_key FROM public_attachment_copies")
                .fetch_all(&self.pool)
                .await
                .unwrap();
        for key in copies {
            let _ = self.storage.delete(&key).await;
        }
        self.task.abort();
        let _ = self.task.await;
        self.pool.close().await;
        sqlx::query(&format!("DROP SCHEMA {} CASCADE", self.schema))
            .execute(&self.admin)
            .await
            .unwrap();
        self.admin.close().await;
    }
}

fn profile(vault: Uuid) -> (Value, String) {
    let salt: Vec<u8> = (0..16).collect();
    let mut digest = Sha256::new();
    digest.update(b"peppy-key-profile-v1\0");
    digest.update(1_u16.to_be_bytes());
    digest.update(&salt);
    digest.update(vault.as_bytes());
    digest.update(1_u32.to_be_bytes());
    (
        json!({"crypto_suite":1,"salt":salt,"vault_id":vault,"key_epoch":1}),
        hex::encode(digest.finalize()),
    )
}

fn ciphertext(plaintext: &[u8]) -> Vec<u8> {
    let key = peppy_crypto::FileKey::generate().unwrap();
    let mut encrypted = Vec::new();
    peppy_crypto::encrypt_stream(Cursor::new(plaintext), &mut encrypted, &key, b"attachment")
        .unwrap();
    encrypted
}

fn png(bytes: usize) -> Vec<u8> {
    let mut body = b"\x89PNG\r\n\x1a\n".to_vec();
    body.resize(bytes, 0);
    body
}

async fn foreign_owner(pool: &PgPool) -> String {
    let vault = Uuid::new_v4();
    let (profile, fingerprint) = profile(vault);
    create_owner(pool, profile, vec![4, 5, 6], fingerprint, 1)
        .await
        .unwrap()
        .device_token
}

fn record(text: &str) -> String {
    URL_SAFE_NO_PAD.encode(text.as_bytes())
}

fn revision(base: Option<Uuid>, text: &str, attachments: &[Uuid], authored: bool) -> Value {
    json!({
        "base_revision_id": base,
        "sealed_record": record(text),
        "attachment_ids": attachments,
        "authored": authored,
    })
}

fn revision_resolving(
    base: Option<Uuid>,
    text: &str,
    attachments: &[Uuid],
    authored: bool,
    resolves: &[Uuid],
) -> Value {
    let mut body = revision(base, text, attachments, authored);
    body["resolves_branch_ids"] = json!(resolves);
    body
}

async fn read_ok(response: Response) -> Value {
    let status = response.status();
    let text = response.text().await.unwrap();
    assert_eq!(status, StatusCode::OK, "{text}");
    serde_json::from_str(&text).unwrap()
}

async fn read_status(response: Response) -> (StatusCode, Value) {
    let status = response.status();
    let text = response.text().await.unwrap();
    (status, serde_json::from_str(&text).unwrap_or(Value::Null))
}

async fn assert_error(response: Response, status: StatusCode, code: &str) {
    let (actual, body) = read_status(response).await;
    assert_eq!(actual, status, "{body}");
    assert_eq!(body["code"], code, "{body}");
}

async fn attachment_id(response: Response) -> Uuid {
    let body = read_ok(response).await;
    body["attachment_id"].as_str().unwrap().parse().unwrap()
}

fn seq_of(change: &Value) -> u64 {
    change["seq"].as_str().unwrap().parse().unwrap()
}

// ---------------------------------------------------------------------------
// HTTP route matrix
// ---------------------------------------------------------------------------

#[tokio::test]
async fn create_advance_and_replay_keep_one_branch_and_the_accepted_deadline() {
    let s = Harness::start().await;
    let draft = Uuid::new_v4();
    let (r1, r2) = (Uuid::new_v4(), Uuid::new_v4());
    let created = s.put_ok(draft, r1, revision(None, "one", &[], true)).await;
    assert_eq!(created["duplicate"], false);
    assert_eq!(created["conflict"], false);
    assert!(created["expires_at"].as_str().unwrap().ends_with('Z'));

    let advanced = s
        .put_ok(draft, r2, revision(Some(r1), "two", &[], true))
        .await;
    assert_eq!(advanced["branch_id"], created["branch_id"]);
    assert_eq!(advanced["conflict"], false);
    assert_eq!(advanced["duplicate"], false);

    let replay = s.put_ok(draft, r1, revision(None, "one", &[], true)).await;
    assert_eq!(replay["duplicate"], true);
    assert_eq!(replay["branch_id"], created["branch_id"]);
    assert_eq!(replay["expires_at"], created["expires_at"]);

    let revisions: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM draft_revisions WHERE vault_id=$1 AND draft_id=$2",
    )
    .bind(s.vault)
    .bind(draft)
    .fetch_one(&s.pool)
    .await
    .unwrap();
    assert_eq!(revisions, 2, "replay must not store a new revision");
    let head: Uuid = sqlx::query_scalar(
        "SELECT head_revision_id FROM draft_branches WHERE vault_id=$1 AND draft_id=$2",
    )
    .bind(s.vault)
    .bind(draft)
    .fetch_one(&s.pool)
    .await
    .unwrap();
    assert_eq!(head, r2);
    s.shutdown().await;
}

#[tokio::test]
async fn non_authored_save_keeps_the_branch_deadline() {
    let s = Harness::start().await;
    let draft = Uuid::new_v4();
    let (r1, r2) = (Uuid::new_v4(), Uuid::new_v4());
    let authored = s.put_ok(draft, r1, revision(None, "one", &[], true)).await;
    let route_only = s
        .put_ok(draft, r2, revision(Some(r1), "one", &[], false))
        .await;
    assert_eq!(route_only["expires_at"], authored["expires_at"]);
    assert_eq!(route_only["branch_id"], authored["branch_id"]);
    s.shutdown().await;
}

#[tokio::test]
async fn stale_base_creates_a_conflict_branch() {
    let s = Harness::start().await;
    let draft = Uuid::new_v4();
    let (r1, r2, stale) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    let first = s.put_ok(draft, r1, revision(None, "one", &[], true)).await;
    let advanced = s
        .put_ok(draft, r2, revision(Some(r1), "two", &[], true))
        .await;
    assert_eq!(advanced["conflict"], false);
    let conflict = s
        .put_ok(draft, stale, revision(Some(r1), "other", &[], true))
        .await;
    assert_eq!(conflict["conflict"], true);
    assert_eq!(conflict["duplicate"], false);
    assert_ne!(conflict["branch_id"], first["branch_id"]);
    let branches: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM draft_branches WHERE vault_id=$1 AND draft_id=$2 AND state='live'",
    )
    .bind(s.vault)
    .bind(draft)
    .fetch_one(&s.pool)
    .await
    .unwrap();
    assert_eq!(branches, 2);
    s.shutdown().await;
}

#[tokio::test]
async fn fourth_live_branch_is_rejected_with_branch_limit() {
    let s = Harness::start().await;
    let draft = Uuid::new_v4();
    for (index, text) in ["a", "b", "c"].into_iter().enumerate() {
        let created = s
            .put_ok(draft, Uuid::new_v4(), revision(None, text, &[], true))
            .await;
        assert_eq!(created["conflict"], index > 0);
    }
    assert_error(
        s.put(draft, Uuid::new_v4(), revision(None, "d", &[], true))
            .await,
        StatusCode::CONFLICT,
        "draft_branch_limit",
    )
    .await;
    s.shutdown().await;
}

#[tokio::test]
async fn resolves_branch_ids_close_same_draft_branches_and_reject_foreign_or_closed() {
    let s = Harness::start().await;
    let d1 = Uuid::new_v4();
    let d2 = Uuid::new_v4();
    let (a1, a2, a3) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    let first = s.put_ok(d1, a1, revision(None, "a", &[], true)).await;
    let second = s.put_ok(d1, a2, revision(None, "b", &[], true)).await;
    let other = s
        .put_ok(d2, Uuid::new_v4(), revision(None, "x", &[], true))
        .await;
    let (b1, b2) = (first["branch_id"].clone(), second["branch_id"].clone());
    let foreign_branch = other["branch_id"]
        .as_str()
        .unwrap()
        .parse::<Uuid>()
        .unwrap();
    let b1_uuid = b1.as_str().unwrap().parse::<Uuid>().unwrap();
    let b2_uuid = b2.as_str().unwrap().parse::<Uuid>().unwrap();

    let a3_response = s
        .put_ok(
            d1,
            a3,
            revision_resolving(Some(a2), "c", &[], true, &[b1_uuid]),
        )
        .await;
    assert_eq!(a3_response["branch_id"], b2);
    let closed: (String, Option<String>) = sqlx::query_as(
        "SELECT state,close_reason FROM draft_branches WHERE vault_id=$1 AND branch_id=$2",
    )
    .bind(s.vault)
    .bind(b1_uuid)
    .fetch_one(&s.pool)
    .await
    .unwrap();
    assert_eq!(closed, ("closed".into(), Some("resolved".into())));
    let head_sealed_null: bool = sqlx::query_scalar(
        "SELECT sealed_record IS NULL FROM draft_revisions WHERE vault_id=$1 AND revision_id=$2",
    )
    .bind(s.vault)
    .bind(a1)
    .fetch_one(&s.pool)
    .await
    .unwrap();
    assert!(head_sealed_null, "closed branch must not keep ciphertext");

    assert_error(
        s.put(
            d1,
            Uuid::new_v4(),
            revision_resolving(Some(a3), "d", &[], true, &[foreign_branch]),
        )
        .await,
        StatusCode::UNPROCESSABLE_ENTITY,
        "draft_invalid_request",
    )
    .await;
    assert_error(
        s.put(
            d1,
            Uuid::new_v4(),
            revision_resolving(Some(a3), "d", &[], true, &[b1_uuid]),
        )
        .await,
        StatusCode::UNPROCESSABLE_ENTITY,
        "draft_invalid_request",
    )
    .await;
    assert_error(
        s.put(
            d1,
            Uuid::new_v4(),
            revision_resolving(Some(a3), "d", &[], true, &[b2_uuid]),
        )
        .await,
        StatusCode::UNPROCESSABLE_ENTITY,
        "draft_invalid_request",
    )
    .await;
    s.shutdown().await;
}

#[tokio::test]
async fn terminal_drafts_reject_puts_with_their_reason() {
    let s = Harness::start().await;
    let (discarded, sent, expired) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    let (d1, d2, d3) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());

    s.put_ok(discarded, d1, revision(None, "x", &[], true))
        .await;
    assert_eq!(s.discard(discarded).await.status(), StatusCode::NO_CONTENT);
    assert_error(
        s.put(
            discarded,
            Uuid::new_v4(),
            revision(Some(d1), "y", &[], true),
        )
        .await,
        StatusCode::GONE,
        "draft_discarded",
    )
    .await;

    s.put_ok(sent, d2, revision(None, "x", &[], true)).await;
    assert_eq!(
        s.send(sent, d2, Uuid::new_v4()).await.status(),
        StatusCode::OK
    );
    assert_error(
        s.put(sent, Uuid::new_v4(), revision(Some(d2), "y", &[], true))
            .await,
        StatusCode::GONE,
        "draft_sent",
    )
    .await;

    let created = s.put_ok(expired, d3, revision(None, "x", &[], true)).await;
    let branch = created["branch_id"].as_str().unwrap().parse().unwrap();
    s.expire_branch(branch).await;
    assert_error(
        s.put(expired, Uuid::new_v4(), revision(Some(d3), "y", &[], true))
            .await,
        StatusCode::GONE,
        "draft_expired",
    )
    .await;
    s.shutdown().await;
}

#[tokio::test]
async fn put_after_branch_deadline_is_expired_before_the_worker_runs() {
    let s = Harness::start().await;
    let draft = Uuid::new_v4();
    let r1 = Uuid::new_v4();
    let created = s.put_ok(draft, r1, revision(None, "x", &[], true)).await;
    let branch: Uuid = created["branch_id"].as_str().unwrap().parse().unwrap();
    sqlx::query("UPDATE draft_branches SET expires_at=now()-interval '1 second' WHERE vault_id=$1 AND branch_id=$2")
        .bind(s.vault)
        .bind(branch)
        .execute(&s.pool)
        .await
        .unwrap();
    assert_error(
        s.put(draft, Uuid::new_v4(), revision(Some(r1), "y", &[], true))
            .await,
        StatusCode::GONE,
        "draft_expired",
    )
    .await;
    let state: String =
        sqlx::query_scalar("SELECT state FROM draft_branches WHERE vault_id=$1 AND branch_id=$2")
            .bind(s.vault)
            .bind(branch)
            .fetch_one(&s.pool)
            .await
            .unwrap();
    assert_eq!(state, "live", "the request path must not run the worker");
    s.shutdown().await;
}

#[tokio::test]
async fn send_is_a_single_winner_and_checks_head_and_command() {
    let s = Harness::start().await;
    let draft = Uuid::new_v4();
    let (r1, r2) = (Uuid::new_v4(), Uuid::new_v4());
    s.put_ok(draft, r1, revision(None, "one", &[], true)).await;
    s.put_ok(draft, r2, revision(Some(r1), "two", &[], true))
        .await;

    assert_error(
        s.send(draft, r1, Uuid::new_v4()).await,
        StatusCode::CONFLICT,
        "draft_revision_not_head",
    )
    .await;

    let (c1, c2) = (Uuid::new_v4(), Uuid::new_v4());
    let (first, second) = tokio::join!(s.send(draft, r2, c1), s.send(draft, r2, c2));
    let results = [read_status(first).await, read_status(second).await];
    let winners: Vec<&(StatusCode, Value)> = results
        .iter()
        .filter(|(status, _)| *status == StatusCode::OK)
        .collect();
    let losers: Vec<&(StatusCode, Value)> = results
        .iter()
        .filter(|(status, _)| *status == StatusCode::CONFLICT)
        .collect();
    assert_eq!(winners.len(), 1, "{results:?}");
    assert_eq!(losers.len(), 1, "{results:?}");
    assert_eq!(winners[0].1["duplicate"], false);
    assert_eq!(losers[0].1["code"], "draft_already_sent");

    let winning: Uuid = sqlx::query_scalar(
        "SELECT command_id FROM draft_terminals WHERE vault_id=$1 AND draft_id=$2",
    )
    .bind(s.vault)
    .bind(draft)
    .fetch_one(&s.pool)
    .await
    .unwrap();
    let replay = read_ok(s.send(draft, r2, winning).await).await;
    assert_eq!(replay["duplicate"], true);
    assert_eq!(replay["command_id"], winning.to_string());
    assert_error(
        s.send(draft, r2, Uuid::new_v4()).await,
        StatusCode::CONFLICT,
        "draft_already_sent",
    )
    .await;
    s.shutdown().await;
}

#[tokio::test]
async fn send_closes_branches_clears_ciphertext_and_converts_holds() {
    let s = Harness::start().await;
    let draft = Uuid::new_v4();
    let (r1, r2) = (Uuid::new_v4(), Uuid::new_v4());
    let a1 = s.draft_attachment(draft, b"head image").await;
    let a2 = s.draft_attachment(draft, b"losing image").await;
    s.put_ok(draft, r1, revision(None, "head", &[a1], true))
        .await;
    s.put_ok(draft, r2, revision(None, "branch", &[a2], true))
        .await;

    assert_eq!(
        s.send(draft, r1, Uuid::new_v4()).await.status(),
        StatusCode::OK
    );

    let live_branches: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM draft_branches WHERE vault_id=$1 AND draft_id=$2 AND state='live'",
    )
    .bind(s.vault)
    .bind(draft)
    .fetch_one(&s.pool)
    .await
    .unwrap();
    assert_eq!(live_branches, 0);
    let superseded: i64 = sqlx::query_scalar("SELECT count(*) FROM draft_branches WHERE vault_id=$1 AND draft_id=$2 AND close_reason='superseded'")
        .bind(s.vault)
        .bind(draft)
        .fetch_one(&s.pool)
        .await
        .unwrap();
    assert_eq!(superseded, 2);
    let ciphertext_left: i64 = sqlx::query_scalar("SELECT count(*) FROM draft_revisions WHERE vault_id=$1 AND draft_id=$2 AND sealed_record IS NOT NULL")
        .bind(s.vault)
        .bind(draft)
        .fetch_one(&s.pool)
        .await
        .unwrap();
    assert_eq!(ciphertext_left, 0);

    let head_holds: Vec<(String, bool)> = sqlx::query_as("SELECT kind, expires_at > now()+interval '29 days' FROM draft_attachment_holds WHERE vault_id=$1 AND attachment_id=$2 AND expires_at>now()")
        .bind(s.vault)
        .bind(a1)
        .fetch_all(&s.pool)
        .await
        .unwrap();
    assert_eq!(head_holds, vec![("send".to_owned(), true)]);
    let losing_live: i64 = sqlx::query_scalar("SELECT count(*) FROM draft_attachment_holds WHERE vault_id=$1 AND attachment_id=$2 AND expires_at>now()")
        .bind(s.vault)
        .bind(a2)
        .fetch_one(&s.pool)
        .await
        .unwrap();
    assert_eq!(losing_live, 0, "non-head holds must expire now");
    s.shutdown().await;
}

#[tokio::test]
async fn discard_is_idempotent_and_reports_terminal_conflicts() {
    let s = Harness::start().await;
    let (discarded, sent) = (Uuid::new_v4(), Uuid::new_v4());
    let (r1, r2) = (Uuid::new_v4(), Uuid::new_v4());
    s.put_ok(discarded, r1, revision(None, "x", &[], true))
        .await;
    assert_eq!(s.discard(discarded).await.status(), StatusCode::NO_CONTENT);
    assert_eq!(s.discard(discarded).await.status(), StatusCode::NO_CONTENT);
    let terminals: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM draft_terminals WHERE vault_id=$1 AND draft_id=$2",
    )
    .bind(s.vault)
    .bind(discarded)
    .fetch_one(&s.pool)
    .await
    .unwrap();
    assert_eq!(terminals, 1);
    assert_error(
        s.send(discarded, r1, Uuid::new_v4()).await,
        StatusCode::GONE,
        "draft_discarded",
    )
    .await;

    s.put_ok(sent, r2, revision(None, "y", &[], true)).await;
    assert_eq!(
        s.send(sent, r2, Uuid::new_v4()).await.status(),
        StatusCode::OK
    );
    assert_error(s.discard(sent).await, StatusCode::GONE, "draft_sent").await;
    s.shutdown().await;
}

#[tokio::test]
async fn reserve_with_draft_id_is_gone_after_the_draft_is_terminal() {
    let s = Harness::start().await;
    let (discarded, sent, expired, live) = (
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
    );
    let cipher = ciphertext(b"reserve");

    s.discard(discarded).await;
    assert_error(
        s.reserve_draft(&s.owner_token, discarded, &cipher).await,
        StatusCode::GONE,
        "draft_discarded",
    )
    .await;

    s.put_ok(sent, Uuid::new_v4(), revision(None, "x", &[], true))
        .await;
    let sent_head: Uuid = sqlx::query_scalar(
        "SELECT head_revision_id FROM draft_branches WHERE vault_id=$1 AND draft_id=$2",
    )
    .bind(s.vault)
    .bind(sent)
    .fetch_one(&s.pool)
    .await
    .unwrap();
    assert_eq!(
        s.send(sent, sent_head, Uuid::new_v4()).await.status(),
        StatusCode::OK
    );
    assert_error(
        s.reserve_draft(&s.owner_token, sent, &cipher).await,
        StatusCode::GONE,
        "draft_sent",
    )
    .await;

    let created = s
        .put_ok(expired, Uuid::new_v4(), revision(None, "x", &[], true))
        .await;
    s.expire_branch(created["branch_id"].as_str().unwrap().parse().unwrap())
        .await;
    assert_error(
        s.reserve_draft(&s.owner_token, expired, &cipher).await,
        StatusCode::GONE,
        "draft_expired",
    )
    .await;

    let response = s.reserve_draft(&s.owner_token, live, &cipher).await;
    assert_eq!(response.status(), StatusCode::OK);
    s.shutdown().await;
}

#[tokio::test]
async fn finalize_after_the_draft_became_terminal_is_gone_and_queues_the_object() {
    let s = Harness::start_with_maintenance().await;
    let draft = Uuid::new_v4();
    let cipher = ciphertext(b"late finalize");
    let attachment = attachment_id(s.reserve_draft(&s.owner_token, draft, &cipher).await).await;
    assert_eq!(
        s.upload(attachment, cipher).await.status(),
        StatusCode::NO_CONTENT
    );
    let key = s.object_key(attachment).await;
    assert!(s.storage.head_bytes(&key).await.is_ok());
    assert_eq!(s.discard(draft).await.status(), StatusCode::NO_CONTENT);

    assert_error(
        s.finalize(attachment).await,
        StatusCode::GONE,
        "draft_discarded",
    )
    .await;
    s.wait_for_storage_cleanup(&key).await;
    assert!(
        !sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM upload_reservations WHERE vault_id=$1 AND attachment_id=$2)",
        )
        .bind(s.vault)
        .bind(attachment)
        .fetch_one(&s.pool)
        .await
        .unwrap()
    );
    let attached: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM attachments WHERE vault_id=$1 AND attachment_id=$2)",
    )
    .bind(s.vault)
    .bind(attachment)
    .fetch_one(&s.pool)
    .await
    .unwrap();
    assert!(!attached);
    s.shutdown().await;
}

#[tokio::test]
async fn worker_keeps_draft_attachments_that_are_referenced_published_or_held_elsewhere() {
    let s = Harness::start().await;
    let draft = Uuid::new_v4();
    let other = Uuid::new_v4();

    let referenced = s.draft_attachment(draft, b"referenced").await;
    sqlx::query("INSERT INTO attachment_record_references(vault_id,attachment_id,producer_device_id,producer_sequence,registered_by_device_id) VALUES($1,$2,$3,1,$3)")
        .bind(s.vault)
        .bind(referenced)
        .bind(s.owner_device)
        .execute(&s.pool)
        .await
        .unwrap();

    let published = s.draft_attachment(draft, b"published").await;
    let copy = Client::new()
        .post(s.url(&format!("/v1/attachments/{published}/public-copies")))
        .bearer_auth(&s.owner_token)
        .header("x-file-name", "photo.png")
        .body(png(32))
        .send()
        .await
        .unwrap();
    read_ok(copy).await;

    let held_elsewhere = s.draft_attachment(draft, b"other draft").await;
    s.put_ok(
        other,
        Uuid::new_v4(),
        revision(None, "other", &[held_elsewhere], true),
    )
    .await;

    let control = s.draft_attachment(draft, b"control").await;

    // Expire every hold owned by the first draft; the other draft's hold stays live.
    sqlx::query("UPDATE draft_attachment_holds SET expires_at=now()-interval '1 second' WHERE vault_id=$1 AND draft_id=$2")
        .bind(s.vault)
        .bind(draft)
        .execute(&s.pool)
        .await
        .unwrap();
    let control_key = s.object_key(control).await;
    expire_due(&s.pool, 10).await.unwrap();

    for kept in [referenced, published, held_elsewhere] {
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM attachments WHERE vault_id=$1 AND attachment_id=$2)",
        )
        .bind(s.vault)
        .bind(kept)
        .fetch_one(&s.pool)
        .await
        .unwrap();
        assert!(exists, "{kept} must survive draft expiry");
    }
    let control_exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM attachments WHERE vault_id=$1 AND attachment_id=$2)",
    )
    .bind(s.vault)
    .bind(control)
    .fetch_one(&s.pool)
    .await
    .unwrap();
    assert!(!control_exists, "the unprotected control must be expired");
    let queued: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM storage_deletions WHERE object_key=$1 AND reason='draft_expired'",
    )
    .bind(&control_key)
    .fetch_one(&s.pool)
    .await
    .unwrap();
    assert_eq!(queued, 1);
    s.shutdown().await;
}

#[tokio::test]
async fn expiry_revisits_a_draft_image_after_its_public_copy_stops_protecting_it() {
    let s = Harness::start_with_maintenance().await;
    let draft = Uuid::new_v4();
    let attachment = s.draft_attachment(draft, b"published then expired").await;
    let key = s.object_key(attachment).await;
    read_ok(
        Client::new()
            .post(s.url(&format!("/v1/attachments/{attachment}/public-copies")))
            .bearer_auth(&s.owner_token)
            .header("x-file-name", "photo.png")
            .body(png(32))
            .send()
            .await
            .unwrap(),
    )
    .await;
    sqlx::query("UPDATE draft_attachment_holds SET expires_at=now()-interval '1 second' WHERE vault_id=$1 AND attachment_id=$2")
        .bind(s.vault)
        .bind(attachment)
        .execute(&s.pool)
        .await
        .unwrap();
    expire_due(&s.pool, 10).await.unwrap();
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM attachments WHERE vault_id=$1 AND attachment_id=$2)",
    )
    .bind(s.vault)
    .bind(attachment)
    .fetch_one(&s.pool)
    .await
    .unwrap();
    assert!(
        exists,
        "the ready public copy protects the private attachment"
    );

    sqlx::query("UPDATE public_attachment_copies SET retired_at=now() WHERE vault_id=$1 AND attachment_id=$2")
        .bind(s.vault)
        .bind(attachment)
        .execute(&s.pool)
        .await
        .unwrap();
    expire_due(&s.pool, 10).await.unwrap();
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM attachments WHERE vault_id=$1 AND attachment_id=$2)",
    )
    .bind(s.vault)
    .bind(attachment)
    .fetch_one(&s.pool)
    .await
    .unwrap();
    assert!(
        !exists,
        "an unheld attachment is revisited without a due branch"
    );
    s.wait_for_storage_cleanup(&key).await;
    s.shutdown().await;
}

#[tokio::test]
async fn release_is_refused_while_a_draft_hold_is_live() {
    let s = Harness::start().await;
    let draft = Uuid::new_v4();
    let attachment = s.draft_attachment(draft, b"held release").await;
    // Establish an otherwise valid release proof, so only the live hold can
    // explain the refusal (not an invalid generation or absent references).
    sqlx::query("UPDATE vaults SET next_cursor=10,replay_floor_cursor=10,compaction_generation=1 WHERE vault_id=$1")
        .bind(s.vault).execute(&s.pool).await.unwrap();
    sqlx::query("INSERT INTO attachment_record_references(vault_id,attachment_id,producer_device_id,producer_sequence,registered_by_device_id) VALUES($1,$2,$3,7,$3)")
        .bind(s.vault).bind(attachment).bind(s.owner_device).execute(&s.pool).await.unwrap();
    sqlx::query("INSERT INTO compacted_records(vault_id,producer_device_id,producer_sequence,envelope_id,cipher_digest,original_cursor) VALUES($1,$2,7,$3,$4,5)")
        .bind(s.vault).bind(s.owner_device).bind(Uuid::new_v4()).bind(vec![0_u8;32]).execute(&s.pool).await.unwrap();
    let release = |generation: &str, cursor: &str| {
        Client::new()
            .delete(s.url(&format!("/v1/attachments/{attachment}")))
            .bearer_auth(&s.owner_token)
            .json(&json!({"compaction_generation": generation, "release_before_cursor": cursor}))
            .send()
    };
    assert_error(
        release("1", "10").await.unwrap(),
        StatusCode::CONFLICT,
        "attachment_release_not_proven",
    )
    .await;

    s.put_ok(
        draft,
        Uuid::new_v4(),
        revision(None, "held", &[attachment], true),
    )
    .await;
    assert_error(
        release("1", "10").await.unwrap(),
        StatusCode::CONFLICT,
        "attachment_release_not_proven",
    )
    .await;
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM attachments WHERE vault_id=$1 AND attachment_id=$2)",
    )
    .bind(s.vault)
    .bind(attachment)
    .fetch_one(&s.pool)
    .await
    .unwrap();
    assert!(exists);
    sqlx::query("UPDATE draft_attachment_holds SET expires_at=now()-interval '1 second' WHERE vault_id=$1 AND attachment_id=$2")
        .bind(s.vault).bind(attachment).execute(&s.pool).await.unwrap();
    assert_eq!(
        release("1", "10").await.unwrap().status(),
        StatusCode::NO_CONTENT
    );
    s.shutdown().await;
}

#[tokio::test]
async fn changes_page_by_vault_cursor_and_bootstrap_returns_tombstones() {
    let s = Harness::start().await;
    let (d1, d2, d3, d4) = (
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
    );
    s.put_ok(d1, Uuid::new_v4(), revision(None, "live", &[], true))
        .await;

    s.put_ok(d2, Uuid::new_v4(), revision(None, "discarded", &[], true))
        .await;
    s.discard(d2).await;

    let b1 = s
        .put_ok(d3, Uuid::new_v4(), revision(None, "first", &[], true))
        .await;
    let r3_first = Uuid::new_v4();
    let b2 = s
        .put_ok(d3, r3_first, revision(None, "second", &[], true))
        .await;
    let r3_head = Uuid::new_v4();
    s.put_ok(
        d3,
        r3_head,
        revision_resolving(
            Some(r3_first),
            "third",
            &[],
            true,
            &[b1["branch_id"].as_str().unwrap().parse().unwrap()],
        ),
    )
    .await;
    assert_eq!(b2["conflict"], true);

    let b4 = s
        .put_ok(d4, Uuid::new_v4(), revision(None, "expired", &[], true))
        .await;
    s.expire_branch(b4["branch_id"].as_str().unwrap().parse().unwrap())
        .await;

    // Sending closes multiple branches and emits a terminal in one transaction.
    // Each row needs its own cursor or a one-item page would skip tombstones.
    let sent = Uuid::new_v4();
    let sent_head = Uuid::new_v4();
    s.put_ok(sent, sent_head, revision(None, "send", &[], true))
        .await;
    s.put_ok(sent, Uuid::new_v4(), revision(None, "conflict", &[], true))
        .await;
    read_ok(s.send(sent, sent_head, Uuid::new_v4()).await).await;

    let foreign = foreign_owner(&s.pool).await;
    read_ok(
        s.put_as(
            &foreign,
            Uuid::new_v4(),
            Uuid::new_v4(),
            revision(None, "foreign", &[], true),
        )
        .await,
    )
    .await;
    let foreign_changes = read_ok(s.changes_as(&foreign, "after=0").await).await;
    assert_eq!(
        foreign_changes["next_after"], "1",
        "cursor counters are per vault"
    );

    let first_page = s.changes("after=0&limit=2").await;
    assert_eq!(first_page["changes"].as_array().unwrap().len(), 2);
    assert_eq!(first_page["more"], true);
    let zero_limit = s.changes("after=0&limit=0").await;
    assert_eq!(zero_limit["changes"].as_array().unwrap().len(), 1);
    let zero_limit_after = zero_limit["next_after"].as_str().unwrap();
    assert!(zero_limit_after.parse::<u64>().unwrap() > 0);

    let mut zero_limit_drain_after = zero_limit_after.to_owned();
    loop {
        let page = s
            .changes(&format!("after={zero_limit_drain_after}&limit=1"))
            .await;
        if page["more"] == false {
            break;
        }
        let next_after = page["next_after"].as_str().unwrap();
        assert_ne!(next_after, zero_limit_drain_after);
        zero_limit_drain_after = next_after.to_owned();
    }

    let mut all: Vec<Value> = Vec::new();
    let mut after = "0".to_owned();
    loop {
        let page = s.changes(&format!("after={after}&limit=1")).await;
        let changes = page["changes"].as_array().unwrap().clone();
        assert!(changes.len() <= 1);
        all.extend(changes);
        after = page["next_after"].as_str().unwrap().to_owned();
        if page["more"] == false {
            break;
        }
    }
    let unpaged = s.changes("after=0").await;
    assert_eq!(unpaged["changes"].as_array().unwrap().len(), all.len());
    assert_eq!(unpaged["more"], false);
    assert!(
        all.windows(2)
            .all(|pair| seq_of(&pair[0]) < seq_of(&pair[1]))
    );
    assert_eq!(after, all.last().unwrap()["seq"].as_str().unwrap());
    let tail = s.changes(&format!("after={after}")).await;
    assert!(tail["changes"].as_array().unwrap().is_empty());
    assert_eq!(tail["more"], false);
    assert_eq!(tail["next_after"], after);

    let kind = |change: &Value| change["kind"].as_str().unwrap().to_owned();
    assert!(all.iter().any(|change| kind(change) == "head"
        && change["draft_id"] == d1.to_string()
        && !change["sealed_record"].as_str().unwrap().is_empty()));
    assert!(all.iter().any(|change| kind(change) == "terminal"
        && change["draft_id"] == d2.to_string()
        && change["reason"] == "discarded"));
    assert!(all.iter().any(|change| kind(change) == "branch_closed"
        && change["branch_id"] == b1["branch_id"]
        && change["reason"] == "resolved"));
    assert!(all.iter().any(|change| kind(change) == "branch_closed"
        && change["branch_id"] == b4["branch_id"]
        && change["reason"] == "expired"));
    assert!(all.iter().any(|change| kind(change) == "terminal"
        && change["draft_id"] == d4.to_string()
        && change["reason"] == "expired"));
    assert_eq!(
        s.changes_as(&s.owner_token, "after=-1").await.status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        s.changes_as(&s.owner_token, "after=abc").await.status(),
        StatusCode::BAD_REQUEST
    );
    s.shutdown().await;
}

#[tokio::test]
async fn another_vault_cannot_see_or_modify_drafts() {
    let s = Harness::start().await;
    let draft = Uuid::new_v4();
    let (r1, r2) = (Uuid::new_v4(), Uuid::new_v4());
    let attachment = s.draft_attachment(draft, b"vault one").await;
    let owned = s
        .put_ok(draft, r1, revision(None, "one", &[attachment], true))
        .await;
    let foreign = foreign_owner(&s.pool).await;

    let foreign_list = read_ok(s.changes_as(&foreign, "after=0").await).await;
    assert!(foreign_list["changes"].as_array().unwrap().is_empty());

    assert_error(
        s.put_as(
            &foreign,
            draft,
            r2,
            revision(Some(r1), "stolen", &[attachment], true),
        )
        .await,
        StatusCode::UNPROCESSABLE_ENTITY,
        "draft_attachment_unavailable",
    )
    .await;
    assert_error(
        s.send_as(&foreign, draft, r1, Uuid::new_v4()).await,
        StatusCode::CONFLICT,
        "draft_revision_not_head",
    )
    .await;
    assert_eq!(
        s.discard_as(&foreign, draft).await.status(),
        StatusCode::NO_CONTENT
    );

    let owner_view = s.changes("after=0").await;
    let heads: Vec<&Value> = owner_view["changes"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|change| change["kind"] == "head" && change["branch_id"] == owned["branch_id"])
        .collect();
    assert_eq!(heads.len(), 1);
    assert_eq!(heads[0]["revision_id"], r1.to_string());
    assert!(
        !owner_view["changes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|change| change["kind"] == "terminal")
    );
    let continued = s
        .put(draft, r2, revision(Some(r1), "two", &[attachment], true))
        .await;
    assert_eq!(continued.status(), StatusCode::OK);
    s.shutdown().await;
}
