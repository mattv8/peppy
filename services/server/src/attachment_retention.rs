//! Durable retention facts shared with the hosted policy adapter.
//!
//! This module intentionally exposes facts rather than a cleanup decision: the
//! private hosted policy remains the authority for entitlement and notice state.

use sqlx::{PgConnection, Row};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct RetentionStorageFacts {
    pub protected_bytes: i64,
    pub removed_bytes: i64,
    pub all_devices_ready: bool,
    pub cleanup_pending: bool,
}

/// Reads retention storage facts on the caller-owned connection.  No policy,
/// pool checkout, or storage I/O happens here, preserving hosted lock order.
pub async fn retention_storage_status_on_connection(
    connection: &mut PgConnection,
    vault_id: Uuid,
) -> Result<RetentionStorageFacts, sqlx::Error> {
    let row = sqlx::query(
        "SELECT \
         COALESCE((SELECT SUM(a.ciphertext_bytes) FROM attachments a WHERE a.vault_id=$1 AND (a.retention_class <> 'message_media' OR NOT EXISTS (SELECT 1 FROM attachment_retention_uses u WHERE u.vault_id=a.vault_id AND u.attachment_id=a.attachment_id AND u.kind='message_history') OR EXISTS (SELECT 1 FROM attachment_retention_uses u WHERE u.vault_id=a.vault_id AND u.attachment_id=a.attachment_id AND u.kind <> 'message_history' AND u.released_at IS NULL))),0)::bigint AS protected_bytes, \
         COALESCE((SELECT SUM(ciphertext_bytes) FROM attachment_removals WHERE vault_id=$1 AND purged_at IS NULL),0)::bigint AS removed_bytes, \
         NOT EXISTS (SELECT 1 FROM devices d LEFT JOIN attachment_retention_devices r ON r.vault_id=d.vault_id AND r.device_id=d.device_id WHERE d.vault_id=$1 AND d.revoked_at IS NULL AND (r.cleanup_ready IS DISTINCT FROM TRUE OR r.reconciliation_needed IS DISTINCT FROM FALSE)) AS all_devices_ready, \
         EXISTS (SELECT 1 FROM attachment_removals WHERE vault_id=$1 AND purged_at IS NULL) AS cleanup_pending",
    )
    .bind(vault_id)
    .fetch_one(connection)
    .await?;
    Ok(RetentionStorageFacts {
        protected_bytes: row.get("protected_bytes"),
        removed_bytes: row.get("removed_bytes"),
        all_devices_ready: row.get("all_devices_ready"),
        cleanup_pending: row.get("cleanup_pending"),
    })
}

/// Invalidates a device's cleanup eligibility on the same connection that is
/// about to process a legacy mutation. The caller owns transaction boundaries
/// so this update can be atomic with that mutation.
pub async fn withdraw_retention_readiness_on_connection(
    connection: &mut PgConnection,
    vault_id: Uuid,
    device_id: Uuid,
) -> Result<(), sqlx::Error> {
    sqlx::query("INSERT INTO attachment_retention_devices(vault_id,device_id,cleanup_ready,reconciliation_needed) VALUES($1,$2,FALSE,TRUE) ON CONFLICT(vault_id,device_id) DO UPDATE SET cleanup_ready=FALSE,reconciliation_needed=TRUE,updated_at=now()")
        .bind(vault_id)
        .bind(device_id)
        .execute(connection)
        .await?;
    Ok(())
}

/// Atomically withdraws readiness when a mutating request does not advertise
/// the current retention protocol. Current headers preserve state; only the
/// explicit reconciliation endpoint can grant readiness.
pub async fn withdraw_legacy_retention_readiness_on_connection(
    connection: &mut PgConnection,
    headers: &axum::http::HeaderMap,
    vault_id: Uuid,
    device_id: Uuid,
) -> Result<(), sqlx::Error> {
    let protocol = headers
        .get(peppy_hosted_api::attachments::ATTACHMENT_RETENTION_PROTOCOL_HEADER)
        .and_then(|value| value.to_str().ok());
    let current = peppy_hosted_api::attachments::retention_protocol_is_current(protocol);
    if current {
        return Ok(());
    }
    withdraw_retention_readiness_on_connection(connection, vault_id, device_id).await
}

/// Recreates bounded missing cleanup queue references without changing debt.
pub async fn repair_retention_cleanup_queue(
    pool: &sqlx::PgPool,
    limit: i64,
) -> Result<u64, sqlx::Error> {
    Ok(sqlx::query("INSERT INTO storage_deletions(object_key,vault_id,reason) SELECT object_key,vault_id,'retention_cleanup' FROM attachment_removals WHERE purged_at IS NULL AND NOT EXISTS (SELECT 1 FROM storage_deletions d WHERE d.object_key=attachment_removals.object_key) ORDER BY removed_at LIMIT $1 ON CONFLICT(object_key) DO NOTHING")
        .bind(limit.max(1))
        .execute(pool)
        .await?
        .rows_affected())
}
