use axum::{
    Json,
    extract::{State, rejection::JsonRejection},
    http::{HeaderMap, StatusCode},
};
use peppy_crypto::vault_identity::VaultIdentityRecoveryRecord;
use peppy_protocol::{
    EstablishVaultIdentityV1, VAULT_IDENTITY_PROTOCOL_VERSION, VaultIdentityRecoveryRecordV1,
    VaultIdentityStatusV1,
};
use sqlx::Row;

use super::{
    ApiError, ApiResult, ApiState, Operation, auth_with_mode, authorize, database_unavailable,
    owner, revalidate_principal,
};

pub(super) async fn get(
    State(state): State<ApiState>,
    headers: HeaderMap,
) -> ApiResult<Json<VaultIdentityStatusV1>> {
    let principal = auth_with_mode(&state, &headers).await?;
    authorize(&state, &principal, Operation::Read).await?;
    let mut tx = crate::scope::begin(&state.db, principal.vault)
        .await
        .map_err(|error| database_unavailable(&error, "vault_identity_get_begin"))?;
    let row = sqlx::query("SELECT operation_id,key_epoch,profile_fingerprint,recovery_record FROM vault_identity_establishments WHERE vault_id=$1")
        .bind(principal.vault)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|error| database_unavailable(&error, "vault_identity_get"))?;
    tx.commit()
        .await
        .map_err(|error| database_unavailable(&error, "vault_identity_get_commit"))?;
    Ok(Json(match row {
        None => VaultIdentityStatusV1::Absent {
            protocol_version: VAULT_IDENTITY_PROTOCOL_VERSION,
        },
        Some(row) => established_status(
            row.get("operation_id"),
            row.get::<i32, _>("key_epoch") as u32,
            row.get("profile_fingerprint"),
            row.get("recovery_record"),
        )?,
    }))
}

pub(super) async fn establish(
    State(state): State<ApiState>,
    headers: HeaderMap,
    request: Result<Json<EstablishVaultIdentityV1>, JsonRejection>,
) -> ApiResult<(StatusCode, Json<VaultIdentityStatusV1>)> {
    let Json(request) = request
        .map_err(|_| ApiError(StatusCode::UNPROCESSABLE_ENTITY, "invalid_vault_identity"))?;
    let principal = auth_with_mode(&state, &headers).await?;
    authorize(&state, &principal, Operation::Publish).await?;
    owner(&principal)?;
    request
        .validate()
        .map_err(|_| ApiError(StatusCode::UNPROCESSABLE_ENTITY, "invalid_vault_identity"))?;
    let recovery = VaultIdentityRecoveryRecord::decode(request.recovery_record.as_bytes())
        .map_err(|_| ApiError(StatusCode::UNPROCESSABLE_ENTITY, "invalid_vault_identity"))?;
    let profile = recovery.profile();
    let recovery_fingerprint = profile
        .fingerprint()
        .map_err(|_| ApiError(StatusCode::UNPROCESSABLE_ENTITY, "invalid_vault_identity"))?;
    if profile.vault_id != principal.vault
        || profile.key_epoch != request.key_epoch
        || recovery_fingerprint != request.profile_fingerprint
    {
        return Err(ApiError(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_vault_identity",
        ));
    }

    let mut tx = crate::scope::begin(&state.db, principal.vault)
        .await
        .map_err(|error| database_unavailable(&error, "vault_identity_establish_begin"))?;
    let current = sqlx::query(
        "SELECT key_epoch,profile_fingerprint FROM vaults WHERE vault_id=$1 FOR UPDATE",
    )
    .bind(principal.vault)
    .fetch_one(&mut *tx)
    .await
    .map_err(|error| database_unavailable(&error, "vault_identity_establish_lock"))?;
    if revalidate_principal(&mut tx, &principal).await? != "owner" {
        return Err(ApiError(StatusCode::FORBIDDEN, "owner_required"));
    }
    if let Some(existing) = sqlx::query("SELECT operation_id,key_epoch,profile_fingerprint,recovery_record FROM vault_identity_establishments WHERE vault_id=$1")
        .bind(principal.vault)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|error| database_unavailable(&error, "vault_identity_establish_lookup"))? {
        let status = established_status(existing.get("operation_id"), existing.get::<i32, _>("key_epoch") as u32, existing.get("profile_fingerprint"), existing.get("recovery_record"))?;
        let identical = matches!(&status, VaultIdentityStatusV1::Established { operation_id, key_epoch, profile_fingerprint, recovery_record, .. } if *operation_id == request.operation_id && *key_epoch == request.key_epoch && profile_fingerprint == &request.profile_fingerprint && recovery_record.as_bytes() == request.recovery_record.as_bytes());
        if !identical { return Err(ApiError(StatusCode::CONFLICT, "vault_identity_conflict")); }
        tx.commit().await.map_err(|error| database_unavailable(&error, "vault_identity_establish_duplicate_commit"))?;
        return Ok((StatusCode::OK, Json(status)));
    }
    if current.get::<i32, _>("key_epoch") as u32 != request.key_epoch
        || current.get::<String, _>("profile_fingerprint") != request.profile_fingerprint
    {
        return Err(ApiError(
            StatusCode::CONFLICT,
            "vault_identity_stale_context",
        ));
    }
    sqlx::query("INSERT INTO vault_identity_establishments(vault_id,operation_id,key_epoch,profile_fingerprint,recovery_record,established_by_device_id) VALUES($1,$2,$3,$4,$5,$6)")
        .bind(principal.vault).bind(request.operation_id).bind(request.key_epoch as i32).bind(&request.profile_fingerprint).bind(request.recovery_record.as_bytes()).bind(principal.device)
        .execute(&mut *tx).await.map_err(|error| database_unavailable(&error, "vault_identity_establish_insert"))?;
    tx.commit()
        .await
        .map_err(|error| database_unavailable(&error, "vault_identity_establish_commit"))?;
    Ok((
        StatusCode::CREATED,
        Json(VaultIdentityStatusV1::Established {
            protocol_version: VAULT_IDENTITY_PROTOCOL_VERSION,
            operation_id: request.operation_id,
            key_epoch: request.key_epoch,
            profile_fingerprint: request.profile_fingerprint,
            recovery_record: request.recovery_record,
        }),
    ))
}

fn established_status(
    operation_id: uuid::Uuid,
    key_epoch: u32,
    profile_fingerprint: String,
    recovery_record: Vec<u8>,
) -> ApiResult<VaultIdentityStatusV1> {
    let recovery_record = VaultIdentityRecoveryRecordV1::from_bytes(recovery_record)
        .map_err(|_| ApiError(StatusCode::INTERNAL_SERVER_ERROR, "vault_identity_corrupt"))?;
    Ok(VaultIdentityStatusV1::Established {
        protocol_version: VAULT_IDENTITY_PROTOCOL_VERSION,
        operation_id,
        key_epoch,
        profile_fingerprint,
        recovery_record,
    })
}
