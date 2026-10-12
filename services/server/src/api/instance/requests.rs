use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::Response,
    routing::{get, post},
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use peppy_hosted_api::enrollment::{
    CreateVaultRequest, SubmitVaultRequest, VaultRequestCreated, VaultRequestState,
    VaultRequestStatus, VaultRequestSummary, VaultSummary,
};
use serde::Deserialize;
use sqlx::Row;
use uuid::Uuid;

use super::{
    ApiError, ApiResult, ApiState, create_instance_session, instance_claim_response,
    normalize_username, notify_instance_change, request_digest, require_admin,
};

pub(super) fn routes() -> Router<ApiState> {
    Router::new()
        .route("/v1/instance/vaults", get(vaults).post(create_vault))
        .route("/v1/instance/vault-requests", post(submit).get(list))
        .route("/v1/instance/vault-requests/{request_id}", get(status))
        .route(
            "/v1/instance/vault-requests/{request_id}/approve",
            post(approve),
        )
        .route("/v1/instance/vault-requests/{request_id}/deny", post(deny))
}

fn request_state(value: &str, expired: bool) -> VaultRequestState {
    if expired && matches!(value, "pending" | "approved") {
        VaultRequestState::Expired
    } else {
        match value {
            "pending" => VaultRequestState::Pending,
            "approved" => VaultRequestState::Approved,
            "denied" => VaultRequestState::Denied,
            "completed" => VaultRequestState::Completed,
            _ => VaultRequestState::Expired,
        }
    }
}

fn digest_header(headers: &HeaderMap) -> Option<[u8; 32]> {
    headers
        .get(peppy_hosted_api::enrollment::VAULT_REQUEST_HEADER)?
        .to_str()
        .ok()
        .map(|secret| crate::instance::digest(secret.as_bytes()))
}

fn digest_value(value: &str) -> Result<Vec<u8>, ApiError> {
    hex::decode(value)
        .ok()
        .filter(|digest| digest.len() == 32)
        .ok_or(ApiError(StatusCode::BAD_REQUEST, "invalid_request"))
}

async fn submit(
    State(state): State<ApiState>,
    super::super::PeerAddress(peer): super::super::PeerAddress,
    headers: HeaderMap,
    Json(request): Json<SubmitVaultRequest>,
) -> ApiResult<Json<VaultRequestCreated>> {
    if matches!(
        request.client,
        peppy_hosted_api::enrollment::ClientKind::Browser
    ) && !super::super::instance_origin_matches(&state, &headers)
    {
        return Err(ApiError(StatusCode::FORBIDDEN, "invalid_origin"));
    }
    super::super::instance_account_admission(
        &state,
        peer,
        b"vault-request",
        "vault_request_limited",
    )
    .await?;
    if request.display_name.trim().is_empty() || request.display_name.len() > 64 {
        return Err(ApiError(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_display_name",
        ));
    }
    let secret_digest = digest_value(&request.request_secret_digest)?;
    let mut tx = state
        .db
        .begin()
        .await
        .map_err(|error| super::super::database_unavailable(&error, "vault_request_begin"))?;
    let invite = sqlx::query("SELECT id, state, expires_at > now() active, GREATEST(0, EXTRACT(EPOCH FROM expires_at-now()))::bigint seconds FROM vault_invites WHERE token_digest=$1 FOR UPDATE")
        .bind(crate::instance::digest(request.invite_token.as_bytes()).as_slice()).fetch_optional(&mut *tx).await.map_err(|error| super::super::database_unavailable(&error, "vault_request_invite"))?;
    let Some(invite) = invite else {
        return Err(ApiError(StatusCode::NOT_FOUND, "invite_invalid"));
    };
    let invite_id: Uuid = invite.get("id");
    if invite.get::<String, _>("state") == "used" {
        let replay = sqlx::query("SELECT id, secret_digest, GREATEST(0, EXTRACT(EPOCH FROM expires_at-now()))::bigint seconds FROM vault_requests WHERE invite_id=$1 FOR UPDATE").bind(invite_id).fetch_optional(&mut *tx).await.map_err(|error| super::super::database_unavailable(&error, "vault_request_replay"))?;
        if let Some(replay) = replay.filter(|row| {
            row.get::<Uuid, _>("id") == request.request_id
                && row.get::<Vec<u8>, _>("secret_digest") == secret_digest
        }) {
            let seconds: i64 = replay.get("seconds");
            tx.commit().await.map_err(|error| {
                super::super::database_unavailable(&error, "vault_request_replay_commit")
            })?;
            return Ok(Json(VaultRequestCreated {
                request_id: request.request_id,
                expires_in_seconds: seconds as u32,
            }));
        }
        return Err(ApiError(StatusCode::NOT_FOUND, "invite_invalid"));
    }
    if invite.get::<String, _>("state") != "active" || !invite.get::<bool, _>("active") {
        return Err(ApiError(StatusCode::NOT_FOUND, "invite_invalid"));
    }
    sqlx::query("UPDATE vault_invites SET state='used' WHERE id=$1")
        .bind(invite_id)
        .execute(&mut *tx)
        .await
        .map_err(|error| super::super::database_unavailable(&error, "vault_request_use_invite"))?;
    sqlx::query("INSERT INTO vault_requests(id, invite_id, display_name, client_kind, secret_digest, state, expires_at) SELECT $1,$2,$3,$4,$5,'pending',expires_at FROM vault_invites WHERE id=$2")
        .bind(request.request_id).bind(invite_id).bind(request.display_name.trim()).bind(match request.client { peppy_hosted_api::enrollment::ClientKind::Browser => "browser", peppy_hosted_api::enrollment::ClientKind::Desktop => "desktop", peppy_hosted_api::enrollment::ClientKind::Android => "android", peppy_hosted_api::enrollment::ClientKind::Ios => "ios" }).bind(secret_digest)
        .execute(&mut *tx).await.map_err(|error| super::super::unique_conflict(error, "invite_invalid", "vault_request_insert"))?;
    let seconds: i64 = invite.get("seconds");
    tx.commit()
        .await
        .map_err(|error| super::super::database_unavailable(&error, "vault_request_commit"))?;
    notify_instance_change(&state);
    Ok(Json(VaultRequestCreated {
        request_id: request.request_id,
        expires_in_seconds: seconds as u32,
    }))
}

async fn list(
    State(state): State<ApiState>,
    headers: HeaderMap,
) -> ApiResult<Json<Vec<VaultRequestSummary>>> {
    require_admin(&state, &headers, false).await?;
    Ok(Json(request_summaries(&state).await?))
}

pub(super) async fn request_summaries(state: &ApiState) -> ApiResult<Vec<VaultRequestSummary>> {
    let rows = sqlx::query("SELECT id, display_name, client_kind, state, expires_at < now() expired, GREATEST(0, EXTRACT(EPOCH FROM expires_at-now()))::bigint seconds, to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"') created_at FROM vault_requests WHERE (state IN ('pending','approved') AND expires_at >= now()) OR created_at > now() - interval '24 hours' ORDER BY created_at DESC")
        .fetch_all(&state.db).await.map_err(|error| super::super::database_unavailable(&error, "vault_request_list"))?;
    rows.into_iter()
        .map(|row| {
            Ok(VaultRequestSummary {
                request_id: row.get("id"),
                display_name: row.get("display_name"),
                client: match row.get::<String, _>("client_kind").as_str() {
                    "browser" => peppy_hosted_api::enrollment::ClientKind::Browser,
                    "desktop" => peppy_hosted_api::enrollment::ClientKind::Desktop,
                    "android" => peppy_hosted_api::enrollment::ClientKind::Android,
                    _ => peppy_hosted_api::enrollment::ClientKind::Ios,
                },
                state: request_state(&row.get::<String, _>("state"), row.get("expired")),
                expires_in_seconds: row.get::<i64, _>("seconds") as u32,
                created_at: row.get("created_at"),
            })
        })
        .collect()
}

async fn approve(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Path(request_id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    decide(&state, &headers, request_id, true).await
}
async fn deny(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Path(request_id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    decide(&state, &headers, request_id, false).await
}
async fn decide(
    state: &ApiState,
    headers: &HeaderMap,
    request_id: Uuid,
    approving: bool,
) -> ApiResult<StatusCode> {
    require_admin(state, headers, true).await?;
    let mut tx = state.db.begin().await.map_err(|error| {
        super::super::database_unavailable(&error, "vault_request_decide_begin")
    })?;
    let request = sqlx::query("SELECT requests.state, requests.expires_at > clock_timestamp() active FROM vault_requests requests JOIN vault_invites invites ON invites.id=requests.invite_id WHERE requests.id=$1 FOR UPDATE OF invites, requests")
        .bind(request_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|error| super::super::database_unavailable(&error, "vault_request_decide_lock"))?;
    let Some(request) = request else {
        return Err(ApiError(StatusCode::CONFLICT, "request_not_pending"));
    };
    let pending = request.get::<String, _>("state") == "pending";
    if (approving && (!pending || !request.get::<bool, _>("active")))
        || (!approving
            && !matches!(
                request.get::<String, _>("state").as_str(),
                "pending" | "approved"
            ))
    {
        return Err(ApiError(StatusCode::CONFLICT, "request_not_pending"));
    }
    let state_value = if approving { "approved" } else { "denied" };
    let result = if approving {
        sqlx::query("UPDATE vault_requests SET state=$2, expires_at=clock_timestamp()+interval '30 minutes', decided_at=clock_timestamp() WHERE id=$1")
    } else {
        sqlx::query("UPDATE vault_requests SET state=$2, decided_at=clock_timestamp() WHERE id=$1")
    }
    .bind(request_id)
    .bind(state_value)
    .execute(&mut *tx)
    .await
    .map_err(|error| super::super::database_unavailable(&error, "vault_request_decide"))?;
    debug_assert_eq!(result.rows_affected(), 1);
    tx.commit().await.map_err(|error| {
        super::super::database_unavailable(&error, "vault_request_decide_commit")
    })?;
    notify_instance_change(state);
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct Wait {
    wait: Option<u64>,
}
async fn status(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Path(request_id): Path<Uuid>,
    Query(wait): Query<Wait>,
) -> ApiResult<Json<VaultRequestStatus>> {
    let digest =
        digest_header(&headers).ok_or(ApiError(StatusCode::NOT_FOUND, "request_not_found"))?;
    let wait = wait.wait.unwrap_or(0).min(15);
    let known = headers
        .get(peppy_hosted_api::enrollment::KNOWN_STATE_HEADER)
        .and_then(|value| value.to_str().ok());
    let mut events = state.instance_events.subscribe();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(wait);
    loop {
        let row = sqlx::query("SELECT state, expires_at < now() expired, GREATEST(0, EXTRACT(EPOCH FROM expires_at-now()))::bigint seconds FROM vault_requests WHERE id=$1 AND secret_digest=$2").bind(request_id).bind(digest.as_slice()).fetch_optional(&state.db).await.map_err(|error| super::super::database_unavailable(&error, "vault_request_status"))?.ok_or(ApiError(StatusCode::NOT_FOUND, "request_not_found"))?;
        let state_value = request_state(&row.get::<String, _>("state"), row.get("expired"));
        let name = match state_value {
            VaultRequestState::Pending => "pending",
            VaultRequestState::Approved => "approved",
            VaultRequestState::Denied => "denied",
            VaultRequestState::Completed => "completed",
            VaultRequestState::Expired => "expired",
        };
        if known != Some(name) || wait == 0 {
            return Ok(Json(VaultRequestStatus {
                request_id,
                state: state_value,
                expires_in_seconds: row.get::<i64, _>("seconds") as u32,
            }));
        }
        if tokio::time::Instant::now() >= deadline {
            return Ok(Json(VaultRequestStatus {
                request_id,
                state: state_value,
                expires_in_seconds: row.get::<i64, _>("seconds") as u32,
            }));
        }
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        let refresh = remaining.min(std::time::Duration::from_secs(5));
        tokio::select! {
            _ = tokio::time::sleep(refresh) => {},
            _ = events.recv() => {},
        }
    }
}

async fn vaults(
    State(state): State<ApiState>,
    headers: HeaderMap,
) -> ApiResult<Json<Vec<VaultSummary>>> {
    require_admin(&state, &headers, false).await?;
    let rows = sqlx::query("SELECT owners.vault_id, owners.username, owners.is_admin, to_char(owners.created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"') created_at, (SELECT count(*) FROM devices WHERE vault_id=owners.vault_id)::bigint device_count, (SELECT COALESCE(sum(ciphertext_bytes),0)::bigint FROM attachments WHERE vault_id=owners.vault_id) storage_bytes FROM vault_owners owners ORDER BY owners.created_at")
        .fetch_all(&state.db).await.map_err(|error| super::super::database_unavailable(&error, "vault_list"))?;
    Ok(Json(
        rows.into_iter()
            .map(|row| VaultSummary {
                vault_id: row.get("vault_id"),
                username: row.get("username"),
                is_admin: row.get("is_admin"),
                device_count: row.get::<i64, _>("device_count") as u64,
                storage_bytes: row.get::<i64, _>("storage_bytes").max(0) as u64,
                created_at: row.get("created_at"),
            })
            .collect(),
    ))
}

async fn create_vault(
    State(state): State<ApiState>,
    super::super::PeerAddress(peer): super::super::PeerAddress,
    headers: HeaderMap,
    Json(request): Json<CreateVaultRequest>,
) -> ApiResult<Response> {
    if matches!(
        request.device.kind,
        peppy_hosted_api::enrollment::ClientKind::Browser
    ) && !super::super::instance_origin_matches(&state, &headers)
    {
        return Err(ApiError(StatusCode::FORBIDDEN, "invalid_origin"));
    }
    super::super::instance_account_admission(&state, peer, b"create-vault", "create_vault_limited")
        .await?;
    let digest = request_digest(&request, &["admin_password", "request_secret"])?;
    let mut tx = state
        .db
        .begin()
        .await
        .map_err(|error| super::super::database_unavailable(&error, "create_vault_begin"))?;
    let request_row = sqlx::query("SELECT requests.state, requests.expires_at > clock_timestamp() active, requests.secret_digest FROM vault_requests requests JOIN vault_invites invites ON invites.id=requests.invite_id WHERE requests.id=$1 FOR UPDATE OF invites, requests").bind(request.request_id).fetch_optional(&mut *tx).await.map_err(|error| super::super::database_unavailable(&error, "create_vault_request"))?.ok_or(ApiError(StatusCode::NOT_FOUND, "request_not_found"))?;
    if request_row.get::<Vec<u8>, _>("secret_digest")
        != crate::instance::digest(request.request_secret.as_bytes())
    {
        return Err(ApiError(StatusCode::NOT_FOUND, "request_not_found"));
    }
    if let Some(owner) = sqlx::query("SELECT vault_id, request_digest, password_hash FROM vault_owners WHERE operation_id=$1 FOR UPDATE").bind(request.operation_id).fetch_optional(&mut *tx).await.map_err(|error| super::super::database_unavailable(&error, "create_vault_replay"))? {
        return replay_create(&state, tx, owner, &request, digest).await;
    }
    if request_row.get::<String, _>("state") != "approved" || !request_row.get::<bool, _>("active")
    {
        return Err(ApiError(StatusCode::CONFLICT, "request_not_approved"));
    }
    let vault_id = super::super::validate_profile(
        &request.material.profile,
        &request.material.fingerprint,
        request.material.key_epoch,
    )
    .map_err(|_| ApiError(StatusCode::UNPROCESSABLE_ENTITY, "invalid_vault_material"))?;
    let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM vaults WHERE vault_id=$1)")
        .bind(vault_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(|error| super::super::database_unavailable(&error, "create_vault_profile"))?;
    if exists {
        return Err(ApiError(StatusCode::CONFLICT, "operation_conflict"));
    }
    let username = normalize_username(&request.username)?;
    crate::instance::validate_password(&request.admin_password)
        .map_err(|code| ApiError(StatusCode::UNPROCESSABLE_ENTITY, code))?;
    let (admission_public_key, credential_digest) =
        super::super::validate_owner_material(&request.material, &request.credential_digest)?;
    let signing_public_key = super::super::validate_proposed_device(&request.device)?;
    let password_hash = super::super::hash_admin_password(
        state.instance_password_slots.clone(),
        request.admin_password.clone(),
    )
    .await?;
    crate::api::bootstrap_owner(
        &mut tx,
        crate::api::OwnerBootstrap {
            vault_id,
            device_id: request.device.device_id,
            public_key_profile: request.material.profile.clone(),
            encrypted_vault_check_header: URL_SAFE_NO_PAD
                .decode(&request.material.check_header)
                .expect("validated"),
            profile_fingerprint: request.material.fingerprint.clone(),
            key_epoch: request.material.key_epoch,
            credential_digest,
        },
    )
    .await
    .map_err(|_| ApiError(StatusCode::UNPROCESSABLE_ENTITY, "invalid_vault_material"))?;
    crate::enrollment::register_owner_enrollment_metadata(
        &mut tx,
        vault_id,
        request.device.device_id,
        request.material.key_epoch,
        crate::enrollment::OwnerEnrollmentMetadata {
            admission_public_key,
            signing_public_key,
            client_kind: request.device.kind,
            device_name: request.device.name.clone(),
        },
    )
    .await
    .map_err(|_| ApiError(StatusCode::UNPROCESSABLE_ENTITY, "invalid_owner_metadata"))?;
    sqlx::query("INSERT INTO vault_owners(vault_id,username,password_hash,is_admin,operation_id,request_digest) VALUES($1,$2,$3,FALSE,$4,$5)").bind(vault_id).bind(username).bind(password_hash).bind(request.operation_id).bind(digest.as_slice()).execute(&mut *tx).await.map_err(create_owner_conflict)?;
    sqlx::query(
        "UPDATE vault_requests SET state='completed', vault_id=$2, decided_at=now() WHERE id=$1",
    )
    .bind(request.request_id)
    .bind(vault_id)
    .execute(&mut *tx)
    .await
    .map_err(|error| super::super::database_unavailable(&error, "create_vault_complete"))?;
    let session = create_instance_session(&mut tx, request.device.kind, vault_id).await?;
    tx.commit()
        .await
        .map_err(|error| super::super::database_unavailable(&error, "create_vault_commit"))?;
    notify_instance_change(&state);
    instance_claim_response(
        vault_id,
        request.device.device_id,
        false,
        session,
        request.device.kind,
    )
}

fn create_owner_conflict(error: sqlx::Error) -> ApiError {
    let operation_conflict = error
        .as_database_error()
        .and_then(|database| database.constraint())
        == Some("vault_owners_operation_id_key");
    super::super::unique_conflict(
        error,
        if operation_conflict {
            "operation_conflict"
        } else {
            "username_taken"
        },
        "create_vault_owner",
    )
}

async fn replay_create(
    state: &ApiState,
    mut tx: sqlx::Transaction<'_, sqlx::Postgres>,
    owner: sqlx::postgres::PgRow,
    request: &CreateVaultRequest,
    digest: [u8; 32],
) -> ApiResult<Response> {
    if owner.get::<Vec<u8>, _>("request_digest").as_slice() != digest {
        return Err(ApiError(StatusCode::CONFLICT, "operation_conflict"));
    }
    if !super::super::password_matches_admin(
        state.instance_password_slots.clone(),
        owner.get("password_hash"),
        request.admin_password.clone(),
    )
    .await?
    {
        return Err(ApiError(StatusCode::CONFLICT, "operation_conflict"));
    }
    let vault_id: Uuid = owner.get("vault_id");
    let session = create_instance_session(&mut tx, request.device.kind, vault_id).await?;
    tx.commit().await.map_err(|error| {
        super::super::database_unavailable(&error, "create_vault_replay_commit")
    })?;
    instance_claim_response(
        vault_id,
        request.device.device_id,
        true,
        session,
        request.device.kind,
    )
}
