//! Immutable per-epoch public key profiles and explicit forward activation.
//!
//! Rows hold only public KDF metadata and the encrypted vault check header.
//! The server never receives a passphrase or key; clients derive and verify
//! locally. Historical rows stay readable for manual old-passphrase recovery.

use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
};
use base64::{
    Engine as _,
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::Row;

use super::{
    ApiError, ApiResult, ApiState, Operation, auth_with_mode, authorize, database_unavailable,
    owner, validate_profile,
};

const MAX_CHECK_HEADER_BYTES: usize = 1_048_576;
const ADMISSION_PUBLIC_KEY_BYTES: usize = 32;

/// `GET /v1/vault/key-profiles` — every registered epoch, oldest first.
pub(super) async fn list(State(s): State<ApiState>, h: HeaderMap) -> ApiResult<Json<Value>> {
    let p = auth_with_mode(&s, &h).await?;
    authorize(&s, &p, Operation::Read).await?;
    let mut tx = crate::scope::begin(&s.db, p.vault)
        .await
        .map_err(|error| database_unavailable(&error, "key_profiles_list_begin"))?;
    let rows = sqlx::query("SELECT k.key_epoch,k.public_key_profile,k.encrypted_vault_check_header,k.profile_fingerprint,k.activated_at IS NOT NULL AS activated,k.key_epoch=v.key_epoch AS current FROM vault_key_profiles k JOIN vaults v ON v.vault_id=k.vault_id WHERE k.vault_id=$1 ORDER BY k.key_epoch")
        .bind(p.vault)
        .fetch_all(&mut *tx)
        .await
        .map_err(|error| database_unavailable(&error, "key_profiles_list"))?;
    tx.commit()
        .await
        .map_err(|error| database_unavailable(&error, "key_profiles_list_commit"))?;
    Ok(Json(json!({
        "key_profiles": rows.into_iter().map(|row| json!({
            "key_epoch": row.get::<i32, _>("key_epoch"),
            "public_key_profile": row.get::<Value, _>("public_key_profile"),
            "encrypted_vault_check_header": STANDARD.encode(row.get::<Vec<u8>, _>("encrypted_vault_check_header")),
            "profile_fingerprint": row.get::<String, _>("profile_fingerprint"),
            "activated": row.get::<bool, _>("activated"),
            "current": row.get::<bool, _>("current"),
        })).collect::<Vec<_>>()
    })))
}

#[derive(Deserialize)]
pub(super) struct RegisterProfile {
    key_epoch: u32,
    public_key_profile: Value,
    /// Standard base64 of the encrypted vault check header for this epoch.
    encrypted_vault_check_header: String,
    profile_fingerprint: String,
    /// Base64url-no-padding Ed25519 verifier derived by the client for this epoch.
    admission_public_key: String,
}

/// `POST /v1/vault/key-profiles` (owner) — registers a future epoch without activating it.
pub(super) async fn register(
    State(s): State<ApiState>,
    h: HeaderMap,
    Json(x): Json<RegisterProfile>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let p = auth_with_mode(&s, &h).await?;
    authorize(&s, &p, Operation::Publish).await?;
    owner(&p)?;
    let invalid = ApiError(StatusCode::UNPROCESSABLE_ENTITY, "invalid_key_profile");
    let vault = validate_profile(&x.public_key_profile, &x.profile_fingerprint, x.key_epoch)
        .map_err(|_| ApiError(StatusCode::UNPROCESSABLE_ENTITY, "invalid_key_profile"))?;
    let header = STANDARD
        .decode(&x.encrypted_vault_check_header)
        .map_err(|_| ApiError(StatusCode::UNPROCESSABLE_ENTITY, "invalid_key_profile"))?;
    let admission_public_key = URL_SAFE_NO_PAD
        .decode(&x.admission_public_key)
        .map_err(|_| ApiError(StatusCode::UNPROCESSABLE_ENTITY, "invalid_key_profile"))?;
    if vault != p.vault
        || header.is_empty()
        || header.len() > MAX_CHECK_HEADER_BYTES
        || admission_public_key.len() != ADMISSION_PUBLIC_KEY_BYTES
        || i32::try_from(x.key_epoch).is_err()
    {
        return Err(invalid);
    }
    let epoch = x.key_epoch as i32;
    let mut tx = crate::scope::begin(&s.db, p.vault)
        .await
        .map_err(|error| database_unavailable(&error, "key_profile_begin"))?;
    let current: i32 =
        sqlx::query_scalar("SELECT key_epoch FROM vaults WHERE vault_id=$1 FOR UPDATE")
            .bind(p.vault)
            .fetch_one(&mut *tx)
            .await
            .map_err(|error| database_unavailable(&error, "key_profile_lock"))?;
    if let Some(existing) = sqlx::query("SELECT p.public_key_profile,p.encrypted_vault_check_header,p.profile_fingerprint,p.activated_at IS NOT NULL AS activated,a.profile_fingerprint AS admission_profile_fingerprint,a.admission_public_key FROM vault_key_profiles p LEFT JOIN vault_admission_keys a ON a.vault_id=p.vault_id AND a.key_epoch=p.key_epoch WHERE p.vault_id=$1 AND p.key_epoch=$2")
        .bind(p.vault)
        .bind(epoch)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|error| database_unavailable(&error, "key_profile_lookup"))?
    {
        let same = existing.get::<Value, _>("public_key_profile") == x.public_key_profile
            && existing.get::<Vec<u8>, _>("encrypted_vault_check_header") == header
            && existing.get::<String, _>("profile_fingerprint") == x.profile_fingerprint
            && existing
                .try_get::<String, _>("admission_profile_fingerprint")
                .is_ok_and(|fingerprint| fingerprint == x.profile_fingerprint)
            && existing
                .try_get::<Vec<u8>, _>("admission_public_key")
                .is_ok_and(|key| key == admission_public_key);
        if !same {
            return Err(ApiError(StatusCode::CONFLICT, "key_profile_conflict"));
        }
        return Ok((
            StatusCode::OK,
            Json(json!({"key_epoch": epoch, "profile_fingerprint": x.profile_fingerprint, "activated": existing.get::<bool, _>("activated"), "duplicate": true})),
        ));
    }
    if epoch <= current {
        return Err(ApiError(StatusCode::CONFLICT, "key_epoch_not_forward"));
    }
    sqlx::query("INSERT INTO vault_key_profiles(vault_id,key_epoch,public_key_profile,encrypted_vault_check_header,profile_fingerprint,created_by_device_id) VALUES($1,$2,$3,$4,$5,$6)")
        .bind(p.vault)
        .bind(epoch)
        .bind(&x.public_key_profile)
        .bind(&header)
        .bind(&x.profile_fingerprint)
        .bind(p.device)
        .execute(&mut *tx)
        .await
        .map_err(|error| {
            if error.as_database_error().and_then(|e| e.code()).is_some_and(|code| code == "23505") {
                ApiError(StatusCode::CONFLICT, "key_profile_conflict")
            } else {
                database_unavailable(&error, "key_profile_insert")
            }
        })?;
    sqlx::query("INSERT INTO vault_admission_keys(vault_id,key_epoch,profile_fingerprint,admission_public_key) VALUES($1,$2,$3,$4)")
        .bind(p.vault)
        .bind(epoch)
        .bind(&x.profile_fingerprint)
        .bind(&admission_public_key)
        .execute(&mut *tx)
        .await
        .map_err(|error| {
            if error.as_database_error().and_then(|e| e.code()).is_some_and(|code| code == "23505") {
                ApiError(StatusCode::CONFLICT, "key_profile_conflict")
            } else {
                database_unavailable(&error, "key_profile_admission_key_insert")
            }
        })?;
    tx.commit()
        .await
        .map_err(|error| database_unavailable(&error, "key_profile_commit"))?;
    Ok((
        StatusCode::CREATED,
        Json(
            json!({"key_epoch": epoch, "profile_fingerprint": x.profile_fingerprint, "activated": false, "duplicate": false}),
        ),
    ))
}

/// `POST /v1/vault/key-profiles/{key_epoch}/activate` (owner) — forward-only cutover.
///
/// Serialized on the vault row with ingest, so a command is checked against
/// exactly one current epoch. Already accepted commands keep their cursor on
/// identical retry; new commands at a retired epoch fail.
pub(super) async fn activate(
    State(s): State<ApiState>,
    h: HeaderMap,
    Path(key_epoch): Path<i32>,
) -> ApiResult<Json<Value>> {
    let p = auth_with_mode(&s, &h).await?;
    authorize(&s, &p, Operation::Publish).await?;
    owner(&p)?;
    let mut tx = crate::scope::begin(&s.db, p.vault)
        .await
        .map_err(|error| database_unavailable(&error, "key_activate_begin"))?;
    let current: i32 =
        sqlx::query_scalar("SELECT key_epoch FROM vaults WHERE vault_id=$1 FOR UPDATE")
            .bind(p.vault)
            .fetch_one(&mut *tx)
            .await
            .map_err(|error| database_unavailable(&error, "key_activate_lock"))?;
    let profile = sqlx::query("SELECT p.public_key_profile,p.encrypted_vault_check_header,p.profile_fingerprint,a.admission_public_key FROM vault_key_profiles p LEFT JOIN vault_admission_keys a ON a.vault_id=p.vault_id AND a.key_epoch=p.key_epoch AND a.profile_fingerprint=p.profile_fingerprint WHERE p.vault_id=$1 AND p.key_epoch=$2")
        .bind(p.vault)
        .bind(key_epoch)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|error| database_unavailable(&error, "key_activate_lookup"))?
        .ok_or(ApiError(StatusCode::NOT_FOUND, "key_profile_not_found"))?;
    let fingerprint: String = profile.get("profile_fingerprint");
    if !profile
        .try_get::<Vec<u8>, _>("admission_public_key")
        .is_ok_and(|key| key.len() == ADMISSION_PUBLIC_KEY_BYTES)
    {
        return Err(ApiError(
            StatusCode::CONFLICT,
            "key_profile_admission_key_missing",
        ));
    }
    if key_epoch == current {
        return Ok(Json(
            json!({"key_epoch": key_epoch, "profile_fingerprint": fingerprint, "duplicate": true}),
        ));
    }
    if key_epoch < current {
        return Err(ApiError(StatusCode::CONFLICT, "key_epoch_not_forward"));
    }
    sqlx::query("UPDATE vaults SET key_epoch=$2,public_key_profile=$3,encrypted_vault_check_header=$4,profile_fingerprint=$5 WHERE vault_id=$1")
        .bind(p.vault)
        .bind(key_epoch)
        .bind(profile.get::<Value, _>("public_key_profile"))
        .bind(profile.get::<Vec<u8>, _>("encrypted_vault_check_header"))
        .bind(&fingerprint)
        .execute(&mut *tx)
        .await
        .map_err(|error| database_unavailable(&error, "key_activate_vault"))?;
    sqlx::query("UPDATE vault_key_profiles SET activated_at=now() WHERE vault_id=$1 AND key_epoch=$2 AND activated_at IS NULL")
        .bind(p.vault)
        .bind(key_epoch)
        .execute(&mut *tx)
        .await
        .map_err(|error| database_unavailable(&error, "key_activate_mark"))?;
    tx.commit()
        .await
        .map_err(|error| database_unavailable(&error, "key_activate_commit"))?;
    Ok(Json(
        json!({"key_epoch": key_epoch, "profile_fingerprint": fingerprint, "duplicate": false}),
    ))
}
