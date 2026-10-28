//! Authenticated rendezvous for QR-first computer enrollment.

use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderMap, Method, StatusCode},
    response::IntoResponse,
    routing::{delete, post},
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use peppy_crypto::enrollment::{
    EnrollmentContext, EnrollmentPurpose, EnrollmentRole as CryptoRole, verify_admission_signature,
    verify_device_proof,
};
use peppy_hosted_api::{
    computer_enrollment::{
        ApproveComputerEnrollmentRequest, ComputerEnrollmentConfirmRequest,
        ComputerEnrollmentCreated, ComputerEnrollmentStatus, ComputerEnrollmentTranscript,
        CreateComputerEnrollmentRequest,
    },
    enrollment::{AdmissionComplete, ClientKind, EnrollmentRole, EnrollmentState, VaultMaterial},
};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Row};
use std::sync::Arc;
use uuid::Uuid;

use crate::{
    api::{EnrollmentAuthorization, EnrollmentAuthorizationRequest, EnrollmentOperation},
    enrollment::{client_kind_name, validate_device_name},
    scope,
};

const TTL_SECONDS: i64 = 300;
const MAX_HANDOFF_BYTES: usize = 16 * 1024 + 16;
const ACTIVE_INVITATION_LIMIT: i64 = 3;

#[derive(Clone)]
pub struct ComputerEnrollmentRouterState {
    pub db: PgPool,
    pub authorization: Arc<dyn EnrollmentAuthorization>,
    pub hosted: bool,
    origin: String,
}

impl ComputerEnrollmentRouterState {
    pub fn new(
        db: PgPool,
        authorization: Arc<dyn EnrollmentAuthorization>,
        hosted: bool,
        origin: String,
    ) -> Result<Self, &'static str> {
        Ok(Self {
            db,
            authorization,
            hosted,
            origin: canonical_origin(&origin)?,
        })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ClaimRequest {
    claim_token: String,
}

pub fn router(state: ComputerEnrollmentRouterState) -> Router {
    Router::new()
        .route("/v1/enrollment/computers", post(create))
        .route("/v1/enrollment/computers/{id}/status", post(status))
        .route("/v1/enrollment/computers/{id}/approve", post(approve))
        .route("/v1/enrollment/computers/{id}/confirm", post(confirm))
        .route("/v1/enrollment/computers/{id}", delete(cancel))
        .with_state(state)
}

const CLEANUP_BATCH_LIMIT: i64 = 1_000;

/// Deletes at most one bounded batch of retired enrollment rows per maintenance tick.
///
/// Approved handoffs stay confirmable until `expires_at`; they become eligible only after
/// the same 30-day grace used for other retired rows, so live or recently expired approvals
/// and consumed replay rows are never removed by this sweep.
pub(crate) async fn cleanup_expired_enrollments(db: &PgPool) -> Result<u64, sqlx::Error> {
    cleanup_batch(db, CLEANUP_BATCH_LIMIT).await
}

async fn cleanup_batch(db: &PgPool, limit: i64) -> Result<u64, sqlx::Error> {
    let deleted = sqlx::query(
        "DELETE FROM computer_enrollments WHERE ctid IN (SELECT ctid FROM computer_enrollments WHERE ((state IN ('cancelled','expired','consumed') OR (state='waiting' AND expires_at < now())) AND created_at < now() - interval '30 days') OR (state='approved' AND expires_at < now() - interval '30 days') ORDER BY expires_at LIMIT $1 FOR UPDATE SKIP LOCKED)",
    )
    .bind(limit)
    .execute(db)
    .await?;
    Ok(deleted.rows_affected())
}

async fn create(
    State(state): State<ComputerEnrollmentRouterState>,
    headers: HeaderMap,
    Json(request): Json<CreateComputerEnrollmentRequest>,
) -> Result<Json<ComputerEnrollmentCreated>, Error> {
    let owner = authorize_owner(
        &state,
        &headers,
        EnrollmentOperation::CreateComputerEnrollment,
    )
    .await?;
    request_origin(&state, &headers)?;
    let values = create_values(&request)?;
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
    sqlx::query("UPDATE computer_enrollments SET state='expired' WHERE vault_id=$1 AND owner_device_id=$2 AND state='waiting' AND expires_at < now()")
        .bind(owner.vault_id)
        .bind(owner.device_id)
        .execute(&mut *tx)
        .await
        .map_err(database)?;
    let existing = sqlx::query("SELECT *, GREATEST(0,floor(extract(epoch FROM expires_at-now())))::bigint AS expires_remaining FROM computer_enrollments WHERE id=$1 FOR UPDATE").bind(request.invitation_id).fetch_optional(&mut *tx).await.map_err(database)?;
    if let Some(row) = existing {
        if exact_create(&row, &owner, &request, &values, &state.origin) {
            let response = ComputerEnrollmentCreated {
                server_nonce: b64(&row.get::<Vec<u8>, _>("server_nonce")),
                expires_in_seconds: remaining(&row),
            };
            tx.commit().await.map_err(database)?;
            return Ok(Json(response));
        }
        return Err(Error(StatusCode::CONFLICT, "computer_enrollment_immutable"));
    }
    let active: i64 = sqlx::query_scalar("SELECT count(*) FROM computer_enrollments WHERE vault_id=$1 AND owner_device_id=$2 AND state IN ('waiting','approved') AND expires_at > now()")
        .bind(owner.vault_id)
        .bind(owner.device_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(database)?;
    if active >= ACTIVE_INVITATION_LIMIT {
        return Err(Error(
            StatusCode::TOO_MANY_REQUESTS,
            "active_invitation_limit",
        ));
    }
    let mut nonce = [0; 32];
    libsodium_rs::random::fill_bytes(&mut nonce);
    let inserted = sqlx::query("INSERT INTO computer_enrollments(id,vault_id,owner_device_id,origin,claim_token_digest,server_nonce,recipient_device_id,recipient_kind,recipient_name,recipient_signing_public_key,recipient_transfer_public_key,recipient_credential_digest,recipient_commitment,profile_fingerprint,key_epoch,state,expires_at) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,'waiting',now()+interval '300 seconds') ON CONFLICT (id) DO NOTHING")
        .bind(request.invitation_id).bind(owner.vault_id).bind(owner.device_id).bind(&state.origin).bind(values.claim_digest.as_slice()).bind(nonce.as_slice()).bind(request.recipient.device.device_id).bind(client_kind_name(request.recipient.device.kind)).bind(&request.recipient.device.name).bind(values.signing_key.as_slice()).bind(values.transfer_key.as_slice()).bind(values.credential_digest.as_slice()).bind(values.commitment.as_slice()).bind(vault.get::<String,_>("profile_fingerprint")).bind(vault.get::<i32,_>("key_epoch")).execute(&mut *tx).await.map_err(database)?;
    if inserted.rows_affected() != 1 {
        let row = sqlx::query("SELECT *, GREATEST(0,floor(extract(epoch FROM expires_at-now())))::bigint AS expires_remaining FROM computer_enrollments WHERE id=$1 FOR UPDATE")
            .bind(request.invitation_id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(database)?;
        if let Some(row) = row
            && exact_create(&row, &owner, &request, &values, &state.origin)
        {
            let response = ComputerEnrollmentCreated {
                server_nonce: b64(&row.get::<Vec<u8>, _>("server_nonce")),
                expires_in_seconds: remaining(&row),
            };
            tx.commit().await.map_err(database)?;
            return Ok(Json(response));
        }
        return Err(Error(StatusCode::CONFLICT, "computer_enrollment_immutable"));
    }
    tx.commit().await.map_err(database)?;
    Ok(Json(ComputerEnrollmentCreated {
        server_nonce: b64(&nonce),
        expires_in_seconds: TTL_SECONDS as u32,
    }))
}

async fn status(
    State(state): State<ComputerEnrollmentRouterState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(request): Json<ClaimRequest>,
) -> Result<Json<ComputerEnrollmentStatus>, Error> {
    let hosted = authorize_claim_hosted(
        &state,
        &headers,
        EnrollmentOperation::ReadComputerEnrollment,
    )
    .await?;
    request_origin(&state, &headers)?;
    let token = claim_digest(&request.claim_token)?;
    let mut tx = match hosted.as_ref() {
        Some(context) => scope::begin(&state.db, context.vault_id).await,
        None => state.db.begin().await,
    }
    .map_err(database)?;
    let row = sqlx::query("SELECT *, expires_at > now() AS expires_valid,GREATEST(0,floor(extract(epoch FROM expires_at-now())))::bigint AS expires_remaining FROM computer_enrollments WHERE id=$1").bind(id).fetch_optional(&mut *tx).await.map_err(database)?.ok_or(Error(StatusCode::NOT_FOUND, "computer_enrollment_not_found"))?;
    if hosted.is_some_and(|context| context.vault_id != row.get::<Uuid, _>("vault_id")) {
        return Err(Error(StatusCode::FORBIDDEN, "foreign_vault"));
    }
    if row.get::<Vec<u8>, _>("claim_token_digest") != token {
        return Err(Error(
            StatusCode::UNAUTHORIZED,
            "computer_enrollment_unauthorized",
        ));
    }
    let response = status_from_row(&row)?;
    tx.commit().await.map_err(database)?;
    Ok(Json(response))
}

async fn approve(
    State(state): State<ComputerEnrollmentRouterState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(request): Json<ApproveComputerEnrollmentRequest>,
) -> Result<Json<ComputerEnrollmentStatus>, Error> {
    let owner = authorize_owner(
        &state,
        &headers,
        EnrollmentOperation::ApproveComputerEnrollment,
    )
    .await?;
    request_origin(&state, &headers)?;
    request
        .transcript
        .validate_fields()
        .map_err(|_| bad("invalid_transcript"))?;
    let package = URL_SAFE_NO_PAD
        .decode(&request.encrypted_key_package)
        .ok()
        .filter(|v| !v.is_empty() && v.len() <= MAX_HANDOFF_BYTES)
        .ok_or(bad("invalid_transfer_package"))?;
    let nonce = decode::<24>(&request.transfer_nonce, "invalid_transfer_nonce")?;
    let proof = decode::<32>(&request.owner_proof, "invalid_owner_proof")?;
    let mut tx = scope::begin(&state.db, owner.vault_id)
        .await
        .map_err(database)?;
    revalidate_owner(&mut tx, &owner).await?;
    let row = sqlx::query("SELECT c.*,v.profile_fingerprint current_fingerprint,v.key_epoch current_epoch,d.revoked_at IS NOT NULL AS owner_revoked,c.expires_at > now() AS expires_valid,GREATEST(0,floor(extract(epoch FROM c.expires_at-now())))::bigint AS expires_remaining FROM computer_enrollments c JOIN vaults v ON v.vault_id=c.vault_id JOIN devices d ON d.vault_id=c.vault_id AND d.device_id=c.owner_device_id WHERE c.id=$1 AND c.vault_id=$2 AND c.owner_device_id=$3 FOR UPDATE OF c,v,d").bind(id).bind(owner.vault_id).bind(owner.device_id).fetch_optional(&mut *tx).await.map_err(database)?.ok_or(Error(StatusCode::NOT_FOUND, "computer_enrollment_not_found"))?;
    if row.get::<bool, _>("owner_revoked")
        || row.get::<String, _>("profile_fingerprint")
            != row.get::<String, _>("current_fingerprint")
        || row.get::<i32, _>("key_epoch") != row.get::<i32, _>("current_epoch")
    {
        return Err(Error(
            StatusCode::CONFLICT,
            "computer_enrollment_epoch_or_owner_changed",
        ));
    }
    transcript_matches(&row, &request.transcript, &state.origin)?;
    if row.get::<String, _>("state") == "approved" {
        if row.get::<Vec<u8>, _>("encrypted_key_package") == package
            && row.get::<Vec<u8>, _>("transfer_nonce") == nonce
            && row.get::<Vec<u8>, _>("owner_proof") == proof
        {
            let response = status_from_row(&row)?;
            tx.commit().await.map_err(database)?;
            return Ok(Json(response));
        }
        return Err(Error(
            StatusCode::CONFLICT,
            "computer_enrollment_already_approved",
        ));
    }
    valid_waiting(&row)?;
    sqlx::query("UPDATE computer_enrollments SET state='approved',owner_transfer_public_key=$2,encrypted_key_package=$3,transfer_nonce=$4,owner_proof=$5 WHERE id=$1 AND state='waiting'").bind(id).bind(decode::<32>(&request.transcript.owner_transfer_public_key, "invalid_owner_transfer_key")?.as_slice()).bind(package).bind(nonce.as_slice()).bind(proof.as_slice()).execute(&mut *tx).await.map_err(database)?;
    let row = sqlx::query("SELECT *, expires_at > now() AS expires_valid,GREATEST(0,floor(extract(epoch FROM expires_at-now())))::bigint AS expires_remaining FROM computer_enrollments WHERE id=$1").bind(id).fetch_one(&mut *tx).await.map_err(database)?;
    tx.commit().await.map_err(database)?;
    Ok(Json(status_from_row(&row)?))
}

async fn confirm(
    State(state): State<ComputerEnrollmentRouterState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(request): Json<ComputerEnrollmentConfirmRequest>,
) -> Result<Json<AdmissionComplete>, Error> {
    let hosted = authorize_claim_hosted(
        &state,
        &headers,
        EnrollmentOperation::ConfirmComputerEnrollment,
    )
    .await?;
    request_origin(&state, &headers)?;
    let token = claim_digest(&request.claim_token)?;
    let device_signature = decode::<64>(&request.device_signature, "invalid_device_proof")?;
    let admission_signature =
        decode::<64>(&request.admission_signature, "invalid_admission_proof")?;
    let mut tx = match hosted.as_ref() {
        Some(context) => scope::begin(&state.db, context.vault_id).await,
        None => state.db.begin().await,
    }
    .map_err(database)?;
    let row = sqlx::query("SELECT c.*,v.public_key_profile,v.encrypted_vault_check_header,v.profile_fingerprint current_fingerprint,v.key_epoch current_epoch,(d.revoked_at IS NOT NULL) owner_revoked,k.admission_public_key,c.expires_at > now() AS expires_valid FROM computer_enrollments c JOIN vaults v ON v.vault_id=c.vault_id JOIN devices d ON d.vault_id=c.vault_id AND d.device_id=c.owner_device_id JOIN vault_admission_keys k ON k.vault_id=v.vault_id AND k.key_epoch=v.key_epoch AND k.profile_fingerprint=v.profile_fingerprint WHERE c.id=$1 FOR UPDATE OF c,v,d").bind(id).fetch_optional(&mut *tx).await.map_err(database)?.ok_or(Error(StatusCode::NOT_FOUND, "computer_enrollment_not_found"))?;
    if hosted.is_some_and(|context| context.vault_id != row.get::<Uuid, _>("vault_id")) {
        return Err(Error(StatusCode::FORBIDDEN, "foreign_vault"));
    }
    if row.get::<Vec<u8>, _>("claim_token_digest") != token {
        return Err(Error(
            StatusCode::UNAUTHORIZED,
            "computer_enrollment_unauthorized",
        ));
    }
    if row.get::<String, _>("state") != "consumed" {
        valid_approved(&row)?;
    }
    if row.get::<bool, _>("owner_revoked")
        || row.get::<String, _>("profile_fingerprint")
            != row.get::<String, _>("current_fingerprint")
        || row.get::<i32, _>("key_epoch") != row.get::<i32, _>("current_epoch")
    {
        return Err(Error(
            StatusCode::CONFLICT,
            "computer_enrollment_epoch_or_owner_changed",
        ));
    }
    let context = context_from_row(&row)?;
    let signing_key: [u8; 32] = row
        .get::<Vec<u8>, _>("recipient_signing_public_key")
        .try_into()
        .map_err(|_| unauthorized("computer_enrollment_invalid"))?;
    let admission_key: [u8; 32] = row
        .get::<Vec<u8>, _>("admission_public_key")
        .try_into()
        .map_err(|_| unauthorized("computer_enrollment_invalid"))?;
    if !verify_device_proof(&signing_key, &context, &device_signature).unwrap_or(false)
        || !verify_admission_signature(&admission_key, &context, &admission_signature)
            .unwrap_or(false)
    {
        return Err(unauthorized("computer_enrollment_proof_mismatch"));
    }
    let device_id: Uuid = row.get("recipient_device_id");
    if row.get::<String, _>("state") != "consumed" {
        let inserted = sqlx::query("INSERT INTO devices(vault_id,device_id,role,public_key,profile_fingerprint,key_epoch,name,kind) VALUES($1,$2,'device',jsonb_build_object('ed25519_public_key',$3),$4,$5,$6,$7) ON CONFLICT (vault_id,device_id) DO NOTHING").bind(row.get::<Uuid,_>("vault_id")).bind(device_id).bind(b64(&row.get::<Vec<u8>,_>("recipient_signing_public_key"))).bind(row.get::<String,_>("current_fingerprint")).bind(row.get::<i32,_>("current_epoch")).bind(row.get::<String,_>("recipient_name")).bind(row.get::<String,_>("recipient_kind")).execute(&mut *tx).await.map_err(database)?;
        if inserted.rows_affected() != 1 {
            return Err(Error(StatusCode::CONFLICT, "device_id_conflict"));
        }
        sqlx::query(
            "INSERT INTO device_credentials(token_digest,vault_id,device_id) VALUES($1,$2,$3)",
        )
        .bind(row.get::<Vec<u8>, _>("recipient_credential_digest"))
        .bind(row.get::<Uuid, _>("vault_id"))
        .bind(device_id)
        .execute(&mut *tx)
        .await
        .map_err(database)?;
        sqlx::query(
            "UPDATE computer_enrollments SET state='consumed' WHERE id=$1 AND state='approved'",
        )
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(database)?;
    }
    let response = admission(&row, device_id)?;
    tx.commit().await.map_err(database)?;
    Ok(Json(response))
}

async fn cancel(
    State(state): State<ComputerEnrollmentRouterState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, Error> {
    let owner = authorize_owner(
        &state,
        &headers,
        EnrollmentOperation::CancelComputerEnrollment,
    )
    .await?;
    let mut tx = scope::begin(&state.db, owner.vault_id)
        .await
        .map_err(database)?;
    revalidate_owner(&mut tx, &owner).await?;
    let changed = sqlx::query("UPDATE computer_enrollments SET state='cancelled' WHERE id=$1 AND vault_id=$2 AND owner_device_id=$3 AND state='waiting'").bind(id).bind(owner.vault_id).bind(owner.device_id).execute(&mut *tx).await.map_err(database)?;
    if changed.rows_affected() != 1 {
        return Err(Error(
            StatusCode::CONFLICT,
            "computer_enrollment_not_cancellable",
        ));
    }
    tx.commit().await.map_err(database)?;
    Ok(StatusCode::NO_CONTENT)
}

struct Owner {
    vault_id: Uuid,
    device_id: Uuid,
    token_digest: Vec<u8>,
}
async fn authorize_owner(
    state: &ComputerEnrollmentRouterState,
    headers: &HeaderMap,
    operation: EnrollmentOperation,
) -> Result<Owner, Error> {
    let hosted = if state.hosted {
        Some(authorize_hosted(state, headers, operation).await?)
    } else {
        None
    };
    let token = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .filter(|v| v.len() == 96 && v.bytes().all(|b| b.is_ascii_hexdigit()))
        .ok_or(unauthorized("invalid_bearer"))?;
    let digest = Sha256::digest(token.as_bytes());
    let row = sqlx::query("SELECT c.vault_id,c.device_id,c.token_digest FROM device_credentials c JOIN devices d ON d.vault_id=c.vault_id AND d.device_id=c.device_id WHERE c.token_digest=$1 AND c.revoked_at IS NULL AND d.revoked_at IS NULL AND d.role='owner'").bind(digest.as_slice()).fetch_optional(&state.db).await.map_err(database)?.ok_or(unauthorized("owner_required"))?;
    let owner = Owner {
        vault_id: row.get("vault_id"),
        device_id: row.get("device_id"),
        token_digest: row.get("token_digest"),
    };
    if hosted.is_some_and(|value| value.vault_id != owner.vault_id) {
        return Err(Error(StatusCode::FORBIDDEN, "foreign_vault"));
    }
    Ok(owner)
}
async fn authorize_claim_hosted(
    state: &ComputerEnrollmentRouterState,
    headers: &HeaderMap,
    operation: EnrollmentOperation,
) -> Result<Option<crate::api::EnrollmentAuthContext>, Error> {
    if state.hosted {
        return Ok(Some(authorize_hosted(state, headers, operation).await?));
    }
    Ok(None)
}
async fn authorize_hosted(
    state: &ComputerEnrollmentRouterState,
    headers: &HeaderMap,
    operation: EnrollmentOperation,
) -> Result<crate::api::EnrollmentAuthContext, Error> {
    state
        .authorization
        .authorize_enrollment(EnrollmentAuthorizationRequest {
            headers: headers.clone(),
            method: route_method(operation),
            operation,
            vault_id: None,
        })
        .await
        .map_err(|_| unauthorized("enrollment_unauthorized"))
}
fn route_method(operation: EnrollmentOperation) -> Method {
    match operation {
        EnrollmentOperation::CancelComputerEnrollment => Method::DELETE,
        _ => Method::POST,
    }
}
async fn revalidate_owner(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    owner: &Owner,
) -> Result<(), Error> {
    let valid: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM device_credentials c JOIN devices d ON d.vault_id=c.vault_id AND d.device_id=c.device_id WHERE c.token_digest=$1 AND c.vault_id=$2 AND c.device_id=$3 AND c.revoked_at IS NULL AND d.revoked_at IS NULL AND d.role='owner')").bind(&owner.token_digest).bind(owner.vault_id).bind(owner.device_id).fetch_one(&mut **tx).await.map_err(database)?;
    valid.then_some(()).ok_or(unauthorized("owner_required"))
}

struct CreateValues {
    claim_digest: [u8; 32],
    signing_key: [u8; 32],
    transfer_key: [u8; 32],
    credential_digest: [u8; 32],
    commitment: [u8; 32],
}
fn create_values(request: &CreateComputerEnrollmentRequest) -> Result<CreateValues, Error> {
    validate_device_name(&request.recipient.device.name).map_err(|_| bad("invalid_device_name"))?;
    if request.invitation_id.is_nil()
        || request.recipient.device.device_id.is_nil()
        || !matches!(
            request.recipient.device.kind,
            ClientKind::Browser | ClientKind::Desktop
        )
        || request.recipient.requested_role != EnrollmentRole::Device
    {
        return Err(bad("invalid_computer_recipient"));
    }
    Ok(CreateValues {
        claim_digest: decode(&request.claim_token_digest, "invalid_claim_token_digest")?,
        signing_key: decode(
            &request.recipient.device.signing_public_key,
            "invalid_signing_key",
        )?,
        transfer_key: decode(
            &request.recipient_transfer_public_key,
            "invalid_transfer_key",
        )?,
        credential_digest: hex_digest(&request.recipient.credential_digest)?,
        commitment: decode(
            &request.recipient_commitment,
            "invalid_recipient_commitment",
        )?,
    })
}
fn exact_create(
    row: &sqlx::postgres::PgRow,
    owner: &Owner,
    request: &CreateComputerEnrollmentRequest,
    value: &CreateValues,
    origin: &str,
) -> bool {
    row.get::<Uuid, _>("vault_id") == owner.vault_id
        && row.get::<Uuid, _>("owner_device_id") == owner.device_id
        && row.get::<String, _>("origin") == origin
        && row.get::<Vec<u8>, _>("claim_token_digest") == value.claim_digest
        && row.get::<Uuid, _>("recipient_device_id") == request.recipient.device.device_id
        && row.get::<String, _>("recipient_kind") == client_kind_name(request.recipient.device.kind)
        && row.get::<String, _>("recipient_name") == request.recipient.device.name
        && row.get::<Vec<u8>, _>("recipient_signing_public_key") == value.signing_key
        && row.get::<Vec<u8>, _>("recipient_transfer_public_key") == value.transfer_key
        && row.get::<Vec<u8>, _>("recipient_credential_digest") == value.credential_digest
        && row.get::<Vec<u8>, _>("recipient_commitment") == value.commitment
}
fn transcript_matches(
    row: &sqlx::postgres::PgRow,
    transcript: &ComputerEnrollmentTranscript,
    origin: &str,
) -> Result<(), Error> {
    let immutable_fields_match = row.get::<String, _>("origin") == origin
        && transcript.origin == origin
        && transcript.vault_id == row.get::<Uuid, _>("vault_id")
        && transcript.fingerprint == row.get::<String, _>("profile_fingerprint")
        && transcript.key_epoch == row.get::<i32, _>("key_epoch") as u32
        && transcript.invitation_id == row.get::<Uuid, _>("id")
        && transcript.granted_role == EnrollmentRole::Device
        && transcript.owner_device_id == row.get::<Uuid, _>("owner_device_id")
        && transcript.recipient.device.device_id == row.get::<Uuid, _>("recipient_device_id")
        && transcript.recipient.device.kind == parse_kind(row.get("recipient_kind"))?
        && transcript.recipient.device.name == row.get::<String, _>("recipient_name")
        && transcript.recipient.requested_role == EnrollmentRole::Device
        && row.get::<Vec<u8>, _>("server_nonce")
            == decode::<32>(&transcript.server_nonce, "invalid_nonce")?
        && row.get::<Vec<u8>, _>("recipient_transfer_public_key")
            == decode::<32>(
                &transcript.recipient_transfer_public_key,
                "invalid_transfer_key",
            )?
        && row.get::<Vec<u8>, _>("recipient_signing_public_key")
            == decode::<32>(
                &transcript.recipient.device.signing_public_key,
                "invalid_signing_key",
            )?
        && row.get::<Vec<u8>, _>("recipient_credential_digest")
            == hex_digest(&transcript.recipient.credential_digest)?;
    immutable_fields_match
        .then_some(())
        .ok_or(Error(StatusCode::CONFLICT, "transcript_mismatch"))
}
fn status_from_row(row: &sqlx::postgres::PgRow) -> Result<ComputerEnrollmentStatus, Error> {
    let expired = !row.get::<bool, _>("expires_valid");
    let state = if expired {
        EnrollmentState::Expired
    } else {
        parse_state(row.get("state"))?
    };
    let approved = matches!(state, EnrollmentState::Approved);
    Ok(ComputerEnrollmentStatus {
        state,
        expires_in_seconds: remaining(row),
        transcript: approved.then(|| transcript(row)).transpose()?,
        encrypted_key_package: approved
            .then(|| {
                row.get::<Option<Vec<u8>>, _>("encrypted_key_package")
                    .map(|v| b64(&v))
            })
            .flatten(),
        transfer_nonce: approved
            .then(|| {
                row.get::<Option<Vec<u8>>, _>("transfer_nonce")
                    .map(|v| b64(&v))
            })
            .flatten(),
        owner_proof: approved
            .then(|| {
                row.get::<Option<Vec<u8>>, _>("owner_proof")
                    .map(|v| b64(&v))
            })
            .flatten(),
    })
}
fn transcript(row: &sqlx::postgres::PgRow) -> Result<ComputerEnrollmentTranscript, Error> {
    Ok(ComputerEnrollmentTranscript {
        origin: row.get("origin"),
        vault_id: row.get("vault_id"),
        fingerprint: row.get("profile_fingerprint"),
        key_epoch: row.get::<i32, _>("key_epoch") as u32,
        invitation_id: row.get("id"),
        server_nonce: b64(&row.get::<Vec<u8>, _>("server_nonce")),
        owner_transfer_public_key: b64(&row.get::<Vec<u8>, _>("owner_transfer_public_key")),
        owner_device_id: row.get("owner_device_id"),
        recipient: peppy_hosted_api::computer_enrollment::ComputerEnrollmentRecipient {
            device: peppy_hosted_api::enrollment::ProposedDevice {
                device_id: row.get("recipient_device_id"),
                kind: parse_kind(row.get("recipient_kind"))?,
                name: row.get("recipient_name"),
                signing_public_key: b64(&row.get::<Vec<u8>, _>("recipient_signing_public_key")),
            },
            credential_digest: hex::encode(row.get::<Vec<u8>, _>("recipient_credential_digest")),
            requested_role: EnrollmentRole::Device,
        },
        recipient_transfer_public_key: b64(&row.get::<Vec<u8>, _>("recipient_transfer_public_key")),
        granted_role: EnrollmentRole::Device,
    })
}
fn context_from_row(row: &sqlx::postgres::PgRow) -> Result<EnrollmentContext, Error> {
    Ok(EnrollmentContext {
        purpose: EnrollmentPurpose::Computer,
        canonical_origin: row.get("origin"),
        vault_id: row.get("vault_id"),
        fingerprint: hex_digest(&row.get::<String, _>("profile_fingerprint"))?,
        key_epoch: row.get::<i32, _>("key_epoch") as u32,
        enrollment_id: row.get("id"),
        server_nonce: row
            .get::<Vec<u8>, _>("server_nonce")
            .try_into()
            .map_err(|_| unauthorized("computer_enrollment_invalid"))?,
        owner_transfer_public_key: row
            .get::<Vec<u8>, _>("owner_transfer_public_key")
            .try_into()
            .map_err(|_| unauthorized("computer_enrollment_invalid"))?,
        recipient_transfer_public_key: row
            .get::<Vec<u8>, _>("recipient_transfer_public_key")
            .try_into()
            .map_err(|_| unauthorized("computer_enrollment_invalid"))?,
        recipient_signing_public_key: row
            .get::<Vec<u8>, _>("recipient_signing_public_key")
            .try_into()
            .map_err(|_| unauthorized("computer_enrollment_invalid"))?,
        device_id: row.get("recipient_device_id"),
        credential_digest: row
            .get::<Vec<u8>, _>("recipient_credential_digest")
            .try_into()
            .map_err(|_| unauthorized("computer_enrollment_invalid"))?,
        role: CryptoRole::Device,
    })
}
fn admission(row: &sqlx::postgres::PgRow, device_id: Uuid) -> Result<AdmissionComplete, Error> {
    Ok(AdmissionComplete {
        vault_id: row.get("vault_id"),
        device_id,
        role: EnrollmentRole::Device,
        material: VaultMaterial {
            profile: row.get("public_key_profile"),
            check_header: b64(&row.get::<Vec<u8>, _>("encrypted_vault_check_header")),
            fingerprint: row.get("current_fingerprint"),
            key_epoch: row.get::<i32, _>("current_epoch") as u32,
            admission_public_key: b64(&row.get::<Vec<u8>, _>("admission_public_key")),
        },
    })
}
fn valid_waiting(row: &sqlx::postgres::PgRow) -> Result<(), Error> {
    if !row.get::<bool, _>("expires_valid") {
        return Err(Error(StatusCode::GONE, "computer_enrollment_expired"));
    }
    if row.get::<String, _>("state") != "waiting" {
        return Err(Error(
            StatusCode::CONFLICT,
            "computer_enrollment_not_waiting",
        ));
    }
    Ok(())
}
fn valid_approved(row: &sqlx::postgres::PgRow) -> Result<(), Error> {
    if !row.get::<bool, _>("expires_valid") {
        return Err(Error(StatusCode::GONE, "computer_enrollment_expired"));
    }
    if row.get::<String, _>("state") != "approved" {
        return Err(Error(
            StatusCode::CONFLICT,
            "computer_enrollment_not_approved",
        ));
    }
    Ok(())
}
fn claim_digest(value: &str) -> Result<[u8; 32], Error> {
    let token: [u8; 32] = decode(value, "invalid_claim_token")?;
    Ok(Sha256::digest(token).into())
}
fn hex_digest(value: &str) -> Result<[u8; 32], Error> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|b| b.is_ascii_digit() || matches!(b, b'a'..=b'f'))
    {
        return Err(bad("invalid_credential_digest"));
    }
    hex::decode(value)
        .ok()
        .and_then(|v| v.try_into().ok())
        .ok_or(bad("invalid_credential_digest"))
}
fn decode<const N: usize>(value: &str, code: &'static str) -> Result<[u8; N], Error> {
    URL_SAFE_NO_PAD
        .decode(value)
        .ok()
        .and_then(|v| v.try_into().ok())
        .ok_or(bad(code))
}
fn b64(value: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(value)
}
fn remaining(row: &sqlx::postgres::PgRow) -> u32 {
    row.get::<i64, _>("expires_remaining") as u32
}
fn parse_kind(value: String) -> Result<ClientKind, Error> {
    match value.as_str() {
        "browser" => Ok(ClientKind::Browser),
        "desktop" => Ok(ClientKind::Desktop),
        _ => Err(unauthorized("computer_enrollment_invalid")),
    }
}
fn parse_state(value: String) -> Result<EnrollmentState, Error> {
    match value.as_str() {
        "waiting" => Ok(EnrollmentState::Waiting),
        "approved" => Ok(EnrollmentState::Approved),
        "consumed" => Ok(EnrollmentState::Consumed),
        "cancelled" => Ok(EnrollmentState::Denied),
        "expired" => Ok(EnrollmentState::Expired),
        _ => Err(unauthorized("computer_enrollment_invalid")),
    }
}
fn canonical_origin(value: &str) -> Result<String, &'static str> {
    let url = url::Url::parse(value).map_err(|_| "configured enrollment origin is invalid")?;
    let loopback = url.host_str().is_some_and(|host| {
        host == "localhost"
            || host
                .parse::<std::net::IpAddr>()
                .is_ok_and(|ip| ip.is_loopback())
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
fn request_origin(state: &ComputerEnrollmentRouterState, headers: &HeaderMap) -> Result<(), Error> {
    if state.hosted {
        return Ok(());
    }
    if let Some(value) = headers.get("origin") {
        let value = value.to_str().map_err(|_| bad("invalid_origin"))?;
        if canonical_origin(value).map_err(bad)? != state.origin {
            return Err(Error(StatusCode::FORBIDDEN, "origin_mismatch"));
        }
    }
    Ok(())
}
fn database(_: sqlx::Error) -> Error {
    Error(StatusCode::SERVICE_UNAVAILABLE, "database_unavailable")
}
fn bad(code: &'static str) -> Error {
    Error(StatusCode::UNPROCESSABLE_ENTITY, code)
}
fn unauthorized(code: &'static str) -> Error {
    Error(StatusCode::UNAUTHORIZED, code)
}
struct Error(StatusCode, &'static str);
impl IntoResponse for Error {
    fn into_response(self) -> axum::response::Response {
        (self.0, Json(serde_json::json!({"error":self.1}))).into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, http::Request};
    use peppy_hosted_api::computer_enrollment::ComputerEnrollmentRecipient;
    use peppy_hosted_api::enrollment::ProposedDevice;
    use sqlx::postgres::PgPoolOptions;
    use tower::ServiceExt;

    fn create_request(kind: ClientKind, role: EnrollmentRole) -> CreateComputerEnrollmentRequest {
        CreateComputerEnrollmentRequest {
            invitation_id: Uuid::new_v4(),
            recipient: ComputerEnrollmentRecipient {
                device: ProposedDevice {
                    device_id: Uuid::new_v4(),
                    kind,
                    name: "Computer".into(),
                    signing_public_key: b64(&[1; 32]),
                },
                credential_digest: "11".repeat(32),
                requested_role: role,
            },
            recipient_transfer_public_key: b64(&[2; 32]),
            recipient_commitment: b64(&[3; 32]),
            claim_token_digest: b64(&[4; 32]),
        }
    }

    #[test]
    fn create_rejects_phone_kinds_and_non_device_roles() {
        assert!(
            create_values(&create_request(ClientKind::Android, EnrollmentRole::Device)).is_err()
        );
        assert!(
            create_values(&create_request(ClientKind::Desktop, EnrollmentRole::Owner)).is_err()
        );
        assert!(
            create_values(&create_request(ClientKind::Browser, EnrollmentRole::Device)).is_ok()
        );
    }

    #[tokio::test]
    async fn create_never_reaches_an_anonymous_database_path() {
        let db = PgPoolOptions::new()
            .connect_lazy("postgres://postgres:postgres@127.0.0.1:1/postgres")
            .unwrap();
        let app = router(
            ComputerEnrollmentRouterState::new(
                db.clone(),
                Arc::new(crate::api::CommunityEnrollmentAuthorization::new(db)),
                false,
                "https://peppy.test".into(),
            )
            .unwrap(),
        );
        let response = app
            .oneshot(
                Request::post("/v1/enrollment/computers")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&create_request(
                            ClientKind::Desktop,
                            EnrollmentRole::Device,
                        ))
                        .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    const DAY: f64 = 86_400.0;

    async fn cleanup_pool() -> PgPool {
        let database_url =
            std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL is required");
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .unwrap();
        let schema = format!("peppy_cleanup_test_{}", Uuid::new_v4().simple());
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
        crate::MIGRATOR.run(&pool).await.unwrap();
        pool
    }

    async fn seed_owner(pool: &PgPool) -> (Uuid, Uuid) {
        let vault = Uuid::new_v4();
        let owner = Uuid::new_v4();
        sqlx::query("INSERT INTO vaults(vault_id,public_key_profile,encrypted_vault_check_header,key_epoch,profile_fingerprint) VALUES($1,$2,$3,1,$4)")
            .bind(vault)
            .bind(serde_json::json!({"vault_id": vault, "key_epoch": 1}))
            .bind(vec![1u8])
            .bind("11".repeat(32))
            .execute(pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO devices(vault_id,device_id,role,public_key,profile_fingerprint,key_epoch,name,kind) VALUES($1,$2,'owner','{}',$3,1,'owner','desktop')")
            .bind(vault)
            .bind(owner)
            .bind("11".repeat(32))
            .execute(pool)
            .await
            .unwrap();
        (vault, owner)
    }

    /// Offsets are seconds from now: negative `expires_in` means already expired,
    /// and `created_ago` is how long ago the row was created.
    async fn insert_enrollment(
        pool: &PgPool,
        (vault, owner): (Uuid, Uuid),
        state: &str,
        expires_in: f64,
        created_ago: f64,
    ) -> Uuid {
        let id = Uuid::new_v4();
        sqlx::query("INSERT INTO computer_enrollments(id,vault_id,owner_device_id,origin,claim_token_digest,server_nonce,recipient_device_id,recipient_kind,recipient_name,recipient_signing_public_key,recipient_transfer_public_key,recipient_credential_digest,recipient_commitment,profile_fingerprint,key_epoch,state,expires_at,created_at) VALUES($1,$2,$3,$4,$5,$6,$7,'desktop','Computer',$8,$9,$10,$11,$12,1,$13,now()+make_interval(secs => $14::float8),now()-make_interval(secs => $15::float8))")
            .bind(id)
            .bind(vault)
            .bind(owner)
            .bind("https://peppy.test")
            .bind(Sha256::digest(id.as_bytes()).to_vec())
            .bind(vec![5u8; 32])
            .bind(Uuid::new_v4())
            .bind(vec![6u8; 32])
            .bind(vec![7u8; 32])
            .bind(vec![8u8; 32])
            .bind(vec![9u8; 32])
            .bind("11".repeat(32))
            .bind(state)
            .bind(expires_in)
            .bind(created_ago)
            .execute(pool)
            .await
            .unwrap();
        id
    }

    async fn exists(pool: &PgPool, id: Uuid) -> bool {
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM computer_enrollments WHERE id=$1)")
            .bind(id)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    #[tokio::test]
    #[ignore = "requires disposable PostgreSQL via TEST_DATABASE_URL; run with --include-ignored"]
    async fn cleanup_removes_retired_rows_past_grace_and_keeps_live_or_recent_approvals() {
        let pool = cleanup_pool().await;
        let seed = seed_owner(&pool).await;
        let old_approved =
            insert_enrollment(&pool, seed, "approved", -31.0 * DAY, 31.0 * DAY + 300.0).await;
        let old_waiting =
            insert_enrollment(&pool, seed, "waiting", -31.0 * DAY, 31.0 * DAY + 300.0).await;
        let old_consumed = insert_enrollment(&pool, seed, "consumed", -3_600.0, 31.0 * DAY).await;
        let live_approved = insert_enrollment(&pool, seed, "approved", 300.0, 0.0).await;
        let recent_expired_approved =
            insert_enrollment(&pool, seed, "approved", -3_600.0, 3_900.0).await;
        let recent_consumed = insert_enrollment(&pool, seed, "consumed", -3_600.0, 3_900.0).await;

        let deleted = cleanup_expired_enrollments(&pool).await.unwrap();

        assert_eq!(deleted, 3);
        assert!(!exists(&pool, old_approved).await);
        assert!(!exists(&pool, old_waiting).await);
        assert!(!exists(&pool, old_consumed).await);
        assert!(exists(&pool, live_approved).await);
        assert!(exists(&pool, recent_expired_approved).await);
        assert!(exists(&pool, recent_consumed).await);
    }

    #[tokio::test]
    #[ignore = "requires disposable PostgreSQL via TEST_DATABASE_URL; run with --include-ignored"]
    async fn cleanup_deletes_at_most_one_batch_per_call() {
        let pool = cleanup_pool().await;
        let seed = seed_owner(&pool).await;
        for _ in 0..5 {
            insert_enrollment(&pool, seed, "approved", -31.0 * DAY, 31.0 * DAY + 300.0).await;
        }

        assert_eq!(cleanup_batch(&pool, 2).await.unwrap(), 2);
        let remaining: i64 = sqlx::query_scalar("SELECT count(*) FROM computer_enrollments")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(remaining, 3);
        assert_eq!(cleanup_batch(&pool, 2).await.unwrap(), 2);
        assert_eq!(cleanup_batch(&pool, 2).await.unwrap(), 1);
        assert_eq!(cleanup_batch(&pool, 2).await.unwrap(), 0);
    }
}
