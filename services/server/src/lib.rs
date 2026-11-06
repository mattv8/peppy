pub mod api;
pub mod attachment_retention;
pub mod config;
pub mod enrollment;
pub mod health;
pub mod instance;
pub(crate) mod live;
pub(crate) mod live_postgres;
pub(crate) mod live_ticket_authority;
pub(crate) mod live_ticket_maintenance;
pub mod live_ticket_upgrade;
pub mod presence;
pub mod rls;
pub mod scope;
pub(crate) mod serving;
pub mod storage;
pub mod web_client;

pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!();

/// Minimal ticket-consumption result for database integration tests.
#[doc(hidden)]
pub struct ConsumedTicketIds {
    pub vault_id: uuid::Uuid,
    pub device_id: uuid::Uuid,
    pub session_id: [u8; 16],
}

/// Exercises the resolver-first ticket consume path without exposing credentials.
#[doc(hidden)]
pub async fn __test_consume_live_ticket(
    pool: &PgPool,
    session_id: [u8; 16],
    secret: [u8; 32],
) -> Result<ConsumedTicketIds, &'static str> {
    api::__test_consume_live_ticket(pool, session_id, secret).await
}

/// Serves a router while attaching each TCP peer address to request extensions.
pub async fn serve<S>(
    listener: tokio::net::TcpListener,
    router: Router,
    shutdown: S,
) -> std::io::Result<()>
where
    S: std::future::Future<Output = ()> + Send + 'static,
{
    serving::serve(listener, router, shutdown).await
}

use axum::{Router, routing::get};
use health::HealthState;
use sqlx::PgPool;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

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
    rls_enforced: bool,
    push_config: PushConfigAdvertisement,
    transport_options: Option<TransportOptions>,
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
            rls_enforced: false,
            push_config: PushConfigAdvertisement::default(),
            transport_options: None,
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

    /// Enables community tenant isolation while retaining community routes.
    /// Production configuration cannot disable this mode.
    pub fn rls_enforced(mut self, enabled: bool) -> Self {
        self.rls_enforced = enabled;
        self
    }

    /// Advertises hosted push capability to enrolled devices.
    pub fn push_config(mut self, advertisement: PushConfigAdvertisement) -> Self {
        self.push_config = advertisement;
        self
    }

    /// Overrides the storage maintenance cadence for this server assembly.
    pub fn storage_cleanup_interval(mut self, interval: Duration) -> Self {
        let mut options = self.transport_options.unwrap_or_default();
        options.storage_cleanup_interval = interval;
        self.transport_options = Some(options);
        self
    }

    /// Initializes health/storage dependencies and assembles routes without
    /// spawning maintenance workers. Call [`Server::start_maintenance`] after
    /// the process has completed its startup sequencing.
    pub async fn build(self) -> Result<Server, Box<dyn std::error::Error + Send + Sync>> {
        let rls_enforced = self.rls_enforced || self.row_security;
        if rls_enforced && self.maintenance_db.is_none() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "RLS mode requires an explicit maintenance database",
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
        if rls_enforced {
            let maintenance_db = self.maintenance_db.as_ref().expect("checked above");
            rls::validate_live_ticket_runtime_capabilities(&self.runtime_db).await?;
            rls::validate_maintenance_role(maintenance_db).await?;
            if self.row_security {
                rls::validate_admission_runtime_privileges(&self.runtime_db).await?;
            } else {
                rls::validate_runtime_role(&self.runtime_db, true).await?;
            }
        }
        let maintenance_db = self
            .maintenance_db
            .unwrap_or_else(|| self.runtime_db.clone());
        let options =
            assembly_transport_options(self.config.replay_retention, self.transport_options);
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
                rls_enforced,
            },
        )
        .map_err(|message| std::io::Error::new(std::io::ErrorKind::InvalidInput, message))?;
        maintenance.db = maintenance_db;
        let router = Router::new()
            .route("/healthz", get(health::liveness))
            .route("/readyz", get(health::readiness))
            .route("/__release", get(health::release))
            .with_state(health)
            .merge(api)
            // Axum's default is 2 MiB; declare it at the server boundary so JSON
            // extractors retain that limit while attachment handlers keep streaming.
            .layer(axum::extract::DefaultBodyLimit::max(2 * 1024 * 1024));
        Ok(Server {
            router,
            maintenance: Some(maintenance),
            maintenance_started: AtomicBool::new(false),
        })
    }
}

fn assembly_transport_options(
    replay_retention: Duration,
    options: Option<TransportOptions>,
) -> TransportOptions {
    let mut options = options.unwrap_or_default();
    options.replay_retention = replay_retention;
    options
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
    let api = api::router_with_initialized_storage(state.database(), state.storage());
    Router::new()
        .route("/healthz", get(health::liveness))
        .route("/readyz", get(health::readiness))
        .route("/__release", get(health::release))
        .with_state(state)
        .merge(api)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn assembly_transport_options_preserve_replay_retention_and_allow_cleanup_override() {
        let replay_retention = Duration::from_secs(86_400);
        let cleanup_interval = Duration::from_millis(200);

        let options = assembly_transport_options(
            replay_retention,
            Some(TransportOptions {
                replay_retention: Duration::from_secs(1),
                storage_cleanup_interval: cleanup_interval,
                ..TransportOptions::default()
            }),
        );

        assert_eq!(options.replay_retention, replay_retention);
        assert_eq!(options.storage_cleanup_interval, cleanup_interval);
        assert_eq!(
            assembly_transport_options(replay_retention, None).storage_cleanup_interval,
            TransportOptions::default().storage_cleanup_interval
        );
    }
}
