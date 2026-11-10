//! Create-on-first-send for RCS groups (M4 C4c). One ledger row per Peppy conversation records the
//! single-use create permit and its outcome. A begun permit with no recorded outcome is unknown,
//! and an unknown outcome blocks further creates until reconciliation. Google identifiers stay in
//! this gateway-local table (I6).

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use peppy_domain::{ConversationId, SendRefusalReason};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Deserialize;

use crate::{
    CommandId, Ctx, Error, GroupCreateRequest, GroupIngestReport,
    rcs_group::{self, GroupMappingState, GroupProof},
    rcs_mapping,
    rcs_target::ConversationCandidateRecord,
};

/// Identifies this process's create operations. A begun row carries the token of the process that
/// began it; only a different token proves that the operation was interrupted.
pub(crate) fn owner_token() -> &'static str {
    static TOKEN: OnceLock<String> = OnceLock::new();
    TOKEN.get_or_init(|| {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_nanos());
        format!("{}:{nanos}", std::process::id())
    })
}

/// Command ids whose Google create answered (or failed to answer) but whose durable result commit
/// did not finish in this process. Process-wide, so a reopened service in this process still
/// settles them; settlement never calls Google.
static UNSETTLED: Mutex<BTreeSet<String>> = Mutex::new(BTreeSet::new());

fn unsettled() -> Result<MutexGuard<'static, BTreeSet<String>>, Error> {
    UNSETTLED.lock().map_err(|_| Error::Database)
}

/// Marks a begun create whose Google answer is about to be committed as awaiting settlement.
pub(crate) fn retain_unsettled(command_id: CommandId) -> Result<(), Error> {
    unsettled()?.insert(command_id.to_string());
    Ok(())
}

/// Releases a settled or committed create. Called only after its ledger transaction committed.
pub(crate) fn release_unsettled(command_ids: &[String]) -> Result<(), Error> {
    let mut retained = unsettled()?;
    for command_id in command_ids {
        retained.remove(command_id);
    }
    Ok(())
}

/// Whether the command's create is begun and still awaiting its answer in this database.
pub(crate) fn is_begun(conn: &Connection, command_id: CommandId) -> Result<bool, Error> {
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM rcs_group_create_ledger WHERE command_id=? AND state=?)",
        params![command_id.to_string(), BEGUN],
        |row| row.get(0),
    )?)
}

/// Persists UNKNOWN for the begun create of `command_id`, fenced to that command: a row owned by
/// another command, or one already settled, is untouched. The command is terminal for sending.
/// Returns whether a begun row was settled.
fn settle_begun(conn: &Connection, command_id: &str) -> Result<bool, Error> {
    let settled = conn.execute(
        "UPDATE rcs_group_create_ledger SET state=?,outcome_code=?,owner_token=NULL WHERE command_id=? AND state=?",
        params![UNKNOWN, UNKNOWN, command_id, BEGUN],
    )?;
    if settled > 0 {
        terminalize(conn, command_id, SendRefusalReason::CreateUnconfirmed)?;
    }
    Ok(settled > 0)
}

/// Settles a retained create the caller names. Only a command whose commit failed in this process
/// is settled; its begun row becomes unknown. The retained entry is released by the caller after
/// the enclosing transaction commits.
pub(crate) fn settle_retained_command(
    conn: &Connection,
    command_id: CommandId,
) -> Result<Vec<String>, Error> {
    let id = command_id.to_string();
    if !unsettled()?.contains(&id) {
        return Err(Error::InvalidRequest(
            "group create not awaiting settlement",
        ));
    }
    settle_begun(conn, &id)?;
    Ok(vec![id])
}

/// Settles every retained create that is begun in this database, and returns the settled ids.
/// Retained ids that belong to another database stay retained.
pub(crate) fn settle_retained(conn: &Connection) -> Result<Vec<String>, Error> {
    let retained: Vec<String> = unsettled()?.iter().cloned().collect();
    let mut settled = Vec::new();
    for id in retained {
        if settle_begun(conn, &id)? {
            settled.push(id);
        }
    }
    Ok(settled)
}

/// Best-effort settlement at a gateway boundary (open, create pass, reconcile pass). Never calls
/// Google. A settlement that cannot commit stays retained, and the begun row keeps blocking creates,
/// until a later boundary or an explicit settlement succeeds.
pub(crate) fn settle_retained_best_effort(conn: &mut Connection) {
    let Ok(tx) = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate) else {
        return;
    };
    let Ok(settled) = settle_retained(&tx) else {
        return;
    };
    if tx.commit().is_ok() {
        let _ = release_unsettled(&settled);
    }
}

/// Recovery boundary: converts begun rows owned by another process into unknown. A begun row is
/// otherwise in progress, so ordinary admission never converts it. The conversions and their
/// terminal rows commit together, so a failure leaves no unknown create without its terminal.
pub(crate) fn recover_interrupted(conn: &mut Connection, owner: &str) -> Result<(), Error> {
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let mut query = tx.prepare(
        "SELECT command_id FROM rcs_group_create_ledger WHERE state=? AND (owner_token IS NULL OR owner_token<>?)",
    )?;
    let interrupted = query
        .query_map(params![BEGUN, owner], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    drop(query);
    for command_id in interrupted {
        settle_begun(&tx, &command_id)?;
    }
    tx.commit()?;
    Ok(())
}

const BEGUN: &str = "begun";
const PENDING: &str = "pending";
const UNKNOWN: &str = "unknown";
const NOT_RCS: &str = "not_rcs";
const REFUSED: &str = "refused";
const LINKED: &str = "linked";
/// States in which a create is in flight or awaiting proof; other groups must not take them.
const ACTIVE_STATES: [&str; 3] = [BEGUN, PENDING, UNKNOWN];

/// Result of committing Google's create answer for a begun permit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GroupCreateOutcome {
    /// Google returned an RCS group id. The group links when an exact proof is observed.
    Pending,
    /// Google says the group is not RCS. Messages to it go MMS and no further create is tried.
    NotRcs,
    /// The outcome is unknown. Further creates are blocked until reconciliation.
    Unknown,
}

/// The one-use permit a native create executes: the canonical other members and the name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GroupCreatePermit {
    pub command_id: CommandId,
    pub numbers: Vec<String>,
    pub name: Option<String>,
}

/// Outcome of asking for a create permit. A refusal is definite: nothing was created.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GroupCreateBegin {
    Permit(GroupCreatePermit),
    Refused(SendRefusalReason),
}

/// Ledger state of a conversation's create, for desktop and native status.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GroupCreateState {
    None,
    Begun,
    Pending,
    Unknown,
    NotRcs,
    Refused(SendRefusalReason),
    Linked,
}

/// What a group send must do before carrier admission. `Waiting` never sends and never falls back
/// to MMS: the message waits for the create or its proof.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GroupCreateStart {
    NotHandled,
    Waiting,
    Refused(SendRefusalReason),
}

/// Desktop-facing group view: roster, link state, and whether a create is offered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GroupView {
    pub name: Option<String>,
    pub members: Vec<String>,
    pub state: GroupLinkState,
    pub member_revision: u64,
    pub can_create: bool,
    pub create_needs_confirmation: bool,
}

/// Link state of a group conversation, as desktop shows it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GroupLinkState {
    Unlinked,
    Proving,
    Live,
    Paused,
    Withdrawn,
}

impl From<Option<GroupMappingState>> for GroupLinkState {
    fn from(state: Option<GroupMappingState>) -> Self {
        match state {
            None | Some(GroupMappingState::Unlinked) => Self::Unlinked,
            Some(GroupMappingState::Proving) => Self::Proving,
            Some(GroupMappingState::Live) => Self::Live,
            Some(GroupMappingState::Paused) => Self::Paused,
            Some(GroupMappingState::Withdrawn) => Self::Withdrawn,
        }
    }
}

/// Google's create answer, exactly as the Go facade encodes it.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateResult {
    code: String,
    #[serde(default)]
    google_conversation_id: Option<String>,
}

struct Row {
    conversation: ConversationId,
    state: String,
    attempted_at_ms: i64,
    outcome: Option<String>,
    google: Option<String>,
}

fn read_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Row> {
    let conversation: String = row.get(0)?;
    Ok(Row {
        conversation: crate::parse(&conversation).map_err(|_| rusqlite::Error::InvalidQuery)?,
        state: row.get(1)?,
        attempted_at_ms: row.get(2)?,
        outcome: row.get(3)?,
        google: row.get(4)?,
    })
}

const ROW_SELECT: &str = "SELECT conversation_id,state,attempted_at_ms,outcome_code,google_conversation_id FROM rcs_group_create_ledger";

fn row(conn: &Connection, conversation_id: ConversationId) -> Result<Option<Row>, Error> {
    Ok(conn
        .query_row(
            &format!("{ROW_SELECT} WHERE conversation_id=?"),
            [conversation_id.to_string()],
            read_row,
        )
        .optional()?)
}

fn rows_in(conn: &Connection, state: &str) -> Result<Vec<Row>, Error> {
    conn.prepare(&format!(
        "{ROW_SELECT} WHERE state=? ORDER BY conversation_id"
    ))?
    .query_map([state], read_row)?
    .collect::<Result<Vec<_>, _>>()
    .map_err(Into::into)
}

fn set_state(
    conn: &Connection,
    conversation_id: ConversationId,
    state: &str,
    outcome: Option<&str>,
) -> Result<(), Error> {
    conn.execute(
        "UPDATE rcs_group_create_ledger SET state=?,outcome_code=? WHERE conversation_id=?",
        params![state, outcome, conversation_id.to_string()],
    )?;
    Ok(())
}

fn refusal_for(outcome: Option<&str>) -> SendRefusalReason {
    match outcome {
        Some("members_changed") => SendRefusalReason::MembersChanged,
        _ => SendRefusalReason::UpdateRequired,
    }
}

/// Records that the message command can never send over RCS or carrier. Set when its create went
/// unknown or was refused. Independent of the conversation ledger, so a later create, a positive
/// reconciliation, or a NOT_RCS answer for another command cannot resurrect it. The row is kept
/// with the consumed-permit evidence and is never removed.
fn terminalize(
    conn: &Connection,
    command_id: &str,
    reason: SendRefusalReason,
) -> Result<(), Error> {
    conn.execute(
        "INSERT INTO rcs_group_create_terminal(command_id,reason) VALUES(?,?) ON CONFLICT(command_id) DO UPDATE SET reason=excluded.reason",
        params![command_id, reason.code()],
    )?;
    Ok(())
}

/// The refusal a terminal message command carries, or `None` when the command is not terminal.
pub(crate) fn terminal_refusal(
    conn: &Connection,
    command_id: CommandId,
) -> Result<Option<SendRefusalReason>, Error> {
    let code: Option<String> = conn
        .query_row(
            "SELECT reason FROM rcs_group_create_terminal WHERE command_id=?",
            [command_id.to_string()],
            |row| row.get(0),
        )
        .optional()?;
    Ok(code.map(|code| match code.as_str() {
        "members_changed" => SendRefusalReason::MembersChanged,
        "update_required" => SendRefusalReason::UpdateRequired,
        _ => SendRefusalReason::CreateUnconfirmed,
    }))
}

enum Gate {
    NotHandled,
    Needed(GroupCreateRequest),
    Waiting,
    Refused(SendRefusalReason),
}

/// Whether this command has ever consumed a create permit. A consumed command never permits again,
/// even after a later user command has taken over the conversation's ledger row.
fn consumed(conn: &Connection, command_id: CommandId) -> Result<bool, Error> {
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM rcs_group_create_permits WHERE command_id=?)",
        [command_id.to_string()],
        |row| row.get(0),
    )?)
}

/// Decides whether a create-requesting send may issue a permit now. Runs inside the caller's
/// transaction and never changes ledger state: a begun row is in progress, and an unknown row
/// stays unknown until a new user command confirms a duplicate.
fn gate(
    conn: &Connection,
    command_id: CommandId,
    conversation_id: ConversationId,
) -> Result<Gate, Error> {
    let Some(request) = rcs_group::send_fields(conn, command_id)?.group_create else {
        return Ok(Gate::NotHandled);
    };
    if let Some(reason) = terminal_refusal(conn, command_id)? {
        return Ok(Gate::Refused(reason));
    }
    if rcs_group::group_mapping(conn, conversation_id)?.is_some()
        || rcs_group::roster(conn, conversation_id)?.len() < 2
        || rcs_mapping::current_link_state(conn)? != Some(true)
    {
        return Ok(Gate::NotHandled);
    }
    let reused = consumed(conn, command_id)?;
    let Some(current) = row(conn, conversation_id)? else {
        return Ok(if reused {
            Gate::Refused(SendRefusalReason::CreateUnconfirmed)
        } else {
            Gate::Needed(request)
        });
    };
    Ok(match current.state.as_str() {
        BEGUN | PENDING => Gate::Waiting,
        UNKNOWN if request.confirm_duplicate && !reused => Gate::Needed(request),
        UNKNOWN => Gate::Refused(SendRefusalReason::CreateUnconfirmed),
        REFUSED => Gate::Refused(refusal_for(current.outcome.as_deref())),
        // Not RCS, or already linked: the MMS and mapping rules decide.
        _ => Gate::NotHandled,
    })
}

/// Send-path hook. Runs before carrier admission for a group send that asked to create.
pub(crate) fn admit(
    conn: &Connection,
    command_id: CommandId,
    conversation_id: ConversationId,
) -> Result<GroupCreateStart, Error> {
    Ok(match gate(conn, command_id, conversation_id)? {
        Gate::NotHandled => GroupCreateStart::NotHandled,
        Gate::Needed(_) | Gate::Waiting => GroupCreateStart::Waiting,
        Gate::Refused(reason) => {
            terminalize(conn, &command_id.to_string(), reason)?;
            GroupCreateStart::Refused(reason)
        }
    })
}

/// Issues the single-use create permit for a send whose gate says a create is needed.
pub(crate) fn begin(
    conn: &Connection,
    command_id: CommandId,
    conversation_id: ConversationId,
    now_ms: i64,
) -> Result<GroupCreateBegin, Error> {
    match gate(conn, command_id, conversation_id)? {
        Gate::Needed(request) => {
            conn.execute(
                "INSERT INTO rcs_group_create_permits(command_id) VALUES(?)",
                [command_id.to_string()],
            )?;
            conn.execute(
                "INSERT INTO rcs_group_create_ledger(conversation_id,command_id,state,attempted_at_ms,reconciled_at_ms,outcome_code,google_conversation_id,owner_token) VALUES(?,?,'begun',?,NULL,NULL,NULL,?) ON CONFLICT(conversation_id) DO UPDATE SET command_id=excluded.command_id,state='begun',attempted_at_ms=excluded.attempted_at_ms,reconciled_at_ms=NULL,outcome_code=NULL,google_conversation_id=NULL,owner_token=excluded.owner_token",
                params![
                    conversation_id.to_string(),
                    command_id.to_string(),
                    now_ms,
                    owner_token()
                ],
            )?;
            Ok(GroupCreateBegin::Permit(GroupCreatePermit {
                command_id,
                numbers: rcs_group::roster(conn, conversation_id)?
                    .into_iter()
                    .collect(),
                name: request.name,
            }))
        }
        Gate::Refused(reason) => {
            terminalize(conn, &command_id.to_string(), reason)?;
            Ok(GroupCreateBegin::Refused(reason))
        }
        Gate::NotHandled | Gate::Waiting => {
            Err(Error::InvalidRequest("group create not permitted"))
        }
    }
}

/// Records Google's answer for a begun permit. Anything other than a recognised answer is unknown,
/// so an ambiguous reply never permits a retry.
pub(crate) fn commit(
    conn: &Connection,
    command_id: CommandId,
    result_json: &str,
) -> Result<GroupCreateOutcome, Error> {
    let Some(current) = conn
        .query_row(
            "SELECT conversation_id,state FROM rcs_group_create_ledger WHERE command_id=?",
            [command_id.to_string()],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?
    else {
        return Err(Error::InvalidRequest("group create not begun"));
    };
    if current.1 != BEGUN {
        return Err(Error::InvalidRequest("group create not begun"));
    }
    let conversation: ConversationId = crate::parse(&current.0)?;
    let answer = serde_json::from_str::<CreateResult>(result_json).ok();
    let (state, outcome, google, reported) = match answer {
        Some(CreateResult {
            code,
            google_conversation_id: Some(id),
        }) if matches!(code.as_str(), "created_rcs_group" | "existing_rcs_group")
            && !id.is_empty() =>
        {
            (PENDING, code, Some(id), GroupCreateOutcome::Pending)
        }
        Some(CreateResult {
            code,
            google_conversation_id: None,
        }) if code == "not_rcs" => (NOT_RCS, code, None, GroupCreateOutcome::NotRcs),
        _ => (
            UNKNOWN,
            UNKNOWN.to_owned(),
            None,
            GroupCreateOutcome::Unknown,
        ),
    };
    conn.execute(
        "UPDATE rcs_group_create_ledger SET state=?,outcome_code=?,google_conversation_id=?,owner_token=NULL WHERE conversation_id=? AND state='begun'",
        params![state, outcome, google, conversation.to_string()],
    )?;
    // The message that asked for an unknown create is terminal: it must never send later, even
    // after a positive reconciliation or a confirmed duplicate links the group.
    if reported == GroupCreateOutcome::Unknown {
        terminalize(
            conn,
            &command_id.to_string(),
            SendRefusalReason::CreateUnconfirmed,
        )?;
    }
    // Only the send that triggered the create is informed; later sends to the group do not repeat it.
    if reported == GroupCreateOutcome::NotRcs {
        rcs_group::record_send_reason(conn, command_id, SendRefusalReason::CreateNotRcs)?;
    }
    Ok(reported)
}

/// The create state the desktop and native status show for a conversation.
pub(crate) fn state(
    conn: &Connection,
    conversation_id: ConversationId,
) -> Result<GroupCreateState, Error> {
    let Some(current) = row(conn, conversation_id)? else {
        return Ok(GroupCreateState::None);
    };
    Ok(match current.state.as_str() {
        BEGUN => GroupCreateState::Begun,
        PENDING => GroupCreateState::Pending,
        UNKNOWN => GroupCreateState::Unknown,
        NOT_RCS => GroupCreateState::NotRcs,
        LINKED => GroupCreateState::Linked,
        REFUSED => GroupCreateState::Refused(refusal_for(current.outcome.as_deref())),
        _ => GroupCreateState::None,
    })
}

/// Whether any conversation with an active create already owns exactly `members`. A group that
/// matches it must not create a second conversation.
pub(crate) fn active_create_owns(
    conn: &Connection,
    members: &BTreeSet<String>,
) -> Result<bool, Error> {
    for state in ACTIVE_STATES {
        for current in rows_in(conn, state)? {
            if rcs_group::roster(conn, current.conversation)? == *members {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

/// Desktop view of a group conversation. `None` when the conversation has no roster and no
/// mapping, so it is not a group.
pub fn group_view(
    conn: &Connection,
    conversation_id: ConversationId,
) -> Result<Option<GroupView>, Error> {
    let members = rcs_group::roster(conn, conversation_id)?;
    let mapping = rcs_group::group_mapping(conn, conversation_id)?;
    if members.is_empty() && mapping.is_none() {
        return Ok(None);
    }
    let (name, member_revision): (Option<String>, u64) = conn
        .query_row(
            "SELECT name,member_revision FROM conversation_group_meta WHERE conversation_id=?",
            [conversation_id.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?
        .unwrap_or((None, 0));
    let current = row(conn, conversation_id)?;
    let create_needs_confirmation = current.as_ref().is_some_and(|row| row.state == UNKNOWN);
    let can_create = mapping.is_none() && members.len() >= 2 && current.is_none();
    Ok(Some(GroupView {
        name,
        state: GroupLinkState::from(mapping.map(|mapping| mapping.state)),
        members: members.into_iter().collect(),
        member_revision,
        can_create,
        create_needs_confirmation,
    }))
}

/// Reconciles pending and unknown rows against one complete Google snapshot. Pending rows link
/// only on an exact roster proof; unknown rows link only on a single provable newer match.
pub(crate) fn resolve(
    conn: &Connection,
    ctx: &Ctx<'_>,
    records: &BTreeMap<&str, &ConversationCandidateRecord>,
    token: &str,
    now_ms: i64,
    report: &mut GroupIngestReport,
) -> Result<(), Error> {
    for current in rows_in(conn, PENDING)? {
        resolve_pending(conn, ctx, &current, records, token, now_ms, report)?;
    }
    for current in rows_in(conn, UNKNOWN)? {
        resolve_unknown(conn, ctx, &current, records, token, now_ms, report)?;
    }
    Ok(())
}

fn resolve_pending(
    conn: &Connection,
    ctx: &Ctx<'_>,
    current: &Row,
    records: &BTreeMap<&str, &ConversationCandidateRecord>,
    token: &str,
    now_ms: i64,
    report: &mut GroupIngestReport,
) -> Result<(), Error> {
    if rcs_group::group_mapping(conn, current.conversation)?.is_some() {
        return set_state(conn, current.conversation, LINKED, None);
    }
    let Some(google) = current.google.as_deref() else {
        return Ok(());
    };
    if rcs_group::claimed_google_ids(conn)?.contains(google) {
        set_state(
            conn,
            current.conversation,
            REFUSED,
            Some("mapping_conflict"),
        )?;
        report.conflicts += 1;
        return Ok(());
    }
    let Some(record) = records.get(google) else {
        return Ok(());
    };
    let (proof, members) = rcs_group::classify_record(conn, record)?;
    if proof != GroupProof::Provable {
        return Ok(());
    }
    if members == rcs_group::roster(conn, current.conversation)? {
        rcs_group::link(
            conn,
            ctx,
            current.conversation,
            record,
            &members,
            token,
            now_ms,
        )?;
        set_state(conn, current.conversation, LINKED, None)?;
        report.linked += 1;
    } else {
        set_state(conn, current.conversation, REFUSED, Some("members_changed"))?;
        report.conflicts += 1;
    }
    Ok(())
}

fn resolve_unknown(
    conn: &Connection,
    ctx: &Ctx<'_>,
    current: &Row,
    records: &BTreeMap<&str, &ConversationCandidateRecord>,
    token: &str,
    now_ms: i64,
    report: &mut GroupIngestReport,
) -> Result<(), Error> {
    let roster = rcs_group::roster(conn, current.conversation)?;
    let claimed = rcs_group::claimed_google_ids(conn)?;
    let mut matches = Vec::new();
    for record in records.values() {
        if claimed.contains(record.google_conversation_id.as_str())
            || record.last_message_ms <= current.attempted_at_ms
        {
            continue;
        }
        let (proof, members) = rcs_group::classify_record(conn, record)?;
        if proof == GroupProof::Provable && members == roster {
            matches.push((*record, members));
        }
    }
    match matches.as_slice() {
        // No provable, newer, unclaimed match is not evidence that Google created nothing: stale,
        // empty, or unprovable snapshots leave the consumed create unknown. Only a new user command
        // that confirms a duplicate may create again.
        [] => {}
        [(record, members)] => {
            rcs_group::link(
                conn,
                ctx,
                current.conversation,
                record,
                members,
                token,
                now_ms,
            )?;
            set_state(conn, current.conversation, LINKED, Some("reconciled"))?;
            report.linked += 1;
        }
        _ => {
            conn.execute(
                "UPDATE rcs_group_create_ledger SET outcome_code='ambiguous',reconciled_at_ms=? WHERE conversation_id=?",
                params![now_ms, current.conversation.to_string()],
            )?;
            report.conflicts += 1;
        }
    }
    Ok(())
}
