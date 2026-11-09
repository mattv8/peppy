mod account_access;
pub(crate) mod admission;
mod attachments;
mod drafts;
mod instance;
mod key_profiles;
mod mcp;
mod sync;
mod vault_identity;

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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeviceAttachmentRetentionError {
    Unsupported,
    Forbidden,
    Unavailable,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeviceAccountAccessError {
    Unsupported,
    Forbidden,
    Unavailable,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AttachmentStoragePolicy {
    pub allow_upload: bool,
    pub limit_bytes: i64,
    pub cleanup_authorized: bool,
    pub policy_version: Option<String>,
}

pub trait AccessPolicy: Send + Sync {
    fn authorize(&self, principal: AccessPrincipal, operation: Operation)
    -> PolicyFuture<'_, bool>;
    fn attachment_quota_bytes(&self, _principal: AccessPrincipal) -> PolicyFuture<'_, Option<i64>> {
        Box::pin(async { None })
    }
    fn attachment_storage_policy_in_transaction<'a>(
        &'a self,
        _principal: AccessPrincipal,
        configured_limit_bytes: i64,
        _connection: &'a mut sqlx::PgConnection,
    ) -> PolicyFuture<'a, Result<AttachmentStoragePolicy, sqlx::Error>> {
        Box::pin(async move {
            Ok(AttachmentStoragePolicy {
                allow_upload: true,
                limit_bytes: configured_limit_bytes,
                cleanup_authorized: false,
                policy_version: None,
            })
        })
    }
    fn device_attachment_storage_status(
        &self,
        _principal: AccessPrincipal,
    ) -> PolicyFuture<
        '_,
        Result<
            peppy_hosted_api::attachments::AttachmentStorageStatusResponse,
            DeviceAttachmentRetentionError,
        >,
    > {
        Box::pin(async { Err(DeviceAttachmentRetentionError::Unsupported) })
    }
    /// Owns authorization for this read. The handler authenticates the device only.
    fn device_account_access(
        &self,
        _principal: AccessPrincipal,
    ) -> PolicyFuture<
        '_,
        Result<peppy_hosted_api::account_access::AccountAccessResponse, DeviceAccountAccessError>,
    > {
        Box::pin(async { Err(DeviceAccountAccessError::Unsupported) })
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
    CreateComputerEnrollment,
    ReadComputerEnrollment,
    ApproveComputerEnrollment,
    ConfirmComputerEnrollment,
    CancelComputerEnrollment,
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
    Extension, Json, Router,
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
use peppy_domain::{DeviceId, VaultId};
use peppy_hosted_api::{DeviceRosterDevice, DeviceRosterResponse};
use peppy_hosted_api::{
    PushConfig, PushConfigAdvertisement,
    enrollment::{
        ClaimInstanceRequest, ClaimInstanceResponse, ClientKind, DeploymentMode, EnrollmentDevice,
        EnrollmentRole, InstanceAccount, InstanceInfo, InstanceLoginRequest, InstanceManagement,
        InstanceSession, VaultMaterial,
    },
};
use peppy_protocol::{
    Envelope, EnvelopePurpose, LiveFrameV2, LiveHelloV2, MEMBERSHIP_PROTOCOL_VERSION,
    MembershipBootstrapV1, MembershipControlV1, MembershipDeviceV1, MembershipRoleV1,
    MembershipVersionV1,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{Executor, PgPool, Row};
use std::{
    collections::HashMap,
    convert::Infallible,
    net::{IpAddr, SocketAddr},
    sync::Arc,
    time::Duration,
};
use uuid::Uuid;

use crate::{
    config::Config,
    live_ticket_work::{
        ExecutionScope, OperationDeadline, TenantDevice, TicketDbWorkError, TicketDbWorkRegistry,
        TicketWorkflow,
    },
    scope,
    storage::Storage,
};

const TOKEN_BYTES_HEX: usize = 96;
/// Fixed dummy hash checked for unknown usernames so login spends comparable Argon2id
/// time. Its parameters match the libsodium INTERACTIVE limits used for stored admin hashes.
const DUMMY_ADMIN_PASSWORD_HASH: &str =
    "$argon2id$v=19$m=65536,t=2,p=1$c2FsdHNhbHRzYWx0cw$EwM3PQ8nhmqXBdGiYHhvPYGBuA1jw0dz45Etz12Z1TQ";

#[cfg(test)]
#[derive(Clone)]
struct LiveTicketTestGate {
    inner: Arc<LiveTicketTestGateInner>,
}

#[cfg(test)]
struct LiveTicketTestGateInner {
    block_calls: usize,
    entered: std::sync::atomic::AtomicUsize,
    entered_notify: tokio::sync::Notify,
    releases: tokio::sync::Semaphore,
}

#[cfg(test)]
impl LiveTicketTestGate {
    fn new(block_calls: usize) -> Self {
        Self {
            inner: Arc::new(LiveTicketTestGateInner {
                block_calls,
                entered: std::sync::atomic::AtomicUsize::new(0),
                entered_notify: tokio::sync::Notify::new(),
                releases: tokio::sync::Semaphore::new(0),
            }),
        }
    }

    async fn enter(&self) {
        self.enter_after(0).await;
    }

    async fn enter_after(&self, skip_calls: usize) {
        let call = self
            .inner
            .entered
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            + 1;
        self.inner.entered_notify.notify_waiters();
        if call > skip_calls && call <= skip_calls + self.inner.block_calls {
            self.inner
                .releases
                .acquire()
                .await
                .expect("live ticket test gate remains open")
                .forget();
        }
    }

    async fn wait_for_entries(&self, expected: usize) {
        loop {
            let notified = self.inner.entered_notify.notified();
            if self.inner.entered.load(std::sync::atomic::Ordering::SeqCst) >= expected {
                return;
            }
            notified.await;
        }
    }

    fn release_one(&self) {
        self.inner.releases.add_permits(1);
    }

    fn entries(&self) -> usize {
        self.inner.entered.load(std::sync::atomic::Ordering::SeqCst)
    }
}

#[cfg(test)]
#[derive(Default)]
struct LiveTicketWorkProbe {
    cancellation_receivers: std::sync::Mutex<Vec<tokio::sync::watch::Receiver<bool>>>,
    receiver_recorded: tokio::sync::Notify,
    workflows_disposed: std::sync::atomic::AtomicUsize,
    workflow_disposed: tokio::sync::Notify,
}

#[cfg(test)]
impl LiveTicketWorkProbe {
    fn record_receiver(&self, receiver: tokio::sync::watch::Receiver<bool>) {
        self.cancellation_receivers
            .lock()
            .expect("work cancellation receiver lock poisoned")
            .push(receiver);
        self.receiver_recorded.notify_waiters();
    }

    async fn wait_for_receiver(&self, index: usize) -> tokio::sync::watch::Receiver<bool> {
        loop {
            let notified = self.receiver_recorded.notified();
            if let Some(receiver) = self
                .cancellation_receivers
                .lock()
                .expect("work cancellation receiver lock poisoned")
                .get(index)
                .cloned()
            {
                return receiver;
            }
            notified.await;
        }
    }

    async fn wait_for_cancellation(&self, index: usize) {
        let mut receiver = self.wait_for_receiver(index).await;
        while !*receiver.borrow_and_update() {
            receiver
                .changed()
                .await
                .expect("work cancellation sender remains alive until cancellation");
        }
    }

    fn record_disposal(&self) {
        self.workflows_disposed
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.workflow_disposed.notify_waiters();
    }

    async fn wait_for_disposals(&self, expected: usize) {
        loop {
            let notified = self.workflow_disposed.notified();
            if self
                .workflows_disposed
                .load(std::sync::atomic::Ordering::SeqCst)
                >= expected
            {
                return;
            }
            notified.await;
        }
    }
}

#[cfg(test)]
struct LiveTicketWorkflowDropProbe(Arc<LiveTicketWorkProbe>);

#[cfg(test)]
impl Drop for LiveTicketWorkflowDropProbe {
    fn drop(&mut self) {
        self.0.record_disposal();
    }
}

#[cfg(test)]
#[derive(Clone, Default)]
struct LiveTicketTestHooks {
    operation_budget: Option<Duration>,
    recheck_operation_budget: Option<Duration>,
    http_operation_budget: Option<Duration>,
    recheck_interval: Option<Duration>,
    lease_duration: Option<Duration>,
    clock_ms: Option<Arc<std::sync::atomic::AtomicU64>>,
    after_device_claim: Option<LiveTicketTestGate>,
    after_sample: Option<LiveTicketTestGate>,
    before_commit: Option<LiveTicketTestGate>,
    after_commit: Option<LiveTicketTestGate>,
    after_validation: Option<LiveTicketTestGate>,
    membership_after_version: Option<LiveTicketTestGate>,
    sampled_at: Arc<std::sync::Mutex<Vec<String>>>,
    credential_query_started: Arc<std::sync::atomic::AtomicUsize>,
    session_query_started: Arc<std::sync::atomic::AtomicUsize>,
    renewal_update_started: Arc<std::sync::atomic::AtomicUsize>,
    commit_started: Arc<std::sync::atomic::AtomicUsize>,
    work_probe: Arc<LiveTicketWorkProbe>,
}

#[derive(Clone)]
pub(super) struct ApiState {
    pub(super) db: PgPool,
    committed: tokio::sync::broadcast::Sender<(Uuid, i64)>,
    presence_changed: tokio::sync::broadcast::Sender<Uuid>,
    drafts_changed: tokio::sync::broadcast::Sender<Uuid>,
    presence_touches: Arc<std::sync::Mutex<HashMap<(Uuid, Uuid), std::time::Instant>>>,
    storage: Option<Storage>,
    vault_attachment_quota_bytes: i64,
    upload_slots: Arc<tokio::sync::Semaphore>,
    options: Arc<TransportOptions>,
    instance_account_admissions: Arc<tokio::sync::Mutex<admission::LocalAdmissions>>,
    mcp_oauth_admissions: Arc<tokio::sync::Mutex<admission::LocalAdmissions>>,
    instance_password_slots: Arc<tokio::sync::Semaphore>,
    pub(super) instance_events: tokio::sync::broadcast::Sender<()>,
    pub(super) event_stream_slots: Arc<tokio::sync::Semaphore>,
    pub(super) event_stream_sessions: Arc<std::sync::Mutex<HashMap<Vec<u8>, usize>>>,
    policy: Arc<dyn AccessPolicy>,
    push_config: PushConfig,
    row_security: bool,
    rls_enforced: bool,
    instance_origin: Option<url::Url>,
    claim_policy: crate::instance::ClaimPolicy,
    setup_window_started: std::time::Instant,
    trusted_proxy_cidrs: Arc<Vec<IpNet>>,
    live_trusted_origin: Option<url::Origin>,
    live_hub: crate::live::LiveHub,
    live_ticket_work: TicketDbWorkRegistry,
    live_backend: Arc<crate::live_postgres::PgLiveBackend>,
    #[cfg(test)]
    live_backend_override: Option<Arc<dyn crate::live::LiveBackend>>,
    #[cfg(test)]
    live_publish_timeout: Duration,
    #[cfg(test)]
    live_ticket_test_hooks: LiveTicketTestHooks,
    live_readiness: tokio::sync::watch::Receiver<bool>,
    live_rate_limiter: Arc<std::sync::Mutex<crate::live::LiveRateLimiter>>,
    live_upgrade_limiter: Arc<std::sync::Mutex<crate::live::LiveRateLimiter>>,
    live_ticket_http_limiter: Arc<std::sync::Mutex<crate::live::LiveRateLimiter>>,
    pub(super) mcp_worker: Option<McpWorkerClient>,
    pub(super) enrollment_authorization: Arc<dyn EnrollmentAuthorization>,
    pub(super) mcp_consent_origin: Option<String>,
}

#[derive(Clone)]
pub(super) struct McpWorkerClient {
    pub(super) url: url::Url,
    pub(super) token_file: std::path::PathBuf,
    pub(super) client: reqwest::Client,
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

/// Builds a storage-less API for tests that do not require attachments.
/// Production startup must use [`crate::ServerBuilder`] so storage is verified.
pub fn router(db: PgPool) -> Router {
    router_with_initialized_storage(db, None)
}

pub(crate) fn router_with_initialized_storage(
    db: PgPool,
    initialized_storage: Option<Storage>,
) -> Router {
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
        initialized_storage,
        options,
        None,
        PushConfigAdvertisement::default(),
        RouterAuthorization {
            policy: Arc::new(CommunityAccessPolicy),
            enrollment: authorization,
            claim_policy: crate::instance::configured_claim_policy(false)
                .expect("setup token configuration must be valid"),
            row_security: false,
            rls_enforced: false,
        },
    )
    .expect("router helper requires a valid enrollment origin");
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
            rls_enforced: false,
        },
    )
    .expect("router helper requires a valid enrollment origin");
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
            rls_enforced: false,
        },
    )
    .expect("router helper requires a valid enrollment origin");
    maintenance.start();
    router
}

pub(crate) struct RouterAuthorization {
    pub(crate) policy: Arc<dyn AccessPolicy>,
    pub(crate) enrollment: Arc<dyn EnrollmentAuthorization>,
    pub(crate) claim_policy: crate::instance::ClaimPolicy,
    pub(crate) row_security: bool,
    pub(crate) rls_enforced: bool,
}

pub(crate) struct Maintenance {
    pub(crate) db: PgPool,
    committed: tokio::sync::broadcast::Sender<(Uuid, i64)>,
    storage: Option<Storage>,
    options: TransportOptions,
    relay_url: Option<url::Url>,
    mcp_worker: Option<McpWorkerClient>,
}

impl Maintenance {
    pub(crate) fn start(self) {
        sync::spawn_maintenance(self.db.clone(), &self.committed, self.storage, self.options);
        spawn_instance_maintenance(self.db.clone());
        mcp::spawn_oauth_maintenance(self.db.clone());
        mcp::spawn_cleanup_maintenance(self.db.clone(), self.mcp_worker.clone());
        if let Some(relay_url) = self.relay_url {
            spawn_wake_maintenance(self.db, &self.committed, relay_url);
        }
    }
}

fn spawn_instance_maintenance(db: PgPool) {
    let live_ticket_db = db.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(60));
        loop {
            interval.tick().await;
            if let Err(error) = crate::live_ticket_maintenance::reap_live_tickets(
                &live_ticket_db,
                crate::live_ticket_maintenance::LIVE_TICKET_REAP_BATCH,
            )
            .await
            {
                tracing::warn!(
                    kind = database_error_kind(&error),
                    "live ticket retention sweep failed"
                );
            }
        }
    });
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
            let result = sqlx::query(
                "DELETE FROM instance_admission_windows WHERE ctid IN (SELECT ctid FROM instance_admission_windows WHERE window_started < date_trunc('minute', now()) - interval '2 hours' ORDER BY window_started LIMIT 1000 FOR UPDATE SKIP LOCKED)",
            )
            .execute(&db)
            .await;
            if let Err(error) = result {
                tracing::warn!(%error, "instance admission retention sweep failed");
            }
            let result = crate::enrollment::computers::cleanup_expired_enrollments(&db).await;
            if let Err(error) = result {
                tracing::warn!(%error, "computer enrollment retention sweep failed");
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
) -> Result<(Router, Maintenance), &'static str> {
    build_router_with_live_backend(
        db,
        RouterBuildOptions {
            config,
            initialized_storage,
            transport: options,
            explicit_relay_url,
            push_advertisement,
        },
        authorization,
        #[cfg(test)]
        None,
    )
}

struct RouterBuildOptions {
    config: Option<Config>,
    initialized_storage: Option<Storage>,
    transport: TransportOptions,
    explicit_relay_url: Option<url::Url>,
    push_advertisement: PushConfigAdvertisement,
}

#[cfg(test)]
struct LiveBackendOverride {
    backend: Arc<dyn crate::live::LiveBackend>,
    readiness: tokio::sync::watch::Receiver<bool>,
    publish_timeout: Duration,
    hub_observer: Option<Arc<std::sync::Mutex<Option<crate::live::LiveHub>>>>,
    state_observer: Option<Arc<std::sync::Mutex<Option<ApiState>>>>,
    ticket_hooks: LiveTicketTestHooks,
}

fn build_router_with_live_backend(
    db: PgPool,
    build: RouterBuildOptions,
    authorization: RouterAuthorization,
    #[cfg(test)] live_override: Option<LiveBackendOverride>,
) -> Result<(Router, Maintenance), &'static str> {
    let RouterBuildOptions {
        config,
        initialized_storage,
        transport: options,
        explicit_relay_url,
        push_advertisement,
    } = build;
    let RouterAuthorization {
        policy,
        enrollment: enrollment_authorization,
        claim_policy,
        row_security,
        rls_enforced,
    } = authorization;
    let (committed, _) = tokio::sync::broadcast::channel(HINT_CAPACITY);
    let (presence_changed, _) = tokio::sync::broadcast::channel(HINT_CAPACITY);
    let (drafts_changed, _) = tokio::sync::broadcast::channel(HINT_CAPACITY);
    let storage = initialized_storage;
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
    let live_trusted_origin = config
        .as_ref()
        .and_then(|config| config.web_live_trusted_origin.clone());
    let mcp_worker = config
        .as_ref()
        .and_then(|config| {
            config
                .mcp_worker_url
                .clone()
                .zip(config.mcp_worker_token_file.clone())
        })
        .map(|(url, token_file)| McpWorkerClient {
            url,
            token_file,
            client: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(5))
                .build()
                .expect("fixed worker client configuration is valid"),
        });
    let mcp_consent_origin = config.as_ref().and_then(|config| {
        let api = config.public_api_url.as_ref()?;
        let web = config.web_client.as_ref()?;
        let api_host = api.host_str()?;
        let host = web.host.trim_end_matches('.');
        if web.serves_all_hosts() || host.eq_ignore_ascii_case(api_host) {
            let mut origin = api.clone();
            origin.set_host(Some(host)).ok()?;
            Some(origin.origin().ascii_serialization())
        } else {
            Some(format!("{}://{}", api.scheme(), host))
        }
    });
    let push_config_response = push_advertisement
        .response()
        .expect("router requires a validated push configuration");
    let live_hub = crate::live::LiveHub::new();
    let live_backend = Arc::new(crate::live_postgres::PgLiveBackend::new(
        db.clone(),
        live_hub.clone(),
    ));
    let live_readiness = live_backend.subscribe_readiness();
    #[cfg(test)]
    let (
        live_backend_override,
        live_readiness,
        live_publish_timeout,
        live_ticket_test_hooks,
        live_state_observer,
    ) = match live_override {
        Some(live_override) => {
            if let Some(hub_observer) = live_override.hub_observer {
                *hub_observer
                    .lock()
                    .expect("live hub observer lock poisoned") = Some(live_hub.clone());
            }
            (
                Some(live_override.backend),
                live_override.readiness,
                live_override.publish_timeout,
                live_override.ticket_hooks,
                live_override.state_observer,
            )
        }
        None => (
            None,
            live_readiness,
            LIVE_PUBLISH_TIMEOUT,
            LiveTicketTestHooks::default(),
            None,
        ),
    };
    let router = Router::new()
        .route("/v1/vault", get(vault_header))
        .route("/v1/devices/{device_id}/revoke", post(revoke_device))
        .route("/v1/devices/self/push-config", get(push_config))
        .route(
            "/v1/devices/self/retention-capability",
            put(attachments::set_retention_capability),
        )
        .route(
            "/v1/devices/self/wake-route",
            put(register_wake_route).delete(revoke_wake_route),
        )
        .route("/v1/devices", get(devices))
        .merge(mcp::routes())
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
        .route("/v1/drafts", get(drafts::list))
        .route(
            "/v1/drafts/{draft_id}/revisions/{revision_id}",
            put(drafts::put),
        )
        .route("/v1/drafts/{draft_id}/send", post(drafts::send))
        .route("/v1/drafts/{draft_id}/discard", post(drafts::discard))
        .route(
            "/v1/vault/key-profiles",
            get(key_profiles::list).post(key_profiles::register),
        )
        .route(
            "/v1/vault/key-profiles/{key_epoch}/activate",
            post(key_profiles::activate),
        )
        .route(
            "/v1/vault/identity",
            get(vault_identity::get).post(vault_identity::establish),
        )
        .route("/v1/vault", delete(delete_vault))
        .route("/v1/commands/{command_id}/receipts", post(receipt))
        .route(
            "/v1/attachments/reserve",
            post(attachments::reserve_attachment),
        )
        .route("/v1/attachments/usage", get(attachments::attachment_usage))
        .route(
            peppy_hosted_api::attachments::routes::DEVICE_ATTACHMENT_STORAGE_STATUS,
            get(attachments::device_attachment_storage_status),
        )
        .route(
            peppy_hosted_api::account_access::routes::DEVICE_ACCOUNT_ACCESS,
            get(account_access::device_account_access),
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
            "/v1/attachments/{attachment_id}/retention-uses",
            post(attachments::register_retention_use),
        )
        .route(
            "/v1/attachments/{attachment_id}/retention-uses/{use_id}/release",
            post(attachments::release_retention_use),
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
        .route("/v1/ws", get(websocket))
        .route(
            "/v1/live/tickets",
            post(issue_live_ticket).delete(close_live_ticket_session),
        )
        .route("/v1/live", get(live_websocket));
    let router = if row_security {
        router
    } else {
        router.merge(instance::routes())
    };
    let state = ApiState {
        db: db.clone(),
        committed: committed.clone(),
        presence_changed,
        drafts_changed,
        presence_touches: Arc::new(std::sync::Mutex::new(HashMap::new())),
        storage: storage.clone(),
        vault_attachment_quota_bytes,
        upload_slots: Arc::new(tokio::sync::Semaphore::new(4)),
        options: Arc::new(options.clone()),
        instance_account_admissions: Arc::new(tokio::sync::Mutex::new(
            admission::LocalAdmissions::new(),
        )),
        mcp_oauth_admissions: Arc::new(tokio::sync::Mutex::new(admission::LocalAdmissions::new())),
        instance_password_slots: Arc::new(tokio::sync::Semaphore::new(2)),
        instance_events: tokio::sync::broadcast::channel(64).0,
        event_stream_slots: Arc::new(tokio::sync::Semaphore::new(32)),
        event_stream_sessions: Arc::new(std::sync::Mutex::new(HashMap::new())),
        policy,
        push_config: push_config_response,
        row_security,
        rls_enforced,
        instance_origin: config
            .as_ref()
            .and_then(|config| config.public_api_url.clone()),
        claim_policy,
        setup_window_started: std::time::Instant::now(),
        trusted_proxy_cidrs: Arc::new(trusted_proxy_cidrs.clone()),
        live_trusted_origin,
        live_hub,
        live_ticket_work: TicketDbWorkRegistry::new(),
        live_backend,
        #[cfg(test)]
        live_backend_override,
        #[cfg(test)]
        live_publish_timeout,
        #[cfg(test)]
        live_ticket_test_hooks,
        live_readiness,
        live_rate_limiter: Arc::new(std::sync::Mutex::new(
            crate::live::LiveRateLimiter::default(),
        )),
        live_upgrade_limiter: Arc::new(std::sync::Mutex::new(
            crate::live::LiveRateLimiter::with_limit(crate::live::LIVE_UPGRADE_RATE_LIMIT),
        )),
        live_ticket_http_limiter: Arc::new(std::sync::Mutex::new(
            crate::live::LiveRateLimiter::with_limit(30),
        )),
        mcp_worker: mcp_worker.clone(),
        enrollment_authorization: enrollment_authorization.clone(),
        mcp_consent_origin,
    };
    #[cfg(test)]
    if let Some(observer) = live_state_observer {
        *observer.lock().expect("live state observer lock poisoned") = Some(state.clone());
    }
    let router = router.with_state(state);
    let manual_state = invitation_origin
        .as_ref()
        .map(|origin| {
            crate::enrollment::manual::EnrollmentRouterState::new(
                db.clone(),
                enrollment_authorization.clone(),
                row_security,
                origin.clone(),
            )
            .map_err(|_| "PUBLIC_API_URL produced an invalid manual-enrollment origin")
            .map(|state| state.with_rls_enforced(rls_enforced))
            .map(|state| state.with_trusted_proxy_cidrs(trusted_proxy_cidrs.clone()))
        })
        .transpose()?;
    let router = match manual_state {
        Some(state) => router.merge(crate::enrollment::manual::router(state)),
        None => router,
    };
    let invitation_state = invitation_origin
        .map(|origin| {
            crate::enrollment::invitations::InvitationRouterState::new(
                db.clone(),
                enrollment_authorization.clone(),
                row_security,
                origin,
            )
            .map(|state| state.with_rls_enforced(rls_enforced))
            .map(|state| state.with_trusted_proxy_cidrs(trusted_proxy_cidrs.clone()))
            .map_err(|_| "PUBLIC_API_URL produced an invalid phone-enrollment origin")
        })
        .transpose()?;
    let router = match invitation_state {
        Some(state) => router.merge(crate::enrollment::invitations::router(state)),
        None => router,
    };
    let computer_enrollment_state = config
        .as_ref()
        .and_then(|config| config.public_api_url.as_ref())
        .map(|origin| origin.origin().ascii_serialization())
        .map(|origin| {
            crate::enrollment::computers::ComputerEnrollmentRouterState::new(
                db.clone(),
                enrollment_authorization.clone(),
                row_security,
                origin,
            )
            .map(|state| state.with_rls_enforced(rls_enforced))
            .map(|state| state.with_trusted_proxy_cidrs(trusted_proxy_cidrs.clone()))
            .map_err(|_| "PUBLIC_API_URL produced an invalid computer-enrollment origin")
        })
        .transpose()?;
    let router = match computer_enrollment_state {
        Some(state) => router.merge(crate::enrollment::computers::router(state)),
        None => router,
    };
    Ok((
        router,
        Maintenance {
            db,
            committed,
            storage,
            options,
            relay_url,
            mcp_worker: mcp_worker.clone(),
        },
    ))
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

/// Admission public key, credential digest and check header, in that order.
type OwnerMaterial = ([u8; 32], [u8; 32], Vec<u8>);

pub(super) fn validate_owner_material(
    material: &VaultMaterial,
    credential_digest: &str,
) -> Result<OwnerMaterial, ApiError> {
    validate_profile(&material.profile, &material.fingerprint, material.key_epoch)
        .map_err(|_| ApiError(StatusCode::UNPROCESSABLE_ENTITY, "invalid_vault_material"))?;
    let check_header = URL_SAFE_NO_PAD
        .decode(&material.check_header)
        .map_err(|_| ApiError(StatusCode::UNPROCESSABLE_ENTITY, "invalid_vault_material"))?;
    if check_header.is_empty() || check_header.len() > 1_048_576 {
        return Err(ApiError(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_vault_material",
        ));
    }
    Ok((
        decode_b64url(
            &material.admission_public_key,
            "invalid_admission_public_key",
        )?,
        decode_hex_digest(credential_digest)?,
        check_header,
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

const INSTANCE_SESSION_COOKIE: &str = "__Host-Peppy-Instance-Session";
const LEGACY_INSTANCE_SESSION_COOKIE: &str = "Peppy-Instance-Session";

fn cookie_token(headers: &HeaderMap) -> Option<&str> {
    let cookies = headers.get("cookie")?.to_str().ok()?;
    cookies
        .split(';')
        .find_map(|part| part.trim().strip_prefix("__Host-Peppy-Instance-Session="))
        .or_else(|| {
            cookies
                .split(';')
                .find_map(|part| part.trim().strip_prefix("Peppy-Instance-Session="))
        })
}

fn instance_session_cookie(token: &str, max_age: u32) -> Result<HeaderValue, ApiError> {
    HeaderValue::from_str(&format!(
        "{INSTANCE_SESSION_COOKIE}={token}; Path=/; Max-Age={max_age}; Secure; HttpOnly; SameSite=Strict"
    ))
    .map_err(|_| ApiError(StatusCode::SERVICE_UNAVAILABLE, "session_unavailable"))
}

fn clear_instance_session_cookie(name: &str) -> HeaderValue {
    HeaderValue::from_str(&format!(
        "{name}=; Path=/; Max-Age=0; Secure; HttpOnly; SameSite=Strict"
    ))
    .expect("fixed cookie name is valid")
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
    let cookie = instance_session_cookie(session_token, session.expires_in_seconds)?;
    session.session_token = None;
    let mut response = Json(session).into_response();
    response.headers_mut().insert("set-cookie", cookie);
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
    let cookie = instance_session_cookie(session_token, session.expires_in_seconds)?;
    session.session_token = None;
    let mut response = Json(ClaimInstanceResponse {
        vault_id,
        device_id,
        replayed,
        session,
    })
    .into_response();
    response.headers_mut().insert("set-cookie", cookie);
    Ok(response)
}

async fn replay_existing_instance_claim(
    state: &ApiState,
    transaction: sqlx::Transaction<'_, sqlx::Postgres>,
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
    // Do not retain the owner lock while durable admission obtains a pooled
    // connection. Under replay load that otherwise exhausts the pool.
    drop(transaction);
    instance_account_admission(
        state,
        peer,
        b"claim-replay",
        "instance_claim_replay_limited",
    )
    .await?;
    let mut transaction = state
        .db
        .begin()
        .await
        .map_err(|error| database_unavailable(&error, "instance_claim_replay_begin"))?;
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
    if !fresh_claim_is_authorized(
        state.claim_policy,
        state.setup_window_started,
        std::time::Instant::now(),
        &request.setup_token,
    ) {
        return Err(claim_policy_error(state.claim_policy, &request.setup_token));
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
    if !fresh_claim_is_authorized(
        state.claim_policy,
        state.setup_window_started,
        std::time::Instant::now(),
        &request.setup_token,
    ) {
        return Err(claim_policy_error(state.claim_policy, &request.setup_token));
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
        response.headers_mut().append(
            "set-cookie",
            clear_instance_session_cookie(INSTANCE_SESSION_COOKIE),
        );
        response.headers_mut().append(
            "set-cookie",
            clear_instance_session_cookie(LEGACY_INSTANCE_SESSION_COOKIE),
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
    let mut tx = scope::begin(db, vault_id)
        .await
        .map_err(|error| database_unavailable(&error, "instance_devices_begin"))?;
    let rows = sqlx::query("SELECT d.device_id, COALESCE(d.name, 'Unnamed device') name, COALESCE(d.kind, 'desktop') kind, d.role, d.revoked_at IS NOT NULL revoked, EXISTS(SELECT 1 FROM device_wake_routes routes WHERE routes.vault_id=d.vault_id AND routes.device_id=d.device_id AND routes.revoked_at IS NULL) gateway_online FROM devices d WHERE d.vault_id=$1 ORDER BY d.created_at")
        .bind(vault_id).fetch_all(&mut *tx).await
        .map_err(|error| database_unavailable(&error, "instance_devices"))?;
    tx.commit()
        .await
        .map_err(|error| database_unavailable(&error, "instance_devices_commit"))?;
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
    let mut tx = scope::begin(&state.db, vault_id)
        .await
        .map_err(|error| database_unavailable(&error, "instance_account_begin"))?;
    let row = sqlx::query("SELECT v.public_key_profile, v.encrypted_vault_check_header, v.profile_fingerprint, v.key_epoch, keys.admission_public_key FROM vaults v JOIN vault_admission_keys keys ON keys.vault_id=v.vault_id AND keys.key_epoch=v.key_epoch WHERE v.vault_id=$1")
        .bind(vault_id).fetch_one(&mut *tx).await
        .map_err(|error| database_unavailable(&error, "instance_account"))?;
    tx.commit()
        .await
        .map_err(|error| database_unavailable(&error, "instance_account_commit"))?;
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
    let mut tx = scope::begin(&state.db, vault_id)
        .await
        .map_err(|error| database_unavailable(&error, "instance_storage_begin"))?;
    let storage_bytes: i64 = sqlx::query_scalar(
        "SELECT COALESCE(sum(ciphertext_bytes), 0)::bigint FROM attachments WHERE vault_id=$1",
    )
    .bind(vault_id)
    .fetch_one(&mut *tx)
    .await
    .map_err(|error| database_unavailable(&error, "instance_storage"))?;
    tx.commit()
        .await
        .map_err(|error| database_unavailable(&error, "instance_storage_commit"))?;
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
    fence_revoked_device_work(&mut tx, vault_id, device_id)
        .await
        .map_err(|error| database_unavailable(&error, "instance_revoke_fence"))?;
    revoke_wake_effects(&mut tx, vault_id, device_id)
        .await
        .map_err(|error| database_unavailable(&error, "instance_revoke_wake_effects"))?;
    tx.commit()
        .await
        .map_err(|error| database_unavailable(&error, "instance_revoke_commit"))?;
    crate::presence::revoke_best_effort(&state.db, vault_id, device_id).await;
    let _ = state.presence_changed.send(vault_id);
    Ok(StatusCode::NO_CONTENT)
}

async fn revoke_wake_effects(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    vault_id: Uuid,
    device_id: Uuid,
) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE device_wake_routes SET revoked_at=now() WHERE vault_id=$1 AND device_id=$2 AND revoked_at IS NULL")
        .bind(vault_id)
        .bind(device_id)
        .execute(&mut **tx)
        .await?;
    sqlx::query("DELETE FROM device_wake_jobs WHERE vault_id=$1 AND device_id=$2")
        .bind(vault_id)
        .bind(device_id)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

async fn fence_revoked_device_work(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    vault_id: Uuid,
    device_id: Uuid,
) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE phone_invitations SET state='denied' WHERE vault_id=$1 AND owner_device_id=$2 AND state IN ('waiting','claimed','approved','confirmed')")
        .bind(vault_id).bind(device_id).execute(&mut **tx).await?;
    sqlx::query("UPDATE computer_enrollments SET state='cancelled' WHERE vault_id=$1 AND owner_device_id=$2 AND state IN ('waiting','approved')")
        .bind(vault_id).bind(device_id).execute(&mut **tx).await?;
    sqlx::query("WITH fenced AS (DELETE FROM upload_reservations WHERE vault_id=$1 AND device_id=$2 AND finalized_at IS NULL RETURNING object_key,uploaded_at) INSERT INTO storage_deletions(object_key,vault_id,reason) SELECT object_key,$1,'upload_attempt' FROM fenced WHERE uploaded_at IS NOT NULL ON CONFLICT(object_key) DO UPDATE SET not_before=now()")
        .bind(vault_id).bind(device_id).execute(&mut **tx).await?;
    Ok(())
}

pub(crate) fn database_error_kind(error: &sqlx::Error) -> &'static str {
    match error {
        sqlx::Error::PoolTimedOut => "pool_timed_out",
        sqlx::Error::PoolClosed => "pool_closed",
        sqlx::Error::Io(_) => "io",
        sqlx::Error::WorkerCrashed => "worker_crashed",
        sqlx::Error::Database(_) => "database",
        _ => "other",
    }
}

pub(super) fn database_unavailable(error: &sqlx::Error, operation: &'static str) -> ApiError {
    tracing::warn!(
        operation,
        kind = database_error_kind(error),
        "database operation failed"
    );
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

pub(super) fn access_principal(principal: &Principal) -> AccessPrincipal {
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
    let principal = authenticated_without_presence(s, headers, operation).await?;
    touch_authenticated_presence(s, &principal).await;
    Ok(principal)
}

async fn authenticated_without_presence(
    s: &ApiState,
    headers: &HeaderMap,
    operation: Operation,
) -> ApiResult<Principal> {
    let principal = auth_with_mode(s, headers).await?;
    authorize(s, &principal, operation).await?;
    Ok(principal)
}

async fn touch_authenticated_presence(s: &ApiState, principal: &Principal) {
    if reserve_presence_touch(&s.presence_touches, principal.vault, principal.device) {
        crate::presence::touch_best_effort(&s.db, principal.vault, principal.device).await;
    }
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
    if !s.rls_enforced {
        return auth_direct(&s.db, headers).await;
    }
    if bearer.len() != TOKEN_BYTES_HEX || !bearer.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(ApiError(StatusCode::UNAUTHORIZED, "invalid_bearer"));
    }
    let digest = Sha256::digest(bearer.as_bytes());
    let resolver = if s.row_security {
        "peppy.resolve_device_credential"
    } else {
        "community_resolve_device_credential"
    };
    let row = sqlx::query(&format!(
        "SELECT vault_id,device_id,role,token_digest FROM {resolver}($1)"
    ))
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
pub(super) async fn revalidate_principal(
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
pub(super) fn owner(p: &Principal) -> ApiResult<()> {
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

const SETUP_WINDOW: Duration = Duration::from_secs(60 * 60);

fn fresh_claim_is_authorized(
    policy: crate::instance::ClaimPolicy,
    started: std::time::Instant,
    now: std::time::Instant,
    token: &str,
) -> bool {
    match policy {
        crate::instance::ClaimPolicy::Open => now.duration_since(started) < SETUP_WINDOW,
        _ => policy.authorizes(token),
    }
}

fn claim_policy_error(policy: crate::instance::ClaimPolicy, token: &str) -> ApiError {
    match policy {
        crate::instance::ClaimPolicy::Open => {
            ApiError(StatusCode::FORBIDDEN, "setup_window_closed")
        }
        crate::instance::ClaimPolicy::Disabled => {
            ApiError(StatusCode::CONFLICT, "instance_already_claimed")
        }
        _ if !policy.authorizes(token) => ApiError(StatusCode::UNAUTHORIZED, "invalid_setup_token"),
        _ => ApiError(StatusCode::FORBIDDEN, "setup_window_closed"),
    }
}

pub(super) async fn instance_account_admission(
    state: &ApiState,
    peer: Option<IpAddr>,
    operation: &[u8],
    code: &'static str,
) -> ApiResult<()> {
    let mut admissions = state.instance_account_admissions.lock().await;
    let source = admission::source_digest(operation, peer);
    let operation = match operation {
        b"claim" => admission::AdmissionOperation::Claim,
        b"claim-replay" => admission::AdmissionOperation::ClaimReplay,
        b"login" => admission::AdmissionOperation::Login,
        b"create-vault" => admission::AdmissionOperation::CreateVault,
        b"vault-request" => admission::AdmissionOperation::VaultRequest,
        _ => {
            return Err(ApiError(
                StatusCode::SERVICE_UNAVAILABLE,
                "admission_unavailable",
            ));
        }
    };
    if !admissions.admit(source) {
        return Err(ApiError(StatusCode::TOO_MANY_REQUESTS, code));
    }
    drop(admissions);
    admission::admit_durably(&state.db, operation, source)
        .await
        .map_err(|error| match error {
            admission::AdmissionError::Limited => ApiError(StatusCode::TOO_MANY_REQUESTS, code),
            admission::AdmissionError::Database(error) => {
                database_unavailable(&error, "instance_admission")
            }
        })
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
    withdraw_legacy_retention_mutation(&mut tx, &h, p.vault, p.device).await?;
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
    // MCP grants are fenced at the same roster-revocation linearization point.
    // The worker cleanup is intentionally durable and retried out-of-process.
    sqlx::query("UPDATE mcp_devices SET cleanup_pending=TRUE WHERE vault_id=$1 AND device_id=$2")
        .bind(p.vault)
        .bind(device_id)
        .execute(&mut *tx)
        .await
        .map_err(|error| database_unavailable(&error, "revoke_mcp_device"))?;
    sqlx::query(
        "UPDATE mcp_grants SET revoked_at=now() WHERE vault_id=$1 AND device_id=$2 AND revoked_at IS NULL",
    )
    .bind(p.vault)
    .bind(device_id)
    .execute(&mut *tx)
    .await
    .map_err(|error| database_unavailable(&error, "revoke_mcp_grants"))?;
    sqlx::query(
        "UPDATE device_credentials SET revoked_at=now() WHERE vault_id=$1 AND device_id=$2",
    )
    .bind(p.vault)
    .bind(device_id)
    .execute(&mut *tx)
    .await
    .map_err(|error| database_unavailable(&error, "revoke_credential"))?;
    fence_revoked_device_work(&mut tx, p.vault, device_id)
        .await
        .map_err(|error| database_unavailable(&error, "revoke_fence"))?;
    revoke_wake_effects(&mut tx, p.vault, device_id)
        .await
        .map_err(|error| database_unavailable(&error, "revoke_wake_effects"))?;
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
    sqlx::query("INSERT INTO storage_deletions(object_key,vault_id,reason) SELECT object_key,$1,'vault_deleted' FROM (SELECT object_key FROM upload_reservations WHERE vault_id=$1 UNION SELECT object_key FROM attachments WHERE vault_id=$1 UNION SELECT object_key FROM attachment_removals WHERE vault_id=$1 UNION SELECT object_key FROM public_attachment_copies WHERE vault_id=$1) keys ON CONFLICT(object_key) DO UPDATE SET reason='vault_deleted',not_before=GREATEST(storage_deletions.not_before,now())")
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
        "attachment_retention_uses",
        "attachment_retention_devices",
        "attachment_removals",
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
        "phone_invitations",
        "computer_enrollments",
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
    #[serde(skip_serializing_if = "is_false")]
    origin_duplicate: bool,
}

fn is_false(value: &bool) -> bool {
    !value
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
pub(crate) async fn withdraw_legacy_retention_mutation(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    headers: &HeaderMap,
    vault_id: Uuid,
    device_id: Uuid,
) -> ApiResult<()> {
    crate::attachment_retention::withdraw_legacy_retention_readiness_on_connection(
        tx, headers, vault_id, device_id,
    )
    .await
    .map_err(|error| database_unavailable(&error, "retention_readiness_withdraw"))
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
    if e.producer_sequence.0 > i64::MAX as u64
        || e.compaction.as_ref().is_some_and(|compaction| {
            compaction
                .supersedes
                .iter()
                .any(|reference| reference.producer_sequence.0 > i64::MAX as u64)
        })
    {
        return Err(ApiError(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_envelope",
        ));
    }
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
    revalidate_principal(&mut tx, &p).await?;
    withdraw_legacy_retention_mutation(&mut tx, &h, p.vault, p.device).await?;
    // Identity lives in the immutable records, which outlive transport-log
    // pruning. A retry stays observable after a key epoch or route changes.
    let compacted=sqlx::query("SELECT original_cursor,cipher_digest FROM compacted_records WHERE vault_id=$1 AND producer_device_id=$2 AND (producer_sequence=$3 OR envelope_id=$4 OR (command_id IS NOT NULL AND command_id=$5)) LIMIT 1").bind(p.vault).bind(p.device).bind(e.producer_sequence.0 as i64).bind(e.envelope_id.0).bind(e.command_id.map(|v|v.0)).fetch_optional(&mut *tx).await.map_err(|error| database_unavailable(&error, "ingest_compacted_duplicate_lookup"))?;
    if let Some(row) = compacted {
        if row.get::<Vec<u8>, _>("cipher_digest") == digest {
            return Ok(Json(Accepted {
                cursor: row.get::<i64, _>("original_cursor").to_string(),
                duplicate: true,
                origin_duplicate: false,
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
                origin_duplicate: false,
            }));
        }
        return Err(ApiError(StatusCode::CONFLICT, "idempotency_conflict"));
    }
    let indexed_origin_tag = if let Some(origin_tag) = e.origin_tag {
        let existing = sqlx::query("SELECT cursor,producer_device_id FROM encrypted_records WHERE vault_id=$1 AND origin_tag=$2 UNION ALL SELECT original_cursor AS cursor,producer_device_id FROM compacted_records WHERE vault_id=$1 AND origin_tag=$2 LIMIT 1")
            .bind(p.vault)
            .bind(origin_tag.to_vec())
            .fetch_optional(&mut *tx)
            .await
            .map_err(|error| database_unavailable(&error, "ingest_origin_duplicate_lookup"))?;
        match existing {
            Some(row) if row.get::<Uuid, _>("producer_device_id") != p.device => {
                tracing::info!(vault_id = %p.vault, "origin tag duplicate");
                return Ok(Json(Accepted {
                    cursor: row.get::<i64, _>("cursor").to_string(),
                    duplicate: true,
                    origin_duplicate: true,
                }));
            }
            Some(_) => None,
            None => Some(origin_tag),
        }
    } else {
        None
    };
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
        let gateway_device_id = match route {
            peppy_protocol::GatewayRoute::Carrier {
                gateway_device_id, ..
            }
            | peppy_protocol::GatewayRoute::RcsLink { gateway_device_id } => gateway_device_id.0,
        };
        let gateway:Option<i32>=sqlx::query_scalar("SELECT 1 FROM devices d WHERE d.vault_id=$1 AND d.device_id=$2 AND d.revoked_at IS NULL AND (d.role='gateway' OR (d.role='owner' AND EXISTS (SELECT 1 FROM device_capabilities c WHERE c.vault_id=d.vault_id AND c.device_id=d.device_id)))").bind(p.vault).bind(gateway_device_id).fetch_optional(&mut *tx).await.map_err(|error| database_unavailable(&error, "ingest_route_lookup"))?;
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
    sqlx::query("INSERT INTO encrypted_records(vault_id,producer_device_id,envelope_id,cursor,producer_sequence,purpose,command_id,cipher_digest,envelope,origin_tag) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)")
        .bind(p.vault)
        .bind(p.device)
        .bind(e.envelope_id.0)
        .bind(c)
        .bind(e.producer_sequence.0 as i64)
        .bind(purpose)
        .bind(e.command_id.map(|v| v.0))
        .bind(digest.as_slice())
        .bind(&payload)
        .bind(indexed_origin_tag.map(|tag| tag.to_vec()))
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
        let gateway = match e.route.as_ref().expect("validated route") {
            peppy_protocol::GatewayRoute::Carrier {
                gateway_device_id, ..
            }
            | peppy_protocol::GatewayRoute::RcsLink { gateway_device_id } => gateway_device_id.0,
        };
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
        origin_duplicate: false,
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
                let row = match row {
                    Ok(Some(row)) => row,
                    Ok(None) => break,
                    Err(error) => {
                        tracing::warn!(
                            operation = "wake_lease",
                            kind = database_error_kind(&error),
                            "database operation failed"
                        );
                        break;
                    }
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

const LIVE_TICKET_SESSION_LIMIT: i64 = 6;
const LIVE_TICKET_LIFETIME_REFUSED: &str = "live_ticket_lifetime_refused";

#[derive(Deserialize)]
struct LiveTicketRequest {
    session_id: String,
    #[serde(default)]
    lifetime_handle: LifetimeHandleInput,
}

#[derive(Default)]
enum LifetimeHandleInput {
    #[default]
    Missing,
    Value(Value),
}

impl<'de> Deserialize<'de> for LifetimeHandleInput {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        Value::deserialize(deserializer).map(Self::Value)
    }
}
#[derive(Serialize)]
struct LiveTicketResponse {
    ticket: String,
    expires_at_ms: i64,
    lifetime_handle: String,
}

const DRAFT_HINT_COOLDOWN: Duration = Duration::from_millis(500);

#[derive(Default)]
struct DraftHintCoalescer {
    cooldown_until: Option<tokio::time::Instant>,
    pending: bool,
}

impl DraftHintCoalescer {
    fn notice(&mut self, now: tokio::time::Instant) -> bool {
        if self
            .cooldown_until
            .is_none_or(|cooldown_until| now >= cooldown_until)
        {
            self.cooldown_until = None;
            self.pending = false;
            true
        } else {
            self.pending = true;
            false
        }
    }

    fn sent(&mut self, completed_at: tokio::time::Instant) -> tokio::time::Instant {
        let cooldown_until = completed_at + DRAFT_HINT_COOLDOWN;
        self.cooldown_until = Some(cooldown_until);
        cooldown_until
    }

    fn timeout_expired(&mut self, now: tokio::time::Instant) -> bool {
        let Some(cooldown_until) = self.cooldown_until else {
            return false;
        };
        if now < cooldown_until {
            return false;
        }
        self.cooldown_until = None;
        std::mem::take(&mut self.pending)
    }

    fn cooling_down(&self) -> bool {
        self.cooldown_until.is_some()
    }
}

async fn websocket(
    State(s): State<ApiState>,
    headers: HeaderMap,
    lease: Option<Extension<crate::serving::ConnectionLease>>,
    shutdown: Option<Extension<tokio::sync::watch::Receiver<bool>>>,
    upgrade: WebSocketUpgrade,
) -> ApiResult<axum::response::Response> {
    let principal = authenticated(&s, &headers, Operation::Sync).await?;
    // Subscribe before replay so a commit between replay and live is never missed.
    let receiver = s.committed.subscribe();
    let presence_receiver = s.presence_changed.subscribe();
    let drafts_receiver = s.drafts_changed.subscribe();
    let lease = lease.map(|Extension(lease)| lease);
    let shutdown = shutdown.map(|Extension(shutdown)| shutdown);
    Ok(upgrade
        .max_message_size(1_100_000)
        .max_frame_size(1_100_000)
        .on_upgrade(move |socket| {
            ws_session(
                socket,
                s,
                principal,
                receiver,
                presence_receiver,
                drafts_receiver,
                lease,
                shutdown,
            )
        })
        .into_response())
}

fn lifetime_refused() -> ApiError {
    ApiError(StatusCode::CONFLICT, LIVE_TICKET_LIFETIME_REFUSED)
}

fn live_ticket_lifetime_handle(value: &str) -> ApiResult<i64> {
    let bytes: [u8; 8] = URL_SAFE_NO_PAD
        .decode(value)
        .ok()
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or_else(lifetime_refused)?;
    let incarnation = i64::from_be_bytes(bytes);
    (incarnation > 0 && URL_SAFE_NO_PAD.encode(bytes) == value)
        .then_some(incarnation)
        .ok_or_else(lifetime_refused)
}

fn requested_lifetime_handle(input: &LifetimeHandleInput) -> ApiResult<Option<i64>> {
    match input {
        LifetimeHandleInput::Missing => Ok(None),
        LifetimeHandleInput::Value(Value::String(value)) => {
            live_ticket_lifetime_handle(value).map(Some)
        }
        LifetimeHandleInput::Value(_) => Err(lifetime_refused()),
    }
}

fn required_lifetime_handle(input: &LifetimeHandleInput) -> ApiResult<i64> {
    requested_lifetime_handle(input)?.ok_or_else(lifetime_refused)
}

fn encode_live_ticket_lifetime_handle(incarnation: i64) -> String {
    URL_SAFE_NO_PAD.encode(incarnation.to_be_bytes())
}

fn live_ticket_session_hash(value: &str) -> ApiResult<Vec<u8>> {
    let bytes = URL_SAFE_NO_PAD
        .decode(value)
        .ok()
        .filter(|bytes| bytes.len() == 16)
        .ok_or(ApiError(
            StatusCode::BAD_REQUEST,
            "live_ticket_session_invalid",
        ))?;
    (URL_SAFE_NO_PAD.encode(&bytes) == value)
        .then(|| Sha256::digest(bytes).to_vec())
        .ok_or(ApiError(
            StatusCode::BAD_REQUEST,
            "live_ticket_session_invalid",
        ))
}

/// Resolves a ticket's tenant through the narrow SECURITY DEFINER function, then
/// consumes it under that tenant's RLS scope. PostgreSQL supplies acceptance time
/// after all row locks have been acquired.
struct ConsumedLiveTicket {
    principal: Principal,
    session_id: [u8; 16],
    session_incarnation: i64,
    connection_id: i64,
    replaced: Option<crate::live::LiveTicketIdentity>,
}

const LIVE_TICKET_HTTP_OPERATION_BUDGET: Duration = Duration::from_secs(60);

fn ticket_http_deadline(state: &ApiState) -> OperationDeadline {
    #[cfg(test)]
    if let Some(budget) = state.live_ticket_test_hooks.http_operation_budget {
        return OperationDeadline::after(budget);
    }
    #[cfg(not(test))]
    let _ = state;
    OperationDeadline::after(LIVE_TICKET_HTTP_OPERATION_BUDGET)
}

fn ticket_http_unavailable() -> ApiError {
    ApiError(StatusCode::SERVICE_UNAVAILABLE, "live_unavailable")
}

fn ticket_work_unavailable(error: &TicketDbWorkError, operation: &'static str) -> ApiError {
    let kind = match error {
        TicketDbWorkError::Database(error) => database_error_kind(error),
        TicketDbWorkError::Deadline => "deadline",
        TicketDbWorkError::Cancelled => "cancelled",
        TicketDbWorkError::Capacity => "capacity",
        TicketDbWorkError::Unsettled => "unsettled",
        TicketDbWorkError::ChildStopped => "child_stopped",
        TicketDbWorkError::Panicked => "panicked",
    };
    tracing::warn!(operation, kind, "live ticket database work failed");
    ticket_http_unavailable()
}

async fn await_ticket_http<T>(
    deadline: OperationDeadline,
    operation: impl std::future::Future<Output = ApiResult<T>>,
) -> ApiResult<T> {
    let remaining = deadline.remaining().ok_or_else(ticket_http_unavailable)?;
    let result = tokio::time::timeout(remaining, operation)
        .await
        .map_err(|_| ticket_http_unavailable())?;
    deadline.remaining().ok_or_else(ticket_http_unavailable)?;
    result
}

async fn run_ticket_http_work<T: Send + 'static>(
    registry: &TicketDbWorkRegistry,
    pool: &PgPool,
    deadline: OperationDeadline,
    workflow: TicketWorkflow<ApiResult<T>>,
) -> ApiResult<T> {
    let (cancel, cancellation) = tokio::sync::watch::channel(false);
    let mut guard = TicketWorkCancellationGuard(Some(cancel));
    let work = registry
        .spawn(pool.clone(), deadline, cancellation, workflow)
        .map_err(|error| ticket_work_unavailable(&error, "live_ticket_http"))?;
    let remaining = deadline.remaining().ok_or_else(ticket_http_unavailable)?;
    let result = tokio::time::timeout(remaining, work.outcome())
        .await
        .map_err(|_| ticket_http_unavailable())?;
    guard.disarm();
    deadline.remaining().ok_or_else(ticket_http_unavailable)?;
    result.map_err(|error| ticket_work_unavailable(&error, "live_ticket_http"))?
}

async fn refuse_ticket_http<T>(
    scope: &mut ExecutionScope,
    error: ApiError,
) -> Result<ApiResult<T>, TicketDbWorkError> {
    scope.stage().await?.execute("ROLLBACK").await?;
    scope.settled();
    Ok(Err(error))
}

async fn consume_live_ticket(
    pool: &PgPool,
    session_id: [u8; 16],
    secret: [u8; 32],
) -> ApiResult<ConsumedLiveTicket> {
    consume_live_ticket_owned(
        &TicketDbWorkRegistry::new(),
        pool,
        OperationDeadline::after(LIVE_TICKET_HTTP_OPERATION_BUDGET),
        session_id,
        secret,
    )
    .await
}

async fn consume_live_ticket_owned(
    registry: &TicketDbWorkRegistry,
    pool: &PgPool,
    deadline: OperationDeadline,
    session_id: [u8; 16],
    secret: [u8; 32],
) -> ApiResult<ConsumedLiveTicket> {
    let digest = Sha256::digest(secret).to_vec();
    let session_hash = Sha256::digest(session_id).to_vec();
    let workflow: TicketWorkflow<ApiResult<ConsumedLiveTicket>> = Box::new(move |scope| {
        Box::pin(async move {
            let invalid = || ApiError(StatusCode::UNAUTHORIZED, "live_ticket_invalid");
            scope.stage().await?.execute("BEGIN").await?;
            scope
                .stage()
                .await?
                .execute("SET LOCAL lock_timeout='60s'")
                .await?;
            scope
                .stage()
                .await?
                .execute("SET LOCAL statement_timeout='60s'")
                .await?;
            let vault: Option<Uuid> =
                sqlx::query_scalar("SELECT vault_id FROM community_resolve_live_ticket_vault($1)")
                    .bind(&digest)
                    .fetch_optional(scope.stage().await?)
                    .await?;
            let Some(vault) = vault else {
                return refuse_ticket_http(scope, invalid()).await;
            };
            sqlx::query("SELECT set_config('peppy.vault_id',$1,true)")
                .bind(vault.to_string())
                .execute(scope.stage().await?)
                .await?;
            let device: Option<Uuid> = sqlx::query_scalar(
                "SELECT device_id FROM live_tickets WHERE ticket_digest=$1 AND vault_id=$2",
            )
            .bind(&digest)
            .bind(vault)
            .fetch_optional(scope.stage().await?)
            .await?;
            let Some(device) = device else {
                return refuse_ticket_http(scope, invalid()).await;
            };
            // Reject immutable/committed invalidity before capacity can mask it. The
            // locked checks below remain authoritative after the device permit is held.
            let valid_before_capacity: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM devices d JOIN device_credentials c ON c.vault_id=d.vault_id AND c.device_id=d.device_id JOIN live_ticket_sessions s ON s.vault_id=d.vault_id AND s.device_id=d.device_id JOIN live_tickets t ON t.vault_id=s.vault_id AND t.device_id=s.device_id AND t.session_id_hash=s.session_id_hash AND t.session_incarnation=s.session_incarnation WHERE d.vault_id=$1 AND d.device_id=$2 AND d.revoked_at IS NULL AND c.revoked_at IS NULL AND s.session_id_hash=$3 AND s.state='open' AND s.lease_until>clock_timestamp() AND t.ticket_digest=$4 AND t.consumed_at IS NULL AND t.expires_at>clock_timestamp())")
                .bind(vault).bind(device).bind(&session_hash).bind(&digest)
                .fetch_one(scope.stage().await?).await?;
            if !valid_before_capacity {
                return refuse_ticket_http(scope, invalid()).await;
            }
            scope
                .claim_device(TenantDevice {
                    vault_id: vault,
                    device_id: device,
                })
                .await?;
            let principal = sqlx::query("SELECT d.role,c.token_digest FROM devices d JOIN device_credentials c ON c.vault_id=d.vault_id AND c.device_id=d.device_id WHERE d.vault_id=$1 AND d.device_id=$2 AND d.revoked_at IS NULL AND c.revoked_at IS NULL FOR UPDATE OF d")
                .bind(vault).bind(device).fetch_optional(scope.stage().await?).await?;
            let Some(principal) = principal else {
                return refuse_ticket_http(scope, invalid()).await;
            };
            let session: Option<(String, i64, Option<i64>)> = sqlx::query_as("SELECT state,session_incarnation,active_connection_id FROM live_ticket_sessions WHERE vault_id=$1 AND device_id=$2 AND session_id_hash=$3 FOR UPDATE")
                .bind(vault).bind(device).bind(&session_hash)
                .fetch_optional(scope.stage().await?).await?;
            let Some((state, session_incarnation, previous_connection)) = session else {
                return refuse_ticket_http(scope, invalid()).await;
            };
            let ticket: Option<(i64, bool)> = sqlx::query_as("SELECT session_incarnation,consumed_at IS NULL FROM live_tickets WHERE ticket_digest=$1 AND vault_id=$2 AND device_id=$3 AND session_id_hash=$4 FOR UPDATE")
                .bind(&digest).bind(vault).bind(device).bind(&session_hash)
                .fetch_optional(scope.stage().await?).await?;
            let Some((ticket_incarnation, unconsumed)) = ticket else {
                return refuse_ticket_http(scope, invalid()).await;
            };
            let sample_started = std::time::Instant::now();
            let accepted_at: String = sqlx::query_scalar("SELECT clock_timestamp()::text")
                .fetch_one(scope.stage().await?)
                .await?;
            let sample_finished = std::time::Instant::now();
            debug_assert!(sample_finished >= sample_started);
            if state != "open" || ticket_incarnation != session_incarnation || !unconsumed {
                return refuse_ticket_http(scope, invalid()).await;
            }
            let live_at_acceptance: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM live_ticket_sessions s JOIN live_tickets t ON t.vault_id=s.vault_id AND t.device_id=s.device_id AND t.session_id_hash=s.session_id_hash AND t.session_incarnation=s.session_incarnation WHERE s.vault_id=$1 AND s.device_id=$2 AND s.session_id_hash=$3 AND t.ticket_digest=$4 AND s.session_incarnation=$5 AND s.state='open' AND s.lease_until>$6::timestamptz AND t.consumed_at IS NULL AND t.expires_at>$6::timestamptz)")
                .bind(vault).bind(device).bind(&session_hash).bind(&digest)
                .bind(session_incarnation).bind(&accepted_at)
                .fetch_one(scope.stage().await?).await?;
            if !live_at_acceptance {
                return refuse_ticket_http(scope, invalid()).await;
            }
            let connection_id = match crate::live_ticket_authority::allocate_connection_id(
                scope.stage().await?,
                vault,
                device,
            )
            .await
            {
                Ok(connection_id) => connection_id,
                Err(crate::live_ticket_authority::AllocationError::Exhausted) => {
                    return refuse_ticket_http(scope, invalid()).await;
                }
                Err(crate::live_ticket_authority::AllocationError::Database(error)) => {
                    return Err(error.into());
                }
            };
            let consumed: Option<i32> = sqlx::query_scalar("UPDATE live_tickets SET consumed_at=$5::timestamptz WHERE ticket_digest=$1 AND vault_id=$2 AND device_id=$3 AND session_id_hash=$4 AND session_incarnation=$6 AND consumed_at IS NULL AND expires_at>$5::timestamptz RETURNING 1")
                .bind(&digest).bind(vault).bind(device).bind(&session_hash)
                .bind(&accepted_at).bind(session_incarnation)
                .fetch_optional(scope.stage().await?).await?;
            if consumed.is_none() {
                return refuse_ticket_http(scope, invalid()).await;
            }
            let updated = sqlx::query("UPDATE live_ticket_sessions SET active_connection_id=$5,lease_until=$6::timestamptz+interval '90 seconds' WHERE vault_id=$1 AND device_id=$2 AND session_id_hash=$3 AND session_incarnation=$4 AND state='open' AND lease_until>$6::timestamptz")
                .bind(vault).bind(device).bind(&session_hash).bind(session_incarnation)
                .bind(connection_id).bind(&accepted_at)
                .execute(scope.stage().await?).await?;
            if updated.rows_affected() != 1 {
                return refuse_ticket_http(scope, invalid()).await;
            }
            scope.stage().await?.execute("COMMIT").await?;
            scope.settled();
            Ok(Ok(ConsumedLiveTicket {
                principal: Principal {
                    vault,
                    device,
                    role: principal.get("role"),
                    token_digest: principal.get("token_digest"),
                },
                session_id,
                session_incarnation,
                connection_id,
                replaced: previous_connection.map(|connection| {
                    crate::live::LiveTicketIdentity::new(
                        session_id,
                        session_incarnation,
                        connection,
                    )
                }),
            }))
        })
    });
    run_ticket_http_work(registry, pool, deadline, workflow).await
}

#[doc(hidden)]
pub async fn __test_consume_live_ticket(
    pool: &PgPool,
    session_id: [u8; 16],
    secret: [u8; 32],
) -> Result<crate::ConsumedTicketIds, &'static str> {
    let consumed = consume_live_ticket(pool, session_id, secret)
        .await
        .map_err(|_| "live_ticket_invalid")?;
    Ok(crate::ConsumedTicketIds {
        vault_id: consumed.principal.vault,
        device_id: consumed.principal.device,
        session_id,
    })
}

async fn begin_ticket_http_device(
    scope: &mut ExecutionScope,
    p: &Principal,
) -> Result<bool, TicketDbWorkError> {
    scope
        .claim_device(TenantDevice {
            vault_id: p.vault,
            device_id: p.device,
        })
        .await?;
    scope.stage().await?.execute("BEGIN").await?;
    sqlx::query("SELECT set_config('peppy.vault_id',$1,true)")
        .bind(p.vault.to_string())
        .execute(scope.stage().await?)
        .await?;
    scope
        .stage()
        .await?
        .execute("SET LOCAL lock_timeout='60s'")
        .await?;
    scope
        .stage()
        .await?
        .execute("SET LOCAL statement_timeout='60s'")
        .await?;
    let locked: Option<i32> =
        sqlx::query_scalar("SELECT 1 FROM devices WHERE vault_id=$1 AND device_id=$2 FOR UPDATE")
            .bind(p.vault)
            .bind(p.device)
            .fetch_optional(scope.stage().await?)
            .await?;
    if locked.is_none() {
        return Ok(false);
    }
    let role: Option<String> = sqlx::query_scalar("SELECT d.role FROM device_credentials c JOIN devices d ON d.vault_id=c.vault_id AND d.device_id=c.device_id WHERE c.token_digest=$1 AND c.vault_id=$2 AND c.device_id=$3 AND c.revoked_at IS NULL AND d.revoked_at IS NULL")
        .bind(&p.token_digest).bind(p.vault).bind(p.device)
        .fetch_optional(scope.stage().await?).await?;
    Ok(role.as_deref() == Some(p.role.as_str()))
}

async fn commit_ticket_http(scope: &mut ExecutionScope) -> Result<(), TicketDbWorkError> {
    scope.stage().await?.execute("COMMIT").await?;
    scope.settled();
    Ok(())
}

async fn terminalize_ticket_http(
    scope: &mut ExecutionScope,
    p: &Principal,
    session: &[u8],
    incarnation: i64,
    accepted_at: &str,
    state: &str,
) -> Result<(), TicketDbWorkError> {
    sqlx::query("UPDATE live_ticket_sessions SET state=$6,closed_at=$5::timestamptz,lease_until=NULL WHERE vault_id=$1 AND device_id=$2 AND session_id_hash=$3 AND session_incarnation=$4 AND state='open'")
        .bind(p.vault).bind(p.device).bind(session).bind(incarnation).bind(accepted_at).bind(state)
        .execute(scope.stage().await?).await?;
    sqlx::query("DELETE FROM live_tickets WHERE vault_id=$1 AND device_id=$2 AND session_id_hash=$3 AND session_incarnation=$4 AND consumed_at IS NULL")
        .bind(p.vault).bind(p.device).bind(session).bind(incarnation)
        .execute(scope.stage().await?).await?;
    Ok(())
}

async fn issue_live_ticket(
    State(state): State<ApiState>,
    PeerAddress(peer): PeerAddress,
    headers: HeaderMap,
    Json(request): Json<LiveTicketRequest>,
) -> ApiResult<(StatusCode, Json<LiveTicketResponse>)> {
    let deadline = ticket_http_deadline(&state);
    if state.live_trusted_origin.is_none() {
        return Err(ApiError(
            StatusCode::SERVICE_UNAVAILABLE,
            "live_ticket_unsupported",
        ));
    }
    let ready = if cfg!(test) {
        *state.live_readiness.borrow()
    } else {
        state.live_backend.is_ready()
    };
    if !ready {
        return Err(ApiError(
            StatusCode::SERVICE_UNAVAILABLE,
            "live_unavailable",
        ));
    }
    let session = live_ticket_session_hash(&request.session_id)?;
    let lifetime = requested_lifetime_handle(&request.lifetime_handle)?;
    let p = await_ticket_http(
        deadline,
        authenticated_without_presence(&state, &headers, Operation::Sync),
    )
    .await?;
    admit_ticket_http_attempt(&state, &p, peer)?;
    libsodium_rs::ensure_init().map_err(|_| ticket_http_unavailable())?;
    let mut ticket = [0_u8; 32];
    libsodium_rs::random::fill_bytes(&mut ticket);
    let principal = p.clone();
    let workflow: TicketWorkflow<ApiResult<LiveTicketResponse>> = Box::new(move |scope| {
        Box::pin(async move {
            issue_ticket_http_owned(scope, &principal, &session, lifetime, ticket).await
        })
    });
    let response =
        run_ticket_http_work(&state.live_ticket_work, &state.db, deadline, workflow).await?;
    await_ticket_http(deadline, async {
        touch_authenticated_presence(&state, &p).await;
        Ok(())
    })
    .await?;
    Ok((StatusCode::CREATED, Json(response)))
}

async fn issue_ticket_http_owned(
    scope: &mut ExecutionScope,
    p: &Principal,
    session: &[u8],
    lifetime: Option<i64>,
    ticket: [u8; 32],
) -> Result<ApiResult<LiveTicketResponse>, TicketDbWorkError> {
    if !begin_ticket_http_device(scope, p).await? {
        return refuse_ticket_http(scope, ApiError(StatusCode::UNAUTHORIZED, "invalid_bearer"))
            .await;
    }
    let current: Option<(String, i64)> = sqlx::query_as("SELECT state,session_incarnation FROM live_ticket_sessions WHERE vault_id=$1 AND device_id=$2 AND session_id_hash=$3 FOR UPDATE")
        .bind(p.vault).bind(p.device).bind(session).fetch_optional(scope.stage().await?).await?;
    let existing = match (lifetime, current) {
        (Some(handle), Some((state, incarnation))) if state == "open" && handle == incarnation => {
            Some(incarnation)
        }
        (None, None) => None,
        _ => return refuse_ticket_http(scope, lifetime_refused()).await,
    };
    let replaceable: Option<(Vec<u8>, i64)> = if existing.is_none() {
        let open: Option<(Vec<u8>, i64, Option<i64>)> = sqlx::query_as("SELECT session_id_hash,session_incarnation,active_connection_id FROM live_ticket_sessions WHERE vault_id=$1 AND device_id=$2 AND state='open' FOR UPDATE")
            .bind(p.vault).bind(p.device).fetch_optional(scope.stage().await?).await?;
        match open {
            Some((session, incarnation, None)) => Some((session, incarnation)),
            Some(_) => return refuse_ticket_http(scope, ticket_rate_limited()).await,
            None => None,
        }
    } else {
        None
    };
    let accepted_at: String = sqlx::query_scalar("SELECT clock_timestamp()::text")
        .fetch_one(scope.stage().await?)
        .await?;
    if let Some(incarnation) = existing {
        let active: bool = sqlx::query_scalar("SELECT lease_until>$5::timestamptz FROM live_ticket_sessions WHERE vault_id=$1 AND device_id=$2 AND session_id_hash=$3 AND session_incarnation=$4")
            .bind(p.vault).bind(p.device).bind(session).bind(incarnation).bind(&accepted_at)
            .fetch_one(scope.stage().await?).await?;
        if !active {
            terminalize_ticket_http(scope, p, session, incarnation, &accepted_at, "terminal")
                .await?;
            commit_ticket_http(scope).await?;
            return Ok(Err(lifetime_refused()));
        }
    } else {
        let retained: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM live_ticket_sessions WHERE vault_id=$1 AND device_id=$2",
        )
        .bind(p.vault)
        .bind(p.device)
        .fetch_one(scope.stage().await?)
        .await?;
        if retained >= 32 {
            return refuse_ticket_http(scope, ticket_rate_limited()).await;
        }
    }
    let (issued, created): (i64, i64) = sqlx::query_as("SELECT count(*) FILTER (WHERE event_kind='ticket_issue'),count(*) FILTER (WHERE event_kind='session_create') FROM live_ticket_admission_events WHERE vault_id=$1 AND device_id=$2 AND created_at>$3::timestamptz-interval '60 seconds'")
        .bind(p.vault).bind(p.device).bind(&accepted_at).fetch_one(scope.stage().await?).await?;
    if issued >= LIVE_TICKET_SESSION_LIMIT
        || (existing.is_none() && created >= LIVE_TICKET_SESSION_LIMIT)
    {
        return refuse_ticket_http(scope, ticket_rate_limited()).await;
    }
    let replaced_hash = replaceable.as_ref().map(|(hash, _)| hash.as_slice());
    let unused: i64 = sqlx::query_scalar("SELECT count(*) FROM live_tickets WHERE vault_id=$1 AND device_id=$2 AND consumed_at IS NULL AND expires_at>$3::timestamptz AND ($4::bytea IS NULL OR session_id_hash<>$4)")
        .bind(p.vault).bind(p.device).bind(&accepted_at).bind(replaced_hash)
        .fetch_one(scope.stage().await?).await?;
    if unused >= 2 {
        return refuse_ticket_http(scope, ticket_rate_limited()).await;
    }
    let incarnation = if let Some(incarnation) = existing {
        incarnation
    } else {
        let incarnation = match crate::live_ticket_authority::allocate_session_incarnation(
            scope.stage().await?,
            p.vault,
            p.device,
        )
        .await
        {
            Ok(value) => value,
            Err(crate::live_ticket_authority::AllocationError::Exhausted) => {
                return refuse_ticket_http(scope, lifetime_refused()).await;
            }
            Err(crate::live_ticket_authority::AllocationError::Database(error)) => {
                return Err(error.into());
            }
        };
        if let Some((old_session, old_incarnation)) = replaceable {
            terminalize_ticket_http(
                scope,
                p,
                &old_session,
                old_incarnation,
                &accepted_at,
                "terminal",
            )
            .await?;
        }
        sqlx::query("INSERT INTO live_ticket_sessions(vault_id,device_id,session_id_hash,session_incarnation,state,created_at,lease_until) VALUES($1,$2,$3,$4,'open',$5::timestamptz,$5::timestamptz+interval '90 seconds')")
            .bind(p.vault).bind(p.device).bind(session).bind(incarnation).bind(&accepted_at)
            .execute(scope.stage().await?).await?;
        incarnation
    };
    sqlx::query("UPDATE live_ticket_sessions SET lease_until=$5::timestamptz+interval '90 seconds' WHERE vault_id=$1 AND device_id=$2 AND session_id_hash=$3 AND session_incarnation=$4 AND state='open'")
        .bind(p.vault).bind(p.device).bind(session).bind(incarnation).bind(&accepted_at)
        .execute(scope.stage().await?).await?;
    let expires_at_ms: i64 = sqlx::query_scalar("INSERT INTO live_tickets(ticket_digest,vault_id,device_id,session_id_hash,session_incarnation,issued_at,expires_at) VALUES($1,$2,$3,$4,$5,$6::timestamptz,$6::timestamptz+interval '30 seconds') RETURNING (extract(epoch FROM expires_at)*1000)::bigint")
        .bind(Sha256::digest(ticket).as_slice()).bind(p.vault).bind(p.device).bind(session).bind(incarnation).bind(&accepted_at)
        .fetch_one(scope.stage().await?).await?;
    sqlx::query("INSERT INTO live_ticket_admission_events(event_id,vault_id,device_id,event_kind,created_at) VALUES($1,$2,$3,'ticket_issue',$4::timestamptz)")
        .bind(Uuid::new_v4()).bind(p.vault).bind(p.device).bind(&accepted_at)
        .execute(scope.stage().await?).await?;
    if existing.is_none() {
        sqlx::query("INSERT INTO live_ticket_admission_events(event_id,vault_id,device_id,event_kind,created_at) VALUES($1,$2,$3,'session_create',$4::timestamptz)")
            .bind(Uuid::new_v4()).bind(p.vault).bind(p.device).bind(&accepted_at)
            .execute(scope.stage().await?).await?;
    }
    commit_ticket_http(scope).await?;
    Ok(Ok(LiveTicketResponse {
        ticket: URL_SAFE_NO_PAD.encode(ticket),
        expires_at_ms,
        lifetime_handle: encode_live_ticket_lifetime_handle(incarnation),
    }))
}

fn ticket_rate_limited() -> ApiError {
    ApiError(StatusCode::TOO_MANY_REQUESTS, "live_ticket_rate_limited")
}

fn admit_ticket_http_attempt(
    state: &ApiState,
    p: &Principal,
    peer: Option<IpAddr>,
) -> ApiResult<()> {
    if state
        .live_ticket_http_limiter
        .lock()
        .expect("ticket HTTP limiter lock poisoned")
        .allow(DeviceId(p.device), peer)
    {
        Ok(())
    } else {
        Err(ticket_rate_limited())
    }
}

async fn close_live_ticket_session(
    State(state): State<ApiState>,
    PeerAddress(peer): PeerAddress,
    headers: HeaderMap,
    Json(request): Json<LiveTicketRequest>,
) -> ApiResult<StatusCode> {
    let deadline = ticket_http_deadline(&state);
    let session = live_ticket_session_hash(&request.session_id)?;
    let incarnation = required_lifetime_handle(&request.lifetime_handle)?;
    let p = await_ticket_http(
        deadline,
        authenticated_without_presence(&state, &headers, Operation::Sync),
    )
    .await?;
    admit_ticket_http_attempt(&state, &p, peer)?;
    let session_id: [u8; 16] = URL_SAFE_NO_PAD
        .decode(&request.session_id)
        .expect("live_ticket_session_hash validated session ID")
        .try_into()
        .expect("live_ticket_session_hash validated session length");
    let principal = p.clone();
    let workflow: TicketWorkflow<ApiResult<Option<crate::live::LiveTicketIdentity>>> = Box::new(
        move |scope| {
            Box::pin(async move {
                let p = &principal;
                if !begin_ticket_http_device(scope, p).await? {
                    return refuse_ticket_http(
                        scope,
                        ApiError(StatusCode::UNAUTHORIZED, "invalid_bearer"),
                    )
                    .await;
                }
                let current: Option<(String, i64, Option<i64>)> = sqlx::query_as("SELECT state,session_incarnation,active_connection_id FROM live_ticket_sessions WHERE vault_id=$1 AND device_id=$2 AND session_id_hash=$3 FOR UPDATE")
                .bind(p.vault).bind(p.device).bind(&session).fetch_optional(scope.stage().await?).await?;
                if !matches!(current, Some((ref state, current_incarnation, _)) if state == "open" && current_incarnation == incarnation)
                {
                    return refuse_ticket_http(scope, lifetime_refused()).await;
                }
                let accepted_at: String = sqlx::query_scalar("SELECT clock_timestamp()::text")
                    .fetch_one(scope.stage().await?)
                    .await?;
                terminalize_ticket_http(scope, p, &session, incarnation, &accepted_at, "closed")
                    .await?;
                let ticket = current.and_then(|(_, _, connection_id)| {
                    connection_id.map(|connection_id| {
                        crate::live::LiveTicketIdentity::new(session_id, incarnation, connection_id)
                    })
                });
                commit_ticket_http(scope).await?;
                Ok(Ok(ticket))
            })
        },
    );
    let ticket =
        run_ticket_http_work(&state.live_ticket_work, &state.db, deadline, workflow).await?;
    if let Some(ticket) = ticket {
        publish_committed_ticket_control(
            &state,
            &p,
            ticket,
            crate::live::LiveControlKind::Close,
            deadline,
        )
        .await;
    }
    await_ticket_http(deadline, async {
        touch_authenticated_presence(&state, &p).await;
        Ok(())
    })
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn publish_committed_ticket_control(
    state: &ApiState,
    p: &Principal,
    ticket: crate::live::LiveTicketIdentity,
    kind: crate::live::LiveControlKind,
    deadline: OperationDeadline,
) {
    let Some(remaining) = deadline.remaining() else {
        return;
    };
    state
        .live_hub
        .cancel_ticket_exact(VaultId(p.vault), DeviceId(p.device), ticket);
    #[cfg(test)]
    let backend = state
        .live_backend_override
        .clone()
        .unwrap_or_else(|| state.live_backend.clone());
    #[cfg(not(test))]
    let backend: Arc<dyn crate::live::LiveBackend> = state.live_backend.clone();
    // A missed control is repaired by exact durable revalidation, never a retry
    // of the committed authority mutation.
    let _ = tokio::time::timeout(
        remaining.min(Duration::from_millis(250)),
        backend.publish_control(VaultId(p.vault), DeviceId(p.device), ticket, kind),
    )
    .await;
}

async fn live_websocket(
    State(state): State<ApiState>,
    PeerAddress(peer): PeerAddress,
    headers: HeaderMap,
    lease: Option<Extension<crate::serving::ConnectionLease>>,
    shutdown: Option<Extension<tokio::sync::watch::Receiver<bool>>>,
    upgrade: WebSocketUpgrade,
) -> ApiResult<Response> {
    let http_deadline = ticket_http_deadline(&state);
    let mode = match crate::live_ticket_upgrade::classify_live_upgrade(
        &headers,
        state.live_trusted_origin.as_ref(),
    ) {
        Err(crate::live_ticket_upgrade::UpgradeRejection::ProtocolInvalid)
            if !headers.contains_key("authorization")
                && !headers.contains_key("sec-websocket-protocol") =>
        {
            authenticated(&state, &headers, Operation::Sync).await?;
            unreachable!("missing credentials cannot authenticate")
        }
        Ok(mode) => mode,
        Err(rejection) => {
            return Err({
                let status = match rejection {
                    crate::live_ticket_upgrade::UpgradeRejection::ProtocolInvalid
                    | crate::live_ticket_upgrade::UpgradeRejection::AuthModeMixed => {
                        StatusCode::BAD_REQUEST
                    }
                    crate::live_ticket_upgrade::UpgradeRejection::OriginForbidden => {
                        StatusCode::FORBIDDEN
                    }
                    crate::live_ticket_upgrade::UpgradeRejection::Unsupported => {
                        StatusCode::SERVICE_UNAVAILABLE
                    }
                };
                ApiError(status, rejection.code())
            });
        }
    };
    let membership_capable = membership_capability(&headers, &mode)?;
    let (principal, ticket, protocol) = match mode {
        crate::live_ticket_upgrade::UpgradeMode::Bearer => (
            authenticated(&state, &headers, Operation::Sync).await?,
            None,
            None,
        ),
        crate::live_ticket_upgrade::UpgradeMode::Ticket {
            session_id,
            secret,
            protocol_token,
        } => {
            let consumed = consume_live_ticket_owned(
                &state.live_ticket_work,
                &state.db,
                http_deadline,
                session_id,
                secret,
            )
            .await?;
            if let Some(replaced) = consumed.replaced {
                publish_committed_ticket_control(
                    &state,
                    &consumed.principal,
                    replaced,
                    crate::live::LiveControlKind::Replace,
                    http_deadline,
                )
                .await;
            }
            await_ticket_http(http_deadline, async {
                let active = still_active(&state, &consumed.principal)
                    .await
                    .map_err(|error| database_unavailable(&error, "live_ticket_recheck"))?;
                if !active {
                    return Err(ApiError(StatusCode::UNAUTHORIZED, "live_ticket_invalid"));
                }
                authorize(&state, &consumed.principal, Operation::Sync).await
            })
            .await?;
            (
                consumed.principal,
                Some(crate::live::LiveTicketIdentity::new(
                    consumed.session_id,
                    consumed.session_incarnation,
                    consumed.connection_id,
                )),
                Some(protocol_token),
            )
        }
    };
    #[cfg(test)]
    let backend = state
        .live_backend_override
        .clone()
        .unwrap_or_else(|| state.live_backend.clone());
    #[cfg(not(test))]
    let backend: Arc<dyn crate::live::LiveBackend> = state.live_backend.clone();
    let ready = if cfg!(test) {
        *state.live_readiness.borrow()
    } else {
        state.live_backend.is_ready()
    };
    if !ready {
        return Err(ApiError(
            StatusCode::SERVICE_UNAVAILABLE,
            "live_unavailable",
        ));
    }
    if !state
        .live_upgrade_limiter
        .lock()
        .expect("live upgrade limiter lock poisoned")
        .allow(DeviceId(principal.device), peer)
    {
        return Err(ApiError(
            StatusCode::TOO_MANY_REQUESTS,
            "live_upgrade_rate_limited",
        ));
    }
    let readiness = state.live_readiness.clone();
    let lease = lease.map(|Extension(lease)| lease);
    let shutdown = shutdown.map(|Extension(shutdown)| shutdown);
    let upgrade = upgrade.max_message_size(8_192).max_frame_size(8_192);
    let upgrade = match protocol {
        Some(protocol) => upgrade.protocols([protocol]),
        None => upgrade,
    };
    if ticket.is_some() {
        http_deadline
            .remaining()
            .ok_or_else(ticket_http_unavailable)?;
    }
    Ok(upgrade
        .on_upgrade(move |socket| {
            live_session(
                socket,
                state,
                principal,
                peer,
                backend,
                readiness,
                LiveSessionLifetime {
                    lease,
                    shutdown,
                    ticket,
                    membership_capable,
                },
            )
        })
        .into_response())
}

const MEMBERSHIP_CAPABILITY_HEADER: &str = "x-peppy-live-capabilities";
const MEMBERSHIP_CAPABILITY: &str = "membership-version-v1";

fn membership_capability(
    headers: &HeaderMap,
    mode: &crate::live_ticket_upgrade::UpgradeMode,
) -> ApiResult<bool> {
    let values: Vec<_> = headers
        .get_all(MEMBERSHIP_CAPABILITY_HEADER)
        .iter()
        .collect();
    if values.is_empty() {
        return Ok(false);
    }
    if !matches!(mode, crate::live_ticket_upgrade::UpgradeMode::Bearer)
        || values.len() != 1
        || values[0].to_str().ok() != Some(MEMBERSHIP_CAPABILITY)
    {
        return Err(ApiError(
            StatusCode::BAD_REQUEST,
            "live_membership_capability_invalid",
        ));
    }
    Ok(true)
}

#[derive(Clone, Copy)]
enum MembershipSnapshotFailure {
    PolicyDenied,
    InactiveCredential,
    Database(&'static str),
    InvalidSnapshot,
    Deadline,
    Cancelled,
    Unavailable,
}

impl MembershipSnapshotFailure {
    fn kind(self) -> &'static str {
        match self {
            Self::PolicyDenied => "policy_denied",
            Self::InactiveCredential => "inactive_credential",
            Self::Database(kind) => kind,
            Self::InvalidSnapshot => "invalid_snapshot",
            Self::Deadline => "deadline",
            Self::Cancelled => "cancelled",
            Self::Unavailable => "unavailable",
        }
    }
}

fn membership_database_failure(error: &sqlx::Error) -> MembershipSnapshotFailure {
    MembershipSnapshotFailure::Database(database_error_kind(error))
}

fn record_membership_snapshot_failure(failure: MembershipSnapshotFailure) {
    tracing::warn!(
        operation = "live_membership_snapshot",
        kind = failure.kind(),
        "live membership snapshot failed"
    );
}

async fn membership_snapshot(
    state: &ApiState,
    principal: &Principal,
    connection_id: &str,
) -> Result<MembershipBootstrapV1, MembershipSnapshotFailure> {
    authorize(state, principal, Operation::Sync)
        .await
        .map_err(|_| MembershipSnapshotFailure::PolicyDenied)?;
    let mut transaction = scope::begin_read_only_repeatable(&state.db, principal.vault)
        .await
        .map_err(|error| membership_database_failure(&error))?;
    let active: Option<i32> = sqlx::query_scalar(
        "SELECT 1 FROM devices d JOIN device_credentials c ON c.vault_id=d.vault_id AND c.device_id=d.device_id WHERE d.vault_id=$1 AND d.device_id=$2 AND c.token_digest=$3 AND d.revoked_at IS NULL AND c.revoked_at IS NULL",
    )
    .bind(principal.vault)
    .bind(principal.device)
    .bind(&principal.token_digest)
    .fetch_optional(&mut *transaction)
    .await
    .map_err(|error| membership_database_failure(&error))?;
    active.ok_or(MembershipSnapshotFailure::InactiveCredential)?;
    let version: i64 =
        sqlx::query_scalar("SELECT version FROM membership_versions WHERE vault_id=$1")
            .bind(principal.vault)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(|error| membership_database_failure(&error))?
            .ok_or(MembershipSnapshotFailure::InvalidSnapshot)?;
    #[cfg(test)]
    if let Some(gate) = &state.live_ticket_test_hooks.membership_after_version {
        gate.enter_after(1).await;
    }
    let rows = sqlx::query("SELECT device_id, revoked_at IS NOT NULL AS revoked, key_epoch::bigint AS key_epoch, role::text AS role FROM devices WHERE vault_id=$1 ORDER BY device_id")
        .bind(principal.vault)
        .fetch_all(&mut *transaction)
        .await
        .map_err(|error| membership_database_failure(&error))?;
    let mut roster = Vec::with_capacity(rows.len());
    for row in rows {
        let role = match row
            .try_get::<String, _>("role")
            .map_err(|_| MembershipSnapshotFailure::InvalidSnapshot)?
            .as_str()
        {
            "owner" => MembershipRoleV1::Owner,
            "gateway" => MembershipRoleV1::Gateway,
            "device" => MembershipRoleV1::Device,
            _ => return Err(MembershipSnapshotFailure::InvalidSnapshot),
        };
        let key_epoch = u32::try_from(
            row.try_get::<i64, _>("key_epoch")
                .map_err(|_| MembershipSnapshotFailure::InvalidSnapshot)?,
        )
        .map_err(|_| MembershipSnapshotFailure::InvalidSnapshot)?;
        if key_epoch == 0 {
            return Err(MembershipSnapshotFailure::InvalidSnapshot);
        }
        roster.push(MembershipDeviceV1 {
            device_id: DeviceId(
                row.try_get("device_id")
                    .map_err(|_| MembershipSnapshotFailure::InvalidSnapshot)?,
            ),
            revoked: row
                .try_get("revoked")
                .map_err(|_| MembershipSnapshotFailure::InvalidSnapshot)?,
            key_epoch,
            role,
        });
    }
    if roster.len() > peppy_protocol::MAX_MEMBERSHIP_ROSTER_ENTRIES {
        return Err(MembershipSnapshotFailure::InvalidSnapshot);
    }
    transaction
        .commit()
        .await
        .map_err(|error| membership_database_failure(&error))?;
    Ok(MembershipBootstrapV1 {
        r#type: "membership_bootstrap".into(),
        version: MEMBERSHIP_PROTOCOL_VERSION,
        connection_id: connection_id.into(),
        membership_version: MembershipVersionV1::parse(&version.to_string())
            .map_err(|_| MembershipSnapshotFailure::InvalidSnapshot)?,
        roster,
    })
}

async fn membership_snapshot_wait(
    state: &ApiState,
    principal: &Principal,
    connection_id: &str,
    deadline: OperationDeadline,
    cancellation: &mut tokio::sync::watch::Receiver<bool>,
    readiness: &mut tokio::sync::watch::Receiver<bool>,
    shutdown: &mut Option<tokio::sync::watch::Receiver<bool>>,
) -> Option<MembershipBootstrapV1> {
    let result = if *cancellation.borrow_and_update()
        || shutdown
            .as_mut()
            .is_some_and(|receiver| *receiver.borrow_and_update())
    {
        Err(MembershipSnapshotFailure::Cancelled)
    } else if !*readiness.borrow_and_update() {
        Err(MembershipSnapshotFailure::Unavailable)
    } else if let Some(remaining) = deadline.remaining() {
        tokio::select! {
            biased;
            _ = cancellation.changed() => Err(MembershipSnapshotFailure::Cancelled),
            _ = wait_for_shutdown(shutdown) => Err(MembershipSnapshotFailure::Cancelled),
            _ = readiness.changed() => Err(MembershipSnapshotFailure::Unavailable),
            result = tokio::time::timeout(remaining, membership_snapshot(state, principal, connection_id)) => {
                result.unwrap_or(Err(MembershipSnapshotFailure::Deadline))
            },
        }
    } else {
        Err(MembershipSnapshotFailure::Deadline)
    };
    match result {
        Ok(snapshot) => Some(snapshot),
        Err(failure) => {
            record_membership_snapshot_failure(failure);
            None
        }
    }
}

fn membership_control_shaped(text: &str) -> bool {
    serde_json::from_str::<Value>(text)
        .ok()
        .and_then(|value| value.get("type")?.as_str().map(str::to_owned))
        .is_some_and(|kind| kind.starts_with("membership_"))
}

async fn live_session(
    mut socket: WebSocket,
    state: ApiState,
    principal: Principal,
    peer: Option<IpAddr>,
    backend: Arc<dyn crate::live::LiveBackend>,
    mut readiness: tokio::sync::watch::Receiver<bool>,
    lifetime: LiveSessionLifetime,
) {
    let LiveSessionLifetime {
        lease,
        mut shutdown,
        ticket,
        membership_capable,
    } = lifetime;
    let operation_deadline = ticket.map(|_| live_ticket_operation_deadline(&state));
    let registration = match ticket {
        Some(ticket) => {
            let deadline = operation_deadline.expect("ticket session has an operation deadline");
            let snapshot = state
                .live_hub
                .ticket_snapshot(VaultId(principal.vault), DeviceId(principal.device));
            match live_ticket_revalidate(
                &state,
                &principal,
                ticket,
                false,
                deadline,
                None,
                &mut readiness,
                &mut shutdown,
            )
            .await
            {
                LiveRevalidation::Authorized => {}
                LiveRevalidation::Unauthorized => {
                    live_close(&mut socket, 1008, "live_unauthorized").await;
                    drop(lease);
                    return;
                }
                LiveRevalidation::Unavailable => {
                    live_close(&mut socket, 1011, "live_unavailable").await;
                    drop(lease);
                    return;
                }
                LiveRevalidation::Cancelled => {
                    drop(lease);
                    return;
                }
            }
            if let Some(status) = live_ticket_after_validation_hook(
                &state,
                deadline,
                None,
                &mut readiness,
                &mut shutdown,
            )
            .await
            {
                match status {
                    LiveRevalidation::Unavailable => {
                        live_close(&mut socket, 1011, "live_unavailable").await
                    }
                    LiveRevalidation::Unauthorized => {
                        live_close(&mut socket, 1008, "live_unauthorized").await
                    }
                    LiveRevalidation::Cancelled => {}
                    LiveRevalidation::Authorized => unreachable!(),
                }
                drop(lease);
                return;
            }
            match state.live_hub.register_ticket_if_current(
                VaultId(principal.vault),
                DeviceId(principal.device),
                snapshot,
                ticket,
            ) {
                Ok(registration) => registration,
                Err(crate::live::LiveTicketInstallRefusal::ChangedEpoch) => {
                    let snapshot = state
                        .live_hub
                        .ticket_snapshot(VaultId(principal.vault), DeviceId(principal.device));
                    match live_ticket_revalidate(
                        &state,
                        &principal,
                        ticket,
                        false,
                        deadline,
                        None,
                        &mut readiness,
                        &mut shutdown,
                    )
                    .await
                    {
                        LiveRevalidation::Authorized => {}
                        LiveRevalidation::Unauthorized => {
                            live_close(&mut socket, 1008, "live_unauthorized").await;
                            drop(lease);
                            return;
                        }
                        LiveRevalidation::Unavailable => {
                            live_close(&mut socket, 1011, "live_unavailable").await;
                            drop(lease);
                            return;
                        }
                        LiveRevalidation::Cancelled => {
                            drop(lease);
                            return;
                        }
                    }
                    if let Some(status) = live_ticket_after_validation_hook(
                        &state,
                        deadline,
                        None,
                        &mut readiness,
                        &mut shutdown,
                    )
                    .await
                    {
                        if matches!(status, LiveRevalidation::Unavailable) {
                            live_close(&mut socket, 1011, "live_unavailable").await;
                        }
                        drop(lease);
                        return;
                    }
                    match state.live_hub.register_ticket_if_current(
                        VaultId(principal.vault),
                        DeviceId(principal.device),
                        snapshot,
                        ticket,
                    ) {
                        Ok(registration) => registration,
                        Err(_) => {
                            live_close(&mut socket, 1008, "live_unauthorized").await;
                            drop(lease);
                            return;
                        }
                    }
                }
                Err(crate::live::LiveTicketInstallRefusal::CurrentOrNewerConnection) => {
                    live_close(&mut socket, 1008, "live_unauthorized").await;
                    drop(lease);
                    return;
                }
            }
        }
        None => state
            .live_hub
            .register(VaultId(principal.vault), DeviceId(principal.device)),
    };
    let generation = registration.generation;
    live_session_inner(
        socket,
        LiveSession {
            state: state.clone(),
            principal: principal.clone(),
            peer,
            backend,
            generation,
            ticket,
            operation_deadline,
            outgoing: registration.receiver,
            cancellation: registration.cancellation,
            readiness,
            shutdown,
            membership_capable,
        },
    )
    .await;
    match ticket {
        Some(ticket) => state.live_hub.cancel_ticket_exact(
            VaultId(principal.vault),
            DeviceId(principal.device),
            ticket,
        ),
        None => state.live_hub.unregister(
            VaultId(principal.vault),
            DeviceId(principal.device),
            generation,
        ),
    }
    drop(lease);
}

async fn live_session_inner(mut socket: WebSocket, session: LiveSession) {
    let LiveSession {
        state,
        principal,
        peer,
        backend,
        generation,
        ticket,
        operation_deadline,
        mut outgoing,
        mut cancellation,
        mut readiness,
        mut shutdown,
        membership_capable,
    } = session;
    let hello = match serde_json::to_string(&LiveHelloV2::new()) {
        Ok(hello) => hello,
        Err(_) => return live_close(&mut socket, 1011, "live_unavailable").await,
    };
    if !matches!(
        live_send(
            &mut socket,
            Message::Text(hello.into()),
            operation_deadline,
            &mut cancellation,
            &mut readiness,
            &mut shutdown,
        )
        .await,
        LiveWait::Complete
    ) {
        return;
    }
    let membership_connection_id =
        membership_capable.then(|| URL_SAFE_NO_PAD.encode(Uuid::new_v4().as_bytes()));
    if let Some(connection_id) = membership_connection_id.as_deref() {
        let membership_deadline = live_ticket_operation_deadline(&state);
        let Some(bootstrap) = membership_snapshot_wait(
            &state,
            &principal,
            connection_id,
            membership_deadline,
            &mut cancellation,
            &mut readiness,
            &mut shutdown,
        )
        .await
        else {
            return live_close(&mut socket, 1011, "live_unavailable").await;
        };
        let Ok(bootstrap) = serde_json::to_string(&MembershipControlV1::Bootstrap(bootstrap))
        else {
            return live_close(&mut socket, 1011, "live_unavailable").await;
        };
        if !matches!(
            live_send(
                &mut socket,
                Message::Text(bootstrap.into()),
                Some(membership_deadline),
                &mut cancellation,
                &mut readiness,
                &mut shutdown
            )
            .await,
            LiveWait::Complete
        ) {
            return;
        }
    }
    let recheck_interval = live_recheck_interval(&state);
    let mut recheck = tokio::time::interval_at(
        tokio::time::Instant::now() + recheck_interval,
        recheck_interval,
    );
    let mut challenge: Option<Vec<u8>> = None;
    loop {
        tokio::select! {
            _ = wait_for_shutdown(&mut shutdown) => return,
            changed = cancellation.changed() => {
                let _ = changed;
                return;
            }
            changed = readiness.changed() => {
                let _ = changed;
                if !*readiness.borrow_and_update() {
                    return live_close(&mut socket, 1011, "live_unavailable").await;
                }
            }
            _ = recheck.tick() => {
                let deadline = ticket.map(|_| live_ticket_recheck_operation_deadline(&state));
                let revalidation = match ticket {
                    Some(ticket) => live_ticket_revalidate(
                        &state,
                        &principal,
                        ticket,
                        true,
                        deadline.expect("ticket check has an operation deadline"),
                        Some(&mut cancellation),
                        &mut readiness,
                        &mut shutdown,
                    ).await,
                    None => live_revalidate(&state, &principal, &mut cancellation, &mut readiness).await,
                };
                match revalidation {
                LiveRevalidation::Authorized => {
                    if challenge.is_some() {
                        return;
                    }
                    let token = Uuid::new_v4().as_bytes().to_vec();
                    if !matches!(live_send(&mut socket, Message::Ping(token.clone().into()), deadline, &mut cancellation, &mut readiness, &mut shutdown).await, LiveWait::Complete) {
                        return;
                    }
                    challenge = Some(token);
                }
                LiveRevalidation::Unauthorized => return live_close(&mut socket, 1008, "live_unauthorized").await,
                LiveRevalidation::Unavailable => return live_close(&mut socket, 1011, "live_unavailable").await,
                    LiveRevalidation::Cancelled => return,
                }
            }
            incoming = socket.recv() => match incoming {
                Some(Ok(Message::Close(_))) | None => return,
                Some(Err(_)) => return live_close(&mut socket, 1008, "invalid_live_frame").await,
                Some(Ok(Message::Ping(payload))) => {
                    if !matches!(live_send(&mut socket, Message::Pong(payload), None, &mut cancellation, &mut readiness, &mut shutdown).await, LiveWait::Complete) { return; }
                }
                Some(Ok(Message::Pong(payload))) => {
                    if challenge.as_ref().is_some_and(|token| token.as_slice() == payload.as_ref()) {
                        challenge = None;
                    }
                }
                Some(Ok(Message::Binary(_))) => return live_close(&mut socket, 1008, "invalid_live_frame").await,
                Some(Ok(Message::Text(text))) => {
                    if let Some(connection_id) = membership_connection_id.as_deref()
                        && membership_control_shaped(&text)
                    {
                        match MembershipControlV1::parse(text.as_bytes()) {
                            Ok(MembershipControlV1::Challenge(challenge)) if challenge.connection_id == connection_id => {
                                let allowed = state.live_rate_limiter.lock().expect("live rate limiter lock poisoned")
                                    .allow(DeviceId(principal.device), peer);
                                if !allowed {
                                    return live_close(&mut socket, 1008, "live_rate_limited").await;
                                }
                                let membership_deadline = live_ticket_operation_deadline(&state);
                                let Some(response) = membership_snapshot_wait(
                                    &state,
                                    &principal,
                                    connection_id,
                                    membership_deadline,
                                    &mut cancellation,
                                    &mut readiness,
                                    &mut shutdown,
                                ).await else {
                                    return live_close(&mut socket, 1011, "live_unavailable").await;
                                };
                                let response = MembershipControlV1::Response(peppy_protocol::MembershipResponseV1 {
                                    r#type: "membership_response".into(),
                                    version: MEMBERSHIP_PROTOCOL_VERSION,
                                    connection_id: connection_id.into(),
                                    challenge_id: challenge.challenge_id,
                                    membership_version: response.membership_version,
                                    roster: response.roster,
                                });
                                let Ok(response) = serde_json::to_string(&response) else {
                                    return live_close(&mut socket, 1011, "live_unavailable").await;
                                };
                                if !matches!(live_send(&mut socket, Message::Text(response.into()), Some(membership_deadline), &mut cancellation, &mut readiness, &mut shutdown).await, LiveWait::Complete) { return; }
                                continue;
                            }
                            Ok(_) | Err(_) => return live_close(&mut socket, 1008, "invalid_live_frame").await,
                        }
                    }
                    let Ok(frame) = serde_json::from_str::<LiveFrameV2>(&text) else {
                        return live_close(&mut socket, 1008, "invalid_live_frame").await;
                    };
                    let allowed = state.live_rate_limiter.lock().expect("live rate limiter lock poisoned")
                        .allow(DeviceId(principal.device), peer);
                    if !allowed {
                        return live_close(&mut socket, 1008, "live_rate_limited").await;
                    }
                    let now_ms = live_now_ms();
                    match crate::live::LiveHub::validate_frame(DeviceId(principal.device), &frame, now_ms) {
                        Ok(()) => {}
                        Err(crate::live::LiveReject::Expired | crate::live::LiveReject::FutureTimestamp) => continue,
                        Err(crate::live::LiveReject::InvalidFrame | crate::live::LiveReject::SenderMismatch) => {
                            return live_close(&mut socket, 1008, "invalid_live_frame").await;
                        }
                    }
                    let deadline = ticket.map(|_| live_ticket_operation_deadline(&state));
                    if let Some(ticket) = ticket {
                        let deadline = deadline.expect("ticket frame has an operation deadline");
                        match live_ticket_revalidate(
                            &state,
                            &principal,
                            ticket,
                            false,
                            deadline,
                            Some(&mut cancellation),
                            &mut readiness,
                            &mut shutdown,
                        ).await {
                            LiveRevalidation::Authorized => {}
                            LiveRevalidation::Unauthorized => return live_close(&mut socket, 1008, "live_unauthorized").await,
                            LiveRevalidation::Unavailable => return live_close(&mut socket, 1011, "live_unavailable").await,
                            LiveRevalidation::Cancelled => return,
                        }
                        #[cfg(test)]
                        if let Some(status) = live_ticket_after_validation_hook(
                            &state,
                            deadline,
                            Some(&mut cancellation),
                            &mut readiness,
                            &mut shutdown,
                        )
                        .await
                        {
                            match status {
                                LiveRevalidation::Unavailable => return live_close(&mut socket, 1011, "live_unavailable").await,
                                LiveRevalidation::Cancelled => return,
                                LiveRevalidation::Authorized | LiveRevalidation::Unauthorized => unreachable!(),
                            }
                        }
                        if let Some(status) = live_ticket_signal_status(
                            deadline,
                            Some(&mut cancellation),
                            &mut readiness,
                            &mut shutdown,
                        ) {
                            match status {
                                LiveRevalidation::Unavailable => return live_close(&mut socket, 1011, "live_unavailable").await,
                                LiveRevalidation::Cancelled => return,
                                LiveRevalidation::Authorized | LiveRevalidation::Unauthorized => unreachable!(),
                            }
                        }
                    }
                    match live_publish(
                        backend.as_ref(),
                        VaultId(principal.vault),
                        frame.clone(),
                        deadline,
                        &mut cancellation,
                        &mut readiness,
                        &mut shutdown,
                        #[cfg(test)]
                        state.live_publish_timeout,
                    )
                    .await
                    {
                        LiveWait::Complete => {}
                        LiveWait::Cancelled => return,
                        LiveWait::Unavailable => {
                            return live_close(&mut socket, 1011, "live_unavailable").await;
                        }
                    }
                    if *cancellation.borrow_and_update() {
                        return;
                    }
                    // The backend was available when this frame was accepted;
                    // local fan-out is only a latency optimization. The hub
                    // fences it to this still-current socket generation.
                    let _ = state.live_hub.forward_current(
                        VaultId(principal.vault),
                        DeviceId(principal.device),
                        generation,
                        frame,
                        live_now_ms(),
                    );
                }
            },
            frame = outgoing.recv() => match frame {
                Some(frame) if live_frame_is_current(&frame, live_now_ms()) => {
                    let deadline = ticket.map(|_| live_ticket_operation_deadline(&state));
                    if let Some(ticket) = ticket {
                        let deadline = deadline.expect("ticket delivery has an operation deadline");
                        match live_ticket_revalidate(
                            &state,
                            &principal,
                            ticket,
                            false,
                            deadline,
                            Some(&mut cancellation),
                            &mut readiness,
                            &mut shutdown,
                        ).await {
                            LiveRevalidation::Authorized => {}
                            LiveRevalidation::Unauthorized => return live_close(&mut socket, 1008, "live_unauthorized").await,
                            LiveRevalidation::Unavailable => return live_close(&mut socket, 1011, "live_unavailable").await,
                            LiveRevalidation::Cancelled => return,
                        }
                        if let Some(status) = live_ticket_signal_status(
                            deadline,
                            Some(&mut cancellation),
                            &mut readiness,
                            &mut shutdown,
                        ) {
                            match status {
                                LiveRevalidation::Unavailable => return live_close(&mut socket, 1011, "live_unavailable").await,
                                LiveRevalidation::Cancelled => return,
                                LiveRevalidation::Authorized | LiveRevalidation::Unauthorized => unreachable!(),
                            }
                        }
                    }
                    let Some(frame_deadline) =
                        live_frame_delivery_deadline(&frame, live_now_ms_for(&state))
                    else {
                        continue;
                    };
                    let Some(frame_remaining) = frame_deadline.remaining() else {
                        continue;
                    };
                    let deadline = match deadline {
                        Some(deadline) => match deadline.remaining() {
                            Some(remaining) => {
                                Some(OperationDeadline::after(bounded_live_delivery_timeout(
                                    remaining,
                                    frame_remaining,
                                )))
                            }
                            None => return,
                        },
                        None => Some(OperationDeadline::after(bounded_live_delivery_timeout(
                            Duration::from_secs(5),
                            frame_remaining,
                        ))),
                    };
                    let Ok(frame) = serde_json::to_string(&frame) else { continue; };
                    if !matches!(live_send(&mut socket, Message::Text(frame.into()), deadline, &mut cancellation, &mut readiness, &mut shutdown).await, LiveWait::Complete) { return; }
                }
                Some(_) => continue,
                None => return,
            },
        }
    }
}

struct LiveSession {
    state: ApiState,
    principal: Principal,
    peer: Option<IpAddr>,
    backend: Arc<dyn crate::live::LiveBackend>,
    generation: u64,
    ticket: Option<crate::live::LiveTicketIdentity>,
    operation_deadline: Option<OperationDeadline>,
    outgoing: tokio::sync::mpsc::Receiver<LiveFrameV2>,
    cancellation: tokio::sync::watch::Receiver<bool>,
    readiness: tokio::sync::watch::Receiver<bool>,
    shutdown: Option<tokio::sync::watch::Receiver<bool>>,
    membership_capable: bool,
}

struct LiveSessionLifetime {
    lease: Option<crate::serving::ConnectionLease>,
    shutdown: Option<tokio::sync::watch::Receiver<bool>>,
    ticket: Option<crate::live::LiveTicketIdentity>,
    membership_capable: bool,
}

fn live_now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

fn live_frame_is_current(frame: &LiveFrameV2, now_ms: u64) -> bool {
    crate::live::LiveHub::validate_current(frame, now_ms).is_ok()
}

fn live_frame_delivery_deadline(frame: &LiveFrameV2, now_ms: u64) -> Option<OperationDeadline> {
    let expires_at_ms = frame.expires_at_ms.parse::<u64>().ok()?;
    let remaining_ms = expires_at_ms.checked_sub(now_ms)?;
    (remaining_ms > 0).then(|| OperationDeadline::after(Duration::from_millis(remaining_ms)))
}

fn bounded_live_delivery_timeout(existing: Duration, frame_remaining: Duration) -> Duration {
    existing.min(frame_remaining)
}

fn live_now_ms_for(state: &ApiState) -> u64 {
    #[cfg(test)]
    if let Some(clock_ms) = &state.live_ticket_test_hooks.clock_ms {
        return clock_ms.load(std::sync::atomic::Ordering::SeqCst);
    }
    #[cfg(not(test))]
    let _ = state;
    live_now_ms()
}

async fn live_close(socket: &mut WebSocket, code: u16, reason: &'static str) {
    let _ = tokio::time::timeout(Duration::from_secs(5), ws_close(socket, code, reason)).await;
}

fn live_effect_remaining(
    deadline: Option<OperationDeadline>,
    fallback: Duration,
) -> Option<Duration> {
    match deadline {
        Some(deadline) => deadline.remaining(),
        None => Some(fallback),
    }
}

async fn live_send(
    socket: &mut WebSocket,
    message: Message,
    deadline: Option<OperationDeadline>,
    cancellation: &mut tokio::sync::watch::Receiver<bool>,
    readiness: &mut tokio::sync::watch::Receiver<bool>,
    shutdown: &mut Option<tokio::sync::watch::Receiver<bool>>,
) -> LiveWait {
    if *cancellation.borrow_and_update() {
        return LiveWait::Cancelled;
    }
    if shutdown
        .as_mut()
        .is_some_and(|value| *value.borrow_and_update())
    {
        return LiveWait::Cancelled;
    }
    if !*readiness.borrow_and_update() {
        return LiveWait::Unavailable;
    }
    let Some(remaining) = live_effect_remaining(deadline, Duration::from_secs(5)) else {
        return LiveWait::Unavailable;
    };
    tokio::select! {
        biased;
        _ = cancellation.changed() => LiveWait::Cancelled,
        _ = wait_for_shutdown(shutdown) => LiveWait::Cancelled,
        _ = readiness.changed() => LiveWait::Unavailable,
        result = tokio::time::timeout(remaining, socket.send(message)) => {
            if matches!(result, Ok(Ok(()))) { LiveWait::Complete } else { LiveWait::Unavailable }
        },
    }
}

const LIVE_PUBLISH_TIMEOUT: Duration = Duration::from_secs(5);

#[cfg_attr(
    test,
    expect(
        clippy::too_many_arguments,
        reason = "test timeout accompanies the independent live stop signals"
    )
)]
async fn live_publish(
    backend: &dyn crate::live::LiveBackend,
    vault_id: VaultId,
    frame: LiveFrameV2,
    deadline: Option<OperationDeadline>,
    cancellation: &mut tokio::sync::watch::Receiver<bool>,
    readiness: &mut tokio::sync::watch::Receiver<bool>,
    shutdown: &mut Option<tokio::sync::watch::Receiver<bool>>,
    #[cfg(test)] publish_timeout: Duration,
) -> LiveWait {
    if *cancellation.borrow_and_update() {
        return LiveWait::Cancelled;
    }
    if shutdown
        .as_mut()
        .is_some_and(|value| *value.borrow_and_update())
    {
        return LiveWait::Cancelled;
    }
    if !*readiness.borrow_and_update() {
        return LiveWait::Unavailable;
    }
    let Some(remaining) = live_effect_remaining(
        deadline,
        #[cfg(test)]
        publish_timeout,
        #[cfg(not(test))]
        LIVE_PUBLISH_TIMEOUT,
    ) else {
        return LiveWait::Unavailable;
    };
    tokio::select! {
        biased;
        _ = cancellation.changed() => LiveWait::Cancelled,
        _ = wait_for_shutdown(shutdown) => LiveWait::Cancelled,
        _ = readiness.changed() => LiveWait::Unavailable,
        result = tokio::time::timeout(remaining, backend.publish(vault_id, frame)) => {
            if matches!(result, Ok(Ok(()))) { LiveWait::Complete } else { LiveWait::Unavailable }
        },
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum LiveWait {
    Complete,
    Cancelled,
    Unavailable,
}

enum LiveRevalidation {
    Authorized,
    Unauthorized,
    Unavailable,
    Cancelled,
}

#[cfg(test)]
const LIVE_RECHECK_INTERVAL: Duration = Duration::from_millis(50);
#[cfg(not(test))]
const LIVE_RECHECK_INTERVAL: Duration = Duration::from_secs(20);

fn live_recheck_interval(_state: &ApiState) -> Duration {
    #[cfg(test)]
    if let Some(interval) = _state.live_ticket_test_hooks.recheck_interval {
        return interval;
    }
    LIVE_RECHECK_INTERVAL
}

struct TicketWorkCancellationGuard(Option<tokio::sync::watch::Sender<bool>>);

impl TicketWorkCancellationGuard {
    fn cancel(&mut self) {
        if let Some(sender) = self.0.take() {
            let _ = sender.send(true);
        }
    }

    fn disarm(&mut self) {
        self.0 = None;
    }
}

impl Drop for TicketWorkCancellationGuard {
    fn drop(&mut self) {
        self.cancel();
    }
}

const LIVE_TICKET_OPERATION_BUDGET: Duration = Duration::from_secs(5);
const LIVE_TICKET_LEASE_DURATION: Duration = Duration::from_secs(90);

fn live_ticket_operation_deadline(_state: &ApiState) -> OperationDeadline {
    #[cfg(test)]
    if let Some(budget) = _state.live_ticket_test_hooks.operation_budget {
        return OperationDeadline::after(budget);
    }
    OperationDeadline::after(LIVE_TICKET_OPERATION_BUDGET)
}

fn live_ticket_recheck_operation_deadline(state: &ApiState) -> OperationDeadline {
    #[cfg(test)]
    if let Some(budget) = state.live_ticket_test_hooks.recheck_operation_budget {
        return OperationDeadline::after(budget);
    }
    live_ticket_operation_deadline(state)
}

fn live_ticket_lease_duration(_state: &ApiState) -> Duration {
    #[cfg(test)]
    if let Some(duration) = _state.live_ticket_test_hooks.lease_duration {
        return duration;
    }
    LIVE_TICKET_LEASE_DURATION
}

async fn live_ticket_after_validation_hook(
    state: &ApiState,
    deadline: OperationDeadline,
    cancellation: Option<&mut tokio::sync::watch::Receiver<bool>>,
    readiness: &mut tokio::sync::watch::Receiver<bool>,
    shutdown: &mut Option<tokio::sync::watch::Receiver<bool>>,
) -> Option<LiveRevalidation> {
    #[cfg(not(test))]
    {
        let _ = (state, deadline, cancellation, readiness, shutdown);
        None
    }
    #[cfg(test)]
    {
        let mut cancellation = cancellation;
        let Some(gate) = state.live_ticket_test_hooks.after_validation.as_ref() else {
            return live_ticket_signal_status(
                deadline,
                cancellation.as_deref_mut(),
                readiness,
                shutdown,
            );
        };
        let Some(remaining) = deadline.remaining() else {
            return Some(LiveRevalidation::Unavailable);
        };
        let wait = gate.enter();
        if let Some(cancellation) = cancellation.as_deref_mut() {
            tokio::select! {
                biased;
                changed = cancellation.changed() => {
                    let _ = changed;
                    return Some(LiveRevalidation::Cancelled);
                }
                _ = wait_for_shutdown(shutdown) => return Some(LiveRevalidation::Cancelled),
                changed = readiness.changed() => {
                    let _ = changed;
                    return Some(LiveRevalidation::Unavailable);
                }
                _ = tokio::time::sleep(remaining) => return Some(LiveRevalidation::Unavailable),
                _ = wait => {}
            }
        } else {
            tokio::select! {
                biased;
                _ = wait_for_shutdown(shutdown) => return Some(LiveRevalidation::Cancelled),
                changed = readiness.changed() => {
                    let _ = changed;
                    return Some(LiveRevalidation::Unavailable);
                }
                _ = tokio::time::sleep(remaining) => return Some(LiveRevalidation::Unavailable),
                _ = wait => {}
            }
        }
        live_ticket_signal_status(deadline, cancellation, readiness, shutdown)
    }
}

fn live_ticket_signal_status(
    deadline: OperationDeadline,
    cancellation: Option<&mut tokio::sync::watch::Receiver<bool>>,
    readiness: &mut tokio::sync::watch::Receiver<bool>,
    shutdown: &mut Option<tokio::sync::watch::Receiver<bool>>,
) -> Option<LiveRevalidation> {
    if cancellation.is_some_and(|value| *value.borrow_and_update()) {
        return Some(LiveRevalidation::Cancelled);
    }
    if shutdown
        .as_mut()
        .is_some_and(|value| *value.borrow_and_update())
    {
        return Some(LiveRevalidation::Cancelled);
    }
    if !*readiness.borrow_and_update() || deadline.remaining().is_none() {
        return Some(LiveRevalidation::Unavailable);
    }
    None
}

#[expect(
    clippy::too_many_arguments,
    reason = "ticket validation keeps each independent stop signal explicit"
)]
async fn live_ticket_revalidate(
    state: &ApiState,
    principal: &Principal,
    ticket: crate::live::LiveTicketIdentity,
    renew: bool,
    deadline: OperationDeadline,
    mut cancellation: Option<&mut tokio::sync::watch::Receiver<bool>>,
    readiness: &mut tokio::sync::watch::Receiver<bool>,
    shutdown: &mut Option<tokio::sync::watch::Receiver<bool>>,
) -> LiveRevalidation {
    if let Some(status) =
        live_ticket_signal_status(deadline, cancellation.as_deref_mut(), readiness, shutdown)
    {
        return status;
    }

    // The role and exact credential presented at ticket consumption are checked
    // together under the device/credential lock. Another active credential for
    // the same device cannot keep a revoked socket authorized.
    let authorization = state.policy.authorize(
        AccessPrincipal {
            vault_id: principal.vault,
            device_id: principal.device,
            role: principal.role.clone(),
        },
        Operation::Sync,
    );
    let Some(remaining) = deadline.remaining() else {
        return LiveRevalidation::Unavailable;
    };
    let authorized = if let Some(cancellation) = cancellation.as_deref_mut() {
        tokio::select! {
            biased;
            changed = cancellation.changed() => {
                let _ = changed;
                return LiveRevalidation::Cancelled;
            }
            _ = wait_for_shutdown(shutdown) => return LiveRevalidation::Cancelled,
            changed = readiness.changed() => {
                let _ = changed;
                return LiveRevalidation::Unavailable;
            }
            result = tokio::time::timeout(remaining, authorization) => match result {
                Ok(value) => value,
                Err(_) => return LiveRevalidation::Unavailable,
            },
        }
    } else {
        tokio::select! {
            biased;
            _ = wait_for_shutdown(shutdown) => return LiveRevalidation::Cancelled,
            changed = readiness.changed() => {
                let _ = changed;
                return LiveRevalidation::Unavailable;
            }
            result = tokio::time::timeout(remaining, authorization) => match result {
                Ok(value) => value,
                Err(_) => return LiveRevalidation::Unavailable,
            },
        }
    };
    if let Some(status) =
        live_ticket_signal_status(deadline, cancellation.as_deref_mut(), readiness, shutdown)
    {
        return status;
    }
    if !authorized {
        return LiveRevalidation::Unauthorized;
    }

    let (work_cancel, work_cancellation) = tokio::sync::watch::channel(false);
    #[cfg(test)]
    state
        .live_ticket_test_hooks
        .work_probe
        .record_receiver(work_cancellation.clone());
    let mut cancellation_guard = TicketWorkCancellationGuard(Some(work_cancel));
    let expected_role = principal.role.clone();
    let expected_token_digest = principal.token_digest.clone();
    let vault = principal.vault;
    let device = principal.device;
    let session_hash = ticket.session_hash().to_vec();
    let lease_duration_millis = i64::try_from(live_ticket_lease_duration(state).as_millis())
        .expect("live ticket lease duration fits in PostgreSQL interval milliseconds");
    #[cfg(test)]
    let test_hooks = state.live_ticket_test_hooks.clone();
    let workflow: TicketWorkflow<bool> = Box::new(move |scope| {
        Box::pin(async move {
            #[cfg(test)]
            let _workflow_drop_probe = LiveTicketWorkflowDropProbe(test_hooks.work_probe.clone());
            scope.stage().await?.execute("BEGIN").await?;
            sqlx::query("SELECT set_config('peppy.vault_id',$1,true)")
                .bind(vault.to_string())
                .execute(scope.stage().await?)
                .await?;
            scope
                .claim_device(TenantDevice {
                    vault_id: vault,
                    device_id: device,
                })
                .await?;
            #[cfg(test)]
            if let Some(gate) = test_hooks.after_device_claim.as_ref() {
                gate.enter().await;
            }
            #[cfg(test)]
            test_hooks
                .credential_query_started
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let current_role: Option<String> = sqlx::query_scalar("SELECT d.role FROM devices d JOIN device_credentials c ON c.vault_id=d.vault_id AND c.device_id=d.device_id WHERE d.vault_id=$1 AND d.device_id=$2 AND c.token_digest=$3 AND d.revoked_at IS NULL AND c.revoked_at IS NULL FOR UPDATE OF d,c")
                .bind(vault)
                .bind(device)
                .bind(&expected_token_digest)
                .fetch_optional(scope.stage().await?)
                .await?;
            #[cfg(test)]
            test_hooks
                .session_query_started
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let current: Option<(String, i64, Option<i64>)> = sqlx::query_as("SELECT state,session_incarnation,active_connection_id FROM live_ticket_sessions WHERE vault_id=$1 AND device_id=$2 AND session_id_hash=$3 FOR UPDATE")
                .bind(vault)
                .bind(device)
                .bind(&session_hash)
                .fetch_optional(scope.stage().await?)
                .await?;
            let sample_started = std::time::Instant::now();
            let sampled_at: String = sqlx::query_scalar("SELECT clock_timestamp()::text")
                .fetch_one(scope.stage().await?)
                .await?;
            let sample_finished = std::time::Instant::now();
            debug_assert!(sample_finished >= sample_started);
            #[cfg(test)]
            {
                test_hooks
                    .sampled_at
                    .lock()
                    .expect("sample record lock poisoned")
                    .push(sampled_at.clone());
                if let Some(gate) = test_hooks.after_sample.as_ref() {
                    gate.enter().await;
                }
            }
            let exact_open = matches!(
                current,
                Some((ref state, incarnation, Some(connection)))
                    if state == "open"
                        && incarnation == ticket.session_incarnation()
                        && connection == ticket.connection_id()
            );
            if current_role.as_deref() != Some(expected_role.as_str()) || !exact_open {
                scope.stage().await?.execute("ROLLBACK").await?;
                scope.settled();
                return Ok(false);
            }
            let valid: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM live_ticket_sessions WHERE vault_id=$1 AND device_id=$2 AND session_id_hash=$3 AND session_incarnation=$4 AND active_connection_id=$5 AND state='open' AND lease_until>$6::timestamptz)")
                .bind(vault)
                .bind(device)
                .bind(&session_hash)
                .bind(ticket.session_incarnation())
                .bind(ticket.connection_id())
                .bind(&sampled_at)
                .fetch_one(scope.stage().await?)
                .await?;
            if !valid {
                scope.stage().await?.execute("ROLLBACK").await?;
                scope.settled();
                return Ok(false);
            }
            if renew {
                #[cfg(test)]
                test_hooks
                    .renewal_update_started
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let updated = sqlx::query("UPDATE live_ticket_sessions SET lease_until=$6::timestamptz+($7*interval '1 millisecond') WHERE vault_id=$1 AND device_id=$2 AND session_id_hash=$3 AND session_incarnation=$4 AND active_connection_id=$5 AND state='open' AND lease_until>$6::timestamptz")
                    .bind(vault)
                    .bind(device)
                    .bind(&session_hash)
                    .bind(ticket.session_incarnation())
                    .bind(ticket.connection_id())
                    .bind(&sampled_at)
                    .bind(lease_duration_millis)
                    .execute(scope.stage().await?)
                    .await?;
                if updated.rows_affected() != 1 {
                    scope.stage().await?.execute("ROLLBACK").await?;
                    scope.settled();
                    return Ok(false);
                }
            }
            #[cfg(test)]
            {
                if let Some(gate) = test_hooks.before_commit.as_ref() {
                    gate.enter().await;
                }
                test_hooks
                    .commit_started
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }
            scope.stage().await?.execute("COMMIT").await?;
            #[cfg(test)]
            if let Some(gate) = test_hooks.after_commit.as_ref() {
                gate.enter().await;
            }
            scope.settled();
            Ok(true)
        })
    });
    let Ok(work) =
        state
            .live_ticket_work
            .spawn(state.db.clone(), deadline, work_cancellation, workflow)
    else {
        return LiveRevalidation::Unavailable;
    };
    let outcome = if let Some(cancellation) = cancellation.as_deref_mut() {
        tokio::select! {
            biased;
            changed = cancellation.changed() => {
                let _ = changed;
                return LiveRevalidation::Cancelled;
            }
            _ = wait_for_shutdown(shutdown) => return LiveRevalidation::Cancelled,
            changed = readiness.changed() => {
                let _ = changed;
                return LiveRevalidation::Unavailable;
            }
            result = work.outcome() => result,
        }
    } else {
        tokio::select! {
            biased;
            _ = wait_for_shutdown(shutdown) => return LiveRevalidation::Cancelled,
            changed = readiness.changed() => {
                let _ = changed;
                return LiveRevalidation::Unavailable;
            }
            result = work.outcome() => result,
        }
    };
    cancellation_guard.disarm();
    if let Some(status) = live_ticket_signal_status(deadline, cancellation, readiness, shutdown) {
        return status;
    }
    match outcome {
        Ok(true) => LiveRevalidation::Authorized,
        Ok(false) => LiveRevalidation::Unauthorized,
        Err(TicketDbWorkError::Cancelled) => LiveRevalidation::Cancelled,
        Err(error) => {
            let _ = ticket_work_unavailable(&error, "live_ticket_revalidate");
            LiveRevalidation::Unavailable
        }
    }
}

async fn live_revalidate(
    state: &ApiState,
    principal: &Principal,
    cancellation: &mut tokio::sync::watch::Receiver<bool>,
    readiness: &mut tokio::sync::watch::Receiver<bool>,
) -> LiveRevalidation {
    if *cancellation.borrow_and_update() {
        return LiveRevalidation::Cancelled;
    }
    if !*readiness.borrow_and_update() {
        return LiveRevalidation::Unavailable;
    }
    let result = tokio::time::timeout(Duration::from_secs(5), async {
        match still_active(state, principal).await {
            Ok(false) => Ok(false),
            Ok(true) => authorize(state, principal, Operation::Sync)
                .await
                .map(|_| true),
            Err(_) => Err(ApiError(
                StatusCode::SERVICE_UNAVAILABLE,
                "live_unavailable",
            )),
        }
    })
    .await;
    if *cancellation.borrow_and_update() {
        return LiveRevalidation::Cancelled;
    }
    if !*readiness.borrow_and_update() {
        return LiveRevalidation::Unavailable;
    }
    match result {
        Ok(Ok(true)) => LiveRevalidation::Authorized,
        Ok(Ok(false)) | Ok(Err(ApiError(StatusCode::FORBIDDEN, _))) => {
            LiveRevalidation::Unauthorized
        }
        Ok(Err(_)) | Err(_) => LiveRevalidation::Unavailable,
    }
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
    Revoked,
    Resync(sync::Resync),
    Backpressure,
    Database,
    Disconnected,
}

impl WsStop {
    async fn close(self, socket: &mut WebSocket) {
        match self {
            Self::PolicyDenied => ws_close(socket, 1008, "access_denied").await,
            Self::Revoked => ws_close(socket, close::REVOKED, "revoked").await,
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

#[allow(clippy::too_many_arguments)]
async fn ws_session(
    socket: WebSocket,
    state: ApiState,
    principal: Principal,
    notices: tokio::sync::broadcast::Receiver<(Uuid, i64)>,
    presence_notices: tokio::sync::broadcast::Receiver<Uuid>,
    drafts_notices: tokio::sync::broadcast::Receiver<Uuid>,
    lease: Option<crate::serving::ConnectionLease>,
    shutdown: Option<tokio::sync::watch::Receiver<bool>>,
) {
    ws_session_inner(
        socket,
        state,
        principal,
        notices,
        presence_notices,
        drafts_notices,
        shutdown,
    )
    .await;
    drop(lease);
}

async fn ws_session_inner(
    mut socket: WebSocket,
    state: ApiState,
    principal: Principal,
    mut notices: tokio::sync::broadcast::Receiver<(Uuid, i64)>,
    mut presence_notices: tokio::sync::broadcast::Receiver<Uuid>,
    mut drafts_notices: tokio::sync::broadcast::Receiver<Uuid>,
    mut shutdown: Option<tokio::sync::watch::Receiver<bool>>,
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
        "frame_types": ["ready", "event", "resync_required", "presence_changed", "drafts_changed"],
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
    let mut draft_hint_coalescer = DraftHintCoalescer::default();
    let mut draft_hint_cooldown = Box::pin(tokio::time::sleep(Duration::from_secs(3_600)));
    loop {
        let stop = tokio::select! {
            _ = wait_for_shutdown(&mut shutdown) => WsStop::Disconnected,
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
                        if presence_active
                            && let Ok(mut tx) = scope::begin(&state.db, principal.vault).await
                            && let Ok(changed) = crate::presence::renew(
                                &mut tx,
                                principal.vault,
                                principal.device,
                                connection_id,
                            )
                            .await
                            && tx.commit().await.is_ok()
                            && changed
                        {
                            let _ = state.presence_changed.send(principal.vault);
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
            draft_notice = drafts_notices.recv() => {
                match draft_notice {
                    Ok(vault) if vault == principal.vault => {}
                    Ok(_) => continue,
                    // The channel carries hints only; after lag, one local hint
                    // is enough to prompt the receiver to pull its durable state.
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        if presence_active { close_presence(&state, &principal, connection_id).await; }
                        return
                    }
                }
                if !draft_hint_coalescer.notice(tokio::time::Instant::now()) {
                    continue;
                }
                if send_frame(&mut socket, json!({"type":"drafts_changed"}).to_string(), options.send_timeout).await.is_err() {
                    WsStop::Disconnected
                } else {
                    let cooldown_until = draft_hint_coalescer.sent(tokio::time::Instant::now());
                    draft_hint_cooldown.as_mut().reset(cooldown_until);
                    continue
                }
            },
            _ = &mut draft_hint_cooldown, if draft_hint_coalescer.cooling_down() => {
                if !draft_hint_coalescer.timeout_expired(tokio::time::Instant::now()) {
                    continue;
                }
                if send_frame(&mut socket, json!({"type":"drafts_changed"}).to_string(), options.send_timeout).await.is_err() {
                    WsStop::Disconnected
                } else {
                    let cooldown_until = draft_hint_coalescer.sent(tokio::time::Instant::now());
                    draft_hint_cooldown.as_mut().reset(cooldown_until);
                    continue
                }
            },
        };
        if presence_active {
            close_presence(&state, &principal, connection_id).await;
        }
        return stop.close(&mut socket).await;
    }
}

async fn wait_for_shutdown(shutdown: &mut Option<tokio::sync::watch::Receiver<bool>>) {
    let Some(shutdown) = shutdown else {
        std::future::pending().await
    };
    if *shutdown.borrow() {
        return;
    }
    let _ = shutdown.changed().await;
}

async fn close_presence(state: &ApiState, principal: &Principal, connection_id: Uuid) {
    if crate::presence::close(&state.db, principal.vault, connection_id)
        .await
        .unwrap_or(false)
    {
        let _ = state.presence_changed.send(principal.vault);
    }
}
pub(super) async fn still_active(state: &ApiState, p: &Principal) -> Result<bool, sqlx::Error> {
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
        match still_active(state, principal).await {
            Ok(true) => {}
            Ok(false) => return Err(WsStop::Revoked),
            Err(error) => {
                tracing::warn!(error_kind = %error, "websocket replay revalidation failed");
                return Err(WsStop::Database);
            }
        }
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
mod live_socket_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expired_recipient_frame_has_no_delivery_deadline() {
        let frame = LiveFrameV2 {
            protocol_version: peppy_protocol::EPHEMERAL_V2_PROTOCOL_VERSION,
            kind: peppy_protocol::EphemeralSignalV2Kind::Typing,
            sender_device_id: DeviceId(Uuid::new_v4()),
            key_epoch: 1,
            session_id: Uuid::new_v4().to_string(),
            sent_at_ms: "1".into(),
            expires_at_ms: "2".into(),
            sequence: "1".into(),
            ciphertext_b64: STANDARD.encode([1_u8]),
        };

        assert!(live_frame_delivery_deadline(&frame, 3).is_none());
    }

    #[test]
    fn live_delivery_timeout_uses_the_smaller_send_and_frame_bounds() {
        assert_eq!(
            bounded_live_delivery_timeout(Duration::from_secs(5), Duration::from_secs(15)),
            Duration::from_secs(5)
        );
        assert_eq!(
            bounded_live_delivery_timeout(Duration::from_secs(5), Duration::from_millis(20)),
            Duration::from_millis(20)
        );
    }

    #[test]
    fn heartbeat_disconnects_before_replacing_an_unanswered_challenge() {
        assert_eq!(heartbeat_action(false), HeartbeatAction::SendChallenge);
        assert_eq!(heartbeat_action(true), HeartbeatAction::Disconnect);
    }

    #[test]
    fn draft_hint_coalescer_enforces_leading_and_trailing_windows() {
        let start = tokio::time::Instant::now();
        let mut coalescer = DraftHintCoalescer::default();

        assert!(coalescer.notice(start));
        coalescer.sent(start);
        for _ in 0..100 {
            assert!(!coalescer.notice(start + Duration::from_millis(100)));
        }
        assert!(!coalescer.notice(start + Duration::from_millis(499)));
        assert!(!coalescer.timeout_expired(start + Duration::from_millis(499)));
        assert!(coalescer.timeout_expired(start + Duration::from_millis(500)));

        coalescer.sent(start + Duration::from_millis(500));
        assert!(!coalescer.notice(start + Duration::from_millis(500)));
        assert!(!coalescer.timeout_expired(start + Duration::from_millis(999)));
        assert!(coalescer.timeout_expired(start + Duration::from_millis(1_000)));
    }

    #[test]
    fn draft_hint_coalescer_has_no_trailing_send_without_a_pending_notice() {
        let start = tokio::time::Instant::now();
        let mut coalescer = DraftHintCoalescer::default();

        assert!(coalescer.notice(start));
        coalescer.sent(start);
        assert!(!coalescer.timeout_expired(start + Duration::from_millis(500)));
        assert!(coalescer.notice(start + Duration::from_millis(500)));
    }

    #[test]
    fn draft_hint_cooldown_starts_after_send_completion() {
        let notice_at = tokio::time::Instant::now();
        let send_completed_at = notice_at + Duration::from_millis(100);
        let mut coalescer = DraftHintCoalescer::default();

        assert!(coalescer.notice(notice_at));
        coalescer.sent(send_completed_at);
        assert!(!coalescer.notice(send_completed_at));
        assert!(!coalescer.timeout_expired(notice_at + Duration::from_millis(500)));
        assert!(!coalescer.timeout_expired(notice_at + Duration::from_millis(599)));
        assert!(coalescer.timeout_expired(notice_at + Duration::from_millis(600)));
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

    #[test]
    fn community_policy_keeps_the_transaction_policy_extension_object_safe() {
        fn accepts_trait_object(_: &dyn AccessPolicy) {}

        accepts_trait_object(&CommunityAccessPolicy);
        let _ = <CommunityAccessPolicy as AccessPolicy>::attachment_storage_policy_in_transaction;
    }

    #[test]
    fn open_setup_window_closes_after_sixty_minutes() {
        let started = std::time::Instant::now();

        assert!(fresh_claim_is_authorized(
            crate::instance::ClaimPolicy::Open,
            started,
            started + Duration::from_secs(59 * 60 + 59),
            ""
        ));
        assert!(!fresh_claim_is_authorized(
            crate::instance::ClaimPolicy::Open,
            started,
            started + Duration::from_secs(60 * 60),
            ""
        ));
    }

    #[test]
    fn configured_setup_token_is_not_limited_by_the_open_window() {
        let started = std::time::Instant::now();
        let token = "a".repeat(32);

        assert!(fresh_claim_is_authorized(
            crate::instance::ClaimPolicy::TokenRequired(crate::instance::digest(token.as_bytes())),
            started,
            started + Duration::from_secs(60 * 60),
            &token
        ));
    }

    #[test]
    fn host_cookie_wins_over_a_legacy_cookie() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "cookie",
            HeaderValue::from_static(
                "Peppy-Instance-Session=legacy; __Host-Peppy-Instance-Session=current",
            ),
        );

        assert_eq!(cookie_token(&headers), Some("current"));
    }
}
