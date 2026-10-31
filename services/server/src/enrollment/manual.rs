//! Manual shared-secret enrollment proof handlers.
//!
//! The parent API router owns mounting this router and supplies the deployment
//! authorization policy explicitly; this module never treats an invalid hosted
//! authorization as an anonymous community request.

use axum::{
    Json, Router,
    extract::{ConnectInfo, Extension, Path, State},
    http::{HeaderMap, Method, StatusCode},
    routing::post,
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use ipnet::IpNet;
use peppy_crypto::enrollment::{
    EnrollmentContext, EnrollmentPurpose, EnrollmentRole as CryptoEnrollmentRole,
    verify_admission_signature, verify_device_proof,
};
use peppy_hosted_api::enrollment::{
    AdmissionChallenge, AdmissionChallengeRequest, AdmissionComplete, ClientKind,
    CompleteAdmissionRequest, EnrollmentRole, ProposedDevice, VaultMaterial, decode_base64url,
    encode_base64url,
};
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Postgres, Row, Transaction};
use std::{
    collections::HashMap,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::Arc,
    time::{Duration, Instant},
};
use uuid::Uuid;

use crate::{
    api::{EnrollmentAuthorization, EnrollmentAuthorizationRequest, EnrollmentOperation},
    enrollment::{client_kind_name, validate_device_name},
    scope,
};

const CHALLENGE_TTL_SECONDS: i64 = 120;
const MAX_ACTIVE_CHALLENGES_PER_VAULT: i64 = 100;
const MAX_FAILED_PROOFS: i32 = 5;
const MANUAL_OWNER_LIMIT: u8 = 5;
const MANUAL_OWNER_WINDOW: Duration = Duration::from_secs(60);
const MAX_MANUAL_OWNER_SOURCES: usize = 1024;
const UNKNOWN_PEER_SOURCE: IpAddr = IpAddr::V4(Ipv4Addr::UNSPECIFIED);
const EXPIRED_ADMISSION_PURGE_LIMIT: i64 = 64;

/// Dependencies deliberately supplied by the API assembler rather than read
/// from process configuration. `hosted` means every request requires the
/// account authorization policy, including completion after a public proof.
#[derive(Clone)]
pub struct EnrollmentRouterState {
    pub db: PgPool,
    pub authorization: Arc<dyn EnrollmentAuthorization>,
    pub hosted: bool,
    canonical_origin: String,
    manual_owner_admissions: Arc<std::sync::Mutex<HashMap<IpAddr, ManualOwnerAdmission>>>,
    trusted_proxy_cidrs: Arc<Vec<IpNet>>,
}

struct ManualOwnerAdmission {
    window: Instant,
    count: u8,
}

impl EnrollmentRouterState {
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
            manual_owner_admissions: Arc::new(std::sync::Mutex::new(HashMap::new())),
            trusted_proxy_cidrs: Arc::new(Vec::new()),
        })
    }

    pub fn with_trusted_proxy_cidrs(mut self, trusted_proxy_cidrs: Vec<IpNet>) -> Self {
        self.trusted_proxy_cidrs = Arc::new(trusted_proxy_cidrs);
        self
    }
}

/// Returns the unmounted manual-enrollment routes.
///
/// The API assembler must merge this router with the application router using
/// its existing `ApiState`-backed routes; see the session report for the exact
/// installer call.
pub fn router(state: EnrollmentRouterState) -> Router {
    Router::new()
        .route("/v1/enrollment/challenges", post(create_challenge))
        .route(
            "/v1/enrollment/challenges/{enrollment_id}/complete",
            post(complete_challenge),
        )
        .with_state(state)
}

async fn create_challenge(
    State(state): State<EnrollmentRouterState>,
    peer: Option<Extension<ConnectInfo<SocketAddr>>>,
    headers: HeaderMap,
    Json(request): Json<AdmissionChallengeRequest>,
) -> Result<Json<AdmissionChallenge>, ManualError> {
    let community_origin = (!state.hosted)
        .then(|| request_origin(&state, &headers))
        .transpose()?;
    let signing_public_key = validate_device(&request.device)?;
    let credential_digest = credential_digest(&request.credential_digest)?;
    let authorization = state
        .authorization
        .authorize_enrollment(EnrollmentAuthorizationRequest {
            headers: headers.clone(),
            method: Method::POST,
            operation: EnrollmentOperation::ManualChallenge,
            vault_id: if state.hosted { None } else { request.vault_id },
        })
        .await;

    let (vault_id, role, authenticated, authorization_device) = match authorization {
        Ok(context) => (
            context.vault_id,
            role_from_authorization(&context.role, request.device.kind)?,
            true,
            context.device_id,
        ),
        Err(_) if state.hosted => {
            return Err(ManualError(
                StatusCode::UNAUTHORIZED,
                "enrollment_unauthorized",
            ));
        }
        Err(_) => (
            community_vault(
                &state,
                request.vault_id,
                request.vault_owner.as_deref(),
                peer.map(|Extension(ConnectInfo(peer))| peer),
                &headers,
                true,
            )
            .await?,
            public_role(request.device.kind),
            false,
            None,
        ),
    };
    let origin = community_origin.unwrap_or_else(|| state.canonical_origin.clone());
    if !state.hosted
        && request
            .vault_id
            .is_some_and(|requested| requested != vault_id)
    {
        return Err(ManualError(StatusCode::FORBIDDEN, "foreign_vault"));
    }
    if !state.hosted && authenticated && request.vault_owner.is_some() {
        let selected = community_vault(
            &state,
            None,
            request.vault_owner.as_deref(),
            None,
            &headers,
            false,
        )
        .await?;
        if selected != vault_id {
            return Err(ManualError(StatusCode::FORBIDDEN, "foreign_vault"));
        }
    }

    let mut transaction = scope::begin(&state.db, vault_id)
        .await
        .map_err(database_error)?;
    sqlx::query("DELETE FROM device_admissions WHERE id IN (SELECT id FROM device_admissions WHERE vault_id=$1 AND consumed_at IS NULL AND expires_at<=now() ORDER BY expires_at LIMIT $2)")
        .bind(vault_id)
        .bind(EXPIRED_ADMISSION_PURGE_LIMIT)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
    let vault = sqlx::query(
        "SELECT public_key_profile, profile_fingerprint, key_epoch
         FROM vaults WHERE vault_id = $1 FOR UPDATE",
    )
    .bind(vault_id)
    .fetch_optional(&mut *transaction)
    .await
    .map_err(database_error)?
    .ok_or(ManualError(StatusCode::NOT_FOUND, "vault_not_found"))?;
    if let Some(device_id) = authorization_device {
        crate::attachment_retention::withdraw_legacy_retention_readiness_on_connection(
            &mut *transaction,
            &headers,
            vault_id,
            device_id,
        )
        .await
        .map_err(database_error)?;
    }
    let fingerprint = parse_fingerprint(vault.get("profile_fingerprint"))?;
    let key_epoch = vault.get::<i32, _>("key_epoch") as u32;
    let admission_key = sqlx::query_scalar::<_, Vec<u8>>(
        "SELECT admission_public_key FROM vault_admission_keys
         WHERE vault_id = $1 AND key_epoch = $2 AND profile_fingerprint = $3",
    )
    .bind(vault_id)
    .bind(key_epoch as i32)
    .bind(vault.get::<String, _>("profile_fingerprint"))
    .fetch_optional(&mut *transaction)
    .await
    .map_err(database_error)?
    .ok_or(ManualError(
        StatusCode::CONFLICT,
        "admission_key_unavailable",
    ))?;
    let _: [u8; 32] = admission_key
        .try_into()
        .map_err(|_| ManualError(StatusCode::CONFLICT, "admission_key_unavailable"))?;

    if let Some(existing) = sqlx::query(
        "SELECT id, nonce, requested_role, device_public_key, kind, name, context_digest
         FROM device_admissions
         WHERE vault_id = $1 AND device_id = $2 AND credential_digest = $3 FOR UPDATE",
    )
    .bind(vault_id)
    .bind(request.device.device_id)
    .bind(credential_digest.as_slice())
    .fetch_optional(&mut *transaction)
    .await
    .map_err(database_error)?
    {
        let expired: bool =
            sqlx::query_scalar("SELECT expires_at <= now() FROM device_admissions WHERE id = $1")
                .bind(existing.get::<Uuid, _>("id"))
                .fetch_one(&mut *transaction)
                .await
                .map_err(database_error)?;
        let consumed: bool = sqlx::query_scalar(
            "SELECT consumed_at IS NOT NULL FROM device_admissions WHERE id = $1",
        )
        .bind(existing.get::<Uuid, _>("id"))
        .fetch_one(&mut *transaction)
        .await
        .map_err(database_error)?;
        if expired && !consumed {
            sqlx::query("DELETE FROM device_admissions WHERE id = $1")
                .bind(existing.get::<Uuid, _>("id"))
                .execute(&mut *transaction)
                .await
                .map_err(database_error)?;
        } else {
            if consumed {
                return Err(ManualError(
                    StatusCode::CONFLICT,
                    "challenge_already_consumed",
                ));
            }
            let existing_key: [u8; 32] = existing
                .get::<Vec<u8>, _>("device_public_key")
                .try_into()
                .map_err(|_| ManualError(StatusCode::CONFLICT, "challenge_identity_mismatch"))?;
            let existing_nonce: [u8; 32] = existing
                .get::<Vec<u8>, _>("nonce")
                .try_into()
                .map_err(|_| ManualError(StatusCode::CONFLICT, "challenge_identity_mismatch"))?;
            let existing_role = parse_role(existing.get("requested_role"))?;
            let existing_context = manual_context(ManualContextFields {
                origin: origin.clone(),
                vault_id,
                fingerprint,
                key_epoch,
                enrollment_id: existing.get("id"),
                nonce: existing_nonce,
                signing_key: existing_key,
                device_id: request.device.device_id,
                credential_digest,
                role: existing_role,
            });
            let existing_digest: [u8; 32] = Sha256::digest(
                existing_context
                    .canonical_bytes()
                    .map_err(|_| ManualError(StatusCode::BAD_REQUEST, "invalid_context"))?,
            )
            .into();
            if existing.get::<Vec<u8>, _>("context_digest").as_slice() != existing_digest
                || existing_key != signing_public_key
                || existing.get::<String, _>("kind") != client_kind_name(request.device.kind)
                || existing.get::<String, _>("name") != request.device.name
            {
                return Err(ManualError(
                    StatusCode::CONFLICT,
                    "challenge_identity_mismatch",
                ));
            }
            transaction.commit().await.map_err(database_error)?;
            return Ok(Json(AdmissionChallenge {
                enrollment_id: existing.get("id"),
                origin,
                vault_id,
                profile: vault.get("public_key_profile"),
                fingerprint: hex::encode(fingerprint),
                key_epoch,
                device: request.device,
                requested_role: existing_role,
                nonce: encode_base64url(&existing_nonce),
                credential_digest: request.credential_digest,
                expires_in_seconds: CHALLENGE_TTL_SECONDS as u32,
            }));
        }
    }

    let active: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM device_admissions
         WHERE vault_id = $1 AND consumed_at IS NULL AND expires_at > now()",
    )
    .bind(vault_id)
    .fetch_one(&mut *transaction)
    .await
    .map_err(database_error)?;
    if active >= MAX_ACTIVE_CHALLENGES_PER_VAULT {
        return Err(ManualError(
            StatusCode::TOO_MANY_REQUESTS,
            "challenge_limit",
        ));
    }

    let enrollment_id = Uuid::new_v4();
    let mut nonce = [0; 32];
    libsodium_rs::random::fill_bytes(&mut nonce);
    let context = manual_context(ManualContextFields {
        origin: origin.clone(),
        vault_id,
        fingerprint,
        key_epoch,
        enrollment_id,
        nonce,
        signing_key: signing_public_key,
        device_id: request.device.device_id,
        credential_digest,
        role,
    });
    let context_digest: [u8; 32] = Sha256::digest(
        context
            .canonical_bytes()
            .map_err(|_| ManualError(StatusCode::BAD_REQUEST, "invalid_context"))?,
    )
    .into();
    sqlx::query(
        "INSERT INTO device_admissions
         (id, vault_id, device_id, kind, name, requested_role, device_public_key,
          credential_digest, nonce, origin, expires_at, context_digest)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,now() + interval '120 seconds',$11)",
    )
    .bind(enrollment_id)
    .bind(vault_id)
    .bind(request.device.device_id)
    .bind(client_kind_name(request.device.kind))
    .bind(&request.device.name)
    .bind(role_name(role))
    .bind(signing_public_key.as_slice())
    .bind(credential_digest.as_slice())
    .bind(nonce.as_slice())
    .bind(&origin)
    .bind(context_digest.as_slice())
    .execute(&mut *transaction)
    .await
    .map_err(database_error)?;
    transaction.commit().await.map_err(database_error)?;

    Ok(Json(AdmissionChallenge {
        enrollment_id,
        origin,
        vault_id,
        profile: vault.get("public_key_profile"),
        fingerprint: hex::encode(fingerprint),
        key_epoch,
        device: request.device,
        requested_role: role,
        nonce: encode_base64url(&nonce),
        credential_digest: request.credential_digest,
        expires_in_seconds: CHALLENGE_TTL_SECONDS as u32,
    }))
}

async fn complete_challenge(
    State(state): State<EnrollmentRouterState>,
    headers: HeaderMap,
    Path(enrollment_id): Path<Uuid>,
    Json(request): Json<CompleteAdmissionRequest>,
) -> Result<Json<AdmissionComplete>, ManualError> {
    let community_origin = (!state.hosted)
        .then(|| request_origin(&state, &headers))
        .transpose()?;
    let credential_digest = credential_digest(&request.credential_digest)?;
    let admission_signature = decode_base64url::<64>(&request.admission_signature)
        .map_err(|_| ManualError(StatusCode::UNAUTHORIZED, "invalid_admission_proof"))?;
    let device_signature = decode_base64url::<64>(&request.device_signature)
        .map_err(|_| ManualError(StatusCode::UNAUTHORIZED, "invalid_device_proof"))?;

    let hosted_context = if state.hosted {
        Some(
            state
                .authorization
                .authorize_enrollment(EnrollmentAuthorizationRequest {
                    headers: headers.clone(),
                    method: Method::POST,
                    operation: EnrollmentOperation::ManualComplete,
                    // Resolve the account-bound tenant before any RLS-scoped read.
                    vault_id: None,
                })
                .await
                .map_err(|_| ManualError(StatusCode::UNAUTHORIZED, "enrollment_unauthorized"))?,
        )
    } else {
        None
    };
    let vault_id = if let Some(context) = hosted_context.as_ref() {
        context.vault_id
    } else {
        sqlx::query_scalar::<_, Uuid>("SELECT vault_id FROM device_admissions WHERE id = $1")
            .bind(enrollment_id)
            .fetch_optional(&state.db)
            .await
            .map_err(database_error)?
            .ok_or(ManualError(StatusCode::UNAUTHORIZED, "challenge_invalid"))?
    };
    let origin = community_origin.unwrap_or_else(|| state.canonical_origin.clone());

    let mut transaction = scope::begin(&state.db, vault_id)
        .await
        .map_err(database_error)?;
    let row = sqlx::query(
        "SELECT a.device_id, a.kind, a.name, a.requested_role, a.device_public_key,
                a.credential_digest, a.nonce, a.origin, a.failed_attempts,
                a.expires_at, a.consumed_at IS NOT NULL AS consumed, a.context_digest,
                v.public_key_profile, v.encrypted_vault_check_header, v.profile_fingerprint,
                v.key_epoch, k.admission_public_key
         FROM device_admissions a
         JOIN vaults v ON v.vault_id = a.vault_id
         JOIN vault_admission_keys k ON k.vault_id = v.vault_id
           AND k.key_epoch = v.key_epoch AND k.profile_fingerprint = v.profile_fingerprint
         WHERE a.id = $1 AND a.vault_id = $2 AND a.expires_at > now() FOR UPDATE OF a, v",
    )
    .bind(enrollment_id)
    .bind(vault_id)
    .fetch_optional(&mut *transaction)
    .await
    .map_err(database_error)?
    .ok_or(ManualError(StatusCode::UNAUTHORIZED, "challenge_invalid"))?;
    if let Some(device_id) = hosted_context
        .as_ref()
        .and_then(|context| context.device_id)
    {
        crate::attachment_retention::withdraw_legacy_retention_readiness_on_connection(
            &mut *transaction,
            &headers,
            vault_id,
            device_id,
        )
        .await
        .map_err(database_error)?;
    }
    if row.get::<String, _>("origin") != origin
        || row.get::<i32, _>("failed_attempts") >= MAX_FAILED_PROOFS
        || row.get::<Vec<u8>, _>("credential_digest").as_slice() != credential_digest
    {
        record_failed_proof(&mut transaction, enrollment_id).await?;
        transaction.commit().await.map_err(database_error)?;
        return Err(ManualError(
            StatusCode::UNAUTHORIZED,
            "challenge_proof_mismatch",
        ));
    }
    let fingerprint = parse_fingerprint(row.get("profile_fingerprint"))?;
    let signing_key: [u8; 32] = row
        .get::<Vec<u8>, _>("device_public_key")
        .try_into()
        .map_err(|_| ManualError(StatusCode::UNAUTHORIZED, "challenge_invalid"))?;
    let admission_key: [u8; 32] = row
        .get::<Vec<u8>, _>("admission_public_key")
        .try_into()
        .map_err(|_| ManualError(StatusCode::UNAUTHORIZED, "challenge_invalid"))?;
    let nonce: [u8; 32] = row
        .get::<Vec<u8>, _>("nonce")
        .try_into()
        .map_err(|_| ManualError(StatusCode::UNAUTHORIZED, "challenge_invalid"))?;
    let role = parse_role(row.get("requested_role"))?;
    let context = manual_context(ManualContextFields {
        origin,
        vault_id,
        fingerprint,
        key_epoch: row.get::<i32, _>("key_epoch") as u32,
        enrollment_id,
        nonce,
        signing_key,
        device_id: row.get("device_id"),
        credential_digest,
        role,
    });
    let context_digest: [u8; 32] = Sha256::digest(
        context
            .canonical_bytes()
            .map_err(|_| ManualError(StatusCode::UNAUTHORIZED, "challenge_invalid"))?,
    )
    .into();
    if row.get::<Vec<u8>, _>("context_digest").as_slice() != context_digest
        || !verify_admission_signature(&admission_key, &context, &admission_signature)
            .unwrap_or(false)
        || !verify_device_proof(&signing_key, &context, &device_signature).unwrap_or(false)
    {
        record_failed_proof(&mut transaction, enrollment_id).await?;
        transaction.commit().await.map_err(database_error)?;
        return Err(ManualError(
            StatusCode::UNAUTHORIZED,
            "challenge_proof_mismatch",
        ));
    }
    let material = material_from_row(&row, admission_key)?;
    if !row.get::<bool, _>("consumed") {
        sqlx::query("INSERT INTO devices(vault_id,device_id,role,public_key,profile_fingerprint,key_epoch,name,kind) VALUES($1,$2,$3,jsonb_build_object('ed25519_public_key',$4),$5,$6,$7,$8)")
            .bind(vault_id).bind(row.get::<Uuid, _>("device_id")).bind(role_name(role))
            .bind(URL_SAFE_NO_PAD.encode(signing_key)).bind(row.get::<String, _>("profile_fingerprint"))
            .bind(row.get::<i32, _>("key_epoch")).bind(row.get::<String, _>("name")).bind(row.get::<String, _>("kind"))
            .execute(&mut *transaction).await.map_err(database_error)?;
        sqlx::query(
            "INSERT INTO device_credentials(token_digest,vault_id,device_id) VALUES($1,$2,$3)",
        )
        .bind(credential_digest.as_slice())
        .bind(vault_id)
        .bind(row.get::<Uuid, _>("device_id"))
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
        sqlx::query(
            "UPDATE device_admissions SET consumed_at=now() WHERE id=$1 AND consumed_at IS NULL",
        )
        .bind(enrollment_id)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
    }
    transaction.commit().await.map_err(database_error)?;
    Ok(Json(AdmissionComplete {
        vault_id,
        device_id: row.get("device_id"),
        role,
        material,
    }))
}

struct ManualContextFields {
    origin: String,
    vault_id: Uuid,
    fingerprint: [u8; 32],
    key_epoch: u32,
    enrollment_id: Uuid,
    nonce: [u8; 32],
    signing_key: [u8; 32],
    device_id: Uuid,
    credential_digest: [u8; 32],
    role: EnrollmentRole,
}

fn manual_context(fields: ManualContextFields) -> EnrollmentContext {
    let ManualContextFields {
        origin,
        vault_id,
        fingerprint,
        key_epoch,
        enrollment_id,
        nonce,
        signing_key,
        device_id,
        credential_digest,
        role,
    } = fields;
    EnrollmentContext {
        purpose: EnrollmentPurpose::Manual,
        canonical_origin: origin,
        vault_id,
        fingerprint,
        key_epoch,
        enrollment_id,
        server_nonce: nonce,
        owner_transfer_public_key: [0; 32],
        recipient_transfer_public_key: [0; 32],
        recipient_signing_public_key: signing_key,
        device_id,
        credential_digest,
        role: match role {
            EnrollmentRole::Owner => CryptoEnrollmentRole::Owner,
            EnrollmentRole::Device => CryptoEnrollmentRole::Device,
            EnrollmentRole::Gateway => CryptoEnrollmentRole::Gateway,
        },
    }
}

async fn community_vault(
    state: &EnrollmentRouterState,
    requested: Option<Uuid>,
    owner: Option<&str>,
    peer: Option<SocketAddr>,
    headers: &HeaderMap,
    admit_owner: bool,
) -> Result<Uuid, ManualError> {
    let owner = normalize_owner_selector(owner)?;
    if admit_owner {
        admit_manual_owner_source(
            &state.manual_owner_admissions,
            peer.map(|peer| resolve_client_ip(peer, headers, &state.trusted_proxy_cidrs))
                .unwrap_or(UNKNOWN_PEER_SOURCE),
        )?;
    }
    let vault_id = if let Some(requested) = requested {
        let exists: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM vault_owners WHERE vault_id=$1)")
                .bind(requested)
                .fetch_one(&state.db)
                .await
                .map_err(database_error)?;
        if !exists {
            return Err(ManualError(StatusCode::NOT_FOUND, "vault_not_found"));
        }
        requested
    } else if let Some(owner) = owner.as_deref() {
        sqlx::query_scalar("SELECT vault_id FROM vault_owners WHERE username=$1")
            .bind(owner)
            .fetch_optional(&state.db)
            .await
            .map_err(database_error)?
            .ok_or(ManualError(StatusCode::NOT_FOUND, "vault_not_found"))?
    } else {
        let owners: Vec<Uuid> =
            sqlx::query_scalar("SELECT vault_id FROM vault_owners ORDER BY created_at LIMIT 2")
                .fetch_all(&state.db)
                .await
                .map_err(database_error)?;
        match owners.as_slice() {
            [] => return Err(ManualError(StatusCode::CONFLICT, "instance_unclaimed")),
            [vault_id] => *vault_id,
            _ => return Err(ManualError(StatusCode::CONFLICT, "vault_owner_required")),
        }
    };
    if let (Some(_), Some(owner)) = (requested, owner) {
        let selected =
            sqlx::query_scalar::<_, Uuid>("SELECT vault_id FROM vault_owners WHERE username=$1")
                .bind(owner)
                .fetch_optional(&state.db)
                .await
                .map_err(database_error)?
                .ok_or(ManualError(StatusCode::NOT_FOUND, "vault_not_found"))?;
        if selected != vault_id {
            return Err(ManualError(StatusCode::FORBIDDEN, "foreign_vault"));
        }
    }
    Ok(vault_id)
}

fn normalize_owner_selector(owner: Option<&str>) -> Result<Option<String>, ManualError> {
    let Some(owner) = owner.map(str::trim).filter(|owner| !owner.is_empty()) else {
        return Ok(None);
    };
    let owner = owner.to_ascii_lowercase();
    let valid = (1..=32).contains(&owner.len())
        && owner.bytes().enumerate().all(|(index, byte)| {
            byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || (index > 0 && matches!(byte, b'.' | b'_' | b'-'))
        });
    if valid {
        Ok(Some(owner))
    } else {
        Err(ManualError(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_username",
        ))
    }
}

fn admit_manual_owner_source(
    admissions: &std::sync::Mutex<HashMap<IpAddr, ManualOwnerAdmission>>,
    source: IpAddr,
) -> Result<(), ManualError> {
    let now = Instant::now();
    let mut admissions = admissions
        .lock()
        .expect("manual owner admissions lock poisoned");
    admissions.retain(|_, admission| now.duration_since(admission.window) < MANUAL_OWNER_WINDOW);
    if !admissions.contains_key(&source)
        && admissions.len() >= MAX_MANUAL_OWNER_SOURCES
        && let Some(source) = admissions.keys().next().copied()
    {
        admissions.remove(&source);
    }
    let admission = admissions.entry(source).or_insert(ManualOwnerAdmission {
        window: now,
        count: 0,
    });
    admission.count = admission.count.saturating_add(1);
    if admission.count > MANUAL_OWNER_LIMIT {
        return Err(ManualError(
            StatusCode::TOO_MANY_REQUESTS,
            "manual_owner_limited",
        ));
    }
    Ok(())
}

fn resolve_client_ip(peer: SocketAddr, headers: &HeaderMap, trusted: &[IpNet]) -> IpAddr {
    if !trusted.iter().any(|network| network.contains(&peer.ip())) {
        return peer.ip();
    }
    let forwarded: Vec<_> = headers.get_all("x-forwarded-for").iter().collect();
    if forwarded.len() != 1 || forwarded[0].as_bytes().len() > 8192 {
        return peer.ip();
    }
    let Some(hops) = std::str::from_utf8(forwarded[0].as_bytes())
        .ok()
        .and_then(|value| {
            let hops: Vec<_> = value.split(',').map(str::trim).collect();
            (1..=16)
                .contains(&hops.len())
                .then_some(hops)
                .filter(|hops| hops.iter().all(|hop| !hop.is_empty()))
        })
        .and_then(|hops| {
            hops.into_iter()
                .map(|hop| hop.parse().ok())
                .collect::<Option<Vec<IpAddr>>>()
        })
    else {
        return peer.ip();
    };
    hops.into_iter()
        .rev()
        .find(|hop| !trusted.iter().any(|network| network.contains(hop)))
        .unwrap_or_else(|| peer.ip())
}

fn validate_device(device: &ProposedDevice) -> Result<[u8; 32], ManualError> {
    validate_device_name(&device.name)
        .map_err(|_| ManualError(StatusCode::UNPROCESSABLE_ENTITY, "invalid_device_name"))?;
    if device.device_id.is_nil() {
        return Err(ManualError(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_device_id",
        ));
    }
    decode_base64url(&device.signing_public_key)
        .map_err(|_| ManualError(StatusCode::UNPROCESSABLE_ENTITY, "invalid_signing_key"))
}
fn credential_digest(value: &str) -> Result<[u8; 32], ManualError> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        return Err(ManualError(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_credential_digest",
        ));
    }
    hex::decode(value)
        .ok()
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or(ManualError(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_credential_digest",
        ))
}
fn parse_fingerprint(value: String) -> Result<[u8; 32], ManualError> {
    credential_digest(&value)
        .map_err(|_| ManualError(StatusCode::CONFLICT, "invalid_vault_fingerprint"))
}
fn public_role(kind: ClientKind) -> EnrollmentRole {
    match kind {
        ClientKind::Android | ClientKind::Ios => EnrollmentRole::Gateway,
        ClientKind::Browser | ClientKind::Desktop => EnrollmentRole::Device,
    }
}
fn role_from_authorization(role: &str, kind: ClientKind) -> Result<EnrollmentRole, ManualError> {
    if matches!(kind, ClientKind::Android | ClientKind::Ios) {
        return Ok(EnrollmentRole::Gateway);
    }
    if role == "owner" {
        Ok(EnrollmentRole::Owner)
    } else {
        Err(ManualError(StatusCode::FORBIDDEN, "owner_required"))
    }
}
fn parse_role(value: String) -> Result<EnrollmentRole, ManualError> {
    match value.as_str() {
        "owner" => Ok(EnrollmentRole::Owner),
        "device" => Ok(EnrollmentRole::Device),
        "gateway" => Ok(EnrollmentRole::Gateway),
        _ => Err(ManualError(StatusCode::UNAUTHORIZED, "challenge_invalid")),
    }
}
fn role_name(role: EnrollmentRole) -> &'static str {
    match role {
        EnrollmentRole::Owner => "owner",
        EnrollmentRole::Device => "device",
        EnrollmentRole::Gateway => "gateway",
    }
}
fn material_from_row(
    row: &sqlx::postgres::PgRow,
    admission_key: [u8; 32],
) -> Result<VaultMaterial, ManualError> {
    Ok(VaultMaterial {
        profile: row.get("public_key_profile"),
        check_header: URL_SAFE_NO_PAD.encode(row.get::<Vec<u8>, _>("encrypted_vault_check_header")),
        fingerprint: row.get("profile_fingerprint"),
        key_epoch: row.get::<i32, _>("key_epoch") as u32,
        admission_public_key: URL_SAFE_NO_PAD.encode(admission_key),
    })
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
    state: &EnrollmentRouterState,
    headers: &HeaderMap,
) -> Result<String, ManualError> {
    let Some(value) = headers.get("origin") else {
        return Ok(state.canonical_origin.clone());
    };
    let value = value
        .to_str()
        .map_err(|_| ManualError(StatusCode::BAD_REQUEST, "invalid_origin"))?;
    let origin = canonical_configured_origin(value)
        .map_err(|_| ManualError(StatusCode::BAD_REQUEST, "invalid_origin"))?;
    if origin != state.canonical_origin {
        return Err(ManualError(StatusCode::FORBIDDEN, "origin_mismatch"));
    }
    Ok(origin)
}
async fn record_failed_proof(
    transaction: &mut Transaction<'_, Postgres>,
    enrollment_id: Uuid,
) -> Result<(), ManualError> {
    sqlx::query(
        "UPDATE device_admissions
         SET failed_attempts = failed_attempts + 1,
             expires_at = CASE WHEN failed_attempts + 1 >= $2 THEN now() ELSE expires_at END
         WHERE id = $1 AND consumed_at IS NULL",
    )
    .bind(enrollment_id)
    .bind(MAX_FAILED_PROOFS)
    .execute(&mut **transaction)
    .await
    .map_err(database_error)?;
    Ok(())
}
fn database_error(error: sqlx::Error) -> ManualError {
    tracing::warn!(
        operation = "manual_enrollment",
        kind = crate::api::database_error_kind(&error),
        "database operation failed"
    );
    ManualError(StatusCode::SERVICE_UNAVAILABLE, "database_unavailable")
}
struct ManualError(StatusCode, &'static str);
impl axum::response::IntoResponse for ManualError {
    fn into_response(self) -> axum::response::Response {
        (
            self.0,
            Json(serde_json::json!({"error": self.1, "code": self.1})),
        )
            .into_response()
    }
}
