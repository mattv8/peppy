use std::{net::SocketAddr, time::Duration};

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use http_body_util::BodyExt;
use peppy_server::{ServerBuilder, config::Config, web_client::WebClientConfig};
use sqlx::postgres::PgPoolOptions;
use tempfile::TempDir;
use tower::ServiceExt;
use url::Url;

fn config(revision: &str) -> Config {
    Config {
        bind_addr: "127.0.0.1:0".parse::<SocketAddr>().unwrap(),
        database_url: "postgres://ignored:ignored@127.0.0.1:1/ignored".into(),
        release_identity: revision.into(),
        s3: None,
        public_api_url: None,
        public_attachment_url: None,
        vault_attachment_quota_bytes: 512 * 1024 * 1024,
        trusted_proxy_cidrs: Vec::new(),
        replay_retention: Duration::from_secs(86_400),
        relay_url: None,
        web_client: None,
    }
}

fn pool() -> sqlx::PgPool {
    PgPoolOptions::new()
        .connect_lazy("postgres://ignored:ignored@127.0.0.1:1/ignored")
        .unwrap()
}

#[tokio::test]
async fn builder_uses_the_supplied_config_without_environment_configuration() {
    let server = ServerBuilder::new(config("builder-test"), pool())
        .build()
        .await
        .unwrap();
    let response = server
        .router()
        .oneshot(Request::get("/__release").body(Body::empty()).unwrap())
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .as_ref(),
        br#"{"revision":"builder-test"}"#,
    );
}

#[tokio::test]
async fn builder_assembles_routes_without_starting_maintenance() {
    let server = ServerBuilder::new(config("routes-only"), pool())
        .build()
        .await
        .unwrap();
    let response = server
        .router()
        .oneshot(Request::get("/healthz").body(Body::empty()).unwrap())
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn builder_leaves_web_client_wrapping_to_the_entrypoint() {
    let assets = TempDir::new().unwrap();
    std::fs::write(assets.path().join("index.html"), "browser client").unwrap();
    let mut server_config = config("api-only");
    server_config.web_client = Some(
        WebClientConfig::new(
            "app.example.test".into(),
            assets.path().into(),
            Url::parse("https://api.example.test").unwrap(),
        )
        .unwrap(),
    );
    let server = ServerBuilder::new(server_config, pool())
        .build()
        .await
        .unwrap();
    let response = server
        .router()
        .oneshot(
            Request::get("/")
                .header("host", "app.example.test")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn maintenance_can_only_be_started_once() {
    let mut server = ServerBuilder::new(config("maintenance"), pool())
        .build()
        .await
        .unwrap();

    assert!(server.start_maintenance().is_ok());
    assert!(server.start_maintenance().is_err());
}

#[tokio::test]
async fn row_security_requires_an_explicit_maintenance_database() {
    let error = ServerBuilder::new(config("hosted-rls"), pool())
        .row_security(true)
        .build()
        .await
        .err()
        .expect("hosted RLS must not silently run maintenance on the runtime pool");

    assert!(error.to_string().contains("maintenance database"));
}
