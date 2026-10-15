pub mod api;
pub mod config;
pub mod enrollment;
pub mod health;
pub mod instance;
pub mod scope;
pub mod storage;
pub mod web_client;

pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!();

/// Serves a router while attaching each TCP peer address to request extensions.
pub async fn serve<S>(
    listener: tokio::net::TcpListener,
    router: Router,
    shutdown: S,
) -> std::io::Result<()>
where
    S: std::future::Future<Output = ()> + Send + 'static,
{
    let (stream_shutdown, stream_shutdown_receiver) = tokio::sync::watch::channel(false);
    let shutdown = async move {
        shutdown.await;
        let _ = stream_shutdown.send(true);
    };
    axum::serve(
        listener,
        router
            .layer(axum::Extension(stream_shutdown_receiver))
            .into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown)
    .await
}

use axum::{Router, routing::get};
use health::HealthState;
use sqlx::PgPool;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::{
    api::{
        AccessPolicy, CommunityAccessPolicy, CommunityEnrollmentAuthorization,
        EnrollmentAuthorization, TransportOptions,
    },
    config::Config,
};
use peppy_hosted_api::PushConfigAdvertisement;
use std::sync::Arc;

/// Explicit server assembly. Unlike the legacy API router helpers, this does
/// not start background work while building routes.
pub struct ServerBuilder {
    config: Config,
    runtime_db: PgPool,
    maintenance_db: Option<PgPool>,
    access_policy: Arc<dyn AccessPolicy>,
    enrollment_authorization: Arc<dyn EnrollmentAuthorization>,
    setup_token: Option<String>,
    row_security: bool,
    push_config: PushConfigAdvertisement,
}

impl ServerBuilder {
    /// Assembles a self-hosted community server with no hosted authorization
    /// adapter or setup-token override.
    pub fn community(config: Config, runtime_db: PgPool) -> Self {
        Self::new(config, runtime_db)
    }

    pub fn new(config: Config, runtime_db: PgPool) -> Self {
        let enrollment_authorization = Arc::new(CommunityEnrollmentAuthorization::with_origin(
            runtime_db.clone(),
            config.public_api_url.as_ref(),
        ));
        Self {
            config,
            runtime_db: runtime_db.clone(),
            maintenance_db: None,
            access_policy: Arc::new(CommunityAccessPolicy),
            enrollment_authorization,
            setup_token: None,
            row_security: false,
            push_config: PushConfigAdvertisement::default(),
        }
    }

    /// Supplies the operator setup token directly, overriding `PEPPY_SETUP_TOKEN`.
    /// An empty token explicitly selects open first-claim setup.
    pub fn setup_token(mut self, token: impl Into<String>) -> Self {
        self.setup_token = Some(token.into());
        self
    }

    pub fn maintenance_database(mut self, database: PgPool) -> Self {
        self.maintenance_db = Some(database);
        self
    }

    pub fn access_policy(mut self, policy: Arc<dyn AccessPolicy>) -> Self {
        self.access_policy = policy;
        self
    }

    /// Supplies hosted/account enrollment authorization without coupling the
    /// reusable server to a private account implementation.
    pub fn enrollment_authorization(
        mut self,
        authorization: Arc<dyn EnrollmentAuthorization>,
    ) -> Self {
        self.enrollment_authorization = authorization;
        self
    }

    /// Enables the hosted resolver path for a runtime role constrained by RLS.
    /// Community deployments retain direct credential lookups by default.
    pub fn row_security(mut self, enabled: bool) -> Self {
        self.row_security = enabled;
        self
    }

    /// Advertises hosted push capability to enrolled devices.
    pub fn push_config(mut self, advertisement: PushConfigAdvertisement) -> Self {
        self.push_config = advertisement;
        self
    }

    /// Initializes health/storage dependencies and assembles routes without
    /// spawning maintenance workers. Call [`Server::start_maintenance`] after
    /// the process has completed its startup sequencing.
    pub async fn build(self) -> Result<Server, Box<dyn std::error::Error + Send + Sync>> {
        if self.row_security && self.maintenance_db.is_none() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "row-security mode requires an explicit maintenance database",
            )
            .into());
        }
        if !self.push_config.validate() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "push configuration is inconsistent",
            )
            .into());
        }
        let claim_policy = match self.setup_token.as_deref() {
            Some(token) => instance::claim_policy(self.row_security, Some(token)),
            None => instance::configured_claim_policy(self.row_security),
        }
        .map_err(|message| std::io::Error::new(std::io::ErrorKind::InvalidInput, message))?;
        let health = HealthState::from_database(&self.config, self.runtime_db.clone()).await?;
        let maintenance_db = self
            .maintenance_db
            .unwrap_or_else(|| self.runtime_db.clone());
        let options = TransportOptions {
            replay_retention: self.config.replay_retention,
            ..TransportOptions::default()
        };
        let (api, mut maintenance) = api::build_router(
            self.runtime_db,
            Some(self.config),
            health.storage(),
            options,
            None,
            self.push_config,
            api::RouterAuthorization {
                policy: self.access_policy,
                enrollment: self.enrollment_authorization,
                claim_policy,
                row_security: self.row_security,
            },
        );
        maintenance.db = maintenance_db;
        let router = Router::new()
            .route("/healthz", get(health::liveness))
            .route("/readyz", get(health::readiness))
            .route("/__release", get(health::release))
            .with_state(health)
            .merge(api);
        Ok(Server {
            router,
            maintenance: Some(maintenance),
            maintenance_started: AtomicBool::new(false),
        })
    }
}

#[derive(Debug, thiserror::Error)]
#[error("maintenance has already been started")]
pub struct MaintenanceAlreadyStarted;

/// An assembled server. Cloning its router does not start additional workers.
pub struct Server {
    router: Router,
    maintenance: Option<api::Maintenance>,
    maintenance_started: AtomicBool,
}

impl Server {
    pub fn router(&self) -> Router {
        self.router.clone()
    }

    /// Starts this server's maintenance workers exactly once.
    pub fn start_maintenance(&mut self) -> Result<(), MaintenanceAlreadyStarted> {
        if self
            .maintenance_started
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Err(MaintenanceAlreadyStarted);
        }
        if let Some(maintenance) = self.maintenance.take() {
            maintenance.start();
        }
        Ok(())
    }
}

pub fn app(state: HealthState) -> Router {
    let api = api::router(state.database());
    Router::new()
        .route("/healthz", get(health::liveness))
        .route("/readyz", get(health::readiness))
        .route("/__release", get(health::release))
        .with_state(state)
        .merge(api)
}
