use std::{net::SocketAddr, sync::Arc, time::Duration};

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use http_body_util::BodyExt;
use peppy_hosted_api::{ApnsEnvironment, PushConfigAdvertisement, PushProvider};
use peppy_server::{
    ServerBuilder,
    api::{
        EnrollmentAuthContext, EnrollmentAuthorization, EnrollmentAuthorizationFuture,
        EnrollmentAuthorizationRequest,
    },
    config::Config,
    web_client::WebClientConfig,
};
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

struct HostedEnrollmentAuthorization;

impl EnrollmentAuthorization for HostedEnrollmentAuthorization {
    fn authorize_enrollment(
        &self,
        _request: EnrollmentAuthorizationRequest,
    ) -> EnrollmentAuthorizationFuture<'_, Result<EnrollmentAuthContext, StatusCode>> {
        Box::pin(async { Err(StatusCode::UNAUTHORIZED) })
    }
}

#[tokio::test]
async fn builder_rejects_a_short_operator_setup_token() {
    let error = ServerBuilder::new(config("short-setup-token"), pool())
        .setup_token("a".repeat(31))
        .build()
        .await
        .err()
        .expect("short operator setup tokens must fail server assembly");

    assert!(error.to_string().contains("setup token"));
}

#[tokio::test]
async fn builder_uses_the_supplied_config_without_environment_configuration() {
    let server = ServerBuilder::new(config("builder-test"), pool())
        .setup_token("builder-test-token-at-least-32-bytes")
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
        .setup_token("builder-test-token-at-least-32-bytes")
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
        .setup_token("builder-test-token-at-least-32-bytes")
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
        .setup_token("builder-test-token-at-least-32-bytes")
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

#[tokio::test]
async fn builder_accepts_a_private_enrollment_authorization_adapter() {
    let server = ServerBuilder::new(config("hosted-enrollment"), pool())
        .setup_token("builder-test-token-at-least-32-bytes")
        .enrollment_authorization(Arc::new(HostedEnrollmentAuthorization))
        .build()
        .await;

    assert!(server.is_ok());
}

#[tokio::test]
async fn builder_rejects_inconsistent_push_configuration() {
    let error = ServerBuilder::new(config("invalid-push-config"), pool())
        .push_config(PushConfigAdvertisement {
            available: true,
            providers: vec![PushProvider::Fcm],
            apns_environment: Some(ApnsEnvironment::Production),
        })
        .build()
        .await
        .err()
        .expect("an APNs environment without APNs must fail closed");

    assert!(error.to_string().contains("push configuration"));
}

async fn challenge_route_status(server: peppy_server::Server) -> StatusCode {
    server
        .router()
        .oneshot(
            Request::post("/v1/enrollment/challenges")
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap()
        .status()
}

#[tokio::test]
async fn builder_rejects_a_non_loopback_http_enrollment_origin_and_mounts_https() {
    let mut insecure = config("insecure-enrollment-origin");
    insecure.public_api_url = Some(Url::parse("http://lan-host.example.test").unwrap());
    let error = ServerBuilder::community(insecure, pool())
        .setup_token("builder-test-token-at-least-32-bytes")
        .build()
        .await
        .err()
        .expect("an enrollment origin the routers reject must fail assembly");
    let error = error
        .downcast_ref::<std::io::Error>()
        .expect("assembly configuration faults are io errors");
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
    assert!(error.to_string().contains("PUBLIC_API_URL"));
    assert!(!error.to_string().contains("lan-host"));

    let without_origin = ServerBuilder::community(config("no-enrollment-origin"), pool())
        .setup_token("builder-test-token-at-least-32-bytes")
        .build()
        .await
        .unwrap();
    assert_eq!(
        challenge_route_status(without_origin).await,
        StatusCode::NOT_FOUND
    );

    let mut secure = config("https-enrollment-origin");
    secure.public_api_url = Some(Url::parse("https://api.example.test").unwrap());
    let mounted = ServerBuilder::community(secure, pool())
        .setup_token("builder-test-token-at-least-32-bytes")
        .build()
        .await
        .unwrap();
    // Only the mounted enrollment router reaches JSON extraction for this incomplete body.
    assert_eq!(
        challenge_route_status(mounted).await,
        StatusCode::UNPROCESSABLE_ENTITY
    );
}
