//! Best-effort, vault-scoped device presence storage.

use peppy_hosted_api::{DevicePresence, PresenceState};
use sqlx::{Postgres, Row, Transaction};
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};
use uuid::Uuid;

pub const LEASE_SECONDS: i64 = 60;
pub const TOUCH_SECONDS: i64 = 30;
const MAX_SOCKETS_PER_DEVICE: i64 = 32;
const UNAVAILABLE_WARN_INTERVAL: Duration = Duration::from_secs(30);
static UNAVAILABLE_WARNINGS: OnceLock<Mutex<HashMap<&'static str, Instant>>> = OnceLock::new();

fn warn_unavailable(operation: &'static str, error: &sqlx::Error) {
    let now = Instant::now();
    let warnings = UNAVAILABLE_WARNINGS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut warnings = warnings
        .lock()
        .expect("presence warning cache lock poisoned");
    if warnings
        .get(operation)
        .is_some_and(|last| now.duration_since(*last) < UNAVAILABLE_WARN_INTERVAL)
    {
        return;
    }
    warnings.insert(operation, now);
    tracing::warn!(%error, operation, "presence unavailable");
}

/// Records an authenticated HTTP observation without affecting its operation.
pub async fn touch_best_effort(db: &sqlx::PgPool, vault_id: Uuid, device_id: Uuid) {
    let result = async {
        let mut tx = crate::scope::begin(db, vault_id).await?;
        touch(&mut tx, vault_id, device_id).await?;
        tx.commit().await
    }
    .await;
    if let Err(error) = result {
        warn_unavailable("touch", &error);
    }
}

pub async fn open(
    tx: &mut Transaction<'_, Postgres>,
    vault_id: Uuid,
    device_id: Uuid,
    connection_id: Uuid,
) -> Result<(), sqlx::Error> {
    let active: Option<i32> = sqlx::query_scalar(
        "SELECT 1 FROM devices WHERE vault_id=$1 AND device_id=$2 AND revoked_at IS NULL FOR UPDATE",
    )
    .bind(vault_id)
    .bind(device_id)
    .fetch_optional(&mut **tx)
    .await?;
    if active.is_none() {
        return Ok(());
    }
    sqlx::query("DELETE FROM device_sockets WHERE vault_id=$1 AND ctid IN (SELECT ctid FROM device_sockets WHERE vault_id=$1 AND lease_until < now() LIMIT 256)")
        .bind(vault_id)
        .execute(&mut **tx)
        .await?;
    let sockets: i64 = sqlx::query_scalar("SELECT count(*) FROM device_sockets WHERE vault_id=$1 AND device_id=$2 AND lease_until > now()")
        .bind(vault_id).bind(device_id).fetch_one(&mut **tx).await?;
    if sockets >= MAX_SOCKETS_PER_DEVICE {
        return Ok(());
    }
    // Hello establishes an owned socket row, but not a live lease: only a Pong
    // after a server control Ping can make the device connected.
    sqlx::query("INSERT INTO device_sockets(connection_id,vault_id,device_id,lease_until,responsive) VALUES($3,$1,$2,now() + make_interval(secs => $4),FALSE)")
        .bind(vault_id).bind(device_id).bind(connection_id).bind(LEASE_SECONDS).execute(&mut **tx).await?;
    Ok(())
}

pub async fn renew(
    tx: &mut Transaction<'_, Postgres>,
    vault_id: Uuid,
    device_id: Uuid,
    connection_id: Uuid,
) -> Result<bool, sqlx::Error> {
    let was_responsive: Option<bool> = sqlx::query_scalar("SELECT responsive FROM device_sockets WHERE connection_id=$3 AND vault_id=$1 AND device_id=$2 FOR UPDATE")
        .bind(vault_id).bind(device_id).bind(connection_id).fetch_optional(&mut **tx).await?;
    let changed = sqlx::query("UPDATE device_sockets SET lease_until=now() + make_interval(secs => $4),responsive=TRUE WHERE connection_id=$3 AND vault_id=$1 AND device_id=$2 AND EXISTS(SELECT 1 FROM devices WHERE vault_id=$1 AND device_id=$2 AND revoked_at IS NULL)")
        .bind(vault_id).bind(device_id).bind(connection_id).bind(LEASE_SECONDS).execute(&mut **tx).await?.rows_affected();
    if changed != 0 {
        touch(tx, vault_id, device_id).await?;
    }
    let other_responsive: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM device_sockets WHERE vault_id=$1 AND device_id=$2 AND connection_id <> $3 AND responsive AND lease_until > now())")
        .bind(vault_id).bind(device_id).bind(connection_id).fetch_one(&mut **tx).await?;
    Ok(changed != 0 && was_responsive == Some(false) && !other_responsive)
}

pub async fn close(
    db: &sqlx::PgPool,
    vault_id: Uuid,
    connection_id: Uuid,
) -> Result<bool, sqlx::Error> {
    let mut tx = crate::scope::begin(db, vault_id).await?;
    let removed: Option<(Uuid, bool)> = sqlx::query_as(
        "DELETE FROM device_sockets WHERE connection_id=$1 AND vault_id=$2 RETURNING device_id,responsive",
    )
    .bind(connection_id)
    .bind(vault_id)
    .fetch_optional(&mut *tx)
    .await?;
    let remaining = match removed.as_ref() {
        Some((device_id, _)) => sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM device_sockets WHERE vault_id=$1 AND device_id=$2 AND lease_until > now() AND responsive)")
            .bind(vault_id).bind(*device_id).fetch_one(&mut *tx).await?,
        None => false,
    };
    tx.commit().await?;
    Ok(removed.is_some_and(|(_, responsive)| responsive) && !remaining)
}

pub async fn revoke_best_effort(db: &sqlx::PgPool, vault_id: Uuid, device_id: Uuid) {
    let result = async {
        let mut tx = crate::scope::begin(db, vault_id).await?;
        sqlx::query("DELETE FROM device_sockets WHERE vault_id=$1 AND device_id=$2")
            .bind(vault_id)
            .bind(device_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await
    }
    .await;
    if let Err(error) = result {
        warn_unavailable("revoke_cleanup", &error);
    }
}

pub async fn lookup(
    tx: &mut Transaction<'_, Postgres>,
    vault_id: Uuid,
    device_id: Uuid,
    revoked: bool,
) -> Result<Option<DevicePresence>, sqlx::Error> {
    if revoked {
        return Ok(None);
    }
    let row = sqlx::query("SELECT CASE WHEN p.last_seen_at IS NULL THEN NULL ELSE GREATEST(0, EXTRACT(EPOCH FROM now() - p.last_seen_at))::bigint END age_seconds, EXISTS(SELECT 1 FROM device_sockets s WHERE s.vault_id=$1 AND s.device_id=$2 AND s.responsive AND s.lease_until > now()) connected FROM device_presence p WHERE p.vault_id=$1 AND p.device_id=$2")
        .bind(vault_id).bind(device_id).fetch_optional(&mut **tx).await?;
    Ok(row
        .and_then(|row| {
            row.get::<Option<i64>, _>("age_seconds")
                .map(|age_seconds| DevicePresence {
                    state: if row.get("connected") {
                        PresenceState::Connected
                    } else {
                        PresenceState::Seen
                    },
                    last_seen_age_seconds: Some(age_seconds as u64),
                })
        })
        .or(Some(DevicePresence {
            state: PresenceState::Never,
            last_seen_age_seconds: None,
        })))
}

/// Reads a roster projection in a separate scoped transaction so optional
/// hosted grants/schema cannot poison the caller's durable roster transaction.
pub async fn lookup_many_best_effort(
    db: &sqlx::PgPool,
    vault_id: Uuid,
    devices: &[(Uuid, bool)],
) -> HashMap<Uuid, DevicePresence> {
    let active: Vec<Uuid> = devices
        .iter()
        .filter_map(|(device_id, revoked)| (!revoked).then_some(*device_id))
        .collect();
    if active.is_empty() {
        return HashMap::new();
    }
    let result = async {
        let mut tx = crate::scope::begin(db, vault_id).await?;
        let rows = sqlx::query("SELECT requested.device_id, CASE WHEN p.last_seen_at IS NULL THEN NULL ELSE GREATEST(0, EXTRACT(EPOCH FROM now() - p.last_seen_at))::bigint END age_seconds, EXISTS(SELECT 1 FROM device_sockets s WHERE s.vault_id=$1 AND s.device_id=requested.device_id AND s.responsive AND s.lease_until > now()) connected FROM unnest($2::uuid[]) AS requested(device_id) LEFT JOIN device_presence p ON p.vault_id=$1 AND p.device_id=requested.device_id")
            .bind(vault_id).bind(&active).fetch_all(&mut *tx).await?;
        tx.commit().await?;
        Ok::<_, sqlx::Error>(rows)
    }
    .await;
    match result {
        Ok(rows) => rows
            .into_iter()
            .map(|row| {
                let age = row.get::<Option<i64>, _>("age_seconds");
                let presence = match age {
                    Some(age_seconds) => DevicePresence {
                        state: if row.get("connected") {
                            PresenceState::Connected
                        } else {
                            PresenceState::Seen
                        },
                        last_seen_age_seconds: Some(age_seconds as u64),
                    },
                    None => DevicePresence {
                        state: PresenceState::Never,
                        last_seen_age_seconds: None,
                    },
                };
                (row.get("device_id"), presence)
            })
            .collect(),
        Err(error) => {
            warn_unavailable("lookup", &error);
            HashMap::new()
        }
    }
}

async fn touch(
    tx: &mut Transaction<'_, Postgres>,
    vault_id: Uuid,
    device_id: Uuid,
) -> Result<(), sqlx::Error> {
    sqlx::query("INSERT INTO device_presence(vault_id,device_id,last_seen_at) VALUES($1,$2,now()) ON CONFLICT(vault_id,device_id) DO UPDATE SET last_seen_at=EXCLUDED.last_seen_at WHERE device_presence.last_seen_at < now() - make_interval(secs => $3)")
        .bind(vault_id).bind(device_id).bind(TOUCH_SECONDS)
        .execute(&mut **tx).await?;
    Ok(())
}
