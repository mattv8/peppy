//! Core-owned retained downloaded-ciphertext cache policy.
//!
//! LRU metadata is durable so reopening a store preserves its retained-download
//! inventory. Renderer/native leases are intentionally process-local: a crashed
//! host must not pin ciphertext forever after reopening the store.

use crate::{AttachmentId, AttachmentState, Client, Error, Store, media};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use std::collections::HashMap;

/// Maximum retained verified downloaded ciphertext, independent of plaintext limits.
pub const RETAINED_DOWNLOAD_CIPHERTEXT_BUDGET_BYTES: u64 = 128 * 1024 * 1024;

const EVICTABLE_AVAILABLE_PREDICATE: &str = r#"
    a.state = 'available'
    AND NOT EXISTS (SELECT 1 FROM attachment_demand d WHERE d.attachment_id = a.attachment_id)
    AND NOT EXISTS (SELECT 1 FROM message_media_meta m WHERE m.thumbnail_attachment_id = a.attachment_id)
    AND NOT EXISTS (
        SELECT 1 FROM mms_acquisition_parts p
        WHERE p.attachment_id = a.attachment_id OR p.thumbnail_attachment_id = a.attachment_id
    )
    AND NOT EXISTS (SELECT 1 FROM compose_drafts d WHERE instr(d.attachment_ids, a.attachment_id) > 0)
    AND NOT EXISTS (
        SELECT 1 FROM command_ledger l
        JOIN commands c ON c.command_id = l.command_id
        JOIN message_attachments m ON m.message_id = c.message_id
        LEFT JOIN attempts attempt ON attempt.command_id = l.command_id
        WHERE m.attachment_id = a.attachment_id
          AND l.retired = 0
          AND l.historical = 0
          AND (attempt.command_id IS NULL OR attempt.state IN ('attempt_recorded', 'submitted_to_os', 'outcome_unknown'))
    )
    AND NOT (
        a.plaintext_bytes <= 2097152
        AND NOT EXISTS (SELECT 1 FROM message_media_meta m WHERE m.attachment_id = a.attachment_id)
    )
    AND NOT EXISTS (SELECT 1 FROM contact_photo_received p WHERE p.attachment_id = a.attachment_id)
    AND NOT EXISTS (SELECT 1 FROM contact_photo_attachments p WHERE p.attachment_id = a.attachment_id)
    AND NOT EXISTS (
        SELECT 1 FROM contact_photo_references r
        WHERE r.attachment_id = a.attachment_id
          AND (r.retain_until IS NULL OR r.retain_until > CAST(strftime('%s', 'now') AS INTEGER))
    )
    AND NOT EXISTS (
        SELECT 1 FROM contact_photo_registrations r
        WHERE r.attachment_id = a.attachment_id AND r.acknowledged = 0
    )
    AND NOT EXISTS (
        SELECT 1 FROM outbox o
        WHERE o.state = 'unsealed'
          AND (instr(CAST(o.plain AS TEXT), a.attachment_id) > 0
               OR EXISTS (
                   SELECT 1 FROM contact_photo_aliases alias
                   WHERE alias.new_id = a.attachment_id
                     AND instr(CAST(o.plain AS TEXT), alias.old_id) > 0
               ))
    )
"#;

/// Per-open-store lease state. This is deliberately not persisted.
#[derive(Default)]
pub(crate) struct MediaCacheState {
    leases: HashMap<AttachmentId, u32>,
}

impl Client {
    /// Records one process-local user of locally readable ciphertext.
    ///
    /// Every successful retain must be balanced with `release_attachment_ciphertext`.
    pub fn retain_attachment_ciphertext(&self, attachment_id: AttachmentId) -> Result<(), Error> {
        let mut store = self.lock()?;
        let state = locally_readable_state(&store, attachment_id)?;
        if state == AttachmentState::Available {
            record_access(&mut store, attachment_id)?;
            store.conn.execute(
                "DELETE FROM attachment_demand WHERE attachment_id=?",
                [attachment_id.to_string()],
            )?;
        }
        let count = store.media_cache.leases.entry(attachment_id).or_default();
        *count = count
            .checked_add(1)
            .ok_or(Error::InvalidRequest("media lease overflow"))?;
        Ok(())
    }

    /// Records access without taking a lease.
    ///
    /// Available downloads update durable LRU metadata. Locally prepared source ciphertext is
    /// already protected from cache eviction, so this is a successful no-op for it.
    pub fn touch_attachment_ciphertext(&self, attachment_id: AttachmentId) -> Result<(), Error> {
        let mut store = self.lock()?;
        if locally_readable_state(&store, attachment_id)? == AttachmentState::Available {
            record_access(&mut store, attachment_id)?;
        }
        Ok(())
    }

    /// Releases one process-local ciphertext lease and trims budget pressure.
    ///
    /// Returns `true` only when this Store held and released a lease for `attachment_id`.
    pub fn release_attachment_ciphertext(
        &self,
        attachment_id: AttachmentId,
    ) -> Result<bool, Error> {
        let mut store = self.lock()?;
        let released = match store.media_cache.leases.get_mut(&attachment_id) {
            Some(count) if *count > 1 => {
                *count -= 1;
                true
            }
            Some(_) => {
                store.media_cache.leases.remove(&attachment_id);
                true
            }
            None => false,
        };
        trim_store(&mut store, RETAINED_DOWNLOAD_CIPHERTEXT_BUDGET_BYTES)?;
        Ok(released)
    }
}

/// Creates durable cache inventory and LRU metadata. Safe on fresh and upgraded stores.
pub(crate) fn initialize(conn: &Connection) -> Result<(), Error> {
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS media_cache_entries(
            attachment_id TEXT PRIMARY KEY,
            last_access INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS media_cache_entries_lru
            ON media_cache_entries(last_access, attachment_id);
        "#,
    )?;
    conn.execute(
        r#"
        INSERT OR IGNORE INTO media_cache_entries(attachment_id, last_access)
        SELECT attachment_id, rowid
        FROM attachments
        WHERE state = 'available'
        "#,
        [],
    )?;
    // Demand left behind on an already-available original (for example by pre-fix builds) is
    // fulfilled. Pending originals keep their demand.
    conn.execute(
        r#"
        DELETE FROM attachment_demand
        WHERE attachment_id IN (SELECT attachment_id FROM attachments WHERE state = 'available')
        "#,
        [],
    )?;
    Ok(())
}

/// Called after verified downloaded ciphertext becomes available, while the Store lock is held.
pub(crate) fn record_installed_attachment(
    store: &mut Store,
    attachment_id: AttachmentId,
) -> Result<(), Error> {
    record_installed_access(store, attachment_id)?;
    trim_store(store, RETAINED_DOWNLOAD_CIPHERTEXT_BUDGET_BYTES)
}

fn record_installed_access(store: &mut Store, attachment_id: AttachmentId) -> Result<(), Error> {
    let tx = store
        .conn
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    let next_access: i64 = tx.query_row(
        "SELECT COALESCE(MAX(last_access),0)+1 FROM media_cache_entries",
        [],
        |row| row.get(0),
    )?;
    tx.execute(
        r#"
        INSERT INTO media_cache_entries(attachment_id, last_access)
        VALUES(?, ?)
        ON CONFLICT(attachment_id) DO UPDATE SET last_access = excluded.last_access
        "#,
        params![attachment_id.to_string(), next_access],
    )?;
    tx.execute(
        "DELETE FROM attachment_demand WHERE attachment_id=?",
        [attachment_id.to_string()],
    )?;
    tx.commit()?;
    Ok(())
}

/// Preserves the existing explicit eviction entry point while applying cache eligibility rules.
pub(crate) fn evict_attachment_ciphertext(
    client: &Client,
    attachment_id: AttachmentId,
) -> Result<bool, Error> {
    let mut store = client.lock()?;
    evict_store(&mut store, attachment_id)
}

/// Uses a small policy only from in-crate tests; production has no budget override.
#[cfg(test)]
pub(crate) fn trim_to_budget_for_test(client: &Client, budget: u64) -> Result<(), Error> {
    let mut store = client.lock()?;
    trim_store(&mut store, budget)
}

fn record_access(store: &mut Store, attachment_id: AttachmentId) -> Result<(), Error> {
    let available: bool = store.conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM attachments WHERE attachment_id=? AND state='available')",
        [attachment_id.to_string()],
        |row| row.get(0),
    )?;
    if !available {
        return Err(Error::InvalidMedia);
    }
    let tx = store
        .conn
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    let next_access: i64 = tx.query_row(
        "SELECT COALESCE(MAX(last_access),0)+1 FROM media_cache_entries",
        [],
        |row| row.get(0),
    )?;
    tx.execute(
        r#"
        INSERT INTO media_cache_entries(attachment_id, last_access)
        VALUES(?, ?)
        ON CONFLICT(attachment_id) DO UPDATE SET last_access = excluded.last_access
        "#,
        params![attachment_id.to_string(), next_access],
    )?;
    tx.commit()?;
    Ok(())
}

fn locally_readable_state(
    store: &Store,
    attachment_id: AttachmentId,
) -> Result<AttachmentState, Error> {
    let state = crate::attachment_row(&store.conn, attachment_id)?
        .ok_or(Error::NotFound)?
        .state;
    if state.is_local() {
        Ok(state)
    } else {
        Err(Error::InvalidMedia)
    }
}

fn trim_store(store: &mut Store, budget: u64) -> Result<(), Error> {
    while retained_bytes(store)? > budget {
        let Some(candidate) = least_recent_evictable(store)? else {
            break;
        };
        if !evict_store(store, candidate)? {
            break;
        }
    }
    Ok(())
}

fn retained_bytes(store: &Store) -> Result<u64, Error> {
    let eligible_bytes: i64 = store.conn.query_row(
        &format!(
            r#"
            SELECT COALESCE(SUM(a.ciphertext_bytes), 0)
            FROM attachments a
            JOIN media_cache_entries e ON e.attachment_id = a.attachment_id
            WHERE {EVICTABLE_AVAILABLE_PREDICATE}
            "#
        ),
        [],
        |row| row.get(0),
    )?;
    let mut eligible_bytes = u64::try_from(eligible_bytes).map_err(|_| Error::Database)?;
    for attachment_id in store.media_cache.leases.keys() {
        let leased_bytes: Option<i64> = store
            .conn
            .query_row(
                &format!(
                    r#"
                SELECT a.ciphertext_bytes
                FROM attachments a
                JOIN media_cache_entries e ON e.attachment_id = a.attachment_id
                WHERE a.attachment_id = ? AND {EVICTABLE_AVAILABLE_PREDICATE}
                "#
                ),
                [attachment_id.to_string()],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(leased_bytes) = leased_bytes {
            eligible_bytes = eligible_bytes
                .checked_sub(u64::try_from(leased_bytes).map_err(|_| Error::Database)?)
                .ok_or(Error::Database)?;
        }
    }
    Ok(eligible_bytes)
}

fn least_recent_evictable(store: &Store) -> Result<Option<AttachmentId>, Error> {
    let mut statement = store.conn.prepare(&format!(
        r#"
        SELECT a.attachment_id
        FROM attachments a
        JOIN media_cache_entries e ON e.attachment_id = a.attachment_id
        WHERE {EVICTABLE_AVAILABLE_PREDICATE}
        ORDER BY e.last_access, a.attachment_id
        "#
    ))?;
    let ids = statement.query_map([], |row| row.get::<_, String>(0))?;
    for id in ids {
        let id: AttachmentId = id?.parse().map_err(|_| Error::Database)?;
        if !store.media_cache.leases.contains_key(&id) {
            return Ok(Some(id));
        }
    }
    Ok(None)
}

fn evict_store(store: &mut Store, attachment_id: AttachmentId) -> Result<bool, Error> {
    if store.media_cache.leases.contains_key(&attachment_id)
        || is_protected(&store.conn, attachment_id)?
    {
        return Ok(false);
    }
    let available: bool = store.conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM attachments WHERE attachment_id=? AND state='available')",
        [attachment_id.to_string()],
        |row| row.get(0),
    )?;
    if !available {
        return Ok(false);
    }
    let tx = store
        .conn
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    tx.execute(
        "UPDATE attachments SET state='pending_download' WHERE attachment_id=? AND state='available'",
        [attachment_id.to_string()],
    )?;
    tx.execute(
        "DELETE FROM attachment_demand WHERE attachment_id=?",
        [attachment_id.to_string()],
    )?;
    tx.execute(
        "DELETE FROM media_cache_entries WHERE attachment_id=?",
        [attachment_id.to_string()],
    )?;
    tx.commit()?;
    std::fs::remove_file(media::cipher_path(&store.media_dir, attachment_id))
        .map_err(|_| Error::Storage)?;
    Ok(true)
}

fn is_protected(conn: &Connection, attachment_id: AttachmentId) -> Result<bool, Error> {
    let evictable: bool = conn.query_row(
        &format!(
            r#"
            SELECT EXISTS(
                SELECT 1 FROM attachments a
                WHERE a.attachment_id = ? AND {EVICTABLE_AVAILABLE_PREDICATE}
            )
            "#
        ),
        [attachment_id.to_string()],
        |row| row.get(0),
    )?;
    Ok(!evictable)
}

#[cfg(test)]
mod tests;
