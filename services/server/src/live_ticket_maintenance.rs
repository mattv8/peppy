use sqlx::PgPool;

pub(crate) const LIVE_TICKET_REAP_BATCH: i64 = 1_000;

/// Reclaims one bounded batch from each live-ticket retention category.
pub(crate) async fn reap_live_tickets(db: &PgPool, batch: i64) -> Result<u64, sqlx::Error> {
    let terminalized = sqlx::query("UPDATE live_ticket_sessions SET state='terminal', closed_at=now(), lease_until=NULL WHERE ctid IN (SELECT ctid FROM live_ticket_sessions WHERE state='open' AND lease_until<=now() ORDER BY lease_until LIMIT $1 FOR UPDATE SKIP LOCKED)")
        .bind(batch)
        .execute(db)
        .await?
        .rows_affected();
    let tickets = sqlx::query("DELETE FROM live_tickets WHERE ctid IN (SELECT ctid FROM live_tickets WHERE expires_at<=now() OR consumed_at IS NOT NULL ORDER BY expires_at LIMIT $1 FOR UPDATE SKIP LOCKED)")
        .bind(batch)
        .execute(db)
        .await?
        .rows_affected();
    let tombstones = sqlx::query("DELETE FROM live_ticket_sessions WHERE ctid IN (SELECT ctid FROM live_ticket_sessions WHERE state IN ('closed','terminal') AND closed_at<now()-interval '5 minutes' ORDER BY closed_at LIMIT $1 FOR UPDATE SKIP LOCKED)")
        .bind(batch)
        .execute(db)
        .await?
        .rows_affected();
    Ok(terminalized + tickets + tombstones)
}
