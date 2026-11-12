//! Optional opaque wake relay. Provider adapters only receive content-free hints.

pub mod providers;

use std::{
    collections::HashMap,
    future::Future,
    net::{IpAddr, SocketAddr},
    pin::Pin,
    sync::Arc,
    time::{Duration, SystemTime},
};

use axum::extract::DefaultBodyLimit;
use axum::{
    Json, Router,
    extract::{ConnectInfo, Path, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, post},
};
use ipnet::IpNet;
use rand::{RngCore, rngs::OsRng};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::{Executor, PgPool, Postgres, Transaction};
use subtle::ConstantTimeEq;
use tokio::sync::Mutex;
use uuid::Uuid;

pub const CHALLENGE_TTL: Duration = Duration::from_secs(600);
const ADMISSION_WINDOW_SECONDS: u64 = 60;
const ADMISSION_WINDOW: Duration = Duration::from_secs(ADMISSION_WINDOW_SECONDS);
const ADMISSION_LIMIT: u16 = 20;
const WAKE_ADMISSION_LIMIT: u16 = 120;
const MAX_WAKE_ADMISSION_LIMIT: u32 = 100_000;
const ADMISSION_MAP_CAP: usize = 4096;
const EXPIRED_REGISTRATION_CLEANUP_LIMIT: i64 = 100;
const EXPIRED_ADMISSION_CLEANUP_LIMIT: i64 = 1000;
pub const WAKE_DELIVERY_BATCH_LIMIT: usize = 32;
const MAX_TOKEN_BYTES: usize = 4096;
const MAX_IDENTITY_BYTES: usize = 4096;
const MAX_IDEMPOTENCY_BYTES: usize = 128;
const MAX_NONCE_BYTES: usize = 256;
const SECRET_HEX_BYTES: usize = 64;
const MAX_FORWARDED_HEADER_BYTES: usize = 8192;
const MAX_FORWARDED_HOPS: usize = 16;

#[derive(Clone)]
pub struct AppState {
    pool: PgPool,
    provider: Arc<dyn PushProvider>,
    admissions: Arc<Mutex<HashMap<String, Admission>>>,
    wake_admissions: Arc<Mutex<HashMap<String, Admission>>>,
    management_admissions: Arc<Mutex<HashMap<String, Admission>>>,
    wake_admission_limit: u32,
    trusted_proxy_cidrs: Arc<Vec<IpNet>>,
    release_identity: Arc<String>,
}
struct Admission {
    window: SystemTime,
    count: u32,
}

#[derive(Clone, Copy)]
enum AdmissionKind {
    Registration,
    Wake,
    Management,
}

#[derive(Debug, Clone, Copy)]
enum AdmissionError {
    RateLimited,
    DatabaseUnavailable,
}

impl AdmissionKind {
    fn name(self) -> &'static str {
        match self {
            Self::Registration => "registration",
            Self::Wake => "wake",
            Self::Management => "management",
        }
    }
}

/// Narrow outbound boundary: only provider token and opaque relay hints cross it.
pub trait PushProvider: Send + Sync {
    fn configured(&self) -> bool;
    fn supports(&self, provider: Provider) -> bool;
    fn provider_name(&self) -> &'static str;
    fn apns_environment(&self) -> Option<&'static str> {
        None
    }
    fn send_challenge<'a>(
        &'a self,
        provider: Provider,
        token: &'a str,
        challenge: &'a str,
    ) -> ProviderFuture<'a>;
    fn send_wake<'a>(
        &'a self,
        provider: Provider,
        token: &'a str,
        opaque_nonce: &'a str,
    ) -> ProviderFuture<'a>;
}
pub type ProviderFuture<'a> = Pin<Box<dyn Future<Output = Result<(), ProviderError>> + Send + 'a>>;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderError {
    Retryable,
    InvalidToken,
    Permanent,
    Unavailable,
}

#[derive(Clone, Copy)]
enum DeliveryDiagnostic {
    DatabaseFailure,
    ProviderPermanent,
    ProviderRetryExhausted,
}

impl DeliveryDiagnostic {
    fn error_class(self) -> &'static str {
        match self {
            Self::DatabaseFailure => "database_failure",
            Self::ProviderPermanent => "provider_permanent",
            Self::ProviderRetryExhausted => "provider_retry_exhausted",
        }
    }
}

fn report_delivery_diagnostic(diagnostic: DeliveryDiagnostic) {
    tracing::warn!(
        operation = "wake_delivery",
        error_class = diagnostic.error_class(),
        "relay wake delivery failed"
    );
}

fn report_cleanup_database_failure() {
    tracing::warn!(
        operation = "expired_registration_cleanup",
        error_class = "database_failure",
        "relay database maintenance failed"
    );
}

pub struct UnconfiguredProvider;
impl PushProvider for UnconfiguredProvider {
    fn configured(&self) -> bool {
        false
    }
    fn supports(&self, _: Provider) -> bool {
        false
    }
    fn provider_name(&self) -> &'static str {
        "unconfigured"
    }
    fn send_challenge<'a>(&'a self, _: Provider, _: &'a str, _: &'a str) -> ProviderFuture<'a> {
        Box::pin(async { Err(ProviderError::Unavailable) })
    }
    fn send_wake<'a>(&'a self, _: Provider, _: &'a str, _: &'a str) -> ProviderFuture<'a> {
        Box::pin(async { Err(ProviderError::Unavailable) })
    }
}

#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq, sqlx::Type)]
#[serde(rename_all = "lowercase")]
#[sqlx(type_name = "text", rename_all = "lowercase")]
pub enum Provider {
    Apns,
    Fcm,
}
impl Provider {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "apns" => Some(Self::Apns),
            "fcm" => Some(Self::Fcm),
            _ => None,
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegisterRequest {
    pub provider: Provider,
    pub device_token: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfirmRequest {
    pub challenge: String,
    pub installation_public_identity: Option<String>,
    pub route_id: Option<Uuid>,
    pub manage_credential: Option<String>,
    pub wake_credential: Option<String>,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WakeRequest {
    pub wake_credential: String,
    pub idempotency_id: String,
    pub opaque_nonce: String,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RevokeRequest {
    pub manage_credential: String,
}
#[derive(Debug, Serialize, Deserialize)]
pub struct RegistrationPending {
    pub registration_id: Uuid,
    pub expires_in_seconds: u64,
}
#[derive(Debug, Serialize, Deserialize)]
pub struct RouteCredentials {
    pub route_id: Uuid,
    pub manage_credential: String,
    pub wake_credential: String,
}
#[derive(Debug, Serialize)]
pub struct Health {
    pub status: &'static str,
    pub provider: &'static str,
    pub warning: &'static str,
    pub providers: Vec<&'static str>,
    pub apns_environment: Option<&'static str>,
}
#[derive(Debug, Serialize)]
pub struct ProviderCapabilities {
    pub available: bool,
    pub confirm_candidates: bool,
    pub providers: Vec<&'static str>,
    pub apns_environment: Option<&'static str>,
}
#[derive(Debug, Serialize)]
struct ApiError {
    code: &'static str,
}
type ApiResult<T> = Result<(StatusCode, Json<T>), (StatusCode, Json<ApiError>)>;

impl AppState {
    pub fn new(pool: PgPool) -> Self {
        Self::with_provider(pool, Arc::new(UnconfiguredProvider))
    }
    pub fn with_provider(pool: PgPool, provider: Arc<dyn PushProvider>) -> Self {
        Self::with_provider_and_proxy_cidrs(pool, provider, Vec::new(), "unknown".into())
    }
    pub fn with_provider_and_proxy_cidrs(
        pool: PgPool,
        provider: Arc<dyn PushProvider>,
        trusted_proxy_cidrs: Vec<IpNet>,
        release_identity: String,
    ) -> Self {
        Self::with_provider_proxy_cidrs_and_wake_limit(
            pool,
            provider,
            trusted_proxy_cidrs,
            release_identity,
            WAKE_ADMISSION_LIMIT.into(),
        )
    }
    pub fn with_provider_proxy_cidrs_and_wake_limit(
        pool: PgPool,
        provider: Arc<dyn PushProvider>,
        trusted_proxy_cidrs: Vec<IpNet>,
        release_identity: String,
        wake_admission_limit: u32,
    ) -> Self {
        Self {
            pool,
            provider,
            admissions: Arc::new(Mutex::new(HashMap::new())),
            wake_admissions: Arc::new(Mutex::new(HashMap::new())),
            management_admissions: Arc::new(Mutex::new(HashMap::new())),
            wake_admission_limit,
            trusted_proxy_cidrs: Arc::new(trusted_proxy_cidrs),
            release_identity: Arc::new(release_identity),
        }
    }
    pub fn provider_configured(&self) -> bool {
        self.provider.configured()
    }
    async fn admit_registration(&self, ip: SocketAddr) -> Result<(), AdmissionError> {
        let source = self
            .prefilter_admit(AdmissionKind::Registration, ip)
            .await?;
        self.admit_durable(AdmissionKind::Registration, source)
            .await
    }
    async fn admit_route_wake(
        &self,
        route_id: Uuid,
        transaction: &mut Transaction<'_, Postgres>,
    ) -> Result<(), AdmissionError> {
        self.admit_durable_with(
            AdmissionKind::Wake,
            route_id.to_string(),
            &mut **transaction,
        )
        .await
    }
    async fn admit_route_management(
        &self,
        route_id: Uuid,
        transaction: &mut Transaction<'_, Postgres>,
    ) -> Result<(), AdmissionError> {
        self.admit_durable_with(
            AdmissionKind::Wake,
            format!("management:{route_id}"),
            &mut **transaction,
        )
        .await
    }
    async fn admit_durable(
        &self,
        kind: AdmissionKind,
        source: String,
    ) -> Result<(), AdmissionError> {
        self.admit_durable_with(kind, source, &self.pool).await
    }
    async fn admit_durable_with<'executor, E>(
        &self,
        kind: AdmissionKind,
        source: String,
        executor: E,
    ) -> Result<(), AdmissionError>
    where
        E: Executor<'executor, Database = Postgres>,
    {
        let limit = match kind {
            AdmissionKind::Registration => u32::from(ADMISSION_LIMIT),
            AdmissionKind::Wake => self.wake_admission_limit,
            AdmissionKind::Management => self.wake_admission_limit,
        };
        let admitted: bool = sqlx::query_scalar(
            "WITH admitted AS (INSERT INTO relay_admissions(admission_kind,source,window_started_at,attempts) VALUES($1,$2,date_trunc('minute', statement_timestamp()),1) ON CONFLICT(admission_kind,source,window_started_at) DO UPDATE SET attempts=relay_admissions.attempts+1 WHERE relay_admissions.attempts < $3 RETURNING attempts) SELECT EXISTS(SELECT 1 FROM admitted)",
        )
        .bind(kind.name())
        .bind(source)
        .bind(i32::try_from(limit).expect("admission limit fits i32"))
        .fetch_one(executor)
        .await
        .map_err(|_| AdmissionError::DatabaseUnavailable)?;
        if !admitted {
            return Err(AdmissionError::RateLimited);
        }
        Ok(())
    }
    async fn prefilter_admit(
        &self,
        kind: AdmissionKind,
        ip: SocketAddr,
    ) -> Result<String, AdmissionError> {
        let now = SystemTime::now();
        let window = admission_window(now);
        let key = match kind {
            AdmissionKind::Registration => admission_source(ip.ip()),
            AdmissionKind::Wake => canonical_source(ip.ip()),
            AdmissionKind::Management => canonical_source(ip.ip()),
        };
        let (entries, limit) = match kind {
            AdmissionKind::Registration => (&self.admissions, u32::from(ADMISSION_LIMIT)),
            AdmissionKind::Wake => (&self.wake_admissions, self.wake_admission_limit),
            AdmissionKind::Management => (&self.management_admissions, self.wake_admission_limit),
        };
        let mut entries = entries.lock().await;
        entries.retain(|_, value| {
            window
                .duration_since(value.window)
                .map(|elapsed| elapsed < ADMISSION_WINDOW)
                .unwrap_or(true)
        });
        if entries.len() >= ADMISSION_MAP_CAP && !entries.contains_key(&key) {
            return Err(AdmissionError::RateLimited);
        }
        let entry = entries
            .entry(key.clone())
            .or_insert(Admission { window, count: 0 });
        if entry.count >= limit {
            return Err(AdmissionError::RateLimited);
        }
        entry.count += 1;
        Ok(key)
    }
    async fn admit_request(
        &self,
        peer: SocketAddr,
        headers: &HeaderMap,
    ) -> Result<(), AdmissionError> {
        self.admit_registration(self.client_ip(peer, headers)).await
    }
    async fn admit_wake_request(
        &self,
        peer: SocketAddr,
        headers: &HeaderMap,
    ) -> Result<(), AdmissionError> {
        self.prefilter_admit(AdmissionKind::Wake, self.client_ip(peer, headers))
            .await
            .map(|_| ())
    }
    async fn admit_management_request(
        &self,
        peer: SocketAddr,
        headers: &HeaderMap,
    ) -> Result<(), AdmissionError> {
        self.prefilter_admit(AdmissionKind::Management, self.client_ip(peer, headers))
            .await
            .map(|_| ())
    }
    fn client_ip(&self, peer: SocketAddr, headers: &HeaderMap) -> SocketAddr {
        resolve_client_ip(peer, headers, &self.trusted_proxy_cidrs)
    }
}

fn admission_source(ip: IpAddr) -> String {
    match ip.to_canonical() {
        IpAddr::V4(ip) => ip.to_string(),
        IpAddr::V6(ip) => {
            let segments = ip.segments();
            format!(
                "{:x}:{:x}:{:x}:{:x}::/64",
                segments[0], segments[1], segments[2], segments[3]
            )
        }
    }
}

fn canonical_source(ip: IpAddr) -> String {
    ip.to_canonical().to_string()
}

fn admission_window(now: SystemTime) -> SystemTime {
    let elapsed = now
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    let window_seconds = elapsed.as_secs() / ADMISSION_WINDOW_SECONDS * ADMISSION_WINDOW_SECONDS;
    SystemTime::UNIX_EPOCH + Duration::from_secs(window_seconds)
}

/// Parses the bounded per-IP wake admission budget. The default preserves the
/// historical 120 requests/minute limit; it never creates an unauthenticated bypass.
pub fn wake_admission_limit(value: Option<&str>) -> Result<u32, String> {
    let Some(value) = value else {
        return Ok(WAKE_ADMISSION_LIMIT.into());
    };
    let parsed = value
        .parse::<u32>()
        .map_err(|_| "PEPPY_RELAY_WAKE_ADMISSION_LIMIT must be an integer".to_owned())?;
    if !(1..=MAX_WAKE_ADMISSION_LIMIT).contains(&parsed) {
        return Err(format!(
            "PEPPY_RELAY_WAKE_ADMISSION_LIMIT must be between 1 and {MAX_WAKE_ADMISSION_LIMIT}"
        ));
    }
    Ok(parsed)
}

/// Parses the comma-separated CIDR allowlist used for proxy forwarding.
/// An unset or empty value disables forwarded-header trust entirely.
pub fn trusted_proxy_cidrs(value: Option<&str>) -> Result<Vec<IpNet>, String> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    if value.is_empty() {
        return Ok(Vec::new());
    }
    value
        .split(',')
        .map(|cidr| {
            if cidr.is_empty() || cidr.trim() != cidr {
                return Err(
                    "PEPPY_RELAY_TRUSTED_PROXY_CIDRS must be a comma-separated CIDR list".into(),
                );
            }
            cidr.parse::<IpNet>()
                .map_err(|_| format!("invalid trusted proxy CIDR: {cidr}"))
        })
        .collect()
}

fn resolve_client_ip(peer: SocketAddr, headers: &HeaderMap, trusted: &[IpNet]) -> SocketAddr {
    if !trusted.iter().any(|network| network.contains(&peer.ip())) {
        return peer;
    }
    let forwarded: Vec<_> = headers.get_all("x-forwarded-for").iter().collect();
    if forwarded.len() != 1 {
        return peer;
    }
    let value = forwarded[0].as_bytes();
    if value.len() > MAX_FORWARDED_HEADER_BYTES {
        return peer;
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
    let Some(hops) = hops else { return peer };
    for hop in hops.into_iter().rev() {
        if !trusted.iter().any(|network| network.contains(&hop)) {
            return SocketAddr::new(hop, peer.port());
        }
    }
    peer
}

pub fn app(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(health))
        .route("/readyz", get(ready))
        .route("/v1/capabilities", get(capabilities))
        .route("/__release", get(release))
        .route("/v1/registrations", post(register))
        .route("/v1/registrations/{id}/confirm", post(confirm))
        .route("/v1/routes/{id}/wake", post(wake))
        .route("/v1/routes/{id}/revoke", post(revoke))
        .layer(DefaultBodyLimit::max(8 * 1024))
        .with_state(state)
}
#[derive(Debug, Serialize)]
struct ReleaseIdentity {
    revision: String,
}
async fn release(State(state): State<AppState>) -> Json<ReleaseIdentity> {
    Json(ReleaseIdentity {
        revision: (*state.release_identity).clone(),
    })
}
async fn health(State(state): State<AppState>) -> impl IntoResponse {
    (
        StatusCode::OK,
        Json(Health {
            status: "ok",
            provider: state.provider.provider_name(),
            warning: if state.provider.configured() {
                "provider configured; delivery is best effort"
            } else {
                "no provider adapter configured; relay cannot deliver push"
            },
            providers: configured_providers(&state),
            apns_environment: state.provider.apns_environment(),
        }),
    )
}
async fn capabilities(State(state): State<AppState>) -> Json<ProviderCapabilities> {
    Json(ProviderCapabilities {
        available: state.provider.configured(),
        confirm_candidates: true,
        providers: configured_providers(&state),
        apns_environment: state.provider.apns_environment(),
    })
}
async fn ready(State(state): State<AppState>) -> impl IntoResponse {
    if sqlx::query("SELECT 1").execute(&state.pool).await.is_err() {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(Health {
                status: "unavailable",
                provider: state.provider.provider_name(),
                warning: "database unavailable",
                providers: configured_providers(&state),
                apns_environment: state.provider.apns_environment(),
            }),
        );
    }
    if !state.provider.configured() {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(Health {
                status: "unavailable",
                provider: state.provider.provider_name(),
                warning: "no provider adapter configured; relay cannot deliver push",
                providers: configured_providers(&state),
                apns_environment: state.provider.apns_environment(),
            }),
        );
    }
    (
        StatusCode::OK,
        Json(Health {
            status: "ready",
            provider: state.provider.provider_name(),
            warning: "provider configured; delivery is best effort",
            providers: configured_providers(&state),
            apns_environment: state.provider.apns_environment(),
        }),
    )
}

fn configured_providers(state: &AppState) -> Vec<&'static str> {
    [Provider::Apns, Provider::Fcm]
        .into_iter()
        .filter(|provider| state.provider.supports(*provider))
        .map(provider_name)
        .collect()
}

async fn register(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(request): Json<RegisterRequest>,
) -> ApiResult<RegistrationPending> {
    valid_nonempty(
        &request.device_token,
        MAX_TOKEN_BYTES,
        "invalid_device_token",
    )?;
    state
        .admit_request(peer, &headers)
        .await
        .map_err(admission_error)?;
    if !state.provider.supports(request.provider) {
        return Err(unconfigured());
    }
    let id = Uuid::new_v4();
    let challenge = secret();
    let insert = sqlx::query("INSERT INTO relay_registrations(id,provider,token,challenge_digest,expires_at) VALUES($1,$2,$3,$4,now()+interval '10 minutes')")
        .bind(id).bind(provider_name(request.provider)).bind(&request.device_token).bind(digest(&challenge)).execute(&state.pool).await;
    insert.map_err(db)?;
    if state
        .provider
        .send_challenge(request.provider, &request.device_token, &challenge)
        .await
        .is_err()
    {
        let _ = sqlx::query("DELETE FROM relay_registrations WHERE id=$1 AND confirmed_at IS NULL")
            .bind(id)
            .execute(&state.pool)
            .await;
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ApiError {
                code: "provider_delivery_failed",
            }),
        ));
    }
    Ok((
        StatusCode::CREATED,
        Json(RegistrationPending {
            registration_id: id,
            expires_in_seconds: CHALLENGE_TTL.as_secs(),
        }),
    ))
}

async fn confirm(
    Path(id): Path<Uuid>,
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(request): Json<ConfirmRequest>,
) -> ApiResult<RouteCredentials> {
    valid_secret(&request.challenge, "invalid_challenge")?;
    if let Some(identity) = &request.installation_public_identity {
        valid_nonempty(identity, MAX_IDENTITY_BYTES, "invalid_identity")?;
    }
    let candidate = confirmation_candidate(&request)?;
    state
        .admit_request(peer, &headers)
        .await
        .map_err(admission_error)?;
    if !state.provider.configured() {
        return Err(unconfigured());
    }
    let mut tx = state.pool.begin().await.map_err(db)?;
    let row = sqlx::query_as::<_, (String, String, Vec<u8>, bool, bool)>("SELECT provider,token,challenge_digest,confirmed_at IS NOT NULL,expires_at>now() FROM relay_registrations WHERE id=$1 AND revoked_at IS NULL FOR UPDATE")
        .bind(id).fetch_optional(&mut *tx).await.map_err(db)?;
    let Some((provider, token, stored, confirmed, unexpired)) = row else {
        return Err(unauthorized());
    };
    if !constant_time_eq(&stored, &digest(&request.challenge)) {
        return Err(unauthorized());
    }
    if confirmed {
        let Some(candidate) = candidate else {
            return Err(unauthorized());
        };
        let route = sqlx::query_as::<_, (Vec<u8>, Vec<u8>)>("SELECT manage_digest,wake_digest FROM relay_routes WHERE id=$1 AND registration_id=$2 AND revoked_at IS NULL FOR UPDATE")
            .bind(candidate.route_id)
            .bind(id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(db)?;
        let Some((manage_digest, wake_digest)) = route else {
            return Err(unauthorized());
        };
        if !constant_time_eq(&manage_digest, &digest(&candidate.manage_credential))
            || !constant_time_eq(&wake_digest, &digest(&candidate.wake_credential))
        {
            return Err(unauthorized());
        }
        tx.commit().await.map_err(db)?;
        return Ok((StatusCode::CREATED, Json(candidate.into())));
    }
    if !unexpired {
        return Err(unauthorized());
    }
    let candidate = candidate.unwrap_or_else(|| ConfirmationCandidate {
        route_id: Uuid::new_v4(),
        manage_credential: secret(),
        wake_credential: secret(),
    });
    let provider = Provider::parse(&provider).ok_or_else(db_error)?;
    let inserted = sqlx::query("INSERT INTO relay_routes(id,registration_id,provider,token,manage_digest,wake_digest) VALUES($1,$2,$3,$4,$5,$6) ON CONFLICT(id) DO NOTHING")
        .bind(candidate.route_id).bind(id).bind(provider_name(provider)).bind(token).bind(digest(&candidate.manage_credential)).bind(digest(&candidate.wake_credential)).execute(&mut *tx).await.map_err(db)?;
    if inserted.rows_affected() != 1 {
        return Err(unauthorized());
    }
    let changed = sqlx::query("UPDATE relay_registrations SET confirmed_at=now(),installation_public_identity=$2 WHERE id=$1 AND confirmed_at IS NULL")
        .bind(id).bind(request.installation_public_identity).execute(&mut *tx).await.map_err(db)?;
    if changed.rows_affected() != 1 {
        return Err(unauthorized());
    }
    tx.commit().await.map_err(db)?;
    Ok((StatusCode::CREATED, Json(candidate.into())))
}

struct ConfirmationCandidate {
    route_id: Uuid,
    manage_credential: String,
    wake_credential: String,
}

impl From<ConfirmationCandidate> for RouteCredentials {
    fn from(candidate: ConfirmationCandidate) -> Self {
        Self {
            route_id: candidate.route_id,
            manage_credential: candidate.manage_credential,
            wake_credential: candidate.wake_credential,
        }
    }
}

fn confirmation_candidate(
    request: &ConfirmRequest,
) -> Result<Option<ConfirmationCandidate>, (StatusCode, Json<ApiError>)> {
    match (
        request.route_id,
        request.manage_credential.as_deref(),
        request.wake_credential.as_deref(),
    ) {
        (None, None, None) => Ok(None),
        (Some(route_id), Some(manage_credential), Some(wake_credential)) => {
            valid_secret(manage_credential, "invalid_confirmation_candidate")?;
            valid_secret(wake_credential, "invalid_confirmation_candidate")?;
            Ok(Some(ConfirmationCandidate {
                route_id,
                manage_credential: manage_credential.to_owned(),
                wake_credential: wake_credential.to_owned(),
            }))
        }
        _ => Err(bad("invalid_confirmation_candidate")),
    }
}

async fn wake(
    Path(id): Path<Uuid>,
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(request): Json<WakeRequest>,
) -> ApiResult<serde_json::Value> {
    valid_secret(&request.wake_credential, "invalid_wake_credential")?;
    valid_nonempty(
        &request.idempotency_id,
        MAX_IDEMPOTENCY_BYTES,
        "invalid_wake_metadata",
    )?;
    valid_nonempty(
        &request.opaque_nonce,
        MAX_NONCE_BYTES,
        "invalid_wake_metadata",
    )?;
    // Wake traffic has an independent, higher pre-auth budget so server wake
    // bursts cannot consume public enrollment capacity or bypass brute-force limits.
    state
        .admit_wake_request(peer, &headers)
        .await
        .map_err(admission_error)?;
    if !state.provider.configured() {
        return Err(unconfigured());
    }
    let mut tx = state.pool.begin().await.map_err(db)?;
    let row = sqlx::query_as::<_, (String, Vec<u8>)>("SELECT provider,wake_digest FROM relay_routes WHERE id=$1 AND revoked_at IS NULL FOR UPDATE")
        .bind(id).fetch_optional(&mut *tx).await.map_err(db)?;
    let Some((route_provider, stored)) = row else {
        return Err(unauthorized());
    };
    if !constant_time_eq(&stored, &digest(&request.wake_credential)) {
        return Err(unauthorized());
    }
    state
        .admit_route_wake(id, &mut tx)
        .await
        .map_err(admission_error)?;
    let Some(route_provider) = Provider::parse(&route_provider) else {
        return Err(db_error());
    };
    if !state.provider.supports(route_provider) {
        return Err(unconfigured());
    }
    let delivered: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM relay_wake_deliveries WHERE route_id=$1 AND idempotency_id=$2)")
        .bind(id).bind(&request.idempotency_id).fetch_one(&mut *tx).await.map_err(db)?;
    if delivered {
        tx.commit().await.map_err(db)?;
        return Ok((
            StatusCode::ACCEPTED,
            Json(serde_json::json!({"accepted": true})),
        ));
    }
    // A repeated idempotency ID must not reclaim an in-flight leased delivery.
    // A newer authoritative wake coalesces over the older pending hint.
    sqlx::query("INSERT INTO relay_wake_jobs(route_id,idempotency_id,opaque_nonce,state) VALUES($1,$2,$3,'pending') ON CONFLICT(route_id) DO UPDATE SET idempotency_id=EXCLUDED.idempotency_id,opaque_nonce=CASE WHEN relay_wake_jobs.idempotency_id=EXCLUDED.idempotency_id THEN relay_wake_jobs.opaque_nonce ELSE EXCLUDED.opaque_nonce END,state=CASE WHEN relay_wake_jobs.idempotency_id=EXCLUDED.idempotency_id THEN relay_wake_jobs.state ELSE 'pending' END,attempts=CASE WHEN relay_wake_jobs.idempotency_id=EXCLUDED.idempotency_id THEN relay_wake_jobs.attempts ELSE 0 END,next_attempt_at=CASE WHEN relay_wake_jobs.idempotency_id=EXCLUDED.idempotency_id THEN relay_wake_jobs.next_attempt_at WHEN relay_wake_jobs.state='attempted' THEN relay_wake_jobs.next_attempt_at ELSE now() END,updated_at=now()")
        .bind(id).bind(&request.idempotency_id).bind(&request.opaque_nonce).execute(&mut *tx).await.map_err(db)?;
    tx.commit().await.map_err(db)?;
    Ok((
        StatusCode::ACCEPTED,
        Json(serde_json::json!({"accepted": true})),
    ))
}

/// Process at most one durable coalesced wake job. Called by the bounded relay
/// maintenance task, never by an inbound request, so provider HTTP cannot block
/// request admission or create duplicate delivery after a 202 response.
pub async fn deliver_one(state: &AppState) -> Result<bool, sqlx::Error> {
    let mut tx = state.pool.begin().await?;
    let job = sqlx::query_as::<_, (Uuid, String, String, String, String, i32)>("SELECT j.route_id,r.provider,r.token,j.opaque_nonce,j.idempotency_id,j.attempts FROM relay_wake_jobs j JOIN relay_routes r ON r.id=j.route_id WHERE r.revoked_at IS NULL AND j.state IN ('pending','attempted') AND j.next_attempt_at<=now() ORDER BY j.updated_at FOR UPDATE SKIP LOCKED LIMIT 1").fetch_optional(&mut *tx).await?;
    let Some((route_id, provider_name, token, nonce, idempotency_id, attempts)) = job else {
        tx.commit().await?;
        return Ok(false);
    };
    // The lease is longer than the fixed provider timeout. A process crash can
    // recover it later without a second worker concurrently resending a 202 job.
    sqlx::query("UPDATE relay_wake_jobs SET state='attempted',next_attempt_at=now()+interval '60 seconds',updated_at=now() WHERE route_id=$1")
        .bind(route_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    let outcome = match Provider::parse(&provider_name) {
        Some(provider) => state.provider.send_wake(provider, &token, &nonce).await,
        None => Err(ProviderError::InvalidToken),
    };
    let mut tx = state.pool.begin().await?;
    match outcome {
        Ok(()) => {
            sqlx::query("INSERT INTO relay_wake_deliveries(route_id,idempotency_id) VALUES($1,$2) ON CONFLICT DO NOTHING")
                .bind(route_id).bind(&idempotency_id)
                .execute(&mut *tx)
                .await?;
            sqlx::query("DELETE FROM relay_wake_jobs WHERE route_id=$1 AND idempotency_id=$2 AND state='attempted'")
                .bind(route_id).bind(&idempotency_id).execute(&mut *tx).await?;
        }
        Err(ProviderError::InvalidToken) => {
            sqlx::query(
                "UPDATE relay_routes SET revoked_at=now(),token='' WHERE id=$1 AND token=$2",
            )
            .bind(route_id)
            .bind(&token)
            .execute(&mut *tx)
            .await?;
            sqlx::query("UPDATE relay_registrations SET revoked_at=now(),token='' WHERE id=(SELECT registration_id FROM relay_routes WHERE id=$1)")
                .bind(route_id).execute(&mut *tx).await?;
            sqlx::query("DELETE FROM relay_wake_jobs WHERE route_id=$1")
                .bind(route_id)
                .execute(&mut *tx)
                .await?;
        }
        Err(ProviderError::Permanent) => {
            sqlx::query("UPDATE relay_wake_jobs SET state='failed',attempts=attempts+1,updated_at=now() WHERE route_id=$1 AND idempotency_id=$2 AND state='attempted'")
                .bind(route_id).bind(&idempotency_id).execute(&mut *tx).await?;
            report_delivery_diagnostic(DeliveryDiagnostic::ProviderPermanent);
        }
        Err(_) => {
            sqlx::query("UPDATE relay_wake_jobs SET state=CASE WHEN attempts >= 8 THEN 'failed' ELSE 'pending' END,attempts=attempts+1,next_attempt_at=now() + make_interval(secs => LEAST(3600, 5 * (2 ^ LEAST(attempts, 10)))),updated_at=now() WHERE route_id=$1 AND idempotency_id=$2 AND state='attempted'").bind(route_id).bind(&idempotency_id).execute(&mut *tx).await?;
            if attempts >= 8 {
                report_delivery_diagnostic(DeliveryDiagnostic::ProviderRetryExhausted);
            }
        }
    }
    sqlx::query(
        "DELETE FROM relay_wake_deliveries WHERE delivered_at < now() - interval '24 hours'",
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(true)
}

/// Deletes a bounded batch of expired registrations that never produced a route.
/// Confirmed registrations are retained for credential-bound replay recovery.
pub async fn cleanup_expired_registrations(state: &AppState) -> Result<u64, sqlx::Error> {
    let deleted = sqlx::query("WITH expired AS (SELECT id FROM relay_registrations WHERE confirmed_at IS NULL AND expires_at <= now() ORDER BY expires_at FOR UPDATE SKIP LOCKED LIMIT $1) DELETE FROM relay_registrations registration USING expired WHERE registration.id=expired.id AND registration.confirmed_at IS NULL")
        .bind(EXPIRED_REGISTRATION_CLEANUP_LIMIT)
        .execute(&state.pool)
        .await?;
    cleanup_expired_admissions(state).await?;
    Ok(deleted.rows_affected())
}

/// Runs one bounded maintenance pass without exposing provider or database error details.
pub async fn run_maintenance_pass(state: &AppState) {
    for _ in 0..WAKE_DELIVERY_BATCH_LIMIT {
        match deliver_one(state).await {
            Ok(true) => {}
            Ok(false) => break,
            Err(_) => {
                report_delivery_diagnostic(DeliveryDiagnostic::DatabaseFailure);
                break;
            }
        }
    }
    if cleanup_expired_registrations(state).await.is_err() {
        report_cleanup_database_failure();
    }
}

async fn cleanup_expired_admissions(state: &AppState) -> Result<u64, sqlx::Error> {
    let deleted = sqlx::query(
        "WITH expired AS (SELECT admission_kind,source,window_started_at FROM relay_admissions WHERE window_started_at < date_trunc('minute', now()) ORDER BY window_started_at FOR UPDATE SKIP LOCKED LIMIT $1) DELETE FROM relay_admissions admission USING expired WHERE (admission.admission_kind,admission.source,admission.window_started_at)=(expired.admission_kind,expired.source,expired.window_started_at)",
    )
    .bind(EXPIRED_ADMISSION_CLEANUP_LIMIT)
    .execute(&state.pool)
    .await?;
    Ok(deleted.rows_affected())
}

async fn revoke(
    Path(id): Path<Uuid>,
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(request): Json<RevokeRequest>,
) -> Result<StatusCode, (StatusCode, Json<ApiError>)> {
    valid_secret(&request.manage_credential, "invalid_manage_credential")?;
    state
        .admit_management_request(peer, &headers)
        .await
        .map_err(admission_error)?;
    let mut tx = state.pool.begin().await.map_err(db)?;
    let row = sqlx::query_as::<_, (Uuid, Vec<u8>)>("SELECT registration_id,manage_digest FROM relay_routes WHERE id=$1 AND revoked_at IS NULL FOR UPDATE")
        .bind(id).fetch_optional(&mut *tx).await.map_err(db)?;
    let Some((registration, stored)) = row else {
        return Err(unauthorized());
    };
    if !constant_time_eq(&stored, &digest(&request.manage_credential)) {
        return Err(unauthorized());
    }
    state
        .admit_route_management(id, &mut tx)
        .await
        .map_err(admission_error)?;
    sqlx::query("UPDATE relay_routes SET revoked_at=now(),token='' WHERE id=$1")
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(db)?;
    sqlx::query("UPDATE relay_registrations SET revoked_at=now(),token='' WHERE id=$1")
        .bind(registration)
        .execute(&mut *tx)
        .await
        .map_err(db)?;
    sqlx::query("DELETE FROM relay_wake_jobs WHERE route_id=$1")
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(db)?;
    tx.commit().await.map_err(db)?;
    Ok(StatusCode::NO_CONTENT)
}

fn provider_name(provider: Provider) -> &'static str {
    match provider {
        Provider::Apns => "apns",
        Provider::Fcm => "fcm",
    }
}
fn secret() -> String {
    let mut bytes = [0_u8; 32];
    OsRng.fill_bytes(&mut bytes);
    hex::encode(bytes)
}
fn digest(value: &str) -> Vec<u8> {
    Sha256::digest(value.as_bytes()).to_vec()
}
fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    left.ct_eq(right).into()
}
fn valid_nonempty(
    value: &str,
    maximum: usize,
    code: &'static str,
) -> Result<(), (StatusCode, Json<ApiError>)> {
    if value.is_empty() || value.len() > maximum {
        Err(bad(code))
    } else {
        Ok(())
    }
}
fn valid_secret(value: &str, code: &'static str) -> Result<(), (StatusCode, Json<ApiError>)> {
    if value.len() == SECRET_HEX_BYTES && value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        Ok(())
    } else {
        Err(bad(code))
    }
}
fn bad(code: &'static str) -> (StatusCode, Json<ApiError>) {
    (StatusCode::BAD_REQUEST, Json(ApiError { code }))
}
fn unauthorized() -> (StatusCode, Json<ApiError>) {
    (
        StatusCode::UNAUTHORIZED,
        Json(ApiError {
            code: "unauthorized",
        }),
    )
}
fn unconfigured() -> (StatusCode, Json<ApiError>) {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(ApiError {
            code: "provider_unconfigured",
        }),
    )
}
fn rate(error: ApiError) -> (StatusCode, Json<ApiError>) {
    (StatusCode::TOO_MANY_REQUESTS, Json(error))
}
fn admission_error(error: AdmissionError) -> (StatusCode, Json<ApiError>) {
    match error {
        AdmissionError::RateLimited => rate(ApiError {
            code: "rate_limited",
        }),
        AdmissionError::DatabaseUnavailable => db_error(),
    }
}
fn db(_: sqlx::Error) -> (StatusCode, Json<ApiError>) {
    db_error()
}
fn db_error() -> (StatusCode, Json<ApiError>) {
    tracing::warn!(
        kind = "database_query_failed",
        "relay database operation failed"
    );
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(ApiError {
            code: "database_unavailable",
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::{
        Connection, Executor,
        postgres::{PgConnection, PgPoolOptions},
    };
    use std::sync::Mutex as StdMutex;
    use tracing::{Event, Subscriber, field::Field};
    use tracing_subscriber::{
        Layer, Registry,
        field::Visit,
        layer::{Context, SubscriberExt},
        registry::LookupSpan,
    };

    #[derive(Default)]
    struct FakeProvider {
        challenges: StdMutex<Vec<(Provider, String, String)>>,
        wakes: StdMutex<Vec<(Provider, String, String)>>,
    }
    impl PushProvider for FakeProvider {
        fn configured(&self) -> bool {
            true
        }
        fn supports(&self, _: Provider) -> bool {
            true
        }
        fn provider_name(&self) -> &'static str {
            "fake"
        }
        fn send_challenge<'a>(
            &'a self,
            provider: Provider,
            token: &'a str,
            challenge: &'a str,
        ) -> ProviderFuture<'a> {
            Box::pin(async move {
                self.challenges
                    .lock()
                    .unwrap()
                    .push((provider, token.into(), challenge.into()));
                Ok(())
            })
        }
        fn send_wake<'a>(
            &'a self,
            provider: Provider,
            token: &'a str,
            nonce: &'a str,
        ) -> ProviderFuture<'a> {
            Box::pin(async move {
                self.wakes
                    .lock()
                    .unwrap()
                    .push((provider, token.into(), nonce.into()));
                Ok(())
            })
        }
    }

    struct FailingProvider(StdMutex<Vec<ProviderError>>);
    impl PushProvider for FailingProvider {
        fn configured(&self) -> bool {
            true
        }
        fn supports(&self, _: Provider) -> bool {
            true
        }
        fn provider_name(&self) -> &'static str {
            "failing-fake"
        }
        fn send_challenge<'a>(&'a self, _: Provider, _: &'a str, _: &'a str) -> ProviderFuture<'a> {
            Box::pin(async { Ok(()) })
        }
        fn send_wake<'a>(&'a self, _: Provider, _: &'a str, _: &'a str) -> ProviderFuture<'a> {
            let error = self.0.lock().unwrap().remove(0);
            Box::pin(async move { Err(error) })
        }
    }

    #[derive(Clone)]
    struct DiagnosticCapture(Arc<StdMutex<Vec<String>>>);
    impl<S> Layer<S> for DiagnosticCapture
    where
        S: Subscriber + for<'a> LookupSpan<'a>,
    {
        fn on_event(&self, event: &Event<'_>, _: Context<'_, S>) {
            if event.metadata().target() != "peppy_push_relay"
                || *event.metadata().level() != tracing::Level::WARN
            {
                return;
            }

            struct Fields {
                values: String,
                has_operation: bool,
            }
            impl Visit for Fields {
                fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
                    self.values.push_str(field.name());
                    self.values.push('=');
                    self.values.push_str(&format!("{value:?}"));
                    self.values.push(';');
                    self.has_operation |= field.name() == "operation";
                }
                fn record_str(&mut self, field: &Field, value: &str) {
                    self.values.push_str(field.name());
                    self.values.push('=');
                    self.values.push_str(value);
                    self.values.push(';');
                    self.has_operation |= field.name() == "operation";
                }
            }
            let mut fields = Fields {
                values: String::new(),
                has_operation: false,
            };
            event.record(&mut fields);
            if fields.has_operation {
                self.0.lock().unwrap().push(fields.values);
            }
        }
    }
    async fn test_pool() -> (PgPool, String) {
        test_pool_with_connections(1).await
    }
    async fn test_pool_with_connections(connections: u32) -> (PgPool, String) {
        let url = std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL is required");
        let schema = format!("relay_test_{}", Uuid::new_v4().simple());
        let mut admin = PgConnection::connect(&url).await.unwrap();
        admin
            .execute(sqlx::query(&format!("CREATE SCHEMA {schema}")))
            .await
            .unwrap();
        admin
            .execute(sqlx::query(&format!("SET search_path TO {schema}")))
            .await
            .unwrap();
        sqlx::migrate!().run(&mut admin).await.unwrap();
        drop(admin);
        let schema_for_pool = schema.clone();
        let pool = PgPoolOptions::new()
            .max_connections(connections)
            .after_connect(move |conn, _| {
                let s = schema_for_pool.clone();
                Box::pin(async move {
                    sqlx::query(&format!("SET search_path TO {s}"))
                        .execute(&mut *conn)
                        .await?;
                    Ok(())
                })
            })
            .connect(&url)
            .await
            .unwrap();
        (pool, schema)
    }
    async fn cleanup(pool: PgPool, schema: String) {
        pool.close().await;
        let url = std::env::var("TEST_DATABASE_URL").unwrap();
        let mut admin = PgConnection::connect(&url).await.unwrap();
        admin
            .execute(sqlx::query(&format!("DROP SCHEMA {schema} CASCADE")))
            .await
            .unwrap();
    }
    async fn database_window(pool: &PgPool) -> i64 {
        sqlx::query_scalar(
            "SELECT extract(epoch FROM date_trunc('minute', statement_timestamp()))::bigint",
        )
        .fetch_one(pool)
        .await
        .unwrap()
    }
    async fn admit_route_wake_in_transaction(
        state: &AppState,
        pool: &PgPool,
        route_id: Uuid,
    ) -> Result<(), AdmissionError> {
        let mut transaction = pool.begin().await.unwrap();
        let result = state.admit_route_wake(route_id, &mut transaction).await;
        if result.is_ok() {
            transaction.commit().await.unwrap();
        }
        result
    }
    async fn serve(state: AppState) -> (String, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            axum::serve(
                listener,
                app(state).into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await
            .unwrap();
        });
        (format!("http://{address}"), task)
    }
    async fn post(
        client: &reqwest::Client,
        base: &str,
        path: &str,
        body: serde_json::Value,
    ) -> reqwest::Response {
        client
            .post(format!("{base}{path}"))
            .json(&body)
            .send()
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn router_db_contract_is_durable_and_socket_wired() {
        let (pool, schema) = test_pool().await;
        let fake = Arc::new(FakeProvider::default());
        let (base, task) = serve(AppState::with_provider(pool.clone(), fake.clone())).await;
        let client = reqwest::Client::new();
        let registered = post(
            &client,
            &base,
            "/v1/registrations",
            serde_json::json!({"provider":"fcm","device_token":"token-a"}),
        )
        .await;
        assert_eq!(registered.status(), StatusCode::CREATED);
        let registration: RegistrationPending = registered.json().await.unwrap();
        let challenge = fake.challenges.lock().unwrap()[0].2.clone();
        let candidate_route_id = Uuid::new_v4();
        let candidate_manage = "a".repeat(64);
        let candidate_wake = "b".repeat(64);
        assert_eq!(
            post(
                &client,
                &base,
                &format!("/v1/registrations/{}/confirm", registration.registration_id),
                serde_json::json!({"challenge": "0".repeat(64)})
            )
            .await
            .status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            post(
                &client,
                &base,
                &format!("/v1/registrations/{}/confirm", registration.registration_id),
                serde_json::json!({"challenge": &challenge, "route_id": candidate_route_id}),
            )
            .await
            .status(),
            StatusCode::BAD_REQUEST
        );
        let confirmed = post(
            &client,
            &base,
            &format!("/v1/registrations/{}/confirm", registration.registration_id),
            serde_json::json!({"challenge":&challenge,"route_id":candidate_route_id,"manage_credential":&candidate_manage,"wake_credential":&candidate_wake}),
        )
        .await;
        assert_eq!(confirmed.status(), StatusCode::CREATED);
        let route: RouteCredentials = confirmed.json().await.unwrap();
        assert_eq!(route.route_id, candidate_route_id);
        assert_eq!(route.manage_credential, candidate_manage);
        assert_eq!(route.wake_credential, candidate_wake);
        let collision_registration = post(
            &client,
            &base,
            "/v1/registrations",
            serde_json::json!({"provider":"fcm","device_token":"token-collision"}),
        )
        .await
        .json::<RegistrationPending>()
        .await
        .unwrap();
        let collision_challenge = fake.challenges.lock().unwrap()[1].2.clone();
        assert_eq!(
            post(
                &client,
                &base,
                &format!("/v1/registrations/{}/confirm", collision_registration.registration_id),
                serde_json::json!({"challenge":&collision_challenge,"route_id":candidate_route_id,"manage_credential":"d".repeat(64),"wake_credential":"e".repeat(64)}),
            )
            .await
            .status(),
            StatusCode::UNAUTHORIZED
        );
        let replay = post(
            &client,
            &base,
            &format!("/v1/registrations/{}/confirm", registration.registration_id),
            serde_json::json!({"challenge":&challenge,"route_id":candidate_route_id,"manage_credential":&candidate_manage,"wake_credential":&candidate_wake}),
        )
        .await;
        assert_eq!(replay.status(), StatusCode::CREATED);
        assert_eq!(
            replay.json::<RouteCredentials>().await.unwrap().route_id,
            route.route_id
        );
        for request in [
            serde_json::json!({"challenge":"f".repeat(64),"route_id":candidate_route_id,"manage_credential":&candidate_manage,"wake_credential":&candidate_wake}),
            serde_json::json!({"challenge":&challenge,"route_id":candidate_route_id,"manage_credential":&candidate_manage,"wake_credential":"c".repeat(64)}),
            serde_json::json!({"challenge":&challenge,"route_id":Uuid::new_v4(),"manage_credential":&candidate_manage,"wake_credential":&candidate_wake}),
        ] {
            assert_eq!(
                post(
                    &client,
                    &base,
                    &format!("/v1/registrations/{}/confirm", registration.registration_id),
                    request,
                )
                .await
                .status(),
                StatusCode::UNAUTHORIZED
            );
        }
        assert_eq!(
            post(
                &client,
                &base,
                &format!(
                    "/v1/registrations/{}/confirm",
                    collision_registration.registration_id
                ),
                serde_json::json!({"challenge":&collision_challenge,"route_id":candidate_route_id,"manage_credential":&candidate_manage,"wake_credential":&candidate_wake}),
            )
            .await
            .status(),
            StatusCode::UNAUTHORIZED
        );
        let route_count: i64 =
            sqlx::query_scalar("SELECT count(*) FROM relay_routes WHERE registration_id=$1")
                .bind(registration.registration_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(route_count, 1);
        sqlx::query(
            "UPDATE relay_registrations SET expires_at=now()-interval '1 second' WHERE id=$1",
        )
        .bind(registration.registration_id)
        .execute(&pool)
        .await
        .unwrap();
        assert_eq!(
            post(
                &client,
                &base,
                &format!("/v1/registrations/{}/confirm", registration.registration_id),
                serde_json::json!({"challenge":&challenge,"route_id":candidate_route_id,"manage_credential":&candidate_manage,"wake_credential":&candidate_wake}),
            )
            .await
            .status(),
            StatusCode::CREATED
        );
        assert_eq!(
            post(
                &client,
                &base,
                &format!("/v1/registrations/{}/confirm", registration.registration_id),
                serde_json::json!({"challenge":&challenge,"route_id":candidate_route_id,"manage_credential":"c".repeat(64),"wake_credential":&candidate_wake}),
            )
            .await
            .status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            post(
                &client,
                &base,
                &format!("/v1/registrations/{}/confirm", registration.registration_id),
                serde_json::json!({"challenge":"f".repeat(64)})
            )
            .await
            .status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(post(&client, &base, &format!("/v1/routes/{}/wake", route.route_id), serde_json::json!({"wake_credential":route.manage_credential,"idempotency_id":"one","opaque_nonce":"n1"})).await.status(), StatusCode::UNAUTHORIZED);
        for (id, nonce) in [("one", "n1"), ("two", "n2")] {
            assert_eq!(post(&client, &base, &format!("/v1/routes/{}/wake", route.route_id), serde_json::json!({"wake_credential":route.wake_credential,"idempotency_id":id,"opaque_nonce":nonce})).await.status(), StatusCode::ACCEPTED);
        }
        let job: (String, String) = sqlx::query_as(
            "SELECT idempotency_id,opaque_nonce FROM relay_wake_jobs WHERE route_id=$1",
        )
        .bind(route.route_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(job, ("two".into(), "n2".into()));
        assert!(
            deliver_one(&AppState::with_provider(pool.clone(), fake.clone()))
                .await
                .unwrap()
        );
        assert_eq!(fake.wakes.lock().unwrap().last().unwrap().2, "n2");
        assert_eq!(
            post(
                &client,
                &base,
                &format!("/v1/routes/{}/revoke", route.route_id),
                serde_json::json!({"manage_credential":route.wake_credential})
            )
            .await
            .status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            post(
                &client,
                &base,
                &format!("/v1/routes/{}/revoke", route.route_id),
                serde_json::json!({"manage_credential":route.manage_credential})
            )
            .await
            .status(),
            StatusCode::NO_CONTENT
        );
        assert_eq!(
            post(
                &client,
                &base,
                &format!("/v1/registrations/{}/confirm", registration.registration_id),
                serde_json::json!({"challenge":&challenge,"route_id":candidate_route_id,"manage_credential":&candidate_manage,"wake_credential":&candidate_wake}),
            )
            .await
            .status(),
            StatusCode::UNAUTHORIZED
        );
        let tokens: (String, String) = sqlx::query_as("SELECT r.token,g.token FROM relay_routes r JOIN relay_registrations g ON g.id=r.registration_id WHERE r.id=$1").bind(route.route_id).fetch_one(&pool).await.unwrap();
        assert!(tokens.0.is_empty() && tokens.1.is_empty());
        let jobs: i64 =
            sqlx::query_scalar("SELECT count(*) FROM relay_wake_jobs WHERE route_id=$1")
                .bind(route.route_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(jobs, 0);
        assert_eq!(
            post(
                &client,
                &base,
                "/v1/registrations",
                serde_json::json!({"provider":"fcm","device_token":"x","extra":true})
            )
            .await
            .status(),
            StatusCode::UNPROCESSABLE_ENTITY
        );
        assert_eq!(
            client
                .post(format!("{base}/v1/registrations"))
                .header("content-type", "application/json")
                .body("x".repeat(9000))
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::PAYLOAD_TOO_LARGE
        );
        task.abort();
        cleanup(pool, schema).await;
    }

    #[tokio::test]
    async fn unconfigured_provider_returns_503_with_readyz_warning() {
        let (pool, schema) = test_pool().await;
        let (base, task) = serve(AppState::new(pool.clone())).await;
        let client = reqwest::Client::new();

        // Register should return 503 provider_unconfigured
        let register_resp = post(
            &client,
            &base,
            "/v1/registrations",
            serde_json::json!({"provider":"fcm","device_token":"token-a"}),
        )
        .await;
        assert_eq!(register_resp.status(), StatusCode::SERVICE_UNAVAILABLE);
        let error_text = register_resp.text().await.unwrap();
        assert!(error_text.contains("provider_unconfigured"));

        // No registration should be written
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM relay_registrations")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count, 0);

        // Confirm with a registration ID should also return 503 when unconfigured
        let confirm_resp = post(
            &client,
            &base,
            &format!("/v1/registrations/{}/confirm", Uuid::new_v4()),
            serde_json::json!({"challenge":"f".repeat(64)}),
        )
        .await;
        assert_eq!(confirm_resp.status(), StatusCode::SERVICE_UNAVAILABLE);

        // Wake with a route ID should also return 503 when unconfigured
        let wake_resp = post(
            &client,
            &base,
            &format!("/v1/routes/{}/wake", Uuid::new_v4()),
            serde_json::json!({"wake_credential":"f".repeat(64),"idempotency_id":"id","opaque_nonce":"n"}),
        )
        .await;
        assert_eq!(wake_resp.status(), StatusCode::SERVICE_UNAVAILABLE);

        // No job should be written
        let job_count: i64 = sqlx::query_scalar("SELECT count(*) FROM relay_wake_jobs")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(job_count, 0);

        // readyz should report warning about unconfigured provider
        let ready_resp = client.get(format!("{base}/readyz")).send().await.unwrap();
        assert_eq!(ready_resp.status(), StatusCode::SERVICE_UNAVAILABLE);
        let health_text = ready_resp.text().await.unwrap();
        assert!(health_text.contains("unconfigured"));
        assert!(health_text.contains("no provider adapter configured"));

        let release_resp = client
            .get(format!("{base}/__release"))
            .send()
            .await
            .unwrap();
        assert_eq!(release_resp.status(), StatusCode::OK);
        assert_eq!(
            release_resp.json::<serde_json::Value>().await.unwrap()["revision"],
            "unknown"
        );
        let capabilities = client
            .get(format!("{base}/v1/capabilities"))
            .send()
            .await
            .unwrap();
        assert_eq!(capabilities.status(), StatusCode::OK);
        assert_eq!(
            capabilities.json::<serde_json::Value>().await.unwrap(),
            serde_json::json!({
                "available": false,
                "confirm_candidates": true,
                "providers": [],
                "apns_environment": null,
            })
        );

        task.abort();
        cleanup(pool, schema).await;
    }

    #[test]
    fn trusted_proxy_resolution_is_bounded_and_fails_closed() {
        use axum::http::{HeaderMap, HeaderValue};

        let trusted = trusted_proxy_cidrs(Some("10.0.0.0/8,2001:db8::/32")).unwrap();
        let peer: SocketAddr = "10.0.0.8:443".parse().unwrap();
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-forwarded-for",
            HeaderValue::from_static("198.51.100.9, 10.0.0.4, 2001:db8::2"),
        );
        assert_eq!(
            resolve_client_ip(peer, &headers, &trusted).ip(),
            "198.51.100.9".parse::<IpAddr>().unwrap()
        );

        // An untrusted direct peer cannot spoof its admission identity.
        let direct: SocketAddr = "203.0.113.12:443".parse().unwrap();
        assert_eq!(resolve_client_ip(direct, &headers, &trusted), direct);

        let mut duplicate = headers.clone();
        duplicate.append("x-forwarded-for", HeaderValue::from_static("198.51.100.10"));
        assert_eq!(resolve_client_ip(peer, &duplicate, &trusted), peer);
        headers.insert("x-forwarded-for", HeaderValue::from_static("not-an-ip"));
        assert_eq!(resolve_client_ip(peer, &headers, &trusted), peer);
        headers.insert(
            "x-forwarded-for",
            HeaderValue::from_str(&format!("{}1", "1".repeat(MAX_FORWARDED_HEADER_BYTES))).unwrap(),
        );
        assert_eq!(resolve_client_ip(peer, &headers, &trusted), peer);
    }

    #[test]
    fn proxy_configuration_rejects_invalid_cidrs() {
        assert!(trusted_proxy_cidrs(Some("10.0.0.0/8,not-a-cidr")).is_err());
        assert!(trusted_proxy_cidrs(Some(" 10.0.0.0/8")).is_err());
        assert!(trusted_proxy_cidrs(None).unwrap().is_empty());
    }

    #[test]
    fn wake_admission_limit_is_bounded_and_defaults_to_legacy_value() {
        assert_eq!(wake_admission_limit(None), Ok(120));
        assert_eq!(wake_admission_limit(Some("1")), Ok(1));
        assert_eq!(wake_admission_limit(Some("100000")), Ok(100_000));
        assert!(wake_admission_limit(Some("0")).is_err());
        assert!(wake_admission_limit(Some("100001")).is_err());
        assert!(wake_admission_limit(Some("not-a-number")).is_err());
    }

    #[test]
    fn admission_sources_canonicalize_ipv4_mapped_addresses() {
        let mapped: IpAddr = "::ffff:198.51.100.9".parse().unwrap();
        let ipv4: IpAddr = "198.51.100.9".parse().unwrap();
        assert_eq!(admission_source(mapped), admission_source(ipv4));
        assert_eq!(canonical_source(mapped), canonical_source(ipv4));
    }

    #[test]
    fn confirmation_candidates_must_be_all_or_none() {
        let request = ConfirmRequest {
            challenge: "f".repeat(64),
            installation_public_identity: None,
            route_id: Some(Uuid::new_v4()),
            manage_credential: Some("a".repeat(64)),
            wake_credential: None,
        };
        assert!(confirmation_candidate(&request).is_err());
    }

    #[tokio::test]
    async fn trusted_clients_have_independent_admission_buckets() {
        let (pool, schema) = test_pool().await;
        let state = AppState::with_provider_and_proxy_cidrs(
            pool.clone(),
            Arc::new(FakeProvider::default()),
            trusted_proxy_cidrs(Some("127.0.0.0/8")).unwrap(),
            "test-release".into(),
        );
        let peer: SocketAddr = "127.0.0.1:443".parse().unwrap();
        for client in ["198.51.100.1", "198.51.100.2"] {
            let mut headers = HeaderMap::new();
            headers.insert(
                "x-forwarded-for",
                axum::http::HeaderValue::from_str(client).unwrap(),
            );
            for _ in 0..ADMISSION_LIMIT {
                state.admit_request(peer, &headers).await.unwrap();
            }
            assert!(state.admit_request(peer, &headers).await.is_err());
        }
        cleanup(pool, schema).await;
    }

    #[tokio::test]
    async fn admission_is_atomic_across_independent_relay_instances() {
        let (pool, schema) = test_pool_with_connections(4).await;
        let first = AppState::with_provider(pool.clone(), Arc::new(FakeProvider::default()));
        let second = AppState::with_provider(pool.clone(), Arc::new(FakeProvider::default()));
        let address: SocketAddr = "[2001:db8:1:2::1]:443".parse().unwrap();
        for _ in 0..3 {
            let before = database_window(&pool).await;
            let attempts = u32::from(ADMISSION_LIMIT) * 2;
            let mut tasks = Vec::new();
            for attempt in 0..attempts {
                let state = if attempt % 2 == 0 {
                    first.clone()
                } else {
                    second.clone()
                };
                tasks.push(tokio::spawn(async move {
                    state.admit_registration(address).await
                }));
            }
            let mut admitted = 0;
            for task in tasks {
                admitted += task.await.unwrap().is_ok() as u32;
            }
            if before == database_window(&pool).await {
                assert_eq!(admitted, u32::from(ADMISSION_LIMIT));
                let stored: i32 = sqlx::query_scalar(
                    "SELECT attempts FROM relay_admissions WHERE admission_kind='registration' ORDER BY window_started_at DESC LIMIT 1",
                )
                .fetch_one(&pool)
                .await
                .unwrap();
                assert_eq!(stored, i32::from(ADMISSION_LIMIT));
                cleanup(pool, schema).await;
                return;
            }
        }
        cleanup(pool, schema).await;
        panic!("admission test crossed three database-minute boundaries");
    }

    #[tokio::test]
    async fn admission_cleanup_removes_expired_ledger_rows_and_buckets_ipv6_by_64() {
        let (pool, schema) = test_pool().await;
        sqlx::query(
            "INSERT INTO relay_admissions(admission_kind,source,window_started_at,attempts) VALUES('registration','expired',now()-interval '2 minutes',1)",
        )
        .execute(&pool)
        .await
        .unwrap();
        let state = AppState::with_provider(pool.clone(), Arc::new(FakeProvider::default()));
        assert_eq!(cleanup_expired_admissions(&state).await.unwrap(), 1);
        for attempt in 0..3 {
            let before = database_window(&pool).await;
            let first: SocketAddr = format!("[2001:db8:1:{}::1]:443", attempt + 2)
                .parse()
                .unwrap();
            let second: SocketAddr = format!("[2001:db8:1:{}::2]:443", attempt + 2)
                .parse()
                .unwrap();
            let different_prefix: SocketAddr = format!("[2001:db8:1:{}::1]:443", attempt + 10)
                .parse()
                .unwrap();
            for _ in 0..ADMISSION_LIMIT {
                state.admit_registration(first).await.unwrap();
            }
            let same_prefix_rejected = state.admit_registration(second).await.is_err();
            let different_prefix_admitted =
                state.admit_registration(different_prefix).await.is_ok();
            if before == database_window(&pool).await {
                assert!(same_prefix_rejected);
                assert!(different_prefix_admitted);
                break;
            }
            if attempt == 2 {
                cleanup(pool, schema).await;
                panic!("admission test crossed three database-minute boundaries");
            }
        }
        let expired: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM relay_admissions WHERE source='expired')",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(!expired);
        cleanup(pool, schema).await;
    }

    #[tokio::test]
    async fn wake_admission_is_atomic_across_independent_relay_instances() {
        let (pool, schema) = test_pool_with_connections(2).await;
        let first = AppState::with_provider_proxy_cidrs_and_wake_limit(
            pool.clone(),
            Arc::new(FakeProvider::default()),
            Vec::new(),
            "test-release".into(),
            1,
        );
        let second = AppState::with_provider_proxy_cidrs_and_wake_limit(
            pool.clone(),
            Arc::new(FakeProvider::default()),
            Vec::new(),
            "test-release".into(),
            1,
        );
        for _ in 0..3 {
            let before = database_window(&pool).await;
            let route_id = Uuid::new_v4();
            let (first_result, second_result) = tokio::join!(
                admit_route_wake_in_transaction(&first, &pool, route_id),
                admit_route_wake_in_transaction(&second, &pool, route_id),
            );
            if before == database_window(&pool).await {
                assert_eq!(
                    first_result.is_ok() as usize + second_result.is_ok() as usize,
                    1
                );
                cleanup(pool, schema).await;
                return;
            }
        }
        cleanup(pool, schema).await;
        panic!("wake admission test crossed three database-minute boundaries");
    }

    #[tokio::test]
    async fn invalid_wake_prefilter_does_not_spend_another_route_budget() {
        let (pool, schema) = test_pool().await;
        let state = AppState::with_provider_proxy_cidrs_and_wake_limit(
            pool.clone(),
            Arc::new(FakeProvider::default()),
            Vec::new(),
            "test-release".into(),
            1,
        );
        let attacker: SocketAddr = "[2001:db8:1:2::1]:443".parse().unwrap();
        let authorized: SocketAddr = "[2001:db8:1:2::2]:443".parse().unwrap();
        state
            .prefilter_admit(AdmissionKind::Wake, attacker)
            .await
            .unwrap();
        state
            .prefilter_admit(AdmissionKind::Wake, authorized)
            .await
            .unwrap();
        admit_route_wake_in_transaction(&state, &pool, Uuid::new_v4())
            .await
            .unwrap();
        cleanup(pool, schema).await;
    }

    #[tokio::test]
    async fn admission_database_failure_returns_coded_503() {
        let pool = PgPoolOptions::new()
            .connect_lazy("postgres://unused:unused@localhost:1/unused")
            .unwrap();
        let state = AppState::with_provider(pool, Arc::new(FakeProvider::default()));
        let error = state
            .admit_registration("198.51.100.1:443".parse().unwrap())
            .await
            .unwrap_err();
        let (status, Json(body)) = admission_error(error);
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(body.code, "database_unavailable");
    }

    #[tokio::test]
    async fn simultaneous_workers_lease_one_delivery_without_duplicate_provider_send() {
        struct BlockingProvider {
            started: tokio::sync::Notify,
            release: tokio::sync::Notify,
            wakes: StdMutex<u16>,
        }
        impl PushProvider for BlockingProvider {
            fn configured(&self) -> bool {
                true
            }
            fn supports(&self, _: Provider) -> bool {
                true
            }
            fn provider_name(&self) -> &'static str {
                "blocking-fake"
            }
            fn send_challenge<'a>(
                &'a self,
                _: Provider,
                _: &'a str,
                _: &'a str,
            ) -> ProviderFuture<'a> {
                Box::pin(async { Ok(()) })
            }
            fn send_wake<'a>(&'a self, _: Provider, _: &'a str, _: &'a str) -> ProviderFuture<'a> {
                Box::pin(async move {
                    *self.wakes.lock().unwrap() += 1;
                    self.started.notify_one();
                    self.release.notified().await;
                    Ok(())
                })
            }
        }
        let (pool, schema) = test_pool_with_connections(2).await;
        let route_id = Uuid::new_v4();
        let registration_id = Uuid::new_v4();
        sqlx::query("INSERT INTO relay_registrations(id,provider,token,challenge_digest,expires_at) VALUES($1,'fcm','token',$2,now()+interval '10 minutes')")
            .bind(registration_id).bind(vec![0_u8; 32]).execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO relay_routes(id,registration_id,provider,token,manage_digest,wake_digest) VALUES($1,$2,'fcm','token',$3,$3)")
            .bind(route_id).bind(registration_id).bind(vec![0_u8; 32]).execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO relay_wake_jobs(route_id,idempotency_id,opaque_nonce,state) VALUES($1,'one','nonce','pending')")
            .bind(route_id).execute(&pool).await.unwrap();
        let provider = Arc::new(BlockingProvider {
            started: tokio::sync::Notify::new(),
            release: tokio::sync::Notify::new(),
            wakes: StdMutex::new(0),
        });
        let state = AppState::with_provider(pool.clone(), provider.clone());
        let first_state = state.clone();
        let first = tokio::spawn(async move { deliver_one(&first_state).await });
        provider.started.notified().await;
        assert!(!deliver_one(&state).await.unwrap());
        provider.release.notify_one();
        assert!(first.await.unwrap().unwrap());
        assert_eq!(*provider.wakes.lock().unwrap(), 1);
        cleanup(pool, schema).await;
    }

    #[tokio::test]
    async fn maintenance_pass_reports_closed_pool_failures_without_error_details() {
        let captured = Arc::new(StdMutex::new(Vec::new()));
        let subscriber = Registry::default().with(DiagnosticCapture(captured.clone()));
        let _subscriber = tracing::subscriber::set_default(subscriber);

        let pool = PgPoolOptions::new()
            .connect_lazy("postgres://unused:unused@localhost:1/unused")
            .unwrap();
        pool.close().await;
        let state = AppState::with_provider(pool, Arc::new(FakeProvider::default()));
        run_maintenance_pass(&state).await;

        let diagnostics = captured.lock().unwrap();
        assert_eq!(diagnostics.len(), 2);
        assert!(diagnostics.iter().any(|event| {
            event.contains("operation=wake_delivery")
                && event.contains("error_class=database_failure")
        }));
        assert!(diagnostics.iter().any(|event| {
            event.contains("operation=expired_registration_cleanup")
                && event.contains("error_class=database_failure")
        }));
    }

    #[tokio::test]
    async fn terminal_delivery_failures_emit_fixed_diagnostics_and_empty_queue_is_quiet() {
        let captured = Arc::new(StdMutex::new(Vec::new()));
        let subscriber = Registry::default().with(DiagnosticCapture(captured.clone()));
        let _subscriber = tracing::subscriber::set_default(subscriber);
        let (pool, schema) = test_pool().await;
        let route_id = Uuid::new_v4();
        let registration_id = Uuid::new_v4();
        let route_token = "route-token-sentinel-23";
        let opaque_nonce = "opaque-nonce-sentinel-47";
        sqlx::query("INSERT INTO relay_registrations(id,provider,token,challenge_digest,expires_at) VALUES($1,'fcm',$2,$3,now()+interval '10 minutes')")
            .bind(registration_id)
            .bind(route_token)
            .bind(vec![0_u8; 32])
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO relay_routes(id,registration_id,provider,token,manage_digest,wake_digest) VALUES($1,$2,'fcm',$3,$4,$4)")
            .bind(route_id)
            .bind(registration_id)
            .bind(route_token)
            .bind(vec![0_u8; 32])
            .execute(&pool)
            .await
            .unwrap();
        let provider = Arc::new(FailingProvider(StdMutex::new(vec![
            ProviderError::Permanent,
            ProviderError::Retryable,
        ])));
        let state = AppState::with_provider(pool.clone(), provider);
        for (idempotency_id, attempts) in [("idem-a-7f3", 0), ("idem-b-9k2", 8)] {
            sqlx::query("INSERT INTO relay_wake_jobs(route_id,idempotency_id,opaque_nonce,state,attempts) VALUES($1,$2,$3,'pending',$4)")
                .bind(route_id)
                .bind(idempotency_id)
                .bind(opaque_nonce)
                .bind(attempts)
                .execute(&pool)
                .await
                .unwrap();
            assert!(deliver_one(&state).await.unwrap());
            sqlx::query("DELETE FROM relay_wake_jobs WHERE route_id=$1")
                .bind(route_id)
                .execute(&pool)
                .await
                .unwrap();
        }

        assert!(!deliver_one(&state).await.unwrap());
        let diagnostics = captured.lock().unwrap().join("\n");
        assert!(diagnostics.contains("operation=wake_delivery"));
        assert!(diagnostics.contains("error_class=provider_permanent"));
        assert!(diagnostics.contains("error_class=provider_retry_exhausted"));
        assert_eq!(captured.lock().unwrap().len(), 2);
        for secret in [route_token, opaque_nonce, "idem-a-7f3", "idem-b-9k2"] {
            assert!(!diagnostics.contains(secret));
        }
        cleanup(pool, schema).await;
    }

    #[tokio::test]
    async fn admission_limits_and_caps_concurrent_registration_attempts() {
        let (pool, schema) = test_pool().await;
        let (base, task) = serve(AppState::with_provider(
            pool.clone(),
            Arc::new(FakeProvider::default()),
        ))
        .await;
        let client = reqwest::Client::builder()
            .pool_max_idle_per_host(0)
            .build()
            .unwrap();

        // Send 21 valid register requests from 127.0.0.1 within the same window
        let mut responses = Vec::new();
        for i in 0..21 {
            let resp = post(
                &client,
                &base,
                "/v1/registrations",
                serde_json::json!({"provider":"fcm","device_token":format!("token-{i}")}),
            )
            .await;
            responses.push(resp.status());
        }

        // First 20 should succeed (CREATED)
        for (i, status) in responses.iter().enumerate().take(20) {
            assert_eq!(
                *status,
                StatusCode::CREATED,
                "request {i} should succeed with CREATED"
            );
        }

        // 21st should be rate limited (TOO_MANY_REQUESTS)
        assert_eq!(
            responses[20],
            StatusCode::TOO_MANY_REQUESTS,
            "21st request should be rate limited"
        );

        // All 20 registrations should be written to database
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM relay_registrations")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count, 20);

        task.abort();
        cleanup(pool, schema).await;
    }

    #[tokio::test]
    async fn custom_wake_admission_limit_is_enforced_over_http() {
        let (pool, schema) = test_pool().await;
        let (base, task) = serve(AppState::with_provider_proxy_cidrs_and_wake_limit(
            pool.clone(),
            Arc::new(FakeProvider::default()),
            Vec::new(),
            "test-release".into(),
            1,
        ))
        .await;
        let client = reqwest::Client::new();
        let request = serde_json::json!({
            "wake_credential": "a".repeat(64),
            "idempotency_id": "one",
            "opaque_nonce": "nonce",
        });
        assert_eq!(
            post(
                &client,
                &base,
                &format!("/v1/routes/{}/wake", Uuid::new_v4()),
                request.clone()
            )
            .await
            .status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            post(
                &client,
                &base,
                &format!("/v1/routes/{}/wake", Uuid::new_v4()),
                request
            )
            .await
            .status(),
            StatusCode::TOO_MANY_REQUESTS
        );
        task.abort();
        cleanup(pool, schema).await;
    }

    #[tokio::test]
    async fn management_revoke_has_admission_independent_of_wakes() {
        let (pool, schema) = test_pool().await;
        let state = AppState::with_provider_proxy_cidrs_and_wake_limit(
            pool.clone(),
            Arc::new(FakeProvider::default()),
            trusted_proxy_cidrs(Some("127.0.0.0/8")).unwrap(),
            "test-release".into(),
            1,
        );
        let (base, task) = serve(state).await;
        let client = reqwest::Client::new();
        let manage_credential = "a".repeat(64);
        let wake_credential = "b".repeat(64);

        for (wake_source, revoke_source, post_revoke_source) in [
            ("198.51.100.1", "198.51.100.2", "198.51.100.4"),
            ("198.51.100.3", "198.51.100.3", "198.51.100.5"),
        ] {
            let registration_id = Uuid::new_v4();
            let route_id = Uuid::new_v4();
            sqlx::query("INSERT INTO relay_registrations(id,provider,token,challenge_digest,expires_at) VALUES($1,'fcm','token',$2,now()+interval '10 minutes')")
                .bind(registration_id).bind(vec![0_u8; 32]).execute(&pool).await.unwrap();
            sqlx::query("INSERT INTO relay_routes(id,registration_id,provider,token,manage_digest,wake_digest) VALUES($1,$2,'fcm','token',$3,$4)")
                .bind(route_id).bind(registration_id).bind(digest(&manage_credential)).bind(digest(&wake_credential)).execute(&pool).await.unwrap();

            let wake = client
                .post(format!("{base}/v1/routes/{route_id}/wake"))
                .header("x-forwarded-for", wake_source)
                .json(&serde_json::json!({"wake_credential":&wake_credential,"idempotency_id":"one","opaque_nonce":"nonce"}))
                .send()
                .await
                .unwrap();
            assert_eq!(wake.status(), StatusCode::ACCEPTED);

            let revoke = client
                .post(format!("{base}/v1/routes/{route_id}/revoke"))
                .header("x-forwarded-for", revoke_source)
                .json(&serde_json::json!({"manage_credential":&manage_credential}))
                .send()
                .await
                .unwrap();
            assert_eq!(revoke.status(), StatusCode::NO_CONTENT);
            let jobs: i64 =
                sqlx::query_scalar("SELECT count(*) FROM relay_wake_jobs WHERE route_id=$1")
                    .bind(route_id)
                    .fetch_one(&pool)
                    .await
                    .unwrap();
            assert_eq!(jobs, 0);
            let revoked: bool =
                sqlx::query_scalar("SELECT revoked_at IS NOT NULL FROM relay_routes WHERE id=$1")
                    .bind(route_id)
                    .fetch_one(&pool)
                    .await
                    .unwrap();
            assert!(revoked);

            let after_revoke = client
                .post(format!("{base}/v1/routes/{route_id}/wake"))
                .header("x-forwarded-for", post_revoke_source)
                .json(&serde_json::json!({"wake_credential":&wake_credential,"idempotency_id":"two","opaque_nonce":"nonce"}))
                .send()
                .await
                .unwrap();
            assert_eq!(after_revoke.status(), StatusCode::UNAUTHORIZED);
            let jobs: i64 =
                sqlx::query_scalar("SELECT count(*) FROM relay_wake_jobs WHERE route_id=$1")
                    .bind(route_id)
                    .fetch_one(&pool)
                    .await
                    .unwrap();
            assert_eq!(jobs, 0);
        }

        task.abort();
        cleanup(pool, schema).await;
    }

    #[tokio::test]
    async fn admission_map_cap_protects_against_unbounded_ipv6_state() {
        let pool = PgPoolOptions::new()
            .connect_lazy("postgres://unused:unused@localhost:1/unused")
            .unwrap();
        let state = AppState::with_provider(pool, Arc::new(FakeProvider::default()));
        for segment in 0..ADMISSION_MAP_CAP as u16 {
            state
                .prefilter_admit(
                    AdmissionKind::Registration,
                    SocketAddr::new(
                        IpAddr::V6(std::net::Ipv6Addr::new(0x2001, segment, 0, 0, 0, 0, 0, 0)),
                        1,
                    ),
                )
                .await
                .unwrap();
        }
        assert!(
            state
                .prefilter_admit(
                    AdmissionKind::Registration,
                    SocketAddr::new(
                        IpAddr::V6(std::net::Ipv6Addr::new(0x2001, 0, 0, 0, 0, 0, 0, 0)),
                        1,
                    )
                )
                .await
                .is_ok()
        );
        assert!(
            state
                .prefilter_admit(
                    AdmissionKind::Registration,
                    SocketAddr::new(
                        IpAddr::V6(std::net::Ipv6Addr::new(0x2001, 4096, 0, 0, 0, 0, 0, 0)),
                        1,
                    )
                )
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn cleanup_expired_unconfirmed_registrations_is_bounded_and_preserves_confirmed() {
        let (pool, schema) = test_pool().await;
        for _ in 0..=EXPIRED_REGISTRATION_CLEANUP_LIMIT {
            sqlx::query("INSERT INTO relay_registrations(id,provider,token,challenge_digest,expires_at) VALUES($1,'fcm','token',$2,now()-interval '1 second')")
                .bind(Uuid::new_v4())
                .bind(vec![0_u8; 32])
                .execute(&pool)
                .await
                .unwrap();
        }
        let confirmed = Uuid::new_v4();
        sqlx::query("INSERT INTO relay_registrations(id,provider,token,challenge_digest,expires_at,confirmed_at) VALUES($1,'fcm','token',$2,now()-interval '1 second',now())")
            .bind(confirmed)
            .bind(vec![0_u8; 32])
            .execute(&pool)
            .await
            .unwrap();
        let state = AppState::with_provider(pool.clone(), Arc::new(FakeProvider::default()));
        assert_eq!(cleanup_expired_registrations(&state).await.unwrap(), 100);
        assert_eq!(cleanup_expired_registrations(&state).await.unwrap(), 1);
        let retained: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM relay_registrations WHERE id=$1)")
                .bind(confirmed)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert!(retained);
        cleanup(pool, schema).await;
    }

    #[tokio::test]
    async fn real_router_test_additional_cases() {
        let (pool, schema) = test_pool().await;
        let fake = Arc::new(FakeProvider::default());
        let (base, task) = serve(AppState::with_provider(pool.clone(), fake.clone())).await;
        let client = reqwest::Client::new();

        // Setup: register and confirm
        let registered = post(
            &client,
            &base,
            "/v1/registrations",
            serde_json::json!({"provider":"apns","device_token":"apple-token"}),
        )
        .await;
        assert_eq!(registered.status(), StatusCode::CREATED);
        let registration: RegistrationPending = registered.json().await.unwrap();
        let challenge = fake.challenges.lock().unwrap()[0].2.clone();

        let confirmed = post(
            &client,
            &base,
            &format!("/v1/registrations/{}/confirm", registration.registration_id),
            serde_json::json!({"challenge": challenge}),
        )
        .await;
        assert_eq!(confirmed.status(), StatusCode::CREATED);
        let route: RouteCredentials = confirmed.json().await.unwrap();

        // Test: cross-route wake denial (wake with route_id from a different route)
        // First, create a second route
        let registered2 = post(
            &client,
            &base,
            "/v1/registrations",
            serde_json::json!({"provider":"apns","device_token":"apple-token-2"}),
        )
        .await;
        assert_eq!(registered2.status(), StatusCode::CREATED);
        let registration2: RegistrationPending = registered2.json().await.unwrap();
        let challenge2 = fake.challenges.lock().unwrap()[1].2.clone();

        let confirmed2 = post(
            &client,
            &base,
            &format!(
                "/v1/registrations/{}/confirm",
                registration2.registration_id
            ),
            serde_json::json!({"challenge": challenge2}),
        )
        .await;
        assert_eq!(confirmed2.status(), StatusCode::CREATED);
        let route2: RouteCredentials = confirmed2.json().await.unwrap();

        // Try to wake route1 with route2's wake_credential should fail
        assert_eq!(
            post(
                &client,
                &base,
                &format!("/v1/routes/{}/wake", route.route_id),
                serde_json::json!({"wake_credential":route2.wake_credential,"idempotency_id":"id","opaque_nonce":"nonce"})
            )
            .await
            .status(),
            StatusCode::UNAUTHORIZED
        );

        // Test: confirm with expired challenge (wait past expiry is not practical in unit test,
        // so we verify the database check works by manually expiring)
        let registration3 = post(
            &client,
            &base,
            "/v1/registrations",
            serde_json::json!({"provider":"fcm","device_token":"fcm-token"}),
        )
        .await;
        assert_eq!(registration3.status(), StatusCode::CREATED);
        let reg3: RegistrationPending = registration3.json().await.unwrap();
        let challenge3 = fake.challenges.lock().unwrap()[2].2.clone();

        // Manually expire the registration in the database
        sqlx::query(
            "UPDATE relay_registrations SET expires_at = now() - interval '1 second' WHERE id=$1",
        )
        .bind(reg3.registration_id)
        .execute(&pool)
        .await
        .unwrap();

        // Try to confirm the expired registration
        let confirm_expired = post(
            &client,
            &base,
            &format!("/v1/registrations/{}/confirm", reg3.registration_id),
            serde_json::json!({"challenge":challenge3}),
        )
        .await;
        assert_eq!(confirm_expired.status(), StatusCode::UNAUTHORIZED);

        // Test: wake and revoke, then try to wake again (post-revoke denial)
        let wake_resp = post(
            &client,
            &base,
            &format!("/v1/routes/{}/wake", route.route_id),
            serde_json::json!({"wake_credential":route.wake_credential,"idempotency_id":"id","opaque_nonce":"nonce"})
        )
        .await;
        assert_eq!(wake_resp.status(), StatusCode::ACCEPTED);

        // Revoke the route
        let revoke_resp = post(
            &client,
            &base,
            &format!("/v1/routes/{}/revoke", route.route_id),
            serde_json::json!({"manage_credential":route.manage_credential}),
        )
        .await;
        assert_eq!(revoke_resp.status(), StatusCode::NO_CONTENT);

        // Try to wake the revoked route
        let wake_revoked = post(
            &client,
            &base,
            &format!("/v1/routes/{}/wake", route.route_id),
            serde_json::json!({"wake_credential":route.wake_credential,"idempotency_id":"id2","opaque_nonce":"nonce2"})
        )
        .await;
        assert_eq!(wake_revoked.status(), StatusCode::UNAUTHORIZED);

        task.abort();
        cleanup(pool, schema).await;
    }
}
