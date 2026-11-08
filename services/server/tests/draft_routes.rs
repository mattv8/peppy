use std::{env, net::SocketAddr, sync::Arc, time::Duration};

use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode, header},
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use http_body_util::BodyExt;
use peppy_hosted_api::drafts::{MAX_DRAFT_ATTACHMENTS, MAX_DRAFT_RECORD_BYTES};
use peppy_server::{
    ServerBuilder,
    api::{AccessPolicy, AccessPrincipal, Operation, PolicyFuture, create_owner},
    config::Config,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{PgPool, postgres::PgPoolOptions};
use tower::ServiceExt;
use url::Url;
use uuid::Uuid;

/// Allows exactly the listed operations and reports nothing else.
struct AllowList(Vec<Operation>);

impl AccessPolicy for AllowList {
    fn authorize(
        &self,
        _principal: AccessPrincipal,
        operation: Operation,
    ) -> PolicyFuture<'_, bool> {
        Box::pin(async move { self.0.contains(&operation) })
    }
}

struct Harness {
    pool: PgPool,
    admin: PgPool,
    schema: String,
    router: Router,
    token: String,
}

impl Harness {
    async fn start(allowed: &[Operation]) -> Self {
        let database_url = env::var("TEST_DATABASE_URL")
            .expect("TEST_DATABASE_URL is required; integration tests never silently skip");
        let admin = PgPoolOptions::new()
            .max_connections(2)
            .connect(&database_url)
            .await
            .unwrap();
        let schema = format!("peppy_draft_routes_{}", Uuid::new_v4().simple());
        sqlx::query(&format!("CREATE SCHEMA {schema}"))
            .execute(&admin)
            .await
            .unwrap();
        let mut isolated: Url = database_url.parse().unwrap();
        isolated
            .query_pairs_mut()
            .append_pair("options", &format!("-csearch_path={schema}"));
        let pool = PgPoolOptions::new()
            .max_connections(4)
            .connect(isolated.as_str())
            .await
            .unwrap();
        peppy_server::MIGRATOR.run(&pool).await.unwrap();

        let mut config = Config::from_env().expect("complete S3 test configuration is required");
        config.bind_addr = "127.0.0.1:0".parse::<SocketAddr>().unwrap();
        config.database_url = isolated.to_string();
        config.release_identity = "draft-routes-test".into();
        config.replay_retention = Duration::from_secs(86_400);
        let server = ServerBuilder::new(config, pool.clone())
            .access_policy(Arc::new(AllowList(allowed.to_vec())))
            .build()
            .await
            .unwrap();

        let vault = Uuid::new_v4();
        let (profile, fingerprint) = profile(vault, 1);
        let owner = create_owner(&pool, profile, vec![7, 8, 9], fingerprint, 1)
            .await
            .unwrap();
        Self {
            pool,
            admin,
            schema,
            router: server.router(),
            token: owner.device_token,
        }
    }

    async fn call(&self, method: &str, path: &str, body: Option<Value>) -> (StatusCode, Value) {
        let builder = Request::builder()
            .method(method)
            .uri(path)
            .header(header::AUTHORIZATION, format!("Bearer {}", self.token));
        let request = match body {
            Some(json) => builder
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(json.to_string())),
            None => builder.body(Body::empty()),
        }
        .unwrap();
        let response = self.router.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    async fn count(&self, table: &str) -> i64 {
        sqlx::query_scalar(&format!("SELECT count(*) FROM {table}"))
            .fetch_one(&self.pool)
            .await
            .unwrap()
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

fn revision_path(draft: Uuid, revision: Uuid) -> String {
    format!("/v1/drafts/{draft}/revisions/{revision}")
}

fn revision_body(base: Option<Uuid>, record: &[u8], attachments: &[Uuid]) -> Value {
    json!({
        "base_revision_id": base,
        "sealed_record": URL_SAFE_NO_PAD.encode(record),
        "attachment_ids": attachments,
        "authored": true,
        "resolves_branch_ids": [],
    })
}

fn denied(status: StatusCode, body: &Value) -> bool {
    status == StatusCode::FORBIDDEN && body["code"] == "operation_not_permitted"
}

#[tokio::test]
async fn list_is_authorized_as_sync() {
    let without_sync = Harness::start(&[Operation::Upload]).await;
    let (status, body) = without_sync.call("GET", "/v1/drafts", None).await;
    assert!(denied(status, &body), "list without Sync: {status} {body}");
    without_sync.shutdown().await;

    let with_sync = Harness::start(&[Operation::Sync]).await;
    let (status, body) = with_sync.call("GET", "/v1/drafts", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["changes"], json!([]));
    with_sync.shutdown().await;
}

#[tokio::test]
async fn put_is_authorized_as_upload_and_denial_writes_nothing() {
    let denied_harness = Harness::start(&[Operation::Sync, Operation::Send]).await;
    let (draft, revision) = (Uuid::new_v4(), Uuid::new_v4());
    let (status, body) = denied_harness
        .call(
            "PUT",
            &revision_path(draft, revision),
            Some(revision_body(None, b"sealed", &[])),
        )
        .await;
    assert!(denied(status, &body), "put without Upload: {status} {body}");
    assert_eq!(denied_harness.count("draft_branches").await, 0);
    assert_eq!(denied_harness.count("draft_revisions").await, 0);
    assert_eq!(denied_harness.count("draft_incarnations").await, 0);
    denied_harness.shutdown().await;

    let allowed = Harness::start(&[Operation::Upload]).await;
    let (status, body) = allowed
        .call(
            "PUT",
            &revision_path(draft, revision),
            Some(revision_body(None, b"sealed", &[])),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["conflict"], false);
    assert_eq!(body["duplicate"], false);
    allowed.shutdown().await;
}

#[tokio::test]
async fn send_and_discard_use_their_own_operations() {
    let draft = Uuid::new_v4();
    let send = json!({"revision_id": Uuid::new_v4(), "command_id": Uuid::new_v4()});

    let upload_only = Harness::start(&[Operation::Upload]).await;
    let (status, body) = upload_only
        .call(
            "POST",
            &format!("/v1/drafts/{draft}/send"),
            Some(send.clone()),
        )
        .await;
    assert!(denied(status, &body), "send without Send: {status} {body}");
    let (status, body) = upload_only
        .call("POST", &format!("/v1/drafts/{draft}/discard"), None)
        .await;
    assert!(
        denied(status, &body),
        "discard without AttachmentDelete: {status} {body}"
    );
    assert_eq!(upload_only.count("draft_terminals").await, 0);
    upload_only.shutdown().await;

    let send_only = Harness::start(&[Operation::Send]).await;
    let (status, body) = send_only
        .call("POST", &format!("/v1/drafts/{draft}/send"), Some(send))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "draft_revision_not_head");
    assert_eq!(send_only.count("draft_terminals").await, 0);
    send_only.shutdown().await;

    let discard_only = Harness::start(&[Operation::AttachmentDelete]).await;
    let (status, body) = discard_only
        .call("POST", &format!("/v1/drafts/{draft}/discard"), None)
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    assert_eq!(discard_only.count("draft_terminals").await, 1);
    discard_only.shutdown().await;
}

#[tokio::test]
async fn put_validation_boundaries_reject_before_writing() {
    let harness = Harness::start(&[
        Operation::Upload,
        Operation::Sync,
        Operation::Send,
        Operation::AttachmentDelete,
    ])
    .await;
    let (draft, revision) = (Uuid::new_v4(), Uuid::new_v4());
    let path = revision_path(draft, revision);
    let foreign = Uuid::new_v4();
    let invalid = "draft_invalid_request";

    let mut unknown_field = revision_body(None, b"sealed", &[]);
    unknown_field["unexpected"] = json!(true);
    let (status, _) = harness.call("PUT", &path, Some(unknown_field)).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "unknown field");

    let mut malformed = revision_body(None, b"sealed", &[]);
    malformed["sealed_record"] = json!("not base64!");
    assert_rejected(
        &harness,
        &path,
        malformed,
        StatusCode::UNPROCESSABLE_ENTITY,
        invalid,
        "malformed encoding",
    )
    .await;

    let oversized = vec![0_u8; MAX_DRAFT_RECORD_BYTES + 1];
    assert_rejected(
        &harness,
        &path,
        revision_body(None, &oversized, &[]),
        StatusCode::PAYLOAD_TOO_LARGE,
        "draft_record_too_large",
        "oversized record",
    )
    .await;

    let too_many: Vec<Uuid> = (0..=MAX_DRAFT_ATTACHMENTS)
        .map(|_| Uuid::new_v4())
        .collect();
    assert_rejected(
        &harness,
        &path,
        revision_body(None, b"sealed", &too_many),
        StatusCode::UNPROCESSABLE_ENTITY,
        invalid,
        "attachment count above limit",
    )
    .await;

    let duplicate = Uuid::new_v4();
    assert_rejected(
        &harness,
        &path,
        revision_body(None, b"sealed", &[duplicate, duplicate]),
        StatusCode::UNPROCESSABLE_ENTITY,
        invalid,
        "duplicate attachment ids",
    )
    .await;

    assert_rejected(
        &harness,
        &path,
        revision_body(None, b"sealed", &[Uuid::nil()]),
        StatusCode::UNPROCESSABLE_ENTITY,
        invalid,
        "nil attachment id",
    )
    .await;

    assert_rejected(
        &harness,
        &path,
        revision_body(None, b"sealed", &[foreign]),
        StatusCode::UNPROCESSABLE_ENTITY,
        "draft_attachment_unavailable",
        "foreign attachment",
    )
    .await;

    let at_limit: Vec<Uuid> = (0..MAX_DRAFT_ATTACHMENTS).map(|_| Uuid::new_v4()).collect();
    assert_rejected(
        &harness,
        &path,
        revision_body(None, b"sealed", &at_limit),
        StatusCode::UNPROCESSABLE_ENTITY,
        "draft_attachment_unavailable",
        "attachment count at limit passes validation",
    )
    .await;

    let mut resolves_revision = revision_body(None, b"sealed", &[]);
    resolves_revision["resolves_branch_ids"] = json!([revision]);
    assert_rejected(
        &harness,
        &path,
        resolves_revision,
        StatusCode::UNPROCESSABLE_ENTITY,
        invalid,
        "resolves a non-branch id",
    )
    .await;

    let mut duplicate_resolves = revision_body(None, b"sealed", &[]);
    let branch = Uuid::new_v4();
    duplicate_resolves["resolves_branch_ids"] = json!([branch, branch]);
    assert_rejected(
        &harness,
        &path,
        duplicate_resolves,
        StatusCode::UNPROCESSABLE_ENTITY,
        invalid,
        "duplicate resolves ids",
    )
    .await;

    for table in [
        "draft_branches",
        "draft_revisions",
        "draft_attachment_holds",
    ] {
        assert_eq!(
            harness.count(table).await,
            0,
            "{table} after rejected requests"
        );
    }

    let exact_limit = vec![0_u8; MAX_DRAFT_RECORD_BYTES];
    let (status, body) = harness
        .call("PUT", &path, Some(revision_body(None, &exact_limit, &[])))
        .await;
    assert_eq!(status, StatusCode::OK, "record at exact limit: {body}");
    assert_eq!(harness.count("draft_branches").await, 1);
    harness.shutdown().await;
}

async fn assert_rejected(
    harness: &Harness,
    path: &str,
    body: Value,
    expected_status: StatusCode,
    expected_code: &str,
    case: &str,
) {
    let (status, response) = harness.call("PUT", path, Some(body)).await;
    assert_eq!(status, expected_status, "{case}: {response}");
    assert_eq!(response["code"], expected_code, "{case}");
}

#[tokio::test]
async fn revision_head_clears_only_the_exact_prior_head() {
    let harness = Harness::start(&[Operation::Upload]).await;
    let draft = Uuid::new_v4();
    let (first, second, sibling) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());

    let (status, first_body) = harness
        .call(
            "PUT",
            &revision_path(draft, first),
            Some(revision_body(None, b"one", &[])),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{first_body}");
    let branch = first_body["branch_id"].clone();

    let (status, second_body) = harness
        .call(
            "PUT",
            &revision_path(draft, second),
            Some(revision_body(Some(first), b"two", &[])),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{second_body}");
    assert_eq!(
        second_body["branch_id"], branch,
        "successor stays on the same branch"
    );

    let (status, duplicate_body) = harness
        .call(
            "PUT",
            &revision_path(draft, second),
            Some(revision_body(Some(first), b"two", &[])),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{duplicate_body}");
    assert_eq!(duplicate_body["duplicate"], true);

    let (status, sibling_body) = harness
        .call(
            "PUT",
            &revision_path(draft, sibling),
            Some(revision_body(None, b"three", &[])),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{sibling_body}");
    assert_eq!(sibling_body["conflict"], true, "second live branch");

    let rows: Vec<(Uuid, bool)> = sqlx::query_as(
        "SELECT revision_id, sealed_record IS NULL FROM draft_revisions WHERE draft_id=$1 ORDER BY revision_id",
    )
    .bind(draft)
    .fetch_all(&harness.pool)
    .await
    .unwrap();
    let cleared = |id: Uuid| {
        rows.iter()
            .find(|(revision, _)| *revision == id)
            .map(|(_, sealed_cleared)| *sealed_cleared)
    };
    assert_eq!(
        cleared(first),
        Some(true),
        "superseded head loses its sealed record"
    );
    assert_eq!(
        cleared(second),
        Some(false),
        "current head keeps its sealed record"
    );
    assert_eq!(
        cleared(sibling),
        Some(false),
        "other branch head is untouched"
    );
    harness.shutdown().await;
}

#[tokio::test]
async fn list_cursor_accepts_only_canonical_unsigned_decimals() {
    let harness = Harness::start(&[Operation::Sync]).await;
    let (status, body) = harness.call("GET", "/v1/drafts", None).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "absent cursor defaults to zero: {body}"
    );
    let cases: [(&str, StatusCode); 10] = [
        ("", StatusCode::BAD_REQUEST),
        ("0", StatusCode::OK),
        ("1", StatusCode::OK),
        ("9223372036854775807", StatusCode::OK),
        ("01", StatusCode::BAD_REQUEST),
        ("%2B1", StatusCode::BAD_REQUEST),
        ("-1", StatusCode::BAD_REQUEST),
        ("1x", StatusCode::BAD_REQUEST),
        ("9223372036854775808", StatusCode::BAD_REQUEST),
        ("%20", StatusCode::BAD_REQUEST),
    ];
    for (raw, expected) in cases {
        let (status, body) = harness
            .call("GET", &format!("/v1/drafts?after={raw}"), None)
            .await;
        assert_eq!(status, expected, "after={raw:?}: {body}");
        if expected == StatusCode::BAD_REQUEST {
            assert_eq!(body["code"], "invalid_cursor", "after={raw:?}");
        }
    }
    harness.shutdown().await;
}
