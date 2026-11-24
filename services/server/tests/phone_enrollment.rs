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
use peppy_hosted_api::enrollment::{CreateInvitationRequest, EnrollmentMethod};
use peppy_server::{
    api::{
        CommunityEnrollmentAuthorization, EnrollmentAuthContext, EnrollmentAuthorization,
        EnrollmentAuthorizationFuture, EnrollmentAuthorizationRequest, EnrollmentOperation,
    },
    enrollment::invitations::{InvitationRouterState, router},
};
use sha2::{Digest, Sha256};
use sqlx::{PgPool, postgres::PgPoolOptions};
use std::sync::Arc;
use tower::ServiceExt;
use uuid::Uuid;

struct HostedAuthorization {
    vault_id: Uuid,
    allowed: bool,
}
impl EnrollmentAuthorization for HostedAuthorization {
    fn authorize_enrollment(
        &self,
        _request: EnrollmentAuthorizationRequest,
    ) -> EnrollmentAuthorizationFuture<'_, Result<EnrollmentAuthContext, StatusCode>> {
        Box::pin(async move {
            self.allowed
                .then_some(EnrollmentAuthContext {
                    vault_id: self.vault_id,
                    device_id: None,
                    role: "account".into(),
                })
                .ok_or(StatusCode::UNAUTHORIZED)
        })
    }
}

#[derive(Default)]
struct MethodCapture {
    calls: std::sync::Mutex<Vec<(EnrollmentOperation, axum::http::Method)>>,
}
impl EnrollmentAuthorization for MethodCapture {
    fn authorize_enrollment(
        &self,
        request: EnrollmentAuthorizationRequest,
    ) -> EnrollmentAuthorizationFuture<'_, Result<EnrollmentAuthContext, StatusCode>> {
        self.calls
            .lock()
            .unwrap()
            .push((request.operation, request.method));
        Box::pin(async { Err(StatusCode::UNAUTHORIZED) })
    }
}

async fn fixture() -> (Router, PgPool, String, String, Uuid) {
    let database_url = std::env::var("TEST_DATABASE_URL")
        .unwrap_or_else(|_| "postgres://postgres:postgres@127.0.0.1:55474/postgres".into());
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(&database_url)
        .await
        .unwrap();
    let schema = format!("peppy_phone_test_{}", Uuid::new_v4().simple());
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
    let owner = Uuid::new_v4();
    let token = "a".repeat(96);
    sqlx::query("INSERT INTO vaults(vault_id,public_key_profile,encrypted_vault_check_header,key_epoch,profile_fingerprint) VALUES($1,$2,$3,1,$4)")
        .bind(vault).bind(serde_json::json!({"vault_id": vault, "key_epoch": 1})).bind(vec![1u8]).bind("11".repeat(32)).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO devices(vault_id,device_id,role,public_key,profile_fingerprint,key_epoch,name,kind) VALUES($1,$2,'owner','{}',$3,1,'owner','desktop')")
        .bind(vault).bind(owner).bind("11".repeat(32)).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO device_credentials(token_digest,vault_id,device_id) VALUES($1,$2,$3)")
        .bind(Sha256::digest(token.as_bytes()).as_slice())
        .bind(vault)
        .bind(owner)
        .execute(&pool)
        .await
        .unwrap();
    let profile = KeyProfile {
        crypto_suite: 1,
        salt: [3; 16],
        vault_id: vault,
        key_epoch: 1,
    };
    let root = derive_root_key("correct horse battery staple", &profile).unwrap();
    let admission = derive_admission_keypair(&root).unwrap();
    sqlx::query("INSERT INTO vault_admission_keys(vault_id,key_epoch,profile_fingerprint,admission_public_key) VALUES($1,1,$2,$3)")
        .bind(vault).bind("11".repeat(32)).bind(admission.public_key().as_slice()).execute(&pool).await.unwrap();
    (
        router(
            InvitationRouterState::new(
                pool.clone(),
                Arc::new(CommunityEnrollmentAuthorization::new(pool.clone())),
                false,
                "https://peppy.test".into(),
            )
            .unwrap(),
        ),
        pool,
        schema,
        token,
        vault,
    )
}

#[tokio::test]
async fn phone_transfer_claim_approval_and_confirm_bind_the_persisted_transcript() {
    let (mut app, pool, schema, owner_token, vault) = fixture().await;
    let invitation_token_digest = URL_SAFE_NO_PAD.encode(Sha256::digest([4; 32]));
    let created_request = CreateInvitationRequest {
        method: EnrollmentMethod::Transfer,
        transfer_public_key: URL_SAFE_NO_PAD.encode([7; 32]),
        invitation_token_digest: invitation_token_digest.clone(),
    };
    let (next, status, created) = request(
        app,
        Request::post("/v1/enrollment/invitations")
            .header("authorization", format!("Bearer {owner_token}"))
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&created_request).unwrap()))
            .unwrap(),
    )
    .await;
    app = next;
    assert_eq!(status, StatusCode::OK);
    let device = crypto_sign::KeyPair::generate().unwrap();
    let device_id = Uuid::new_v4();
    let digest = "22".repeat(32);
    let claim = serde_json::json!({"invitation_token_digest":invitation_token_digest,"claim_authenticator":URL_SAFE_NO_PAD.encode([5;32]),"device":{"device_id":device_id,"kind":"android","name":"phone","signing_public_key":URL_SAFE_NO_PAD.encode(device.public_key.as_bytes())},"transfer_public_key":URL_SAFE_NO_PAD.encode([8;32]),"credential_digest":digest});
    let (next, status, claimed) = request(
        app,
        Request::post("/v1/enrollment/invitations/claim")
            .header("content-type", "application/json")
            .body(Body::from(claim.to_string()))
            .unwrap(),
    )
    .await;
    app = next;
    assert_eq!(status, StatusCode::OK);
    let invitation_id = created["invitation_id"].as_str().unwrap();
    let (next, status, status_body) = request(
        app,
        Request::get(format!("/v1/enrollment/invitations/{invitation_id}"))
            .header("authorization", format!("Bearer {owner_token}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    app = next;
    assert_eq!(status, StatusCode::OK);
    let transcript: peppy_hosted_api::enrollment::EnrollmentTranscript =
        serde_json::from_value(status_body["transcript"].clone()).unwrap();
    let context = EnrollmentContext {
        purpose: EnrollmentPurpose::Phone,
        canonical_origin: transcript.origin,
        vault_id: transcript.vault_id,
        fingerprint: hex::decode(transcript.fingerprint)
            .unwrap()
            .try_into()
            .unwrap(),
        key_epoch: transcript.key_epoch,
        enrollment_id: transcript.enrollment_id,
        server_nonce: URL_SAFE_NO_PAD
            .decode(transcript.nonce)
            .unwrap()
            .try_into()
            .unwrap(),
        owner_transfer_public_key: URL_SAFE_NO_PAD
            .decode(transcript.owner_transfer_public_key)
            .unwrap()
            .try_into()
            .unwrap(),
        recipient_transfer_public_key: URL_SAFE_NO_PAD
            .decode(transcript.recipient_transfer_public_key)
            .unwrap()
            .try_into()
            .unwrap(),
        recipient_signing_public_key: URL_SAFE_NO_PAD
            .decode(transcript.signing_public_key)
            .unwrap()
            .try_into()
            .unwrap(),
        device_id,
        credential_digest: hex::decode(digest).unwrap().try_into().unwrap(),
        role: EnrollmentRole::Gateway,
    };
    let transcript_hash =
        URL_SAFE_NO_PAD.encode(Sha256::digest(context.canonical_bytes().unwrap()));
    let approve = serde_json::json!({"transcript_hash":transcript_hash,"encrypted_key_package":URL_SAFE_NO_PAD.encode([1;32]),"transfer_nonce":URL_SAFE_NO_PAD.encode([2;24])});
    let (next, status, _) = request(
        app,
        Request::post(format!(
            "/v1/enrollment/invitations/{invitation_id}/approve"
        ))
        .header("authorization", format!("Bearer {owner_token}"))
        .header("content-type", "application/json")
        .body(Body::from(approve.to_string()))
        .unwrap(),
    )
    .await;
    app = next;
    assert_eq!(status, StatusCode::OK);
    let profile = KeyProfile {
        crypto_suite: 1,
        salt: [3; 16],
        vault_id: vault,
        key_epoch: 1,
    };
    let root = derive_root_key("correct horse battery staple", &profile).unwrap();
    let admission = derive_admission_keypair(&root).unwrap();
    let confirm = serde_json::json!({"claim_secret":claimed["claim_secret"],"transcript_hash":transcript_hash,"device_signature":URL_SAFE_NO_PAD.encode(sign_device_proof(device.secret_key.as_bytes(), &context).unwrap()),"admission_signature":URL_SAFE_NO_PAD.encode(admission.sign_context(&context).unwrap())});
    let (_, status, complete) = request(
        app,
        Request::post(format!(
            "/v1/enrollment/invitations/{invitation_id}/confirm"
        ))
        .header("content-type", "application/json")
        .body(Body::from(confirm.to_string()))
        .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{complete}");
    assert_eq!(complete["device_id"], device_id.to_string());
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM devices WHERE role='gateway'")
            .fetch_one(&pool)
            .await
            .unwrap(),
        1
    );
    sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
        .execute(&pool)
        .await
        .unwrap();
}

async fn request(app: Router, request: Request<Body>) -> (Router, StatusCode, serde_json::Value) {
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    (
        app,
        status,
        serde_json::from_slice(&body).unwrap_or(serde_json::Value::Null),
    )
}

#[tokio::test]
async fn phone_claim_rejects_unauthorized_and_expired_tokens_without_exposing_owner_credentials() {
    let (mut app, pool, schema, owner_token, _) = fixture().await;
    let invitation_token_digest = URL_SAFE_NO_PAD.encode(Sha256::digest([4; 32]));
    let create = CreateInvitationRequest {
        method: EnrollmentMethod::Transfer,
        transfer_public_key: URL_SAFE_NO_PAD.encode([7; 32]),
        invitation_token_digest: invitation_token_digest.clone(),
    };
    let unauthorized = Request::post("/v1/enrollment/invitations")
        .header("origin", "https://peppy.test")
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&create).unwrap()))
        .unwrap();
    let (next, status, _) = request(app, unauthorized).await;
    app = next;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let create = Request::post("/v1/enrollment/invitations")
        .header("origin", "https://peppy.test")
        .header("authorization", format!("Bearer {owner_token}"))
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&create).unwrap()))
        .unwrap();
    let (next, status, created) = request(app, create).await;
    app = next;
    assert_eq!(status, StatusCode::OK);
    assert!(created.get("qr").is_none());
    assert!(!created.to_string().contains(&owner_token));
    let invitation_id = created["invitation_id"]
        .as_str()
        .unwrap()
        .parse::<Uuid>()
        .unwrap();
    sqlx::query("UPDATE phone_invitations SET expires_at=now()-interval '1 second' WHERE id=$1")
        .bind(invitation_id)
        .execute(&pool)
        .await
        .unwrap();
    let claim = serde_json::json!({
        "invitation_token_digest": invitation_token_digest,
        "claim_authenticator": URL_SAFE_NO_PAD.encode([5; 32]),
        "device": {"device_id": Uuid::new_v4(), "kind":"android", "name":"phone", "signing_public_key":URL_SAFE_NO_PAD.encode([8;32])},
        "transfer_public_key": URL_SAFE_NO_PAD.encode([9;32]),
        "credential_digest": "22".repeat(32)
    });
    let claim = Request::post("/v1/enrollment/invitations/claim")
        .header("origin", "https://peppy.test")
        .header("content-type", "application/json")
        .body(Body::from(claim.to_string()))
        .unwrap();
    let (_, status, _) = request(app, claim).await;
    assert_eq!(status, StatusCode::GONE);
    sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
        .execute(&pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn hosted_invitation_operations_authorize_with_their_route_http_method() {
    // Authorization runs before any SQL for these routes, so a lazy pool never connects.
    let pool = PgPoolOptions::new()
        .connect_lazy("postgres://peppy@127.0.0.1:1/unused")
        .unwrap();
    let capture = Arc::new(MethodCapture::default());
    let hosted = router(
        InvitationRouterState::new(pool, capture.clone(), true, "https://api.peppy.test".into())
            .unwrap(),
    );
    let invitation = Uuid::new_v4();
    let requests = [
        Request::get(format!("/v1/enrollment/invitations/{invitation}"))
            .body(Body::empty())
            .unwrap(),
        Request::delete(format!("/v1/enrollment/invitations/{invitation}"))
            .body(Body::empty())
            .unwrap(),
        Request::post(format!("/v1/enrollment/invitations/{invitation}/approve"))
            .header("content-type", "application/json")
            .body(Body::from(r#"{"transcript_hash":"00"}"#))
            .unwrap(),
    ];
    for http_request in requests {
        let (_, status, _) = request(hosted.clone(), http_request).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }
    let calls = capture.calls.lock().unwrap().clone();
    assert_eq!(
        calls,
        vec![
            (EnrollmentOperation::ReadInvitation, axum::http::Method::GET),
            (
                EnrollmentOperation::CancelInvitation,
                axum::http::Method::DELETE
            ),
            (
                EnrollmentOperation::ApproveInvitation,
                axum::http::Method::POST
            ),
        ]
    );
}

#[tokio::test]
async fn hosted_app_origin_is_authorized_by_policy_not_compared_to_api_origin() {
    let (_, pool, schema, _, vault) = fixture().await;
    let token_digest: [u8; 32] = Sha256::digest([9; 32]).into();
    let invitation_id = Uuid::new_v4();
    let owner: Uuid =
        sqlx::query_scalar("SELECT device_id FROM devices WHERE vault_id=$1 AND role='owner'")
            .bind(vault)
            .fetch_one(&pool)
            .await
            .unwrap();
    sqlx::query("INSERT INTO phone_invitations(id,vault_id,owner_device_id,origin,method,token_digest,owner_transfer_public_key,profile_fingerprint,key_epoch,state,expires_at) VALUES($1,$2,$3,'https://api.peppy.test','transfer',$4,$5,$6,1,'waiting',now()+interval '300 seconds')")
        .bind(invitation_id).bind(vault).bind(owner).bind(token_digest.as_slice()).bind(vec![7u8; 32]).bind("11".repeat(32)).execute(&pool).await.unwrap();
    let request_body = serde_json::json!({"invitation_token_digest":URL_SAFE_NO_PAD.encode(token_digest),"claim_authenticator":URL_SAFE_NO_PAD.encode([6;32]),"device":{"device_id":Uuid::new_v4(),"kind":"android","name":"phone","signing_public_key":URL_SAFE_NO_PAD.encode([8;32])},"transfer_public_key":URL_SAFE_NO_PAD.encode([9;32]),"credential_digest":"22".repeat(32)});
    let app = router(
        InvitationRouterState::new(
            pool.clone(),
            Arc::new(HostedAuthorization {
                vault_id: vault,
                allowed: true,
            }),
            true,
            "https://api.peppy.test".into(),
        )
        .unwrap(),
    );
    let (_, status, _) = request(
        app,
        Request::post("/v1/enrollment/invitations/claim")
            .header("origin", "https://app.peppy.test")
            .header("content-type", "application/json")
            .body(Body::from(request_body.to_string()))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let denied = router(
        InvitationRouterState::new(
            pool.clone(),
            Arc::new(HostedAuthorization {
                vault_id: vault,
                allowed: false,
            }),
            true,
            "https://api.peppy.test".into(),
        )
        .unwrap(),
    );
    let (_, status, _) = request(
        denied,
        Request::post("/v1/enrollment/invitations/claim")
            .header("origin", "https://app.peppy.test")
            .header("content-type", "application/json")
            .body(Body::from(request_body.to_string()))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
        .execute(&pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn phone_claim_failures_carry_matching_error_and_code_fields() {
    let (mut app, pool, schema, owner_token, _) = fixture().await;
    let invitation_token_digest = URL_SAFE_NO_PAD.encode(Sha256::digest([6; 32]));
    let create = CreateInvitationRequest {
        method: EnrollmentMethod::Transfer,
        transfer_public_key: URL_SAFE_NO_PAD.encode([7; 32]),
        invitation_token_digest: invitation_token_digest.clone(),
    };
    let (next, status, _) = request(
        app,
        Request::post("/v1/enrollment/invitations")
            .header("authorization", format!("Bearer {owner_token}"))
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&create).unwrap()))
            .unwrap(),
    )
    .await;
    app = next;
    assert_eq!(status, StatusCode::OK);
    let claim = serde_json::json!({
        "invitation_token_digest": invitation_token_digest,
        "claim_authenticator": URL_SAFE_NO_PAD.encode([5; 32]),
        "device": {"device_id": Uuid::new_v4(), "kind":"android", "name":"phone", "signing_public_key":URL_SAFE_NO_PAD.encode([8;32])},
        "transfer_public_key": URL_SAFE_NO_PAD.encode([9;32]),
        "credential_digest": "22".repeat(32)
    });
    let claim_request = |body: &serde_json::Value| {
        Request::post("/v1/enrollment/invitations/claim")
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap()
    };
    let (next, status, claimed) = request(app, claim_request(&claim)).await;
    app = next;
    assert_eq!(status, StatusCode::OK);
    assert!(claimed["claim_secret"].is_string());

    let (next, status, replayed) = request(app, claim_request(&claim)).await;
    app = next;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(replayed["error"], "claim_secret_not_replayable");
    assert_eq!(replayed["code"], "claim_secret_not_replayable");

    let mut competing = claim.clone();
    competing["claim_authenticator"] = serde_json::json!(URL_SAFE_NO_PAD.encode([4; 32]));
    let (_, status, competing) = request(app, claim_request(&competing)).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(competing["error"], "invitation_already_claimed");
    assert_eq!(competing["code"], "invitation_already_claimed");
    sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
        .execute(&pool)
        .await
        .unwrap();
}

mod support;

#[tokio::test]
async fn approve_rechecks_the_owner_credential_after_waiting_for_the_vault_lock() {
    let (mut app, pool, schema, owner_token, vault) = fixture().await;
    let owner_device_id: Uuid =
        sqlx::query_scalar("SELECT device_id FROM device_credentials WHERE token_digest=$1")
            .bind(Sha256::digest(owner_token.as_bytes()).as_slice())
            .fetch_one(&pool)
            .await
            .unwrap();
    let invitation_token_digest = URL_SAFE_NO_PAD.encode(Sha256::digest([4; 32]));
    let created_request = CreateInvitationRequest {
        method: EnrollmentMethod::Transfer,
        transfer_public_key: URL_SAFE_NO_PAD.encode([7; 32]),
        invitation_token_digest: invitation_token_digest.clone(),
    };
    let (next, status, created) = request(
        app,
        Request::post("/v1/enrollment/invitations")
            .header("authorization", format!("Bearer {owner_token}"))
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&created_request).unwrap()))
            .unwrap(),
    )
    .await;
    app = next;
    assert_eq!(status, StatusCode::OK);
    let device = crypto_sign::KeyPair::generate().unwrap();
    let device_id = Uuid::new_v4();
    let digest = "22".repeat(32);
    let claim = serde_json::json!({"invitation_token_digest":invitation_token_digest,"claim_authenticator":URL_SAFE_NO_PAD.encode([5;32]),"device":{"device_id":device_id,"kind":"android","name":"phone","signing_public_key":URL_SAFE_NO_PAD.encode(device.public_key.as_bytes())},"transfer_public_key":URL_SAFE_NO_PAD.encode([8;32]),"credential_digest":digest});
    let (next, status, _claimed) = request(
        app,
        Request::post("/v1/enrollment/invitations/claim")
            .header("content-type", "application/json")
            .body(Body::from(claim.to_string()))
            .unwrap(),
    )
    .await;
    app = next;
    assert_eq!(status, StatusCode::OK);
    let invitation_id = created["invitation_id"].as_str().unwrap();
    let invitation_uuid: Uuid = invitation_id.parse().unwrap();
    let (next, status, status_body) = request(
        app,
        Request::get(format!("/v1/enrollment/invitations/{invitation_id}"))
            .header("authorization", format!("Bearer {owner_token}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    app = next;
    assert_eq!(status, StatusCode::OK);
    let transcript: peppy_hosted_api::enrollment::EnrollmentTranscript =
        serde_json::from_value(status_body["transcript"].clone()).unwrap();
    let context = EnrollmentContext {
        purpose: EnrollmentPurpose::Phone,
        canonical_origin: transcript.origin,
        vault_id: transcript.vault_id,
        fingerprint: hex::decode(transcript.fingerprint)
            .unwrap()
            .try_into()
            .unwrap(),
        key_epoch: transcript.key_epoch,
        enrollment_id: transcript.enrollment_id,
        server_nonce: URL_SAFE_NO_PAD
            .decode(transcript.nonce)
            .unwrap()
            .try_into()
            .unwrap(),
        owner_transfer_public_key: URL_SAFE_NO_PAD
            .decode(transcript.owner_transfer_public_key)
            .unwrap()
            .try_into()
            .unwrap(),
        recipient_transfer_public_key: URL_SAFE_NO_PAD
            .decode(transcript.recipient_transfer_public_key)
            .unwrap()
            .try_into()
            .unwrap(),
        recipient_signing_public_key: URL_SAFE_NO_PAD
            .decode(transcript.signing_public_key)
            .unwrap()
            .try_into()
            .unwrap(),
        device_id,
        credential_digest: hex::decode(digest).unwrap().try_into().unwrap(),
        role: EnrollmentRole::Gateway,
    };
    let transcript_hash =
        URL_SAFE_NO_PAD.encode(Sha256::digest(context.canonical_bytes().unwrap()));
    let approve = serde_json::json!({"transcript_hash":transcript_hash,"encrypted_key_package":URL_SAFE_NO_PAD.encode([1;32]),"transfer_nonce":URL_SAFE_NO_PAD.encode([2;24])});
    let approve_request = Request::post(format!(
        "/v1/enrollment/invitations/{invitation_id}/approve"
    ))
    .header("authorization", format!("Bearer {owner_token}"))
    .header("content-type", "application/json")
    .body(Body::from(approve.to_string()))
    .unwrap();

    // Hold the vault row lock, as hold_vault_lock does in attachments.rs.
    let mut holder = pool.begin().await.unwrap();
    let holder_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *holder)
        .await
        .unwrap();
    sqlx::query("SELECT 1 FROM vaults WHERE vault_id=$1 FOR UPDATE")
        .bind(vault)
        .execute(&mut *holder)
        .await
        .unwrap();
    let task = tokio::spawn(request(app, approve_request));
    support::wait_blocked_by(&pool, holder_pid, None, std::time::Duration::from_secs(5)).await;
    // Revoke only the owner's credential; the owner device row stays live.
    sqlx::query(
        "UPDATE device_credentials SET revoked_at=now() WHERE vault_id=$1 AND device_id=$2",
    )
    .bind(vault)
    .bind(owner_device_id)
    .execute(&mut *holder)
    .await
    .unwrap();
    holder.commit().await.unwrap();
    let (_, status, body) = tokio::time::timeout(std::time::Duration::from_secs(10), task)
        .await
        .expect("blocked approve did not finish after the lock was released")
        .unwrap();
    let state: String = sqlx::query_scalar("SELECT state FROM phone_invitations WHERE id=$1")
        .bind(invitation_uuid)
        .fetch_one(&pool)
        .await
        .unwrap();
    let approved = state == "approved";
    let code = body["code"]
        .as_str()
        .or(body["error"].as_str())
        .unwrap_or("-");
    eprintln!(
        "P-5 POST /v1/enrollment/invitations/{{id}}/approve status={} code={code} approved={approved}",
        status.as_u16()
    );
    sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    assert!(
        !approved,
        "invitation was approved with a revoked owner credential"
    );
}

async fn hold_vault_lock(
    pool: &PgPool,
    vault: Uuid,
) -> (sqlx::Transaction<'static, sqlx::Postgres>, i32) {
    let mut holder = pool.begin().await.unwrap();
    let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *holder)
        .await
        .unwrap();
    sqlx::query("SELECT 1 FROM vaults WHERE vault_id=$1 FOR UPDATE")
        .bind(vault)
        .execute(&mut *holder)
        .await
        .unwrap();
    (holder, pid)
}

async fn revoke_owner_credential_and_release(
    mut holder: sqlx::Transaction<'static, sqlx::Postgres>,
    owner_token: &str,
) {
    sqlx::query("UPDATE device_credentials SET revoked_at=now() WHERE token_digest=$1")
        .bind(Sha256::digest(owner_token.as_bytes()).as_slice())
        .execute(&mut *holder)
        .await
        .unwrap();
    holder.commit().await.unwrap();
}

fn create_invitation_request(owner_token: &str, digest: [u8; 32]) -> Request<Body> {
    let body = CreateInvitationRequest {
        method: EnrollmentMethod::Transfer,
        transfer_public_key: URL_SAFE_NO_PAD.encode([7; 32]),
        invitation_token_digest: URL_SAFE_NO_PAD.encode(digest),
    };
    Request::post("/v1/enrollment/invitations")
        .header("authorization", format!("Bearer {owner_token}"))
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap()
}

fn cancel_invitation_request(invitation_id: &str, owner_token: &str) -> Request<Body> {
    Request::delete(format!("/v1/enrollment/invitations/{invitation_id}"))
        .header("authorization", format!("Bearer {owner_token}"))
        .body(Body::empty())
        .unwrap()
}

async fn invitation_count(pool: &PgPool, vault: Uuid) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM phone_invitations WHERE vault_id=$1")
        .bind(vault)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn invitation_state(pool: &PgPool, invitation_id: &str) -> String {
    let id: Uuid = invitation_id.parse().unwrap();
    sqlx::query_scalar("SELECT state FROM phone_invitations WHERE id=$1")
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn create_rechecks_the_owner_credential_after_waiting_for_the_vault_lock() {
    let (app, pool, schema, owner_token, vault) = fixture().await;
    let (app, status, _) = request(app, create_invitation_request(&owner_token, [4; 32])).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(invitation_count(&pool, vault).await, 1);

    let (holder, holder_pid) = hold_vault_lock(&pool, vault).await;
    let task = tokio::spawn(request(
        app,
        create_invitation_request(&owner_token, [5; 32]),
    ));
    support::wait_blocked_by(&pool, holder_pid, None, std::time::Duration::from_secs(5)).await;
    revoke_owner_credential_and_release(holder, &owner_token).await;
    let (_, status, body) = tokio::time::timeout(std::time::Duration::from_secs(10), task)
        .await
        .expect("blocked create did not finish after the lock was released")
        .unwrap();
    let count = invitation_count(&pool, vault).await;
    sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    assert_eq!(body["code"], "owner_required", "{body}");
    assert_eq!(count, 1, "blocked create wrote an invitation");
}

#[tokio::test]
async fn cancel_rechecks_the_owner_credential_after_waiting_for_the_vault_lock() {
    let (app, pool, schema, owner_token, vault) = fixture().await;
    let (app, status, created) =
        request(app, create_invitation_request(&owner_token, [4; 32])).await;
    assert_eq!(status, StatusCode::OK);
    let control_id = created["invitation_id"].as_str().unwrap().to_owned();
    let (app, status, _) = request(app, cancel_invitation_request(&control_id, &owner_token)).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(invitation_state(&pool, &control_id).await, "denied");

    let (app, status, created) =
        request(app, create_invitation_request(&owner_token, [5; 32])).await;
    assert_eq!(status, StatusCode::OK);
    let invitation_id = created["invitation_id"].as_str().unwrap().to_owned();
    let (holder, holder_pid) = hold_vault_lock(&pool, vault).await;
    let task = tokio::spawn(request(
        app,
        cancel_invitation_request(&invitation_id, &owner_token),
    ));
    support::wait_blocked_by(&pool, holder_pid, None, std::time::Duration::from_secs(5)).await;
    revoke_owner_credential_and_release(holder, &owner_token).await;
    let (_, status, body) = tokio::time::timeout(std::time::Duration::from_secs(10), task)
        .await
        .expect("blocked cancel did not finish after the lock was released")
        .unwrap();
    let state = invitation_state(&pool, &invitation_id).await;
    sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    assert_eq!(body["code"], "owner_required", "{body}");
    assert_eq!(state, "waiting", "blocked cancel changed the invitation");
}
