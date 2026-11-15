use std::{
    sync::{
        Arc, Weak,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};

use axum::{Json, extract::State, http::StatusCode};
use futures_util::StreamExt;
use serde::Serialize;
use sqlx::{PgPool, postgres::PgPoolOptions};
use time::OffsetDateTime;

use crate::{
    config::Config,
    storage::{DisposableObject, Storage},
};

const STORAGE_PROBE_INTERVAL: Duration = Duration::from_secs(5);
const STORAGE_PROBE_MAX_AGE: Duration = Duration::from_secs(15);

#[derive(Clone)]
pub struct HealthState(Arc<Dependencies>);

struct Dependencies {
    database: PgPool,
    storage: Option<Arc<StorageReadiness>>,
    timeout: Duration,
    release_identity: String,
}

struct StorageReadiness {
    storage: Storage,
    timeout: Duration,
    started_at: tokio::time::Instant,
    max_age: Duration,
    latest_probe_succeeded: AtomicBool,
    last_success_millis: AtomicU64,
}

impl StorageReadiness {
    fn new(storage: Storage, timeout: Duration) -> Self {
        Self::with_timing(storage, timeout, STORAGE_PROBE_MAX_AGE)
    }

    fn with_timing(storage: Storage, timeout: Duration, max_age: Duration) -> Self {
        Self {
            storage,
            timeout,
            started_at: tokio::time::Instant::now(),
            max_age,
            latest_probe_succeeded: AtomicBool::new(true),
            last_success_millis: AtomicU64::new(0),
        }
    }

    fn is_ready(&self) -> bool {
        self.is_ready_at(self.started_at.elapsed())
    }

    /// A failed probe alone does not fail readiness; only a last success older
    /// than `max_age` does, so one slow probe cannot eject a healthy server.
    fn is_ready_at(&self, elapsed: Duration) -> bool {
        elapsed.saturating_sub(Duration::from_millis(
            self.last_success_millis.load(Ordering::Relaxed),
        )) <= self.max_age
    }

    fn record_probe(&self, succeeded: bool) -> bool {
        if succeeded {
            self.last_success_millis.store(
                self.started_at.elapsed().as_millis() as u64,
                Ordering::Relaxed,
            );
        }
        self.latest_probe_succeeded
            .swap(succeeded, Ordering::AcqRel)
            != succeeded
    }
}

fn spawn_storage_probe(storage: Weak<StorageReadiness>) {
    spawn_storage_probe_at_interval(storage, STORAGE_PROBE_INTERVAL);
}

fn spawn_storage_probe_at_interval(storage: Weak<StorageReadiness>, interval_duration: Duration) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval_at(
            tokio::time::Instant::now() + interval_duration,
            interval_duration,
        );
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            interval.tick().await;
            let Some(storage) = storage.upgrade() else {
                return;
            };
            match tokio::time::timeout(storage.timeout, storage.storage.probe()).await {
                Ok(Ok(())) => {
                    if storage.record_probe(true) {
                        tracing::info!("storage readiness probe recovered");
                    }
                }
                Ok(Err(error)) => {
                    if storage.record_probe(false) {
                        tracing::warn!(%error, "storage readiness probe failed");
                    }
                }
                Err(_) => {
                    if storage.record_probe(false) {
                        tracing::warn!("storage readiness probe timed out");
                    }
                }
            }
        }
    });
}

impl HealthState {
    pub async fn connect(
        config: &Config,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let database = PgPoolOptions::new()
            .max_connections(5)
            .acquire_timeout(std::time::Duration::from_secs(2))
            .connect(&config.database_url)
            .await?;
        Self::from_database(config, database).await
    }

    /// Builds health dependencies around an already-owned runtime pool.
    /// Storage initialization happens here exactly once for the server instance.
    pub async fn from_database(
        config: &Config,
        database: PgPool,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let storage = match config.s3.as_ref() {
            Some(s3) => Some(Arc::new(StorageReadiness::new(
                Storage::initialize(s3).await?,
                s3.readiness_timeout,
            ))),
            None => None,
        };
        let dependencies = Arc::new(Dependencies {
            database,
            storage,
            timeout: Duration::from_secs(2),
            release_identity: config.release_identity.clone(),
        });
        if let Some(storage) = &dependencies.storage {
            spawn_storage_probe(Arc::downgrade(storage));
        }
        Ok(Self(dependencies))
    }

    pub fn database(&self) -> PgPool {
        self.0.database.clone()
    }

    pub(crate) fn storage(&self) -> Option<Storage> {
        self.0
            .storage
            .as_ref()
            .map(|storage| storage.storage.clone())
    }
}

#[derive(Serialize)]
pub struct ReleaseIdentity {
    pub revision: String,
}

/// Deployment identity is intentionally separate from readiness so existing
/// status-only probes remain compatible.
pub async fn release(State(state): State<HealthState>) -> Json<ReleaseIdentity> {
    Json(ReleaseIdentity {
        revision: state.0.release_identity.clone(),
    })
}

pub async fn liveness() -> StatusCode {
    StatusCode::OK
}

/// Operator check: provisions the bucket, proves a signed round trip through
/// the same hardened SigV4 adapter the API uses, and proves anonymous reads fail.
pub async fn storage_contract_check(
    config: &Config,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let s3 = config.s3.as_ref().ok_or("S3 configuration is required")?;
    let storage = Storage::initialize(s3).await?;
    let key = format!(
        "peppy-readiness-check-{}.txt",
        OffsetDateTime::now_utc().unix_timestamp_nanos()
    );
    let payload = b"peppy-private-s3-contract";
    let object = DisposableObject::new(storage.clone(), key);
    let result: Result<(), Box<dyn std::error::Error + Send + Sync>> = async {
        object.put_bytes(payload.to_vec()).await?;
        let mut stream = storage.get(object.key()).await?;
        let mut downloaded = Vec::with_capacity(payload.len());
        while let Some(chunk) = stream.next().await {
            downloaded.extend_from_slice(&chunk?);
            if downloaded.len() > payload.len() {
                break;
            }
        }
        if downloaded != payload {
            return Err("signed S3 round trip returned an unexpected body".into());
        }
        let unsigned_status = storage.unsigned_get_status(object.key()).await?;
        if unsigned_status != StatusCode::FORBIDDEN && unsigned_status != StatusCode::UNAUTHORIZED {
            return Err(format!(
                "unsigned private S3 GET returned {unsigned_status}, expected 401 or 403"
            )
            .into());
        }
        Ok(())
    }
    .await;
    let cleanup = object.cleanup().await;
    match (result, cleanup) {
        (Err(error), Err(cleanup_error)) => {
            tracing::warn!(%error, %cleanup_error, "storage contract check cleanup failed");
            Err(error)
        }
        (Err(error), Ok(())) => Err(error),
        (Ok(()), Err(error)) => Err(error.into()),
        (Ok(()), Ok(())) => Ok(()),
    }
}

pub async fn readiness(State(state): State<HealthState>) -> StatusCode {
    match dependencies_ready(&state.0).await {
        Ok(()) => StatusCode::OK,
        Err(error) => {
            tracing::warn!(error = %error, "readiness dependency check failed");
            StatusCode::SERVICE_UNAVAILABLE
        }
    }
}

async fn dependencies_ready(
    dependencies: &Dependencies,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    tokio::time::timeout(
        dependencies.timeout,
        sqlx::query_scalar::<_, i32>("SELECT 1").fetch_one(&dependencies.database),
    )
    .await??;
    tokio::time::timeout(
        dependencies.timeout,
        sqlx::query_scalar::<_, i32>("SELECT 1 FROM peppy_schema_marker WHERE version = 1")
            .fetch_one(&dependencies.database),
    )
    .await??;
    storage_ready(dependencies.storage.as_ref())?;
    Ok(())
}

fn storage_ready(
    storage: Option<&Arc<StorageReadiness>>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    if let Some(storage) = storage
        && !storage.is_ready()
    {
        return Err("storage readiness probe has not succeeded recently".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{sync::Arc, time::Duration};

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use url::Url;

    use super::*;
    use crate::config::S3Config;

    fn storage(endpoint: &str) -> Storage {
        Storage::new(&S3Config {
            endpoint: Url::parse(endpoint).expect("endpoint"),
            bucket: "bucket".into(),
            access_key: "access".into(),
            secret_key: "secret".into(),
            signing_region: "us-west-000".into(),
            precreated_bucket: true,
            readiness_timeout: Duration::from_secs(1),
        })
    }

    #[tokio::test]
    async fn seeded_storage_readiness_answers_without_a_network_request() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind local listener");
        let endpoint = format!(
            "http://{}",
            listener.local_addr().expect("listener address")
        );
        let accepted_connections = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let listener_connections = Arc::clone(&accepted_connections);
        let server = tokio::spawn(async move {
            while listener.accept().await.is_ok() {
                listener_connections.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
        });
        let readiness = Arc::new(StorageReadiness::new(
            storage(&endpoint),
            Duration::from_secs(1),
        ));

        assert!(storage_ready(Some(&readiness)).is_ok());
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert_eq!(
            accepted_connections.load(std::sync::atomic::Ordering::Relaxed),
            0,
            "readiness must not probe storage on the request path"
        );
        server.abort();
    }

    #[tokio::test]
    async fn storage_probes_failing_past_the_max_age_make_readiness_fail() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind local listener");
        let endpoint = format!(
            "http://{}",
            listener.local_addr().expect("listener address")
        );
        let rejected_probes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let server_rejected_probes = Arc::clone(&rejected_probes);
        let server = tokio::spawn(async move {
            loop {
                let (mut socket, _) = listener.accept().await.expect("accept probe");
                let mut request = [0_u8; 1];
                socket.read_exact(&mut request).await.expect("read probe");
                socket
                    .write_all(b"HTTP/1.1 503 Service Unavailable\r\ncontent-length: 0\r\nconnection: close\r\n\r\n")
                    .await
                    .expect("write probe response");
                server_rejected_probes.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
        });
        let readiness = Arc::new(StorageReadiness::with_timing(
            storage(&endpoint),
            Duration::from_secs(1),
            Duration::from_millis(50),
        ));
        spawn_storage_probe_at_interval(Arc::downgrade(&readiness), Duration::from_millis(1));

        tokio::time::timeout(Duration::from_secs(1), async {
            while rejected_probes.load(std::sync::atomic::Ordering::Relaxed) == 0 {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            while readiness.is_ready() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("rejected probes must not refresh readiness");

        assert!(!readiness.is_ready());
        server.abort();
    }

    #[test]
    fn failed_storage_probe_keeps_readiness_while_the_last_success_is_fresh() {
        let max_age = Duration::from_secs(15);
        let readiness = StorageReadiness::with_timing(
            storage("http://127.0.0.1:9"),
            Duration::from_secs(1),
            max_age,
        );

        readiness.record_probe(false);

        assert!(readiness.is_ready_at(max_age));
        assert!(!readiness.is_ready_at(max_age + Duration::from_millis(1)));
    }

    #[test]
    fn successful_storage_probe_after_a_failure_restores_readiness() {
        let max_age = Duration::from_secs(1);
        let since_start = Duration::from_millis(20);
        let readiness = StorageReadiness::with_timing(
            storage("http://127.0.0.1:9"),
            Duration::from_secs(1),
            max_age,
        );
        std::thread::sleep(since_start);
        readiness.record_probe(false);
        assert!(!readiness.is_ready_at(max_age + since_start));

        readiness.record_probe(true);

        assert!(readiness.is_ready_at(max_age + since_start));
    }

    #[test]
    fn stale_storage_success_fails_readiness_without_a_completed_probe() {
        let readiness = StorageReadiness::with_timing(
            storage("http://127.0.0.1:9"),
            Duration::from_secs(1),
            Duration::from_secs(1),
        );

        assert!(!readiness.is_ready_at(Duration::from_secs(2)));
    }
}
