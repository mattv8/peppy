use rusqlite::{Connection, OptionalExtension, params};

use crate::{
    ConversationId, Direction, Error, MessagePayload, Transport, conversation_match, parse,
};

const MERGE_MOVE_ROWS: i64 = 256;
const BACKFILL_ROWS: i64 = 64;
const UNRESOLVED_RESUMMARIZE_ROWS: i64 = 64;
const MERGE_MEMBER_PAGE_SQL: &str = "SELECT local_order FROM messages INDEXED BY messages_conversation WHERE conversation_id=?1 AND local_order>?2 ORDER BY local_order LIMIT ?3";

/// Records each message once, whether reached by insertion or the historical scan.
pub(crate) fn summarize(conn: &Connection, message: &MessagePayload) -> Result<(), Error> {
    let region = conversation_match::identity_region(conn)?;
    let participants = if message.transport == Transport::Mms {
        conversation_match::message_recipient_set(
            conn,
            message,
            Some(&region),
            &mut conversation_match::OwnAddressCache::new(),
        )?
    } else {
        let addresses = match message.direction {
            Direction::Incoming => message.sender_address.iter().cloned().collect(),
            Direction::Outgoing => message.recipients.clone(),
        };
        Some(conversation_match::recipient_set(&addresses, Some(&region)))
    };
    let generation = crate::get_meta(conn, "conversation_mms_generation")?
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or(0);
    if conn.execute(
        "INSERT OR IGNORE INTO conversation_counterpart_messages(message_id,unresolved,generation,conversation_id) VALUES(?,?,?,?)",
        params![message.record.message_id.to_string(), participants.is_none(), generation, message.record.conversation_id.to_string()],
    )? == 0 {
        return Ok(());
    }
    let Some(participants) = participants else {
        crate::set_meta(conn, "conversation_merge_dirty", "1")?;
        return Ok(());
    };
    let count = participants.len();
    let address = participants.first();
    conn.execute(
        "INSERT INTO conversation_counterparts(conversation_id,canonical_address,counterparts,carrier_only,has_group,message_count)
         VALUES(?,?,?,?,?,1)
         ON CONFLICT(conversation_id) DO UPDATE SET
           counterparts=CASE WHEN counterparts=1 AND excluded.counterparts=1 AND canonical_address=excluded.canonical_address THEN 1 ELSE 2 END,
           carrier_only=carrier_only AND excluded.carrier_only,
           has_group=has_group OR excluded.has_group,
           message_count=message_count+1",
        params![message.record.conversation_id.to_string(), address, count as i64,
            message.transport != Transport::Rcs, count != 1],
    )?;
    crate::set_meta(conn, "conversation_merge_dirty", "1")?;
    Ok(())
}

fn eligible(conn: &Connection, id: ConversationId) -> Result<bool, Error> {
    let summary: Option<(Option<String>, i64, bool, bool, i64)> = conn
        .query_row(
            "SELECT canonical_address,counterparts,carrier_only,has_group,message_count FROM conversation_counterparts WHERE conversation_id=?",
            [id.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
        )
        .optional()?;
    let Some((Some(address), 1, true, false, count)) = summary else {
        return Ok(false);
    };
    if count == 0
        || !address.starts_with('+')
        || !phonenumber::parse(None, &address).is_ok_and(|number| number.is_valid())
        || crate::rcs_mapping::has_live_mapping(conn, id)?
        || crate::rcs_group::group_mapping(conn, id)?.is_some()
        || crate::rcs_group::is_group_conversation(conn, id)?
    {
        return Ok(false);
    }
    let excluded: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM rcs_mirror_ids WHERE conversation_id=?1)
          OR EXISTS(SELECT 1 FROM conversation_counterpart_messages WHERE conversation_id=?1 AND unresolved=1)
          OR EXISTS(
            SELECT 1 FROM conversation_merges m JOIN conversation_counterparts child ON child.conversation_id=m.source_id
            JOIN conversation_counterparts parent ON parent.conversation_id=m.target_id
            WHERE m.target_id=?1 AND (child.counterparts<>1 OR child.carrier_only<>1 OR child.has_group<>0
              OR child.canonical_address IS NOT parent.canonical_address
              OR EXISTS(SELECT 1 FROM conversation_counterpart_messages u WHERE u.conversation_id=child.conversation_id AND u.unresolved=1)
              OR EXISTS(SELECT 1 FROM rcs_mirror_ids r WHERE r.conversation_id=child.conversation_id))
          )",
        [id.to_string()],
        |row| row.get(0),
    )?;
    Ok(!excluded)
}

fn finish_merge(tx: &Connection, target: &str, source: &str) -> Result<(), Error> {
    preserve_legacy_conflict(tx, target, source)?;
    let target_draft = crate::load_compose_draft(tx, "conversation_id", target)?;
    let source_draft = crate::load_compose_draft(tx, "conversation_id", source)?;
    match (target_draft, source_draft) {
        (Some(target_draft), Some(source_draft)) => {
            crate::draft_sync::preserve_conversation_merge_draft(
                tx,
                target_draft.draft_id,
                &source_draft,
            )?;
        }
        (None, Some(_)) => {
            tx.execute(
                "UPDATE compose_drafts SET conversation_id=? WHERE conversation_id=?",
                params![target, source],
            )?;
        }
        _ => {}
    }
    tx.execute("UPDATE drafts SET conversation_id=? WHERE conversation_id=? AND NOT EXISTS(SELECT 1 FROM drafts WHERE conversation_id=?)", params![target, source, target])?;
    tx.execute(
        "UPDATE conversation_merges SET target_id=? WHERE target_id=?",
        params![target, source],
    )?;
    tx.execute("INSERT INTO conversation_merges(source_id,target_id) VALUES(?,?) ON CONFLICT(source_id) DO UPDATE SET target_id=excluded.target_id", params![source, target])?;
    for table in [
        "addresses",
        "mms_thread_conversations",
        "banner_candidates",
        "sender_unblock_requests",
    ] {
        tx.execute(
            &format!("UPDATE {table} SET conversation_id=? WHERE conversation_id=?"),
            params![target, source],
        )?;
    }
    tx.execute("UPDATE compose_draft_redirects SET target_conversation_id=? WHERE target_conversation_id=?", params![target, source])?;
    tx.execute("UPDATE compose_draft_redirects SET conversation_id=? WHERE conversation_id=? AND forward_conversation=0", params![target, source])?;
    tx.execute("UPDATE compose_draft_redirects SET forward_conversation=0 WHERE conversation_id=? AND target_conversation_id=? AND forward_conversation=1", params![target, target])?;
    tx.execute(
        "INSERT OR REPLACE INTO provider_folder_writes SELECT ?1,folder,logical_revision,writer_device_id FROM provider_folder_writes WHERE conversation_id IN (?1,?2) ORDER BY logical_revision DESC,writer_device_id DESC LIMIT 1",
        params![target, source],
    )?;
    tx.execute(
        "DELETE FROM provider_folder_writes WHERE conversation_id=?",
        [source],
    )?;
    tx.execute(
        "UPDATE conversation_merge_jobs SET state='sweeping',cursor=0,member_id='' WHERE source_id=?",
        [source],
    )?;
    Ok(())
}

#[cfg(test)]
fn merge_pair(
    conn: &mut Connection,
    target: ConversationId,
    source: ConversationId,
) -> Result<(), Error> {
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    finish_merge(&tx, &target.to_string(), &source.to_string())?;
    tx.commit()?;
    Ok(())
}

fn preserve_legacy_conflict(conn: &Connection, target: &str, source: &str) -> Result<(), Error> {
    let read = |id: &str| -> Result<Option<(String, i64)>, Error> {
        Ok(conn
            .query_row(
                "SELECT content,revision FROM drafts WHERE conversation_id=?",
                [id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?)
    };
    let (Some((target_text, target_revision)), Some((source_text, source_revision))) =
        (read(target)?, read(source)?)
    else {
        return Ok(());
    };
    let target_draft = match crate::load_compose_draft(conn, "conversation_id", target)? {
        Some(draft) => draft,
        None => {
            let id = crate::DraftId::new();
            conn.execute(
                "INSERT INTO compose_drafts(draft_id,conversation_id,text,recipients,attachment_ids,route,revision) VALUES(?,?,?,'[]','[]',NULL,?)",
                params![id.to_string(), target, target_text, target_revision],
            )?;
            crate::load_compose_draft(conn, "draft_id", &id.to_string())?.ok_or(Error::Database)?
        }
    };
    let legacy = crate::ComposeDraft {
        draft_id: crate::DraftId::new(),
        conversation_id: parse(source)?,
        text: source_text,
        recipients: Vec::new(),
        attachment_ids: Vec::new(),
        route: None,
        revision: u64::try_from(source_revision).map_err(|_| Error::Database)?,
        group_name: None,
    };
    crate::draft_sync::preserve_conversation_merge_draft(conn, target_draft.draft_id, &legacy)
}

fn merge_step(conn: &mut Connection) -> Result<bool, Error> {
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let job: Option<(String, String, i64, String, String)> = tx.query_row(
        "SELECT source_id,target_id,cursor,state,member_id FROM conversation_merge_jobs ORDER BY source_id LIMIT 1",
        [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
    ).optional()?;
    let Some((source, mut target, mut cursor, mut state, mut member)) = job else {
        return Ok(false);
    };
    if state == "moving" && (!eligible(&tx, parse(&source)?)? || !eligible(&tx, parse(&target)?)?) {
        tx.execute(
            "UPDATE conversation_merge_jobs SET state='reverting',cursor=0,member_id='' WHERE source_id=?",
            [&source],
        )?;
        state = "reverting".into();
        cursor = 0;
        member.clear();
    }
    let lower: Option<String> = if state == "moving" {
        tx.query_row(
            "SELECT c.conversation_id FROM conversation_counterparts c
             WHERE c.canonical_address=(SELECT canonical_address FROM conversation_counterparts WHERE conversation_id=?1)
               AND c.conversation_id<?1 AND c.counterparts=1 AND c.carrier_only=1 AND c.has_group=0 AND c.message_count>0
               AND NOT EXISTS(SELECT 1 FROM conversation_merges m WHERE m.source_id=c.conversation_id)
               AND NOT EXISTS(SELECT 1 FROM conversation_merge_jobs j WHERE j.source_id=c.conversation_id)
               AND NOT EXISTS(SELECT 1 FROM rcs_mirror_ids r WHERE r.conversation_id=c.conversation_id)
               AND NOT EXISTS(SELECT 1 FROM rcs_conversation_mappings r WHERE r.peppy_conversation_id=c.conversation_id AND r.invalidated_at_ms IS NULL)
               AND NOT EXISTS(SELECT 1 FROM rcs_group_mapping g WHERE g.conversation_id=c.conversation_id)
               AND (SELECT COUNT(*) FROM conversation_group_members g WHERE g.conversation_id=c.conversation_id)<2
               AND EXISTS(SELECT 1 FROM conversation_message_counts n WHERE n.conversation_id=c.conversation_id AND n.total=n.processed)
             ORDER BY c.conversation_id LIMIT 1",
            [&target], |row| row.get(0),
        ).optional()?
    } else {
        None
    };
    if let Some(lower) = lower
        && eligible(&tx, parse(&lower)?)?
    {
        target = lower;
        cursor = 0;
        tx.execute(
            "UPDATE conversation_merge_jobs SET target_id=?,cursor=0,member_id='' WHERE source_id=?",
            params![target, source],
        )?;
        member.clear();
    }
    let destination = if state == "reverting" {
        &source
    } else {
        &target
    };
    tx.execute(
        "INSERT OR IGNORE INTO conversation_merge_job_members(source_id,member_id) VALUES(?1,?1)",
        [&source],
    )?;
    if member.is_empty() {
        tx.execute(
            "INSERT OR IGNORE INTO conversation_merge_job_members(source_id,member_id)
             SELECT ?1,source_id FROM conversation_merges WHERE target_id=?1",
            [&source],
        )?;
        member = tx.query_row(
            "SELECT member_id FROM conversation_merge_job_members WHERE source_id=? ORDER BY member_id LIMIT 1",
            [&source],
            |row| row.get(0),
        )?;
    }
    let orders: Vec<i64> = {
        let mut query = tx.prepare(MERGE_MEMBER_PAGE_SQL)?;
        let rows = query.query_map(params![member, cursor, MERGE_MOVE_ROWS], |row| row.get(0))?;
        rows.collect::<Result<_, _>>()?
    };
    if let Some(&order) = orders.last() {
        tx.execute(
            "UPDATE messages SET effective_conversation_id=?1 WHERE conversation_id=?2 AND local_order>?3 AND local_order<=?4 AND effective_conversation_id<>?1",
            params![destination, member, cursor, order],
        )?;
        cursor = order;
    }
    if state == "moving" {
        let source_visible: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM messages m WHERE m.effective_conversation_id=? AND NOT EXISTS(SELECT 1 FROM send_cancellations c WHERE c.message_id=m.id AND c.state='confirmed'))",
            [&source], |row| row.get(0),
        )?;
        if !source_visible {
            finish_merge(&tx, &target, &source)?;
            tx.commit()?;
            return Ok(true);
        }
    }
    let more_in_member: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM messages INDEXED BY messages_conversation WHERE conversation_id=? AND local_order>?)",
        params![member, cursor], |row| row.get(0),
    )?;
    if more_in_member {
        tx.execute(
            "UPDATE conversation_merge_jobs SET member_id=?,cursor=? WHERE source_id=?",
            params![member, cursor, source],
        )?;
    } else {
        let next: Option<String> = tx.query_row(
            "SELECT member_id FROM conversation_merge_job_members WHERE source_id=? AND member_id>? ORDER BY member_id LIMIT 1",
            params![source, member], |row| row.get(0),
        ).optional()?;
        if let Some(next) = next {
            tx.execute(
                "UPDATE conversation_merge_jobs SET member_id=?,cursor=0 WHERE source_id=?",
                params![next, source],
            )?;
        } else if state == "moving" {
            // New source rows may have arrived in an earlier member while this pass advanced.
            tx.execute(
                "UPDATE conversation_merge_jobs SET member_id='',cursor=0 WHERE source_id=?",
                [&source],
            )?;
        } else if state == "sweeping"
            && tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM messages WHERE effective_conversation_id=?)",
                [&source],
                |row| row.get::<_, bool>(0),
            )?
        {
            tx.execute(
                "UPDATE conversation_merge_jobs SET member_id='',cursor=0 WHERE source_id=?",
                [&source],
            )?;
        } else {
            tx.execute(
                "DELETE FROM conversation_merge_job_members WHERE source_id=?",
                [&source],
            )?;
            tx.execute(
                "DELETE FROM conversation_merge_jobs WHERE source_id=?",
                [&source],
            )?;
        }
    }
    tx.commit()?;
    Ok(true)
}

fn candidate_pair(conn: &Connection) -> Result<Option<(ConversationId, ConversationId)>, Error> {
    let pair: Option<(String, String)> = conn.query_row(
        "WITH eligible AS NOT MATERIALIZED (
           SELECT c.conversation_id, c.canonical_address
           FROM conversation_counterparts c
           WHERE c.counterparts=1 AND c.carrier_only=1 AND c.has_group=0 AND c.message_count>0
             AND c.canonical_address LIKE '+%'
             AND NOT EXISTS (SELECT 1 FROM conversation_merges m WHERE m.source_id=c.conversation_id)
             AND NOT EXISTS (SELECT 1 FROM conversation_merge_jobs j WHERE j.source_id=c.conversation_id)
             AND EXISTS (SELECT 1 FROM conversation_message_counts n WHERE n.conversation_id=c.conversation_id AND n.total=n.processed)
             AND NOT EXISTS (SELECT 1 FROM rcs_mirror_ids r WHERE r.conversation_id=c.conversation_id)
             AND NOT EXISTS (SELECT 1 FROM rcs_conversation_mappings r WHERE r.peppy_conversation_id=c.conversation_id AND r.invalidated_at_ms IS NULL)
             AND NOT EXISTS (SELECT 1 FROM rcs_group_mapping g WHERE g.conversation_id=c.conversation_id)
             AND (SELECT COUNT(*) FROM conversation_group_members g WHERE g.conversation_id=c.conversation_id)<2
             AND NOT EXISTS (
               SELECT 1 FROM conversation_merges m
               JOIN conversation_counterparts s ON s.conversation_id=m.source_id
               WHERE m.target_id=c.conversation_id
                 AND (s.counterparts<>1 OR s.carrier_only<>1 OR s.has_group<>0
                      OR s.canonical_address IS NOT c.canonical_address
                      OR EXISTS(SELECT 1 FROM conversation_counterpart_messages u WHERE u.conversation_id=s.conversation_id AND u.unresolved=1)
                      OR EXISTS(SELECT 1 FROM rcs_mirror_ids r WHERE r.conversation_id=s.conversation_id))
             )
         )
         SELECT a.conversation_id,b.conversation_id FROM eligible a JOIN eligible b
           ON a.canonical_address=b.canonical_address AND a.conversation_id<b.conversation_id
         ORDER BY a.canonical_address,a.conversation_id,b.conversation_id LIMIT 1",
        [], |row| Ok((row.get(0)?, row.get(1)?)),
    ).optional()?;
    pair.map(|(a, b)| Ok((parse(&a)?, parse(&b)?))).transpose()
}

fn resummarize_unresolved(conn: &mut Connection) -> Result<bool, Error> {
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let generation = crate::get_meta(&tx, "conversation_mms_generation")?
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or(0);
    let rows: Vec<(String, String)> = {
        let mut query = tx.prepare(
            "SELECT m.id,m.payload FROM conversation_counterpart_messages u JOIN messages m ON m.id=u.message_id WHERE u.unresolved=1 AND u.generation<? ORDER BY u.generation LIMIT ?",
        )?;
        let rows = query.query_map(params![generation, UNRESOLVED_RESUMMARIZE_ROWS], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })?;
        rows.collect::<Result<_, _>>()?
    };
    if rows.is_empty() {
        return Ok(false);
    }
    for (id, payload) in rows {
        let message: MessagePayload = crate::decode(payload.as_bytes())?;
        tx.execute(
            "DELETE FROM conversation_counterpart_messages WHERE message_id=?",
            [&id],
        )?;
        summarize(&tx, &message)?;
    }
    tx.commit()?;
    Ok(true)
}

fn backfill_step(conn: &mut Connection) -> Result<bool, Error> {
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let cursor = crate::get_meta(&tx, "conversation_counterpart_cursor")?
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(0);
    let rows: Vec<(i64, String, String, String)> = {
        let mut query = tx.prepare(
            "SELECT local_order,payload,id,conversation_id FROM messages WHERE local_order>? ORDER BY local_order LIMIT ?",
        )?;
        let rows = query.query_map(params![cursor, BACKFILL_ROWS], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })?;
        rows.collect::<Result<_, _>>()?
    };
    if rows.is_empty() {
        return Ok(false);
    }
    let last_order = rows.last().map(|row| row.0).ok_or(Error::Database)?;
    for (_, payload, message_id, conversation_id) in rows {
        match crate::decode::<MessagePayload>(payload.as_bytes()) {
            Ok(message) => summarize(&tx, &message)?,
            Err(_) => {
                if tx.execute(
                    "INSERT OR IGNORE INTO conversation_counterpart_messages(message_id,conversation_id) VALUES(?,?)",
                    params![message_id, conversation_id],
                )? != 0
                {
                    tx.execute(
                        "INSERT INTO conversation_counterparts(conversation_id,canonical_address,counterparts,carrier_only,has_group,message_count) VALUES(?,NULL,0,0,1,1)
                         ON CONFLICT(conversation_id) DO UPDATE SET carrier_only=0,has_group=1,message_count=message_count+1",
                        [&conversation_id],
                    )?;
                }
            }
        }
    }
    crate::set_meta(
        &tx,
        "conversation_counterpart_cursor",
        &last_order.to_string(),
    )?;
    tx.commit()?;
    Ok(true)
}

pub(crate) fn converge(conn: &mut Connection, budget: usize) -> Result<bool, Error> {
    for _ in 0..budget {
        if merge_step(conn)? {
            continue;
        }
        if crate::get_meta(conn, "conversation_merge_dirty")?.as_deref() != Some("0") {
            if let Some((target, source)) = candidate_pair(conn)? {
                if eligible(conn, target)? && eligible(conn, source)? {
                    conn.execute(
                        "INSERT OR IGNORE INTO conversation_merge_jobs(source_id,target_id,cursor) VALUES(?,?,0)",
                        params![source.to_string(), target.to_string()],
                    )?;
                } else {
                    conn.execute(
                        "UPDATE conversation_counterparts SET carrier_only=0 WHERE canonical_address=(SELECT canonical_address FROM conversation_counterparts WHERE conversation_id=?)",
                        [target.to_string()],
                    )?;
                }
                continue;
            }
            crate::set_meta(conn, "conversation_merge_dirty", "0")?;
        }
        if resummarize_unresolved(conn)? {
            continue;
        }
        if !backfill_step(conn)? {
            if candidate_pair(conn)?.is_some() {
                crate::set_meta(conn, "conversation_merge_dirty", "1")?;
                continue;
            }
            return Ok(false);
        }
    }
    has_work(conn)
}

pub(crate) fn has_work(conn: &Connection) -> Result<bool, Error> {
    let pending: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM messages WHERE local_order>COALESCE((SELECT CAST(v AS INTEGER) FROM metadata WHERE k='conversation_counterpart_cursor'),0))",
        [], |row| row.get(0),
    )?;
    let jobs: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM conversation_merge_jobs)",
        [],
        |row| row.get(0),
    )?;
    let unresolved: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM conversation_counterpart_messages WHERE unresolved=1 AND generation<COALESCE((SELECT CAST(v AS INTEGER) FROM metadata WHERE k='conversation_mms_generation'),0))",
        [], |row| row.get(0),
    )?;
    Ok(pending || jobs || unresolved || candidate_pair(conn)?.is_some())
}

pub(crate) fn survivor(conn: &Connection, id: ConversationId) -> Result<ConversationId, Error> {
    let target: Option<String> = conn
        .query_row(
            "SELECT target_id FROM conversation_merges WHERE source_id=?",
            [id.to_string()],
            |row| row.get(0),
        )
        .optional()?;
    target
        .map(|target| parse(&target))
        .transpose()
        .map(|v| v.unwrap_or(id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn survivor_reads_flattened_mapping_and_preserves_unmerged_ids() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE conversation_merges(source_id TEXT PRIMARY KEY, target_id TEXT NOT NULL)",
        )
        .unwrap();
        let source = ConversationId::new();
        let target = ConversationId::new();
        assert_eq!(survivor(&conn, source).unwrap(), source);
        conn.execute(
            "INSERT INTO conversation_merges VALUES(?,?)",
            [source.to_string(), target.to_string()],
        )
        .unwrap();
        assert_eq!(survivor(&conn, source).unwrap(), target);
    }

    #[test]
    fn backfill_resumes_and_quarantines_undecodable_summaries() {
        let directory = tempfile::TempDir::new().unwrap();
        let client = crate::Client::open(
            crate::ClientConfig {
                database_path: directory.path().join("backfill.db"),
                vault_id: crate::VaultId::new(),
                device_id: crate::DeviceId::new(),
            },
            crate::DatabaseKey::new(&[3; 32]).unwrap(),
        )
        .unwrap();
        let mut store = client.lock().unwrap();
        for index in 0..(BACKFILL_ROWS + 1) {
            store.conn.execute(
                "INSERT INTO messages(id,conversation_id,source_device_id,source_sequence,payload,seen) VALUES(?,?,?,?,?,0)",
                params![format!("message-{index}"), format!("conversation-{index}"), "device", index, "{}"],
            ).unwrap();
        }
        assert!(converge(&mut store.conn, 1).unwrap());
        let count: i64 = store.conn.query_row(
            "SELECT COUNT(*) FROM conversation_counterparts WHERE carrier_only=0 AND has_group=1",
            [], |row| row.get(0),
        ).unwrap();
        assert_eq!(count, BACKFILL_ROWS);
        assert!(!converge(&mut store.conn, 1).unwrap());
        let count: i64 = store.conn.query_row(
            "SELECT COUNT(*) FROM conversation_counterparts WHERE carrier_only=0 AND has_group=1",
            [], |row| row.get(0),
        ).unwrap();
        assert_eq!(count, BACKFILL_ROWS + 1);
    }

    #[test]
    fn eligibility_requires_one_valid_carrier_counterpart() {
        let directory = tempfile::TempDir::new().unwrap();
        let client = crate::Client::open(
            crate::ClientConfig {
                database_path: directory.path().join("eligibility.db"),
                vault_id: crate::VaultId::new(),
                device_id: crate::DeviceId::new(),
            },
            crate::DatabaseKey::new(&[3; 32]).unwrap(),
        )
        .unwrap();
        let store = client.lock().unwrap();
        let id = ConversationId::new();
        store
            .conn
            .execute(
                "INSERT INTO conversation_counterparts VALUES(?,?,1,1,0,1)",
                params![id.to_string(), "+14155552671"],
            )
            .unwrap();
        assert!(eligible(&store.conn, id).unwrap());
        store.conn.execute(
            "UPDATE conversation_counterparts SET canonical_address='123' WHERE conversation_id=?",
            [id.to_string()],
        ).unwrap();
        assert!(!eligible(&store.conn, id).unwrap());
        store.conn.execute(
            "UPDATE conversation_counterparts SET canonical_address='+14155552671',counterparts=2 WHERE conversation_id=?",
            [id.to_string()],
        ).unwrap();
        assert!(!eligible(&store.conn, id).unwrap());
        store
            .conn
            .execute(
                "UPDATE conversation_counterparts SET counterparts=1 WHERE conversation_id=?",
                [id.to_string()],
            )
            .unwrap();
        store.conn.execute(
            "INSERT INTO rcs_mirror_ids(mirror_id,conversation_id,source,message_id,recorded_at_ms) VALUES('mirror',?,'link','message',1)",
            [id.to_string()],
        ).unwrap();
        assert!(!eligible(&store.conn, id).unwrap());
        store
            .conn
            .execute(
                "DELETE FROM rcs_mirror_ids WHERE conversation_id=?",
                [id.to_string()],
            )
            .unwrap();
        for address in ["+14155552671", "+14155552672"] {
            store.conn.execute(
                "INSERT INTO conversation_group_members(conversation_id,canonical_address) VALUES(?,?)",
                params![id.to_string(), address],
            ).unwrap();
        }
        assert!(!eligible(&store.conn, id).unwrap());
        assert!(!eligible(&store.conn, ConversationId::new()).unwrap());
    }

    #[test]
    fn merge_keeps_original_message_ids_and_flattens_lower_survivor() {
        let directory = tempfile::TempDir::new().unwrap();
        let client = crate::Client::open(
            crate::ClientConfig {
                database_path: directory.path().join("merge.db"),
                vault_id: crate::VaultId::new(),
                device_id: crate::DeviceId::new(),
            },
            crate::DatabaseKey::new(&[3; 32]).unwrap(),
        )
        .unwrap();
        let mut store = client.lock().unwrap();
        let mut ids = [
            ConversationId::new(),
            ConversationId::new(),
            ConversationId::new(),
        ];
        ids.sort_by_key(ToString::to_string);
        for (index, id) in ids.iter().enumerate() {
            store.conn.execute(
                "INSERT INTO messages(id,conversation_id,source_device_id,source_sequence,payload,seen) VALUES(?,?,?,?,?,0)",
                params![format!("message-{index}"), id.to_string(), "device", 1, "{}"],
            ).unwrap();
            store
                .conn
                .execute(
                    "INSERT INTO conversation_counterparts VALUES(?,?,1,1,0,1)",
                    params![id.to_string(), "+14155552671"],
                )
                .unwrap();
        }
        store
            .conn
            .execute(
                "INSERT INTO conversation_pins VALUES(?,1,1,'a','a',1)",
                [ids[2].to_string()],
            )
            .unwrap();
        store
            .conn
            .execute(
                "INSERT INTO conversation_pins VALUES(?,0,2,'b','b',2)",
                [ids[1].to_string()],
            )
            .unwrap();
        let source_draft = crate::DraftId::new();
        let target_draft = crate::DraftId::new();
        for (draft, conversation, text) in [
            (target_draft, ids[1], "survivor draft"),
            (source_draft, ids[2], "source draft"),
        ] {
            store.conn.execute(
                "INSERT INTO compose_drafts(draft_id,conversation_id,text,recipients,attachment_ids,route,revision) VALUES(?,?,?,'[]','[]',NULL,1)",
                params![draft.to_string(), conversation.to_string(), text],
            ).unwrap();
        }
        let source = ids[2].to_string();
        store
            .conn
            .execute(
                "INSERT INTO addresses(address,conversation_id) VALUES('+14155552671',?)",
                [&source],
            )
            .unwrap();
        store.conn.execute("INSERT INTO mms_thread_conversations VALUES('device','generation','sim','thread',?)", [&source]).unwrap();
        store.conn.execute("INSERT INTO banner_candidates(id,kind,conversation_id,title,body,created_at) VALUES('banner','message',?,'title','body',1)", [&source]).unwrap();
        store
            .conn
            .execute(
                "INSERT INTO conversation_folders VALUES(?,'spam',NULL,3,'writer','producer',3)",
                [&source],
            )
            .unwrap();
        store.conn.execute("INSERT INTO sms_acquisitions(acquisition_id,source_generation,subscription_id,provider_message_id,source_row_fingerprint,direction,conversation_id,input,state,reason,message_id,source_sequence,local_order) VALUES('sms','generation','sim','provider','fingerprint','incoming',?,'{}','pending',NULL,'message',7,1)", [&source]).unwrap();
        store.conn.execute("INSERT INTO mms_acquisitions(acquisition_id,source_generation,subscription_id,provider_message_id,direction,conversation_id,input,state,reason,message_id,source_sequence,local_order) VALUES('mms','generation','sim','provider','incoming',?,'{}','pending',NULL,'message',8,1)", [&source]).unwrap();
        store
            .conn
            .execute(
                "INSERT INTO conversation_merge_jobs(source_id,target_id,cursor) VALUES(?,?,0)",
                params![ids[2].to_string(), ids[1].to_string()],
            )
            .unwrap();
        while merge_step(&mut store.conn).unwrap() {}
        let preserved: i64 = store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM draft_sync_conflicts WHERE draft_id=? AND remote_draft_id=?",
                params![target_draft.to_string(), source_draft.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(preserved, 1);
        store
            .conn
            .execute(
                "INSERT INTO conversation_merge_jobs(source_id,target_id,cursor) VALUES(?,?,0)",
                params![ids[1].to_string(), ids[0].to_string()],
            )
            .unwrap();
        while merge_step(&mut store.conn).unwrap() {}
        for table in ["addresses", "mms_thread_conversations", "banner_candidates"] {
            let conversation: String = store
                .conn
                .query_row(
                    &format!("SELECT conversation_id FROM {table} LIMIT 1"),
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(conversation, ids[0].to_string(), "{table}");
        }
        for table in [
            "sms_acquisitions",
            "mms_acquisitions",
            "conversation_folders",
        ] {
            let conversation: String = store
                .conn
                .query_row(
                    &format!("SELECT conversation_id FROM {table} LIMIT 1"),
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(conversation, source, "{table}");
        }
        assert_eq!(survivor(&store.conn, ids[2]).unwrap(), ids[0]);
        let unchanged: i64 = store.conn.query_row(
            "SELECT COUNT(*) FROM messages WHERE effective_conversation_id=? AND conversation_id<>effective_conversation_id",
            [ids[0].to_string()], |row| row.get(0),
        ).unwrap();
        assert_eq!(unchanged, 2);
        let pin: (bool, i64) = store
            .conn
            .query_row(
                "SELECT pinned,logical_revision FROM conversation_pins WHERE conversation_id=?",
                [ids[1].to_string()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(pin, (false, 2));
    }

    #[test]
    fn schema_conversation_columns_are_classified() {
        let directory = tempfile::TempDir::new().unwrap();
        let client = crate::Client::open(
            crate::ClientConfig {
                database_path: directory.path().join("schema.db"),
                vault_id: crate::VaultId::new(),
                device_id: crate::DeviceId::new(),
            },
            crate::DatabaseKey::new(&[3; 32]).unwrap(),
        )
        .unwrap();
        let store = client.lock().unwrap();
        let tables: Vec<String> = store
            .conn
            .prepare(
                "SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'",
            )
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        let classified = [
            "messages",
            "conversation_counterparts",
            "conversation_counterpart_messages",
            "conversation_merge_jobs",
            "conversation_merge_job_members",
            "conversation_message_counts",
            "addresses",
            "mms_thread_conversations",
            "banner_candidates",
            "sender_unblock_requests",
            "provider_folder_writes",
            "conversation_pins",
            "conversation_folders",
            "drafts",
            "compose_drafts",
            "compose_draft_redirects",
            "sms_acquisitions",
            "mms_acquisitions",
            "rcs_mirror_ids",
            "rcs_conversation_mappings",
            "rcs_group_mapping",
            "conversation_group_members",
            // Snapshot staging and message payload readers map IDs at projection/read time.
            "projection_conversation_pins",
            "projection_conversation_folders",
            "projection_sender_unblock_requests",
            "projection_rcs_conversation_metadata",
            "local_search_fts",
            "reaction_requests",
            "message_reactions",
            "reaction_sets_staged",
            // RCS and group state is excluded by eligibility and is never rewritten.
            "rcs_mapping_proofs",
            "rcs_send_ledger",
            "rcs_link_eligibility_projection",
            "rcs_lane_ownership",
            "rcs_message_identities",
            "rcs_provider_detectors",
            "rcs_conversation_metadata",
            "conversation_group_meta",
            "rcs_group_participants",
            "rcs_group_create_ledger",
            "conversation_group_notices",
        ];
        let mut unknown = Vec::new();
        for table in tables {
            let mut statement = store
                .conn
                .prepare(&format!(
                    "PRAGMA table_info(\"{}\")",
                    table.replace('"', "\"\"")
                ))
                .unwrap();
            let columns: Vec<String> = statement
                .query_map([], |row| row.get(1))
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap();
            if columns
                .iter()
                .any(|column| column.contains("conversation_id"))
                && !classified.contains(&table.as_str())
            {
                unknown.push(table);
            }
        }
        assert!(
            unknown.is_empty(),
            "unclassified conversation tables: {unknown:?}"
        );
    }

    #[test]
    fn convergence_selects_lowest_eligible_pair() {
        let directory = tempfile::TempDir::new().unwrap();
        let client = crate::Client::open(
            crate::ClientConfig {
                database_path: directory.path().join("selection.db"),
                vault_id: crate::VaultId::new(),
                device_id: crate::DeviceId::new(),
            },
            crate::DatabaseKey::new(&[3; 32]).unwrap(),
        )
        .unwrap();
        let mut store = client.lock().unwrap();
        let mut ids = [ConversationId::new(), ConversationId::new()];
        ids.sort_by_key(ToString::to_string);
        for id in ids {
            store
                .conn
                .execute(
                    "INSERT INTO conversation_counterparts VALUES(?,?,1,1,0,1)",
                    params![id.to_string(), "+14155552671"],
                )
                .unwrap();
            store
                .conn
                .execute(
                    "INSERT INTO conversation_message_counts VALUES(?,0,0)",
                    [id.to_string()],
                )
                .unwrap();
        }
        assert!(converge(&mut store.conn, 1).unwrap());
        assert!(converge(&mut store.conn, 1).unwrap());
        assert!(!converge(&mut store.conn, 1).unwrap());
        assert_eq!(survivor(&store.conn, ids[1]).unwrap(), ids[0]);
    }

    #[test]
    fn legacy_conflict_is_visible_through_public_draft_status() {
        let directory = tempfile::TempDir::new().unwrap();
        let client = crate::Client::open(
            crate::ClientConfig {
                database_path: directory.path().join("legacy.db"),
                vault_id: crate::VaultId::new(),
                device_id: crate::DeviceId::new(),
            },
            crate::DatabaseKey::new(&[3; 32]).unwrap(),
        )
        .unwrap();
        let target = ConversationId::new();
        let source = ConversationId::new();
        let draft = {
            let mut store = client.lock().unwrap();
            store
                .conn
                .execute(
                    "INSERT INTO drafts VALUES(?,'target text',1)",
                    [target.to_string()],
                )
                .unwrap();
            store
                .conn
                .execute(
                    "INSERT INTO drafts VALUES(?,'source text',2)",
                    [source.to_string()],
                )
                .unwrap();
            merge_pair(&mut store.conn, target, source).unwrap();
            crate::load_compose_draft(&store.conn, "conversation_id", &target.to_string())
                .unwrap()
                .unwrap()
        };
        let status = client.draft_sync_status(draft.draft_id).unwrap();
        assert_eq!(draft.text, "target text");
        assert_eq!(status.conflicts.len(), 1);
        assert_eq!(status.conflicts[0].text, "source text");
    }

    #[test]
    fn job_moves_only_budgeted_rows_and_resumes_after_new_insertion() {
        let directory = tempfile::TempDir::new().unwrap();
        let client = crate::Client::open(
            crate::ClientConfig {
                database_path: directory.path().join("jobs.db"),
                vault_id: crate::VaultId::new(),
                device_id: crate::DeviceId::new(),
            },
            crate::DatabaseKey::new(&[3; 32]).unwrap(),
        )
        .unwrap();
        let mut store = client.lock().unwrap();
        let target = ConversationId::new();
        let source = ConversationId::new();
        for id in [target, source] {
            store
                .conn
                .execute(
                    "INSERT INTO conversation_counterparts VALUES(?,?,1,1,0,1)",
                    params![id.to_string(), "+14155552671"],
                )
                .unwrap();
        }
        for index in 0..(MERGE_MOVE_ROWS * 2 + 7) {
            store.conn.execute(
                "INSERT INTO messages(id,conversation_id,source_device_id,source_sequence,payload,seen) VALUES(?,?,?,?,?,0)",
                params![format!("row-{index}"),source.to_string(),"device",index,"{}"],
            ).unwrap();
        }
        store
            .conn
            .execute(
                "INSERT INTO conversation_merge_jobs(source_id,target_id) VALUES(?,?)",
                params![source.to_string(), target.to_string()],
            )
            .unwrap();
        assert!(converge(&mut store.conn, 1).unwrap());
        let moved: i64 = store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM messages WHERE effective_conversation_id=?",
                [target.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(moved, MERGE_MOVE_ROWS);
        store.conn.execute(
            "INSERT INTO messages(id,conversation_id,source_device_id,source_sequence,payload,seen) VALUES('late',?,'device',9999,'{}',0)",
            [source.to_string()],
        ).unwrap();
        for _ in 0..25 {
            if survivor(&store.conn, source).unwrap() == target {
                break;
            }
            converge(&mut store.conn, 1).unwrap();
        }
        assert_eq!(survivor(&store.conn, source).unwrap(), target);
        let count: i64 = store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM messages WHERE effective_conversation_id=?",
                [target.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, MERGE_MOVE_ROWS * 2 + 8);
    }

    #[test]
    fn mms_own_address_arrival_revisits_unresolved_summary() {
        let directory = tempfile::TempDir::new().unwrap();
        let client = crate::Client::open(
            crate::ClientConfig {
                database_path: directory.path().join("mms-own.db"),
                vault_id: crate::VaultId::new(),
                device_id: crate::DeviceId::new(),
            },
            crate::DatabaseKey::new(&[3; 32]).unwrap(),
        )
        .unwrap();
        let mut store = client.lock().unwrap();
        let device = crate::DeviceId::new();
        let conversation = ConversationId::new();
        let message = MessagePayload {
            record: crate::MessageRecord {
                message_id: crate::MessageId::new(),
                conversation_id: conversation,
                source_sequence: crate::SourceSequence(1),
                attachments: vec![],
            },
            source_device_id: device,
            provider_message_id: None,
            sender_address: Some("+14155552671".into()),
            recipients: vec!["+14155552672".into()],
            subject: None,
            body: "hi".into(),
            transport: Transport::Mms,
            direction: Direction::Incoming,
            rcs_capture: false,
            imported: false,
            sent_at_ms: None,
            ordered_at_ms: None,
            mms_context: Some(crate::MmsContext {
                source_generation: "generation".into(),
                subscription_id: "sim".into(),
                provider_thread_id: None,
            }),
        };
        store.conn.execute(
            "INSERT INTO messages(id,conversation_id,source_device_id,source_sequence,payload,seen) VALUES(?,?,?,?,?,0)",
            params![message.record.message_id.to_string(), conversation.to_string(), device.to_string(), 1, serde_json::to_string(&message).unwrap()],
        ).unwrap();
        summarize(&store.conn, &message).unwrap();
        for index in 1..=UNRESOLVED_RESUMMARIZE_ROWS {
            let mut next = message.clone();
            next.record.message_id = crate::MessageId::new();
            next.record.source_sequence = crate::SourceSequence(index as u64 + 1);
            store.conn.execute(
                "INSERT INTO messages(id,conversation_id,source_device_id,source_sequence,payload,seen) VALUES(?,?,?,?,?,0)",
                params![next.record.message_id.to_string(), conversation.to_string(), device.to_string(), index + 1, serde_json::to_string(&next).unwrap()],
            ).unwrap();
            summarize(&store.conn, &next).unwrap();
        }
        assert!(!eligible(&store.conn, conversation).unwrap());
        store.conn.execute(
            "INSERT INTO mms_own_addresses(source_device_id,subscription_id,address,revision) VALUES(?,'sim','+14155552672',1)",
            [device.to_string()],
        ).unwrap();
        assert!(resummarize_unresolved(&mut store.conn).unwrap());
        let remaining: i64 = store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM conversation_counterpart_messages WHERE unresolved=1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(remaining, 1);
        assert!(!eligible(&store.conn, conversation).unwrap());
        assert!(resummarize_unresolved(&mut store.conn).unwrap());
        assert!(eligible(&store.conn, conversation).unwrap());
        let mut second = message.clone();
        second.record.message_id = crate::MessageId::new();
        second.record.conversation_id = ConversationId::new();
        summarize(&store.conn, &second).unwrap();
        assert!(eligible(&store.conn, second.record.conversation_id).unwrap());
    }

    #[test]
    fn maintenance_is_reported_until_idle_history_converges() {
        let directory = tempfile::TempDir::new().unwrap();
        let client = crate::Client::open(
            crate::ClientConfig {
                database_path: directory.path().join("maintenance.db"),
                vault_id: crate::VaultId::new(),
                device_id: crate::DeviceId::new(),
            },
            crate::DatabaseKey::new(&[3; 32]).unwrap(),
        )
        .unwrap();
        let mut ids = [ConversationId::new(), ConversationId::new()];
        ids.sort_by_key(ToString::to_string);
        {
            let store = client.lock().unwrap();
            for id in ids {
                store
                    .conn
                    .execute(
                        "INSERT INTO conversation_counterparts VALUES(?,?,1,1,0,1)",
                        params![id.to_string(), "+14155552671"],
                    )
                    .unwrap();
                store
                    .conn
                    .execute(
                        "INSERT INTO conversation_message_counts VALUES(?,0,0)",
                        [id.to_string()],
                    )
                    .unwrap();
            }
            for index in 0..24 {
                let id = format!("maintenance-{index}");
                store.conn.execute(
                    "INSERT INTO messages(id,conversation_id,source_device_id,source_sequence,payload,seen) VALUES(?,?,?,?,?,0)",
                    params![id, ids[1].to_string(), "device", index, "{}"],
                ).unwrap();
                store
                    .conn
                    .execute(
                        "INSERT INTO conversation_counterpart_messages(message_id) VALUES(?)",
                        [&id],
                    )
                    .unwrap();
            }
        }
        for _ in 0..30 {
            if !client.sync_work_status().unwrap().pending_apply {
                break;
            }
            client.apply_pending(8).unwrap();
        }
        assert!(!client.sync_work_status().unwrap().pending_apply);
        assert_eq!(client.conversation_redirect(ids[1]).unwrap(), Some(ids[0]));
    }

    #[test]
    fn lower_survivor_retargets_an_in_progress_job() {
        let directory = tempfile::TempDir::new().unwrap();
        let client = crate::Client::open(
            crate::ClientConfig {
                database_path: directory.path().join("retarget.db"),
                vault_id: crate::VaultId::new(),
                device_id: crate::DeviceId::new(),
            },
            crate::DatabaseKey::new(&[3; 32]).unwrap(),
        )
        .unwrap();
        let mut store = client.lock().unwrap();
        let mut ids = [
            ConversationId::new(),
            ConversationId::new(),
            ConversationId::new(),
        ];
        ids.sort_by_key(ToString::to_string);
        for id in [ids[1], ids[2]] {
            store
                .conn
                .execute(
                    "INSERT INTO conversation_counterparts VALUES(?,?,1,1,0,1)",
                    params![id.to_string(), "+14155552671"],
                )
                .unwrap();
        }
        for index in 0..(MERGE_MOVE_ROWS + 1) {
            store.conn.execute(
                "INSERT INTO messages(id,conversation_id,source_device_id,source_sequence,payload,seen) VALUES(?,?,?,?,?,0)",
                params![format!("retarget-{index}"), ids[2].to_string(), "device", index, "{}"],
            ).unwrap();
        }
        store
            .conn
            .execute(
                "INSERT INTO conversation_merge_jobs(source_id,target_id) VALUES(?,?)",
                params![ids[2].to_string(), ids[1].to_string()],
            )
            .unwrap();
        assert!(merge_step(&mut store.conn).unwrap());
        store
            .conn
            .execute(
                "INSERT INTO conversation_counterparts VALUES(?,?,1,1,0,1)",
                params![ids[0].to_string(), "+14155552671"],
            )
            .unwrap();
        store
            .conn
            .execute(
                "INSERT INTO conversation_message_counts VALUES(?,0,0)",
                [ids[0].to_string()],
            )
            .unwrap();
        assert!(merge_step(&mut store.conn).unwrap());
        let target: String = store
            .conn
            .query_row(
                "SELECT target_id FROM conversation_merge_jobs WHERE source_id=?",
                [ids[2].to_string()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(target, ids[0].to_string());
        while merge_step(&mut store.conn).unwrap() {}
        assert_eq!(survivor(&store.conn, ids[2]).unwrap(), ids[0]);
        let count: i64 = store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM messages WHERE effective_conversation_id=?",
                [ids[0].to_string()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, MERGE_MOVE_ROWS + 1);
    }

    #[test]
    fn ineligible_source_reverts_partial_job_before_publishing() {
        let directory = tempfile::TempDir::new().unwrap();
        let client = crate::Client::open(
            crate::ClientConfig {
                database_path: directory.path().join("revert.db"),
                vault_id: crate::VaultId::new(),
                device_id: crate::DeviceId::new(),
            },
            crate::DatabaseKey::new(&[3; 32]).unwrap(),
        )
        .unwrap();
        let mut store = client.lock().unwrap();
        let target = ConversationId::new();
        let source = ConversationId::new();
        for id in [target, source] {
            store
                .conn
                .execute(
                    "INSERT INTO conversation_counterparts VALUES(?,?,1,1,0,1)",
                    params![id.to_string(), "+14155552671"],
                )
                .unwrap();
        }
        for index in 0..(MERGE_MOVE_ROWS + 1) {
            store.conn.execute(
                "INSERT INTO messages(id,conversation_id,source_device_id,source_sequence,payload,seen) VALUES(?,?,'device',?,'{}',0)",
                params![format!("revert-{index}"), source.to_string(), index],
            ).unwrap();
        }
        store
            .conn
            .execute(
                "INSERT INTO conversation_merge_jobs(source_id,target_id) VALUES(?,?)",
                params![source.to_string(), target.to_string()],
            )
            .unwrap();
        assert!(merge_step(&mut store.conn).unwrap());
        store
            .conn
            .execute(
                "UPDATE conversation_counterparts SET has_group=1 WHERE conversation_id=?",
                [source.to_string()],
            )
            .unwrap();
        while merge_step(&mut store.conn).unwrap() {}
        assert_eq!(survivor(&store.conn, source).unwrap(), source);
        let restored: i64 = store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM messages WHERE effective_conversation_id=?",
                [source.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(restored, MERGE_MOVE_ROWS + 1);
    }

    #[test]
    fn final_chunk_publishes_redirect_before_source_disappears() {
        let directory = tempfile::TempDir::new().unwrap();
        let client = crate::Client::open(
            crate::ClientConfig {
                database_path: directory.path().join("atomic-final.db"),
                vault_id: crate::VaultId::new(),
                device_id: crate::DeviceId::new(),
            },
            crate::DatabaseKey::new(&[3; 32]).unwrap(),
        )
        .unwrap();
        let target = ConversationId::new();
        let source = ConversationId::new();
        {
            let store = client.lock().unwrap();
            for id in [target, source] {
                store
                    .conn
                    .execute(
                        "INSERT INTO conversation_counterparts VALUES(?,?,1,1,0,1)",
                        params![id.to_string(), "+14155552671"],
                    )
                    .unwrap();
            }
            for index in 0..(MERGE_MOVE_ROWS + 1) {
                store.conn.execute(
                    "INSERT INTO messages(id,conversation_id,source_device_id,source_sequence,payload,seen) VALUES(?,?,'device',?,'{}',0)",
                    params![format!("atomic-{index}"), source.to_string(), index],
                ).unwrap();
            }
            store
                .conn
                .execute(
                    "INSERT INTO conversation_merge_jobs(source_id,target_id) VALUES(?,?)",
                    params![source.to_string(), target.to_string()],
                )
                .unwrap();
        }
        for _ in 0..8 {
            let listed = client
                .list_conversations()
                .unwrap()
                .iter()
                .any(|item| item.conversation_id == source);
            let redirect = client.conversation_redirect(source).unwrap();
            assert!(listed || redirect == Some(target));
            if redirect.is_some() {
                break;
            }
            let mut store = client.lock().unwrap();
            assert!(merge_step(&mut store.conn).unwrap());
        }
        assert_eq!(client.conversation_redirect(source).unwrap(), Some(target));
        assert!(
            !client
                .list_conversations()
                .unwrap()
                .iter()
                .any(|item| item.conversation_id == source)
        );
    }

    #[test]
    fn member_page_uses_conversation_order_index_without_sorting() {
        let directory = tempfile::TempDir::new().unwrap();
        let client = crate::Client::open(
            crate::ClientConfig {
                database_path: directory.path().join("query-plan.db"),
                vault_id: crate::VaultId::new(),
                device_id: crate::DeviceId::new(),
            },
            crate::DatabaseKey::new(&[3; 32]).unwrap(),
        )
        .unwrap();
        let store = client.lock().unwrap();
        let plan: Vec<String> = store
            .conn
            .prepare(&format!("EXPLAIN QUERY PLAN {MERGE_MEMBER_PAGE_SQL}"))
            .unwrap()
            .query_map(params!["conversation", 0, MERGE_MOVE_ROWS], |row| {
                row.get(3)
            })
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert!(
            plan.iter()
                .any(|line| line.contains("messages_conversation")),
            "{plan:?}"
        );
        assert!(
            !plan.iter().any(|line| line.contains("TEMP B-TREE")),
            "{plan:?}"
        );
    }
}
