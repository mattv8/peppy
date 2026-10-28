use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use http_body_util::BodyExt;
use peppy_client_core::{Client, ClientConfig, DatabaseKey, DeviceId, VaultId};
use peppy_crypto::enrollment::derive_admission_keypair;
use peppy_crypto::{KeyProfile, create_vault_check_header, derive_root_key};
use peppy_hosted_api::{
    computer_enrollment::{
        ComputerEnrollmentRecipient, ComputerEnrollmentTranscript, CreateComputerEnrollmentRequest,
    },
    enrollment::{ClientKind, EnrollmentRole, ProposedDevice},
};
use peppy_hosted_client::newenrollment::EnrollmentClient;
use peppy_server::enrollment::computers::{ComputerEnrollmentRouterState, router};
use sha2::{Digest, Sha256};
use sqlx::{PgPool, postgres::PgPoolOptions};
use std::sync::Arc;
use tower::ServiceExt;
use uuid::Uuid;

const ORIGIN: &str = "https://peppy.test";
const PASSPHRASE: &str = "correct horse battery staple";

async fn fixture() -> (Router, PgPool, String, String, String, Uuid, Uuid) {
    let database_url = std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL is required");
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(&database_url)
        .await
        .unwrap();
    let schema = format!("peppy_computer_test_{}", Uuid::new_v4().simple());
    sqlx::query(&format!("CREATE SCHEMA {schema}"))
        .execute(&admin)
        .await
        .unwrap();
    let mut url: url::Url = database_url.parse().unwrap();
    url.query_pairs_mut()
        .append_pair("options", &format!("-csearch_path={schema}"));
    let pool = PgPoolOptions::new()
        .max_connections(8)
        .connect(url.as_str())
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    let vault = Uuid::new_v4();
    let owner = Uuid::new_v4();
    let owner_token = "a".repeat(96);
    let gateway_token = "b".repeat(96);
    sqlx::query("INSERT INTO vaults(vault_id,public_key_profile,encrypted_vault_check_header,key_epoch,profile_fingerprint) VALUES($1,$2,$3,1,$4)")
        .bind(vault).bind(serde_json::json!({"vault_id":vault,"key_epoch":1})).bind(vec![1u8]).bind("11".repeat(32)).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO devices(vault_id,device_id,role,public_key,profile_fingerprint,key_epoch,name,kind) VALUES($1,$2,'owner','{}',$3,1,'owner','desktop'),($1,$4,'gateway','{}',$3,1,'gateway','android')")
        .bind(vault).bind(owner).bind("11".repeat(32)).bind(Uuid::new_v4()).execute(&pool).await.unwrap();
    for (token, device) in [
        (&owner_token, owner),
        (
            &gateway_token,
            sqlx::query_scalar::<_, Uuid>(
                "SELECT device_id FROM devices WHERE vault_id=$1 AND role='gateway'",
            )
            .bind(vault)
            .fetch_one(&pool)
            .await
            .unwrap(),
        ),
    ] {
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
    sqlx::query("INSERT INTO vault_admission_keys(vault_id,key_epoch,profile_fingerprint,admission_public_key) VALUES($1,1,$2,$3)").bind(vault).bind("11".repeat(32)).bind(vec![9u8;32]).execute(&pool).await.unwrap();
    let app = router(
        ComputerEnrollmentRouterState::new(
            pool.clone(),
            Arc::new(peppy_server::api::CommunityEnrollmentAuthorization::new(
                pool.clone(),
            )),
            false,
            ORIGIN.into(),
        )
        .unwrap(),
    );
    (app, pool, schema, owner_token, gateway_token, vault, owner)
}

fn request_data(id: Uuid, device: Uuid, token: [u8; 32]) -> CreateComputerEnrollmentRequest {
    CreateComputerEnrollmentRequest {
        invitation_id: id,
        recipient: ComputerEnrollmentRecipient {
            device: ProposedDevice {
                device_id: device,
                kind: ClientKind::Desktop,
                name: "Computer".into(),
                signing_public_key: URL_SAFE_NO_PAD.encode([2; 32]),
            },
            credential_digest: "22".repeat(32),
            requested_role: EnrollmentRole::Device,
        },
        recipient_transfer_public_key: URL_SAFE_NO_PAD.encode([3; 32]),
        recipient_commitment: URL_SAFE_NO_PAD.encode([4; 32]),
        claim_token_digest: URL_SAFE_NO_PAD.encode(Sha256::digest(token)),
    }
}
async fn call(app: Router, request: Request<Body>) -> (Router, StatusCode, serde_json::Value) {
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    (
        app,
        status,
        serde_json::from_slice(&body).unwrap_or_default(),
    )
}
fn owner_request(path: String, token: &str, value: serde_json::Value) -> Request<Body> {
    Request::post(path)
        .header("authorization", format!("Bearer {token}"))
        .header("origin", ORIGIN)
        .header("content-type", "application/json")
        .body(Body::from(value.to_string()))
        .unwrap()
}

#[tokio::test]
async fn real_enrollment_clients_complete_computer_admission() {
    let (mut app, pool, schema, owner_token, _, vault, owner_device) = fixture().await;
    let profile = KeyProfile::new(vault, 1).unwrap();
    let fingerprint = profile.fingerprint().unwrap();
    let root = derive_root_key(PASSPHRASE, &profile).unwrap();
    let header = create_vault_check_header(&root, profile.clone()).unwrap();
    let admission = derive_admission_keypair(&root).unwrap();
    sqlx::query("UPDATE vaults SET public_key_profile=$2,encrypted_vault_check_header=$3,profile_fingerprint=$4 WHERE vault_id=$1")
        .bind(vault).bind(serde_json::to_value(&profile).unwrap()).bind(serde_json::to_vec(&header).unwrap()).bind(&fingerprint).execute(&pool).await.unwrap();
    sqlx::query("UPDATE devices SET profile_fingerprint=$3 WHERE vault_id=$1 AND device_id=$2")
        .bind(vault)
        .bind(owner_device)
        .bind(&fingerprint)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE vault_admission_keys SET profile_fingerprint=$2,admission_public_key=$3 WHERE vault_id=$1")
        .bind(vault).bind(&fingerprint).bind(admission.public_key().as_slice()).execute(&pool).await.unwrap();
    let directory = tempfile::TempDir::new().unwrap();
    let core = Client::open(
        ClientConfig {
            database_path: directory.path().join("owner.db"),
            vault_id: VaultId(vault),
            device_id: DeviceId(owner_device),
        },
        DatabaseKey::new(&[7; 32]).unwrap(),
    )
    .unwrap();
    core.unlock(&profile, &header, PASSPHRASE).unwrap();
    let mut claimant =
        EnrollmentClient::new(ORIGIN.into(), ClientKind::Desktop, "computer".into()).unwrap();
    let qr = claimant.begin_computer_claimant().unwrap();
    let mut owner = EnrollmentClient::for_existing_owner(
        ORIGIN.into(),
        ClientKind::Desktop,
        "owner".into(),
        profile,
        header,
    )
    .unwrap();
    owner.configure_computer_owner_device(owner_device).unwrap();
    let create = owner.scan_computer_qr(qr.clone()).unwrap();
    let (next, status, created) = call(
        app,
        owner_request(
            "/v1/enrollment/computers".into(),
            &owner_token,
            serde_json::from_str(&create).unwrap(),
        ),
    )
    .await;
    app = next;
    assert_eq!(status, StatusCode::OK);
    owner.accept_computer_created(created.to_string()).unwrap();
    let approval = owner
        .approve_computer_enrollment_with_client(true, &core)
        .unwrap();
    assert_eq!(
        approval,
        owner
            .approve_computer_enrollment_with_client(true, &core)
            .unwrap()
    );
    let qr_value: serde_json::Value = serde_json::from_str(&qr).unwrap();
    let id = qr_value["invitation_id"].as_str().unwrap();
    let (next, status, _) = call(
        app,
        owner_request(
            format!("/v1/enrollment/computers/{id}/approve"),
            &owner_token,
            serde_json::from_str(&approval).unwrap(),
        ),
    )
    .await;
    app = next;
    assert_eq!(status, StatusCode::OK);
    let (next, status, _) = call(
        app,
        Request::delete(format!("/v1/enrollment/computers/{id}"))
            .header("authorization", format!("Bearer {owner_token}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    app = next;
    assert_eq!(status, StatusCode::CONFLICT);
    let descriptor: serde_json::Value =
        serde_json::from_str(&claimant.computer_status_request().unwrap()).unwrap();
    let (next, status, approved) = call(
        app,
        Request::post(descriptor["path"].as_str().unwrap())
            .header("content-type", "application/json")
            .body(Body::from(descriptor["body"].to_string()))
            .unwrap(),
    )
    .await;
    app = next;
    assert_eq!(status, StatusCode::OK);
    let confirm = claimant
        .confirm_computer_enrollment(approved.to_string())
        .unwrap();
    let (_next, status, complete) = call(
        app,
        Request::post(format!("/v1/enrollment/computers/{id}/confirm"))
            .header("content-type", "application/json")
            .body(Body::from(confirm))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let identity = claimant
        .finish_computer_admission(complete.to_string())
        .unwrap();
    assert!(identity.into_activation().is_some());
    sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
        .execute(&pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn computer_qr_flow_is_bearer_bound_immutable_and_idempotent() {
    let (mut app, pool, schema, owner_token, gateway_token, vault, owner) = fixture().await;
    let id = Uuid::new_v4();
    let device = Uuid::new_v4();
    let claim = [7; 32];
    let create = request_data(id, device, claim);
    let (next, status, _) = call(
        app,
        Request::post(format!("/v1/enrollment/computers/{id}/status"))
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::json!({"claim_token":URL_SAFE_NO_PAD.encode([1;32])}).to_string(),
            ))
            .unwrap(),
    )
    .await;
    app = next;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (next, status, created) = call(
        app,
        owner_request(
            "/v1/enrollment/computers".into(),
            &owner_token,
            serde_json::to_value(&create).unwrap(),
        ),
    )
    .await;
    app = next;
    assert_eq!(status, StatusCode::OK);
    let (next, status, retry) = call(
        app,
        owner_request(
            "/v1/enrollment/computers".into(),
            &owner_token,
            serde_json::to_value(&create).unwrap(),
        ),
    )
    .await;
    app = next;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(retry["server_nonce"], created["server_nonce"]);
    let claim_body = serde_json::json!({"claim_token":URL_SAFE_NO_PAD.encode(claim)});
    let (next, status, _) = call(
        app,
        Request::post(format!("/v1/enrollment/computers/{id}/status"))
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::json!({"claim_token":URL_SAFE_NO_PAD.encode([8;32])}).to_string(),
            ))
            .unwrap(),
    )
    .await;
    app = next;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (next, status, waiting) = call(
        app,
        Request::post(format!("/v1/enrollment/computers/{id}/status"))
            .header("content-type", "application/json")
            .body(Body::from(claim_body.to_string()))
            .unwrap(),
    )
    .await;
    app = next;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(waiting["state"], "waiting");
    let transcript = ComputerEnrollmentTranscript {
        origin: ORIGIN.into(),
        vault_id: vault,
        fingerprint: "11".repeat(32),
        key_epoch: 1,
        invitation_id: id,
        server_nonce: created["server_nonce"].as_str().unwrap().into(),
        owner_transfer_public_key: URL_SAFE_NO_PAD.encode([5; 32]),
        owner_device_id: owner,
        recipient: create.recipient.clone(),
        recipient_transfer_public_key: create.recipient_transfer_public_key.clone(),
        granted_role: EnrollmentRole::Device,
    };
    let approve = serde_json::json!({"transcript":transcript,"encrypted_key_package":URL_SAFE_NO_PAD.encode([6;32]),"transfer_nonce":URL_SAFE_NO_PAD.encode([7;24]),"owner_proof":URL_SAFE_NO_PAD.encode([8;32])});
    let mut mismatched = approve.clone();
    mismatched["transcript"]["recipient"]["device"]["name"] = serde_json::json!("Other");
    let (next, status, _) = call(
        app,
        owner_request(
            format!("/v1/enrollment/computers/{id}/approve"),
            &owner_token,
            mismatched,
        ),
    )
    .await;
    app = next;
    assert_eq!(status, StatusCode::CONFLICT);
    let (next, status, _) = call(
        app,
        owner_request(
            format!("/v1/enrollment/computers/{id}/approve"),
            &gateway_token,
            approve.clone(),
        ),
    )
    .await;
    app = next;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (next, status, approved) = call(
        app,
        owner_request(
            format!("/v1/enrollment/computers/{id}/approve"),
            &owner_token,
            approve.clone(),
        ),
    )
    .await;
    app = next;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(approved["state"], "approved");
    let (next, status, reapproved) = call(
        app,
        owner_request(
            format!("/v1/enrollment/computers/{id}/approve"),
            &owner_token,
            approve,
        ),
    )
    .await;
    app = next;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(reapproved, approved);
    let token_only = call(
        app,
        Request::post(format!("/v1/enrollment/computers/{id}/confirm"))
            .header("content-type", "application/json")
            .body(Body::from(claim_body.to_string()))
            .unwrap(),
    )
    .await;
    assert_eq!(token_only.1, StatusCode::UNPROCESSABLE_ENTITY);
    let invalid_proofs = serde_json::json!({"claim_token":URL_SAFE_NO_PAD.encode(claim),"device_signature":URL_SAFE_NO_PAD.encode([1;64]),"admission_signature":URL_SAFE_NO_PAD.encode([2;64])});
    let (_, status, _) = call(
        token_only.0,
        Request::post(format!("/v1/enrollment/computers/{id}/confirm"))
            .header("content-type", "application/json")
            .body(Body::from(invalid_proofs.to_string()))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM devices WHERE vault_id=$1 AND device_id=$2 AND role='device'"
        )
        .bind(vault)
        .bind(device)
        .fetch_one(&pool)
        .await
        .unwrap(),
        0
    );
    sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
        .execute(&pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn computer_enrollment_rejects_mutation_revocation_expiry_and_cancellation() {
    let (mut app, pool, schema, owner_token, _, vault, _) = fixture().await;
    let id = Uuid::new_v4();
    let create = request_data(id, Uuid::new_v4(), [7; 32]);
    let (next, status, created) = call(
        app,
        owner_request(
            "/v1/enrollment/computers".into(),
            &owner_token,
            serde_json::to_value(&create).unwrap(),
        ),
    )
    .await;
    app = next;
    assert_eq!(status, StatusCode::OK);
    let mut changed = create.clone();
    changed.recipient.device.name = "Other".into();
    let (next, status, _) = call(
        app,
        owner_request(
            "/v1/enrollment/computers".into(),
            &owner_token,
            serde_json::to_value(changed).unwrap(),
        ),
    )
    .await;
    app = next;
    assert_eq!(status, StatusCode::CONFLICT);
    let mut wrong_role = request_data(Uuid::new_v4(), Uuid::new_v4(), [6; 32]);
    wrong_role.recipient.requested_role = EnrollmentRole::Owner;
    let (next, status, _) = call(
        app,
        owner_request(
            "/v1/enrollment/computers".into(),
            &owner_token,
            serde_json::to_value(wrong_role).unwrap(),
        ),
    )
    .await;
    app = next;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let claim = serde_json::json!({"claim_token":URL_SAFE_NO_PAD.encode([7;32])});
    let (next, status, _) = call(
        app,
        Request::delete(format!("/v1/enrollment/computers/{id}"))
            .header("authorization", format!("Bearer {owner_token}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    app = next;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (next, status, _) = call(
        app,
        Request::post(format!("/v1/enrollment/computers/{id}/confirm"))
            .header("content-type", "application/json")
            .body(Body::from(claim.to_string()))
            .unwrap(),
    )
    .await;
    app = next;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let expired = Uuid::new_v4();
    let create = request_data(expired, Uuid::new_v4(), [9; 32]);
    let (next, status, _) = call(
        app,
        owner_request(
            "/v1/enrollment/computers".into(),
            &owner_token,
            serde_json::to_value(create).unwrap(),
        ),
    )
    .await;
    app = next;
    assert_eq!(status, StatusCode::OK);
    sqlx::query("UPDATE computer_enrollments SET expires_at=now()-interval '1 second' WHERE id=$1")
        .bind(expired)
        .execute(&pool)
        .await
        .unwrap();
    let (next, status, body) = call(
        app,
        Request::post(format!("/v1/enrollment/computers/{expired}/status"))
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::json!({"claim_token":URL_SAFE_NO_PAD.encode([9;32])}).to_string(),
            ))
            .unwrap(),
    )
    .await;
    app = next;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["state"], "expired");
    sqlx::query("UPDATE devices SET revoked_at=now() WHERE vault_id=$1 AND role='owner'")
        .bind(vault)
        .execute(&pool)
        .await
        .unwrap();
    let (_, status, _) = call(
        app,
        owner_request(
            "/v1/enrollment/computers".into(),
            &owner_token,
            serde_json::to_value(request_data(Uuid::new_v4(), Uuid::new_v4(), [5; 32])).unwrap(),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(created["expires_in_seconds"], 300);
    sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
        .execute(&pool)
        .await
        .unwrap();
}
