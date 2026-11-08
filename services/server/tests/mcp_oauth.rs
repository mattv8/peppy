use std::{net::SocketAddr, time::Duration};

use axum::{
    Router,
    body::Body,
    extract::ConnectInfo,
    http::{Method, Request, Response, StatusCode},
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use http_body_util::BodyExt;
use peppy_server::{ServerBuilder, config::Config};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{PgPool, postgres::PgPoolOptions};
use tower::ServiceExt;
use url::Url;
use uuid::Uuid;

const ORIGIN: &str = "http://localhost:3000/";

struct Fixture {
    admin: PgPool,
    pool: PgPool,
    schema: String,
    vault: Uuid,
    mcp_device: Uuid,
    owner_token: String,
    member_token: String,
}

impl Fixture {
    async fn new() -> Self {
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&std::env::var("TEST_DATABASE_URL").expect("disposable PostgreSQL URL"))
            .await
            .unwrap();
        let schema = format!("mcp_oauth_{}", Uuid::new_v4().simple());
        sqlx::query(&format!("CREATE SCHEMA {schema}"))
            .execute(&admin)
            .await
            .unwrap();
        let mut database_url: Url = std::env::var("TEST_DATABASE_URL").unwrap().parse().unwrap();
        database_url
            .query_pairs_mut()
            .append_pair("options", &format!("-csearch_path={schema}"));
        let pool = PgPoolOptions::new()
            .max_connections(8)
            .connect(database_url.as_str())
            .await
            .unwrap();
        peppy_server::MIGRATOR.run(&pool).await.unwrap();

        let vault = Uuid::new_v4();
        let owner = Uuid::new_v4();
        let member = Uuid::new_v4();
        let mcp_device = Uuid::new_v4();
        let owner_token = "a".repeat(96);
        let member_token = "b".repeat(96);
        sqlx::query("INSERT INTO vaults(vault_id,public_key_profile,encrypted_vault_check_header,key_epoch,profile_fingerprint) VALUES($1,'{}',$2,1,$3)")
            .bind(vault).bind(vec![1_u8]).bind("11".repeat(32)).execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO devices(vault_id,device_id,role,public_key,profile_fingerprint,key_epoch,name,kind) VALUES($1,$2,'owner','{}',$5,1,'owner','desktop'),($1,$3,'device','{}',$5,1,'member','desktop'),($1,$4,'device','{}',$5,1,'mcp','desktop')")
            .bind(vault).bind(owner).bind(member).bind(mcp_device).bind("11".repeat(32)).execute(&pool).await.unwrap();
        for (token, device) in [(&owner_token, owner), (&member_token, member)] {
            sqlx::query(
                "INSERT INTO device_credentials(token_digest,vault_id,device_id) VALUES($1,$2,$3)",
            )
            .bind(Sha256::digest(token.as_bytes()).as_slice())
            .bind(vault)
            .bind(device)
            .execute(&pool)
            .await
            .unwrap();
        }
        sqlx::query("INSERT INTO mcp_devices(vault_id,device_id) VALUES($1,$2)")
            .bind(vault)
            .bind(mcp_device)
            .execute(&pool)
            .await
            .unwrap();

        Self {
            admin,
            pool,
            schema,
            vault,
            mcp_device,
            owner_token,
            member_token,
        }
    }

    async fn app(&self) -> Router {
        ServerBuilder::community(
            Config {
                bind_addr: "127.0.0.1:0".parse::<SocketAddr>().unwrap(),
                database_url: "unused".into(),
                release_identity: "mcp-oauth-test".into(),
                s3: None,
                public_api_url: Some(Url::parse(ORIGIN).unwrap()),
                public_attachment_url: None,
                vault_attachment_quota_bytes: 1_000_000,
                trusted_proxy_cidrs: Vec::new(),
                replay_retention: Duration::from_secs(60),
                relay_url: None,
                mcp_worker_url: None,
                mcp_worker_token_file: None,
                web_client: None,
                web_live_trusted_origin: None,
            },
            self.pool.clone(),
        )
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

async fn request(
    app: Router,
    method: Method,
    uri: &str,
    body: Body,
    headers: &[(&str, &str)],
) -> Response<Body> {
    let mut builder = Request::builder().method(method).uri(uri);
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    app.oneshot(builder.body(body).unwrap()).await.unwrap()
}

async fn request_from(
    app: Router,
    method: Method,
    uri: &str,
    body: Body,
    headers: &[(&str, &str)],
    peer: SocketAddr,
) -> Response<Body> {
    let mut builder = Request::builder().method(method).uri(uri);
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let mut request = builder.body(body).unwrap();
    request.extensions_mut().insert(ConnectInfo(peer));
    app.oneshot(request).await.unwrap()
}

async fn json(response: Response<Body>) -> Value {
    serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap()
}

async fn register(app: Router) -> String {
    let response = request(
        app.clone(),
        Method::POST,
        "/oauth/register",
        Body::from(
            json!({
                "client_name":"Test client", "redirect_uris":["https://client.example/callback"],
                "scope":"conversations:read messages:send",
            "grant_types":["authorization_code","refresh_token"], "response_types":["code"],
                "token_endpoint_auth_method":"none", "application_type":"web",
                "client_uri":"https://client.example"
                    })
            .to_string(),
        ),
        &[("content-type", "application/json")],
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    json(response).await["client_id"]
        .as_str()
        .unwrap()
        .to_owned()
}

fn challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

async fn authorize(app: Router, client_id: &str, verifier: &str) -> String {
    authorize_with_redirect(app, client_id, verifier, "https://client.example/callback").await
}

async fn authorize_with_redirect(
    app: Router,
    client_id: &str,
    verifier: &str,
    redirect_uri: &str,
) -> String {
    let mut query = url::form_urlencoded::Serializer::new(String::new());
    query.append_pair("response_type", "code");
    query.append_pair("client_id", client_id);
    query.append_pair("redirect_uri", redirect_uri);
    query.append_pair("state", "opaque-state");
    query.append_pair("scope", "conversations:read messages:send");
    query.append_pair("resource", "http://localhost:3000/mcp");
    query.append_pair("code_challenge", &challenge(verifier));
    query.append_pair("code_challenge_method", "S256");
    let uri = format!("/oauth/authorize?{}", query.finish());
    let response = request(app, Method::GET, &uri, Body::empty(), &[]).await;
    assert_eq!(response.status(), StatusCode::TEMPORARY_REDIRECT);
    Url::parse(
        response
            .headers()
            .get("location")
            .unwrap()
            .to_str()
            .unwrap(),
    )
    .unwrap()
    .query_pairs()
    .find(|(key, _)| key == "mcp_authorize")
    .unwrap()
    .1
    .into_owned()
}

async fn register_with_redirect(app: Router, redirect_uri: &str) -> String {
    let response = request(
        app,
        Method::POST,
        "/oauth/register",
        Body::from(
            json!({
                "client_name": "test client",
                "redirect_uris": [redirect_uri],
                "scope": "conversations:read messages:send",
            })
            .to_string(),
        ),
        &[("content-type", "application/json")],
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    json(response).await["client_id"]
        .as_str()
        .unwrap()
        .to_owned()
}

async fn approve(app: Router, authorization_id: &str, owner_token: &str) -> String {
    approve_with_scopes(
        app,
        authorization_id,
        owner_token,
        json!(["conversations:read"]),
    )
    .await
}

async fn approve_with_scopes(
    app: Router,
    authorization_id: &str,
    owner_token: &str,
    scopes: Value,
) -> String {
    let response = request(
        app,
        Method::POST,
        &format!("/v1/mcp/authorizations/{authorization_id}/approve"),
        Body::from(json!({"scopes":scopes}).to_string()),
        &[
            ("content-type", "application/json"),
            ("authorization", &format!("Bearer {owner_token}")),
        ],
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let decision = json(response).await;
    Url::parse(decision["redirectUri"].as_str().unwrap())
        .unwrap()
        .query_pairs()
        .find(|(key, _)| key == "code")
        .unwrap()
        .1
        .into_owned()
}

async fn exchange(app: Router, client_id: &str, code: &str, verifier: &str) -> Value {
    let body = format!(
        "grant_type=authorization_code&client_id={client_id}&code={code}&redirect_uri=https%3A%2F%2Fclient.example%2Fcallback&resource=http%3A%2F%2Flocalhost%3A3000%2Fmcp&code_verifier={verifier}"
    );
    let response = request(
        app,
        Method::POST,
        "/oauth/token",
        Body::from(body),
        &[("content-type", "application/x-www-form-urlencoded")],
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["cache-control"], "no-store");
    json(response).await
}

async fn refresh_request(app: Router, client_id: &str, refresh: &str) -> Response<Body> {
    request(
        app,
        Method::POST,
        "/oauth/token",
        Body::from(format!(
            "grant_type=refresh_token&client_id={client_id}&refresh_token={refresh}&resource=http%3A%2F%2Flocalhost%3A3000%2Fmcp"
        )),
        &[("content-type", "application/x-www-form-urlencoded")],
    )
    .await
}

async fn wait_for_database_lock(pool: &PgPool) {
    for _ in 0..100 {
        let waiting: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM pg_locks WHERE locktype='transactionid' AND NOT granted)",
        )
        .fetch_one(pool)
        .await
        .unwrap();
        if waiting {
            return;
        }
        tokio::task::yield_now().await;
    }
    panic!("request did not wait for the row lock");
}

#[tokio::test]
async fn oauth_http_lifecycle_enforces_pkce_owner_scope_rotation_and_revocation() {
    let fixture = Fixture::new().await;
    let app = fixture.app().await;
    let verifier = "a-valid-pkce-verifier-that-is-long-enough-to-satisfy-rfc7636";
    let client_id = register(app.clone()).await;

    let invalid_redirect = request(app.clone(), Method::GET, &format!("/oauth/authorize?response_type=code&client_id={client_id}&redirect_uri=http%3A%2F%2Fevil.example%2Fcallback&state=x&scope=conversations%3Aread&resource=http%3A%2F%2Flocalhost%3A3000%2Fmcp&code_challenge={}&code_challenge_method=S256", challenge(verifier)), Body::empty(), &[]).await;
    assert_eq!(invalid_redirect.status(), StatusCode::BAD_REQUEST);
    let wrong_resource = request(app.clone(), Method::GET, &format!("/oauth/authorize?response_type=code&client_id={client_id}&redirect_uri=https%3A%2F%2Fclient.example%2Fcallback&state=x&scope=conversations%3Aread&resource=https%3A%2F%2Fevil.example%2Fmcp&code_challenge={}&code_challenge_method=S256", challenge(verifier)), Body::empty(), &[]).await;
    assert_eq!(wrong_resource.status(), StatusCode::BAD_REQUEST);

    let authorization_id = authorize(app.clone(), &client_id, verifier).await;
    let member_view = request(
        app.clone(),
        Method::GET,
        &format!("/v1/mcp/authorizations/{authorization_id}"),
        Body::empty(),
        &[("authorization", &format!("Bearer {}", fixture.member_token))],
    )
    .await;
    assert_eq!(member_view.status(), StatusCode::FORBIDDEN);
    let view = request(
        app.clone(),
        Method::GET,
        &format!("/v1/mcp/authorizations/{authorization_id}"),
        Body::empty(),
        &[("authorization", &format!("Bearer {}", fixture.owner_token))],
    )
    .await;
    assert_eq!(view.status(), StatusCode::OK);
    assert_eq!(
        json(view).await["requestedScopes"],
        json!(["conversations:read", "messages:send"])
    );
    let view = request(
        app.clone(),
        Method::GET,
        &format!("/v1/mcp/authorizations/{authorization_id}"),
        Body::empty(),
        &[("authorization", &format!("Bearer {}", fixture.owner_token))],
    )
    .await;
    let view = json(view).await;
    assert_eq!(view["clientId"], client_id);
    assert_eq!(view["redirectOrigin"], "https://client.example");
    assert_eq!(view["redirectIsLoopback"], false);
    assert!(view.get("state").is_none());
    assert!(view.get("code").is_none());

    let code = approve(app.clone(), &authorization_id, &fixture.owner_token).await;
    let bound: (Uuid, Uuid) =
        sqlx::query_as("SELECT vault_id,device_id FROM mcp_oauth_codes WHERE digest=$1")
            .bind(Sha256::digest(code.as_bytes()).as_slice())
            .fetch_one(&fixture.pool)
            .await
            .unwrap();
    assert_eq!(bound, (fixture.vault, fixture.mcp_device));
    let wrong_verifier = request(app.clone(), Method::POST, "/oauth/token", Body::from(format!("grant_type=authorization_code&client_id={client_id}&code={code}&redirect_uri=https%3A%2F%2Fclient.example%2Fcallback&resource=http%3A%2F%2Flocalhost%3A3000%2Fmcp&code_verifier=wrong-verifier")), &[("content-type", "application/x-www-form-urlencoded")]).await;
    assert_eq!(wrong_verifier.status(), StatusCode::BAD_REQUEST);
    assert_eq!(wrong_verifier.headers()["cache-control"], "no-store");
    assert_eq!(json(wrong_verifier).await, json!({"error":"invalid_grant"}));
    let tokens = exchange(app.clone(), &client_id, &code, verifier).await;
    assert_eq!(tokens["scope"], "conversations:read");
    let access = tokens["access_token"].as_str().unwrap().to_owned();
    let refresh = tokens["refresh_token"].as_str().unwrap().to_owned();
    let replay = request(app.clone(), Method::POST, "/oauth/token", Body::from(format!("grant_type=authorization_code&client_id={client_id}&code={code}&redirect_uri=https%3A%2F%2Fclient.example%2Fcallback&resource=http%3A%2F%2Flocalhost%3A3000%2Fmcp&code_verifier={verifier}")), &[("content-type", "application/x-www-form-urlencoded")]).await;
    assert_eq!(replay.status(), StatusCode::BAD_REQUEST);

    let authenticated = request(
        app.clone(),
        Method::POST,
        "/mcp",
        Body::from("{}"),
        &[
            ("authorization", &format!("Bearer {access}")),
            ("host", "localhost:3000"),
        ],
    )
    .await;
    assert_ne!(
        authenticated.status(),
        StatusCode::UNAUTHORIZED,
        "valid access token reached the MCP router"
    );

    let refresh_response = request(app.clone(), Method::POST, "/oauth/token", Body::from(format!("grant_type=refresh_token&client_id={client_id}&refresh_token={refresh}&resource=http%3A%2F%2Flocalhost%3A3000%2Fmcp")), &[("content-type", "application/x-www-form-urlencoded")]).await;
    assert_eq!(refresh_response.status(), StatusCode::OK);
    let refreshed = json(refresh_response).await;
    assert_eq!(refreshed["scope"], "conversations:read");
    let replacement_access = refreshed["access_token"].as_str().unwrap().to_owned();
    let refresh_replay = request(app.clone(), Method::POST, "/oauth/token", Body::from(format!("grant_type=refresh_token&client_id={client_id}&refresh_token={refresh}&resource=http%3A%2F%2Flocalhost%3A3000%2Fmcp")), &[("content-type", "application/x-www-form-urlencoded")]).await;
    assert_eq!(refresh_replay.status(), StatusCode::BAD_REQUEST);
    assert_eq!(json(refresh_replay).await, json!({"error":"invalid_grant"}));
    let revoked_family_access = request(
        app.clone(),
        Method::POST,
        "/mcp",
        Body::from("{}"),
        &[
            ("authorization", &format!("Bearer {replacement_access}")),
            ("host", "localhost:3000"),
        ],
    )
    .await;
    assert_eq!(revoked_family_access.status(), StatusCode::UNAUTHORIZED);

    let expired_code = approve(
        app.clone(),
        &authorize(app.clone(), &client_id, verifier).await,
        &fixture.owner_token,
    )
    .await;
    let expired_tokens = exchange(app.clone(), &client_id, &expired_code, verifier).await;
    let expired_access = expired_tokens["access_token"].as_str().unwrap();
    sqlx::query("UPDATE mcp_oauth_tokens SET expires_at=now()-interval '1 second' WHERE digest=$1")
        .bind(Sha256::digest(expired_access.as_bytes()).as_slice())
        .execute(&fixture.pool)
        .await
        .unwrap();
    let expired = request(
        app.clone(),
        Method::POST,
        "/mcp",
        Body::from("{}"),
        &[
            ("authorization", &format!("Bearer {expired_access}")),
            ("host", "localhost:3000"),
        ],
    )
    .await;
    assert_eq!(expired.status(), StatusCode::UNAUTHORIZED);

    let expired_refresh_code = approve(
        app.clone(),
        &authorize(app.clone(), &client_id, verifier).await,
        &fixture.owner_token,
    )
    .await;
    let expired_refresh_tokens =
        exchange(app.clone(), &client_id, &expired_refresh_code, verifier).await;
    let expired_refresh = expired_refresh_tokens["refresh_token"].as_str().unwrap();
    sqlx::query("UPDATE mcp_oauth_tokens SET expires_at=now()-interval '1 second' WHERE digest=$1")
        .bind(Sha256::digest(expired_refresh.as_bytes()).as_slice())
        .execute(&fixture.pool)
        .await
        .unwrap();
    let token_count: i64 = sqlx::query_scalar("SELECT count(*) FROM mcp_oauth_tokens")
        .fetch_one(&fixture.pool)
        .await
        .unwrap();
    let expired_refresh_response = request(
        app.clone(),
        Method::POST,
        "/oauth/token",
        Body::from(format!("grant_type=refresh_token&client_id={client_id}&refresh_token={expired_refresh}&resource=http%3A%2F%2Flocalhost%3A3000%2Fmcp")),
        &[("content-type", "application/x-www-form-urlencoded")],
    )
    .await;
    assert_eq!(expired_refresh_response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM mcp_oauth_tokens")
            .fetch_one(&fixture.pool)
            .await
            .unwrap(),
        token_count
    );

    let revoke_code = approve(
        app.clone(),
        &authorize(app.clone(), &client_id, verifier).await,
        &fixture.owner_token,
    )
    .await;
    let revoke_tokens = exchange(app.clone(), &client_id, &revoke_code, verifier).await;
    let revoke_access = revoke_tokens["access_token"].as_str().unwrap();
    let revoke = request(
        app.clone(),
        Method::POST,
        "/oauth/revoke",
        Body::from(format!("token={revoke_access}&client_id={client_id}")),
        &[("content-type", "application/x-www-form-urlencoded")],
    )
    .await;
    assert_eq!(revoke.status(), StatusCode::OK);
    let revoked_access = request(
        app.clone(),
        Method::POST,
        "/mcp",
        Body::from("{}"),
        &[
            ("authorization", &format!("Bearer {revoke_access}")),
            ("host", "localhost:3000"),
        ],
    )
    .await;
    assert_eq!(revoked_access.status(), StatusCode::UNAUTHORIZED);

    let device_code = approve(
        app.clone(),
        &authorize(app.clone(), &client_id, verifier).await,
        &fixture.owner_token,
    )
    .await;
    let device_tokens = exchange(app.clone(), &client_id, &device_code, verifier).await;
    let device_access = device_tokens["access_token"].as_str().unwrap();
    let device_revocation =
        sqlx::query("UPDATE devices SET revoked_at=now() WHERE vault_id=$1 AND device_id=$2")
            .bind(fixture.vault)
            .bind(fixture.mcp_device)
            .execute(&fixture.pool)
            .await
            .unwrap();
    assert_eq!(device_revocation.rows_affected(), 1);
    let revoked_device = request(
        app.clone(),
        Method::POST,
        "/mcp",
        Body::from("{}"),
        &[
            ("authorization", &format!("Bearer {device_access}")),
            ("host", "localhost:3000"),
        ],
    )
    .await;
    assert_eq!(revoked_device.status(), StatusCode::UNAUTHORIZED);
    fixture.cleanup().await;
}

#[tokio::test]
async fn oauth_endpoints_use_oauth_errors() {
    let fixture = Fixture::new().await;
    let app = fixture.app().await;

    let registration = request(
        app.clone(),
        Method::POST,
        "/oauth/register",
        Body::from(json!({"client_name":"","redirect_uris":[]}).to_string()),
        &[("content-type", "application/json")],
    )
    .await;
    assert_eq!(registration.status(), StatusCode::BAD_REQUEST);
    assert_eq!(registration.headers()["cache-control"], "no-store");
    assert_eq!(
        json(registration).await,
        json!({"error":"invalid_client_metadata"})
    );

    let malformed_registration = request(
        app.clone(),
        Method::POST,
        "/oauth/register",
        Body::from("not-json"),
        &[("content-type", "application/json")],
    )
    .await;
    assert_eq!(malformed_registration.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        json(malformed_registration).await,
        json!({"error":"invalid_client_metadata"})
    );

    let wrongly_typed_registration = request(
        app.clone(),
        Method::POST,
        "/oauth/register",
        Body::from(json!({"client_name":42,"redirect_uris":[]}).to_string()),
        &[("content-type", "application/json")],
    )
    .await;
    assert_eq!(wrongly_typed_registration.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        json(wrongly_typed_registration).await,
        json!({"error":"invalid_client_metadata"})
    );

    let unsupported_grant = request(
        app.clone(),
        Method::POST,
        "/oauth/token",
        Body::from("grant_type=client_credentials"),
        &[("content-type", "application/x-www-form-urlencoded")],
    )
    .await;
    assert_eq!(unsupported_grant.status(), StatusCode::BAD_REQUEST);
    assert_eq!(unsupported_grant.headers()["cache-control"], "no-store");
    assert_eq!(
        json(unsupported_grant).await,
        json!({"error":"unsupported_grant_type"})
    );

    let malformed_revoke = request(
        app.clone(),
        Method::POST,
        "/oauth/revoke",
        Body::empty(),
        &[("content-type", "application/x-www-form-urlencoded")],
    )
    .await;
    assert_eq!(malformed_revoke.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        json(malformed_revoke).await,
        json!({"error":"invalid_request"})
    );

    let revoke = request(
        app,
        Method::POST,
        "/oauth/revoke",
        Body::from("token=unknown&client_id=unknown"),
        &[("content-type", "application/x-www-form-urlencoded")],
    )
    .await;
    assert_eq!(revoke.status(), StatusCode::OK);
    assert_eq!(revoke.headers()["cache-control"], "no-store");
    fixture.cleanup().await;
}

#[tokio::test]
async fn authorization_view_redacts_callback_path_and_marks_ipv6_loopback() {
    let fixture = Fixture::new().await;
    let app = fixture.app().await;
    let verifier = "a-valid-pkce-verifier-that-is-long-enough-to-satisfy-rfc7636";

    for (redirect_uri, loopback) in [
        ("http://[::1]:43123/callback", true),
        (
            "https://client.example/callback?code=adversarial&state=opaque",
            false,
        ),
    ] {
        let client_id = register_with_redirect(app.clone(), redirect_uri).await;
        let authorization_id =
            authorize_with_redirect(app.clone(), &client_id, verifier, redirect_uri).await;
        let response = request(
            app.clone(),
            Method::GET,
            &format!("/v1/mcp/authorizations/{authorization_id}"),
            Body::empty(),
            &[("authorization", &format!("Bearer {}", fixture.owner_token))],
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let view = json(response).await;
        assert_eq!(view["redirectIsLoopback"], loopback);
        assert_eq!(
            view["redirectOrigin"],
            if loopback {
                json!("http://[::1]:43123")
            } else {
                json!("https://client.example")
            }
        );
        assert!(view.get("code").is_none());
        assert!(view.get("state").is_none());
    }
    fixture.cleanup().await;
}

#[tokio::test]
async fn concurrent_refresh_rotation_and_reuse_revokes_the_entire_family() {
    let fixture = Fixture::new().await;
    let app = fixture.app().await;
    let verifier = "a-valid-pkce-verifier-that-is-long-enough-to-satisfy-rfc7636";
    let client_id = register(app.clone()).await;
    let authorization_id = authorize(app.clone(), &client_id, verifier).await;
    let code = approve(app.clone(), &authorization_id, &fixture.owner_token).await;
    let tokens = exchange(app.clone(), &client_id, &code, verifier).await;
    let refresh0 = tokens["refresh_token"].as_str().unwrap().to_owned();
    let family: Uuid = sqlx::query_scalar("SELECT family_id FROM mcp_oauth_tokens WHERE digest=$1")
        .bind(Sha256::digest(refresh0.as_bytes()).as_slice())
        .fetch_one(&fixture.pool)
        .await
        .unwrap();
    let first_rotation = request(
        app.clone(),
        Method::POST,
        "/oauth/token",
        Body::from(format!(
            "grant_type=refresh_token&client_id={client_id}&refresh_token={refresh0}&resource=http%3A%2F%2Flocalhost%3A3000%2Fmcp"
        )),
        &[("content-type", "application/x-www-form-urlencoded")],
    )
    .await;
    assert_eq!(first_rotation.status(), StatusCode::OK);
    let refresh1 = json(first_rotation).await["refresh_token"]
        .as_str()
        .unwrap()
        .to_owned();

    sqlx::query(&format!(
        "CREATE FUNCTION mcp_block_rotated_token() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.family_id = '{family}'::uuid THEN PERFORM pg_advisory_xact_lock(42719, 1); END IF; RETURN NEW; END $$"
    ))
        .execute(&fixture.pool)
        .await
        .unwrap();
    sqlx::query("CREATE TRIGGER mcp_block_rotated_token BEFORE INSERT ON mcp_oauth_tokens FOR EACH ROW EXECUTE FUNCTION mcp_block_rotated_token()")
        .execute(&fixture.pool)
        .await
        .unwrap();
    let mut blocker = fixture.pool.acquire().await.unwrap();
    sqlx::query("SELECT pg_advisory_lock(42719, 1)")
        .execute(&mut *blocker)
        .await
        .unwrap();
    let rotate_r1 = format!(
        "grant_type=refresh_token&client_id={client_id}&refresh_token={refresh1}&resource=http%3A%2F%2Flocalhost%3A3000%2Fmcp"
    );
    let rotate = tokio::spawn(request(
        app.clone(),
        Method::POST,
        "/oauth/token",
        Body::from(rotate_r1),
        &[("content-type", "application/x-www-form-urlencoded")],
    ));
    for _ in 0..100 {
        let waiting: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_locks WHERE locktype='advisory' AND classid=42719 AND objid=1 AND NOT granted)")
            .fetch_one(&fixture.pool)
            .await
            .unwrap();
        if waiting {
            break;
        }
        tokio::task::yield_now().await;
    }
    let waiting: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_locks WHERE locktype='advisory' AND classid=42719 AND objid=1 AND NOT granted)")
        .fetch_one(&fixture.pool)
        .await
        .unwrap();
    assert!(waiting, "R1 rotation must block before committing R2");
    let replay_r0 = tokio::spawn(request(
        app,
        Method::POST,
        "/oauth/token",
        Body::from(format!(
            "grant_type=refresh_token&client_id={client_id}&refresh_token={refresh0}&resource=http%3A%2F%2Flocalhost%3A3000%2Fmcp"
        )),
        &[("content-type", "application/x-www-form-urlencoded")],
    ));
    let family_lock: i64 = sqlx::query_scalar("SELECT hashtextextended($1, 0)")
        .bind(family.to_string())
        .fetch_one(&fixture.pool)
        .await
        .unwrap();
    for _ in 0..100 {
        let waiting: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_locks WHERE locktype='advisory' AND classid=(($1::bigint >> 32) & 4294967295)::oid AND objid=($1::bigint & 4294967295)::oid AND objsubid=1 AND NOT granted)")
            .bind(family_lock)
            .fetch_one(&fixture.pool)
            .await
            .unwrap();
        if waiting {
            break;
        }
        tokio::task::yield_now().await;
    }
    let replay_waiting: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_locks WHERE locktype='advisory' AND classid=(($1::bigint >> 32) & 4294967295)::oid AND objid=($1::bigint & 4294967295)::oid AND objsubid=1 AND NOT granted)")
        .bind(family_lock)
        .fetch_one(&fixture.pool)
        .await
        .unwrap();
    assert!(
        replay_waiting,
        "R0 replay must wait for R1's family rotation"
    );
    sqlx::query("SELECT pg_advisory_unlock(42719, 1)")
        .execute(&mut *blocker)
        .await
        .unwrap();
    drop(blocker);
    let rotation = rotate.await.unwrap();
    let replay = replay_r0.await.unwrap();
    assert_eq!(rotation.status(), StatusCode::OK);
    assert_eq!(replay.status(), StatusCode::BAD_REQUEST);
    let live_tokens: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM mcp_oauth_tokens WHERE family_id=$1 AND revoked_at IS NULL",
    )
    .bind(family)
    .fetch_one(&fixture.pool)
    .await
    .unwrap();
    assert_eq!(live_tokens, 0, "family descendants must be revoked");
    sqlx::query("DROP TRIGGER mcp_block_rotated_token ON mcp_oauth_tokens")
        .execute(&fixture.pool)
        .await
        .unwrap();
    sqlx::query("DROP FUNCTION mcp_block_rotated_token()")
        .execute(&fixture.pool)
        .await
        .unwrap();
    fixture.cleanup().await;
}

#[tokio::test]
async fn oauth_validates_token_parameters_client_binding_and_redirect_metadata() {
    let fixture = Fixture::new().await;
    let app = fixture.app().await;
    let verifier = "a-valid-pkce-verifier-that-is-long-enough-to-satisfy-rfc7636";
    let client_id = register(app.clone()).await;

    for body in [
        "",
        "grant_type=",
        "grant_type=authorization_code&client_id=known",
        "grant_type=authorization_code&client_id=known&code=supplied&code_verifier=supplied",
        "grant_type=authorization_code&client_id=known&code=supplied&redirect_uri=https%3A%2F%2Fclient.example%2Fcallback",
        "grant_type=authorization_code&code=supplied&code_verifier=supplied&redirect_uri=https%3A%2F%2Fclient.example%2Fcallback",
        "grant_type=refresh_token&client_id=known",
        "grant_type=refresh_token&client_id=known&refresh_token=supplied",
    ] {
        let response = request(
            app.clone(),
            Method::POST,
            "/oauth/token",
            Body::from(body),
            &[("content-type", "application/x-www-form-urlencoded")],
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(json(response).await, json!({"error":"invalid_request"}));
    }

    let unsupported_grant = request(
        app.clone(),
        Method::POST,
        "/oauth/token",
        Body::from("grant_type=client_credentials"),
        &[("content-type", "application/x-www-form-urlencoded")],
    )
    .await;
    assert_eq!(unsupported_grant.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        json(unsupported_grant).await,
        json!({"error":"unsupported_grant_type"})
    );

    let unknown_client = request(
        app.clone(),
        Method::POST,
        "/oauth/token",
        Body::from("grant_type=refresh_token&refresh_token=supplied&client_id=unknown&resource=http%3A%2F%2Flocalhost%3A3000%2Fmcp"),
        &[("content-type", "application/x-www-form-urlencoded")],
    )
    .await;
    assert_eq!(unknown_client.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        json(unknown_client).await,
        json!({"error":"invalid_client"})
    );

    let authorization_id = authorize(app.clone(), &client_id, verifier).await;
    let code = approve(app.clone(), &authorization_id, &fixture.owner_token).await;
    let tokens = exchange(app.clone(), &client_id, &code, verifier).await;
    let access = tokens["access_token"].as_str().unwrap();
    let other_client = register(app.clone()).await;
    let cross_client_revoke = request(
        app.clone(),
        Method::POST,
        "/oauth/revoke",
        Body::from(format!("token={access}&client_id={other_client}")),
        &[("content-type", "application/x-www-form-urlencoded")],
    )
    .await;
    assert_eq!(cross_client_revoke.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        json(cross_client_revoke).await,
        json!({"error":"invalid_client"})
    );
    let still_valid = request(
        app.clone(),
        Method::POST,
        "/mcp",
        Body::from("{}"),
        &[
            ("authorization", &format!("Bearer {access}")),
            ("host", "localhost:3000"),
        ],
    )
    .await;
    assert_ne!(still_valid.status(), StatusCode::UNAUTHORIZED);

    let valid_revoke = request(
        app.clone(),
        Method::POST,
        "/oauth/revoke",
        Body::from(format!("token={access}&client_id={client_id}")),
        &[("content-type", "application/x-www-form-urlencoded")],
    )
    .await;
    assert_eq!(valid_revoke.status(), StatusCode::OK);
    let repeated_revoke = request(
        app.clone(),
        Method::POST,
        "/oauth/revoke",
        Body::from(format!("token={access}&client_id={client_id}")),
        &[("content-type", "application/x-www-form-urlencoded")],
    )
    .await;
    assert_eq!(repeated_revoke.status(), StatusCode::OK);

    let unsupported_scope = request(
        app.clone(),
        Method::POST,
        "/oauth/register",
        Body::from(json!({"client_name":"test","redirect_uris":["https://client.example/callback"],"scope":"unknown:scope"}).to_string()),
        &[("content-type", "application/json")],
    )
    .await;
    assert_eq!(unsupported_scope.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        json(unsupported_scope).await,
        json!({"error":"invalid_client_metadata"})
    );

    let invalid_redirect = request(
        app.clone(),
        Method::POST,
        "/oauth/register",
        Body::from(
            json!({"client_name":"test","redirect_uris":["http://evil.example/callback"]})
                .to_string(),
        ),
        &[("content-type", "application/json")],
    )
    .await;
    assert_eq!(invalid_redirect.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        json(invalid_redirect).await,
        json!({"error":"invalid_redirect_uri"})
    );

    let oversized_redirect = format!("https://client.example/{}", "a".repeat(2_048));
    let oversized_redirect = request(
        app.clone(),
        Method::POST,
        "/oauth/register",
        Body::from(json!({"client_name":"test","redirect_uris":[oversized_redirect]}).to_string()),
        &[("content-type", "application/json")],
    )
    .await;
    assert_eq!(oversized_redirect.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        json(oversized_redirect).await,
        json!({"error":"invalid_redirect_uri"})
    );
    let fragment_redirect = request(
        app.clone(),
        Method::POST,
        "/oauth/register",
        Body::from(json!({"client_name":"test","redirect_uris":["https://client.example/callback#fragment"]}).to_string()),
        &[("content-type", "application/json")],
    )
    .await;
    assert_eq!(fragment_redirect.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        json(fragment_redirect).await,
        json!({"error":"invalid_redirect_uri"})
    );
    let exact_redirect = format!(
        "https://client.example/{}",
        "a".repeat(2_048 - "https://client.example/".len())
    );
    assert_eq!(exact_redirect.len(), 2_048);
    register_with_redirect(app.clone(), &exact_redirect).await;
    fixture.cleanup().await;
}

#[tokio::test]
async fn concurrent_registrations_do_not_exceed_the_durable_capacity() {
    let fixture = Fixture::new().await;
    sqlx::query("INSERT INTO mcp_oauth_clients(client_id,client_name,redirect_uris,allowed_scopes) SELECT 'preloaded-client-' || value,'preloaded',ARRAY['https://client.example/callback'],ARRAY['conversations:read'] FROM generate_series(1,1023) value")
        .execute(&fixture.pool)
        .await
        .unwrap();
    let app = fixture.app().await;
    let responses = futures_util::future::join_all((0..8).map(|_| {
        request(
            app.clone(),
            Method::POST,
            "/oauth/register",
            Body::from(json!({"client_name":"Race client","redirect_uris":["https://client.example/callback"]}).to_string()),
            &[("content-type", "application/json")],
        )
    }))
    .await;

    assert_eq!(
        responses
            .iter()
            .filter(|response| response.status() == StatusCode::CREATED)
            .count(),
        1
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM mcp_oauth_clients")
            .fetch_one(&fixture.pool)
            .await
            .unwrap(),
        1024
    );
    fixture.cleanup().await;
}

#[tokio::test]
async fn concurrent_authorizations_do_not_exceed_the_durable_capacity() {
    let fixture = Fixture::new().await;
    let app = fixture.app().await;
    let client_id = register(app.clone()).await;
    sqlx::query("INSERT INTO mcp_oauth_authorizations(id,client_id,redirect_uri,state,code_challenge,requested_scopes,resource,expires_at) SELECT 'preloaded-authorization-' || value,$1,'https://client.example/callback','state',repeat('a',43),ARRAY['conversations:read'],'http://localhost:3000/mcp',now()+interval '10 minutes' FROM generate_series(1,4095) value")
        .bind(&client_id)
        .execute(&fixture.pool)
        .await
        .unwrap();
    let verifier = "a-valid-pkce-verifier-that-is-long-enough-to-satisfy-rfc7636";
    let uri = format!(
        "/oauth/authorize?response_type=code&client_id={client_id}&redirect_uri=https%3A%2F%2Fclient.example%2Fcallback&state=opaque-state&scope=conversations%3Aread&resource=http%3A%2F%2Flocalhost%3A3000%2Fmcp&code_challenge={}&code_challenge_method=S256",
        challenge(verifier)
    );
    let responses = futures_util::future::join_all(
        (0..8).map(|_| request(app.clone(), Method::GET, &uri, Body::empty(), &[])),
    )
    .await;

    assert_eq!(
        responses
            .iter()
            .filter(|response| response.status() == StatusCode::TEMPORARY_REDIRECT)
            .count(),
        1
    );
    assert_eq!(sqlx::query_scalar::<_, i64>("SELECT count(*) FROM mcp_oauth_authorizations WHERE expires_at > now() AND decided_at IS NULL AND denied_at IS NULL").fetch_one(&fixture.pool).await.unwrap(), 4096);
    fixture.cleanup().await;
}

#[tokio::test]
async fn oauth_registration_admission_limits_sources_and_global_anonymous_traffic() {
    let fixture = Fixture::new().await;
    let app = fixture.app().await;
    let body =
        json!({"client_name":"Limited client","redirect_uris":["https://client.example/callback"]})
            .to_string();
    let source: SocketAddr = "192.0.2.1:1234".parse().unwrap();
    for _ in 0..10 {
        assert_eq!(
            request_from(
                app.clone(),
                Method::POST,
                "/oauth/register",
                Body::from(body.clone()),
                &[("content-type", "application/json")],
                source
            )
            .await
            .status(),
            StatusCode::CREATED
        );
    }
    assert_eq!(
        request_from(
            app.clone(),
            Method::POST,
            "/oauth/register",
            Body::from(body.clone()),
            &[("content-type", "application/json")],
            source
        )
        .await
        .status(),
        StatusCode::TOO_MANY_REQUESTS
    );
    for host in 2..=91 {
        let peer = SocketAddr::new(std::net::Ipv4Addr::new(192, 0, 2, host).into(), 1234);
        assert_eq!(
            request_from(
                app.clone(),
                Method::POST,
                "/oauth/register",
                Body::from(body.clone()),
                &[("content-type", "application/json")],
                peer
            )
            .await
            .status(),
            StatusCode::CREATED
        );
    }
    assert_eq!(
        request_from(
            app,
            Method::POST,
            "/oauth/register",
            Body::from(body),
            &[("content-type", "application/json")],
            "192.0.2.92:1234".parse().unwrap()
        )
        .await
        .status(),
        StatusCode::TOO_MANY_REQUESTS
    );
    fixture.cleanup().await;
}

#[tokio::test]
async fn sweeping_unused_registrations_restores_registration_capacity() {
    let fixture = Fixture::new().await;
    sqlx::query("INSERT INTO mcp_oauth_clients(client_id,client_name,redirect_uris,allowed_scopes,created_at) SELECT 'expired-client-' || value,'expired',ARRAY['https://client.example/callback'],ARRAY['conversations:read'],now()-interval '25 hours' FROM generate_series(1,1024) value")
        .execute(&fixture.pool)
        .await
        .unwrap();
    let client_id = register(fixture.app().await).await;
    assert!(!client_id.is_empty());
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM mcp_oauth_clients")
            .fetch_one(&fixture.pool)
            .await
            .unwrap(),
        513
    );
    fixture.cleanup().await;
}

#[tokio::test]
async fn discovery_and_registration_advertise_accepted_oauth_metadata() {
    let fixture = Fixture::new().await;
    let app = fixture.app().await;
    let metadata = request(
        app.clone(),
        Method::GET,
        "/.well-known/oauth-authorization-server",
        Body::empty(),
        &[],
    )
    .await;
    assert_eq!(metadata.status(), StatusCode::OK);
    assert_eq!(
        json(metadata).await["grant_types_supported"],
        json!(["authorization_code", "refresh_token"])
    );
    let metadata = request(
        app.clone(),
        Method::GET,
        "/.well-known/oauth-authorization-server",
        Body::empty(),
        &[],
    )
    .await;
    let metadata = json(metadata).await;
    assert_eq!(metadata["response_types_supported"], json!(["code"]));
    assert_eq!(
        metadata["token_endpoint_auth_methods_supported"],
        json!(["none"])
    );
    let registration = request(
        app,
        Method::POST,
        "/oauth/register",
        Body::from(json!({"client_name":"metadata client","redirect_uris":["https://client.example/callback"]}).to_string()),
        &[("content-type", "application/json")],
    )
    .await;
    assert_eq!(registration.status(), StatusCode::CREATED);
    let registration = json(registration).await;
    assert_eq!(registration["scope"], "conversations:read");
    assert_eq!(
        registration["grant_types"],
        json!(["authorization_code", "refresh_token"])
    );
    assert_eq!(registration["response_types"], json!(["code"]));
    assert_eq!(registration["token_endpoint_auth_method"], "none");
    fixture.cleanup().await;
}

#[tokio::test]
async fn refresh_revoke_revokes_the_entire_token_family() {
    let fixture = Fixture::new().await;
    let app = fixture.app().await;
    let verifier = "a-valid-pkce-verifier-that-is-long-enough-to-satisfy-rfc7636";
    let client_id = register(app.clone()).await;
    let code = approve(
        app.clone(),
        &authorize(app.clone(), &client_id, verifier).await,
        &fixture.owner_token,
    )
    .await;
    let tokens = exchange(app.clone(), &client_id, &code, verifier).await;
    let refresh = tokens["refresh_token"].as_str().unwrap();
    let revoke = request(
        app,
        Method::POST,
        "/oauth/revoke",
        Body::from(format!("token={refresh}&client_id={client_id}")),
        &[("content-type", "application/x-www-form-urlencoded")],
    )
    .await;
    assert_eq!(revoke.status(), StatusCode::OK);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM mcp_oauth_tokens WHERE family_id=(SELECT family_id FROM mcp_oauth_tokens WHERE digest=$1) AND revoked_at IS NULL")
            .bind(Sha256::digest(refresh.as_bytes()).as_slice())
            .fetch_one(&fixture.pool).await.unwrap(),
        0
    );
    fixture.cleanup().await;
}

#[tokio::test]
async fn oauth_token_admission_preserves_unspent_refresh_tokens() {
    let fixture = Fixture::new().await;
    let app = fixture.app().await;
    let verifier = "a-valid-pkce-verifier-that-is-long-enough-to-satisfy-rfc7636";
    let client_id = register(app.clone()).await;
    let code = approve(
        app.clone(),
        &authorize(app.clone(), &client_id, verifier).await,
        &fixture.owner_token,
    )
    .await;
    let refresh = exchange(app.clone(), &client_id, &code, verifier).await["refresh_token"]
        .as_str()
        .unwrap()
        .to_owned();
    let peer = "192.0.2.55:1234".parse().unwrap();
    for _ in 0..10 {
        assert_eq!(request_from(app.clone(), Method::POST, "/oauth/token", Body::from("grant_type=refresh_token&client_id=unknown&refresh_token=unknown&resource=http%3A%2F%2Flocalhost%3A3000%2Fmcp"), &[("content-type", "application/x-www-form-urlencoded")], peer).await.status(), StatusCode::BAD_REQUEST);
    }
    let throttled = request_from(app, Method::POST, "/oauth/token", Body::from(format!("grant_type=refresh_token&client_id={client_id}&refresh_token={refresh}&resource=http%3A%2F%2Flocalhost%3A3000%2Fmcp")), &[("content-type", "application/x-www-form-urlencoded")], peer).await;
    assert_eq!(throttled.status(), StatusCode::TOO_MANY_REQUESTS);
    let used: bool =
        sqlx::query_scalar("SELECT used_at IS NOT NULL FROM mcp_oauth_tokens WHERE digest=$1")
            .bind(Sha256::digest(refresh.as_bytes()).as_slice())
            .fetch_one(&fixture.pool)
            .await
            .unwrap();
    assert!(!used);
    fixture.cleanup().await;
}

#[tokio::test]
async fn oauth_sweep_retains_recent_replay_evidence_and_prunes_expired_history_in_batches() {
    let fixture = Fixture::new().await;
    let app = fixture.app().await;
    let verifier = "a-valid-pkce-verifier-that-is-long-enough-to-satisfy-rfc7636";
    let client_id = register(app.clone()).await;
    let code = approve(
        app.clone(),
        &authorize(app.clone(), &client_id, verifier).await,
        &fixture.owner_token,
    )
    .await;
    let refresh = exchange(app.clone(), &client_id, &code, verifier).await["refresh_token"]
        .as_str()
        .unwrap()
        .to_owned();
    let family: Uuid = sqlx::query_scalar("SELECT family_id FROM mcp_oauth_tokens WHERE digest=$1")
        .bind(Sha256::digest(refresh.as_bytes()).as_slice())
        .fetch_one(&fixture.pool)
        .await
        .unwrap();
    let (grant_id, device_id): (Uuid, Uuid) =
        sqlx::query_as("SELECT grant_id,device_id FROM mcp_oauth_tokens WHERE digest=$1")
            .bind(Sha256::digest(refresh.as_bytes()).as_slice())
            .fetch_one(&fixture.pool)
            .await
            .unwrap();
    sqlx::query("UPDATE mcp_oauth_clients SET created_at=clock_timestamp()-interval '25 hours' WHERE client_id=$1")
        .bind(&client_id).execute(&fixture.pool).await.unwrap();
    sqlx::query("UPDATE mcp_oauth_tokens SET expires_at=clock_timestamp()-interval '6 days' WHERE family_id=$1")
        .bind(family).execute(&fixture.pool).await.unwrap();
    sqlx::query("UPDATE mcp_oauth_tokens SET revoked_at=clock_timestamp() WHERE digest=$1")
        .bind(Sha256::digest(refresh.as_bytes()).as_slice())
        .execute(&fixture.pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO mcp_oauth_codes(digest,vault_id,device_id,grant_id,client_id,redirect_uri,code_challenge,resource,expires_at) SELECT decode(lpad(to_hex(value),64,'0'),'hex'),$1,$2,$3,$4,'https://client.example/callback',repeat('a',43),'http://localhost:3000/mcp',clock_timestamp()-interval '2 days' FROM generate_series(1,513) value")
        .bind(fixture.vault).bind(device_id).bind(grant_id).bind(&client_id).execute(&fixture.pool).await.unwrap();
    sqlx::query("INSERT INTO mcp_oauth_tokens(digest,token_kind,family_id,vault_id,device_id,grant_id,client_id,resource,expires_at) SELECT decode(lpad(to_hex(value + 1024),64,'0'),'hex'),'access',$1,$2,$3,$4,$5,'http://localhost:3000/mcp',clock_timestamp()-interval '8 days' FROM generate_series(1,513) value")
        .bind(family).bind(fixture.vault).bind(device_id).bind(grant_id).bind(&client_id).execute(&fixture.pool).await.unwrap();
    sqlx::query("SELECT mcp_sweep_oauth()")
        .execute(&fixture.pool)
        .await
        .unwrap();
    assert_eq!(sqlx::query_scalar::<_, i64>("SELECT count(*) FROM mcp_oauth_codes WHERE client_id=$1 AND expires_at < clock_timestamp()-interval '1 day'").bind(&client_id).fetch_one(&fixture.pool).await.unwrap(), 1);
    assert_eq!(sqlx::query_scalar::<_, i64>("SELECT count(*) FROM mcp_oauth_tokens WHERE client_id=$1 AND expires_at < clock_timestamp()-interval '7 days'").bind(&client_id).fetch_one(&fixture.pool).await.unwrap(), 1);
    assert_eq!(sqlx::query_scalar::<_, i64>("SELECT count(*) FROM mcp_oauth_tokens WHERE family_id=$1 AND expires_at > clock_timestamp()-interval '7 days'").bind(family).fetch_one(&fixture.pool).await.unwrap(), 2);
    sqlx::query("SELECT mcp_sweep_oauth()")
        .execute(&fixture.pool)
        .await
        .unwrap();
    assert_eq!(sqlx::query_scalar::<_, i64>("SELECT count(*) FROM mcp_oauth_codes WHERE client_id=$1 AND expires_at < clock_timestamp()-interval '1 day'").bind(&client_id).fetch_one(&fixture.pool).await.unwrap(), 0);
    assert_eq!(sqlx::query_scalar::<_, i64>("SELECT count(*) FROM mcp_oauth_tokens WHERE client_id=$1 AND expires_at < clock_timestamp()-interval '7 days'").bind(&client_id).fetch_one(&fixture.pool).await.unwrap(), 0);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM mcp_oauth_clients WHERE client_id=$1")
            .bind(&client_id)
            .fetch_one(&fixture.pool)
            .await
            .unwrap(),
        1
    );
    fixture.cleanup().await;
}

#[tokio::test]
async fn oauth_rejects_non_adjacent_duplicate_scopes_at_every_public_boundary() {
    let fixture = Fixture::new().await;
    let app = fixture.app().await;
    let verifier = "a-valid-pkce-verifier-that-is-long-enough-to-satisfy-rfc7636";
    let registration = request(
        app.clone(),
        Method::POST,
        "/oauth/register",
        Body::from(json!({"client_name":"duplicates","redirect_uris":["https://client.example/callback"],"scope":"conversations:read messages:send conversations:read"}).to_string()),
        &[("content-type", "application/json")],
    ).await;
    assert_eq!(registration.status(), StatusCode::BAD_REQUEST);
    let client_id = register(app.clone()).await;
    let mut query = url::form_urlencoded::Serializer::new(String::new());
    query.append_pair("response_type", "code");
    query.append_pair("client_id", &client_id);
    query.append_pair("redirect_uri", "https://client.example/callback");
    query.append_pair("state", "opaque-state");
    query.append_pair(
        "scope",
        "conversations:read messages:send conversations:read",
    );
    query.append_pair("resource", "http://localhost:3000/mcp");
    query.append_pair("code_challenge", &challenge(verifier));
    query.append_pair("code_challenge_method", "S256");
    let authorization = request(
        app.clone(),
        Method::GET,
        &format!("/oauth/authorize?{}", query.finish()),
        Body::empty(),
        &[],
    )
    .await;
    assert_eq!(authorization.status(), StatusCode::BAD_REQUEST);
    let authorization_id = authorize(app.clone(), &client_id, verifier).await;
    let approval = request(
        app.clone(),
        Method::POST,
        &format!("/v1/mcp/authorizations/{authorization_id}/approve"),
        Body::from(
            json!({"scopes":["conversations:read","messages:send","conversations:read"]})
                .to_string(),
        ),
        &[
            ("content-type", "application/json"),
            ("authorization", &format!("Bearer {}", fixture.owner_token)),
        ],
    )
    .await;
    assert_eq!(approval.status(), StatusCode::BAD_REQUEST);
    let grants: i64 = sqlx::query_scalar("SELECT count(*) FROM mcp_grants")
        .fetch_one(&fixture.pool)
        .await
        .unwrap();
    assert_eq!(grants, 0);
    let pending: bool =
        sqlx::query_scalar("SELECT decided_at IS NULL FROM mcp_oauth_authorizations WHERE id=$1")
            .bind(authorization_id)
            .fetch_one(&fixture.pool)
            .await
            .unwrap();
    assert!(pending);
    fixture.cleanup().await;
}

#[tokio::test]
async fn sweep_keeps_an_old_clients_live_code_after_its_authorization_expires() {
    let fixture = Fixture::new().await;
    let app = fixture.app().await;
    let verifier = "a-valid-pkce-verifier-that-is-long-enough-to-satisfy-rfc7636";
    let client_id = register(app.clone()).await;
    let authorization_id = authorize(app.clone(), &client_id, verifier).await;
    let code = approve(app.clone(), &authorization_id, &fixture.owner_token).await;
    sqlx::query("UPDATE mcp_oauth_clients SET created_at=clock_timestamp()-interval '25 hours' WHERE client_id=$1")
        .bind(&client_id).execute(&fixture.pool).await.unwrap();
    sqlx::query("UPDATE mcp_oauth_authorizations SET expires_at=clock_timestamp()-interval '1 second' WHERE id=$1")
        .bind(&authorization_id).execute(&fixture.pool).await.unwrap();
    sqlx::query("SELECT mcp_sweep_oauth()")
        .execute(&fixture.pool)
        .await
        .unwrap();
    assert!(
        sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM mcp_oauth_clients WHERE client_id=$1)"
        )
        .bind(&client_id)
        .fetch_one(&fixture.pool)
        .await
        .unwrap()
    );
    let tokens = exchange(app, &client_id, &code, verifier).await;
    assert!(tokens["access_token"].is_string());
    fixture.cleanup().await;
}

#[tokio::test]
async fn sweep_skips_an_old_client_with_a_concurrently_inserted_authorization() {
    let fixture = Fixture::new().await;
    let client_id = register(fixture.app().await).await;
    sqlx::query("UPDATE mcp_oauth_clients SET created_at=clock_timestamp()-interval '25 hours' WHERE client_id=$1")
        .bind(&client_id).execute(&fixture.pool).await.unwrap();
    let authorization_id = format!("concurrent-{client_id}");
    let mut child_transaction = fixture.pool.begin().await.unwrap();
    sqlx::query("INSERT INTO mcp_oauth_authorizations(id,client_id,redirect_uri,state,code_challenge,requested_scopes,resource,expires_at) VALUES($1,$2,'https://client.example/callback','state',repeat('a',43),ARRAY['conversations:read'],'http://localhost:3000/mcp',clock_timestamp()+interval '10 minutes')")
        .bind(&authorization_id).bind(&client_id).execute(&mut *child_transaction).await.unwrap();
    let sweep_pool = fixture.pool.clone();
    let sweep = tokio::spawn(async move {
        sqlx::query("SELECT mcp_sweep_oauth()")
            .execute(&sweep_pool)
            .await
    });
    assert!(
        tokio::time::timeout(Duration::from_secs(2), sweep)
            .await
            .unwrap()
            .unwrap()
            .is_ok()
    );
    child_transaction.commit().await.unwrap();
    assert!(
        sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM mcp_oauth_clients WHERE client_id=$1)"
        )
        .bind(&client_id)
        .fetch_one(&fixture.pool)
        .await
        .unwrap()
    );
    assert!(
        sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM mcp_oauth_authorizations WHERE id=$1)"
        )
        .bind(&authorization_id)
        .fetch_one(&fixture.pool)
        .await
        .unwrap()
    );
    fixture.cleanup().await;
}

#[tokio::test]
async fn code_exchange_rechecks_expiry_after_a_lock_only_wait() {
    let fixture = Fixture::new().await;
    let app = fixture.app().await;
    let verifier = "a-valid-pkce-verifier-that-is-long-enough-to-satisfy-rfc7636";
    let client_id = register(app.clone()).await;
    let code = approve(
        app.clone(),
        &authorize(app.clone(), &client_id, verifier).await,
        &fixture.owner_token,
    )
    .await;
    let digest = Sha256::digest(code.as_bytes());
    sqlx::query("UPDATE mcp_oauth_codes SET expires_at=clock_timestamp()+interval '500 milliseconds' WHERE digest=$1")
        .bind(digest.as_slice())
        .execute(&fixture.pool)
        .await
        .unwrap();
    let token_count: i64 = sqlx::query_scalar("SELECT count(*) FROM mcp_oauth_tokens")
        .fetch_one(&fixture.pool)
        .await
        .unwrap();
    let mut blocker = fixture.pool.begin().await.unwrap();
    sqlx::query("SELECT 1 FROM mcp_oauth_codes WHERE digest=$1 FOR UPDATE")
        .bind(digest.as_slice())
        .execute(&mut *blocker)
        .await
        .unwrap();
    let request_code = code.clone();
    let request_client = client_id.clone();
    let exchange_request = tokio::spawn(request(
        app,
        Method::POST,
        "/oauth/token",
        Body::from(format!(
            "grant_type=authorization_code&client_id={request_client}&code={request_code}&redirect_uri=https%3A%2F%2Fclient.example%2Fcallback&resource=http%3A%2F%2Flocalhost%3A3000%2Fmcp&code_verifier={verifier}"
        )),
        &[("content-type", "application/x-www-form-urlencoded")],
    ));
    wait_for_database_lock(&fixture.pool).await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    blocker.commit().await.unwrap();
    let response = exchange_request.await.unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(json(response).await, json!({"error":"invalid_grant"}));
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM mcp_oauth_tokens")
            .fetch_one(&fixture.pool)
            .await
            .unwrap(),
        token_count
    );
    assert!(
        sqlx::query_scalar::<_, bool>(
            "SELECT used_at IS NULL FROM mcp_oauth_codes WHERE digest=$1"
        )
        .bind(digest.as_slice())
        .fetch_one(&fixture.pool)
        .await
        .unwrap()
    );
    fixture.cleanup().await;
}

#[tokio::test]
async fn refresh_rechecks_expiry_after_a_grant_lock_only_wait() {
    let fixture = Fixture::new().await;
    let app = fixture.app().await;
    let verifier = "a-valid-pkce-verifier-that-is-long-enough-to-satisfy-rfc7636";
    let client_id = register(app.clone()).await;
    let code = approve(
        app.clone(),
        &authorize(app.clone(), &client_id, verifier).await,
        &fixture.owner_token,
    )
    .await;
    let refresh = exchange(app.clone(), &client_id, &code, verifier).await["refresh_token"]
        .as_str()
        .unwrap()
        .to_owned();
    let digest = Sha256::digest(refresh.as_bytes());
    let grant_id: Uuid =
        sqlx::query_scalar("SELECT grant_id FROM mcp_oauth_tokens WHERE digest=$1")
            .bind(digest.as_slice())
            .fetch_one(&fixture.pool)
            .await
            .unwrap();
    sqlx::query("UPDATE mcp_oauth_tokens SET expires_at=clock_timestamp()+interval '500 milliseconds' WHERE digest=$1")
        .bind(digest.as_slice())
        .execute(&fixture.pool)
        .await
        .unwrap();
    let token_count: i64 = sqlx::query_scalar("SELECT count(*) FROM mcp_oauth_tokens")
        .fetch_one(&fixture.pool)
        .await
        .unwrap();
    let mut blocker = fixture.pool.begin().await.unwrap();
    sqlx::query("SELECT 1 FROM mcp_grants WHERE id=$1 FOR UPDATE")
        .bind(grant_id)
        .execute(&mut *blocker)
        .await
        .unwrap();
    let request_client = client_id.clone();
    let request_refresh = refresh.clone();
    let refresh_request =
        tokio::spawn(async move { refresh_request(app, &request_client, &request_refresh).await });
    wait_for_database_lock(&fixture.pool).await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    blocker.commit().await.unwrap();
    let response = refresh_request.await.unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(json(response).await, json!({"error":"invalid_grant"}));
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM mcp_oauth_tokens")
            .fetch_one(&fixture.pool)
            .await
            .unwrap(),
        token_count
    );
    assert!(
        sqlx::query_scalar::<_, bool>(
            "SELECT used_at IS NULL FROM mcp_oauth_tokens WHERE digest=$1"
        )
        .bind(digest.as_slice())
        .fetch_one(&fixture.pool)
        .await
        .unwrap()
    );
    fixture.cleanup().await;
}

#[tokio::test]
async fn refresh_rotation_then_oauth_revoke_serializes_without_live_descendants() {
    let fixture = Fixture::new().await;
    let app = fixture.app().await;
    let verifier = "a-valid-pkce-verifier-that-is-long-enough-to-satisfy-rfc7636";
    let client_id = register(app.clone()).await;
    let code = approve(
        app.clone(),
        &authorize(app.clone(), &client_id, verifier).await,
        &fixture.owner_token,
    )
    .await;
    let refresh = exchange(app.clone(), &client_id, &code, verifier).await["refresh_token"]
        .as_str()
        .unwrap()
        .to_owned();
    let family: Uuid = sqlx::query_scalar("SELECT family_id FROM mcp_oauth_tokens WHERE digest=$1")
        .bind(Sha256::digest(refresh.as_bytes()).as_slice())
        .fetch_one(&fixture.pool)
        .await
        .unwrap();
    sqlx::query(&format!(
        "CREATE FUNCTION mcp_block_refresh_rotation() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.family_id = '{family}'::uuid THEN PERFORM pg_advisory_xact_lock(42720, 1); END IF; RETURN NEW; END $$"
    ))
    .execute(&fixture.pool)
    .await
    .unwrap();
    sqlx::query("CREATE TRIGGER mcp_block_refresh_rotation BEFORE INSERT ON mcp_oauth_tokens FOR EACH ROW EXECUTE FUNCTION mcp_block_refresh_rotation()")
        .execute(&fixture.pool)
        .await
        .unwrap();
    let mut blocker = fixture.pool.acquire().await.unwrap();
    sqlx::query("SELECT pg_advisory_lock(42720, 1)")
        .execute(&mut *blocker)
        .await
        .unwrap();
    let rotation_client = client_id.clone();
    let rotation_refresh = refresh.clone();
    let rotation = tokio::spawn(async move {
        refresh_request(app.clone(), &rotation_client, &rotation_refresh).await
    });
    for _ in 0..100 {
        let waiting: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_locks WHERE locktype='advisory' AND classid=42720 AND objid=1 AND NOT granted)")
            .fetch_one(&fixture.pool)
            .await
            .unwrap();
        if waiting {
            break;
        }
        tokio::task::yield_now().await;
    }
    let revoke_app = fixture.app().await;
    let revoke_client = client_id.clone();
    let revoke_refresh = refresh.clone();
    let revoke = tokio::spawn(async move {
        request(
            revoke_app,
            Method::POST,
            "/oauth/revoke",
            Body::from(format!("token={revoke_refresh}&client_id={revoke_client}")),
            &[("content-type", "application/x-www-form-urlencoded")],
        )
        .await
    });
    let family_lock: i64 = sqlx::query_scalar("SELECT hashtextextended($1, 0)")
        .bind(family.to_string())
        .fetch_one(&fixture.pool)
        .await
        .unwrap();
    for _ in 0..100 {
        let waiting: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_locks WHERE locktype='advisory' AND classid=(($1::bigint >> 32) & 4294967295)::oid AND objid=($1::bigint & 4294967295)::oid AND objsubid=1 AND NOT granted)")
            .bind(family_lock)
            .fetch_one(&fixture.pool)
            .await
            .unwrap();
        if waiting {
            break;
        }
        tokio::task::yield_now().await;
    }
    sqlx::query("SELECT pg_advisory_unlock(42720, 1)")
        .execute(&mut *blocker)
        .await
        .unwrap();
    drop(blocker);
    assert_eq!(rotation.await.unwrap().status(), StatusCode::OK);
    assert_eq!(revoke.await.unwrap().status(), StatusCode::OK);
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM mcp_oauth_tokens WHERE family_id=$1 AND revoked_at IS NULL",
        )
        .bind(family)
        .fetch_one(&fixture.pool)
        .await
        .unwrap(),
        0
    );
    sqlx::query("DROP TRIGGER mcp_block_refresh_rotation ON mcp_oauth_tokens")
        .execute(&fixture.pool)
        .await
        .unwrap();
    sqlx::query("DROP FUNCTION mcp_block_refresh_rotation()")
        .execute(&fixture.pool)
        .await
        .unwrap();
    fixture.cleanup().await;
}

#[tokio::test]
async fn oauth_revoke_then_refresh_rotation_serializes_without_live_descendants() {
    let fixture = Fixture::new().await;
    let app = fixture.app().await;
    let verifier = "a-valid-pkce-verifier-that-is-long-enough-to-satisfy-rfc7636";
    let client_id = register(app.clone()).await;
    let code = approve(
        app.clone(),
        &authorize(app.clone(), &client_id, verifier).await,
        &fixture.owner_token,
    )
    .await;
    let refresh = exchange(app.clone(), &client_id, &code, verifier).await["refresh_token"]
        .as_str()
        .unwrap()
        .to_owned();
    let family: Uuid = sqlx::query_scalar("SELECT family_id FROM mcp_oauth_tokens WHERE digest=$1")
        .bind(Sha256::digest(refresh.as_bytes()).as_slice())
        .fetch_one(&fixture.pool)
        .await
        .unwrap();
    sqlx::query(&format!(
        "CREATE FUNCTION mcp_block_family_revoke() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.family_id = '{family}'::uuid THEN PERFORM pg_advisory_xact_lock(42721, 1); END IF; RETURN NEW; END $$"
    ))
    .execute(&fixture.pool)
    .await
    .unwrap();
    sqlx::query("CREATE TRIGGER mcp_block_family_revoke BEFORE UPDATE ON mcp_oauth_tokens FOR EACH ROW EXECUTE FUNCTION mcp_block_family_revoke()")
        .execute(&fixture.pool)
        .await
        .unwrap();
    let mut blocker = fixture.pool.acquire().await.unwrap();
    sqlx::query("SELECT pg_advisory_lock(42721, 1)")
        .execute(&mut *blocker)
        .await
        .unwrap();
    let revoke_client = client_id.clone();
    let revoke_refresh = refresh.clone();
    let revoke = tokio::spawn(async move {
        request(
            app,
            Method::POST,
            "/oauth/revoke",
            Body::from(format!("token={revoke_refresh}&client_id={revoke_client}")),
            &[("content-type", "application/x-www-form-urlencoded")],
        )
        .await
    });
    for _ in 0..100 {
        let waiting: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_locks WHERE locktype='advisory' AND classid=42721 AND objid=1 AND NOT granted)")
            .fetch_one(&fixture.pool)
            .await
            .unwrap();
        if waiting {
            break;
        }
        tokio::task::yield_now().await;
    }
    let refresh_client = client_id.clone();
    let refresh_token = refresh.clone();
    let refresh_app = fixture.app().await;
    let rotation =
        tokio::spawn(
            async move { refresh_request(refresh_app, &refresh_client, &refresh_token).await },
        );
    let family_lock: i64 = sqlx::query_scalar("SELECT hashtextextended($1, 0)")
        .bind(family.to_string())
        .fetch_one(&fixture.pool)
        .await
        .unwrap();
    for _ in 0..100 {
        let waiting: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_locks WHERE locktype='advisory' AND classid=(($1::bigint >> 32) & 4294967295)::oid AND objid=($1::bigint & 4294967295)::oid AND objsubid=1 AND NOT granted)")
            .bind(family_lock)
            .fetch_one(&fixture.pool)
            .await
            .unwrap();
        if waiting {
            break;
        }
        tokio::task::yield_now().await;
    }
    sqlx::query("SELECT pg_advisory_unlock(42721, 1)")
        .execute(&mut *blocker)
        .await
        .unwrap();
    drop(blocker);
    assert_eq!(revoke.await.unwrap().status(), StatusCode::OK);
    assert_eq!(rotation.await.unwrap().status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM mcp_oauth_tokens WHERE family_id=$1 AND revoked_at IS NULL",
        )
        .bind(family)
        .fetch_one(&fixture.pool)
        .await
        .unwrap(),
        0
    );
    sqlx::query("DROP TRIGGER mcp_block_family_revoke ON mcp_oauth_tokens")
        .execute(&fixture.pool)
        .await
        .unwrap();
    sqlx::query("DROP FUNCTION mcp_block_family_revoke()")
        .execute(&fixture.pool)
        .await
        .unwrap();
    fixture.cleanup().await;
}

#[tokio::test]
async fn sweep_retains_future_revoked_refreshes_and_used_ancestors_detect_reuse() {
    let fixture = Fixture::new().await;
    let app = fixture.app().await;
    let verifier = "a-valid-pkce-verifier-that-is-long-enough-to-satisfy-rfc7636";
    let client_id = register(app.clone()).await;
    let code = approve(
        app.clone(),
        &authorize(app.clone(), &client_id, verifier).await,
        &fixture.owner_token,
    )
    .await;
    let refresh0 = exchange(app.clone(), &client_id, &code, verifier).await["refresh_token"]
        .as_str()
        .unwrap()
        .to_owned();
    let refresh1 =
        json(refresh_request(app.clone(), &client_id, &refresh0).await).await["refresh_token"]
            .as_str()
            .unwrap()
            .to_owned();
    let (family, grant_id, device_id): (Uuid, Uuid, Uuid) =
        sqlx::query_as("SELECT family_id,grant_id,device_id FROM mcp_oauth_tokens WHERE digest=$1")
            .bind(Sha256::digest(refresh0.as_bytes()).as_slice())
            .fetch_one(&fixture.pool)
            .await
            .unwrap();
    sqlx::query("UPDATE mcp_oauth_tokens SET expires_at=clock_timestamp()-interval '6 days' WHERE digest=$1")
        .bind(Sha256::digest(refresh0.as_bytes()).as_slice())
        .execute(&fixture.pool)
        .await
        .unwrap();
    let retained_digest = Sha256::digest(b"future-expiry-revoked-refresh");
    sqlx::query("INSERT INTO mcp_oauth_tokens(digest,token_kind,family_id,vault_id,device_id,grant_id,client_id,resource,expires_at,revoked_at) VALUES($1,'refresh',$2,$3,$4,$5,$6,'http://localhost:3000/mcp',clock_timestamp()+interval '1 day',clock_timestamp())")
        .bind(retained_digest.as_slice())
        .bind(family)
        .bind(fixture.vault)
        .bind(device_id)
        .bind(grant_id)
        .bind(&client_id)
        .execute(&fixture.pool)
        .await
        .unwrap();
    sqlx::query("SELECT mcp_sweep_oauth()")
        .execute(&fixture.pool)
        .await
        .unwrap();
    assert!(
        sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM mcp_oauth_tokens WHERE digest=$1)"
        )
        .bind(retained_digest.as_slice())
        .fetch_one(&fixture.pool)
        .await
        .unwrap()
    );
    let replay = refresh_request(app, &client_id, &refresh0).await;
    assert_eq!(replay.status(), StatusCode::BAD_REQUEST);
    assert_eq!(json(replay).await, json!({"error":"invalid_grant"}));
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM mcp_oauth_tokens WHERE family_id=$1 AND revoked_at IS NULL",
        )
        .bind(family)
        .fetch_one(&fixture.pool)
        .await
        .unwrap(),
        0
    );
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT token_kind FROM mcp_oauth_tokens WHERE digest=$1")
            .bind(Sha256::digest(refresh1.as_bytes()).as_slice())
            .fetch_one(&fixture.pool)
            .await
            .unwrap(),
        "refresh"
    );
    fixture.cleanup().await;
}

#[tokio::test]
async fn management_grant_and_device_revocations_prevent_code_and_refresh_issuance() {
    for revoke_device in [false, true] {
        let fixture = Fixture::new().await;
        let app = fixture.app().await;
        let verifier = "a-valid-pkce-verifier-that-is-long-enough-to-satisfy-rfc7636";
        let client_id = register(app.clone()).await;
        let code_for_exchange = approve(
            app.clone(),
            &authorize(app.clone(), &client_id, verifier).await,
            &fixture.owner_token,
        )
        .await;
        let refresh =
            exchange(app.clone(), &client_id, &code_for_exchange, verifier).await["refresh_token"]
                .as_str()
                .unwrap()
                .to_owned();
        let code = approve(
            app.clone(),
            &authorize(app.clone(), &client_id, verifier).await,
            &fixture.owner_token,
        )
        .await;
        let grant_id: Uuid =
            sqlx::query_scalar("SELECT grant_id FROM mcp_oauth_codes WHERE digest=$1")
                .bind(Sha256::digest(code.as_bytes()).as_slice())
                .fetch_one(&fixture.pool)
                .await
                .unwrap();
        let refresh_grant_id: Uuid =
            sqlx::query_scalar("SELECT grant_id FROM mcp_oauth_tokens WHERE digest=$1")
                .bind(Sha256::digest(refresh.as_bytes()).as_slice())
                .fetch_one(&fixture.pool)
                .await
                .unwrap();
        let revoke_paths = if revoke_device {
            vec![format!("/v1/devices/{}/revoke", fixture.mcp_device)]
        } else {
            vec![
                format!("/v1/mcp/grants/{grant_id}/revoke"),
                format!("/v1/mcp/grants/{refresh_grant_id}/revoke"),
            ]
        };
        for revoke_path in revoke_paths {
            let revoke = request(
                app.clone(),
                Method::POST,
                &revoke_path,
                Body::empty(),
                &[("authorization", &format!("Bearer {}", fixture.owner_token))],
            )
            .await;
            assert_eq!(revoke.status(), StatusCode::NO_CONTENT);
        }
        let token_count: i64 = sqlx::query_scalar("SELECT count(*) FROM mcp_oauth_tokens")
            .fetch_one(&fixture.pool)
            .await
            .unwrap();
        let code_response = request(
            app.clone(),
            Method::POST,
            "/oauth/token",
            Body::from(format!(
                "grant_type=authorization_code&client_id={client_id}&code={code}&redirect_uri=https%3A%2F%2Fclient.example%2Fcallback&resource=http%3A%2F%2Flocalhost%3A3000%2Fmcp&code_verifier={verifier}"
            )),
            &[("content-type", "application/x-www-form-urlencoded")],
        )
        .await;
        assert_eq!(code_response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(json(code_response).await, json!({"error":"invalid_grant"}));
        let refresh_response = refresh_request(app, &client_id, &refresh).await;
        assert_eq!(refresh_response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            json(refresh_response).await,
            json!({"error":"invalid_grant"})
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM mcp_oauth_tokens")
                .fetch_one(&fixture.pool)
                .await
                .unwrap(),
            token_count
        );
        fixture.cleanup().await;
    }
}

#[tokio::test]
async fn sweep_reclaims_clients_behind_more_than_one_batch_of_protected_history() {
    let fixture = Fixture::new().await;
    let app = fixture.app().await;
    let verifier = "a-valid-pkce-verifier-that-is-long-enough-to-satisfy-rfc7636";
    let client_id = register(app.clone()).await;
    let code = approve(
        app.clone(),
        &authorize(app.clone(), &client_id, verifier).await,
        &fixture.owner_token,
    )
    .await;
    let refresh = exchange(app.clone(), &client_id, &code, verifier).await["refresh_token"]
        .as_str()
        .unwrap()
        .to_owned();
    let (grant_id, device_id): (Uuid, Uuid) =
        sqlx::query_as("SELECT grant_id,device_id FROM mcp_oauth_tokens WHERE digest=$1")
            .bind(Sha256::digest(refresh.as_bytes()).as_slice())
            .fetch_one(&fixture.pool)
            .await
            .unwrap();
    sqlx::query("INSERT INTO mcp_oauth_clients(client_id,client_name,redirect_uris,allowed_scopes,created_at) SELECT 'protected-client-' || value,'protected',ARRAY['https://client.example/callback'],ARRAY['conversations:read'],clock_timestamp()-interval '26 hours' FROM generate_series(1,512) value")
        .execute(&fixture.pool).await.unwrap();
    sqlx::query("INSERT INTO mcp_oauth_tokens(digest,token_kind,family_id,vault_id,device_id,grant_id,client_id,resource,expires_at) SELECT decode(lpad(to_hex(value + 4096),64,'0'),'hex'),'refresh',$1,$2,$3,$4,'protected-client-' || value,'http://localhost:3000/mcp',clock_timestamp()+interval '1 day' FROM generate_series(1,512) value")
        .bind(Uuid::new_v4()).bind(fixture.vault).bind(device_id).bind(grant_id).execute(&fixture.pool).await.unwrap();
    sqlx::query("INSERT INTO mcp_oauth_clients(client_id,client_name,redirect_uris,allowed_scopes,created_at) SELECT 'reclaimable-client-' || value,'reclaimable',ARRAY['https://client.example/callback'],ARRAY['conversations:read'],clock_timestamp()-interval '25 hours' FROM generate_series(1,512) value")
        .execute(&fixture.pool).await.unwrap();
    sqlx::query("SELECT mcp_sweep_oauth()")
        .execute(&fixture.pool)
        .await
        .unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM mcp_oauth_clients WHERE client_id LIKE 'protected-client-%'"
        )
        .fetch_one(&fixture.pool)
        .await
        .unwrap(),
        512
    );
    assert_eq!(sqlx::query_scalar::<_, i64>("SELECT count(*) FROM mcp_oauth_tokens WHERE client_id LIKE 'protected-client-%' AND expires_at > clock_timestamp()").fetch_one(&fixture.pool).await.unwrap(), 512);
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM mcp_oauth_clients WHERE client_id LIKE 'reclaimable-client-%'"
        )
        .fetch_one(&fixture.pool)
        .await
        .unwrap(),
        0
    );
    assert!(!register(app).await.is_empty());
    fixture.cleanup().await;
}
