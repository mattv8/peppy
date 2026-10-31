//! Single-phone invitation enrollment handlers.

use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderMap, Method, StatusCode},
    response::IntoResponse,
    routing::{get, post},
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use peppy_crypto::enrollment::{
    EnrollmentContext, EnrollmentPurpose, EnrollmentRole as CryptoRole, verify_admission_signature,
    verify_device_proof,
};
use peppy_hosted_api::enrollment::{
    AdmissionComplete, ApproveInvitationRequest, ClaimInvitationRequest, ClientKind,
    ConfirmInvitationRequest, CreateInvitationRequest, EnrollmentMethod, EnrollmentRole,
    EnrollmentState, EnrollmentTranscript, InvitationClaim, InvitationCreated,
    InvitationPublicClaim, InvitationStatus, ProposedDevice, VaultMaterial, decode_base64url,
    encode_base64url,
};
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Row};
use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, SystemTime},
};
use uuid::Uuid;

use crate::{
    api::{EnrollmentAuthorization, EnrollmentAuthorizationRequest, EnrollmentOperation},
    enrollment::{
        client_kind_name,
        owner_authorization::{OwnerAuthorizationError, OwnerCredential, resolve_owner_credential},
        validate_device_name,
    },
    scope,
};

const INVITATION_TTL_SECONDS: i64 = 300;
const CLAIM_LIMIT: u8 = 10;
const CLAIM_WINDOW: Duration = Duration::from_secs(60);
const CLAIM_BUCKET_CAP: usize = 4096;
const MAX_HANDOFF_BYTES: usize = 16 * 1024 + 16;

#[derive(Clone)]
pub struct InvitationRouterState {
    pub db: PgPool,
    pub authorization: Arc<dyn EnrollmentAuthorization>,
    /// Hosted deployments require the supplied account policy for token consumers.
    pub hosted: bool,
    canonical_origin: String,
    claims: Arc<tokio::sync::Mutex<HashMap<String, ClaimBucket>>>,
}

#[derive(Clone, Copy)]
struct ClaimBucket {
    window: SystemTime,
    count: u8,
}

impl InvitationRouterState {
    pub fn new(
        db: PgPool,
        authorization: Arc<dyn EnrollmentAuthorization>,
        hosted: bool,
        configured_origin: String,
    ) -> Result<Self, &'static str> {
        Ok(Self {
            db,
            authorization,
            hosted,
            canonical_origin: canonical_configured_origin(&configured_origin)?,
            claims: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
        })
    }
}

pub fn router(state: InvitationRouterState) -> Router {
    Router::new()
        .route("/v1/enrollment/invitations", post(create_invitation))
        .route("/v1/enrollment/invitations/claim", post(claim_invitation))
        .route(
            "/v1/enrollment/invitations/{invitation_id}",
            get(invitation_status).delete(cancel_invitation),
        )
        .route(
            "/v1/enrollment/invitations/{invitation_id}/approve",
            post(approve_invitation),
        )
        .route(
            "/v1/enrollment/invitations/{invitation_id}/confirm",
            post(confirm_invitation),
        )
        .with_state(state)
}

async fn create_invitation(
    State(state): State<InvitationRouterState>,
    headers: HeaderMap,
    Json(request): Json<CreateInvitationRequest>,
) -> Result<Json<InvitationCreated>, InvitationError> {
    let owner = authorize_owner(&state, &headers, EnrollmentOperation::CreateInvitation).await?;
    let origin = request_origin(&state, &headers)?;
    let owner_transfer_public_key = decode_base64url::<32>(&request.transfer_public_key)
        .map_err(|_| bad("invalid_transfer_key"))?;
    let token_digest = decode_base64url::<32>(&request.invitation_token_digest)
        .map_err(|_| bad("invalid_invitation_token_digest"))?;
    let mut tx = scope::begin(&state.db, owner.vault_id)
        .await
        .map_err(database)?;
    revalidate_owner(&mut tx, &owner).await?;
    let vault = sqlx::query(
        "SELECT profile_fingerprint,key_epoch FROM vaults WHERE vault_id=$1 FOR UPDATE",
    )
    .bind(owner.vault_id)
    .fetch_one(&mut *tx)
    .await
    .map_err(database)?;
    withdraw_legacy_enrollment(&mut tx, &headers, &owner).await?;
    let active: i64 = sqlx::query_scalar("SELECT count(*) FROM phone_invitations WHERE vault_id=$1 AND owner_device_id=$2 AND state IN ('waiting','claimed','approved','confirmed') AND expires_at>now()")
        .bind(owner.vault_id).bind(owner.device_id).fetch_one(&mut *tx).await.map_err(database)?;
    if active >= 3 {
        return Err(InvitationError(
            StatusCode::TOO_MANY_REQUESTS,
            "active_invitation_limit",
        ));
    }
    let invitation_id = Uuid::new_v4();
    sqlx::query("INSERT INTO phone_invitations(id,vault_id,owner_device_id,origin,method,token_digest,owner_transfer_public_key,profile_fingerprint,key_epoch,state,expires_at) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,'waiting',now()+interval '300 seconds')")
        .bind(invitation_id).bind(owner.vault_id).bind(owner.device_id).bind(&origin).bind(method_name(request.method)).bind(token_digest.as_slice()).bind(owner_transfer_public_key.as_slice()).bind(vault.get::<String,_>("profile_fingerprint")).bind(vault.get::<i32,_>("key_epoch"))
        .execute(&mut *tx).await.map_err(database)?;
    tx.commit().await.map_err(database)?;
    Ok(Json(InvitationCreated {
        invitation_id,
        expires_in_seconds: INVITATION_TTL_SECONDS as u32,
    }))
}

async fn claim_invitation(
    State(state): State<InvitationRouterState>,
    headers: HeaderMap,
    Json(request): Json<ClaimInvitationRequest>,
) -> Result<Json<InvitationClaim>, InvitationError> {
    let hosted = if state.hosted {
        Some(
            authorize_hosted(
                &state,
                &headers,
                EnrollmentOperation::ConfirmInvitation,
                None,
            )
            .await?,
        )
    } else {
        None
    };
    let origin = request_origin(&state, &headers)?;
    let token_digest = decode_base64url::<32>(&request.invitation_token_digest)
        .map_err(|_| bad("invalid_invitation_token_digest"))?;
    let claim_authenticator = decode_base64url::<32>(&request.claim_authenticator)
        .map_err(|_| bad("invalid_claim_authenticator"))?;
    claim_rate_limit(&state, token_digest).await?;
    let signing_key = validate_device(&request.device)?;
    let transfer_key = decode_base64url::<32>(&request.transfer_public_key)
        .map_err(|_| bad("invalid_transfer_key"))?;
    let credential_digest = credential_digest(&request.credential_digest)?;
    let mut tx = match hosted.as_ref() {
        Some(context) => scope::begin(&state.db, context.vault_id).await,
        None => state.db.begin().await,
    }
    .map_err(database)?;
    let row = sqlx::query("SELECT id,vault_id,origin,method,owner_transfer_public_key,profile_fingerprint,key_epoch,state,claim_secret_digest,claim_authenticator,claimed_device_id,claimed_kind,claimed_name,claimed_public_key,claimed_transfer_public_key,claimed_credential_digest,expires_at > now() AS expires_valid,GREATEST(0,floor(extract(epoch FROM expires_at-now())))::bigint AS expires_remaining FROM phone_invitations WHERE token_digest=$1 FOR UPDATE")
        .bind(token_digest.as_slice()).fetch_optional(&mut *tx).await.map_err(database)?.ok_or(InvitationError(StatusCode::NOT_FOUND, "invitation_not_found"))?;
    if row.get::<String, _>("origin") != origin {
        return Err(InvitationError(StatusCode::FORBIDDEN, "origin_mismatch"));
    }
    if let Some(context) = hosted.as_ref()
        && row.get::<Uuid, _>("vault_id") != context.vault_id
    {
        return Err(InvitationError(StatusCode::FORBIDDEN, "foreign_vault"));
    }
    if !row.get::<bool, _>("expires_valid") {
        expire(&mut tx, row.get("id")).await?;
        tx.commit().await.map_err(database)?;
        return Err(InvitationError(StatusCode::GONE, "invitation_expired"));
    }
    if row.get::<String, _>("state") != "waiting" {
        if exact_claim(
            &row,
            &request,
            signing_key,
            transfer_key,
            credential_digest,
            claim_authenticator,
        ) {
            return Err(InvitationError(
                StatusCode::CONFLICT,
                "claim_secret_not_replayable",
            ));
        }
        return Err(InvitationError(
            StatusCode::CONFLICT,
            "invitation_already_claimed",
        ));
    }
    let claim_secret = random_secret();
    sqlx::query("UPDATE phone_invitations SET state='claimed',claim_secret_digest=$2,claim_authenticator=$3,claimed_device_id=$4,claimed_kind=$5,claimed_name=$6,claimed_public_key=$7,claimed_transfer_public_key=$8,claimed_credential_digest=$9 WHERE id=$1")
        .bind(row.get::<Uuid,_>("id")).bind(digest(&claim_secret).as_slice()).bind(claim_authenticator.as_slice()).bind(request.device.device_id).bind(client_kind_name(request.device.kind)).bind(&request.device.name).bind(signing_key.as_slice()).bind(transfer_key.as_slice()).bind(credential_digest.as_slice())
        .execute(&mut *tx).await.map_err(database)?;
    tx.commit().await.map_err(database)?;
    Ok(Json(InvitationClaim {
        invitation_id: row.get("id"),
        claim_secret,
        origin,
        vault_id: row.get("vault_id"),
        fingerprint: row.get("profile_fingerprint"),
        key_epoch: row.get::<i32, _>("key_epoch") as u32,
        method: parse_method(row.get("method"))?,
        owner_transfer_public_key: encode_base64url(
            &row.get::<Vec<u8>, _>("owner_transfer_public_key"),
        ),
        expires_in_seconds: remaining(&row),
    }))
}

async fn invitation_status(
    State(state): State<InvitationRouterState>,
    headers: HeaderMap,
    Path(invitation_id): Path<Uuid>,
) -> Result<Json<InvitationStatus>, InvitationError> {
    let owner_auth = headers.contains_key("authorization");
    let hosted = if state.hosted {
        Some(authorize_hosted(&state, &headers, EnrollmentOperation::ReadInvitation, None).await?)
    } else {
        None
    };
    let owner = if owner_auth {
        Some(owner(&state, &headers).await?)
    } else {
        None
    };
    let claim_digest = if owner.is_none() {
        Some(claim_secret(&headers)?)
    } else {
        None
    };
    if let (Some(owner), Some(context)) = (owner.as_ref(), hosted.as_ref())
        && owner.vault_id != context.vault_id
    {
        return Err(InvitationError(StatusCode::FORBIDDEN, "foreign_vault"));
    }
    request_origin(&state, &headers)?;
    let mut tx = match (owner.as_ref(), hosted.as_ref()) {
        (Some(owner), _) => scope::begin(&state.db, owner.vault_id).await,
        (None, Some(context)) => scope::begin(&state.db, context.vault_id).await,
        (None, None) => state.db.begin().await,
    }
    .map_err(database)?;
    let row = sqlx::query("SELECT p.*,v.public_key_profile,p.expires_at > now() AS expires_valid,GREATEST(0,floor(extract(epoch FROM p.expires_at-now())))::bigint AS expires_remaining FROM phone_invitations p JOIN vaults v ON v.vault_id=p.vault_id WHERE p.id=$1")
        .bind(invitation_id).fetch_optional(&mut *tx).await.map_err(database)?.ok_or(InvitationError(StatusCode::NOT_FOUND, "invitation_not_found"))?;
    if let Some(context) = hosted.as_ref()
        && row.get::<Uuid, _>("vault_id") != context.vault_id
    {
        return Err(InvitationError(StatusCode::FORBIDDEN, "foreign_vault"));
    }
    if row.get::<String, _>("origin") != state.canonical_origin {
        return Err(InvitationError(StatusCode::FORBIDDEN, "origin_mismatch"));
    }
    let is_owner = owner.is_some_and(|value| {
        value.vault_id == row.get::<Uuid, _>("vault_id")
            && value.device_id == row.get::<Uuid, _>("owner_device_id")
    });
    let is_claimant = claim_digest.is_some_and(|value| {
        row.get::<Option<Vec<u8>>, _>("claim_secret_digest")
            .is_some_and(|stored| stored == value)
    });
    if !is_owner && !is_claimant {
        return Err(InvitationError(
            StatusCode::UNAUTHORIZED,
            "invitation_unauthorized",
        ));
    }
    status_from_row(&row, is_owner || is_claimant)
}

async fn approve_invitation(
    State(state): State<InvitationRouterState>,
    headers: HeaderMap,
    Path(invitation_id): Path<Uuid>,
    Json(request): Json<ApproveInvitationRequest>,
) -> Result<Json<InvitationStatus>, InvitationError> {
    let principal =
        authorize_owner(&state, &headers, EnrollmentOperation::ApproveInvitation).await?;
    let mut tx = scope::begin(&state.db, principal.vault_id)
        .await
        .map_err(database)?;
    revalidate_owner(&mut tx, &principal).await?;
    let row = invitation_for_owner(&mut tx, invitation_id, &principal).await?;
    withdraw_legacy_enrollment(&mut tx, &headers, &principal).await?;
    if row.get::<String, _>("origin") != state.canonical_origin {
        return Err(InvitationError(StatusCode::FORBIDDEN, "origin_mismatch"));
    }
    valid_pending(&row)?;
    request_origin(&state, &headers)?;
    let context = context_from_row(&row)?;
    transcript_matches(&context, &request.transcript_hash)?;
    match parse_method(row.get("method"))? {
        EnrollmentMethod::Transfer => {
            let package = request
                .encrypted_key_package
                .as_deref()
                .ok_or(bad("transfer_package_required"))?;
            let package = URL_SAFE_NO_PAD
                .decode(package)
                .ok()
                .filter(|value| !value.is_empty() && value.len() <= MAX_HANDOFF_BYTES)
                .ok_or(bad("invalid_transfer_package"))?;
            let nonce = request
                .transfer_nonce
                .as_deref()
                .ok_or(bad("transfer_nonce_required"))?;
            let nonce = decode_base64url::<24>(nonce).map_err(|_| bad("invalid_transfer_nonce"))?;
            sqlx::query("UPDATE phone_invitations SET state='approved',encrypted_key_package=$2,transfer_nonce=$3 WHERE id=$1").bind(invitation_id).bind(package).bind(nonce.as_slice()).execute(&mut *tx).await.map_err(database)?;
        }
        EnrollmentMethod::SharedSecret => {
            if request.encrypted_key_package.is_some() || request.transfer_nonce.is_some() {
                return Err(bad("shared_secret_has_no_transfer"));
            }
            sqlx::query("UPDATE phone_invitations SET state='approved' WHERE id=$1")
                .bind(invitation_id)
                .execute(&mut *tx)
                .await
                .map_err(database)?;
        }
    }
    let row = sqlx::query("SELECT p.*,v.public_key_profile,p.expires_at > now() AS expires_valid,GREATEST(0,floor(extract(epoch FROM p.expires_at-now())))::bigint AS expires_remaining FROM phone_invitations p JOIN vaults v ON v.vault_id=p.vault_id WHERE p.id=$1").bind(invitation_id).fetch_one(&mut *tx).await.map_err(database)?;
    tx.commit().await.map_err(database)?;
    status_from_row(&row, true)
}

async fn confirm_invitation(
    State(state): State<InvitationRouterState>,
    headers: HeaderMap,
    Path(invitation_id): Path<Uuid>,
    Json(request): Json<ConfirmInvitationRequest>,
) -> Result<Json<AdmissionComplete>, InvitationError> {
    let hosted = if state.hosted {
        Some(
            authorize_hosted(
                &state,
                &headers,
                EnrollmentOperation::ConfirmInvitation,
                None,
            )
            .await?,
        )
    } else {
        None
    };
    request_origin(&state, &headers)?;
    let secret = token_digest(&request.claim_secret)?;
    let admission_signature = decode_base64url::<64>(&request.admission_signature)
        .map_err(|_| unauthorized("invalid_admission_proof"))?;
    let device_signature = decode_base64url::<64>(&request.device_signature)
        .map_err(|_| unauthorized("invalid_device_proof"))?;
    let mut tx = match hosted.as_ref() {
        Some(context) => scope::begin(&state.db, context.vault_id).await,
        None => state.db.begin().await,
    }
    .map_err(database)?;
    let row = sqlx::query("SELECT p.*,v.public_key_profile,v.encrypted_vault_check_header,v.profile_fingerprint current_fingerprint,v.key_epoch current_epoch,k.admission_public_key,(d.revoked_at IS NOT NULL) owner_revoked,p.expires_at > now() AS expires_valid,GREATEST(0,floor(extract(epoch FROM p.expires_at-now())))::bigint AS expires_remaining FROM phone_invitations p JOIN vaults v ON v.vault_id=p.vault_id JOIN vault_admission_keys k ON k.vault_id=v.vault_id AND k.key_epoch=v.key_epoch AND k.profile_fingerprint=v.profile_fingerprint JOIN devices d ON d.vault_id=p.vault_id AND d.device_id=p.owner_device_id WHERE p.id=$1 FOR UPDATE OF p,v,d")
        .bind(invitation_id).fetch_optional(&mut *tx).await.map_err(database)?.ok_or(InvitationError(StatusCode::NOT_FOUND, "invitation_not_found"))?;
    if row.get::<String, _>("origin") != state.canonical_origin {
        return Err(InvitationError(StatusCode::FORBIDDEN, "origin_mismatch"));
    }
    if let Some(context) = hosted.as_ref()
        && row.get::<Uuid, _>("vault_id") != context.vault_id
    {
        return Err(InvitationError(StatusCode::FORBIDDEN, "foreign_vault"));
    }
    if row
        .get::<Option<Vec<u8>>, _>("claim_secret_digest")
        .is_none_or(|stored| stored != secret)
    {
        return Err(unauthorized("invitation_unauthorized"));
    }
    let already_consumed = row.get::<String, _>("state") == "consumed";
    if !already_consumed {
        valid_approved(&row)?;
    }
    if row.get::<String, _>("profile_fingerprint") != row.get::<String, _>("current_fingerprint")
        || row.get::<i32, _>("key_epoch") != row.get::<i32, _>("current_epoch")
        || row.get::<bool, _>("owner_revoked")
    {
        return Err(InvitationError(
            StatusCode::CONFLICT,
            "invitation_epoch_or_owner_changed",
        ));
    }
    let context = context_from_row(&row)?;
    transcript_matches(&context, &request.transcript_hash)?;
    let admission_key: [u8; 32] = row
        .get::<Vec<u8>, _>("admission_public_key")
        .try_into()
        .map_err(|_| unauthorized("invitation_invalid"))?;
    let signing_key: [u8; 32] = row
        .get::<Vec<u8>, _>("claimed_public_key")
        .try_into()
        .map_err(|_| unauthorized("invitation_invalid"))?;
    if !verify_admission_signature(&admission_key, &context, &admission_signature).unwrap_or(false)
        || !verify_device_proof(&signing_key, &context, &device_signature).unwrap_or(false)
    {
        return Err(unauthorized("invitation_proof_mismatch"));
    }
    let device_id: Uuid = row.get("claimed_device_id");
    let role = role_for(row.get("claimed_kind"));
    if already_consumed {
        let material = VaultMaterial {
            profile: row.get("public_key_profile"),
            check_header: encode_base64url(&row.get::<Vec<u8>, _>("encrypted_vault_check_header")),
            fingerprint: row.get("current_fingerprint"),
            key_epoch: row.get::<i32, _>("current_epoch") as u32,
            admission_public_key: encode_base64url(&admission_key),
        };
        tx.commit().await.map_err(database)?;
        return Ok(Json(AdmissionComplete {
            vault_id: row.get("vault_id"),
            device_id,
            role,
            material,
        }));
    }
    let inserted = sqlx::query("INSERT INTO devices(vault_id,device_id,role,public_key,profile_fingerprint,key_epoch,name,kind) VALUES($1,$2,$3,jsonb_build_object('ed25519_public_key',$4),$5,$6,$7,$8) ON CONFLICT (vault_id,device_id) DO NOTHING")
        .bind(row.get::<Uuid,_>("vault_id")).bind(device_id).bind(role_name(role)).bind(URL_SAFE_NO_PAD.encode(signing_key)).bind(row.get::<String,_>("current_fingerprint")).bind(row.get::<i32,_>("current_epoch")).bind(row.get::<String,_>("claimed_name")).bind(row.get::<String,_>("claimed_kind")).execute(&mut *tx).await.map_err(database)?;
    if inserted.rows_affected() == 1 {
        sqlx::query(
            "INSERT INTO device_credentials(token_digest,vault_id,device_id) VALUES($1,$2,$3)",
        )
        .bind(row.get::<Vec<u8>, _>("claimed_credential_digest"))
        .bind(row.get::<Uuid, _>("vault_id"))
        .bind(device_id)
        .execute(&mut *tx)
        .await
        .map_err(database)?;
    } else {
        return Err(InvitationError(StatusCode::CONFLICT, "device_id_conflict"));
    }
    sqlx::query("UPDATE phone_invitations SET state='consumed' WHERE id=$1 AND state='approved'")
        .bind(invitation_id)
        .execute(&mut *tx)
        .await
        .map_err(database)?;
    let material = VaultMaterial {
        profile: row.get("public_key_profile"),
        check_header: encode_base64url(&row.get::<Vec<u8>, _>("encrypted_vault_check_header")),
        fingerprint: row.get("current_fingerprint"),
        key_epoch: row.get::<i32, _>("current_epoch") as u32,
        admission_public_key: encode_base64url(&admission_key),
    };
    tx.commit().await.map_err(database)?;
    Ok(Json(AdmissionComplete {
        vault_id: row.get("vault_id"),
        device_id,
        role,
        material,
    }))
}

async fn cancel_invitation(
    State(state): State<InvitationRouterState>,
    headers: HeaderMap,
    Path(invitation_id): Path<Uuid>,
) -> Result<StatusCode, InvitationError> {
    let principal =
        authorize_owner(&state, &headers, EnrollmentOperation::CancelInvitation).await?;
    let mut tx = scope::begin(&state.db, principal.vault_id)
        .await
        .map_err(database)?;
    revalidate_owner(&mut tx, &principal).await?;
    let row = invitation_for_owner(&mut tx, invitation_id, &principal).await?;
    withdraw_legacy_enrollment(&mut tx, &headers, &principal).await?;
    if row.get::<String, _>("state") == "consumed" {
        return Err(InvitationError(StatusCode::CONFLICT, "invitation_consumed"));
    }
    sqlx::query("UPDATE phone_invitations SET state='denied' WHERE id=$1")
        .bind(invitation_id)
        .execute(&mut *tx)
        .await
        .map_err(database)?;
    tx.commit().await.map_err(database)?;
    Ok(StatusCode::NO_CONTENT)
}

type Owner = OwnerCredential;

async fn owner(
    state: &InvitationRouterState,
    headers: &HeaderMap,
) -> Result<Owner, InvitationError> {
    resolve_owner_credential(&state.db, state.hosted, headers)
        .await
        .map_err(owner_authorization)
}

fn owner_authorization(error: OwnerAuthorizationError) -> InvitationError {
    match error {
        OwnerAuthorizationError::InvalidBearer => unauthorized("invalid_bearer"),
        OwnerAuthorizationError::OwnerRequired => unauthorized("owner_required"),
        OwnerAuthorizationError::Database(error) => database(error),
    }
}
async fn authorize_owner(
    state: &InvitationRouterState,
    headers: &HeaderMap,
    operation: EnrollmentOperation,
) -> Result<Owner, InvitationError> {
    let account = if state.hosted {
        Some(authorize_hosted(state, headers, operation, None).await?)
    } else {
        None
    };
    let owner = owner(state, headers).await?;
    if let Some(account) = account
        && account.vault_id != owner.vault_id
    {
        return Err(InvitationError(StatusCode::FORBIDDEN, "foreign_vault"));
    }
    Ok(owner)
}
async fn withdraw_legacy_enrollment(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    headers: &HeaderMap,
    owner: &Owner,
) -> Result<(), InvitationError> {
    crate::attachment_retention::withdraw_legacy_retention_readiness_on_connection(
        tx,
        headers,
        owner.vault_id,
        owner.device_id,
    )
    .await
    .map_err(database)
}

async fn revalidate_owner(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    owner: &Owner,
) -> Result<(), InvitationError> {
    let valid = sqlx::query_scalar::<_,bool>("SELECT EXISTS(SELECT 1 FROM device_credentials c JOIN devices d ON d.vault_id=c.vault_id AND d.device_id=c.device_id WHERE c.token_digest=$1 AND c.vault_id=$2 AND c.device_id=$3 AND c.revoked_at IS NULL AND d.revoked_at IS NULL AND d.role='owner')").bind(&owner.token_digest).bind(owner.vault_id).bind(owner.device_id).fetch_one(&mut **tx).await.map_err(database)?;
    if valid {
        Ok(())
    } else {
        Err(unauthorized("owner_required"))
    }
}
fn route_method(operation: EnrollmentOperation) -> Method {
    match operation {
        EnrollmentOperation::ReadInvitation => Method::GET,
        EnrollmentOperation::CancelInvitation => Method::DELETE,
        EnrollmentOperation::ManualChallenge
        | EnrollmentOperation::ManualComplete
        | EnrollmentOperation::CreateInvitation
        | EnrollmentOperation::ApproveInvitation
        | EnrollmentOperation::ConfirmInvitation
        | EnrollmentOperation::CreateComputerEnrollment
        | EnrollmentOperation::ReadComputerEnrollment
        | EnrollmentOperation::ApproveComputerEnrollment
        | EnrollmentOperation::ConfirmComputerEnrollment => Method::POST,
        EnrollmentOperation::CancelComputerEnrollment => Method::DELETE,
    }
}
async fn authorize_hosted(
    state: &InvitationRouterState,
    headers: &HeaderMap,
    operation: EnrollmentOperation,
    vault_id: Option<Uuid>,
) -> Result<crate::api::EnrollmentAuthContext, InvitationError> {
    state
        .authorization
        .authorize_enrollment(EnrollmentAuthorizationRequest {
            headers: headers.clone(),
            method: route_method(operation),
            operation,
            vault_id,
        })
        .await
        .map_err(|_| unauthorized("enrollment_unauthorized"))
}
async fn invitation_for_owner(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    id: Uuid,
    owner: &Owner,
) -> Result<sqlx::postgres::PgRow, InvitationError> {
    sqlx::query("SELECT p.*,v.public_key_profile,p.expires_at > now() AS expires_valid,GREATEST(0,floor(extract(epoch FROM p.expires_at-now())))::bigint AS expires_remaining FROM phone_invitations p JOIN vaults v ON v.vault_id=p.vault_id WHERE p.id=$1 AND p.vault_id=$2 AND p.owner_device_id=$3 FOR UPDATE OF p,v").bind(id).bind(owner.vault_id).bind(owner.device_id).fetch_optional(&mut **tx).await.map_err(database)?.ok_or(InvitationError(StatusCode::NOT_FOUND,"invitation_not_found"))
}
async fn expire(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    id: Uuid,
) -> Result<(), InvitationError> {
    sqlx::query("UPDATE phone_invitations SET state='expired' WHERE id=$1 AND state NOT IN ('consumed','denied')").bind(id).execute(&mut **tx).await.map_err(database)?;
    Ok(())
}
async fn claim_rate_limit(
    state: &InvitationRouterState,
    key: [u8; 32],
) -> Result<(), InvitationError> {
    let now = SystemTime::now();
    let mut buckets = state.claims.lock().await;
    buckets
        .retain(|_, bucket| now.duration_since(bucket.window).unwrap_or_default() < CLAIM_WINDOW);
    let key = hex::encode(key);
    if buckets.len() >= CLAIM_BUCKET_CAP
        && !buckets.contains_key(&key)
        && let Some(oldest) = buckets
            .iter()
            .min_by_key(|(_, value)| value.window)
            .map(|(key, _)| key.clone())
    {
        buckets.remove(&oldest);
    }
    let bucket = buckets.entry(key).or_insert(ClaimBucket {
        window: now,
        count: 0,
    });
    if now.duration_since(bucket.window).unwrap_or_default() >= CLAIM_WINDOW {
        *bucket = ClaimBucket {
            window: now,
            count: 0,
        };
    }
    if bucket.count >= CLAIM_LIMIT {
        return Err(InvitationError(
            StatusCode::TOO_MANY_REQUESTS,
            "claim_rate_limited",
        ));
    }
    bucket.count += 1;
    Ok(())
}
fn validate_device(device: &ProposedDevice) -> Result<[u8; 32], InvitationError> {
    validate_device_name(&device.name).map_err(|_| bad("invalid_device_name"))?;
    if device.device_id.is_nil() {
        return Err(bad("invalid_device_id"));
    }
    decode_base64url(&device.signing_public_key).map_err(|_| bad("invalid_signing_key"))
}
fn token_digest(value: &str) -> Result<[u8; 32], InvitationError> {
    decode_base64url::<32>(value).map_err(|_| bad("invalid_invitation_token"))?;
    Ok(digest(value))
}
fn claim_secret(headers: &HeaderMap) -> Result<[u8; 32], InvitationError> {
    token_digest(
        headers
            .get("peppy-claim-secret")
            .and_then(|value| value.to_str().ok())
            .ok_or(unauthorized("missing_claim_secret"))?,
    )
}
fn credential_digest(value: &str) -> Result<[u8; 32], InvitationError> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        return Err(bad("invalid_credential_digest"));
    }
    hex::decode(value)
        .ok()
        .and_then(|value| value.try_into().ok())
        .ok_or(bad("invalid_credential_digest"))
}
fn random_secret() -> String {
    let mut value = [0; 32];
    libsodium_rs::random::fill_bytes(&mut value);
    encode_base64url(&value)
}
fn digest(value: &str) -> [u8; 32] {
    Sha256::digest(value.as_bytes()).into()
}
fn canonical_configured_origin(value: &str) -> Result<String, &'static str> {
    let url = url::Url::parse(value).map_err(|_| "configured enrollment origin is invalid")?;
    let loopback = url.host_str().is_some_and(|host| {
        host == "localhost"
            || host
                .parse::<std::net::IpAddr>()
                .is_ok_and(|address| address.is_loopback())
    });
    if url.scheme() != "https" && !(url.scheme() == "http" && loopback) {
        return Err("configured enrollment origin must be HTTPS or loopback HTTP");
    }
    let origin = url.origin().ascii_serialization();
    if origin == "null" || origin != value.trim_end_matches('/') {
        return Err("configured enrollment origin must not include a path, query, or fragment");
    }
    Ok(origin)
}
fn request_origin(
    state: &InvitationRouterState,
    headers: &HeaderMap,
) -> Result<String, InvitationError> {
    if state.hosted {
        return Ok(state.canonical_origin.clone());
    }
    let Some(value) = headers.get("origin") else {
        return Ok(state.canonical_origin.clone());
    };
    let value = value.to_str().map_err(|_| bad("invalid_origin"))?;
    let origin = canonical_configured_origin(value).map_err(bad)?;
    if origin != state.canonical_origin {
        return Err(InvitationError(StatusCode::FORBIDDEN, "origin_mismatch"));
    }
    Ok(origin)
}
fn exact_claim(
    row: &sqlx::postgres::PgRow,
    request: &ClaimInvitationRequest,
    key: [u8; 32],
    transfer: [u8; 32],
    credential: [u8; 32],
    claim_authenticator: [u8; 32],
) -> bool {
    row.get::<Option<Uuid>, _>("claimed_device_id") == Some(request.device.device_id)
        && row.get::<Option<String>, _>("claimed_kind").as_deref()
            == Some(client_kind_name(request.device.kind))
        && row.get::<Option<String>, _>("claimed_name").as_deref() == Some(&request.device.name)
        && row
            .get::<Option<Vec<u8>>, _>("claimed_public_key")
            .is_some_and(|value| value == key)
        && row
            .get::<Option<Vec<u8>>, _>("claimed_transfer_public_key")
            .is_some_and(|value| value == transfer)
        && row
            .get::<Option<Vec<u8>>, _>("claimed_credential_digest")
            .is_some_and(|value| value == credential)
        && row
            .get::<Option<Vec<u8>>, _>("claim_authenticator")
            .is_some_and(|value| value == claim_authenticator)
}
fn context_from_row(row: &sqlx::postgres::PgRow) -> Result<EnrollmentContext, InvitationError> {
    Ok(EnrollmentContext {
        purpose: EnrollmentPurpose::Phone,
        canonical_origin: row.get("origin"),
        vault_id: row.get("vault_id"),
        fingerprint: credential_digest(&row.get::<String, _>("profile_fingerprint"))
            .map_err(|_| unauthorized("invitation_invalid"))?,
        key_epoch: row.get::<i32, _>("key_epoch") as u32,
        enrollment_id: row.get("id"),
        server_nonce: row
            .get::<Vec<u8>, _>("token_digest")
            .try_into()
            .map_err(|_| unauthorized("invitation_invalid"))?,
        owner_transfer_public_key: row
            .get::<Vec<u8>, _>("owner_transfer_public_key")
            .try_into()
            .map_err(|_| unauthorized("invitation_invalid"))?,
        recipient_transfer_public_key: row
            .get::<Option<Vec<u8>>, _>("claimed_transfer_public_key")
            .ok_or(unauthorized("invitation_unclaimed"))?
            .try_into()
            .map_err(|_| unauthorized("invitation_invalid"))?,
        recipient_signing_public_key: row
            .get::<Option<Vec<u8>>, _>("claimed_public_key")
            .ok_or(unauthorized("invitation_unclaimed"))?
            .try_into()
            .map_err(|_| unauthorized("invitation_invalid"))?,
        device_id: row
            .get::<Option<Uuid>, _>("claimed_device_id")
            .ok_or(unauthorized("invitation_unclaimed"))?,
        credential_digest: row
            .get::<Option<Vec<u8>>, _>("claimed_credential_digest")
            .ok_or(unauthorized("invitation_unclaimed"))?
            .try_into()
            .map_err(|_| unauthorized("invitation_invalid"))?,
        role: crypto_role(role_for(row.get("claimed_kind"))),
    })
}
fn transcript_matches(context: &EnrollmentContext, hash: &str) -> Result<(), InvitationError> {
    let supplied = decode_base64url::<32>(hash).map_err(|_| bad("invalid_transcript_hash"))?;
    let expected: [u8; 32] = Sha256::digest(
        context
            .canonical_bytes()
            .map_err(|_| bad("invalid_context"))?,
    )
    .into();
    if supplied == expected {
        Ok(())
    } else {
        Err(InvitationError(StatusCode::CONFLICT, "transcript_mismatch"))
    }
}
fn status_from_row(
    row: &sqlx::postgres::PgRow,
    authorized: bool,
) -> Result<Json<InvitationStatus>, InvitationError> {
    let expired = !row.get::<bool, _>("expires_valid");
    let state = if expired {
        EnrollmentState::Expired
    } else {
        parse_state(row.get("state"))?
    };
    let claim = if row.get::<Option<Uuid>, _>("claimed_device_id").is_some() {
        Some(InvitationPublicClaim {
            device: ProposedDevice {
                device_id: row.get("claimed_device_id"),
                kind: parse_kind(row.get("claimed_kind"))?,
                name: row.get("claimed_name"),
                signing_public_key: encode_base64url(&row.get::<Vec<u8>, _>("claimed_public_key")),
            },
            transfer_public_key: encode_base64url(
                &row.get::<Vec<u8>, _>("claimed_transfer_public_key"),
            ),
            credential_digest: hex::encode(row.get::<Vec<u8>, _>("claimed_credential_digest")),
            claim_authenticator: encode_base64url(&row.get::<Vec<u8>, _>("claim_authenticator")),
        })
    } else {
        None
    };
    let transcript = if authorized && claim.is_some() {
        Some(transcript_from_row(row)?)
    } else {
        None
    };
    let approved = authorized && matches!(state, EnrollmentState::Approved);
    Ok(Json(InvitationStatus {
        state,
        expires_in_seconds: remaining(row),
        claim,
        transcript,
        encrypted_key_package: approved
            .then(|| {
                row.get::<Option<Vec<u8>>, _>("encrypted_key_package")
                    .map(|value| encode_base64url(&value))
            })
            .flatten(),
        transfer_nonce: approved
            .then(|| {
                row.get::<Option<Vec<u8>>, _>("transfer_nonce")
                    .map(|value| encode_base64url(&value))
            })
            .flatten(),
        profile: authorized.then(|| row.get("public_key_profile")),
    }))
}
fn transcript_from_row(
    row: &sqlx::postgres::PgRow,
) -> Result<EnrollmentTranscript, InvitationError> {
    Ok(EnrollmentTranscript {
        origin: row.get("origin"),
        vault_id: row.get("vault_id"),
        fingerprint: row.get("profile_fingerprint"),
        key_epoch: row.get::<i32, _>("key_epoch") as u32,
        enrollment_id: row.get("id"),
        nonce: encode_base64url(&row.get::<Vec<u8>, _>("token_digest")),
        owner_transfer_public_key: encode_base64url(
            &row.get::<Vec<u8>, _>("owner_transfer_public_key"),
        ),
        recipient_transfer_public_key: encode_base64url(
            &row.get::<Vec<u8>, _>("claimed_transfer_public_key"),
        ),
        signing_public_key: encode_base64url(&row.get::<Vec<u8>, _>("claimed_public_key")),
        device_id: row.get("claimed_device_id"),
        credential_digest: hex::encode(row.get::<Vec<u8>, _>("claimed_credential_digest")),
        role: role_for(row.get("claimed_kind")),
    })
}
fn valid_pending(row: &sqlx::postgres::PgRow) -> Result<(), InvitationError> {
    if !row.get::<bool, _>("expires_valid") {
        return Err(InvitationError(StatusCode::GONE, "invitation_expired"));
    }
    if row.get::<String, _>("state") != "claimed" {
        return Err(InvitationError(
            StatusCode::CONFLICT,
            "invitation_not_claimed",
        ));
    }
    Ok(())
}
fn valid_approved(row: &sqlx::postgres::PgRow) -> Result<(), InvitationError> {
    if !row.get::<bool, _>("expires_valid") {
        return Err(InvitationError(StatusCode::GONE, "invitation_expired"));
    }
    if row.get::<String, _>("state") != "approved" {
        return Err(InvitationError(
            StatusCode::CONFLICT,
            "invitation_not_approved",
        ));
    }
    Ok(())
}
fn remaining(row: &sqlx::postgres::PgRow) -> u32 {
    row.get::<i64, _>("expires_remaining") as u32
}
fn method_name(method: EnrollmentMethod) -> &'static str {
    match method {
        EnrollmentMethod::Transfer => "transfer",
        EnrollmentMethod::SharedSecret => "shared_secret",
    }
}
fn parse_method(value: String) -> Result<EnrollmentMethod, InvitationError> {
    match value.as_str() {
        "transfer" => Ok(EnrollmentMethod::Transfer),
        "shared_secret" => Ok(EnrollmentMethod::SharedSecret),
        _ => Err(unauthorized("invitation_invalid")),
    }
}
fn parse_state(value: String) -> Result<EnrollmentState, InvitationError> {
    match value.as_str() {
        "waiting" => Ok(EnrollmentState::Waiting),
        "claimed" => Ok(EnrollmentState::Claimed),
        "approved" => Ok(EnrollmentState::Approved),
        "confirmed" => Ok(EnrollmentState::Confirmed),
        "consumed" => Ok(EnrollmentState::Consumed),
        "expired" => Ok(EnrollmentState::Expired),
        "denied" => Ok(EnrollmentState::Denied),
        _ => Err(unauthorized("invitation_invalid")),
    }
}
fn parse_kind(value: Option<String>) -> Result<ClientKind, InvitationError> {
    match value.as_deref() {
        Some("browser") => Ok(ClientKind::Browser),
        Some("desktop") => Ok(ClientKind::Desktop),
        Some("android") => Ok(ClientKind::Android),
        Some("ios") => Ok(ClientKind::Ios),
        _ => Err(unauthorized("invitation_invalid")),
    }
}
fn role_for(kind: Option<String>) -> EnrollmentRole {
    match kind.as_deref() {
        Some("android") | Some("ios") => EnrollmentRole::Gateway,
        _ => EnrollmentRole::Device,
    }
}
fn role_name(role: EnrollmentRole) -> &'static str {
    match role {
        EnrollmentRole::Owner => "owner",
        EnrollmentRole::Device => "device",
        EnrollmentRole::Gateway => "gateway",
    }
}
fn crypto_role(role: EnrollmentRole) -> CryptoRole {
    match role {
        EnrollmentRole::Owner => CryptoRole::Owner,
        EnrollmentRole::Device => CryptoRole::Device,
        EnrollmentRole::Gateway => CryptoRole::Gateway,
    }
}
fn database(error: sqlx::Error) -> InvitationError {
    tracing::warn!(
        operation = "phone_enrollment",
        kind = crate::api::database_error_kind(&error),
        "database operation failed"
    );
    InvitationError(StatusCode::SERVICE_UNAVAILABLE, "database_unavailable")
}
fn bad(code: &'static str) -> InvitationError {
    InvitationError(StatusCode::UNPROCESSABLE_ENTITY, code)
}
fn unauthorized(code: &'static str) -> InvitationError {
    InvitationError(StatusCode::UNAUTHORIZED, code)
}
struct InvitationError(StatusCode, &'static str);
impl IntoResponse for InvitationError {
    fn into_response(self) -> axum::response::Response {
        (
            self.0,
            Json(serde_json::json!({"error":self.1,"code":self.1})),
        )
            .into_response()
    }
}
