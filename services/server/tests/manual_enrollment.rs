use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use http_body_util::BodyExt;
use libsodium_rs::crypto_sign;
use peppy_crypto::{
    KeyProfile, derive_root_key,
    enrollment::{
        EnrollmentContext, EnrollmentPurpose, EnrollmentRole, derive_admission_keypair,
        sign_device_proof,
    },
};
use peppy_hosted_api::enrollment::{
    AdmissionChallenge, AdmissionChallengeRequest, ClientKind, CompleteAdmissionRequest,
    EnrollmentRole as ApiRole, ProposedDevice,
};
use peppy_server::{
    api::{
        CommunityEnrollmentAuthorization, EnrollmentAuthContext, EnrollmentAuthorization,
        EnrollmentAuthorizationFuture, EnrollmentAuthorizationRequest,
    },
    enrollment::manual::{EnrollmentRouterState, router},
};
use sqlx::{PgPool, postgres::PgPoolOptions};
use std::sync::Arc;
use tower::ServiceExt;
use uuid::Uuid;

struct HostedAuthorization(Uuid);

impl EnrollmentAuthorization for HostedAuthorization {
    fn authorize_enrollment(
        &self,
        _request: EnrollmentAuthorizationRequest,
    ) -> EnrollmentAuthorizationFuture<'_, Result<EnrollmentAuthContext, StatusCode>> {
        Box::pin(async move {
            Ok(EnrollmentAuthContext {
                vault_id: self.0,
                device_id: None,
                role: "owner".into(),
            })
        })
    }
}

async fn app() -> (
    Router,
    PgPool,
    String,
    Uuid,
    peppy_crypto::enrollment::AdmissionKeypair,
) {
    let database_url = std::env::var("TEST_DATABASE_URL")
        .unwrap_or_else(|_| "postgres://postgres:postgres@127.0.0.1:55474/postgres".into());
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(&database_url)
        .await
        .unwrap();
    let schema = format!("peppy_manual_test_{}", Uuid::new_v4().simple());
    sqlx::query(&format!("CREATE SCHEMA {schema}"))
        .execute(&admin)
        .await
        .unwrap();
    let mut url: url::Url = database_url.parse().unwrap();
    url.query_pairs_mut()
        .append_pair("options", &format!("-csearch_path={schema}"));
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(url.as_str())
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    let vault = Uuid::new_v4();
    let fingerprint = "11".repeat(32);
    sqlx::query("INSERT INTO vaults(vault_id,public_key_profile,encrypted_vault_check_header,key_epoch,profile_fingerprint) VALUES($1,$2,$3,1,$4)")
        .bind(vault).bind(serde_json::json!({"vault_id": vault, "key_epoch": 1}))
        .bind(vec![9u8, 8, 7]).bind(&fingerprint).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO vault_owners(vault_id,username,operation_id,request_digest,password_hash,is_admin) VALUES($1,'admin',$2,$3,'test',TRUE)")
        .bind(vault).bind(Uuid::new_v4()).bind(vec![1u8; 32]).execute(&pool).await.unwrap();
    let profile = KeyProfile::new(vault, 1).unwrap();
    let root = derive_root_key("correct horse battery staple", &profile).unwrap();
    let admission = derive_admission_keypair(&root).unwrap();
    sqlx::query("INSERT INTO vault_admission_keys(vault_id,key_epoch,profile_fingerprint,admission_public_key) VALUES($1,1,$2,$3)")
        .bind(vault).bind(fingerprint).bind(admission.public_key().as_slice()).execute(&pool).await.unwrap();
    (
        router(
            EnrollmentRouterState::new(
                pool.clone(),
                Arc::new(CommunityEnrollmentAuthorization::new(pool.clone())),
                false,
                "https://peppy.test".into(),
            )
            .unwrap(),
        ),
        pool,
        schema,
        vault,
        admission,
    )
}

async fn call(
    app: Router,
    path: &str,
    body: serde_json::Value,
) -> (Router, StatusCode, serde_json::Value) {
    call_with_origin(app, path, body, "https://peppy.test").await
}

async fn call_with_origin(
    app: Router,
    path: &str,
    body: serde_json::Value,
    origin: &str,
) -> (Router, StatusCode, serde_json::Value) {
    let response = app
        .clone()
        .oneshot(
            Request::post(path)
                .header("origin", origin)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    (app, status, serde_json::from_slice(&body).unwrap())
}

fn challenge_request(
    vault_id: Option<Uuid>,
    vault_owner: Option<&str>,
) -> AdmissionChallengeRequest {
    let device = crypto_sign::KeyPair::generate().unwrap();
    AdmissionChallengeRequest {
        device: ProposedDevice {
            device_id: Uuid::new_v4(),
            kind: ClientKind::Desktop,
            name: "selector test".into(),
            signing_public_key: URL_SAFE_NO_PAD.encode(device.public_key.as_bytes()),
        },
        vault_id,
        vault_owner: vault_owner.map(str::to_owned),
        credential_digest: hex::encode(Uuid::new_v4().as_bytes().repeat(2)),
    }
}

async fn add_vault(pool: &PgPool, username: &str) -> Uuid {
    let vault = Uuid::new_v4();
    let fingerprint = "22".repeat(32);
    sqlx::query("INSERT INTO vaults(vault_id,public_key_profile,encrypted_vault_check_header,key_epoch,profile_fingerprint) VALUES($1,$2,$3,1,$4)")
        .bind(vault)
        .bind(serde_json::json!({"vault_id": vault, "key_epoch": 1}))
        .bind(vec![9u8, 8, 7])
        .bind(&fingerprint)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO vault_owners(vault_id,username,operation_id,request_digest,password_hash,is_admin) VALUES($1,$2,$3,$4,'test',FALSE)")
        .bind(vault)
        .bind(username)
        .bind(Uuid::new_v4())
        .bind(vec![2u8; 32])
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO vault_admission_keys(vault_id,key_epoch,profile_fingerprint,admission_public_key) SELECT $1,key_epoch,$2,admission_public_key FROM vault_admission_keys LIMIT 1")
        .bind(vault)
        .bind(fingerprint)
        .execute(pool)
        .await
        .unwrap();
    vault
}

#[tokio::test]
async fn manual_challenge_rejects_wrong_psk_and_replay_and_expiry() {
    let (mut app, pool, schema, vault, admission) = app().await;
    let device = crypto_sign::KeyPair::generate().unwrap();
    let device_id = Uuid::new_v4();
    let digest = "22".repeat(32);
    let request = AdmissionChallengeRequest {
        device: ProposedDevice {
            device_id,
            kind: ClientKind::Android,
            name: "phone".into(),
            signing_public_key: URL_SAFE_NO_PAD.encode(device.public_key.as_bytes()),
        },
        vault_id: Some(vault),
        vault_owner: None,
        credential_digest: digest.clone(),
    };
    let hosted_app = router(
        EnrollmentRouterState::new(
            pool.clone(),
            Arc::new(HostedAuthorization(vault)),
            true,
            "https://peppy.pro".into(),
        )
        .unwrap(),
    );
    let mut hosted_request = request.clone();
    hosted_request.device.device_id = Uuid::new_v4();
    hosted_request.credential_digest = "44".repeat(32);
    let (_, status, hosted_challenge) = call_with_origin(
        hosted_app,
        "/v1/enrollment/challenges",
        serde_json::to_value(&hosted_request).unwrap(),
        "https://app.peppy.pro",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(hosted_challenge["origin"], "https://peppy.pro");
    let hosted_without_policy = router(
        EnrollmentRouterState::new(
            pool.clone(),
            Arc::new(CommunityEnrollmentAuthorization::new(pool.clone())),
            true,
            "https://peppy.pro".into(),
        )
        .unwrap(),
    );
    let (_, status, _) = call_with_origin(
        hosted_without_policy,
        "/v1/enrollment/challenges",
        serde_json::to_value(&hosted_request).unwrap(),
        "https://relay.example",
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (next, status, _) = call_with_origin(
        app,
        "/v1/enrollment/challenges",
        serde_json::to_value(&request).unwrap(),
        "https://relay.example",
    )
    .await;
    app = next;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (next, status, challenge_json) = call(
        app,
        "/v1/enrollment/challenges",
        serde_json::to_value(request).unwrap(),
    )
    .await;
    app = next;
    assert_eq!(status, StatusCode::OK);
    let challenge: AdmissionChallenge = serde_json::from_value(challenge_json).unwrap();
    assert_eq!(challenge.requested_role, ApiRole::Gateway);
    let context = EnrollmentContext {
        purpose: EnrollmentPurpose::Manual,
        canonical_origin: challenge.origin,
        vault_id: vault,
        fingerprint: hex::decode(challenge.fingerprint)
            .unwrap()
            .try_into()
            .unwrap(),
        key_epoch: challenge.key_epoch,
        enrollment_id: challenge.enrollment_id,
        server_nonce: URL_SAFE_NO_PAD
            .decode(challenge.nonce)
            .unwrap()
            .try_into()
            .unwrap(),
        owner_transfer_public_key: [0; 32],
        recipient_transfer_public_key: [0; 32],
        recipient_signing_public_key: *device.public_key.as_bytes(),
        device_id,
        credential_digest: hex::decode(&digest).unwrap().try_into().unwrap(),
        role: EnrollmentRole::Gateway,
    };
    let device_signature = sign_device_proof(device.secret_key.as_bytes(), &context).unwrap();
    let proof = CompleteAdmissionRequest {
        admission_signature: URL_SAFE_NO_PAD.encode([0; 64]),
        device_signature: URL_SAFE_NO_PAD.encode(device_signature),
        credential_digest: digest.clone(),
    };
    let (next, status, _) = call(
        app,
        &format!(
            "/v1/enrollment/challenges/{}/complete",
            challenge.enrollment_id
        ),
        serde_json::to_value(&proof).unwrap(),
    )
    .await;
    app = next;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let proof = CompleteAdmissionRequest {
        admission_signature: URL_SAFE_NO_PAD.encode(admission.sign_context(&context).unwrap()),
        device_signature: URL_SAFE_NO_PAD.encode(device_signature),
        credential_digest: digest,
    };
    let (next, status, completed) = call(
        app,
        &format!(
            "/v1/enrollment/challenges/{}/complete",
            challenge.enrollment_id
        ),
        serde_json::to_value(&proof).unwrap(),
    )
    .await;
    app = next;
    assert_eq!(status, StatusCode::OK);
    assert!(
        completed
            .get("material")
            .unwrap()
            .get("check_header")
            .is_some()
    );
    let (next, status, _) = call(
        app,
        &format!(
            "/v1/enrollment/challenges/{}/complete",
            challenge.enrollment_id
        ),
        serde_json::to_value(&proof).unwrap(),
    )
    .await;
    app = next;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM devices WHERE vault_id=$1")
            .bind(vault)
            .fetch_one(&pool)
            .await
            .unwrap(),
        1
    );
    sqlx::query("UPDATE device_admissions SET expires_at=now()-interval '1 second' WHERE id=$1")
        .bind(challenge.enrollment_id)
        .execute(&pool)
        .await
        .unwrap();
    let (next, status, _) = call(
        app,
        &format!(
            "/v1/enrollment/challenges/{}/complete",
            challenge.enrollment_id
        ),
        serde_json::to_value(proof).unwrap(),
    )
    .await;
    app = next;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let renewal = AdmissionChallengeRequest {
        device: ProposedDevice {
            device_id: Uuid::new_v4(),
            kind: ClientKind::Desktop,
            name: "renewed desktop".into(),
            signing_public_key: URL_SAFE_NO_PAD.encode(device.public_key.as_bytes()),
        },
        vault_id: Some(vault),
        vault_owner: None,
        credential_digest: "33".repeat(32),
    };
    let (next, status, renewal_json) = call(
        app,
        "/v1/enrollment/challenges",
        serde_json::to_value(&renewal).unwrap(),
    )
    .await;
    app = next;
    assert_eq!(status, StatusCode::OK);
    let renewal_challenge: AdmissionChallenge = serde_json::from_value(renewal_json).unwrap();
    let wrong_renewal_proof = CompleteAdmissionRequest {
        admission_signature: URL_SAFE_NO_PAD.encode([0; 64]),
        device_signature: URL_SAFE_NO_PAD.encode([0; 64]),
        credential_digest: renewal.credential_digest.clone(),
    };
    for _ in 0..5 {
        let (next, status, _) = call(
            app,
            &format!(
                "/v1/enrollment/challenges/{}/complete",
                renewal_challenge.enrollment_id
            ),
            serde_json::to_value(&wrong_renewal_proof).unwrap(),
        )
        .await;
        app = next;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }
    let (_, status, _) = call(
        app,
        "/v1/enrollment/challenges",
        serde_json::to_value(renewal).unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
        .execute(&pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn manual_challenge_selects_multi_vaults_without_owner_enumeration() {
    let (app, pool, schema, vault, _) = app().await;

    let (_, status, selected) = call(
        app.clone(),
        "/v1/enrollment/challenges",
        serde_json::to_value(challenge_request(None, None)).unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(selected["vault_id"], vault.to_string());

    let (_, status, selected) = call(
        app.clone(),
        "/v1/enrollment/challenges",
        serde_json::to_value(challenge_request(Some(vault), None)).unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(selected["vault_id"], vault.to_string());

    let second = add_vault(&pool, "second").await;
    let (_, status, selected) = call(
        app.clone(),
        "/v1/enrollment/challenges",
        serde_json::to_value(challenge_request(Some(second), None)).unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(selected["vault_id"], second.to_string());

    let (_, status, selected) = call(
        app.clone(),
        "/v1/enrollment/challenges",
        serde_json::to_value(challenge_request(None, Some("SECOND"))).unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(selected["vault_id"], second.to_string());

    for (request, expected_status, expected_error) in [
        (
            challenge_request(Some(Uuid::new_v4()), None),
            StatusCode::NOT_FOUND,
            "vault_not_found",
        ),
        (
            challenge_request(None, Some("missing")),
            StatusCode::NOT_FOUND,
            "vault_not_found",
        ),
        (
            challenge_request(Some(vault), Some("second")),
            StatusCode::FORBIDDEN,
            "foreign_vault",
        ),
        (
            challenge_request(None, None),
            StatusCode::CONFLICT,
            "vault_owner_required",
        ),
        (
            challenge_request(None, Some("  ")),
            StatusCode::CONFLICT,
            "vault_owner_required",
        ),
        (
            challenge_request(None, Some("bad owner")),
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_username",
        ),
    ] {
        let (_, status, body) = call(
            app.clone(),
            "/v1/enrollment/challenges",
            serde_json::to_value(request).unwrap(),
        )
        .await;
        assert_eq!(status, expected_status);
        assert_eq!(body["error"], expected_error);
    }

    sqlx::query("INSERT INTO instance_sessions(token_digest,vault_id,expires_at) VALUES($1,$2,now()+interval '1 hour')")
        .bind(peppy_server::instance::digest(b"session").as_slice())
        .bind(vault)
        .execute(&pool)
        .await
        .unwrap();
    let response = app
        .oneshot(
            Request::post("/v1/enrollment/challenges")
                .header("origin", "https://peppy.test")
                .header("content-type", "application/json")
                .header("peppy-instance-session", "session")
                .body(Body::from(
                    serde_json::to_string(&challenge_request(Some(second), None)).unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
        .execute(&pool)
        .await
        .unwrap();
}
