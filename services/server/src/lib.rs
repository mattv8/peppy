pub mod api;
pub mod config;
pub mod health;
pub mod scope;
pub mod storage;
pub mod web_client;

pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!();

use axum::{Router, routing::get};
use health::HealthState;
use sqlx::PgPool;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::{
    api::{AccessPolicy, CommunityAccessPolicy, TransportOptions},
    config::Config,
};
use std::sync::Arc;

/// Explicit server assembly. Unlike the legacy API router helpers, this never
/// reads process environment or starts background work while building routes.
pub struct ServerBuilder {
    config: Config,
    runtime_db: PgPool,
    maintenance_db: Option<PgPool>,
    access_policy: Arc<dyn AccessPolicy>,
    row_security: bool,
}

impl ServerBuilder {
    pub fn new(config: Config, runtime_db: PgPool) -> Self {
        Self {
            config,
            runtime_db,
            maintenance_db: None,
            access_policy: Arc::new(CommunityAccessPolicy),
            row_security: false,
        }
    }

    pub fn maintenance_database(mut self, database: PgPool) -> Self {
        self.maintenance_db = Some(database);
        self
    }

    pub fn access_policy(mut self, policy: Arc<dyn AccessPolicy>) -> Self {
        self.access_policy = policy;
        self
    }

    /// Enables the hosted resolver path for a runtime role constrained by RLS.
    /// Community deployments retain direct credential lookups by default.
    pub fn row_security(mut self, enabled: bool) -> Self {
        self.row_security = enabled;
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
            self.access_policy,
            self.row_security,
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
