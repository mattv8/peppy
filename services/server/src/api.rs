mod attachments;
mod instance;
mod key_profiles;
mod sync;

pub use sync::{DrainStats, TransportOptions, compact_records, drain_outbox, prune_replay_log};

pub type PolicyFuture<'a, T> = std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send + 'a>>;

pub type EnrollmentAuthorizationFuture<'a, T> =
    std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send + 'a>>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Operation {
    Read,
    Sync,
    Export,
    Revoke,
    /// Removes an attachment while retaining the vault.
    AttachmentDelete,
    /// Removes the complete vault. Hosted policy requires account deletion.
    DeleteVault,
    Send,
    Upload,
    Publish,
}

#[derive(Clone, Debug)]
pub struct AccessPrincipal {
    pub vault_id: Uuid,
    pub device_id: Uuid,
    pub role: String,
}

pub trait AccessPolicy: Send + Sync {
    fn authorize(&self, principal: AccessPrincipal, operation: Operation)
    -> PolicyFuture<'_, bool>;
    fn attachment_quota_bytes(&self, _principal: AccessPrincipal) -> PolicyFuture<'_, Option<i64>> {
        Box::pin(async { None })
    }
}

pub struct CommunityAccessPolicy;
impl AccessPolicy for CommunityAccessPolicy {
    fn authorize(
        &self,
        _principal: AccessPrincipal,
        _operation: Operation,
    ) -> PolicyFuture<'_, bool> {
        Box::pin(async { true })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EnrollmentOperation {
    ManualChallenge,
    ManualComplete,
    CreateInvitation,
    ReadInvitation,
    ApproveInvitation,
    ConfirmInvitation,
    CancelInvitation,
}

#[derive(Clone, Debug)]
pub struct EnrollmentAuthorizationRequest {
    pub headers: axum::http::HeaderMap,
    pub method: axum::http::Method,
    pub operation: EnrollmentOperation,
    pub vault_id: Option<Uuid>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EnrollmentAuthContext {
    pub vault_id: Uuid,
    pub device_id: Option<Uuid>,
    pub role: String,
}

pub trait EnrollmentAuthorization: Send + Sync {
    fn authorize_enrollment(
        &self,
        request: EnrollmentAuthorizationRequest,
    ) -> EnrollmentAuthorizationFuture<'_, Result<EnrollmentAuthContext, axum::http::StatusCode>>;
}

pub struct CommunityEnrollmentAuthorization {
    db: PgPool,
    origin: Option<String>,
}

impl CommunityEnrollmentAuthorization {
    pub fn new(db: PgPool) -> Self {
        Self { db, origin: None }
    }

    pub fn with_origin(db: PgPool, origin: Option<&url::Url>) -> Self {
        Self {
            db,
            origin: origin.map(|value| value.origin().ascii_serialization()),
        }
    }
}

impl EnrollmentAuthorization for CommunityEnrollmentAuthorization {
    fn authorize_enrollment(
        &self,
        request: EnrollmentAuthorizationRequest,
    ) -> EnrollmentAuthorizationFuture<'_, Result<EnrollmentAuthContext, axum::http::StatusCode>>
    {
        Box::pin(async move {
            let browser = cookie_token(&request.headers).is_some();
            let token = request
                .headers
                .get("peppy-instance-session")
                .and_then(|value| value.to_str().ok())
                .or_else(|| cookie_token(&request.headers));
            let Some(token) = token else {
                return Err(axum::http::StatusCode::UNAUTHORIZED);
            };
            let row = instance::lookup_session(&self.db, token)
                .await
                .map_err(|_| axum::http::StatusCode::SERVICE_UNAVAILABLE)?
                .ok_or(axum::http::StatusCode::UNAUTHORIZED)?;
            if browser {
                let origin = request
                    .headers
                    .get("origin")
                    .and_then(|value| value.to_str().ok())
                    .ok_or(axum::http::StatusCode::FORBIDDEN)?;
                if self.origin.as_deref() != Some(origin) {
                    return Err(axum::http::StatusCode::FORBIDDEN);
                }
                let csrf = request
                    .headers
                    .get("x-peppy-csrf-token")
                    .and_then(|value| value.to_str().ok())
                    .ok_or(axum::http::StatusCode::FORBIDDEN)?;
                let expected = row.1;
                if expected.as_deref() != Some(crate::instance::digest(csrf.as_bytes()).as_slice())
                {
                    return Err(axum::http::StatusCode::FORBIDDEN);
                }
            }
            let vault_id = row.0.vault_id;
            Ok(EnrollmentAuthContext {
                vault_id,
                device_id: None,
                role: "owner".into(),
            })
        })
    }
}

use axum::{
    Json, Router,
    extract::ws::{CloseFrame, Message, Utf8Bytes, WebSocket, WebSocketUpgrade},
    extract::{ConnectInfo, FromRequestParts, Path, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::{delete, get, post, put},
};
use base64::{
    Engine as _,
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
};
use ipnet::IpNet;
use peppy_hosted_api::{
    PushConfig, PushConfigAdvertisement,
    enrollment::{
        ClaimInstanceRequest, ClaimInstanceResponse, ClientKind, DeploymentMode, EnrollmentDevice,
        EnrollmentRole, InstanceAccount, InstanceInfo, InstanceLoginRequest, InstanceManagement,
        InstanceSession, VaultMaterial,
    },
};
use peppy_hosted_api::{DeviceRosterDevice, DeviceRosterResponse};
use peppy_protocol::{Envelope, EnvelopePurpose};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Row};
use std::{
    collections::HashMap,
    convert::Infallible,
    net::{IpAddr, SocketAddr},
    sync::Arc,
    time::{Duration, SystemTime},
};
use uuid::Uuid;

use crate::{config::Config, scope, storage::Storage};

const TOKEN_BYTES_HEX: usize = 96;
/// Fixed dummy hash checked for unknown usernames so login spends comparable Argon2id
/// time. Its parameters match the libsodium INTERACTIVE limits used for stored admin hashes.
const DUMMY_ADMIN_PASSWORD_HASH: &str =
    "$argon2id$v=19$m=65536,t=2,p=1$c2FsdHNhbHRzYWx0cw$EwM3PQ8nhmqXBdGiYHhvPYGBuA1jw0dz45Etz12Z1TQ";

#[derive(Clone)]
pub(super) struct ApiState {
    pub(super) db: PgPool,
    committed: tokio::sync::broadcast::Sender<(Uuid, i64)>,
    presence_changed: tokio::sync::broadcast::Sender<Uuid>,
    presence_touches: Arc<std::sync::Mutex<HashMap<(Uuid, Uuid), std::time::Instant>>>,
    storage: Option<Storage>,
    vault_attachment_quota_bytes: i64,
    upload_slots: Arc<tokio::sync::Semaphore>,
    options: Arc<TransportOptions>,
    instance_account_admissions: Arc<tokio::sync::Mutex<InstanceAccountAdmissions>>,
    instance_password_slots: Arc<tokio::sync::Semaphore>,
    pub(super) instance_events: tokio::sync::broadcast::Sender<()>,
    pub(super) event_stream_slots: Arc<tokio::sync::Semaphore>,
    pub(super) event_stream_sessions: Arc<std::sync::Mutex<HashMap<Vec<u8>, usize>>>,
    policy: Arc<dyn AccessPolicy>,
    push_config: PushConfig,
    row_security: bool,
    instance_origin: Option<url::Url>,
    claim_policy: crate::instance::ClaimPolicy,
    trusted_proxy_cidrs: Arc<Vec<IpNet>>,
}
struct RateAdmission {
    window: SystemTime,
    count: u8,
}

struct InstanceAccountAdmissions {
    window: SystemTime,
    count: u16,
    sources: HashMap<[u8; 32], RateAdmission>,
}

struct PeerAddress(Option<IpAddr>);

const MAX_FORWARDED_HEADER_BYTES: usize = 8192;
const MAX_FORWARDED_HOPS: usize = 16;

impl FromRequestParts<ApiState> for PeerAddress {
    type Rejection = Infallible;

    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        state: &ApiState,
    ) -> Result<Self, Self::Rejection> {
        let peer = parts
            .extensions
            .get::<ConnectInfo<SocketAddr>>()
            .map(|ConnectInfo(address)| *address);
        Ok(Self(peer.map(|peer| {
            resolve_client_ip(peer, &parts.headers, &state.trusted_proxy_cidrs)
        })))
    }
}

fn resolve_client_ip(peer: SocketAddr, headers: &HeaderMap, trusted: &[IpNet]) -> IpAddr {
    if !trusted.iter().any(|network| network.contains(&peer.ip())) {
        return peer.ip();
    }
    let forwarded: Vec<_> = headers.get_all("x-forwarded-for").iter().collect();
    if forwarded.len() != 1 {
        return peer.ip();
    }
    let value = forwarded[0].as_bytes();
    if value.len() > MAX_FORWARDED_HEADER_BYTES {
        return peer.ip();
    }
    let hops: Option<Vec<IpAddr>> = std::str::from_utf8(value)
        .ok()
        .and_then(|value| {
            let hops: Vec<_> = value.split(',').map(str::trim).collect();
            (1..=MAX_FORWARDED_HOPS)
                .contains(&hops.len())
                .then_some(hops)
                .filter(|hops| hops.iter().all(|hop| !hop.is_empty()))
        })
        .and_then(|hops| hops.into_iter().map(|hop| hop.parse().ok()).collect());
    let Some(hops) = hops else {
        return peer.ip();
    };
    hops.into_iter()
        .rev()
        .find(|hop| !trusted.iter().any(|network| network.contains(hop)))
        .unwrap_or_else(|| peer.ip())
}

/// Capacity of the in-process commit-hint channel shared by all sockets.
const HINT_CAPACITY: usize = 1024;

/// Builds the API with transport policy from the environment
/// (`PEPPY_REPLAY_RETENTION_DAYS`, default 30) and starts the bounded
/// in-process outbox/retention maintenance task.
pub fn router(db: PgPool) -> Router {
    let config = Config::from_env().ok();
    let mut options = TransportOptions::default();
    if let Some(config) = &config {
        options.replay_retention = config.replay_retention;
    }
    let authorization = Arc::new(CommunityEnrollmentAuthorization::with_origin(
        db.clone(),
        config
            .as_ref()
            .and_then(|value| value.public_api_url.as_ref()),
    ));
    let (router, maintenance) = build_router(
        db.clone(),
        config,
        None,
        options,
        None,
        PushConfigAdvertisement::default(),
        RouterAuthorization {
            policy: Arc::new(CommunityAccessPolicy),
            enrollment: authorization,
            claim_policy: crate::instance::configured_claim_policy(false)
                .expect("setup token configuration must be valid"),
            row_security: false,
        },
    );
    maintenance.start();
    router
}

/// Same as [`router`] with explicit transport timing/retention policy.
pub fn router_with_options(db: PgPool, options: TransportOptions) -> Router {
    let config = Config::from_env().ok();
    let authorization = Arc::new(CommunityEnrollmentAuthorization::with_origin(
        db.clone(),
        config
            .as_ref()
            .and_then(|value| value.public_api_url.as_ref()),
    ));
    let (router, maintenance) = build_router(
        db.clone(),
        config,
        None,
        options,
        None,
        PushConfigAdvertisement::default(),
        RouterAuthorization {
            policy: Arc::new(CommunityAccessPolicy),
            enrollment: authorization,
            claim_policy: crate::instance::configured_claim_policy(false)
                .expect("setup token configuration must be valid"),
            row_security: false,
        },
    );
    maintenance.start();
    router
}

/// Testable router variant with an explicit, fixed relay base URL.
pub fn router_with_options_and_relay(
    db: PgPool,
    options: TransportOptions,
    relay_url: url::Url,
) -> Router {
    let (router, maintenance) = build_router(
        db.clone(),
        None,
        None,
        options,
        Some(relay_url),
        PushConfigAdvertisement::default(),
        RouterAuthorization {
            policy: Arc::new(CommunityAccessPolicy),
            enrollment: Arc::new(CommunityEnrollmentAuthorization::new(db.clone())),
            claim_policy: crate::instance::configured_claim_policy(false)
                .expect("setup token configuration must be valid"),
            row_security: false,
        },
    );
    maintenance.start();
    router
}

pub(crate) struct RouterAuthorization {
    pub(crate) policy: Arc<dyn AccessPolicy>,
    pub(crate) enrollment: Arc<dyn EnrollmentAuthorization>,
    pub(crate) claim_policy: crate::instance::ClaimPolicy,
    pub(crate) row_security: bool,
}

pub(crate) struct Maintenance {
    pub(crate) db: PgPool,
    committed: tokio::sync::broadcast::Sender<(Uuid, i64)>,
    storage: Option<Storage>,
    options: TransportOptions,
    relay_url: Option<url::Url>,
}

impl Maintenance {
    pub(crate) fn start(self) {
        sync::spawn_maintenance(self.db.clone(), &self.committed, self.storage, self.options);
        spawn_instance_maintenance(self.db.clone());
        if let Some(relay_url) = self.relay_url {
            spawn_wake_maintenance(self.db, &self.committed, relay_url);
        }
    }
}

fn spawn_instance_maintenance(db: PgPool) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(3_600));
        loop {
            interval.tick().await;
            let result = sqlx::query(
                "DELETE FROM vault_requests WHERE (state IN ('denied', 'completed') OR (state IN ('pending', 'approved') AND expires_at < now())) AND created_at < now() - interval '30 days'",
            )
            .execute(&db)
            .await;
            if let Err(error) = result {
                tracing::warn!(%error, "instance request retention sweep failed");
            }
            let result = sqlx::query(
                "DELETE FROM vault_invites WHERE (state IN ('revoked', 'used') OR (state = 'active' AND expires_at < now())) AND created_at < now() - interval '30 days'",
            )
            .execute(&db)
            .await;
            if let Err(error) = result {
                tracing::warn!(%error, "instance invite retention sweep failed");
            }
        }
    });
}

pub(crate) fn build_router(
    db: PgPool,
    config: Option<Config>,
    initialized_storage: Option<Storage>,
    options: TransportOptions,
    explicit_relay_url: Option<url::Url>,
    push_advertisement: PushConfigAdvertisement,
    authorization: RouterAuthorization,
) -> (Router, Maintenance) {
    let RouterAuthorization {
        policy,
        enrollment: enrollment_authorization,
        claim_policy,
        row_security,
    } = authorization;
    let (committed, _) = tokio::sync::broadcast::channel(HINT_CAPACITY);
    let (presence_changed, _) = tokio::sync::broadcast::channel(HINT_CAPACITY);
    let storage = initialized_storage.or_else(|| {
        config
            .as_ref()
            .and_then(|config| config.s3.as_ref().map(Storage::new))
    });
    let relay_url =
        explicit_relay_url.or_else(|| config.as_ref().and_then(|config| config.relay_url.clone()));
    let invitation_origin = config
        .as_ref()
        .and_then(|config| config.public_api_url.as_ref())
        .map(|origin| origin.origin().ascii_serialization());
    let vault_attachment_quota_bytes = config
        .as_ref()
        .map(|config| config.vault_attachment_quota_bytes)
        .unwrap_or(512 * 1024 * 1024);
    let trusted_proxy_cidrs = config
        .as_ref()
        .map(|config| config.trusted_proxy_cidrs.clone())
        .unwrap_or_default();
    let push_config_response = push_advertisement
        .response()
        .expect("router requires a validated push configuration");
    let router = Router::new()
        .route("/v1/vault", get(vault_header))
        .route("/v1/devices/{device_id}/revoke", post(revoke_device))
        .route("/v1/devices/self/push-config", get(push_config))
        .route(
            "/v1/devices/self/wake-route",
            put(register_wake_route).delete(revoke_wake_route),
        )
        .route("/v1/devices", get(devices))
        .route(
            "/v1/capabilities",
            get(capabilities).post(update_capabilities),
        )
        .route("/v1/events", post(ingest))
        .route(
            "/v1/compaction/capability",
            post(register_compaction_capability),
        )
        .route("/v1/commands", post(ingest))
        .route("/v1/events", get(sync::events))
        .route("/v1/snapshot", get(sync::snapshot_start))
        .route("/v1/snapshot/records", get(sync::snapshot_records))
        .route("/v1/commands/pending", get(sync::pending_commands))
        .route(
            "/v1/vault/key-profiles",
            get(key_profiles::list).post(key_profiles::register),
        )
        .route(
            "/v1/vault/key-profiles/{key_epoch}/activate",
            post(key_profiles::activate),
        )
        .route("/v1/vault", delete(delete_vault))
        .route("/v1/commands/{command_id}/receipts", post(receipt))
        .route(
            "/v1/attachments/reserve",
            post(attachments::reserve_attachment),
        )
        .route(
            "/v1/attachments/{attachment_id}/upload",
            put(attachments::upload_attachment),
        )
        .route(
            "/v1/attachments/{attachment_id}/finalize",
            post(attachments::finalize_attachment),
        )
        .route(
            "/v1/attachments/{attachment_id}",
            get(attachments::download_attachment).delete(attachments::release_attachment),
        )
        .route(
            "/v1/attachments/{attachment_id}/references",
            post(attachments::register_attachment_references),
        )
        .route(
            "/v1/attachments/{attachment_id}/public-copies",
            post(attachments::create_public_copy),
        )
        .route(
            "/v1/public-copies/{share_id}/revoke",
            post(attachments::revoke_public_copy),
        )
        .route(
            "/file/mms-usercontent/{token}/{safe_name}",
            get(attachments::download_public_copy),
        )
        .route("/v1/ws", get(websocket));
    let router = if row_security {
        router
    } else {
        router.merge(instance::routes())
    };
    let router = router.with_state(ApiState {
        db: db.clone(),
        committed: committed.clone(),
        presence_changed,
        presence_touches: Arc::new(std::sync::Mutex::new(HashMap::new())),
        storage: storage.clone(),
        vault_attachment_quota_bytes,
        upload_slots: Arc::new(tokio::sync::Semaphore::new(4)),
        options: Arc::new(options.clone()),
        instance_account_admissions: Arc::new(tokio::sync::Mutex::new(InstanceAccountAdmissions {
            window: SystemTime::now(),
            count: 0,
            sources: HashMap::new(),
        })),
        instance_password_slots: Arc::new(tokio::sync::Semaphore::new(2)),
        instance_events: tokio::sync::broadcast::channel(64).0,
        event_stream_slots: Arc::new(tokio::sync::Semaphore::new(32)),
        event_stream_sessions: Arc::new(std::sync::Mutex::new(HashMap::new())),
        policy,
        push_config: push_config_response,
        row_security,
        instance_origin: config
            .as_ref()
            .and_then(|config| config.public_api_url.clone()),
        claim_policy,
        trusted_proxy_cidrs: Arc::new(trusted_proxy_cidrs.clone()),
    });
    let router = match invitation_origin.as_ref().and_then(|origin| {
        crate::enrollment::manual::EnrollmentRouterState::new(
            db.clone(),
            enrollment_authorization.clone(),
            row_security,
            origin.clone(),
        )
        .ok()
        .map(|state| state.with_trusted_proxy_cidrs(trusted_proxy_cidrs.clone()))
    }) {
        Some(state) => router.merge(crate::enrollment::manual::router(state)),
        None => router,
    };
    let router = match invitation_origin.and_then(|origin| {
        crate::enrollment::invitations::InvitationRouterState::new(
            db.clone(),
            enrollment_authorization.clone(),
            row_security,
            origin,
        )
        .ok()
    }) {
        Some(state) => router.merge(crate::enrollment::invitations::router(state)),
        None => router,
    };
    (
        router,
        Maintenance {
            db,
            committed,
            storage,
            options,
            relay_url,
        },
    )
}

#[derive(Debug)]
pub(super) struct ApiError(pub(super) StatusCode, pub(super) &'static str);
impl IntoResponse for ApiError {
    fn into_response(self) -> axum::response::Response {
        (self.0, Json(json!({"code":self.1}))).into_response()
    }
}
pub(super) type ApiResult<T> = Result<T, ApiError>;

pub(super) async fn instance_info(State(state): State<ApiState>) -> ApiResult<Json<InstanceInfo>> {
    let claimed: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM vault_owners WHERE is_admin)")
            .fetch_one(&state.db)
            .await
            .map_err(|error| database_unavailable(&error, "instance_info"))?;
    Ok(Json(InstanceInfo {
        mode: DeploymentMode::Community,
        claimed,
        setup_required: !claimed && state.claim_policy.requires_setup_token(),
        password_auth: claimed,
        qr_pairing: claimed,
        shared_secret_pairing: claimed,
    }))
}

fn decode_hex_digest(value: &str) -> Result<[u8; 32], ApiError> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(ApiError(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_credential_digest",
        ));
    }
    hex::decode(value)
        .ok()
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or(ApiError(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_credential_digest",
        ))
}

fn decode_b64url<const N: usize>(value: &str, code: &'static str) -> Result<[u8; N], ApiError> {
    URL_SAFE_NO_PAD
        .decode(value)
        .ok()
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or(ApiError(StatusCode::UNPROCESSABLE_ENTITY, code))
}

pub(super) fn validate_proposed_device(
    device: &peppy_hosted_api::enrollment::ProposedDevice,
) -> Result<[u8; 32], ApiError> {
    crate::enrollment::validate_device_name(&device.name)
        .map_err(|_| ApiError(StatusCode::UNPROCESSABLE_ENTITY, "invalid_device_name"))?;
    decode_b64url(&device.signing_public_key, "invalid_signing_public_key")
}

pub(super) fn validate_claim_material(
    request: &ClaimInstanceRequest,
) -> Result<([u8; 32], [u8; 32]), ApiError> {
    let profile_vault = validate_profile(
        &request.material.profile,
        &request.material.fingerprint,
        request.material.key_epoch,
    )
    .map_err(|_| ApiError(StatusCode::UNPROCESSABLE_ENTITY, "invalid_vault_material"))?;
    if profile_vault.is_nil() || request.device.device_id.is_nil() {
        return Err(ApiError(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_vault_material",
        ));
    }
    let header = URL_SAFE_NO_PAD
        .decode(&request.material.check_header)
        .map_err(|_| ApiError(StatusCode::UNPROCESSABLE_ENTITY, "invalid_vault_material"))?;
    if header.is_empty() || header.len() > 1_048_576 {
        return Err(ApiError(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_vault_material",
        ));
    }
    Ok((
        decode_b64url(
            &request.material.admission_public_key,
            "invalid_admission_public_key",
        )?,
        decode_hex_digest(&request.credential_digest)?,
    ))
}

pub(super) fn validate_owner_material(
    material: &VaultMaterial,
    credential_digest: &str,
) -> Result<([u8; 32], [u8; 32]), ApiError> {
    validate_profile(&material.profile, &material.fingerprint, material.key_epoch)
        .map_err(|_| ApiError(StatusCode::UNPROCESSABLE_ENTITY, "invalid_vault_material"))?;
    Ok((
        decode_b64url(
            &material.admission_public_key,
            "invalid_admission_public_key",
        )?,
        decode_hex_digest(credential_digest)?,
    ))
}

pub(super) async fn create_instance_session(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    client: ClientKind,
    vault_id: Uuid,
) -> ApiResult<InstanceSession> {
    let session_token = crate::instance::random_token()
        .map_err(|_| ApiError(StatusCode::SERVICE_UNAVAILABLE, "session_unavailable"))?;
    let csrf_token = matches!(client, ClientKind::Browser)
        .then(|| crate::instance::browser_csrf_token(&session_token));
    sqlx::query("INSERT INTO instance_sessions(token_digest, csrf_digest, vault_id, expires_at) VALUES($1, $2, $3, now() + interval '12 hours')")
        .bind(crate::instance::digest(session_token.as_bytes()).as_slice())
        .bind(csrf_token.as_ref().map(|token| crate::instance::digest(token.as_bytes()).to_vec()))
        .bind(vault_id)
        .execute(&mut **transaction)
        .await
        .map_err(|error| database_unavailable(&error, "instance_session_create"))?;
    Ok(InstanceSession {
        session_token: Some(session_token),
        csrf_token,
        expires_in_seconds: 43_200,
    })
}

pub(super) async fn hash_admin_password(
    slots: Arc<tokio::sync::Semaphore>,
    password: String,
) -> ApiResult<String> {
    let _permit = slots
        .acquire_owned()
        .await
        .map_err(|_| ApiError(StatusCode::SERVICE_UNAVAILABLE, "password_hash_failed"))?;
    tokio::task::spawn_blocking(move || crate::instance::password_hash(&password))
        .await
        .map_err(|_| ApiError(StatusCode::SERVICE_UNAVAILABLE, "password_hash_failed"))?
        .map_err(|code| ApiError(StatusCode::SERVICE_UNAVAILABLE, code))
}

pub(super) async fn password_matches_admin(
    slots: Arc<tokio::sync::Semaphore>,
    hash: String,
    password: String,
) -> ApiResult<bool> {
    let _permit = slots
        .acquire_owned()
        .await
        .map_err(|_| ApiError(StatusCode::SERVICE_UNAVAILABLE, "password_verify_failed"))?;
    tokio::task::spawn_blocking(move || crate::instance::password_matches(&hash, &password))
        .await
        .map_err(|_| ApiError(StatusCode::SERVICE_UNAVAILABLE, "password_verify_failed"))
}

pub(super) fn instance_origin_matches(state: &ApiState, headers: &HeaderMap) -> bool {
    let Some(origin) = headers.get("origin").and_then(|value| value.to_str().ok()) else {
        return false;
    };
    if let Some(expected) = &state.instance_origin {
        return origin == expected.origin().ascii_serialization();
    }
    let Ok(origin) = origin.parse::<url::Url>() else {
        return false;
    };
    let host = headers.get("host").and_then(|value| value.to_str().ok());
    let authority = origin.host_str().map(|hostname| match origin.port() {
        Some(port) => format!("{hostname}:{port}"),
        None => hostname.to_owned(),
    });
    matches!(origin.scheme(), "http" | "https")
        && origin
            .host_str()
            .is_some_and(|host| matches!(host, "localhost" | "127.0.0.1" | "[::1]"))
        && host
            .zip(authority.as_deref())
            .is_some_and(|(host, authority)| host == authority)
}

fn cookie_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get("cookie")?
        .to_str()
        .ok()?
        .split(';')
        .find_map(|part| part.trim().strip_prefix("Peppy-Instance-Session="))
}

pub(super) fn instance_session_token(headers: &HeaderMap) -> Option<(&str, bool)> {
    cookie_token(headers)
        .map(|token| (token, true))
        .or_else(|| {
            headers
                .get("peppy-instance-session")
                .and_then(|value| value.to_str().ok())
                .map(|token| (token, false))
        })
}

pub(super) fn instance_session_response(
    mut session: InstanceSession,
    client: ClientKind,
) -> ApiResult<Response> {
    if !matches!(client, ClientKind::Browser) {
        return Ok(Json(session).into_response());
    }
    let session_token = session.session_token.as_deref().ok_or(ApiError(
        StatusCode::SERVICE_UNAVAILABLE,
        "session_unavailable",
    ))?;
    let cookie = format!(
        "Peppy-Instance-Session={session_token}; Path=/; Max-Age={}; Secure; HttpOnly; SameSite=Strict",
        session.expires_in_seconds
    );
    session.session_token = None;
    let mut response = Json(session).into_response();
    response.headers_mut().insert(
        "set-cookie",
        HeaderValue::from_str(&cookie)
            .map_err(|_| ApiError(StatusCode::SERVICE_UNAVAILABLE, "session_unavailable"))?,
    );
    Ok(response)
}

pub(super) fn instance_claim_response(
    vault_id: Uuid,
    device_id: Uuid,
    replayed: bool,
    mut session: InstanceSession,
    client: ClientKind,
) -> ApiResult<Response> {
    if !matches!(client, ClientKind::Browser) {
        return Ok(Json(ClaimInstanceResponse {
            vault_id,
            device_id,
            replayed,
            session,
        })
        .into_response());
    }
    let session_token = session.session_token.as_deref().ok_or(ApiError(
        StatusCode::SERVICE_UNAVAILABLE,
        "session_unavailable",
    ))?;
    let cookie = format!(
        "Peppy-Instance-Session={session_token}; Path=/; Max-Age={}; Secure; HttpOnly; SameSite=Strict",
        session.expires_in_seconds
    );
    session.session_token = None;
    let mut response = Json(ClaimInstanceResponse {
        vault_id,
        device_id,
        replayed,
        session,
    })
    .into_response();
    response.headers_mut().insert(
        "set-cookie",
        HeaderValue::from_str(&cookie)
            .map_err(|_| ApiError(StatusCode::SERVICE_UNAVAILABLE, "session_unavailable"))?,
    );
    Ok(response)
}

async fn replay_existing_instance_claim(
    state: &ApiState,
    mut transaction: sqlx::Transaction<'_, sqlx::Postgres>,
    existing: sqlx::postgres::PgRow,
    request: &ClaimInstanceRequest,
    request_digest: [u8; 32],
    peer: Option<IpAddr>,
) -> ApiResult<Response> {
    let operation_id: Uuid = existing.get("operation_id");
    let previous_digest: Vec<u8> = existing.get("request_digest");
    if operation_id != request.operation_id || previous_digest.as_slice() != request_digest {
        return Err(ApiError(StatusCode::CONFLICT, "instance_already_claimed"));
    }
    instance_account_admission(
        state,
        peer,
        b"claim-replay",
        "instance_claim_replay_limited",
    )
    .await?;
    let password_hash: String = existing.get("password_hash");
    if !password_matches_admin(
        state.instance_password_slots.clone(),
        password_hash,
        request.admin_password.clone(),
    )
    .await?
    {
        return Err(ApiError(StatusCode::CONFLICT, "instance_already_claimed"));
    }
    let session = create_instance_session(
        &mut transaction,
        request.device.kind,
        existing.get("vault_id"),
    )
    .await?;
    transaction
        .commit()
        .await
        .map_err(|error| database_unavailable(&error, "instance_claim_replay"))?;
    instance_claim_response(
        existing.get("vault_id"),
        request.device.device_id,
        true,
        session,
        request.device.kind,
    )
}

async fn claim_instance(
    State(state): State<ApiState>,
    PeerAddress(peer): PeerAddress,
    headers: HeaderMap,
    Json(request): Json<ClaimInstanceRequest>,
) -> ApiResult<Response> {
    if matches!(request.device.kind, ClientKind::Browser)
        && !instance_origin_matches(&state, &headers)
    {
        return Err(ApiError(StatusCode::FORBIDDEN, "invalid_origin"));
    }
    let request_digest = instance::request_digest(&request, &["admin_password", "setup_token"])?;
    let mut replay_transaction = state
        .db
        .begin()
        .await
        .map_err(|error| database_unavailable(&error, "instance_claim_begin"))?;
    if let Some(existing) = sqlx::query("SELECT vault_id, operation_id, request_digest, password_hash FROM vault_owners WHERE is_admin FOR UPDATE")
        .fetch_optional(&mut *replay_transaction).await.map_err(|error| database_unavailable(&error, "instance_claim_lookup"))?
    {
        return replay_existing_instance_claim(
            &state,
            replay_transaction,
            existing,
            &request,
            request_digest,
            peer,
        )
        .await;
    }
    drop(replay_transaction);

    // Fresh claims consume a bounded admission before token validation or expensive
    // material/password work. Replays intentionally retain their separate path.
    instance_account_admission(&state, peer, b"claim", "instance_claim_limited").await?;
    match state.claim_policy {
        crate::instance::ClaimPolicy::Disabled => {
            return Err(ApiError(StatusCode::CONFLICT, "instance_already_claimed"));
        }
        policy if !policy.authorizes(&request.setup_token) => {
            return Err(ApiError(StatusCode::UNAUTHORIZED, "invalid_setup_token"));
        }
        _ => {}
    }
    let username = instance::normalize_claim_username(&request.username)?;
    let (admission_public_key, credential_digest) = validate_claim_material(&request)?;
    let signing_public_key = validate_proposed_device(&request.device)?;
    crate::instance::validate_password(&request.admin_password)
        .map_err(|code| ApiError(StatusCode::UNPROCESSABLE_ENTITY, code))?;
    let vault_id = validate_profile(
        &request.material.profile,
        &request.material.fingerprint,
        request.material.key_epoch,
    )
    .map_err(|_| ApiError(StatusCode::UNPROCESSABLE_ENTITY, "invalid_vault_material"))?;
    let mut transaction = state
        .db
        .begin()
        .await
        .map_err(|error| database_unavailable(&error, "instance_claim_begin"))?;
    if let Some(existing) = sqlx::query("SELECT vault_id, operation_id, request_digest, password_hash FROM vault_owners WHERE is_admin FOR UPDATE")
        .fetch_optional(&mut *transaction).await.map_err(|error| database_unavailable(&error, "instance_claim_lookup"))?
    {
        return replay_existing_instance_claim(
            &state,
            transaction,
            existing,
            &request,
            request_digest,
            peer,
        )
        .await;
    }
    let password_hash = hash_admin_password(
        state.instance_password_slots.clone(),
        request.admin_password.clone(),
    )
    .await?;
    crate::api::bootstrap_owner(
        &mut transaction,
        OwnerBootstrap {
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
    .map_err(|_| ApiError(StatusCode::CONFLICT, "instance_already_claimed"))?;
    crate::enrollment::register_owner_enrollment_metadata(
        &mut transaction,
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
    sqlx::query("INSERT INTO vault_owners(vault_id, username, password_hash, is_admin, operation_id, request_digest) VALUES($1, $2, $3, TRUE, $4, $5)")
        .bind(vault_id).bind(username).bind(password_hash).bind(request.operation_id).bind(request_digest.as_slice())
        .execute(&mut *transaction).await.map_err(|error| unique_conflict(error, "instance_already_claimed", "instance_claim_insert"))?;
    let session = create_instance_session(&mut transaction, request.device.kind, vault_id).await?;
    transaction
        .commit()
        .await
        .map_err(|error| database_unavailable(&error, "instance_claim_commit"))?;
    instance_claim_response(
        vault_id,
        request.device.device_id,
        false,
        session,
        request.device.kind,
    )
}

async fn instance_login(
    State(state): State<ApiState>,
    PeerAddress(peer): PeerAddress,
    headers: HeaderMap,
    Json(request): Json<InstanceLoginRequest>,
) -> ApiResult<Response> {
    if matches!(request.client, ClientKind::Browser) && !instance_origin_matches(&state, &headers) {
        return Err(ApiError(StatusCode::FORBIDDEN, "invalid_origin"));
    }
    instance_account_admission(&state, peer, b"login", "instance_login_limited").await?;
    crate::instance::validate_password(&request.admin_password)
        .map_err(|code| ApiError(StatusCode::UNAUTHORIZED, code))?;
    let mut transaction = state
        .db
        .begin()
        .await
        .map_err(|error| database_unavailable(&error, "instance_login_begin"))?;
    let owner = if request.username.trim().is_empty() {
        sqlx::query("SELECT vault_id, password_hash FROM vault_owners WHERE is_admin")
            .fetch_optional(&mut *transaction)
            .await
    } else {
        let username = instance::normalize_username(&request.username)?;
        sqlx::query("SELECT vault_id, password_hash FROM vault_owners WHERE username=$1")
            .bind(username)
            .fetch_optional(&mut *transaction)
            .await
    }
    .map_err(|error| database_unavailable(&error, "instance_login_lookup"))?;
    let (vault_id, password_hash) = owner
        .map(|row| (Some(row.get("vault_id")), row.get("password_hash")))
        .unwrap_or((None, DUMMY_ADMIN_PASSWORD_HASH.into()));
    if !password_matches_admin(
        state.instance_password_slots.clone(),
        password_hash,
        request.admin_password,
    )
    .await?
    {
        return Err(ApiError(StatusCode::UNAUTHORIZED, "invalid_admin_password"));
    }
    let Some(vault_id) = vault_id else {
        return Err(ApiError(StatusCode::UNAUTHORIZED, "invalid_admin_password"));
    };
    let session = create_instance_session(&mut transaction, request.client, vault_id).await?;
    transaction
        .commit()
        .await
        .map_err(|error| database_unavailable(&error, "instance_login_commit"))?;
    instance_session_response(session, request.client)
}

pub(super) async fn instance_logout(
    State(state): State<ApiState>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    instance::require_session(&state, &headers, true).await?;
    let (token, browser) = instance_session_token(&headers)
        .ok_or(ApiError(StatusCode::UNAUTHORIZED, "invalid_session"))?;
    sqlx::query("UPDATE instance_sessions SET revoked_at = now() WHERE token_digest = $1 AND revoked_at IS NULL")
        .bind(crate::instance::digest(token.as_bytes()).as_slice()).execute(&state.db).await
        .map_err(|error| database_unavailable(&error, "instance_logout"))?;
    let mut response = StatusCode::NO_CONTENT.into_response();
    if browser {
        response.headers_mut().insert(
            "set-cookie",
            HeaderValue::from_static(
                "Peppy-Instance-Session=; Path=/; Max-Age=0; Secure; HttpOnly; SameSite=Strict",
            ),
        );
    }
    Ok(response)
}

fn enrollment_role(role: String) -> Result<EnrollmentRole, ApiError> {
    match role.as_str() {
        "owner" => Ok(EnrollmentRole::Owner),
        "device" => Ok(EnrollmentRole::Device),
        "gateway" => Ok(EnrollmentRole::Gateway),
        _ => Err(ApiError(
            StatusCode::SERVICE_UNAVAILABLE,
            "invalid_device_role",
        )),
    }
}

fn client_kind(kind: String) -> Result<ClientKind, ApiError> {
    match kind.as_str() {
        "browser" => Ok(ClientKind::Browser),
        "desktop" => Ok(ClientKind::Desktop),
        "android" => Ok(ClientKind::Android),
        "ios" => Ok(ClientKind::Ios),
        _ => Err(ApiError(
            StatusCode::SERVICE_UNAVAILABLE,
            "invalid_device_kind",
        )),
    }
}

async fn instance_devices(db: &PgPool, vault_id: Uuid) -> ApiResult<Vec<EnrollmentDevice>> {
    let rows = sqlx::query("SELECT d.device_id, COALESCE(d.name, 'Unnamed device') name, COALESCE(d.kind, 'desktop') kind, d.role, d.revoked_at IS NOT NULL revoked, EXISTS(SELECT 1 FROM device_wake_routes routes WHERE routes.vault_id=d.vault_id AND routes.device_id=d.device_id AND routes.revoked_at IS NULL) gateway_online FROM devices d WHERE d.vault_id=$1 ORDER BY d.created_at")
        .bind(vault_id).fetch_all(db).await
        .map_err(|error| database_unavailable(&error, "instance_devices"))?;
    let presence_by_device = crate::presence::lookup_many_best_effort(
        db,
        vault_id,
        &rows
            .iter()
            .map(|row| (row.get("device_id"), row.get("revoked")))
            .collect::<Vec<_>>(),
    )
    .await;
    let mut devices = Vec::with_capacity(rows.len());
    for row in rows {
        let device_id = row.get("device_id");
        let revoked = row.get("revoked");
        let presence = presence_by_device.get(&device_id).cloned();
        let gateway_online = row.get("gateway_online");
        devices.push(EnrollmentDevice {
            device_id,
            name: row.get("name"),
            kind: client_kind(row.get("kind"))?,
            role: enrollment_role(row.get("role"))?,
            revoked,
            // This legacy field intentionally describes wake registration only.
            gateway_online,
            presence,
            wake_route: gateway_online,
        });
    }
    Ok(devices)
}

pub(super) async fn instance_account(
    State(state): State<ApiState>,
    headers: HeaderMap,
) -> ApiResult<Json<InstanceAccount>> {
    let vault_id = instance::require_session(&state, &headers, false)
        .await?
        .vault_id;
    let row = sqlx::query("SELECT v.public_key_profile, v.encrypted_vault_check_header, v.profile_fingerprint, v.key_epoch, keys.admission_public_key FROM vaults v JOIN vault_admission_keys keys ON keys.vault_id=v.vault_id AND keys.key_epoch=v.key_epoch WHERE v.vault_id=$1")
        .bind(vault_id).fetch_one(&state.db).await
        .map_err(|error| database_unavailable(&error, "instance_account"))?;
    Ok(Json(InstanceAccount {
        vault_id,
        material: VaultMaterial {
            profile: row.get("public_key_profile"),
            check_header: URL_SAFE_NO_PAD
                .encode(row.get::<Vec<u8>, _>("encrypted_vault_check_header")),
            fingerprint: row.get("profile_fingerprint"),
            key_epoch: row.get::<i32, _>("key_epoch") as u32,
            admission_public_key: URL_SAFE_NO_PAD
                .encode(row.get::<Vec<u8>, _>("admission_public_key")),
        },
        devices: instance_devices(&state.db, vault_id).await?,
    }))
}

pub(super) async fn instance_management(
    State(state): State<ApiState>,
    headers: HeaderMap,
) -> ApiResult<Json<InstanceManagement>> {
    let session = instance::require_session(&state, &headers, false)
        .await
        .map_err(|error| {
            if error.0 == StatusCode::UNAUTHORIZED {
                ApiError(StatusCode::FORBIDDEN, "admin_session_required")
            } else {
                error
            }
        })?;
    let vault_id = session.vault_id;
    let storage_bytes: i64 = sqlx::query_scalar(
        "SELECT COALESCE(sum(ciphertext_bytes), 0)::bigint FROM attachments WHERE vault_id=$1",
    )
    .bind(vault_id)
    .fetch_one(&state.db)
    .await
    .map_err(|error| database_unavailable(&error, "instance_storage"))?;
    Ok(Json(InstanceManagement {
        vault_id,
        devices: instance_devices(&state.db, vault_id).await?,
        storage_bytes: storage_bytes.max(0) as u64,
        quota_bytes: state.vault_attachment_quota_bytes.max(0) as u64,
        replay_retention_days: state
            .options
            .replay_retention
            .as_secs()
            .saturating_div(86_400) as u32,
        is_admin: session.is_admin,
        username: session.username,
    }))
}

pub(super) async fn instance_revoke_device(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Path(device_id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    let vault_id = instance::require_session(&state, &headers, true)
        .await?
        .vault_id;
    let mut tx = scope::begin(&state.db, vault_id)
        .await
        .map_err(|error| database_unavailable(&error, "instance_revoke_begin"))?;
    sqlx::query("SELECT 1 FROM vaults WHERE vault_id=$1 FOR UPDATE")
        .bind(vault_id)
        .execute(&mut *tx)
        .await
        .map_err(|error| database_unavailable(&error, "instance_revoke_lock"))?;
    let target = sqlx::query("SELECT role, revoked_at IS NULL active FROM devices WHERE vault_id=$1 AND device_id=$2 FOR UPDATE")
        .bind(vault_id).bind(device_id).fetch_optional(&mut *tx).await
        .map_err(|error| database_unavailable(&error, "instance_revoke_lookup"))?
        .ok_or(ApiError(StatusCode::NOT_FOUND, "device_not_found"))?;
    if !target.get::<bool, _>("active") {
        return Err(ApiError(StatusCode::CONFLICT, "device_already_revoked"));
    }
    if target.get::<String, _>("role") == "owner" {
        let owners: i64 = sqlx::query_scalar("SELECT count(*) FROM devices WHERE vault_id=$1 AND role='owner' AND revoked_at IS NULL")
            .bind(vault_id).fetch_one(&mut *tx).await.map_err(|error| database_unavailable(&error, "instance_revoke_owner_count"))?;
        if owners <= 1 {
            return Err(ApiError(StatusCode::CONFLICT, "last_active_owner"));
        }
    }
    sqlx::query("UPDATE devices SET revoked_at=now() WHERE vault_id=$1 AND device_id=$2")
        .bind(vault_id)
        .bind(device_id)
        .execute(&mut *tx)
        .await
        .map_err(|error| database_unavailable(&error, "instance_revoke_device"))?;
    sqlx::query(
        "UPDATE device_credentials SET revoked_at=now() WHERE vault_id=$1 AND device_id=$2",
    )
    .bind(vault_id)
    .bind(device_id)
    .execute(&mut *tx)
    .await
    .map_err(|error| database_unavailable(&error, "instance_revoke_credential"))?;
    tx.commit()
        .await
        .map_err(|error| database_unavailable(&error, "instance_revoke_commit"))?;
    crate::presence::revoke_best_effort(&state.db, vault_id, device_id).await;
    let _ = state.presence_changed.send(vault_id);
    Ok(StatusCode::NO_CONTENT)
}

pub(super) fn database_unavailable(error: &sqlx::Error, operation: &'static str) -> ApiError {
    tracing::warn!(operation, error_kind = %error, "database operation failed");
    ApiError(StatusCode::SERVICE_UNAVAILABLE, "database_unavailable")
}

fn insert_error(error: sqlx::Error, operation: &'static str) -> ApiError {
    unique_conflict(error, "idempotency_conflict", operation)
}

/// A unique violation is a client conflict with `code`; anything else is a
/// logged transient database failure.
pub(super) fn unique_conflict(
    error: sqlx::Error,
    code: &'static str,
    operation: &'static str,
) -> ApiError {
    if error
        .as_database_error()
        .and_then(|database| database.code())
        .is_some_and(|code| code == "23505")
    {
        ApiError(StatusCode::CONFLICT, code)
    } else {
        database_unavailable(&error, operation)
    }
}

#[derive(Clone)]
pub(super) struct Principal {
    vault: Uuid,
    device: Uuid,
    role: String,
    token_digest: Vec<u8>,
}

fn access_principal(principal: &Principal) -> AccessPrincipal {
    AccessPrincipal {
        vault_id: principal.vault,
        device_id: principal.device,
        role: principal.role.clone(),
    }
}

pub(super) async fn authorize(
    s: &ApiState,
    principal: &Principal,
    operation: Operation,
) -> ApiResult<()> {
    if s.policy
        .authorize(access_principal(principal), operation)
        .await
    {
        Ok(())
    } else {
        Err(ApiError(StatusCode::FORBIDDEN, "operation_not_permitted"))
    }
}

pub(super) async fn attachment_quota(s: &ApiState, principal: &Principal) -> Option<i64> {
    s.policy
        .attachment_quota_bytes(access_principal(principal))
        .await
}

pub(super) async fn authorize_resolved(
    s: &ApiState,
    vault: Uuid,
    device: Uuid,
    operation: Operation,
) -> ApiResult<()> {
    if s.policy
        .authorize(
            AccessPrincipal {
                vault_id: vault,
                device_id: device,
                role: "public".into(),
            },
            operation,
        )
        .await
    {
        Ok(())
    } else {
        Err(ApiError(StatusCode::FORBIDDEN, "operation_not_permitted"))
    }
}

async fn authenticated(
    s: &ApiState,
    headers: &HeaderMap,
    operation: Operation,
) -> ApiResult<Principal> {
    let principal = auth_with_mode(s, headers).await?;
    authorize(s, &principal, operation).await?;
    if reserve_presence_touch(&s.presence_touches, principal.vault, principal.device) {
        crate::presence::touch_best_effort(&s.db, principal.vault, principal.device).await;
    }
    Ok(principal)
}

const PRESENCE_TOUCH_CACHE_LIMIT: usize = 1_024;
const PRESENCE_TOUCH_CACHE_TTL: Duration = Duration::from_secs(60);

fn reserve_presence_touch(
    touches: &std::sync::Mutex<HashMap<(Uuid, Uuid), std::time::Instant>>,
    vault_id: Uuid,
    device_id: Uuid,
) -> bool {
    let now = std::time::Instant::now();
    let mut touches = touches.lock().expect("presence touch cache lock poisoned");
    touches.retain(|_, seen| now.duration_since(*seen) <= PRESENCE_TOUCH_CACHE_TTL);
    if touches.get(&(vault_id, device_id)).is_some_and(|seen| {
        now.duration_since(*seen) < Duration::from_secs(crate::presence::TOUCH_SECONDS as u64)
    }) {
        return false;
    }
    if touches.len() >= PRESENCE_TOUCH_CACHE_LIMIT {
        touches.clear();
    }
    touches.insert((vault_id, device_id), now);
    true
}
async fn auth_direct(db: &PgPool, headers: &HeaderMap) -> ApiResult<Principal> {
    let bearer = headers
        .get("authorization")
        .and_then(|h| h.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .ok_or(ApiError(StatusCode::UNAUTHORIZED, "missing_bearer"))?;
    if bearer.len() != TOKEN_BYTES_HEX || !bearer.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(ApiError(StatusCode::UNAUTHORIZED, "invalid_bearer"));
    }
    let digest = Sha256::digest(bearer.as_bytes());
    let row = sqlx::query("SELECT c.vault_id,c.device_id,d.role,c.token_digest FROM device_credentials c JOIN devices d ON d.vault_id=c.vault_id AND d.device_id=c.device_id WHERE c.token_digest=$1 AND c.revoked_at IS NULL AND d.revoked_at IS NULL")
        .bind(digest.as_slice()).fetch_optional(db).await.map_err(|error| database_unavailable(&error, "api_query"))?.ok_or(ApiError(StatusCode::UNAUTHORIZED,"invalid_bearer"))?;
    Ok(Principal {
        vault: row.get("vault_id"),
        device: row.get("device_id"),
        role: row.get("role"),
        token_digest: row.get("token_digest"),
    })
}

pub(super) async fn auth_with_mode(s: &ApiState, headers: &HeaderMap) -> ApiResult<Principal> {
    let bearer = headers
        .get("authorization")
        .and_then(|h| h.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .ok_or(ApiError(StatusCode::UNAUTHORIZED, "missing_bearer"))?;
    if !s.row_security {
        return auth_direct(&s.db, headers).await;
    }
    if bearer.len() != TOKEN_BYTES_HEX || !bearer.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(ApiError(StatusCode::UNAUTHORIZED, "invalid_bearer"));
    }
    let digest = Sha256::digest(bearer.as_bytes());
    let row = sqlx::query(
        "SELECT vault_id,device_id,role,token_digest FROM peppy.resolve_device_credential($1)",
    )
    .bind(digest.as_slice())
    .fetch_optional(&s.db)
    .await
    .map_err(|error| database_unavailable(&error, "credential_resolve"))?
    .ok_or(ApiError(StatusCode::UNAUTHORIZED, "invalid_bearer"))?;
    Ok(Principal {
        vault: row.get("vault_id"),
        device: row.get("device_id"),
        role: row.get("role"),
        token_digest: row.get("token_digest"),
    })
}

/// Re-check the exact bearer credential after serializing on the vault row.
/// The initial HTTP authentication can otherwise become stale while waiting for
/// a concurrent revocation/delete transaction to release that lock.
async fn revalidate_principal(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    principal: &Principal,
) -> ApiResult<String> {
    sqlx::query_scalar("SELECT d.role FROM device_credentials c JOIN devices d ON d.vault_id=c.vault_id AND d.device_id=c.device_id WHERE c.token_digest=$1 AND c.vault_id=$2 AND c.device_id=$3 AND c.revoked_at IS NULL AND d.revoked_at IS NULL")
        .bind(&principal.token_digest)
        .bind(principal.vault)
        .bind(principal.device)
        .fetch_optional(&mut **tx)
        .await
        .map_err(|error| database_unavailable(&error, "principal_revalidate"))?
        .ok_or(ApiError(StatusCode::UNAUTHORIZED, "invalid_bearer"))
}
fn owner(p: &Principal) -> ApiResult<()> {
    if p.role == "owner" {
        Ok(())
    } else {
        Err(ApiError(StatusCode::FORBIDDEN, "owner_required"))
    }
}
fn credential_token() -> String {
    (0..3)
        .map(|_| Uuid::new_v4().simple().to_string())
        .collect()
}
fn profile_ok(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
pub(super) fn validate_profile(
    profile: &Value,
    fingerprint: &str,
    epoch: u32,
) -> Result<Uuid, &'static str> {
    if !profile_ok(fingerprint) || epoch == 0 {
        return Err("invalid profile fingerprint or epoch");
    }
    let object = profile
        .as_object()
        .ok_or("public key profile must be an object")?;
    if object.get("crypto_suite").and_then(Value::as_u64) != Some(1) {
        return Err("unsupported crypto suite");
    }
    if object.get("key_epoch").and_then(Value::as_u64) != Some(epoch.into()) {
        return Err("profile epoch mismatch");
    }
    let vault: Uuid = object
        .get("vault_id")
        .and_then(Value::as_str)
        .ok_or("profile vault id missing")?
        .parse()
        .map_err(|_| "invalid profile vault id")?;
    let salt = object
        .get("salt")
        .and_then(Value::as_array)
        .ok_or("profile salt missing")?;
    if salt.len() != 16 || salt.iter().any(|v| v.as_u64().is_none_or(|b| b > 255)) {
        return Err("invalid profile salt");
    }
    let mut digest = Sha256::new();
    digest.update(b"peppy-key-profile-v1\0");
    digest.update(1u16.to_be_bytes());
    for byte in salt {
        digest.update([byte.as_u64().expect("validated") as u8]);
    }
    digest.update(vault.as_bytes());
    digest.update(epoch.to_be_bytes());
    if hex::encode(digest.finalize()) != fingerprint {
        return Err("profile fingerprint mismatch");
    }
    Ok(vault)
}
#[derive(Serialize)]
pub struct Credential {
    pub device_token: String,
    pub vault_id: Uuid,
    pub device_id: Uuid,
    pub role: String,
}

/// Test fixture helper for creating an owner with nonsecret crypto metadata.
/// The server never receives or derives a passphrase.
pub async fn create_owner(
    db: &PgPool,
    public_key_profile: Value,
    check_header: Vec<u8>,
    profile_fingerprint: String,
    key_epoch: u32,
) -> Result<Credential, String> {
    let vault = validate_profile(&public_key_profile, &profile_fingerprint, key_epoch)
        .map_err(str::to_owned)?;
    let device = Uuid::new_v4();
    let t = credential_token();
    let mut tx = scope::begin(db, vault).await.map_err(|e| e.to_string())?;
    bootstrap_owner(
        &mut tx,
        OwnerBootstrap {
            vault_id: vault,
            device_id: device,
            public_key_profile,
            encrypted_vault_check_header: check_header,
            profile_fingerprint: profile_fingerprint.clone(),
            key_epoch,
            credential_digest: Sha256::digest(t.as_bytes()).into(),
        },
    )
    .await?;
    tx.commit().await.map_err(|e| e.to_string())?;
    Ok(Credential {
        device_token: t,
        vault_id: vault,
        device_id: device,
        role: "owner".into(),
    })
}

#[derive(Serialize)]
struct VaultResponse {
    vault_id: Uuid,
    public_key_profile: Value,
    encrypted_vault_check_header: String,
    key_epoch: u32,
    profile_fingerprint: String,
    device_id: Uuid,
    role: String,
}
async fn vault_header(State(s): State<ApiState>, h: HeaderMap) -> ApiResult<Json<VaultResponse>> {
    let p = authenticated(&s, &h, Operation::Read).await?;
    let mut tx = scope::begin(&s.db, p.vault)
        .await
        .map_err(|error| database_unavailable(&error, "vault_header_begin"))?;
    // Encoded here: PostgreSQL's encode(...,'base64') inserts a newline every 76 characters.
    let r=sqlx::query("SELECT vault_id,public_key_profile,encrypted_vault_check_header,key_epoch,profile_fingerprint FROM vaults WHERE vault_id=$1").bind(p.vault).fetch_one(&mut *tx).await.map_err(|error| database_unavailable(&error, "vault_header"))?;
    Ok(Json(VaultResponse {
        vault_id: r.get("vault_id"),
        public_key_profile: r.get("public_key_profile"),
        encrypted_vault_check_header: STANDARD
            .encode(r.get::<Vec<u8>, _>("encrypted_vault_check_header")),
        key_epoch: r.get::<i32, _>("key_epoch") as u32,
        profile_fingerprint: r.get("profile_fingerprint"),
        device_id: p.device,
        role: p.role,
    }))
}

const INSTANCE_ACCOUNT_SOURCE_LIMIT: u8 = 10;
const INSTANCE_ACCOUNT_GLOBAL_LIMIT: u16 = 100;
const INSTANCE_ACCOUNT_ADMISSION_WINDOW: Duration = Duration::from_secs(60);

pub(super) async fn instance_account_admission(
    state: &ApiState,
    peer: Option<IpAddr>,
    operation: &[u8],
    code: &'static str,
) -> ApiResult<()> {
    let now = SystemTime::now();
    let mut admissions = state.instance_account_admissions.lock().await;
    if now.duration_since(admissions.window).unwrap_or_default()
        >= INSTANCE_ACCOUNT_ADMISSION_WINDOW
    {
        admissions.window = now;
        admissions.count = 0;
        admissions.sources.clear();
    }
    let mut source_hasher = Sha256::new();
    source_hasher.update(b"instance-account-v1:");
    source_hasher.update(operation);
    match peer {
        Some(IpAddr::V4(address)) => source_hasher.update(address.octets()),
        Some(IpAddr::V6(address)) => source_hasher.update(address.octets()),
        None => source_hasher.update(b"global-fallback"),
    }
    let source = source_hasher.finalize().into();
    if admissions.count >= INSTANCE_ACCOUNT_GLOBAL_LIMIT {
        return Err(ApiError(StatusCode::TOO_MANY_REQUESTS, code));
    }
    let source_admission = admissions.sources.entry(source).or_insert(RateAdmission {
        window: now,
        count: 0,
    });
    if now
        .duration_since(source_admission.window)
        .unwrap_or_default()
        >= INSTANCE_ACCOUNT_ADMISSION_WINDOW
    {
        *source_admission = RateAdmission {
            window: now,
            count: 0,
        };
    }
    if source_admission.count >= INSTANCE_ACCOUNT_SOURCE_LIMIT {
        return Err(ApiError(StatusCode::TOO_MANY_REQUESTS, code));
    }
    source_admission.count += 1;
    admissions.count += 1;
    Ok(())
}

/// Validated, caller-selected material for an atomic owner bootstrap. Hosted
/// provisioning supplies a client-held credential digest and keeps its grant
/// consumption and account link in the same transaction. This function never
/// commits the transaction and never receives the replayable credential.
pub struct OwnerBootstrap {
    pub vault_id: Uuid,
    pub device_id: Uuid,
    pub public_key_profile: Value,
    pub encrypted_vault_check_header: Vec<u8>,
    pub profile_fingerprint: String,
    pub key_epoch: u32,
    pub credential_digest: [u8; 32],
}

pub async fn bootstrap_owner(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    bootstrap: OwnerBootstrap,
) -> Result<(), String> {
    let profile_vault = validate_profile(
        &bootstrap.public_key_profile,
        &bootstrap.profile_fingerprint,
        bootstrap.key_epoch,
    )
    .map_err(str::to_owned)?;
    if profile_vault != bootstrap.vault_id || bootstrap.device_id.is_nil() {
        return Err("bootstrap vault or device id is invalid".into());
    }
    if bootstrap.encrypted_vault_check_header.is_empty()
        || bootstrap.encrypted_vault_check_header.len() > 1_048_576
    {
        return Err("invalid encrypted vault check header".into());
    }
    scope::set(tx, bootstrap.vault_id)
        .await
        .map_err(|error| error.to_string())?;
    sqlx::query("INSERT INTO vaults(vault_id,public_key_profile,encrypted_vault_check_header,key_epoch,profile_fingerprint) VALUES($1,$2,$3,$4,$5)")
        .bind(bootstrap.vault_id).bind(&bootstrap.public_key_profile).bind(&bootstrap.encrypted_vault_check_header).bind(bootstrap.key_epoch as i32).bind(&bootstrap.profile_fingerprint).execute(&mut **tx).await.map_err(|e|e.to_string())?;
    sqlx::query("INSERT INTO vault_key_profiles(vault_id,key_epoch,public_key_profile,encrypted_vault_check_header,profile_fingerprint,created_by_device_id,activated_at) SELECT vault_id,key_epoch,public_key_profile,encrypted_vault_check_header,profile_fingerprint,$2,now() FROM vaults WHERE vault_id=$1")
        .bind(bootstrap.vault_id).bind(bootstrap.device_id).execute(&mut **tx).await.map_err(|e|e.to_string())?;
    sqlx::query("INSERT INTO devices(vault_id,device_id,role,public_key,profile_fingerprint,key_epoch) VALUES($1,$2,'owner',$3,$4,$5)")
        .bind(bootstrap.vault_id).bind(bootstrap.device_id).bind(&bootstrap.public_key_profile).bind(&bootstrap.profile_fingerprint).bind(bootstrap.key_epoch as i32).execute(&mut **tx).await.map_err(|e|e.to_string())?;
    sqlx::query("INSERT INTO device_credentials(token_digest,vault_id,device_id) VALUES($1,$2,$3)")
        .bind(bootstrap.credential_digest.as_slice())
        .bind(bootstrap.vault_id)
        .bind(bootstrap.device_id)
        .execute(&mut **tx)
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}
async fn revoke_device(
    State(s): State<ApiState>,
    h: HeaderMap,
    Path(device_id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    let p = authenticated(&s, &h, Operation::Revoke).await?;
    if device_id != p.device {
        owner(&p)?;
    }
    let mut tx = scope::begin(&s.db, p.vault)
        .await
        .map_err(|error| database_unavailable(&error, "revoke_begin"))?;
    // Serialize every roster change on the vault row.
    sqlx::query("SELECT 1 FROM vaults WHERE vault_id=$1 FOR UPDATE")
        .bind(p.vault)
        .execute(&mut *tx)
        .await
        .map_err(|error| database_unavailable(&error, "revoke_vault_lock"))?;
    let caller_role = revalidate_principal(&mut tx, &p).await?;
    if device_id != p.device && caller_role != "owner" {
        return Err(ApiError(StatusCode::FORBIDDEN, "owner_required"));
    }
    let target = sqlx::query("SELECT role,revoked_at IS NULL active FROM devices WHERE vault_id=$1 AND device_id=$2 FOR UPDATE").bind(p.vault).bind(device_id).fetch_optional(&mut *tx).await.map_err(|error| database_unavailable(&error, "revoke_target_lookup"))?.ok_or(ApiError(StatusCode::NOT_FOUND, "device_not_found"))?;
    if !target.get::<bool, _>("active") {
        return Err(ApiError(StatusCode::CONFLICT, "device_already_revoked"));
    }
    if target.get::<String, _>("role") == "owner" {
        let owners: i64 = sqlx::query_scalar("SELECT count(*) FROM devices WHERE vault_id=$1 AND role='owner' AND revoked_at IS NULL").bind(p.vault).fetch_one(&mut *tx).await.map_err(|error| database_unavailable(&error, "revoke_owner_count"))?;
        if owners <= 1 {
            return Err(ApiError(StatusCode::CONFLICT, "last_active_owner"));
        }
    }
    sqlx::query("UPDATE devices SET revoked_at=now() WHERE vault_id=$1 AND device_id=$2")
        .bind(p.vault)
        .bind(device_id)
        .execute(&mut *tx)
        .await
        .map_err(|error| database_unavailable(&error, "revoke_device"))?;
    sqlx::query(
        "UPDATE device_credentials SET revoked_at=now() WHERE vault_id=$1 AND device_id=$2",
    )
    .bind(p.vault)
    .bind(device_id)
    .execute(&mut *tx)
    .await
    .map_err(|error| database_unavailable(&error, "revoke_credential"))?;
    sqlx::query("UPDATE device_wake_routes SET revoked_at=now() WHERE vault_id=$1 AND device_id=$2 AND revoked_at IS NULL").bind(p.vault).bind(device_id).execute(&mut *tx).await.map_err(|error| database_unavailable(&error, "revoke_wake_route"))?;
    sqlx::query("DELETE FROM device_wake_jobs WHERE vault_id=$1 AND device_id=$2")
        .bind(p.vault)
        .bind(device_id)
        .execute(&mut *tx)
        .await
        .map_err(|error| database_unavailable(&error, "revoke_wake_job"))?;
    tx.commit()
        .await
        .map_err(|error| database_unavailable(&error, "revoke_commit"))?;
    crate::presence::revoke_best_effort(&s.db, p.vault, device_id).await;
    let _ = s.presence_changed.send(p.vault);
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WakeRouteRequest {
    route_id: Uuid,
    wake_credential: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WakeRouteDeleteQuery {
    route_id: Option<Uuid>,
}

async fn push_config(State(s): State<ApiState>, h: HeaderMap) -> ApiResult<Json<PushConfig>> {
    authenticated(&s, &h, Operation::Sync).await?;
    Ok(Json(s.push_config))
}

async fn register_wake_route(
    State(s): State<ApiState>,
    h: HeaderMap,
    Json(x): Json<WakeRouteRequest>,
) -> ApiResult<StatusCode> {
    let p = authenticated(&s, &h, Operation::Sync).await?;
    if x.route_id.is_nil()
        || x.wake_credential.len() != 64
        || !x.wake_credential.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return Err(ApiError(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_wake_route",
        ));
    }
    let mut tx = scope::begin(&s.db, p.vault)
        .await
        .map_err(|error| database_unavailable(&error, "wake_route_begin"))?;
    sqlx::query("SELECT 1 FROM vaults WHERE vault_id=$1 FOR UPDATE")
        .bind(p.vault)
        .execute(&mut *tx)
        .await
        .map_err(|error| database_unavailable(&error, "wake_route_vault_lock"))?;
    revalidate_principal(&mut tx, &p).await?;
    sqlx::query("INSERT INTO device_wake_routes(vault_id,device_id,route_id,wake_credential) VALUES($1,$2,$3,$4) ON CONFLICT(vault_id,device_id) DO UPDATE SET route_id=EXCLUDED.route_id,wake_credential=EXCLUDED.wake_credential,revoked_at=NULL,created_at=now(),generation=device_wake_routes.generation+1")
        .bind(p.vault).bind(p.device).bind(x.route_id).bind(x.wake_credential).execute(&mut *tx).await.map_err(|error| database_unavailable(&error, "wake_route_register"))?;
    tx.commit()
        .await
        .map_err(|error| database_unavailable(&error, "wake_route_commit"))?;
    Ok(StatusCode::NO_CONTENT)
}

async fn revoke_wake_route(
    State(s): State<ApiState>,
    h: HeaderMap,
    Query(query): Query<WakeRouteDeleteQuery>,
) -> ApiResult<StatusCode> {
    let p = authenticated(&s, &h, Operation::Revoke).await?;
    let mut tx = scope::begin(&s.db, p.vault)
        .await
        .map_err(|error| database_unavailable(&error, "wake_route_revoke_begin"))?;
    sqlx::query("SELECT 1 FROM vaults WHERE vault_id=$1 FOR UPDATE")
        .bind(p.vault)
        .execute(&mut *tx)
        .await
        .map_err(|error| database_unavailable(&error, "wake_route_revoke_vault_lock"))?;
    revalidate_principal(&mut tx, &p).await?;
    sqlx::query("WITH revoked AS (UPDATE device_wake_routes SET revoked_at=now() WHERE vault_id=$1 AND device_id=$2 AND revoked_at IS NULL AND ($3::uuid IS NULL OR route_id=$3) RETURNING vault_id,device_id) DELETE FROM device_wake_jobs jobs USING revoked WHERE jobs.vault_id=revoked.vault_id AND jobs.device_id=revoked.device_id")
        .bind(p.vault)
        .bind(p.device)
        .bind(query.route_id)
        .execute(&mut *tx)
        .await
        .map_err(|error| database_unavailable(&error, "wake_route_revoke"))?;
    tx.commit()
        .await
        .map_err(|error| database_unavailable(&error, "wake_route_revoke_commit"))?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct VaultDeleteRequest {
    vault_id: Uuid,
}

/// Deletes one vault and queues its objects. The caller supplies a scoped
/// transaction; this primitive takes the vault lock but never commits.
pub async fn delete_vault_in_transaction(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    vault_id: Uuid,
) -> Result<(), sqlx::Error> {
    // Hosted account deletion starts its own transaction. Scope it here rather
    // than relying on every caller to remember the RLS invariant.
    scope::set(tx, vault_id).await?;
    sqlx::query("SELECT 1 FROM vaults WHERE vault_id=$1 FOR UPDATE")
        .bind(vault_id)
        .execute(&mut **tx)
        .await?;
    sqlx::query("INSERT INTO storage_deletions(object_key,vault_id,reason) SELECT object_key,$1,'vault_deleted' FROM (SELECT object_key FROM upload_reservations WHERE vault_id=$1 UNION SELECT object_key FROM attachments WHERE vault_id=$1 UNION SELECT object_key FROM public_attachment_copies WHERE vault_id=$1) keys ON CONFLICT(object_key) DO UPDATE SET reason='vault_deleted',not_before=GREATEST(storage_deletions.not_before,now())")
        .bind(vault_id).execute(&mut **tx).await?;
    sqlx::query("SET LOCAL peppy.compaction = 'on'")
        .execute(&mut **tx)
        .await?;
    sqlx::query("SET LOCAL peppy.vault_delete = 'on'")
        .execute(&mut **tx)
        .await?;
    for table in [
        "device_wake_jobs",
        "device_wake_routes",
        "command_receipts",
        "commands",
        "device_cursors",
        "device_capabilities",
        "attachment_record_references",
        "record_supersessions",
        "record_compaction",
        "compacted_records",
        "event_log",
        "encrypted_records",
        "public_attachment_copies",
        "attachments",
        "upload_reservations",
        "outbox_jobs",
        "vault_key_profiles",
        "device_credentials",
        "devices",
    ] {
        sqlx::query(&format!("DELETE FROM {table} WHERE vault_id=$1"))
            .bind(vault_id)
            .execute(&mut **tx)
            .await?;
    }
    sqlx::query("DELETE FROM vaults WHERE vault_id=$1")
        .bind(vault_id)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

async fn delete_vault(
    State(s): State<ApiState>,
    h: HeaderMap,
    Json(x): Json<VaultDeleteRequest>,
) -> ApiResult<StatusCode> {
    let p = authenticated(&s, &h, Operation::DeleteVault).await?;
    owner(&p)?;
    if x.vault_id != p.vault {
        return Err(ApiError(StatusCode::CONFLICT, "vault_id_mismatch"));
    }
    let mut tx = scope::begin(&s.db, p.vault)
        .await
        .map_err(|error| database_unavailable(&error, "vault_delete_begin"))?;
    sqlx::query("SELECT 1 FROM vaults WHERE vault_id=$1 FOR UPDATE")
        .bind(p.vault)
        .execute(&mut *tx)
        .await
        .map_err(|error| database_unavailable(&error, "vault_delete_lock"))?;
    if revalidate_principal(&mut tx, &p).await? != "owner" {
        return Err(ApiError(StatusCode::FORBIDDEN, "owner_required"));
    }
    delete_vault_in_transaction(&mut tx, p.vault)
        .await
        .map_err(|error| database_unavailable(&error, "vault_delete"))?;
    tx.commit()
        .await
        .map_err(|error| database_unavailable(&error, "vault_delete_commit"))?;
    Ok(StatusCode::NO_CONTENT)
}

async fn devices(State(s): State<ApiState>, h: HeaderMap) -> ApiResult<Json<DeviceRosterResponse>> {
    let p = authenticated(&s, &h, Operation::Read).await?;
    let mut tx = scope::begin(&s.db, p.vault)
        .await
        .map_err(|error| database_unavailable(&error, "devices_begin"))?;
    let rows = sqlx::query("SELECT d.device_id,d.name,d.role,d.revoked_at IS NOT NULL revoked,d.kind,d.profile_fingerprint,d.key_epoch,EXISTS(SELECT 1 FROM device_wake_routes routes WHERE routes.vault_id=d.vault_id AND routes.device_id=d.device_id AND routes.revoked_at IS NULL) wake_route FROM devices d WHERE d.vault_id=$1 ORDER BY d.created_at")
        .bind(p.vault).fetch_all(&mut *tx).await.map_err(|error| database_unavailable(&error, "api_query"))?;
    tx.commit()
        .await
        .map_err(|error| database_unavailable(&error, "devices_commit"))?;
    let presence_by_device = crate::presence::lookup_many_best_effort(
        &s.db,
        p.vault,
        &rows
            .iter()
            .map(|row| (row.get("device_id"), row.get("revoked")))
            .collect::<Vec<_>>(),
    )
    .await;
    let mut devices = Vec::with_capacity(rows.len());
    for row in rows {
        let device_id = row.get("device_id");
        let revoked = row.get("revoked");
        let presence = presence_by_device.get(&device_id).cloned();
        let wake_route = row.get("wake_route");
        devices.push(DeviceRosterDevice {
            device_id,
            name: row.get("name"),
            role: row.get("role"),
            revoked,
            kind: row.get("kind"),
            profile_fingerprint: row.get("profile_fingerprint"),
            key_epoch: row.get::<i32, _>("key_epoch") as u32,
            presence,
            wake_route,
        });
    }
    Ok(Json(DeviceRosterResponse { devices }))
}

async fn capabilities(State(s): State<ApiState>, h: HeaderMap) -> ApiResult<Json<Value>> {
    let p = authenticated(&s, &h, Operation::Read).await?;
    let mut tx = scope::begin(&s.db, p.vault)
        .await
        .map_err(|error| database_unavailable(&error, "capabilities_begin"))?;
    let rows=sqlx::query("SELECT c.device_id,c.simulator,c.capabilities,c.updated_at FROM device_capabilities c JOIN devices d ON d.vault_id=c.vault_id AND d.device_id=c.device_id WHERE c.vault_id=$1 AND d.revoked_at IS NULL ORDER BY c.updated_at DESC").bind(p.vault).fetch_all(&mut *tx).await.map_err(|error| database_unavailable(&error, "api_query"))?;
    Ok(Json(
        json!({"capabilities":rows.into_iter().map(|r|json!({"device_id":r.get::<Uuid,_>("device_id"),"simulator":r.get::<bool,_>("simulator"),"capabilities":r.get::<Value,_>("capabilities")})).collect::<Vec<_>>()}),
    ))
}
#[derive(Deserialize)]
struct CapabilityUpdate {
    simulator: bool,
    capabilities: Value,
}
async fn update_capabilities(
    State(s): State<ApiState>,
    h: HeaderMap,
    Json(x): Json<CapabilityUpdate>,
) -> ApiResult<StatusCode> {
    let p = authenticated(&s, &h, Operation::Publish).await?;
    if p.role != "owner" && p.role != "gateway" {
        return Err(ApiError(StatusCode::FORBIDDEN, "gateway_required"));
    }
    let mut tx = scope::begin(&s.db, p.vault)
        .await
        .map_err(|error| database_unavailable(&error, "capability_update_begin"))?;
    sqlx::query("INSERT INTO device_capabilities(vault_id,device_id,simulator,capabilities) VALUES($1,$2,$3,$4) ON CONFLICT(vault_id,device_id) DO UPDATE SET simulator=EXCLUDED.simulator,capabilities=EXCLUDED.capabilities,updated_at=now()")
        .bind(p.vault).bind(p.device).bind(x.simulator).bind(x.capabilities).execute(&mut *tx).await.map_err(|error| database_unavailable(&error, "api_query"))?;
    tx.commit()
        .await
        .map_err(|error| database_unavailable(&error, "capability_update_commit"))?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Serialize)]
struct Accepted {
    cursor: String,
    duplicate: bool,
}
/// Explicit opt-in is required from every non-revoked consumer before this
/// vault can delete immutable snapshot rows.
async fn register_compaction_capability(
    State(s): State<ApiState>,
    h: HeaderMap,
) -> ApiResult<StatusCode> {
    let p = authenticated(&s, &h, Operation::Publish).await?;
    let mut tx = scope::begin(&s.db, p.vault)
        .await
        .map_err(|error| database_unavailable(&error, "compaction_capability_begin"))?;
    sqlx::query("UPDATE devices SET compaction_generation_fence=TRUE WHERE vault_id=$1 AND device_id=$2 AND revoked_at IS NULL")
        .bind(p.vault).bind(p.device).execute(&mut *tx).await
        .map_err(|error| database_unavailable(&error, "compaction_capability_register"))?;
    tx.commit()
        .await
        .map_err(|error| database_unavailable(&error, "compaction_capability_commit"))?;
    Ok(StatusCode::NO_CONTENT)
}
async fn ingest(
    State(s): State<ApiState>,
    h: HeaderMap,
    Json(e): Json<Envelope>,
) -> ApiResult<Json<Accepted>> {
    let p = authenticated(&s, &h, Operation::Send).await?;
    if e.vault_id.0 != p.vault || e.producer_device_id.0 != p.device {
        return Err(ApiError(StatusCode::FORBIDDEN, "producer_not_authorized"));
    }
    e.validate()
        .map_err(|_| ApiError(StatusCode::UNPROCESSABLE_ENTITY, "invalid_envelope"))?;
    let digest = e
        .wire_digest()
        .map_err(|_| ApiError(StatusCode::UNPROCESSABLE_ENTITY, "invalid_envelope"))?;
    let mut tx = scope::begin(&s.db, p.vault)
        .await
        .map_err(|error| database_unavailable(&error, "ingest_begin"))?;
    // Lock first: cursor allocation, duplicate detection and key-epoch cutover
    // are all serialized per vault, so concurrent identical retries resolve to
    // `duplicate:true` rather than a unique-violation conflict.
    let vault_meta = sqlx::query(
        "SELECT key_epoch,profile_fingerprint FROM vaults WHERE vault_id=$1 FOR UPDATE",
    )
    .bind(p.vault)
    .fetch_one(&mut *tx)
    .await
    .map_err(|error| database_unavailable(&error, "ingest_vault_lock"))?;
    // Identity lives in the immutable records, which outlive transport-log
    // pruning. A retry stays observable after a key epoch or route changes.
    let compacted=sqlx::query("SELECT original_cursor,cipher_digest FROM compacted_records WHERE vault_id=$1 AND producer_device_id=$2 AND (producer_sequence=$3 OR envelope_id=$4 OR (command_id IS NOT NULL AND command_id=$5)) LIMIT 1").bind(p.vault).bind(p.device).bind(e.producer_sequence.0 as i64).bind(e.envelope_id.0).bind(e.command_id.map(|v|v.0)).fetch_optional(&mut *tx).await.map_err(|error| database_unavailable(&error, "ingest_compacted_duplicate_lookup"))?;
    if let Some(row) = compacted {
        if row.get::<Vec<u8>, _>("cipher_digest") == digest {
            return Ok(Json(Accepted {
                cursor: row.get::<i64, _>("original_cursor").to_string(),
                duplicate: true,
            }));
        }
        return Err(ApiError(StatusCode::CONFLICT, "idempotency_conflict"));
    }
    let existing=sqlx::query("SELECT cursor,cipher_digest FROM encrypted_records WHERE vault_id=$1 AND producer_device_id=$2 AND (producer_sequence=$3 OR envelope_id=$4 OR (command_id IS NOT NULL AND command_id=$5)) LIMIT 1").bind(p.vault).bind(p.device).bind(e.producer_sequence.0 as i64).bind(e.envelope_id.0).bind(e.command_id.map(|v|v.0)).fetch_optional(&mut *tx).await.map_err(|error| database_unavailable(&error, "ingest_duplicate_lookup"))?;
    if let Some(row) = existing {
        if row.get::<Vec<u8>, _>("cipher_digest") == digest {
            let c: i64 = row.get("cursor");
            return Ok(Json(Accepted {
                cursor: c.to_string(),
                duplicate: true,
            }));
        }
        return Err(ApiError(StatusCode::CONFLICT, "idempotency_conflict"));
    }
    let current_epoch = vault_meta.get::<i32, _>("key_epoch") as u32;
    let current_fingerprint: String = vault_meta.get("profile_fingerprint");
    match e.purpose {
        // New carrier commands are only accepted under the active epoch.
        EnvelopePurpose::Command => {
            if current_epoch != e.key_epoch || current_fingerprint != e.profile_fingerprint {
                return Err(ApiError(StatusCode::CONFLICT, "retired_key_epoch"));
            }
        }
        // Events may lag a manual rotation but must name an activated profile.
        EnvelopePurpose::Event => {
            let known: Option<i32> = sqlx::query_scalar("SELECT 1 FROM vault_key_profiles WHERE vault_id=$1 AND key_epoch=$2 AND profile_fingerprint=$3 AND activated_at IS NOT NULL")
                .bind(p.vault)
                .bind(i32::try_from(e.key_epoch).unwrap_or(-1))
                .bind(&e.profile_fingerprint)
                .fetch_optional(&mut *tx)
                .await
                .map_err(|error| database_unavailable(&error, "ingest_key_profile"))?;
            if known.is_none() {
                return Err(ApiError(StatusCode::CONFLICT, "key_epoch_not_active"));
            }
        }
    }
    if let Some(route) = &e.route {
        let gateway:Option<i32>=sqlx::query_scalar("SELECT 1 FROM devices d WHERE d.vault_id=$1 AND d.device_id=$2 AND d.revoked_at IS NULL AND (d.role='gateway' OR (d.role='owner' AND EXISTS (SELECT 1 FROM device_capabilities c WHERE c.vault_id=d.vault_id AND c.device_id=d.device_id)))").bind(p.vault).bind(route.gateway_device_id.0).fetch_optional(&mut *tx).await.map_err(|error| database_unavailable(&error, "ingest_route_lookup"))?;
        if gateway.is_none() {
            return Err(ApiError(StatusCode::FORBIDDEN, "gateway_not_authorized"));
        }
    }
    let c: i64 = sqlx::query_scalar(
        "UPDATE vaults SET next_cursor=next_cursor+1 WHERE vault_id=$1 RETURNING next_cursor",
    )
    .bind(p.vault)
    .fetch_one(&mut *tx)
    .await
    .map_err(|error| database_unavailable(&error, "ingest_cursor"))?;
    let payload = serde_json::to_value(&e)
        .map_err(|_| ApiError(StatusCode::UNPROCESSABLE_ENTITY, "invalid_envelope"))?;
    let purpose = match e.purpose {
        EnvelopePurpose::Event => "event",
        EnvelopePurpose::Command => "command",
    };
    sqlx::query("INSERT INTO event_log(vault_id,cursor,envelope_id,producer_device_id,producer_sequence,purpose,command_id,cipher_digest,envelope) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9)").bind(p.vault).bind(c).bind(e.envelope_id.0).bind(p.device).bind(e.producer_sequence.0 as i64).bind(purpose).bind(e.command_id.map(|v|v.0)).bind(digest.as_slice()).bind(&payload).execute(&mut *tx).await.map_err(|error| insert_error(error, "ingest_event_insert"))?;
    sqlx::query("INSERT INTO encrypted_records(vault_id,producer_device_id,envelope_id,cursor,producer_sequence,purpose,command_id,cipher_digest,envelope) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9)")
        .bind(p.vault)
        .bind(p.device)
        .bind(e.envelope_id.0)
        .bind(c)
        .bind(e.producer_sequence.0 as i64)
        .bind(purpose)
        .bind(e.command_id.map(|v| v.0))
        .bind(digest.as_slice())
        .bind(&payload)
        .execute(&mut *tx)
        .await
        .map_err(|error| insert_error(error, "ingest_record_insert"))?;
    // Compaction is opaque bookkeeping only. The envelope digest intentionally
    // excludes it so a retry against an older server remains idempotent; the
    // first accepted arrival is consequently authoritative for this metadata.
    if let Some(compaction) = &e.compaction {
        sqlx::query("INSERT INTO record_compaction(vault_id,cursor,compaction_key,terminal) VALUES($1,$2,$3,$4)")
            .bind(p.vault)
            .bind(c)
            .bind(&compaction.key)
            .bind(compaction.terminal)
            .execute(&mut *tx)
            .await
            .map_err(|error| insert_error(error, "ingest_compaction_insert"))?;
        if !compaction.supersedes.is_empty() {
            let target_devices: Vec<_> = compaction
                .supersedes
                .iter()
                .map(|reference| reference.producer_device_id.0)
                .collect();
            let target_sequences: Vec<_> = compaction
                .supersedes
                .iter()
                .map(|reference| reference.producer_sequence.0 as i64)
                .collect();
            sqlx::query("INSERT INTO record_supersessions(vault_id,by_cursor,target_producer_device_id,target_producer_sequence) SELECT $1,$2,target_device,target_sequence FROM UNNEST($3::uuid[],$4::bigint[]) AS targets(target_device,target_sequence)")
                .bind(p.vault)
                .bind(c)
                .bind(target_devices)
                .bind(target_sequences)
                .execute(&mut *tx)
                .await
                .map_err(|error| insert_error(error, "ingest_supersession_insert"))?;
        }
    }
    if let Some(id) = e.command_id {
        let gateway = e
            .route
            .as_ref()
            .expect("validated route")
            .gateway_device_id
            .0;
        sqlx::query("INSERT INTO commands(vault_id,producer_device_id,command_id,gateway_device_id,cipher_digest,cursor) VALUES($1,$2,$3,$4,$5,$6)").bind(p.vault).bind(p.device).bind(id.0).bind(gateway).bind(digest.as_slice()).bind(c).execute(&mut *tx).await.map_err(|error| insert_error(error, "ingest_command_insert"))?;
    }
    // A lightweight reference only; the envelope stays in the log/records.
    sqlx::query("INSERT INTO outbox_jobs(vault_id,cursor,kind) VALUES($1,$2,'sync')")
        .bind(p.vault)
        .bind(c)
        .execute(&mut *tx)
        .await
        .map_err(|error| database_unavailable(&error, "ingest_outbox_insert"))?;
    // Wake routes are device-scoped and coalesced; ciphertext remains opaque and
    // no event type (including contacts) is inspected here.
    sqlx::query("INSERT INTO device_wake_jobs(vault_id,device_id,cursor,job_id,opaque_nonce) SELECT r.vault_id,r.device_id,$2,gen_random_uuid(),encode(uuid_send(gen_random_uuid()) || uuid_send(gen_random_uuid()),'base64') FROM device_wake_routes r JOIN devices d ON d.vault_id=r.vault_id AND d.device_id=r.device_id WHERE r.vault_id=$1 AND r.revoked_at IS NULL AND d.revoked_at IS NULL AND r.device_id<>$3 ON CONFLICT(vault_id,device_id) DO UPDATE SET cursor=GREATEST(device_wake_jobs.cursor,EXCLUDED.cursor),job_id=CASE WHEN device_wake_jobs.cursor < EXCLUDED.cursor THEN EXCLUDED.job_id ELSE device_wake_jobs.job_id END,opaque_nonce=CASE WHEN device_wake_jobs.cursor < EXCLUDED.cursor THEN EXCLUDED.opaque_nonce ELSE device_wake_jobs.opaque_nonce END,attempts=CASE WHEN device_wake_jobs.cursor < EXCLUDED.cursor THEN 0 ELSE device_wake_jobs.attempts END,lease_token=CASE WHEN device_wake_jobs.cursor < EXCLUDED.cursor THEN NULL ELSE device_wake_jobs.lease_token END,lease_until=CASE WHEN device_wake_jobs.cursor < EXCLUDED.cursor THEN NULL ELSE device_wake_jobs.lease_until END,available_at=CASE WHEN device_wake_jobs.cursor < EXCLUDED.cursor THEN now() WHEN device_wake_jobs.lease_until IS NULL OR device_wake_jobs.lease_until <= now() THEN LEAST(device_wake_jobs.available_at,now()) ELSE device_wake_jobs.available_at END")
        .bind(p.vault).bind(c).bind(p.device).execute(&mut *tx).await.map_err(|error| database_unavailable(&error, "ingest_wake_enqueue"))?;
    tx.commit()
        .await
        .map_err(|error| database_unavailable(&error, "ingest_commit"))?;
    // Immediate latency hint; the outbox drainer republishes and sockets rescan.
    let _ = s.committed.send((p.vault, c));
    Ok(Json(Accepted {
        cursor: c.to_string(),
        duplicate: false,
    }))
}

/// The relay endpoint is an operator-fixed origin. This worker sends only the
/// route credential and opaque identifiers; it never handles provider or route
/// management credentials and it is entirely disabled when no relay is set.
const WAKE_BATCH_SIZE: i64 = 16;
const WAKE_LEASE_SECONDS: i64 = 30;
const WAKE_MAX_ATTEMPTS: i32 = 8;
const WAKE_MAX_AGE_HOURS: i64 = 24;

fn relay_wake_url(relay_url: &url::Url, route: Uuid) -> Result<url::Url, url::ParseError> {
    let mut base = relay_url.clone();
    if !base.path().ends_with('/') {
        base.set_path(&format!("{}/", base.path()));
    }
    base.join(&format!("v1/routes/{route}/wake"))
}

fn spawn_wake_maintenance(
    db: PgPool,
    hints: &tokio::sync::broadcast::Sender<(Uuid, i64)>,
    relay_url: url::Url,
) {
    let weak = hints.downgrade();
    tokio::spawn(async move {
        let client = match reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(3))
            .timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .build()
        {
            Ok(client) => client,
            Err(error) => {
                tracing::warn!(error = %error, "wake relay client unavailable");
                return;
            }
        };
        let mut timer = tokio::time::interval(Duration::from_millis(250));
        loop {
            timer.tick().await;
            if weak.strong_count() == 0 || db.is_closed() {
                return;
            }
            for _ in 0..WAKE_BATCH_SIZE {
                let lease = Uuid::new_v4();
                let row = sqlx::query("WITH due AS (SELECT j.vault_id,j.device_id FROM device_wake_jobs j JOIN device_wake_routes r ON r.vault_id=j.vault_id AND r.device_id=j.device_id WHERE j.available_at<=now() AND (j.lease_until IS NULL OR j.lease_until<=now()) AND r.revoked_at IS NULL ORDER BY j.available_at LIMIT 1 FOR UPDATE OF j SKIP LOCKED) UPDATE device_wake_jobs j SET lease_token=$1,lease_until=now()+($2 * interval '1 second'),last_attempt_at=now() FROM due JOIN device_wake_routes r ON r.vault_id=due.vault_id AND r.device_id=due.device_id WHERE j.vault_id=due.vault_id AND j.device_id=due.device_id RETURNING j.vault_id,j.device_id,j.cursor,j.job_id,j.opaque_nonce,j.attempts,j.created_at > now()-($3 * interval '1 hour') fresh,r.route_id,r.generation,r.wake_credential")
                    .bind(lease).bind(WAKE_LEASE_SECONDS).bind(WAKE_MAX_AGE_HOURS).fetch_optional(&db).await;
                let Ok(Some(row)) = row else {
                    break;
                };
                let vault: Uuid = row.get("vault_id");
                let device: Uuid = row.get("device_id");
                let cursor: i64 = row.get("cursor");
                let job_id: Uuid = row.get("job_id");
                let nonce: String = row.get("opaque_nonce");
                let attempts: i32 = row.get("attempts");
                let fresh: bool = row.get("fresh");
                let route: Uuid = row.get("route_id");
                let generation: i64 = row.get("generation");
                let credential: String = row.get("wake_credential");
                let url = match relay_wake_url(&relay_url, route) {
                    Ok(url) => url,
                    Err(error) => {
                        tracing::warn!(error = %error, "invalid wake relay URL");
                        break;
                    }
                };
                let response = client.post(url).json(&json!({"wake_credential":credential,"idempotency_id":job_id,"opaque_nonce":nonce})).send().await;
                let status = response.as_ref().ok().map(|value| value.status());
                if matches!(status, Some(StatusCode::ACCEPTED)) {
                    let _ = sqlx::query("DELETE FROM device_wake_jobs WHERE vault_id=$1 AND device_id=$2 AND job_id=$3 AND cursor=$4 AND lease_token=$5").bind(vault).bind(device).bind(job_id).bind(cursor).bind(lease).execute(&db).await;
                } else if matches!(status, Some(StatusCode::UNAUTHORIZED)) {
                    let revoked = sqlx::query("UPDATE device_wake_routes SET revoked_at=now() WHERE vault_id=$1 AND device_id=$2 AND route_id=$3 AND generation=$4 AND revoked_at IS NULL").bind(vault).bind(device).bind(route).bind(generation).execute(&db).await.map(|result| result.rows_affected()).unwrap_or(0);
                    if revoked == 1 {
                        let _ = sqlx::query("DELETE FROM device_wake_jobs WHERE vault_id=$1 AND device_id=$2 AND job_id=$3 AND cursor=$4 AND lease_token=$5").bind(vault).bind(device).bind(job_id).bind(cursor).bind(lease).execute(&db).await;
                    } else {
                        let _ = sqlx::query("UPDATE device_wake_jobs SET lease_token=NULL,lease_until=NULL,available_at=now() WHERE vault_id=$1 AND device_id=$2 AND job_id=$3 AND lease_token=$4").bind(vault).bind(device).bind(job_id).bind(lease).execute(&db).await;
                    }
                } else {
                    let permanent = status.is_some_and(|status| {
                        status.is_client_error() && status != StatusCode::TOO_MANY_REQUESTS
                    });
                    let terminal = permanent || attempts + 1 >= WAKE_MAX_ATTEMPTS || !fresh;
                    if terminal {
                        let _ = sqlx::query("DELETE FROM device_wake_jobs WHERE vault_id=$1 AND device_id=$2 AND job_id=$3 AND cursor=$4 AND lease_token=$5").bind(vault).bind(device).bind(job_id).bind(cursor).bind(lease).execute(&db).await;
                        tracing::warn!(status = ?status.map(|value| value.as_u16()), "wake relay delivery abandoned");
                    } else {
                        let delay = 2_i64.pow((attempts.max(0) as u32).min(8));
                        let _ = sqlx::query("UPDATE device_wake_jobs SET attempts=attempts+1,lease_token=NULL,lease_until=NULL,available_at=now()+($4 * interval '1 second') WHERE vault_id=$1 AND device_id=$2 AND job_id=$3 AND lease_token=$5").bind(vault).bind(device).bind(job_id).bind(delay).bind(lease).execute(&db).await;
                    }
                }
            }
        }
    });
}

#[derive(Deserialize)]
struct Receipt {
    receipt: Value,
}
async fn receipt(
    State(s): State<ApiState>,
    h: HeaderMap,
    Path(command_id): Path<Uuid>,
    Json(x): Json<Receipt>,
) -> ApiResult<StatusCode> {
    // Reporting an already-authorized command's outcome is sync bookkeeping,
    // not permission to send a new command. Keep it available in read-only mode.
    let p = authenticated(&s, &h, Operation::Sync).await?;
    if p.role != "owner" && p.role != "gateway" {
        return Err(ApiError(StatusCode::FORBIDDEN, "gateway_required"));
    }
    let mut tx = scope::begin(&s.db, p.vault)
        .await
        .map_err(|error| database_unavailable(&error, "receipt_begin"))?;
    let ok: Option<i32> = sqlx::query_scalar(
        "SELECT 1 FROM commands WHERE vault_id=$1 AND command_id=$2 AND gateway_device_id=$3",
    )
    .bind(p.vault)
    .bind(command_id)
    .bind(p.device)
    .fetch_optional(&mut *tx)
    .await
    .map_err(|error| database_unavailable(&error, "api_query"))?;
    if ok.is_none() {
        return Err(ApiError(StatusCode::FORBIDDEN, "receipt_not_targeted"));
    }
    sqlx::query("INSERT INTO command_receipts(vault_id,command_id,gateway_device_id,receipt) VALUES($1,$2,$3,$4) ON CONFLICT(vault_id,command_id,gateway_device_id) DO UPDATE SET receipt=EXCLUDED.receipt,created_at=now()").bind(p.vault).bind(command_id).bind(p.device).bind(x.receipt).execute(&mut *tx).await.map_err(|error| database_unavailable(&error, "api_query"))?;
    tx.commit()
        .await
        .map_err(|error| database_unavailable(&error, "receipt_commit"))?;
    Ok(StatusCode::NO_CONTENT)
}

/// WebSocket close codes. Clients reconnect and resume on 4429, take a
/// snapshot on 4409, stop on 4401, and upgrade on 4426.
mod close {
    pub const INVALID_HELLO: u16 = 4400;
    pub const REVOKED: u16 = 4401;
    pub const HELLO_TIMEOUT: u16 = 4408;
    pub const RESYNC_REQUIRED: u16 = 4409;
    pub const UNSUPPORTED_VERSION: u16 = 4426;
    pub const BACKPRESSURE: u16 = 4429;
    pub const SERVER_ERROR: u16 = 1011;
}
const WS_PROTOCOL_VERSION: u64 = 1;
const WS_REPLAY_PAGE: i64 = 100;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HeartbeatAction {
    SendChallenge,
    Disconnect,
}

fn heartbeat_action(has_outstanding_challenge: bool) -> HeartbeatAction {
    if has_outstanding_challenge {
        HeartbeatAction::Disconnect
    } else {
        HeartbeatAction::SendChallenge
    }
}

async fn websocket(
    State(s): State<ApiState>,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> ApiResult<axum::response::Response> {
    let principal = authenticated(&s, &headers, Operation::Sync).await?;
    // Subscribe before replay so a commit between replay and live is never missed.
    let receiver = s.committed.subscribe();
    let presence_receiver = s.presence_changed.subscribe();
    Ok(upgrade
        .max_message_size(1_100_000)
        .max_frame_size(1_100_000)
        .on_upgrade(move |socket| ws_session(socket, s, principal, receiver, presence_receiver))
        .into_response())
}

async fn ws_close(socket: &mut WebSocket, code: u16, reason: &'static str) {
    let _ = socket
        .send(Message::Close(Some(CloseFrame {
            code,
            reason: Utf8Bytes::from_static(reason),
        })))
        .await;
}

/// Why a socket stopped; mapped to one close code.
enum WsStop {
    PolicyDenied,
    Resync(sync::Resync),
    Backpressure,
    Database,
    Disconnected,
}

impl WsStop {
    async fn close(self, socket: &mut WebSocket) {
        match self {
            Self::PolicyDenied => ws_close(socket, 1008, "access_denied").await,
            Self::Resync(resync) => {
                let _ = socket.send(Message::Text(resync.ws_frame().into())).await;
                ws_close(socket, close::RESYNC_REQUIRED, "resync_required").await;
            }
            Self::Backpressure => ws_close(socket, close::BACKPRESSURE, "backpressure").await,
            Self::Database => ws_close(socket, close::SERVER_ERROR, "database_unavailable").await,
            Self::Disconnected => {}
        }
    }
}

enum HelloError {
    /// The client went away; there is nobody to send a close frame to.
    Disconnected,
    Rejected(u16, &'static str),
}

/// Reads the client hello within the configured timeout. Pings and pongs are
/// ignored; any other frame is a protocol error.
async fn read_hello(socket: &mut WebSocket, timeout: Duration) -> Result<i64, HelloError> {
    use HelloError::Rejected;
    let deadline = tokio::time::Instant::now() + timeout;
    let text = loop {
        match tokio::time::timeout_at(deadline, socket.recv()).await {
            Err(_) => return Err(Rejected(close::HELLO_TIMEOUT, "hello_timeout")),
            Ok(Some(Ok(Message::Text(text)))) => break text,
            Ok(Some(Ok(Message::Ping(_) | Message::Pong(_)))) => continue,
            Ok(Some(Ok(Message::Binary(_)))) => {
                return Err(Rejected(close::INVALID_HELLO, "invalid_hello"));
            }
            Ok(Some(Ok(Message::Close(_)) | Err(_)) | None) => {
                return Err(HelloError::Disconnected);
            }
        }
    };
    let hello: Value =
        serde_json::from_str(&text).map_err(|_| Rejected(close::INVALID_HELLO, "invalid_hello"))?;
    match hello.get("protocol_version").and_then(Value::as_u64) {
        Some(WS_PROTOCOL_VERSION) => {}
        Some(_) => {
            return Err(Rejected(
                close::UNSUPPORTED_VERSION,
                "unsupported_protocol_version",
            ));
        }
        None => return Err(Rejected(close::INVALID_HELLO, "invalid_hello")),
    }
    hello
        .get("resume_cursor")
        .and_then(Value::as_str)
        .and_then(|cursor| sync::parse_cursor(cursor).ok())
        .ok_or(Rejected(close::INVALID_HELLO, "invalid_hello"))
}

async fn ws_session(
    mut socket: WebSocket,
    state: ApiState,
    principal: Principal,
    mut notices: tokio::sync::broadcast::Receiver<(Uuid, i64)>,
    mut presence_notices: tokio::sync::broadcast::Receiver<Uuid>,
) {
    let options = Arc::clone(&state.options);
    let mut cursor = match read_hello(&mut socket, options.hello_timeout).await {
        Ok(cursor) => cursor,
        Err(HelloError::Disconnected) => return,
        Err(HelloError::Rejected(code, reason)) => {
            return ws_close(&mut socket, code, reason).await;
        }
    };
    if authorize(&state, &principal, Operation::Sync)
        .await
        .is_err()
    {
        return WsStop::PolicyDenied.close(&mut socket).await;
    }
    let (high_water, replay_floor) = match sync::position(&state.db, principal.vault, cursor).await
    {
        Ok(Ok(marks)) => marks,
        Ok(Err(resync)) => return WsStop::Resync(resync).close(&mut socket).await,
        Err(error) => {
            tracing::warn!(error_kind = %error, "websocket handshake position failed");
            return WsStop::Database.close(&mut socket).await;
        }
    };
    // Negotiation acknowledgment: the server's version, the accepted resume
    // point and current watermarks, then replay frames follow.
    let ready = json!({
        "type": "ready",
        "protocol_version": WS_PROTOCOL_VERSION,
        "resume_cursor": cursor.to_string(),
        "high_water_cursor": high_water.to_string(),
        "replay_floor_cursor": replay_floor.to_string(),
        "frame_types": ["ready", "event", "resync_required", "presence_changed"],
    });
    if let Err(stop) = send_frame(&mut socket, ready.to_string(), options.send_timeout).await {
        return stop.close(&mut socket).await;
    }
    if let Err(stop) = send_replay(&mut socket, &state, &principal, &mut cursor).await {
        return stop.close(&mut socket).await;
    }
    let connection_id = Uuid::new_v4();
    let presence_active = match scope::begin(&state.db, principal.vault).await {
        Ok(mut tx) => {
            match crate::presence::open(&mut tx, principal.vault, principal.device, connection_id)
                .await
            {
                Ok(()) => tx.commit().await.is_ok(),
                Err(error) => {
                    tracing::debug!(%error, "presence socket lease unavailable");
                    false
                }
            }
        }
        Err(error) => {
            tracing::debug!(%error, "presence socket scope unavailable");
            false
        }
    };
    // A pending row is intentionally not advertised; the first challenge Pong
    // performs the connected transition.
    let mut recheck = tokio::time::interval(Duration::from_secs(20));
    let mut challenge: Option<Vec<u8>> = None;
    // Broadcast is only a latency hint; committed rows are the durable source.
    let mut durable_replay = tokio::time::interval(options.durable_replay_interval);
    durable_replay.tick().await;
    loop {
        let stop = tokio::select! {
            _ = recheck.tick() => match still_active(&state, &principal).await {
                Ok(true) => {
                    if authorize(&state, &principal, Operation::Sync).await.is_err() {
                        WsStop::PolicyDenied
                    } else {
                        match heartbeat_action(challenge.is_some()) {
                            HeartbeatAction::Disconnect => WsStop::Disconnected,
                            HeartbeatAction::SendChallenge => {
                            let token = Uuid::new_v4().as_bytes().to_vec();
                            match tokio::time::timeout(options.send_timeout, socket.send(Message::Ping(token.clone().into()))).await {
                                Ok(Ok(())) => {
                                    challenge = Some(token);
                                    continue;
                                }
                                Ok(Err(_)) | Err(_) => WsStop::Disconnected,
                            }
                            }
                        }
                    }
                },
                Ok(false) => {
                    if presence_active { close_presence(&state, &principal, connection_id).await; }
                    return ws_close(&mut socket, close::REVOKED, "revoked").await
                },
                Err(error) => {
                    tracing::warn!(error_kind = %error, "websocket revocation recheck failed");
                    WsStop::Database
                }
            },
            _ = durable_replay.tick() => match send_replay(&mut socket, &state, &principal, &mut cursor).await {
                Ok(()) => continue,
                Err(stop) => stop,
            },
            incoming = socket.recv() => match incoming {
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => {
                    if presence_active { close_presence(&state, &principal, connection_id).await; }
                    return
                },
                Some(Ok(Message::Text(_))) | Some(Ok(Message::Binary(_))) => {
                    if presence_active { close_presence(&state, &principal, connection_id).await; }
                    return ws_close(&mut socket, close::INVALID_HELLO, "unexpected_client_frame").await;
                }
                Some(Ok(Message::Ping(_))) => continue,
                // Only a Pong after the authenticated session's control pings
                // proves the peer is responsive; client traffic alone is not a lease.
                Some(Ok(Message::Pong(payload))) => {
                    if challenge.as_ref().is_some_and(|token| token.as_slice() == payload.as_ref()) {
                        challenge = None;
                        if presence_active && let Ok(mut tx) = scope::begin(&state.db, principal.vault).await {
                            if crate::presence::renew(&mut tx, principal.vault, principal.device, connection_id)
                                .await
                                .unwrap_or(false)
                                && tx.commit().await.is_ok()
                            {
                                let _ = state.presence_changed.send(principal.vault);
                            }
                        }
                    }
                    continue
                },
            },
            notice = notices.recv() => match notice {
                // Hints at or below the delivered cursor are already satisfied.
                Ok((vault, hinted)) if vault == principal.vault && hinted > cursor => {
                    match send_replay(&mut socket, &state, &principal, &mut cursor).await {
                        Ok(()) => continue,
                        Err(stop) => stop,
                    }
                }
                Ok(_) => continue,
                // Missed hints are recovered from committed rows, not by
                // disconnecting every lagging socket at once.
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                    match send_replay(&mut socket, &state, &principal, &mut cursor).await {
                        Ok(()) => continue,
                        Err(stop) => stop,
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    if presence_active { close_presence(&state, &principal, connection_id).await; }
                    return
                },
            },
            presence = presence_notices.recv() => match presence {
                Ok(vault) if vault == principal.vault => {
                    if send_frame(&mut socket, json!({"type":"presence_changed"}).to_string(), options.send_timeout).await.is_err() {
                        WsStop::Disconnected
                    } else { continue }
                }
                Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    if presence_active { close_presence(&state, &principal, connection_id).await; }
                    return
                },
            },
        };
        if presence_active {
            close_presence(&state, &principal, connection_id).await;
        }
        return stop.close(&mut socket).await;
    }
}
async fn close_presence(state: &ApiState, principal: &Principal, connection_id: Uuid) {
    if crate::presence::close(&state.db, principal.vault, connection_id)
        .await
        .unwrap_or(false)
    {
        let _ = state.presence_changed.send(principal.vault);
    }
}
async fn still_active(state: &ApiState, p: &Principal) -> Result<bool, sqlx::Error> {
    let mut tx = scope::begin(&state.db, p.vault).await?;
    let active = sqlx::query_scalar::<_, i32>("SELECT 1 FROM devices d JOIN device_credentials c ON c.vault_id=d.vault_id AND c.device_id=d.device_id WHERE d.vault_id=$1 AND d.device_id=$2 AND c.token_digest=$3 AND d.revoked_at IS NULL AND c.revoked_at IS NULL")
        .bind(p.vault)
        .bind(p.device)
        .bind(&p.token_digest)
        .fetch_optional(&mut *tx)
        .await?
        .is_some();
    tx.commit().await?;
    Ok(active)
}
async fn send_frame(
    socket: &mut WebSocket,
    frame: String,
    timeout: Duration,
) -> Result<(), WsStop> {
    match tokio::time::timeout(timeout, socket.send(Message::Text(frame.into()))).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(_)) => Err(WsStop::Disconnected),
        Err(_) => Err(WsStop::Backpressure),
    }
}
/// Sends every retained committed row after `cursor`, page by page, from the
/// durable log. Any retention or rollback mismatch becomes a typed resync.
async fn send_replay(
    socket: &mut WebSocket,
    state: &ApiState,
    principal: &Principal,
    cursor: &mut i64,
) -> Result<(), WsStop> {
    loop {
        if authorize(state, principal, Operation::Sync).await.is_err() {
            return Err(WsStop::PolicyDenied);
        }
        let rows =
            match sync::replay_page(&state.db, principal.vault, *cursor, WS_REPLAY_PAGE).await {
                Ok(sync::ReplayPage::Rows { rows, .. }) => rows,
                Ok(sync::ReplayPage::Resync(resync)) => return Err(WsStop::Resync(resync)),
                Err(error) => {
                    tracing::warn!(error_kind = %error, "websocket replay failed");
                    return Err(WsStop::Database);
                }
            };
        if rows.is_empty() {
            return Ok(());
        }
        for record in rows {
            let next = record.cursor;
            let frame = serde_json::to_string(&sync::EventFrame::new(record))
                .map_err(|_| WsStop::Database)?;
            send_frame(socket, frame, state.options.send_timeout).await?;
            *cursor = next;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heartbeat_disconnects_before_replacing_an_unanswered_challenge() {
        assert_eq!(heartbeat_action(false), HeartbeatAction::SendChallenge);
        assert_eq!(heartbeat_action(true), HeartbeatAction::Disconnect);
    }

    #[test]
    fn touch_cache_throttles_one_device_without_throttling_another() {
        let touches = std::sync::Mutex::new(HashMap::new());
        let vault = Uuid::new_v4();
        let device = Uuid::new_v4();
        assert!(reserve_presence_touch(&touches, vault, device));
        assert!(!reserve_presence_touch(&touches, vault, device));
        assert!(reserve_presence_touch(&touches, vault, Uuid::new_v4()));
    }

    #[test]
    fn profile_requires_matching_vault_epoch_and_fingerprint() {
        let vault = Uuid::new_v4();
        let salt: Vec<Value> = (0_u8..16).map(|byte| json!(byte)).collect();
        let mut digest = Sha256::new();
        digest.update(b"peppy-key-profile-v1\0");
        digest.update(1_u16.to_be_bytes());
        for byte in 0_u8..16 {
            digest.update([byte]);
        }
        digest.update(vault.as_bytes());
        digest.update(3_u32.to_be_bytes());
        let fingerprint = hex::encode(digest.finalize());
        let profile = json!({"crypto_suite":1,"salt":salt,"vault_id":vault,"key_epoch":3});
        assert_eq!(validate_profile(&profile, &fingerprint, 3), Ok(vault));
        assert!(validate_profile(&profile, &fingerprint, 2).is_err());
    }
}
