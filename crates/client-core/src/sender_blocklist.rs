//! Synced blocked-sender lists, phone-observed messaging-app folder changes and durable unblock work.
//!
//! The gateway mirrors its messaging app by writing ordinary `conversation_folder` events on
//! observed edges (a thread archived, a number blocked). It only reverts folder states it wrote
//! itself (`provider_folder_writes`), so a user's own folder choice is never undone.
use super::*;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use std::collections::BTreeSet;

const CLOCK: &str = "sender_blocklist_clock";
const FOLDER_CLOCK: &str = "conversation_folder_clock";
const ARCHIVE_GENERATIONS: &str = "provider_archive_generations";
pub(crate) const MAX_NUMBERS: usize = 5000;
pub(crate) const MAX_NUMBER_BYTES: usize = 64;
/// An unanswered unblock request is actionable for a week, like contact edit requests.
const REQUEST_TTL_MS: i64 = 7 * 24 * 60 * 60 * 1000;

pub(crate) fn outcome_code(outcome: SenderUnblockOutcome) -> &'static str {
    match outcome {
        SenderUnblockOutcome::Unblocked => "unblocked",
        SenderUnblockOutcome::NotBlocked => "not_blocked",
        SenderUnblockOutcome::NotAllowed => "not_allowed",
        SenderUnblockOutcome::Failed => "failed",
    }
}

fn valid_numbers(numbers: &[String]) -> bool {
    numbers.len() <= MAX_NUMBERS
        && numbers
            .iter()
            .all(|n| !n.is_empty() && n.len() <= MAX_NUMBER_BYTES)
}

pub(crate) fn valid_blocklist(
    numbers: &[String],
    revision: u64,
    writer: &str,
    producer: &str,
) -> bool {
    revision > 0
        && revision < i64::MAX as u64
        && writer == producer
        && uuid::Uuid::parse_str(writer).is_ok()
        && valid_numbers(numbers)
        && numbers.windows(2).all(|w| w[0] < w[1])
}

pub(crate) fn valid_request(
    conversation_id: ConversationId,
    numbers: &[String],
    target: &str,
    writer: &str,
    producer: &str,
) -> bool {
    !conversation_id.0.is_nil()
        && writer == producer
        && uuid::Uuid::parse_str(target).is_ok()
        && !numbers.is_empty()
        && valid_numbers(numbers)
        && numbers.windows(2).all(|w| w[0] < w[1])
}

pub(crate) fn apply_blocklist(
    conn: &Connection,
    numbers: &[String],
    can_unblock: bool,
    revision: u64,
    writer: &str,
    producer: &str,
    sequence: u64,
) -> Result<(), Error> {
    let revision = to_i64(revision)?;
    let sequence = to_i64(sequence)?;
    let clock = get_meta(conn, CLOCK)?
        .and_then(|v| v.parse().ok())
        .unwrap_or(0_i64);
    set_meta(conn, CLOCK, &clock.max(revision).to_string())?;
    conn.execute("INSERT INTO sender_blocklists(producer_device_id,numbers,can_unblock,logical_revision,writer_device_id,source_sequence) VALUES(?,?,?,?,?,?) ON CONFLICT(producer_device_id) DO UPDATE SET numbers=excluded.numbers,can_unblock=excluded.can_unblock,logical_revision=excluded.logical_revision,writer_device_id=excluded.writer_device_id,source_sequence=excluded.source_sequence WHERE excluded.logical_revision>sender_blocklists.logical_revision OR (excluded.logical_revision=sender_blocklists.logical_revision AND excluded.writer_device_id>sender_blocklists.writer_device_id)", params![producer, json(&numbers)?, can_unblock, revision, writer, sequence])?;
    Ok(())
}

pub(crate) fn apply_request(
    conn: &Connection,
    request_id: uuid::Uuid,
    conversation_id: ConversationId,
    numbers: &[String],
    target: &str,
    historical: bool,
) -> Result<(), Error> {
    conn.execute("INSERT INTO sender_unblock_requests(request_id,conversation_id,target_device_id,numbers,status,historical,created_at_ms) VALUES(?,?,?,?,'pending',?,?) ON CONFLICT(request_id) DO NOTHING", params![request_id.to_string(), conversation_id.to_string(), target, json(&numbers)?, historical, notifications::now_ms()])?;
    Ok(())
}

pub(crate) fn apply_result(
    conn: &Connection,
    request_id: uuid::Uuid,
    outcome: SenderUnblockOutcome,
    producer: &str,
) -> Result<(), Error> {
    conn.execute("UPDATE sender_unblock_requests SET status=? WHERE request_id=? AND target_device_id=? AND status='pending'", params![outcome_code(outcome), request_id.to_string(), producer])?;
    Ok(())
}

fn effective_folder(conn: &Connection, id: ConversationId) -> Result<ConversationFolder, Error> {
    let folder: String = conn.query_row(
        &format!("SELECT {} FROM (SELECT MAX(sort_at_ms) AS sort_at_ms FROM messages WHERE conversation_id=?1) c LEFT JOIN conversation_folders f ON f.conversation_id=?1", conversation_folders::effective_folder_sql("c.sort_at_ms")),
        [id.to_string()],
        |r| r.get(0),
    )?;
    ConversationFolder::from_db(&folder)
}

/// Whether the current folder row is the one this gateway wrote from a provider observation.
fn provider_owned(
    conn: &Connection,
    id: ConversationId,
    folder: ConversationFolder,
) -> Result<bool, Error> {
    Ok(conn.query_row("SELECT EXISTS(SELECT 1 FROM provider_folder_writes w JOIN conversation_folders f ON f.conversation_id=w.conversation_id AND f.logical_revision=w.logical_revision AND f.writer_device_id=w.writer_device_id WHERE w.conversation_id=? AND w.folder=?)", params![id.to_string(), folder.as_str()], |r| r.get(0))?)
}

/// Emits and applies a folder event; provider writes are recorded so they may later be reverted.
fn write_folder(
    conn: &Connection,
    ctx: &Ctx<'_>,
    id: ConversationId,
    folder: ConversationFolder,
    provider: bool,
) -> Result<(), Error> {
    let current: Option<i64> = conn
        .query_row(
            "SELECT logical_revision FROM conversation_folders WHERE conversation_id=?",
            [id.to_string()],
            |r| r.get(0),
        )
        .optional()?;
    let clock = get_meta(conn, FOLDER_CLOCK)?
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(0);
    let base = clock.max(current.and_then(|v| u64::try_from(v).ok()).unwrap_or(0));
    if base >= i64::MAX as u64 - 1 {
        return Err(Error::InvalidRequest("conversation folder revision"));
    }
    let revision = base + 1;
    let archived_through_ms: Option<i64> = if folder == ConversationFolder::Archived {
        conn.query_row(
            "SELECT MAX(sort_at_ms) FROM messages WHERE conversation_id=?",
            [id.to_string()],
            |r| r.get(0),
        )?
    } else {
        None
    };
    set_meta(conn, FOLDER_CLOCK, &revision.to_string())?;
    let writer = ctx.device_id.to_string();
    let (_, sequence) = enqueue(
        conn,
        ctx,
        EnvelopePurpose::Event,
        None,
        None,
        &PrivatePayload::ConversationFolder {
            conversation_id: id,
            folder,
            archived_through_ms,
            logical_revision: revision,
            writer_device_id: writer.clone(),
        },
    )?;
    conversation_folders::apply(
        conn,
        id,
        folder,
        archived_through_ms,
        revision,
        &writer,
        &writer,
        sequence,
    )?;
    if provider {
        conn.execute("INSERT INTO provider_folder_writes(conversation_id,folder,logical_revision,writer_device_id) VALUES(?,?,?,?) ON CONFLICT(conversation_id) DO UPDATE SET folder=excluded.folder,logical_revision=excluded.logical_revision,writer_device_id=excluded.writer_device_id", params![id.to_string(), folder.as_str(), to_i64(revision)?, writer])?;
    }
    Ok(())
}

/// Canonical addresses of a 1:1 conversation; empty for groups and MMS-only threads.
fn peer_numbers(
    conn: &Connection,
    region: &str,
    id: ConversationId,
) -> Result<BTreeSet<String>, Error> {
    let raw: Vec<String> = conn
        .prepare("SELECT address FROM addresses WHERE conversation_id=?")?
        .query_map([id.to_string()], |r| r.get(0))?
        .collect::<Result<_, _>>()?;
    Ok(raw
        .iter()
        .map(|a| conversation_match::canonical_address(a, Some(region)))
        .collect())
}

fn listed_numbers(conn: &Connection, only_unblockable: bool) -> Result<BTreeSet<String>, Error> {
    let sql = if only_unblockable {
        "SELECT DISTINCT n.value FROM sender_blocklists b, json_each(b.numbers) n WHERE b.can_unblock=1"
    } else {
        "SELECT DISTINCT n.value FROM sender_blocklists b, json_each(b.numbers) n"
    };
    Ok(conn
        .prepare(sql)?
        .query_map([], |r| r.get(0))?
        .collect::<Result<_, _>>()?)
}

fn is_blocked(peers: &BTreeSet<String>, listed: &BTreeSet<String>) -> bool {
    !peers.is_empty() && peers.is_subset(listed)
}

/// Per-call spam context for listing conversations: each set is read at most once.
#[derive(Default)]
pub(crate) struct FolderContext {
    region: Option<String>,
    listed: Option<BTreeSet<String>>,
    unblockable: Option<BTreeSet<String>>,
    unblock: Option<std::collections::HashMap<String, SenderUnblockStatus>>,
}

impl FolderContext {
    fn unblock_status(
        &mut self,
        conn: &Connection,
        id: ConversationId,
    ) -> Result<Option<SenderUnblockStatus>, Error> {
        if self.unblock.is_none() {
            let rows: Vec<(String, String)> = conn
                .prepare("SELECT conversation_id,status FROM (SELECT conversation_id,status,created_at_ms,historical,ROW_NUMBER() OVER (PARTITION BY conversation_id ORDER BY created_at_ms DESC,rowid DESC) AS newest FROM sender_unblock_requests) WHERE newest=1 AND (status<>'pending' OR (historical=0 AND created_at_ms>?))")?
                .query_map([notifications::now_ms() - REQUEST_TTL_MS], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<Result<_, _>>()?;
            self.unblock = Some(
                rows.into_iter()
                    .filter_map(|(id, status)| match status.as_str() {
                        "pending" => Some((id, SenderUnblockStatus::Pending)),
                        "failed" | "not_allowed" => Some((id, SenderUnblockStatus::Failed)),
                        _ => None,
                    })
                    .collect(),
            );
        }
        Ok(self
            .unblock
            .as_ref()
            .and_then(|map| map.get(&id.to_string()).copied()))
    }

    /// Reason, unblock status and unblock capability for one listed conversation.
    pub(crate) fn state(
        &mut self,
        conn: &Connection,
        id: ConversationId,
        folder: ConversationFolder,
    ) -> Result<
        (
            Option<ConversationFolderReason>,
            Option<SenderUnblockStatus>,
            bool,
        ),
        Error,
    > {
        let unblock = self.unblock_status(conn, id)?;
        if folder != ConversationFolder::Spam {
            return Ok((None, unblock, false));
        }
        if self.region.is_none() {
            self.region = Some(conversation_match::identity_region(conn)?);
            self.listed = Some(listed_numbers(conn, false)?);
            self.unblockable = Some(listed_numbers(conn, true)?);
        }
        let peers = peer_numbers(conn, self.region.as_deref().unwrap_or_default(), id)?;
        if !self
            .listed
            .as_ref()
            .is_some_and(|listed| is_blocked(&peers, listed))
        {
            return Ok((Some(ConversationFolderReason::Marked), unblock, false));
        }
        let can_unblock = self
            .unblockable
            .as_ref()
            .is_some_and(|listed| is_blocked(&peers, listed));
        Ok((
            Some(ConversationFolderReason::Blocked),
            unblock,
            can_unblock,
        ))
    }
}

fn conversations_for_number(conn: &Connection, number: &str) -> Result<BTreeSet<String>, Error> {
    Ok(conn
        .prepare("SELECT DISTINCT conversation_id FROM addresses WHERE address=?")?
        .query_map([number], |r| r.get(0))?
        .collect::<Result<_, _>>()?)
}

/// Moves conversations of newly listed numbers to Spam and returns gateway-written Spam to Inbox
/// for numbers no longer listed. A listed number with no conversation yet is remembered so the
/// edge applies once its conversation is captured or imported.
fn apply_blocklist_edges(
    conn: &Connection,
    ctx: &Ctx<'_>,
    added: &BTreeSet<String>,
    removed: &BTreeSet<String>,
) -> Result<u32, Error> {
    let region = conversation_match::identity_region(conn)?;
    let listed = listed_numbers(conn, false)?;
    let mut emitted = 0;
    for number in added {
        let conversations = conversations_for_number(conn, number)?;
        if conversations.is_empty() {
            conn.execute(
                "INSERT OR IGNORE INTO sender_blocklist_unmapped(number) VALUES(?)",
                [number],
            )?;
            continue;
        }
        conn.execute(
            "DELETE FROM sender_blocklist_unmapped WHERE number=?",
            [number],
        )?;
        for raw in conversations {
            let id: ConversationId = parse(&raw)?;
            if is_blocked(&peer_numbers(conn, &region, id)?, &listed)
                && effective_folder(conn, id)? != ConversationFolder::Spam
            {
                write_folder(conn, ctx, id, ConversationFolder::Spam, true)?;
                emitted += 1;
            }
        }
    }
    for number in removed {
        conn.execute(
            "DELETE FROM sender_blocklist_unmapped WHERE number=?",
            [number],
        )?;
        for raw in conversations_for_number(conn, number)? {
            let id: ConversationId = parse(&raw)?;
            if !is_blocked(&peer_numbers(conn, &region, id)?, &listed)
                && effective_folder(conn, id)? == ConversationFolder::Spam
                && provider_owned(conn, id, ConversationFolder::Spam)?
            {
                write_folder(conn, ctx, id, ConversationFolder::Inbox, true)?;
                emitted += 1;
            }
        }
    }
    Ok(emitted)
}

/// Listed numbers whose conversation has appeared since they were listed.
fn newly_mapped_numbers(conn: &Connection) -> Result<BTreeSet<String>, Error> {
    Ok(conn
        .prepare("SELECT u.number FROM sender_blocklist_unmapped u WHERE EXISTS(SELECT 1 FROM addresses a WHERE a.address=u.number)")?
        .query_map([], |r| r.get(0))?
        .collect::<Result<_, _>>()?)
}

/// This phone's current SMS and MMS provider generations: thread ids from an earlier provider
/// generation may have been reused.
fn current_generations(conn: &Connection) -> Result<Vec<String>, Error> {
    Ok(conn
        .prepare("SELECT source_generation FROM (SELECT source_generation FROM sms_acquisitions ORDER BY local_order DESC LIMIT 1) UNION SELECT source_generation FROM (SELECT source_generation FROM mms_acquisitions ORDER BY local_order DESC LIMIT 1) ORDER BY 1")?
        .query_map([], |r| r.get(0))?
        .collect::<Result<_, _>>()?)
}

/// The conversation this phone linked to a provider thread in a current source generation.
fn thread_conversation(
    conn: &Connection,
    device: &str,
    generations: &str,
    thread: &str,
) -> Result<Option<ConversationId>, Error> {
    let raw: Option<String> = conn
        .query_row(
            "SELECT conversation_id FROM mms_thread_conversations WHERE source_device_id=?1 AND provider_thread_id=?2 AND source_generation IN (SELECT value FROM json_each(?3)) LIMIT 1",
            params![device, thread, generations],
            |r| r.get(0),
        )
        .optional()?;
    raw.map(|id| parse(&id)).transpose()
}

fn require_keys(ctx: &Ctx<'_>) -> Result<(), Error> {
    if ctx.active_epoch.is_none_or(|e| !ctx.keys.contains_key(&e)) {
        return Err(Error::KeysUnavailable);
    }
    Ok(())
}

fn work_rows(rows: Vec<(String, String)>) -> Result<Vec<SenderUnblockWork>, Error> {
    rows.into_iter()
        .map(|(request_id, numbers)| {
            Ok(SenderUnblockWork {
                request_id,
                numbers: serde_json::from_str(&numbers).map_err(|_| Error::Database)?,
            })
        })
        .collect()
}

fn own_list(
    conn: &Connection,
    device: &str,
) -> Result<Option<(BTreeSet<String>, bool, u64)>, Error> {
    let row: Option<(String, bool, i64)> = conn
        .query_row("SELECT numbers,can_unblock,logical_revision FROM sender_blocklists WHERE producer_device_id=?", [device], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .optional()?;
    row.map(|(numbers, can_unblock, revision)| {
        Ok((
            serde_json::from_str(&numbers).map_err(|_| Error::Database)?,
            can_unblock,
            u64::try_from(revision).map_err(|_| Error::Database)?,
        ))
    })
    .transpose()
}

fn emit_result(
    conn: &Connection,
    ctx: &Ctx<'_>,
    request: uuid::Uuid,
    outcome: SenderUnblockOutcome,
) -> Result<(), Error> {
    let device = ctx.device_id.to_string();
    conn.execute(
        "UPDATE sender_unblock_attempts SET completed=1 WHERE request_id=?",
        [request.to_string()],
    )?;
    apply_result(conn, request, outcome, &device)?;
    enqueue(
        conn,
        ctx,
        EnvelopePurpose::Event,
        None,
        None,
        &PrivatePayload::SenderUnblockResult {
            request_id: request,
            outcome,
            writer_device_id: device,
        },
    )?;
    Ok(())
}

impl Client {
    /// Publishes this gateway's blocked-number list (`None` keeps the previous numbers, for when
    /// read access was lost) and moves conversations whose only peer was newly blocked to Spam.
    /// Returns `false` when the published list itself is unchanged.
    pub fn publish_sender_blocklist(
        &self,
        numbers: Option<Vec<String>>,
        can_unblock: bool,
    ) -> Result<bool, Error> {
        let mut guard = self.lock()?;
        let (conn, ctx) = guard.parts();
        require_keys(&ctx)?;
        let device = ctx.device_id.to_string();
        let previous = own_list(conn, &device)?;
        let old = previous
            .as_ref()
            .map(|(numbers, _, _)| numbers.clone())
            .unwrap_or_default();
        let new: BTreeSet<String> = match numbers {
            Some(numbers) => {
                let region = conversation_match::identity_region(conn)?;
                numbers
                    .iter()
                    .map(|n| conversation_match::canonical_address(n, Some(&region)))
                    .filter(|n| !n.is_empty())
                    .collect()
            }
            None => old.clone(),
        };
        let values: Vec<String> = new.iter().cloned().collect();
        if !valid_numbers(&values) {
            return Err(Error::InvalidRequest("sender blocklist"));
        }
        let unchanged = match &previous {
            Some((_, previous_can_unblock, _)) => {
                *previous_can_unblock == can_unblock && old == new
            }
            None => values.is_empty() && !can_unblock,
        };
        let late = newly_mapped_numbers(conn)?;
        if unchanged && late.is_empty() {
            return Ok(false);
        }
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if !unchanged {
            let clock = get_meta(&tx, CLOCK)?
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(0);
            let base = clock.max(previous.as_ref().map_or(0, |(_, _, revision)| *revision));
            if base >= i64::MAX as u64 - 1 {
                return Err(Error::InvalidRequest("sender blocklist revision"));
            }
            let revision = base + 1;
            let (_, sequence) = enqueue(
                &tx,
                &ctx,
                EnvelopePurpose::Event,
                None,
                None,
                &PrivatePayload::SenderBlocklist {
                    numbers: values.clone(),
                    can_unblock,
                    logical_revision: revision,
                    writer_device_id: device.clone(),
                },
            )?;
            apply_blocklist(
                &tx,
                &values,
                can_unblock,
                revision,
                &device,
                &device,
                sequence,
            )?;
        }
        let mut added: BTreeSet<String> = new.difference(&old).cloned().collect();
        added.extend(late.intersection(&new).cloned());
        let removed = old.difference(&new).cloned().collect();
        apply_blocklist_edges(&tx, &ctx, &added, &removed)?;
        tx.commit()?;
        Ok(!unchanged)
    }

    /// Mirrors the messaging app's archived threads: a newly archived thread archives its Inbox
    /// conversation; a thread leaving the archive returns it to Inbox only if this gateway archived it.
    /// A thread is observed only once it maps to a conversation, so threads captured or imported
    /// later still archive. Returns the number of folder events emitted.
    pub fn reconcile_provider_archive(
        &self,
        archived_thread_ids: Vec<String>,
    ) -> Result<u32, Error> {
        let mut guard = self.lock()?;
        let (conn, ctx) = guard.parts();
        require_keys(&ctx)?;
        let device = ctx.device_id.to_string();
        let current: BTreeSet<String> = current_generations(conn)?.into_iter().collect();
        let generations = json(&current)?;
        let stored: BTreeSet<String> = match get_meta(conn, ARCHIVE_GENERATIONS)? {
            Some(value) => serde_json::from_str(&value).map_err(|_| Error::Database)?,
            None => BTreeSet::new(),
        };
        let generation_changed = stored != current;
        // A provider reset replaces a generation, and its thread ids may repeat old ones. A first
        // MMS (or SMS) generation joining is not a reset. After a reset, conversations this gateway
        // archived earlier are left as they are: old thread ids can no longer identify them.
        let provider_reset = !stored.is_subset(&current);
        let prior: BTreeSet<String> = if provider_reset {
            BTreeSet::new()
        } else {
            conn.prepare("SELECT thread_id FROM provider_archive_observed")?
                .query_map([], |r| r.get(0))?
                .collect::<Result<_, _>>()?
        };
        let mut mapped = std::collections::BTreeMap::new();
        for thread in archived_thread_ids
            .into_iter()
            .filter(|t| !t.is_empty() && t.len() <= MAX_PROVIDER_ID_BYTES)
        {
            if prior.contains(&thread) {
                mapped.insert(thread, None);
            } else if let Some(id) = thread_conversation(conn, &device, &generations, &thread)? {
                mapped.insert(thread, Some(id));
            }
        }
        let incoming: BTreeSet<String> = mapped.keys().cloned().collect();
        if incoming == prior && !generation_changed {
            return Ok(0);
        }
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if provider_reset {
            tx.execute("DELETE FROM provider_archive_observed", [])?;
        }
        if generation_changed {
            set_meta(&tx, ARCHIVE_GENERATIONS, &generations)?;
        }
        let mut emitted = 0;
        for (thread, id) in &mapped {
            let Some(id) = id else { continue };
            if effective_folder(&tx, *id)? == ConversationFolder::Inbox {
                write_folder(&tx, &ctx, *id, ConversationFolder::Archived, true)?;
                emitted += 1;
            }
            tx.execute(
                "INSERT OR IGNORE INTO provider_archive_observed(thread_id) VALUES(?)",
                [thread],
            )?;
        }
        for thread in prior.difference(&incoming) {
            if let Some(id) = thread_conversation(&tx, &device, &generations, thread)?
                && effective_folder(&tx, id)? == ConversationFolder::Archived
                && provider_owned(&tx, id, ConversationFolder::Archived)?
            {
                write_folder(&tx, &ctx, id, ConversationFolder::Inbox, true)?;
                emitted += 1;
            }
            tx.execute(
                "DELETE FROM provider_archive_observed WHERE thread_id=?",
                [thread],
            )?;
        }
        tx.commit()?;
        Ok(emitted)
    }

    /// Asks the phone that listed this conversation's sender as blocked to unblock it, and moves
    /// the conversation to Inbox in the same transaction.
    pub fn request_sender_unblock(&self, conversation_id: ConversationId) -> Result<(), Error> {
        let mut guard = self.lock()?;
        let (conn, ctx) = guard.parts();
        require_keys(&ctx)?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if effective_folder(&tx, conversation_id)? != ConversationFolder::Spam {
            return Err(Error::InvalidRequest("sender unblock"));
        }
        let region = conversation_match::identity_region(&tx)?;
        let peers = peer_numbers(&tx, &region, conversation_id)?;
        if !is_blocked(&peers, &listed_numbers(&tx, true)?) {
            return Err(Error::InvalidRequest("sender unblock"));
        }
        let lists: Vec<(String, String)> = tx
            .prepare("SELECT producer_device_id,numbers FROM sender_blocklists WHERE can_unblock=1 ORDER BY logical_revision DESC,writer_device_id DESC")?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<Result<_, _>>()?;
        let target = lists
            .into_iter()
            .find(|(_, numbers)| {
                serde_json::from_str::<BTreeSet<String>>(numbers)
                    .is_ok_and(|listed| peers.is_subset(&listed))
            })
            .map(|(producer, _)| producer)
            .ok_or(Error::InvalidRequest("sender unblock"))?;
        let request_id = uuid::Uuid::new_v4();
        let numbers: Vec<String> = peers.into_iter().collect();
        apply_request(&tx, request_id, conversation_id, &numbers, &target, false)?;
        enqueue(
            &tx,
            &ctx,
            EnvelopePurpose::Event,
            None,
            None,
            &PrivatePayload::SenderUnblockRequest {
                request_id,
                conversation_id,
                numbers,
                target_device_id: target,
                writer_device_id: ctx.device_id.to_string(),
            },
        )?;
        write_folder(&tx, &ctx, conversation_id, ConversationFolder::Inbox, false)?;
        tx.commit()?;
        Ok(())
    }

    /// Live, unexpired unblock requests addressed to this phone that have not been started.
    pub fn pending_sender_unblocks(&self) -> Result<Vec<SenderUnblockWork>, Error> {
        let s = self.lock()?;
        let rows: Vec<(String, String)> = s
            .conn
            .prepare("SELECT r.request_id,r.numbers FROM sender_unblock_requests r WHERE r.target_device_id=?1 AND r.historical=0 AND r.status='pending' AND r.created_at_ms>?2 AND NOT EXISTS(SELECT 1 FROM sender_unblock_attempts a WHERE a.request_id=r.request_id) ORDER BY r.created_at_ms,r.rowid")?
            .query_map(params![s.config.device_id.to_string(), notifications::now_ms() - REQUEST_TTL_MS], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<Result<_, _>>()?;
        work_rows(rows)
    }

    /// Takes the one-use permit for a request. Returns `false` when it was already started or is
    /// not actionable; a live request this phone may not carry out (it cannot unblock, or a
    /// number is not on its own list) is answered `NotAllowed` instead of being started.
    pub fn begin_sender_unblock(&self, request_id: &str) -> Result<bool, Error> {
        let request = uuid::Uuid::parse_str(request_id)
            .map_err(|_| Error::InvalidRequest("sender unblock"))?;
        let mut guard = self.lock()?;
        let (conn, ctx) = guard.parts();
        let device = ctx.device_id.to_string();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let numbers: Option<String> = tx
            .query_row("SELECT r.numbers FROM sender_unblock_requests r WHERE r.request_id=? AND r.target_device_id=? AND r.historical=0 AND r.status='pending' AND r.created_at_ms>? AND NOT EXISTS(SELECT 1 FROM sender_unblock_attempts a WHERE a.request_id=r.request_id)", params![request_id, device, notifications::now_ms() - REQUEST_TTL_MS], |r| r.get(0))
            .optional()?;
        let Some(numbers) = numbers else {
            return Ok(false);
        };
        let numbers: BTreeSet<String> =
            serde_json::from_str(&numbers).map_err(|_| Error::Database)?;
        tx.execute(
            "INSERT INTO sender_unblock_attempts(request_id,begun_at_ms,completed) VALUES(?,?,0)",
            params![request_id, notifications::now_ms()],
        )?;
        let refusal = match own_list(&tx, &device)? {
            Some((_, false, _)) | None => Some(SenderUnblockOutcome::NotAllowed),
            // Unblocked in the messaging app since the request was made.
            Some((listed, true, _)) if !numbers.is_subset(&listed) => {
                Some(SenderUnblockOutcome::NotBlocked)
            }
            Some(_) => None,
        };
        if let Some(outcome) = refusal {
            require_keys(&ctx)?;
            emit_result(&tx, &ctx, request, outcome)?;
        }
        tx.commit()?;
        Ok(refusal.is_none())
    }

    /// Started requests without a recorded outcome; reconcile them, never repeat the unblock.
    pub fn interrupted_sender_unblocks(&self) -> Result<Vec<SenderUnblockWork>, Error> {
        let s = self.lock()?;
        let rows: Vec<(String, String)> = s
            .conn
            .prepare("SELECT r.request_id,r.numbers FROM sender_unblock_requests r JOIN sender_unblock_attempts a ON a.request_id=r.request_id WHERE r.target_device_id=? AND r.status='pending' AND a.completed=0 ORDER BY a.begun_at_ms")?
            .query_map([s.config.device_id.to_string()], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<Result<_, _>>()?;
        work_rows(rows)
    }

    /// Records the outcome of a started request; fails for a request that was never begun.
    pub fn complete_sender_unblock(
        &self,
        request_id: &str,
        outcome: SenderUnblockOutcome,
    ) -> Result<(), Error> {
        let request = uuid::Uuid::parse_str(request_id)
            .map_err(|_| Error::InvalidRequest("sender unblock"))?;
        let mut guard = self.lock()?;
        let (conn, ctx) = guard.parts();
        require_keys(&ctx)?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let started: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM sender_unblock_attempts a JOIN sender_unblock_requests r ON r.request_id=a.request_id WHERE a.request_id=? AND a.completed=0 AND r.status='pending' AND r.target_device_id=?)", params![request_id, ctx.device_id.to_string()], |r| r.get(0))?;
        if !started {
            return Err(Error::InvalidRequest("sender unblock"));
        }
        emit_result(&tx, &ctx, request, outcome)?;
        tx.commit()?;
        Ok(())
    }
}
