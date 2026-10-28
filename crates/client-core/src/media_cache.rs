//! Core-owned retained downloaded-ciphertext cache policy.
//!
//! LRU metadata is durable so reopening a store preserves its retained-download
//! inventory. Renderer/native leases are intentionally process-local: a crashed
//! host must not pin ciphertext forever after reopening the store.

use crate::{AttachmentId, AttachmentState, Client, Error, Store, media};
use rusqlite::{Connection, TransactionBehavior, params};
use std::collections::HashMap;

/// Maximum retained verified downloaded ciphertext, independent of plaintext limits.
pub const RETAINED_DOWNLOAD_CIPHERTEXT_BUDGET_BYTES: u64 = 128 * 1024 * 1024;

const EVICTABLE_AVAILABLE_PREDICATE: &str = r#"
    a.state = 'available'
    AND NOT EXISTS (SELECT 1 FROM attachment_demand d WHERE d.attachment_id = a.attachment_id)
    AND (
        EXISTS (SELECT 1 FROM gateway_mms_media_evictions e WHERE e.attachment_id=a.attachment_id AND e.unlinked_at IS NOT NULL)
        OR NOT EXISTS (SELECT 1 FROM message_media_meta m WHERE m.thumbnail_attachment_id = a.attachment_id)
    )
    AND (
        EXISTS (SELECT 1 FROM gateway_mms_media_evictions e WHERE e.attachment_id=a.attachment_id AND e.unlinked_at IS NOT NULL)
        OR NOT EXISTS (
            SELECT 1 FROM mms_acquisition_parts p
            WHERE p.attachment_id = a.attachment_id OR p.thumbnail_attachment_id = a.attachment_id
        )
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
    AND (
        EXISTS (SELECT 1 FROM gateway_mms_media_evictions e WHERE e.attachment_id=a.attachment_id AND e.unlinked_at IS NOT NULL)
        OR NOT (
            a.plaintext_bytes <= 2097152
            AND NOT EXISTS (SELECT 1 FROM message_media_meta m WHERE m.attachment_id = a.attachment_id)
        )
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

const MAX_GATEWAY_MMS_CLEANUP_BATCH: usize = 100;

const GATEWAY_MMS_CLEANUP_ELIGIBLE: &str = r#"
    a.state = 'uploaded'
    AND a.remote_object_id IS NOT NULL
    AND EXISTS (
        SELECT 1
        FROM mms_acquisition_parts p
        JOIN mms_acquisitions acquisition ON acquisition.acquisition_id = p.acquisition_id
        WHERE (p.attachment_id = a.attachment_id OR p.thumbnail_attachment_id = a.attachment_id)
          AND acquisition.state = 'complete'
    )
    AND NOT EXISTS (SELECT 1 FROM attachment_demand d WHERE d.attachment_id = a.attachment_id)
    AND NOT EXISTS (SELECT 1 FROM compose_drafts d WHERE instr(d.attachment_ids, a.attachment_id) > 0)
    AND NOT EXISTS (
        SELECT 1
        FROM contact_photo_received p
        WHERE p.attachment_id = a.attachment_id
    )
    AND NOT EXISTS (
        SELECT 1
        FROM contact_photo_attachments p
        WHERE p.attachment_id = a.attachment_id
    )
    AND NOT EXISTS (
        SELECT 1
        FROM contact_photo_references r
        WHERE r.attachment_id = a.attachment_id
          AND (r.retain_until IS NULL OR r.retain_until > CAST(strftime('%s', 'now') AS INTEGER))
    )
    AND NOT EXISTS (
        SELECT 1
        FROM contact_photo_registrations r
        WHERE r.attachment_id = a.attachment_id AND r.acknowledged = 0
    )
    AND NOT EXISTS (
        SELECT 1
        FROM outbox_attachments oa
        JOIN outbox o ON o.envelope_id = oa.envelope_id
        WHERE oa.attachment_id = a.attachment_id
          AND o.state IN ('unsealed', 'queued')
    )
"#;

/// Bounded result of removing Android gateway MMS capture ciphertext.
///
/// `pending_unlink` is nonzero after an unlink failure. The durable marker remains, so a later
/// call retries it without re-scanning all uploaded attachments.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GatewayMmsCleanup {
    pub marked: u32,
    pub unlinked: u32,
    pub pending_unlink: u32,
}

/// Per-open-store lease state. This is deliberately not persisted.
#[derive(Default)]
pub(crate) struct MediaCacheState {
    leases: HashMap<AttachmentId, u32>,
}

impl Client {
    /// Marks and removes a bounded batch of uploaded Android gateway MMS capture copies.
    ///
    /// This deliberately retains attachment metadata, file keys, and remote IDs. A durable marker
    /// is committed before unlinking, so a failed unlink is retried by the next bounded call.
    /// Only the Android gateway binding may invoke this API.
    pub fn cleanup_uploaded_gateway_mms_ciphertext(
        &self,
        limit: u32,
    ) -> Result<GatewayMmsCleanup, Error> {
        let limit = usize::try_from(limit).map_err(|_| Error::InvalidRequest("cleanup limit"))?;
        if !(1..=MAX_GATEWAY_MMS_CLEANUP_BATCH).contains(&limit) {
            return Err(Error::InvalidRequest("cleanup limit"));
        }

        let mut store = self.lock()?;
        cleanup_uploaded_gateway_mms(&mut store, limit)
    }

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
        CREATE TABLE IF NOT EXISTS gateway_mms_media_evictions(
            attachment_id TEXT PRIMARY KEY,
            marked_at INTEGER NOT NULL,
            unlinked_at INTEGER
        );
        CREATE INDEX IF NOT EXISTS gateway_mms_media_evictions_pending
            ON gateway_mms_media_evictions(marked_at, attachment_id)
            WHERE unlinked_at IS NULL;
        CREATE TABLE IF NOT EXISTS gateway_mms_cleanup_queue(
            attachment_id TEXT PRIMARY KEY,
            retry_order INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS gateway_mms_cleanup_queue_order
            ON gateway_mms_cleanup_queue(retry_order, attachment_id);
        CREATE TABLE IF NOT EXISTS gateway_mms_cleanup_bootstrap(
            id INTEGER PRIMARY KEY CHECK(id=1),
            attachment_rowid INTEGER NOT NULL,
            finished INTEGER NOT NULL
        );
        INSERT OR IGNORE INTO gateway_mms_cleanup_bootstrap(id,attachment_rowid,finished)
        VALUES(1,0,0);
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

/// Enqueues one recently uploaded candidate without walking attachment history. Call after the
/// upload state and remote ID commit; quiescence is rechecked by the bounded cleanup worker.
pub(crate) fn enqueue_gateway_mms_cleanup_candidate(
    conn: &Connection,
    attachment_id: AttachmentId,
) -> Result<(), Error> {
    let candidate: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM attachments a WHERE a.attachment_id=? AND a.state='uploaded' AND a.remote_object_id IS NOT NULL AND EXISTS(SELECT 1 FROM mms_acquisition_parts p WHERE p.attachment_id=a.attachment_id OR p.thumbnail_attachment_id=a.attachment_id))",
        [attachment_id.to_string()],
        |row| row.get(0),
    )?;
    if candidate {
        enqueue_gateway_mms_cleanup_id(conn, attachment_id)?;
    }
    Ok(())
}

/// Enqueues MMS attachments registered on a newly acknowledged envelope. The caller must invoke
/// this in the acknowledgement transaction after the state change.
pub(crate) fn enqueue_acknowledged_gateway_mms_cleanup(
    conn: &Connection,
    envelope_id: &str,
) -> Result<(), Error> {
    let mut statement = conn.prepare(
        "SELECT oa.attachment_id FROM outbox_attachments oa JOIN attachments a ON a.attachment_id=oa.attachment_id WHERE oa.envelope_id=? AND a.state='uploaded' AND a.remote_object_id IS NOT NULL AND EXISTS(SELECT 1 FROM mms_acquisition_parts p WHERE p.attachment_id=a.attachment_id OR p.thumbnail_attachment_id=a.attachment_id)",
    )?;
    let attachment_ids = statement
        .query_map([envelope_id], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    for attachment_id in attachment_ids {
        enqueue_gateway_mms_cleanup_id(conn, attachment_id.parse().map_err(|_| Error::Database)?)?;
    }
    Ok(())
}

/// Returns whether an attachment's local ciphertext was deliberately evicted by gateway MMS
/// cleanup. Callers must treat a marked `uploaded` attachment as remote-only even though
/// `uploaded` remains its durable upload state.
pub(crate) fn gateway_mms_ciphertext_evicted(
    conn: &Connection,
    attachment_id: AttachmentId,
) -> Result<bool, Error> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM gateway_mms_media_evictions e JOIN attachments a ON a.attachment_id=e.attachment_id WHERE e.attachment_id=? AND a.state='uploaded')",
        [attachment_id.to_string()],
        |row| row.get(0),
    )
    .map_err(Into::into)
}

/// Moves a deliberately evicted upload into the existing explicit-download flow.
///
/// The marker remains so thumbnail eagerness cannot re-download it on every gateway pass. The
/// caller must add the explicit demand before `pending_downloads` is queried.
pub(crate) fn request_gateway_mms_rehydration(
    store: &mut Store,
    attachment_id: AttachmentId,
) -> Result<bool, Error> {
    let tx = store
        .conn
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    let changed = request_gateway_mms_rehydration_in(&tx, attachment_id)?;
    tx.commit()?;
    Ok(changed)
}

/// Uses the caller's transaction to make an evicted gateway capture available to an active carrier
/// command. The caller must remain responsible for rejecting historical and retired commands.
pub(crate) fn request_gateway_mms_rehydration_in(
    conn: &Connection,
    attachment_id: AttachmentId,
) -> Result<bool, Error> {
    let marked: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM gateway_mms_media_evictions WHERE attachment_id=?)",
        [attachment_id.to_string()],
        |row| row.get(0),
    )?;
    if !marked {
        return Ok(false);
    }
    let changed = conn.execute(
        "UPDATE attachments SET state='pending_download' WHERE attachment_id=? AND state='uploaded' AND remote_object_id IS NOT NULL",
        [attachment_id.to_string()],
    )?;
    if changed != 0 {
        conn.execute(
            "INSERT INTO attachment_demand(attachment_id,requested_at) VALUES(?,CAST(strftime('%s','now') AS INTEGER)*1000) ON CONFLICT(attachment_id) DO NOTHING",
            [attachment_id.to_string()],
        )?;
        conn.execute(
            "UPDATE gateway_mms_media_evictions SET unlinked_at=NULL WHERE attachment_id=?",
            [attachment_id.to_string()],
        )?;
    }
    Ok(changed != 0)
}

/// Called after verified downloaded ciphertext becomes available, while the Store lock is held.
pub(crate) fn record_installed_attachment(
    store: &mut Store,
    attachment_id: AttachmentId,
) -> Result<(), Error> {
    record_installed_access(store, attachment_id)?;
    if mark_gateway_mms_rehydration_fulfilled(store, attachment_id)? {
        return Ok(());
    }
    trim_store(store, RETAINED_DOWNLOAD_CIPHERTEXT_BUDGET_BYTES)
}

fn cleanup_uploaded_gateway_mms(
    store: &mut Store,
    limit: usize,
) -> Result<GatewayMmsCleanup, Error> {
    let mut result = GatewayMmsCleanup::default();
    bootstrap_gateway_mms_cleanup_queue(&mut store.conn)?;
    let mut attachment_ids = marked_gateway_mms_evictions(&store.conn, limit)?;
    let remaining = limit.saturating_sub(attachment_ids.len());
    for attachment_id in queued_gateway_mms_cleanup_candidates(store, remaining)? {
        if mark_gateway_mms_eviction(store, attachment_id)? {
            result.marked = result.marked.checked_add(1).ok_or(Error::Database)?;
            attachment_ids.push(attachment_id);
        } else {
            rotate_gateway_mms_cleanup_candidate(&store.conn, attachment_id)?;
        }
    }

    for attachment_id in attachment_ids {
        if store.media_cache.leases.contains_key(&attachment_id) {
            result.pending_unlink = result
                .pending_unlink
                .checked_add(1)
                .ok_or(Error::Database)?;
            continue;
        }
        match std::fs::remove_file(media::cipher_path(&store.media_dir, attachment_id)) {
            Ok(()) => {
                record_gateway_mms_unlink(store, attachment_id)?;
                result.unlinked = result.unlinked.checked_add(1).ok_or(Error::Database)?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                record_gateway_mms_unlink(store, attachment_id)?;
                result.unlinked = result.unlinked.checked_add(1).ok_or(Error::Database)?;
            }
            Err(_) => {
                result.pending_unlink = result
                    .pending_unlink
                    .checked_add(1)
                    .ok_or(Error::Database)?;
            }
        }
    }
    Ok(result)
}

fn marked_gateway_mms_evictions(
    conn: &Connection,
    limit: usize,
) -> Result<Vec<AttachmentId>, Error> {
    let mut statement = conn.prepare(
        &format!(
            "SELECT e.attachment_id FROM gateway_mms_media_evictions e JOIN attachments a ON a.attachment_id=e.attachment_id WHERE e.unlinked_at IS NULL AND {GATEWAY_MMS_CLEANUP_ELIGIBLE} ORDER BY e.marked_at, e.attachment_id LIMIT ?"
        ),
    )?;
    statement
        .query_map([limit as i64], |row| row.get::<_, String>(0))?
        .map(|row| row?.parse().map_err(|_| Error::Database))
        .collect()
}

fn queued_gateway_mms_cleanup_candidates(
    store: &Store,
    limit: usize,
) -> Result<Vec<AttachmentId>, Error> {
    let mut statement = store.conn.prepare(
        "SELECT attachment_id FROM gateway_mms_cleanup_queue ORDER BY retry_order,attachment_id LIMIT ?",
    )?;
    statement
        .query_map([limit as i64], |row| row.get::<_, String>(0))?
        .map(|row| row?.parse().map_err(|_| Error::Database))
        .filter(|id| match id {
            Ok(id) => !store.media_cache.leases.contains_key(id),
            Err(_) => true,
        })
        .collect()
}

fn mark_gateway_mms_eviction(
    store: &mut Store,
    attachment_id: AttachmentId,
) -> Result<bool, Error> {
    if store.media_cache.leases.contains_key(&attachment_id) {
        return Ok(false);
    }
    let tx = store
        .conn
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    let eligible: bool = tx.query_row(
        &format!(
            "SELECT EXISTS(SELECT 1 FROM attachments a WHERE a.attachment_id=? AND {GATEWAY_MMS_CLEANUP_ELIGIBLE})"
        ),
        [attachment_id.to_string()],
        |row| row.get(0),
    )?;
    if !eligible {
        tx.commit()?;
        return Ok(false);
    }
    let marked = tx.execute(
        "INSERT OR IGNORE INTO gateway_mms_media_evictions(attachment_id,marked_at) VALUES(?,CAST(strftime('%s','now') AS INTEGER)*1000)",
        [attachment_id.to_string()],
    )?;
    tx.commit()?;
    if marked != 0 {
        store.conn.execute(
            "DELETE FROM gateway_mms_cleanup_queue WHERE attachment_id=?",
            [attachment_id.to_string()],
        )?;
    }
    Ok(marked != 0)
}

fn bootstrap_gateway_mms_cleanup_queue(conn: &mut Connection) -> Result<(), Error> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let (cursor, finished): (i64, bool) = tx.query_row(
        "SELECT attachment_rowid,finished FROM gateway_mms_cleanup_bootstrap WHERE id=1",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    if finished {
        tx.commit()?;
        return Ok(());
    }
    let rows = {
        let mut statement = tx.prepare(
            "SELECT rowid,attachment_id FROM attachments WHERE rowid>? ORDER BY rowid LIMIT ?",
        )?;
        statement
            .query_map([cursor, MAX_GATEWAY_MMS_CLEANUP_BATCH as i64], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?
    };
    let last_rowid = rows.last().map(|(rowid, _)| *rowid);
    for (_, attachment_id) in rows {
        enqueue_gateway_mms_cleanup_candidate(
            &tx,
            attachment_id.parse().map_err(|_| Error::Database)?,
        )?;
    }
    match last_rowid {
        Some(rowid) => tx.execute(
            "UPDATE gateway_mms_cleanup_bootstrap SET attachment_rowid=? WHERE id=1",
            [rowid],
        )?,
        None => tx.execute(
            "UPDATE gateway_mms_cleanup_bootstrap SET finished=1 WHERE id=1",
            [],
        )?,
    };
    tx.commit()?;
    Ok(())
}

fn enqueue_gateway_mms_cleanup_id(
    conn: &Connection,
    attachment_id: AttachmentId,
) -> Result<(), Error> {
    conn.execute(
        "INSERT INTO gateway_mms_cleanup_queue(attachment_id,retry_order) VALUES(?,COALESCE((SELECT MAX(retry_order) FROM gateway_mms_cleanup_queue),0)+1) ON CONFLICT(attachment_id) DO NOTHING",
        [attachment_id.to_string()],
    )?;
    Ok(())
}

fn rotate_gateway_mms_cleanup_candidate(
    conn: &Connection,
    attachment_id: AttachmentId,
) -> Result<(), Error> {
    conn.execute(
        "UPDATE gateway_mms_cleanup_queue SET retry_order=COALESCE((SELECT MAX(retry_order) FROM gateway_mms_cleanup_queue),0)+1 WHERE attachment_id=?",
        [attachment_id.to_string()],
    )?;
    Ok(())
}

fn record_gateway_mms_unlink(store: &mut Store, attachment_id: AttachmentId) -> Result<(), Error> {
    store.conn.execute(
        "UPDATE gateway_mms_media_evictions SET unlinked_at=CAST(strftime('%s','now') AS INTEGER)*1000 WHERE attachment_id=?",
        [attachment_id.to_string()],
    )?;
    Ok(())
}

fn mark_gateway_mms_rehydration_fulfilled(
    store: &mut Store,
    attachment_id: AttachmentId,
) -> Result<bool, Error> {
    let fulfilled = store.conn.execute(
        "UPDATE gateway_mms_media_evictions SET unlinked_at=CAST(strftime('%s','now') AS INTEGER)*1000 WHERE attachment_id=? AND unlinked_at IS NULL",
        [attachment_id.to_string()],
    )?;
    store.conn.execute(
        "DELETE FROM gateway_mms_cleanup_queue WHERE attachment_id=?",
        [attachment_id.to_string()],
    )?;
    Ok(fulfilled != 0)
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
    if gateway_mms_ciphertext_evicted(&store.conn, attachment_id)? {
        return Err(Error::InvalidMedia);
    }
    let state = crate::attachment_row(&store.conn, attachment_id)?
        .ok_or(Error::NotFound)?
        .state;
    if state.is_local() {
        Ok(state)
    } else {
        Err(Error::InvalidMedia)
    }
}

/// Evicts least-recently-used unleased candidates until unleased retained bytes fit the budget.
///
/// Eligibility is read once, in LRU order, and the statement is dropped before any eviction.
/// Leases cannot change while the Store lock is held, so the snapshot stays valid.
fn trim_store(store: &mut Store, budget: u64) -> Result<(), Error> {
    let candidates = evictable_candidates(store)?;
    let mut retained = 0_u64;
    for (attachment_id, bytes) in &candidates {
        if !store.media_cache.leases.contains_key(attachment_id) {
            retained = retained.checked_add(*bytes).ok_or(Error::Database)?;
        }
    }
    for (attachment_id, bytes) in candidates {
        if retained <= budget {
            break;
        }
        if store.media_cache.leases.contains_key(&attachment_id) {
            continue;
        }
        if !evict_store(store, attachment_id)? {
            break;
        }
        retained = retained.checked_sub(bytes).ok_or(Error::Database)?;
    }
    Ok(())
}

/// Every eviction-eligible row with its ciphertext size, least recently used first.
fn evictable_candidates(store: &Store) -> Result<Vec<(AttachmentId, u64)>, Error> {
    let mut statement = store.conn.prepare(&format!(
        r#"
        SELECT a.attachment_id, a.ciphertext_bytes
        FROM attachments a
        JOIN media_cache_entries e ON e.attachment_id = a.attachment_id
        WHERE {EVICTABLE_AVAILABLE_PREDICATE}
        ORDER BY e.last_access, a.attachment_id
        "#
    ))?;
    let rows = statement.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
    })?;
    let mut candidates = Vec::new();
    for row in rows {
        let (id, bytes) = row?;
        let id: AttachmentId = id.parse().map_err(|_| Error::Database)?;
        let bytes = u64::try_from(bytes).map_err(|_| Error::Database)?;
        candidates.push((id, bytes));
    }
    Ok(candidates)
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
    // The eviction is already committed, so a ciphertext file that is already gone reaches the
    // same end state. Other I/O failures still surface as storage errors.
    match std::fs::remove_file(media::cipher_path(&store.media_dir, attachment_id)) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(true),
        Err(_) => Err(Error::Storage),
    }
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
