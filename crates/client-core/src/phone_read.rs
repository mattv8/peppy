use super::*;

pub const PHONE_READ_EFFECT_TTL_MS: i64 = 86_400_000;
const PHONE_READ_ENABLED_SQL: &str =
    "SELECT phone_read_enabled AND sms_sync_enabled FROM gateway_settings WHERE id=1";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderRowRef {
    pub source_generation: String,
    pub subscription_id: String,
    pub provider_message_id: String,
    pub provider_thread_id: String,
    pub provider_date_ms: i64,
    pub source_row_fingerprint: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PhoneReadEffect {
    pub message_id: MessageId,
    pub row: ProviderRowRef,
}

impl Client {
    pub fn pending_phone_read_effects(
        &self,
        now_ms: i64,
        limit: u32,
    ) -> Result<Vec<PhoneReadEffect>, Error> {
        let mut store = self.lock()?;
        if restore_guarded(&store.conn)?
            || store
                .active_epoch
                .is_none_or(|epoch| !store.keys.contains_key(&epoch))
        {
            return Ok(Vec::new());
        }
        let tx = store
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        retire_expired(&tx, now_ms)?;
        if !phone_read_enabled(&tx)? {
            tx.commit()?;
            return Ok(Vec::new());
        }
        let mut statement = tx.prepare(
            "SELECT message_id,source_generation,subscription_id,provider_message_id,provider_thread_id,provider_date_ms,source_row_fingerprint FROM phone_read_effects ORDER BY created_at_ms LIMIT ?",
        )?;
        let effects = statement
            .query_map(params![i64::from(limit)], |row| {
                Ok(PhoneReadEffect {
                    message_id: parse(&row.get::<_, String>(0)?)
                        .map_err(|_| rusqlite::Error::InvalidQuery)?,
                    row: ProviderRowRef {
                        source_generation: row.get(1)?,
                        subscription_id: row.get(2)?,
                        provider_message_id: row.get(3)?,
                        provider_thread_id: row.get(4)?,
                        provider_date_ms: row.get(5)?,
                        source_row_fingerprint: row.get(6)?,
                    },
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        drop(statement);
        tx.commit()?;
        Ok(effects)
    }

    pub fn complete_phone_read_effects(&self, message_ids: &[MessageId]) -> Result<(), Error> {
        let mut store = self.lock()?;
        let tx = store
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        retire_effects(&tx, message_ids)?;
        tx.commit()?;
        Ok(())
    }

    pub fn sms_provider_rows_seen(&self, rows: &[ProviderRowRef]) -> Result<bool, Error> {
        if rows.is_empty() {
            return Ok(false);
        }
        let store = self.lock()?;
        rows.iter().try_fold(true, |all_seen, row| {
            if !all_seen {
                return Ok(false);
            }
            let seen = row_seen(&store.conn, row)?;
            Ok(seen)
        })
    }

    pub fn consume_phone_read_effects(
        &self,
        message_ids: &[MessageId],
        rows: &[ProviderRowRef],
        now_ms: i64,
    ) -> Result<u32, Error> {
        if message_ids.is_empty() || rows.is_empty() || !unique_ids(message_ids) {
            return Ok(0);
        }
        let mut store = self.lock()?;
        if restore_guarded(&store.conn)?
            || store
                .active_epoch
                .is_none_or(|epoch| !store.keys.contains_key(&epoch))
        {
            return Ok(0);
        }
        let tx = store
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        retire_expired(&tx, now_ms)?;
        if !phone_read_enabled(&tx)? {
            tx.commit()?;
            return Ok(0);
        }
        let Some(requested) = requested_effect_rows(&tx, message_ids)? else {
            tx.commit()?;
            return Ok(0);
        };
        let scope = EffectScope::from(&requested[0]);
        let mut requested_are_current = true;
        for row in &requested {
            requested_are_current &= row_seen(&tx, row)?;
        }
        if requested.iter().any(|row| EffectScope::from(row) != scope)
            || !requested_are_current
            || rows.iter().any(|row| EffectScope::from(row) != scope)
            || !rows_seen_tx(&tx, rows)?
        {
            tx.commit()?;
            return Ok(0);
        }
        retire_effects(&tx, message_ids)?;
        tx.commit()?;
        u32::try_from(message_ids.len()).map_err(|_| Error::Database)
    }
}

pub(crate) fn enqueue_phone_read_effect_tx(
    tx: &rusqlite::Transaction<'_>,
    message_id: MessageId,
    now_ms: i64,
) -> Result<(), Error> {
    if restore_guarded(tx)? || !phone_read_enabled(tx)? {
        return Ok(());
    }
    insert_phone_read_effect(tx, message_id, now_ms)
}

pub(crate) fn enqueue_phone_read_effect(
    conn: &Connection,
    message_id: MessageId,
    now_ms: i64,
) -> Result<(), Error> {
    if restore_guarded(conn)? {
        return Ok(());
    }
    let enabled: bool = conn.query_row(PHONE_READ_ENABLED_SQL, [], |row| row.get(0))?;
    if !enabled {
        return Ok(());
    }
    insert_phone_read_effect(conn, message_id, now_ms)?;
    Ok(())
}

pub(crate) fn retire_all_phone_read_effects(tx: &rusqlite::Transaction<'_>) -> Result<(), Error> {
    tx.execute(
        "INSERT OR IGNORE INTO phone_read_effect_terminals(message_id) SELECT message_id FROM phone_read_effects",
        [],
    )?;
    tx.execute("DELETE FROM phone_read_effects", [])?;
    Ok(())
}

fn phone_read_enabled(conn: &rusqlite::Transaction<'_>) -> Result<bool, Error> {
    conn.query_row(PHONE_READ_ENABLED_SQL, [], |row| row.get(0))
        .map_err(Into::into)
}

fn retire_expired(tx: &rusqlite::Transaction<'_>, now_ms: i64) -> Result<(), Error> {
    tx.execute(
        "INSERT OR IGNORE INTO phone_read_effect_terminals(message_id) SELECT message_id FROM phone_read_effects WHERE created_at_ms < ?",
        params![now_ms.saturating_sub(PHONE_READ_EFFECT_TTL_MS)],
    )?;
    tx.execute(
        "DELETE FROM phone_read_effects WHERE created_at_ms < ?",
        params![now_ms.saturating_sub(PHONE_READ_EFFECT_TTL_MS)],
    )?;
    Ok(())
}

fn retire_effects(tx: &rusqlite::Transaction<'_>, message_ids: &[MessageId]) -> Result<(), Error> {
    for message_id in message_ids {
        tx.execute(
            "INSERT OR IGNORE INTO phone_read_effect_terminals(message_id) VALUES(?)",
            params![message_id.to_string()],
        )?;
        tx.execute(
            "DELETE FROM phone_read_effects WHERE message_id=?",
            params![message_id.to_string()],
        )?;
    }
    Ok(())
}

fn rows_seen_tx(tx: &rusqlite::Transaction<'_>, rows: &[ProviderRowRef]) -> Result<bool, Error> {
    for row in rows {
        let seen = row_seen(tx, row)?;
        if !seen {
            return Ok(false);
        }
    }
    Ok(true)
}

const EFFECT_ACQUISITION_PREDICATE: &str = "a.direction='incoming' AND a.state='complete' AND json_extract(a.input,'$.imported')=0 AND json_extract(a.input,'$.sent_at_ms')>0 AND json_extract(a.input,'$.source.provider_thread_id') IS NOT NULL";

fn insert_phone_read_effect(
    conn: &Connection,
    message_id: MessageId,
    now_ms: i64,
) -> Result<(), Error> {
    conn.execute(
        &format!("INSERT OR IGNORE INTO phone_read_effects(message_id,source_generation,subscription_id,provider_message_id,provider_thread_id,provider_date_ms,source_row_fingerprint,created_at_ms) SELECT a.message_id,a.source_generation,a.subscription_id,a.provider_message_id,COALESCE(json_extract(a.input,'$.source.provider_thread_id'),''),json_extract(a.input,'$.sent_at_ms'),a.source_row_fingerprint,? FROM sms_acquisitions a WHERE a.message_id=? AND {EFFECT_ACQUISITION_PREDICATE} AND NOT EXISTS(SELECT 1 FROM phone_read_effect_terminals t WHERE t.message_id=a.message_id)"),
        params![now_ms, message_id.to_string()],
    )?;
    Ok(())
}

fn row_seen(conn: &Connection, row: &ProviderRowRef) -> Result<bool, Error> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sms_acquisitions a JOIN messages m ON m.id=a.message_id WHERE a.source_generation=? AND a.subscription_id=? AND a.provider_message_id=? AND a.source_row_fingerprint=? AND a.direction='incoming' AND a.state='complete' AND json_extract(a.input,'$.imported')=0 AND json_extract(a.input,'$.sent_at_ms')=? AND COALESCE(json_extract(a.input,'$.source.provider_thread_id'),'')=? AND m.seen=1)",
        params![row.source_generation, row.subscription_id, row.provider_message_id, row.source_row_fingerprint, row.provider_date_ms, row.provider_thread_id],
        |result| result.get(0),
    ).map_err(Into::into)
}

#[derive(PartialEq, Eq)]
struct EffectScope<'a> {
    source_generation: &'a str,
    subscription_id: &'a str,
    provider_thread_id: &'a str,
}

impl<'a> From<&'a ProviderRowRef> for EffectScope<'a> {
    fn from(row: &'a ProviderRowRef) -> Self {
        Self {
            source_generation: &row.source_generation,
            subscription_id: &row.subscription_id,
            provider_thread_id: &row.provider_thread_id,
        }
    }
}

fn requested_effect_rows(
    tx: &rusqlite::Transaction<'_>,
    message_ids: &[MessageId],
) -> Result<Option<Vec<ProviderRowRef>>, Error> {
    message_ids.iter().map(|message_id| {
        tx.query_row(
            "SELECT source_generation,subscription_id,provider_message_id,provider_thread_id,provider_date_ms,source_row_fingerprint FROM phone_read_effects WHERE message_id=?",
            params![message_id.to_string()],
            |row| Ok(ProviderRowRef { source_generation: row.get(0)?, subscription_id: row.get(1)?, provider_message_id: row.get(2)?, provider_thread_id: row.get(3)?, provider_date_ms: row.get(4)?, source_row_fingerprint: row.get(5)? }),
        ).optional().map_err(Into::into)
    }).collect()
}

fn unique_ids(message_ids: &[MessageId]) -> bool {
    let mut unique = HashSet::with_capacity(message_ids.len());
    message_ids
        .iter()
        .all(|message_id| unique.insert(*message_id))
}
