use std::sync::Arc;

use axum::{Json, extract::State, http::StatusCode};
use futures_util::StreamExt;
use serde::Serialize;
use sqlx::{PgPool, postgres::PgPoolOptions};
use time::OffsetDateTime;

use crate::{config::Config, storage::Storage};

#[derive(Clone)]
pub struct HealthState(Arc<Dependencies>);

struct Dependencies {
    database: PgPool,
    storage: Option<(Storage, std::time::Duration)>,
    timeout: std::time::Duration,
    release_identity: String,
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
            Some(s3) => Some((
                tokio::time::timeout(s3.readiness_timeout, Storage::initialize(s3)).await??,
                s3.readiness_timeout,
            )),
            None => None,
        };
        Ok(Self(Arc::new(Dependencies {
            database,
            storage,
            timeout: std::time::Duration::from_secs(2),
            release_identity: config.release_identity.clone(),
        })))
    }

    pub fn database(&self) -> PgPool {
        self.0.database.clone()
    }

    pub(crate) fn storage(&self) -> Option<Storage> {
        self.0.storage.as_ref().map(|(storage, _)| storage.clone())
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
    let storage = Storage::new(s3);
    let key = format!(
        "peppy-readiness-check-{}.txt",
        OffsetDateTime::now_utc().unix_timestamp_nanos()
    );
    if !s3.precreated_bucket {
        storage.ensure_bucket().await?;
    } else {
        storage.probe().await?;
    }
    let payload = b"peppy-private-s3-contract";
    storage.put_bytes(&key, payload.to_vec()).await?;
    let mut stream = storage.get(&key).await?;
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
    let unsigned_status = storage.unsigned_get_status(&key).await?;
    if unsigned_status != StatusCode::FORBIDDEN && unsigned_status != StatusCode::UNAUTHORIZED {
        return Err(format!(
            "unsigned private S3 GET returned {unsigned_status}, expected 401 or 403"
        )
        .into());
    }
    storage.delete(&key).await?;
    Ok(())
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
    if let Some((storage, timeout)) = &dependencies.storage {
        tokio::time::timeout(*timeout, storage.probe()).await??;
    }
    Ok(())
}
