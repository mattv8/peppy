use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use peppy_domain::{ConversationId, DeviceId, SendRefusalReason};
use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    CommandId, Ctx, Error, GroupCreateRequest, MessagePayload,
    conversation_match::{self, OwnAddressCache, canonical_address, message_recipient_set},
    group_metadata,
    rcs_mapping::{self, RcsConversationType, RcsLinkState, RcsMappingObservation, RcsSendMode},
    rcs_target::{ConversationCandidateRecord, ConversationCandidateSnapshot},
};

const RCS_CONVERSATION_TYPE: i32 = 2;
const ACTIVE_CONVERSATION_STATUS: i32 = 1;
const PHONE_IDENTIFIER_TYPE: i32 = 1;
const GROUP_OWNER_PREFIX: &str = "rcs_link_owner_google:";
const CLAIM_FLOOR_KEY: &str = "rcs_group_claim_floor_ms";
/// Mirrors `rcs_receive::RCS_CLAIM_BACKFILL_WINDOW_MS`: the 1:1 link claim time is `now - 15 min`.
const CLAIM_BACKFILL_WINDOW_MS: i64 = 15 * 60 * 1_000;

/// Gateway-local group mapping state. Only `Live` permits an RCS group operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GroupMappingState {
    Unlinked,
    Proving,
    Live,
    Paused,
    Withdrawn,
}

impl GroupMappingState {
    fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "unlinked" => Self::Unlinked,
            "proving" => Self::Proving,
            "live" => Self::Live,
            "paused" => Self::Paused,
            "withdrawn" => Self::Withdrawn,
            _ => return None,
        })
    }

    /// States whose conversation keeps receiving RCS records, history, and provider routing.
    pub(crate) fn receives(self) -> bool {
        matches!(self, Self::Live | Self::Paused | Self::Proving)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct GroupMapping {
    pub(crate) conversation_id: ConversationId,
    pub(crate) google_conversation_id: String,
    pub(crate) gateway_device_id: DeviceId,
    pub(crate) state: GroupMappingState,
    pub(crate) proved_member_revision: u64,
    pub(crate) member_set_digest: String,
    pub(crate) proof_observed_at_ms: i64,
    pub(crate) snapshot_token: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GroupProof {
    Provable,
    NotProvable,
    Withdrawn,
}

/// Counts of durable outcomes from one complete candidate snapshot. Google identifiers stay
/// inside core; callers query the mapping by Peppy conversation when they need one.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GroupIngestReport {
    /// Unmapped provable groups attached to an existing Peppy conversation.
    pub linked: usize,
    /// Unmapped provable groups with recent activity that got a new Peppy conversation.
    pub created: usize,
    /// Mappings that ended the pass `Live` (new, re-proved, or unchanged).
    pub live: usize,
    /// Mappings whose roster changed in this pass (member_revision advanced).
    pub members_changed: usize,
    /// Live mappings that became unprovable and were paused.
    pub paused: usize,
    /// Mappings withdrawn because the group vanished, became read-only, or self left.
    pub withdrawn: usize,
    /// Unmapped groups that are not provable (including read-only or self-left).
    pub not_provable: usize,
    /// Provable unmapped groups with no recent activity: nothing is created until a new message.
    pub dormant: usize,
    /// Groups refused for 1:1 mutual exclusion or for another gateway's claim.
    pub conflicts: usize,
}

pub(crate) fn group_mapping(
    conn: &Connection,
    conversation_id: ConversationId,
) -> Result<Option<GroupMapping>, Error> {
    Ok(conn
        .query_row(
            &format!("{MAPPING_SELECT} WHERE conversation_id=?"),
            [conversation_id.to_string()],
            read_mapping,
        )
        .optional()?)
}

const MAPPING_SELECT: &str = "SELECT conversation_id,google_conversation_id,gateway_device_id,state,proved_member_revision,member_set_digest,proof_observed_at_ms,snapshot_token FROM rcs_group_mapping";

fn read_mapping(row: &Row<'_>) -> rusqlite::Result<GroupMapping> {
    let conversation: String = row.get(0)?;
    let gateway: String = row.get(2)?;
    let state: String = row.get(3)?;
    Ok(GroupMapping {
        conversation_id: crate::parse(&conversation).map_err(|_| rusqlite::Error::InvalidQuery)?,
        google_conversation_id: row.get(1)?,
        gateway_device_id: gateway.parse().map_err(|_| rusqlite::Error::InvalidQuery)?,
        state: GroupMappingState::parse(&state).ok_or(rusqlite::Error::InvalidQuery)?,
        proved_member_revision: row.get(4)?,
        member_set_digest: row.get(5)?,
        proof_observed_at_ms: row.get(6)?,
        snapshot_token: row.get(7)?,
    })
}

pub(crate) fn proved_member_revision(
    conn: &Connection,
    conversation_id: ConversationId,
) -> Result<Option<u64>, Error> {
    Ok(group_mapping(conn, conversation_id)?.and_then(|mapping| {
        (mapping.state == GroupMappingState::Live).then_some(mapping.proved_member_revision)
    }))
}

/// The stored member digest of a Live group, read as advisory input for the Go prepare request.
/// It is not a proof and carries no freshness: the fresh observation, re-proof, and the
/// freshness-checked preparation that follow are what authorize a send.
pub(crate) fn stored_member_digest(
    conn: &Connection,
    conversation_id: ConversationId,
) -> Result<Option<String>, Error> {
    Ok(group_mapping(conn, conversation_id)?.and_then(|mapping| {
        (mapping.state == GroupMappingState::Live).then_some(mapping.member_set_digest)
    }))
}

pub(crate) fn roster(
    conn: &Connection,
    conversation_id: ConversationId,
) -> Result<BTreeSet<String>, Error> {
    conn.prepare("SELECT canonical_address FROM conversation_group_members WHERE conversation_id=? ORDER BY canonical_address")?
        .query_map([conversation_id.to_string()], |row| row.get(0))?
        .collect::<Result<BTreeSet<_>, _>>()
        .map_err(Into::into)
}

/// Moves a live or proving mapping to `paused`; withdrawn mappings stay withdrawn. A real change
/// syncs the link authority so other devices show the pause.
pub(crate) fn pause_group(
    conn: &Connection,
    ctx: &Ctx<'_>,
    conversation_id: ConversationId,
    now_ms: i64,
) -> Result<bool, Error> {
    let paused = conn.execute(
        "UPDATE rcs_group_mapping SET state='paused',updated_at_ms=? WHERE conversation_id=? AND state IN ('live','proving')",
        params![now_ms, conversation_id.to_string()],
    )? > 0;
    if paused {
        group_metadata::sync_link_state(conn, ctx, conversation_id, now_ms)?;
    }
    Ok(paused)
}

/// I2 proof. Google's visible non-self set minus ALL vault own addresses must have at least two
/// members. The Go digest is intentionally not recomputed: it represents that gateway's view.
pub(crate) fn classify_record(
    conn: &Connection,
    record: &ConversationCandidateRecord,
) -> Result<(GroupProof, BTreeSet<String>), Error> {
    let visible = record
        .participants
        .iter()
        .filter(|participant| participant.visible)
        .collect::<Vec<_>>();
    let self_count = visible
        .iter()
        .filter(|participant| participant.self_)
        .count();
    let invalid_participant = record.invisible_participant_count != 0
        || visible.iter().any(|participant| {
            participant.identifier_type != PHONE_IDENTIFIER_TYPE
                || !participant.canonical_address_present
                || !canonical_e164(&participant.canonical_address)
        });
    let own = own_addresses(conn)?;
    let members = visible
        .iter()
        .filter(|participant| !participant.self_)
        .map(|participant| participant.canonical_address.clone())
        .filter(|address| !own.contains(address))
        .collect::<BTreeSet<_>>();
    if record.read_only || self_count != 1 {
        return Ok((GroupProof::Withdrawn, members));
    }
    if !record.group
        || !record.is_group_chat
        || record.conversation_type != RCS_CONVERSATION_TYPE
        || record.conversation_status != ACTIVE_CONVERSATION_STATUS
        || invalid_participant
        || record.visible_non_self_count
            != visible
                .iter()
                .filter(|participant| !participant.self_)
                .count()
        || members.len() < 2
    {
        return Ok((GroupProof::NotProvable, members));
    }
    Ok((GroupProof::Provable, members))
}

/// Every vault own address (all devices and SIMs), canonicalised like Google's participants.
pub(crate) fn own_addresses(conn: &Connection) -> Result<BTreeSet<String>, Error> {
    let region = conversation_match::identity_region(conn)?;
    conn.prepare("SELECT DISTINCT address FROM mms_own_addresses")?
        .query_map([], |row| row.get::<_, String>(0))?
        .map(|address| address.map(|address| canonical_address(&address, Some(&region))))
        .collect::<Result<BTreeSet<_>, _>>()
        .map_err(Into::into)
}

fn canonical_e164(address: &str) -> bool {
    let Some(digits) = address.strip_prefix('+') else {
        return false;
    };
    (1..=15).contains(&digits.len())
        && matches!(digits.as_bytes().first(), Some(b'1'..=b'9'))
        && digits.bytes().all(|digit| digit.is_ascii_digit())
}

fn valid_snapshot(snapshot: &ConversationCandidateSnapshot) -> bool {
    snapshot.code == "conversation_candidate_snapshot"
        && snapshot.complete
        && snapshot.snapshot_token
            == format!(
                "{}:{}:{}",
                snapshot.generation, snapshot.stream_epoch, snapshot.revision
            )
}

/// Applies one complete Google snapshot to this gateway's group mappings and links, creates, or
/// withdraws mappings. Runs inside the caller's transaction.
pub(crate) fn ingest_snapshot(
    conn: &Connection,
    ctx: &Ctx<'_>,
    snapshot: &ConversationCandidateSnapshot,
    now_ms: i64,
) -> Result<GroupIngestReport, Error> {
    if !valid_snapshot(snapshot) {
        return Err(Error::InvalidRequest("group snapshot"));
    }
    let mut records = BTreeMap::new();
    for record in &snapshot.conversations {
        if record.google_conversation_id.is_empty()
            || records
                .insert(record.google_conversation_id.as_str(), record)
                .is_some()
        {
            return Err(Error::InvalidRequest("group snapshot"));
        }
    }
    let token = snapshot.snapshot_token.as_str();
    let mut report = GroupIngestReport::default();
    let mut handled = BTreeSet::new();
    for mapping in gateway_mappings(conn, ctx.device_id)? {
        handled.insert(mapping.google_conversation_id.clone());
        match records.get(mapping.google_conversation_id.as_str()) {
            None => {
                if withdraw(conn, ctx, mapping.conversation_id, now_ms)? {
                    report.withdrawn += 1;
                }
            }
            Some(record) => reprove(conn, ctx, &mapping, record, token, now_ms, &mut report)?,
        }
    }
    crate::rcs_group_create::resolve(conn, ctx, &records, token, now_ms, &mut report)?;
    let claimed = claimed_google_ids(conn)?;
    let mut candidates = None;
    for (google, record) in &records {
        if handled.contains(*google) {
            continue;
        }
        if claimed.contains(*google) {
            report.conflicts += 1;
            continue;
        }
        let (proof, members) = classify_record(conn, record)?;
        if proof != GroupProof::Provable {
            report.not_provable += 1;
            continue;
        }
        if candidates.is_none() {
            candidates = Some(roster_candidates(conn)?);
        }
        let candidate_list = candidates.as_deref().unwrap_or_default();
        if let Some(conversation_id) = first_eligible(conn, candidate_list, &members)? {
            link(conn, ctx, conversation_id, record, &members, token, now_ms)?;
            report.linked += 1;
        } else if crate::rcs_group_create::active_create_owns(conn, &members)? {
            // A create for this roster is in flight or awaiting proof: never a second conversation.
            report.conflicts += 1;
        } else if record.last_message_ms > claim_floor(conn, now_ms)? {
            link(
                conn,
                ctx,
                ConversationId::new(),
                record,
                &members,
                token,
                now_ms,
            )?;
            report.created += 1;
        } else {
            report.dormant += 1;
        }
    }
    // Links, pauses, withdrawals, and create settlements from this pass sync once, at its end.
    group_metadata::sync_all_link_states(conn, ctx, now_ms)?;
    Ok(report)
}

fn reprove(
    conn: &Connection,
    ctx: &Ctx<'_>,
    mapping: &GroupMapping,
    record: &ConversationCandidateRecord,
    token: &str,
    now_ms: i64,
    report: &mut GroupIngestReport,
) -> Result<(), Error> {
    let conversation_id = mapping.conversation_id;
    let (proof, members) = classify_record(conn, record)?;
    match proof {
        GroupProof::Withdrawn => {
            if withdraw(conn, ctx, conversation_id, now_ms)? {
                report.withdrawn += 1;
            }
            return Ok(());
        }
        GroupProof::NotProvable => {
            if pause_group(conn, ctx, conversation_id, now_ms)? {
                report.paused += 1;
            }
            return Ok(());
        }
        GroupProof::Provable => {}
    }
    if one_to_one_exists(conn, conversation_id)? {
        withdraw(conn, ctx, conversation_id, now_ms)?;
        report.conflicts += 1;
        return Ok(());
    }
    if roster(conn, conversation_id)? != members {
        pause_group(conn, ctx, conversation_id, now_ms)?;
        report.members_changed += 1;
    }
    let revision = group_metadata::publish(
        conn,
        ctx,
        conversation_id,
        Some(&record.name),
        &members,
        now_ms,
    )?;
    let proof = LiveProof {
        google_conversation_id: &mapping.google_conversation_id,
        member_revision: revision,
        member_digest: &record.member_digest,
        snapshot_token: token,
        observed_at_ms: now_ms,
    };
    upsert_live(conn, conversation_id, ctx.device_id, &proof)?;
    report.live += 1;
    Ok(())
}

pub(crate) fn link(
    conn: &Connection,
    ctx: &Ctx<'_>,
    conversation_id: ConversationId,
    record: &ConversationCandidateRecord,
    members: &BTreeSet<String>,
    token: &str,
    now_ms: i64,
) -> Result<(), Error> {
    let revision = group_metadata::publish(
        conn,
        ctx,
        conversation_id,
        Some(&record.name),
        members,
        now_ms,
    )?;
    let proof = LiveProof {
        google_conversation_id: &record.google_conversation_id,
        member_revision: revision,
        member_digest: &record.member_digest,
        snapshot_token: token,
        observed_at_ms: now_ms,
    };
    upsert_live(conn, conversation_id, ctx.device_id, &proof)?;
    conn.execute(
        "INSERT INTO metadata(k,v) VALUES(?,?) ON CONFLICT(k) DO UPDATE SET v=excluded.v",
        params![
            format!("{GROUP_OWNER_PREFIX}{conversation_id}"),
            record.google_conversation_id
        ],
    )?;
    crate::rcs_receive::ensure_group_receive_lane(conn, conversation_id, now_ms)
}

/// A proof observed in one snapshot. The digest is Go's opaque token, stored verbatim (A1).
struct LiveProof<'a> {
    google_conversation_id: &'a str,
    member_revision: u64,
    member_digest: &'a str,
    snapshot_token: &'a str,
    observed_at_ms: i64,
}

fn upsert_live(
    conn: &Connection,
    conversation_id: ConversationId,
    gateway: DeviceId,
    proof: &LiveProof<'_>,
) -> Result<(), Error> {
    conn.execute(
        "INSERT INTO rcs_group_mapping(conversation_id,google_conversation_id,gateway_device_id,state,proved_member_revision,member_set_digest,proof_observed_at_ms,snapshot_token,created_at_ms,updated_at_ms) VALUES(?,?,?,'live',?,?,?,?,?,?) ON CONFLICT(conversation_id) DO UPDATE SET state='live',proved_member_revision=excluded.proved_member_revision,member_set_digest=excluded.member_set_digest,proof_observed_at_ms=excluded.proof_observed_at_ms,snapshot_token=excluded.snapshot_token,updated_at_ms=excluded.updated_at_ms",
        params![
            conversation_id.to_string(),
            proof.google_conversation_id,
            gateway.to_string(),
            proof.member_revision,
            proof.member_digest,
            proof.observed_at_ms,
            proof.snapshot_token,
            proof.observed_at_ms,
            proof.observed_at_ms,
        ],
    )?;
    Ok(())
}

fn withdraw(
    conn: &Connection,
    ctx: &Ctx<'_>,
    conversation_id: ConversationId,
    now_ms: i64,
) -> Result<bool, Error> {
    let withdrawn = conn.execute(
        "UPDATE rcs_group_mapping SET state='withdrawn',updated_at_ms=? WHERE conversation_id=? AND state<>'withdrawn'",
        params![now_ms, conversation_id.to_string()],
    )? > 0;
    if withdrawn {
        group_metadata::sync_link_state(conn, ctx, conversation_id, now_ms)?;
    }
    Ok(withdrawn)
}

fn gateway_mappings(conn: &Connection, gateway: DeviceId) -> Result<Vec<GroupMapping>, Error> {
    conn.prepare(&format!(
        "{MAPPING_SELECT} WHERE gateway_device_id=? ORDER BY conversation_id"
    ))?
    .query_map([gateway.to_string()], read_mapping)?
    .collect::<Result<Vec<_>, _>>()
    .map_err(Into::into)
}

/// Conversations this gateway holds `Live`: the only group states whose eligibility is `Eligible`.
pub(crate) fn live_conversations(
    conn: &Connection,
    gateway: DeviceId,
) -> Result<HashSet<ConversationId>, Error> {
    conn.prepare(
        "SELECT conversation_id FROM rcs_group_mapping WHERE gateway_device_id=? AND state='live'",
    )?
    .query_map([gateway.to_string()], |row| row.get::<_, String>(0))?
    .collect::<Result<Vec<_>, _>>()?
    .iter()
    .map(|id| crate::parse(id))
    .collect()
}

pub(crate) fn claimed_google_ids(conn: &Connection) -> Result<BTreeSet<String>, Error> {
    conn.prepare("SELECT google_conversation_id FROM rcs_group_mapping")?
        .query_map([], |row| row.get(0))?
        .collect::<Result<BTreeSet<_>, _>>()
        .map_err(Into::into)
}

fn one_to_one_exists(conn: &Connection, conversation_id: ConversationId) -> Result<bool, Error> {
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM rcs_conversation_mappings WHERE peppy_conversation_id=?)",
        [conversation_id.to_string()],
        |row| row.get(0),
    )?)
}

/// Creation floor shared with the 1:1 link claim: set once, on the first unmatched decision.
fn claim_floor(conn: &Connection, now_ms: i64) -> Result<i64, Error> {
    let stored: Option<String> = conn
        .query_row(
            "SELECT v FROM metadata WHERE k=?",
            [CLAIM_FLOOR_KEY],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(value) = stored {
        return value.parse().map_err(|_| Error::Database);
    }
    let floor = now_ms.saturating_sub(CLAIM_BACKFILL_WINDOW_MS);
    conn.execute(
        "INSERT INTO metadata(k,v) VALUES(?,?)",
        params![CLAIM_FLOOR_KEY, floor.to_string()],
    )?;
    Ok(floor)
}

struct Candidate {
    conversation_id: String,
    members: BTreeSet<String>,
    activity_ms: i64,
}

/// Every conversation whose roster has at least two members: the synced roster when present,
/// otherwise its newest message's participant set (legacy MMS groups). Most recent first.
fn roster_candidates(conn: &Connection) -> Result<Vec<Candidate>, Error> {
    let activity: HashMap<String, i64> = conn
        .prepare("SELECT conversation_id,MAX(sort_at_ms) FROM messages GROUP BY conversation_id")?
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<Result<_, _>>()?;
    let mut rosters: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for row in conn
        .prepare("SELECT conversation_id,canonical_address FROM conversation_group_members")?
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
    {
        let (conversation_id, address) = row?;
        rosters.entry(conversation_id).or_default().insert(address);
    }
    let own = own_addresses(conn)?;
    let region = conversation_match::identity_region(conn)?;
    let mut own_cache = OwnAddressCache::new();
    let mut newest = conn.prepare(
        "SELECT conversation_id,payload FROM (SELECT conversation_id,payload,ROW_NUMBER() OVER (PARTITION BY conversation_id ORDER BY sort_at_ms DESC,local_order DESC) AS newest FROM messages) WHERE newest=1",
    )?;
    for row in newest.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })? {
        let (conversation_id, payload) = row?;
        if rosters.contains_key(&conversation_id) {
            continue;
        }
        let Ok(payload) = crate::decode::<MessagePayload>(payload.as_bytes()) else {
            continue;
        };
        let Some(participants) =
            message_recipient_set(conn, &payload, Some(&region), &mut own_cache)?
        else {
            continue;
        };
        rosters.insert(
            conversation_id,
            participants
                .into_iter()
                .filter(|address| !own.contains(address))
                .collect(),
        );
    }
    let mut candidates = rosters
        .into_iter()
        .filter(|(_, members)| members.len() >= 2)
        .map(|(conversation_id, members)| Candidate {
            activity_ms: activity.get(&conversation_id).copied().unwrap_or(0),
            conversation_id,
            members,
        })
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| {
        right
            .activity_ms
            .cmp(&left.activity_ms)
            .then_with(|| left.conversation_id.cmp(&right.conversation_id))
    });
    Ok(candidates)
}

/// Identical-set candidates tie-break by activity then id (Q4). A conversation with any 1:1
/// mapping, any group mapping, or another group's ownership claim is never eligible (B2).
fn first_eligible(
    conn: &Connection,
    candidates: &[Candidate],
    members: &BTreeSet<String>,
) -> Result<Option<ConversationId>, Error> {
    for candidate in candidates {
        if &candidate.members != members {
            continue;
        }
        let conversation_id = crate::parse::<ConversationId>(&candidate.conversation_id)?;
        let blocked: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM rcs_group_mapping WHERE conversation_id=?1)
                OR EXISTS(SELECT 1 FROM rcs_conversation_mappings WHERE peppy_conversation_id=?1)
                OR EXISTS(SELECT 1 FROM metadata WHERE k=?2)
                OR EXISTS(SELECT 1 FROM rcs_group_create_ledger WHERE conversation_id=?1 AND state IN ('begun','pending','unknown'))",
            params![
                conversation_id.to_string(),
                format!("{GROUP_OWNER_PREFIX}{conversation_id}")
            ],
            |row| row.get(0),
        )?;
        if !blocked {
            return Ok(Some(conversation_id));
        }
    }
    Ok(None)
}

/// Refuses a group mapping when the Peppy conversation already carries any 1:1 mapping row.
pub(crate) fn has_group_mapping(
    conn: &Connection,
    conversation_id: ConversationId,
) -> Result<bool, Error> {
    Ok(group_mapping(conn, conversation_id)?.is_some())
}

/// Whether `canonical` is a current roster member of a Live mapping for `conversation_id` (I7).
pub(crate) fn live_roster_contains(
    conn: &Connection,
    conversation_id: ConversationId,
    canonical: &str,
) -> Result<bool, Error> {
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM rcs_group_mapping g JOIN conversation_group_members m ON m.conversation_id=g.conversation_id WHERE g.conversation_id=? AND g.state='live' AND m.canonical_address=?)",
        params![conversation_id.to_string(), canonical],
        |row| row.get(0),
    )?)
}

/// The group mapping only while it still receives RCS traffic. Receive never pauses (I5), so
/// Paused and Proving stay receivable; Withdrawn and Unlinked do not.
pub(crate) fn receivable_mapping(
    conn: &Connection,
    conversation_id: ConversationId,
) -> Result<Option<GroupMapping>, Error> {
    Ok(group_mapping(conn, conversation_id)?.filter(|mapping| mapping.state.receives()))
}

/// Receivable group conversations whose current roster equals `participants` exactly.
pub(crate) fn receivable_conversations_for_roster(
    conn: &Connection,
    participants: &BTreeSet<String>,
) -> Result<Vec<ConversationId>, Error> {
    let ids = conn
        .prepare("SELECT conversation_id FROM rcs_group_mapping WHERE state IN ('live','paused','proving') ORDER BY conversation_id")?
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    let mut matches = Vec::new();
    for id in ids {
        let conversation_id = crate::parse(&id).map_err(|_| Error::Database)?;
        if roster(conn, conversation_id)? == *participants {
            matches.push(conversation_id);
        }
    }
    Ok(matches)
}

/// Desktop group fields that accompany a draft send to two or more recipients. Stored in the
/// durable send intent, so a remote acceptance sends with the same fields.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupDraftSend {
    /// Name written with the roster on a first send, and carried into a create request.
    pub group_name: Option<String>,
    /// The roster revision the composer showed; `None` when it showed none.
    pub expected_member_revision: Option<u64>,
    /// Set only by the explicit create-again action after an unknown create outcome.
    pub group_create_confirm_duplicate: bool,
}

/// Group fields written into a `SendCommand` and remembered by the gateway for admission.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct GroupSendFields {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) expected_member_revision: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) group_create: Option<GroupCreateRequest>,
}

const SEND_FIELDS_PREFIX: &str = "rcs_group_send_v1/";
const GROUP_NAME_MAX_BYTES: usize = 256;

fn send_fields_key(command_id: CommandId) -> String {
    format!("{SEND_FIELDS_PREFIX}{command_id}")
}

/// Group fields the gateway recorded for `command_id`; absent for 1:1 and pre-M4 commands.
pub(crate) fn send_fields(
    conn: &Connection,
    command_id: CommandId,
) -> Result<GroupSendFields, Error> {
    let value: Option<String> = conn
        .query_row(
            "SELECT v FROM metadata WHERE k=?",
            [send_fields_key(command_id)],
            |row| row.get(0),
        )
        .optional()?;
    match value {
        Some(value) => serde_json::from_str(&value).map_err(|_| Error::Database),
        None => Ok(GroupSendFields::default()),
    }
}

/// Records a received command's group fields once. Empty fields are not recorded.
pub(crate) fn record_send_fields(
    conn: &Connection,
    command_id: CommandId,
    fields: &GroupSendFields,
) -> Result<(), Error> {
    if *fields == GroupSendFields::default() {
        return Ok(());
    }
    conn.execute(
        "INSERT INTO metadata(k,v) VALUES(?,?) ON CONFLICT(k) DO NOTHING",
        params![
            send_fields_key(command_id),
            serde_json::to_string(fields).map_err(|_| Error::Database)?
        ],
    )?;
    Ok(())
}

/// Whether a conversation routes as a group for a send: a group mapping or a roster of two or more.
pub(crate) fn is_group_conversation(
    conn: &Connection,
    conversation_id: ConversationId,
) -> Result<bool, Error> {
    Ok(
        group_mapping(conn, conversation_id)?.is_some()
            || roster(conn, conversation_id)?.len() >= 2,
    )
}

const SEND_REASON_PREFIX: &str = "rcs_group_send_reason_v1/";

fn send_reason_key(command_id: CommandId) -> String {
    format!("{SEND_REASON_PREFIX}{command_id}")
}

/// Records the definite reason a group send was refused or informed (desktop-facing, no ids).
pub(crate) fn record_send_reason(
    conn: &Connection,
    command_id: CommandId,
    reason: SendRefusalReason,
) -> Result<(), Error> {
    conn.execute(
        "INSERT INTO metadata(k,v) VALUES(?,?) ON CONFLICT(k) DO UPDATE SET v=excluded.v",
        params![
            send_reason_key(command_id),
            serde_json::to_string(&reason).map_err(|_| Error::Database)?
        ],
    )?;
    Ok(())
}

/// The recorded reason for a group send, if one was refused or informed.
pub(crate) fn send_reason(
    conn: &Connection,
    command_id: CommandId,
) -> Result<Option<SendRefusalReason>, Error> {
    let value: Option<String> = conn
        .query_row(
            "SELECT v FROM metadata WHERE k=?",
            [send_reason_key(command_id)],
            |row| row.get(0),
        )
        .optional()?;
    value
        .map(|value| serde_json::from_str(&value).map_err(|_| Error::Database))
        .transpose()
}

/// Composer-side group fields for a send to `recipients`. A send to two or more other members
/// with no roster yet writes the roster and its metadata event in the caller's transaction, and
/// the returned expectation is that new revision. A send to fewer than two other members is not
/// a group send.
pub(crate) fn prepare_group_send(
    conn: &Connection,
    ctx: &Ctx<'_>,
    conversation_id: ConversationId,
    recipients: &[String],
    group: &GroupDraftSend,
    now_ms: i64,
) -> Result<Option<GroupSendFields>, Error> {
    let members = group_send_members(conn, recipients)?;
    if members.len() < 2 {
        return Ok(None);
    }
    let name = group.group_name.as_deref().filter(|name| !name.is_empty());
    if name.is_some_and(|name| name.len() > GROUP_NAME_MAX_BYTES) {
        return Err(Error::InvalidRequest("group name"));
    }
    let expected_member_revision = if roster_exists(conn, conversation_id)? {
        group.expected_member_revision
    } else {
        Some(group_metadata::publish(
            conn,
            ctx,
            conversation_id,
            name,
            &members,
            now_ms,
        )?)
    };
    let group_create =
        group_mapping(conn, conversation_id)?
            .is_none()
            .then(|| GroupCreateRequest {
                name: name.map(str::to_owned),
                confirm_duplicate: group.group_create_confirm_duplicate,
            });
    Ok(Some(GroupSendFields {
        expected_member_revision,
        group_create,
    }))
}

/// Other members of a send, canonical and sorted, with the vault's own addresses removed.
fn group_send_members(conn: &Connection, recipients: &[String]) -> Result<BTreeSet<String>, Error> {
    let region = conversation_match::identity_region(conn)?;
    let own = own_addresses(conn)?;
    let members = recipients
        .iter()
        .map(|recipient| canonical_address(recipient, Some(&region)))
        .filter(|address| !own.contains(address))
        .collect::<BTreeSet<_>>();
    if members.iter().any(|member| !canonical_e164(member)) {
        return Err(Error::InvalidRequest("group recipients must be canonical"));
    }
    Ok(members)
}

fn roster_exists(conn: &Connection, conversation_id: ConversationId) -> Result<bool, Error> {
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM conversation_group_members WHERE conversation_id=?)",
        [conversation_id.to_string()],
        |row| row.get(0),
    )?)
}

/// Domain separator for the group proof digest, so it can never equal a 1:1 proof digest.
const GROUP_PROOF_DOMAIN: &str = "peppy-rcs-group-proof-v1";

/// Stored group state that authorizes one native group RCS send. Prepare captures it and begin
/// re-derives it; the handoff carries only its fields, never Google identifiers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GroupSendAuthority {
    pub conversation_id: ConversationId,
    pub google_conversation_id: String,
    pub proof_version: u32,
    /// The proved member revision, carried as the handoff's mapping revision.
    pub mapping_revision: u64,
    /// Go's digest of the proved member set; the prepare and commit handoffs both carry it.
    pub member_set_digest: String,
    /// The proof observation time, carried as the handoff's verified time.
    pub verified_at_ms: i64,
    pub identity_region: String,
    pub own_identity: String,
    pub proof_digest: [u8; 32],
    pub roster: BTreeSet<String>,
}

/// Outcome of checking a fresh target observation against a prepared group authority.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GroupObservationCheck {
    Exact,
    /// The roster or Google's member digest differs from the proof: refuse and pause (I1).
    MembersChanged,
    /// The observation is stale or contradicts the stored mapping: refuse without a pause.
    Unproven,
}

/// The member set one fresh target observation proves, and Go's digest over the same numbers.
struct ObservedRoster {
    roster: BTreeSet<String>,
    digest: String,
}

/// Captures the proved group a native send may use, or the definite reason it may not. The
/// recipients must be exactly the proved roster, and the composer's revision the proved one.
pub(crate) fn native_send_authority(
    conn: &Connection,
    conversation_id: ConversationId,
    command_id: CommandId,
    recipients: &[String],
    now_ms: i64,
) -> Result<GroupSendAuthority, Error> {
    let mapping =
        group_mapping(conn, conversation_id)?.ok_or(Error::InvalidRequest("mapping unproven"))?;
    match mapping.state {
        GroupMappingState::Live => {}
        GroupMappingState::Paused => return Err(Error::GroupMembersChanged),
        GroupMappingState::Withdrawn => return Err(Error::InvalidRequest("group withdrawn")),
        GroupMappingState::Unlinked | GroupMappingState::Proving => {
            return Err(Error::InvalidRequest("mapping unproven"));
        }
    }
    if !rcs_mapping::is_fresh(now_ms, mapping.proof_observed_at_ms) {
        return Err(Error::InvalidRequest("mapping unproven"));
    }
    if send_fields(conn, command_id)?.expected_member_revision
        != Some(mapping.proved_member_revision)
    {
        return Err(Error::GroupMembersChanged);
    }
    let proved_roster = roster(conn, conversation_id)?;
    if proved_roster.len() < 2 || group_send_members(conn, recipients)? != proved_roster {
        return Err(Error::InvalidRequest("group recipients changed"));
    }
    let verified_at_ms = mapping.proof_observed_at_ms;
    proof_authority(conn, &mapping, proved_roster, verified_at_ms)?
        .ok_or(Error::InvalidRequest("mapping unproven"))
}

/// The authority a stored group mapping proves, stamped with `verified_at_ms`. `None` when this
/// vault has no resolved identity region or confirmed own identity to prove against.
fn proof_authority(
    conn: &Connection,
    mapping: &GroupMapping,
    proved_roster: BTreeSet<String>,
    verified_at_ms: i64,
) -> Result<Option<GroupSendAuthority>, Error> {
    let identity_region = crate::rcs_target::resolved_region(conn).map_err(|_| Error::Database)?;
    let own_identity = rcs_mapping::confirmed_own_identity(conn)?;
    let (Some(identity_region), Some(own_identity)) = (identity_region, own_identity) else {
        return Ok(None);
    };
    let proof_digest = group_proof_digest(
        mapping.conversation_id,
        &mapping.google_conversation_id,
        mapping.proved_member_revision,
        &proved_roster,
        &mapping.member_set_digest,
    );
    Ok(Some(GroupSendAuthority {
        conversation_id: mapping.conversation_id,
        google_conversation_id: mapping.google_conversation_id.clone(),
        proof_version: rcs_mapping::RCS_MAPPING_PROOF_VERSION,
        mapping_revision: mapping.proved_member_revision,
        member_set_digest: mapping.member_set_digest.clone(),
        verified_at_ms,
        identity_region,
        own_identity,
        proof_digest,
        roster: proved_roster,
    }))
}

/// Outcome of re-proving a live group against one fresh target observation on the send path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GroupProofOutcome {
    /// The group is proved by this observation; its proof time is now the observation's.
    Proven,
    /// The observed members differ from the proof. The group is paused.
    MembersChanged,
    /// The observation is not a current proof of a live group. Nothing changed except context.
    Refused,
}

/// Re-proves a live group against the target observation the send is about to use. This is the
/// group analogue of the 1:1 per-send verification: the stored proof time becomes the
/// observation's, so the group's facts carry the same time as Go's route and target observations
/// and stay inside Go's observation TTL. Currency and I2 are checked exactly as `check_observation`.
pub(crate) fn prove_observation(
    conn: &Connection,
    ctx: &Ctx<'_>,
    observation: &RcsMappingObservation,
    now_ms: i64,
) -> Result<GroupProofOutcome, Error> {
    let Some(conversation_id) =
        group_conversation_for_google(conn, &observation.google_conversation_id)?
    else {
        return Ok(GroupProofOutcome::Refused);
    };
    let Some(mapping) = group_mapping(conn, conversation_id)? else {
        return Ok(GroupProofOutcome::Refused);
    };
    if mapping.state != GroupMappingState::Live {
        return Ok(GroupProofOutcome::Refused);
    }
    let proved_roster = roster(conn, conversation_id)?;
    let observed_at_ms = observation.observed_at_ms;
    let Some(authority) = proof_authority(conn, &mapping, proved_roster, observed_at_ms)? else {
        return Ok(GroupProofOutcome::Refused);
    };
    match check_observation(conn, &authority, observation, now_ms)? {
        GroupObservationCheck::Exact => {
            conn.execute(
                "UPDATE rcs_group_mapping SET proof_observed_at_ms=?,updated_at_ms=? WHERE conversation_id=? AND state='live' AND proved_member_revision=?",
                params![
                    observed_at_ms,
                    now_ms,
                    conversation_id.to_string(),
                    mapping.proved_member_revision
                ],
            )?;
            Ok(GroupProofOutcome::Proven)
        }
        GroupObservationCheck::MembersChanged => {
            pause_group(conn, ctx, conversation_id, now_ms)?;
            Ok(GroupProofOutcome::MembersChanged)
        }
        GroupObservationCheck::Unproven => Ok(GroupProofOutcome::Refused),
    }
}

/// The member digest a native group prepare must carry when the composer's revision and recipients
/// match the proved roster. Freshness is enforced at prepare, not while listing pending commands.
pub(crate) fn native_candidate_digest(
    conn: &Connection,
    conversation_id: ConversationId,
    command_id: CommandId,
    recipients: &[String],
) -> Result<Option<String>, Error> {
    let Some(mapping) = group_mapping(conn, conversation_id)? else {
        return Ok(None);
    };
    if mapping.state != GroupMappingState::Live
        || send_fields(conn, command_id)?.expected_member_revision
            != Some(mapping.proved_member_revision)
    {
        return Ok(None);
    }
    let proved_roster = roster(conn, conversation_id)?;
    let recipients_match = match group_send_members(conn, recipients) {
        Ok(members) => members == proved_roster,
        Err(_) => false,
    };
    Ok((proved_roster.len() >= 2 && recipients_match).then_some(mapping.member_set_digest))
}

/// Checks a fresh target observation against the authority captured at prepare (B / D1). Currency
/// mirrors the 1:1 rules; the member check is I2 over the observation plus Go's digest.
pub(crate) fn check_observation(
    conn: &Connection,
    authority: &GroupSendAuthority,
    observation: &RcsMappingObservation,
    now_ms: i64,
) -> Result<GroupObservationCheck, Error> {
    if !rcs_mapping::admission_context_is_v2(conn)?
        || rcs_mapping::update_context(conn, observation)? != rcs_mapping::ContextUpdate::Unchanged
    {
        return Ok(GroupObservationCheck::Unproven);
    }
    let current = observation.google_conversation_id == authority.google_conversation_id
        && observation.mapping_revision == authority.mapping_revision
        && observation.observed_at_ms == authority.verified_at_ms
        && observation.identity_region == authority.identity_region
        && observation.link_state == RcsLinkState::Linked
        && rcs_mapping::is_fresh(now_ms, observation.observed_at_ms)
        && rcs_mapping::canonical_recipient(&observation.selected_sim_own_identity).as_deref()
            == Some(authority.own_identity.as_str())
        && rcs_mapping::native_attestation_is_current(observation, now_ms, &authority.own_identity)
        && rcs_mapping::native_observation_is_current(observation, now_ms, &authority.own_identity)
        && observation.conversation_type == RcsConversationType::Rcs
        && observation.status == "eligible"
        && observation.send_mode == RcsSendMode::Automatic
        && !observation.read_only
        && observation.default_outgoing
        && observation.google_candidate_count == 1;
    if !current {
        return Ok(GroupObservationCheck::Unproven);
    }
    let Some(observed) = observed_roster(conn, observation)? else {
        return Ok(GroupObservationCheck::MembersChanged);
    };
    if observed.roster != authority.roster || observed.digest != authority.member_set_digest {
        return Ok(GroupObservationCheck::MembersChanged);
    }
    Ok(GroupObservationCheck::Exact)
}

/// The group conversation that owns `google_conversation_id`, if any.
pub(crate) fn group_conversation_for_google(
    conn: &Connection,
    google_conversation_id: &str,
) -> Result<Option<ConversationId>, Error> {
    conn.query_row(
        "SELECT conversation_id FROM rcs_group_mapping WHERE google_conversation_id=?",
        [google_conversation_id],
        |row| row.get::<_, String>(0),
    )
    .optional()?
    .map(|id| crate::parse(&id).map_err(|_| Error::Database))
    .transpose()
}

/// I2 over one observation: visible, canonical, phone-typed participants with exactly one self,
/// whose non-self set minus every vault own address has at least two members. `None` when not
/// provable. The digest uses the same non-self numbers Go hashes, before own-address removal.
fn observed_roster(
    conn: &Connection,
    observation: &RcsMappingObservation,
) -> Result<Option<ObservedRoster>, Error> {
    let region = conversation_match::identity_region(conn)?;
    let own = own_addresses(conn)?;
    let mut self_count = 0;
    let mut members = BTreeSet::new();
    for participant in &observation.participants {
        let address = canonical_address(&participant.identity, Some(&region));
        if !participant.is_visible
            || participant.identifier_type != PHONE_IDENTIFIER_TYPE
            || !canonical_e164(&address)
        {
            return Ok(None);
        }
        if participant.is_me {
            self_count += 1;
        } else {
            members.insert(address);
        }
    }
    let roster = members
        .iter()
        .filter(|address| !own.contains(*address))
        .cloned()
        .collect::<BTreeSet<_>>();
    if self_count != 1 || roster.len() < 2 {
        return Ok(None);
    }
    Ok(Some(ObservedRoster {
        roster,
        digest: member_set_digest(&members),
    }))
}

/// Go's member digest: SHA-256 of the sorted, deduplicated canonical numbers joined by LF, base64.
pub(crate) fn member_set_digest(members: &BTreeSet<String>) -> String {
    let joined = members
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join("\n");
    STANDARD.encode(Sha256::digest(joined.as_bytes()))
}

/// The 32-byte group proof: a length-prefixed encoding of the domain, the Peppy and Google ids, the
/// proved revision, the sorted roster, and the Go digest. It is derived from stored fields only.
fn group_proof_digest(
    conversation_id: ConversationId,
    google_conversation_id: &str,
    member_revision: u64,
    roster: &BTreeSet<String>,
    member_set_digest: &str,
) -> [u8; 32] {
    let mut digest = Sha256::new();
    absorb(&mut digest, GROUP_PROOF_DOMAIN.as_bytes());
    absorb(&mut digest, conversation_id.to_string().as_bytes());
    absorb(&mut digest, google_conversation_id.as_bytes());
    absorb(&mut digest, &member_revision.to_be_bytes());
    absorb(&mut digest, &(roster.len() as u64).to_be_bytes());
    for member in roster {
        absorb(&mut digest, member.as_bytes());
    }
    absorb(&mut digest, member_set_digest.as_bytes());
    digest.finalize().into()
}

fn absorb(digest: &mut Sha256, bytes: &[u8]) {
    digest.update((bytes.len() as u64).to_be_bytes());
    digest.update(bytes);
}

#[cfg(test)]
mod tests {
    use super::*;
    use peppy_crypto::{create_vault_check_header, derive_root_key};
    use serde_json::{Value, json};
    use std::sync::atomic::{AtomicI64, Ordering};

    use crate::rcs_mapping::{
        RcsConversationType, RcsLinkState, RcsMappingObservation, RcsParticipantObservation,
        RcsSendMode,
    };
    use crate::rcs_native::{
        NativeCarrierRouteBinding, NativeOwnNumberSource, NativeRcsEvidence, NativeRcsLifetime,
        NativeRcsOutgoing, NativeRcsPreparationTarget, NativeRcsSimPayload, NativeRcsTargetClass,
        NativeSettingsCurrentness, NativeSettingsProvenance,
    };
    use crate::{
        Client, ClientConfig, CommandId, Cursor, DatabaseKey, KeyProfile, MessageId, MessageRecord,
        SendState, SnapshotPurpose, SnapshotRecord, SourceSequence, VaultId,
        rcs_group_create::GroupLinkState,
    };
    use std::collections::HashSet;

    /// Unique per-row sequence so seeded messages never collide on (device, sequence).
    static NEXT_SEQUENCE: AtomicI64 = AtomicI64::new(1);

    const PASSPHRASE: &str = "correct horse battery staple";
    const SELF: &str = "+15550000001";
    const OTHER_PHONE: &str = "+15550000002";
    const ALEX: &str = "+15551230001";
    const SAM: &str = "+15551230002";
    const LEE: &str = "+15551230003";
    const NOW: i64 = 1_800_000_000_000;
    const HOUR: i64 = 3_600_000;

    /// The keys of one vault, shared by every device opened on it.
    #[derive(Clone)]
    struct VaultKeys {
        id: VaultId,
        profile: KeyProfile,
        header: peppy_crypto::VaultCheckHeader,
    }

    fn new_vault_keys() -> VaultKeys {
        let id = VaultId::new();
        let profile = KeyProfile::new(id.0, 1).unwrap();
        let root = derive_root_key(PASSPHRASE, &profile).unwrap();
        let header = create_vault_check_header(&root, profile.clone()).unwrap();
        VaultKeys {
            id,
            profile,
            header,
        }
    }

    struct Gateway {
        _dir: tempfile::TempDir,
        client: Client,
        device: DeviceId,
        keys: VaultKeys,
    }

    impl Gateway {
        fn open() -> Self {
            Self::open_on(&new_vault_keys())
        }

        /// A device whose server supports compaction, so its group events carry `supersedes`.
        fn open_compacting() -> Self {
            let gateway = Self::open();
            gateway
                .client
                .set_server_compaction_state(true, true)
                .unwrap();
            gateway
        }

        fn open_on(keys: &VaultKeys) -> Self {
            let dir = tempfile::TempDir::new().unwrap();
            let device = DeviceId::new();
            let config = ClientConfig {
                database_path: dir.path().join("groups.db"),
                vault_id: keys.id,
                device_id: device,
            };
            let client = Client::open(config, DatabaseKey::new(&[7; 32]).unwrap()).unwrap();
            client
                .unlock(&keys.profile, &keys.header, PASSPHRASE)
                .unwrap();
            Self {
                _dir: dir,
                client,
                device,
                keys: keys.clone(),
            }
        }

        /// Compacts this device's log the way the server does (an event is retired once a
        /// retained event supersedes it), then restores a new device on the same vault from the
        /// retained records alone through the snapshot import path.
        fn restored_from_compaction(&self) -> Gateway {
            let log = self.client.pending_outbox().unwrap();
            let superseded = log
                .iter()
                .filter_map(|envelope| envelope.compaction.as_ref())
                .flat_map(|compaction| compaction.supersedes.iter())
                .map(|reference| (reference.producer_device_id, reference.producer_sequence.0))
                .collect::<HashSet<_>>();
            let records = log
                .iter()
                .enumerate()
                .filter(|(_, envelope)| {
                    !superseded
                        .contains(&(envelope.producer_device_id, envelope.producer_sequence.0))
                })
                .map(|(index, envelope)| SnapshotRecord {
                    cursor: Cursor(index as u64 + 1),
                    envelope: envelope.clone(),
                })
                .collect::<Vec<_>>();
            assert!(
                records.len() < log.len(),
                "compaction must retire at least one superseded event"
            );
            let restored = Gateway::open_on(&self.keys);
            restored
                .client
                .set_server_compaction_state(true, true)
                .unwrap();
            let session = restored
                .client
                .begin_snapshot_with_compaction(
                    Cursor(log.len() as u64),
                    records.len() as u64,
                    SnapshotPurpose::Resync,
                    Some(1),
                )
                .unwrap();
            restored
                .client
                .append_snapshot_page(session.generation, &records)
                .unwrap();
            restored.client.finish_snapshot(session.generation).unwrap();
            while restored
                .client
                .apply_pending(100)
                .unwrap()
                .snapshot_remaining
                > 0
            {}
            restored
        }

        /// Records an own address as the vault's synced identity table would.
        fn own_address(&self, device: &str, address: &str) {
            self.client
                .lock()
                .unwrap()
                .conn
                .execute(
                    "INSERT INTO mms_own_addresses(source_device_id,subscription_id,address,revision) VALUES(?,'sim-1',?,1)",
                    params![device, address],
                )
                .unwrap();
        }

        fn ingest(&self, snapshot: &Value) -> Result<GroupIngestReport, Error> {
            let mut store = self.client.lock().unwrap();
            let (conn, ctx) = store.parts();
            let snapshot =
                serde_json::from_value::<ConversationCandidateSnapshot>(snapshot.clone())
                    .map_err(|_| Error::InvalidRequest("test snapshot"))?;
            ingest_snapshot(conn, &ctx, &snapshot, NOW)
        }

        fn mapping(&self, conversation_id: ConversationId) -> Option<GroupMapping> {
            group_mapping(&self.client.lock().unwrap().conn, conversation_id).unwrap()
        }

        fn mapping_by_google(&self, google: &str) -> Option<GroupMapping> {
            let store = self.client.lock().unwrap();
            let conversation: Option<String> = store
                .conn
                .query_row(
                    "SELECT conversation_id FROM rcs_group_mapping WHERE google_conversation_id=?",
                    [google],
                    |row| row.get(0),
                )
                .optional()
                .unwrap();
            conversation
                .and_then(|id| group_mapping(&store.conn, crate::parse(&id).unwrap()).unwrap())
        }

        fn roster_of(&self, conversation_id: ConversationId) -> BTreeSet<String> {
            roster(&self.client.lock().unwrap().conn, conversation_id).unwrap()
        }

        fn member_revision(&self, conversation_id: ConversationId) -> u64 {
            self.client
                .lock()
                .unwrap()
                .conn
                .query_row(
                    "SELECT member_revision FROM conversation_group_meta WHERE conversation_id=?",
                    [conversation_id.to_string()],
                    |row| row.get(0),
                )
                .unwrap()
        }

        /// Pins a 1:1 mapping row onto `conversation_id` (any state) to exercise mutual exclusion.
        fn one_to_one_row(&self, conversation_id: ConversationId) {
            self.client
                .lock()
                .unwrap()
                .conn
                .execute(
                    "INSERT INTO rcs_conversation_mappings(peppy_conversation_id,google_conversation_id,class,proof_version,proof_hash,mapping_revision,verified_at_ms,invalidated_at_ms,invalidation_code) VALUES(?,'one-to-one-google','one_to_one',2,zeroblob(32),1,?,?,'test')",
                    params![conversation_id.to_string(), NOW, NOW],
                )
                .unwrap();
        }

        /// Seeds a conversation with a synced roster, as a prior link or create would.
        fn seed_roster(&self, members: &[&str], name: Option<&str>) -> ConversationId {
            let conversation_id = ConversationId::new();
            let set = members
                .iter()
                .map(|m| m.to_string())
                .collect::<BTreeSet<_>>();
            let mut store = self.client.lock().unwrap();
            let (conn, ctx) = store.parts();
            group_metadata::publish(conn, &ctx, conversation_id, name, &set, NOW).unwrap();
            conversation_id
        }

        /// Seeds a 1:1-style message history so a conversation has activity and a newest message.
        fn seed_message_at(&self, conversation_id: ConversationId, sort_at_ms: i64) {
            self.client
                .lock()
                .unwrap()
                .conn
                .execute(
                    "INSERT INTO messages(id,conversation_id,source_device_id,source_sequence,payload,seen,sort_at_ms) VALUES(?,?,?,?,'{}',1,?)",
                    params![
                        uuid::Uuid::new_v4().to_string(),
                        conversation_id.to_string(),
                        self.device.to_string(),
                        NEXT_SEQUENCE.fetch_add(1, Ordering::Relaxed),
                        sort_at_ms
                    ],
                )
                .unwrap();
        }
    }

    fn participant(number: &str, visible: bool, is_self: bool, name: &str) -> Value {
        json!({
            "participant_id": format!("p-{number}"),
            "identifier_type": 1,
            "raw_number": number,
            "formatted_number": number,
            "participant_id_present": true,
            "number_present": true,
            "visible": visible,
            "self": is_self,
            "canonical_address": number,
            "canonical_address_present": true,
            "display_name": name,
        })
    }

    /// Google's view of a group: `self` plus the given visible non-self numbers.
    fn group(id: &str, others: &[&str], name: &str, last_message_ms: i64) -> Value {
        let mut participants = vec![participant(SELF, true, true, "Me")];
        participants.extend(
            others
                .iter()
                .map(|number| participant(number, true, false, "")),
        );
        json!({
            "google_conversation_id": id,
            "conversation_type": 2,
            "conversation_status": 1,
            "classification": "eligible",
            "group": true,
            "visible_non_self_count": others.len(),
            "last_message_ms": last_message_ms,
            "participants": participants,
            "name": name,
            "is_group_chat": true,
            "read_only": false,
            "invisible_participant_count": 0,
            "member_digest": format!("digest-{id}-{}", others.len()),
        })
    }

    fn snapshot(revision: u64, conversations: Vec<Value>) -> Value {
        json!({
            "code": "conversation_candidate_snapshot",
            "snapshot_token": format!("1:2:{revision}"),
            "generation": 1,
            "stream_epoch": 2,
            "revision": revision,
            "run_incarnation": "run",
            "pairing_evidence_id": "pairing",
            "complete": true,
            "conversations": conversations,
        })
    }

    fn group_with(mut record: Value, f: impl FnOnce(&mut Value)) -> Value {
        f(&mut record);
        record
    }

    #[test]
    fn proof_subtracts_every_vault_own_address_including_the_other_phone() {
        let gateway = Gateway::open();
        gateway.own_address(&gateway.device.to_string(), SELF);
        gateway.own_address("other-phone", OTHER_PHONE);
        // Google lists the other phone as a visible non-self participant: it is ours, not a member.
        let record = group("g1", &[OTHER_PHONE, ALEX, SAM], "Weekend", NOW);
        let parsed: ConversationCandidateRecord = serde_json::from_value(record).unwrap();
        let (proof, members) =
            classify_record(&gateway.client.lock().unwrap().conn, &parsed).unwrap();
        assert_eq!(proof, GroupProof::Provable);
        assert_eq!(members, BTreeSet::from([ALEX.to_owned(), SAM.to_owned()]));
    }

    #[test]
    fn two_gateways_match_the_same_roster_when_each_subtracts_all_own_addresses() {
        let gateway = Gateway::open();
        gateway.own_address(&gateway.device.to_string(), SELF);
        gateway.own_address("other-phone", OTHER_PHONE);
        let conversation = gateway.seed_roster(&[ALEX, SAM], Some("Weekend"));
        let report = gateway
            .ingest(&snapshot(
                1,
                vec![group("g1", &[OTHER_PHONE, ALEX, SAM], "Weekend", NOW)],
            ))
            .unwrap();
        assert_eq!(report.linked, 1);
        assert_eq!(
            gateway.mapping(conversation).unwrap().state,
            GroupMappingState::Live
        );
    }

    #[test]
    fn one_visible_non_self_is_not_provable_and_never_links_to_a_one_to_one() {
        let gateway = Gateway::open();
        let one_to_one = gateway.seed_roster(&[ALEX], None);
        let report = gateway
            .ingest(&snapshot(1, vec![group("g1", &[ALEX], "", NOW)]))
            .unwrap();
        assert_eq!(report.not_provable, 1);
        assert_eq!(report.linked, 0);
        assert!(gateway.mapping(one_to_one).is_none());
    }

    #[test]
    fn shrunk_to_two_group_keeps_its_own_conversation_and_does_not_join_a_one_to_one() {
        let gateway = Gateway::open();
        let group_conversation = gateway.seed_roster(&[ALEX, SAM, LEE], Some("Trio"));
        gateway
            .ingest(&snapshot(
                1,
                vec![group("g1", &[ALEX, SAM, LEE], "Trio", NOW)],
            ))
            .unwrap();
        // The group shrinks to two members: the existing 1:1 with Alex must not be linked.
        let one_to_one = gateway.seed_roster(&[ALEX], None);
        gateway.one_to_one_row(one_to_one);
        let report = gateway
            .ingest(&snapshot(2, vec![group("g1", &[ALEX, SAM], "Trio", NOW)]))
            .unwrap();
        assert_eq!(report.members_changed, 1);
        assert!(gateway.mapping(one_to_one).is_none());
        assert_eq!(
            gateway.mapping(group_conversation).unwrap().state,
            GroupMappingState::Live
        );
        assert_eq!(
            gateway.roster_of(group_conversation),
            BTreeSet::from([ALEX.to_owned(), SAM.to_owned()])
        );
    }

    #[test]
    fn one_to_one_mapped_conversation_is_never_linked_to_a_group() {
        let gateway = Gateway::open();
        let conversation = gateway.seed_roster(&[ALEX, SAM], None);
        gateway.one_to_one_row(conversation);
        let report = gateway
            .ingest(&snapshot(1, vec![group("g1", &[ALEX, SAM], "", NOW)]))
            .unwrap();
        assert_eq!(report.linked, 0);
        assert!(gateway.mapping(conversation).is_none());
        assert!(gateway.mapping_by_google("g1").is_some());
    }

    #[test]
    fn identical_set_tie_break_prefers_most_recent_activity_then_conversation_id() {
        let gateway = Gateway::open();
        let older = gateway.seed_roster(&[ALEX, SAM], None);
        let newer = gateway.seed_roster(&[ALEX, SAM], None);
        gateway.seed_message_at(older, NOW - 2 * HOUR);
        gateway.seed_message_at(newer, NOW - HOUR);
        let report = gateway
            .ingest(&snapshot(1, vec![group("g1", &[ALEX, SAM], "", NOW)]))
            .unwrap();
        assert_eq!(report.linked, 1);
        assert!(gateway.mapping(newer).is_some());
        assert!(gateway.mapping(older).is_none());
    }

    #[test]
    fn identical_set_with_equal_activity_resolves_by_conversation_id() {
        let gateway = Gateway::open();
        let first = gateway.seed_roster(&[ALEX, SAM], None);
        let second = gateway.seed_roster(&[ALEX, SAM], None);
        gateway.seed_message_at(first, NOW - HOUR);
        gateway.seed_message_at(second, NOW - HOUR);
        gateway
            .ingest(&snapshot(1, vec![group("g1", &[ALEX, SAM], "", NOW)]))
            .unwrap();
        let expected = if first.to_string() < second.to_string() {
            first
        } else {
            second
        };
        assert!(gateway.mapping(expected).is_some());
    }

    #[test]
    fn unmatched_group_with_recent_activity_creates_a_conversation_but_dormant_one_does_not() {
        let gateway = Gateway::open();
        // The claim floor is set on the first decision: now - 15 min.
        let report = gateway
            .ingest(&snapshot(
                1,
                vec![
                    group("dormant", &[ALEX, SAM], "Old", NOW - HOUR),
                    group("active", &[SAM, LEE], "New", NOW - 60_000),
                ],
            ))
            .unwrap();
        assert_eq!(report.created, 1);
        assert_eq!(report.dormant, 1);
        assert!(gateway.mapping_by_google("dormant").is_none());
        let created = gateway.mapping_by_google("active").unwrap();
        assert_eq!(created.state, GroupMappingState::Live);
        assert_eq!(
            gateway.roster_of(created.conversation_id),
            BTreeSet::from([SAM.to_owned(), LEE.to_owned()])
        );
    }

    #[test]
    fn dormant_group_creates_on_its_next_message_after_the_floor() {
        let gateway = Gateway::open();
        gateway
            .ingest(&snapshot(
                1,
                vec![group("g1", &[ALEX, SAM], "", NOW - HOUR)],
            ))
            .unwrap();
        assert!(gateway.mapping_by_google("g1").is_none());
        let report = gateway
            .ingest(&snapshot(2, vec![group("g1", &[ALEX, SAM], "", NOW)]))
            .unwrap();
        assert_eq!(report.created, 1);
        assert!(gateway.mapping_by_google("g1").is_some());
    }

    #[test]
    fn member_change_pauses_replaces_roster_bumps_revision_and_reproves_live() {
        let gateway = Gateway::open();
        gateway
            .ingest(&snapshot(
                1,
                vec![group("g1", &[ALEX, SAM], "Weekend", NOW)],
            ))
            .unwrap();
        let mapping = gateway.mapping_by_google("g1").unwrap();
        let before = gateway.member_revision(mapping.conversation_id);
        let report = gateway
            .ingest(&snapshot(
                2,
                vec![group("g1", &[ALEX, SAM, LEE], "Weekend", NOW)],
            ))
            .unwrap();
        assert_eq!(report.members_changed, 1);
        assert_eq!(report.live, 1);
        let after = gateway.mapping_by_google("g1").unwrap();
        assert_eq!(after.state, GroupMappingState::Live);
        assert_eq!(after.proved_member_revision, before + 1);
        assert_eq!(gateway.member_revision(after.conversation_id), before + 1);
        assert_eq!(after.member_set_digest, "digest-g1-3");
        assert_eq!(
            gateway.roster_of(after.conversation_id),
            BTreeSet::from([ALEX.to_owned(), SAM.to_owned(), LEE.to_owned()])
        );
    }

    #[test]
    fn member_change_retires_legacy_mms_thread_rows_for_the_conversation() {
        let gateway = Gateway::open();
        gateway
            .ingest(&snapshot(1, vec![group("g1", &[ALEX, SAM], "", NOW)]))
            .unwrap();
        let conversation = gateway.mapping_by_google("g1").unwrap().conversation_id;
        gateway.client.lock().unwrap().conn.execute(
            "INSERT INTO mms_thread_conversations(source_device_id,source_generation,subscription_id,provider_thread_id,conversation_id) VALUES('dev',1,'sim-1','thread-1',?)",
            [conversation.to_string()],
        ).unwrap();
        gateway
            .ingest(&snapshot(2, vec![group("g1", &[ALEX, LEE], "", NOW)]))
            .unwrap();
        let rows: i64 = gateway
            .client
            .lock()
            .unwrap()
            .conn
            .query_row(
                "SELECT COUNT(*) FROM mms_thread_conversations WHERE conversation_id=?",
                [conversation.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(rows, 0);
    }

    #[test]
    fn unprovable_live_group_pauses_and_recovers_when_the_proof_returns() {
        let gateway = Gateway::open();
        gateway
            .ingest(&snapshot(1, vec![group("g1", &[ALEX, SAM], "", NOW)]))
            .unwrap();
        let conversation = gateway.mapping_by_google("g1").unwrap().conversation_id;
        let paused = group_with(group("g1", &[ALEX, SAM], "", NOW), |record| {
            record["participants"][1]["canonical_address_present"] = json!(false);
            record["participants"][1]["canonical_address"] = json!("");
        });
        let report = gateway.ingest(&snapshot(2, vec![paused])).unwrap();
        assert_eq!(report.paused, 1);
        assert_eq!(
            gateway.mapping(conversation).unwrap().state,
            GroupMappingState::Paused
        );
        assert_eq!(
            proved_member_revision(&gateway.client.lock().unwrap().conn, conversation).unwrap(),
            None
        );
        gateway
            .ingest(&snapshot(3, vec![group("g1", &[ALEX, SAM], "", NOW)]))
            .unwrap();
        assert_eq!(
            gateway.mapping(conversation).unwrap().state,
            GroupMappingState::Live
        );
    }

    #[test]
    fn group_absent_from_a_complete_snapshot_is_withdrawn() {
        let gateway = Gateway::open();
        gateway
            .ingest(&snapshot(1, vec![group("g1", &[ALEX, SAM], "", NOW)]))
            .unwrap();
        let report = gateway.ingest(&snapshot(2, vec![])).unwrap();
        assert_eq!(report.withdrawn, 1);
        let conversation = gateway.mapping_by_google("g1").unwrap().conversation_id;
        assert_eq!(
            gateway.mapping(conversation).unwrap().state,
            GroupMappingState::Withdrawn
        );
    }

    #[test]
    fn read_only_or_self_left_group_is_withdrawn_and_is_not_relinked() {
        let gateway = Gateway::open();
        gateway
            .ingest(&snapshot(1, vec![group("g1", &[ALEX, SAM], "", NOW)]))
            .unwrap();
        let read_only = group_with(group("g1", &[ALEX, SAM], "", NOW), |record| {
            record["read_only"] = json!(true);
        });
        assert_eq!(
            gateway
                .ingest(&snapshot(2, vec![read_only]))
                .unwrap()
                .withdrawn,
            1
        );
        let self_left = group_with(group("g2", &[ALEX, SAM], "", NOW), |record| {
            record["participants"][0]["self"] = json!(false);
        });
        let report = gateway.ingest(&snapshot(3, vec![self_left])).unwrap();
        assert_eq!(report.not_provable, 1);
        assert!(gateway.mapping_by_google("g2").is_none());
    }

    #[test]
    fn group_claimed_by_another_gateway_is_a_conflict_not_a_second_link() {
        let gateway = Gateway::open();
        let conversation = ConversationId::new();
        let other_gateway = DeviceId::new();
        gateway.client.lock().unwrap().conn.execute(
            "INSERT INTO rcs_group_mapping(conversation_id,google_conversation_id,gateway_device_id,state,proved_member_revision,member_set_digest,proof_observed_at_ms,snapshot_token,created_at_ms,updated_at_ms) VALUES(?,'g1',?,'live',1,'d',?,'tok',?,?)",
            params![conversation.to_string(), other_gateway.to_string(), NOW, NOW, NOW],
        ).unwrap();
        let report = gateway
            .ingest(&snapshot(1, vec![group("g1", &[ALEX, SAM], "", NOW)]))
            .unwrap();
        assert_eq!(report.conflicts, 1);
        assert_eq!(report.created, 0);
        // The other gateway's mapping is untouched and no second mapping appeared for g1.
        let mapping = gateway.mapping(conversation).unwrap();
        assert_eq!(mapping.gateway_device_id, other_gateway);
        assert_eq!(mapping.state, GroupMappingState::Live);
    }

    #[test]
    fn ownership_claim_blocks_another_group_from_taking_a_conversation() {
        let gateway = Gateway::open();
        let conversation = gateway.seed_roster(&[ALEX, SAM], None);
        gateway
            .client
            .lock()
            .unwrap()
            .conn
            .execute(
                "INSERT INTO metadata(k,v) VALUES(?,'some-other-google-group')",
                [format!("{GROUP_OWNER_PREFIX}{conversation}")],
            )
            .unwrap();
        let report = gateway
            .ingest(&snapshot(1, vec![group("g1", &[ALEX, SAM], "", NOW)]))
            .unwrap();
        assert_eq!(report.linked, 0);
        assert!(gateway.mapping(conversation).is_none());
    }

    #[test]
    fn name_change_emits_an_event_and_an_absent_name_does_not_clear() {
        let gateway = Gateway::open();
        gateway
            .ingest(&snapshot(
                1,
                vec![group("g1", &[ALEX, SAM], "Weekend", NOW)],
            ))
            .unwrap();
        let conversation = gateway.mapping_by_google("g1").unwrap().conversation_id;
        gateway
            .ingest(&snapshot(
                2,
                vec![group("g1", &[ALEX, SAM], "Weekend trip", NOW)],
            ))
            .unwrap();
        assert_eq!(name_of(&gateway, conversation), Some("Weekend trip".into()));
        gateway
            .ingest(&snapshot(3, vec![group("g1", &[ALEX, SAM], "", NOW)]))
            .unwrap();
        assert_eq!(name_of(&gateway, conversation), Some("Weekend trip".into()));
    }

    /// Stable label for a carrier admission, so the matrix below reads as a table.
    fn admission_label(admission: crate::rcs_send::CarrierAdmission) -> &'static str {
        use crate::rcs_send::CarrierAdmission as A;
        match admission {
            A::Permitted => "permitted",
            A::ShortCodeSms => "short_code_sms",
            A::GroupMmsPreM4 => "group_mms_pre_m4",
            A::Blocked => "blocked",
            A::InvalidateProof => "invalidate_proof",
            A::GroupRefused(reason) => reason.code(),
        }
    }

    #[test]
    fn group_state_gates_carrier_mms_before_the_unmapped_rules() {
        let gateway = Gateway::open();
        let conversation = gateway.seed_roster(&[ALEX, SAM], None);
        let recipients = [ALEX.to_owned(), SAM.to_owned()];
        let client = gateway.client.lock().unwrap();
        let conn = &client.conn;
        conn.execute(
            "INSERT INTO rcs_group_mapping(conversation_id,google_conversation_id,gateway_device_id,state,proved_member_revision,member_set_digest,proof_observed_at_ms,snapshot_token,created_at_ms,updated_at_ms) VALUES(?,'g1',?,'live',1,'d',?,'tok',?,?)",
            params![conversation.to_string(), DeviceId::new().to_string(), NOW, NOW, NOW],
        )
        .unwrap();
        let admit = |conn: &Connection, transport: crate::Transport, expected: Option<u64>| {
            admission_label(
                crate::rcs_send::carrier_admission(
                    conn,
                    conversation,
                    &recipients,
                    transport,
                    transport != crate::Transport::Sms,
                    "sub-1",
                    NOW,
                    expected,
                )
                .unwrap(),
            )
        };
        let text = crate::Transport::Sms;
        let media = crate::Transport::Mms;
        let cases = [
            // Media into a receiving group is blocked, as a live 1:1 mapping is (D2).
            ("live", media, Some(1), "blocked"),
            ("live", media, None, "blocked"),
            ("paused", media, Some(1), "blocked"),
            // Text without the composer's revision must reload; a stale revision is refused.
            ("live", text, None, "update_required"),
            ("paused", text, None, "update_required"),
            ("live", text, Some(2), "members_changed"),
            ("paused", text, Some(1), "members_changed"),
            // The proved revision takes the native group RCS permit, never the carrier.
            ("live", text, Some(1), "blocked"),
            ("withdrawn", text, Some(1), "withdrawn"),
            ("withdrawn", media, None, "withdrawn"),
            ("proving", text, Some(1), "blocked"),
            ("proving", media, None, "blocked"),
            // Unmapped rules: no link state at all means carrier MMS is permitted, as for 1:1.
            ("unlinked", media, Some(1), "permitted"),
        ];
        for (state, transport, expected, label) in cases {
            conn.execute("UPDATE rcs_group_mapping SET state=?1", [state])
                .unwrap();
            assert_eq!(
                admit(conn, transport, expected),
                label,
                "state {state}, transport {transport:?}, expected {expected:?}"
            );
        }
        conn.execute("DELETE FROM rcs_group_mapping", []).unwrap();
        assert_eq!(
            admit(conn, media, None),
            "permitted",
            "a 1:1-style conversation is unchanged"
        );
    }

    fn route_for(gateway: &Gateway) -> crate::GatewayRoute {
        crate::GatewayRoute::Carrier {
            gateway_device_id: gateway.device,
            subscription_id: "sub-1".to_owned(),
        }
    }

    fn draft_to(gateway: &Gateway, recipients: &[&str]) -> crate::ComposeDraft {
        gateway
            .client
            .save_new_compose_draft(crate::ComposeDraftUpdate {
                group_name: None,
                text: "hello".to_owned(),
                recipients: recipients.iter().map(|r| (*r).to_owned()).collect(),
                attachment_ids: Vec::new(),
                route: Some(route_for(gateway)),
            })
            .unwrap()
    }

    fn group_send(name: Option<&str>, expected: Option<u64>) -> GroupDraftSend {
        GroupDraftSend {
            group_name: name.map(str::to_owned),
            expected_member_revision: expected,
            group_create_confirm_duplicate: false,
        }
    }

    fn message_count(gateway: &Gateway, conversation: ConversationId) -> i64 {
        gateway
            .client
            .lock()
            .unwrap()
            .conn
            .query_row(
                "SELECT COUNT(*) FROM messages WHERE conversation_id=?",
                [conversation.to_string()],
                |row| row.get(0),
            )
            .unwrap()
    }

    #[test]
    fn first_group_send_writes_roster_metadata_and_command_together() {
        let gateway = Gateway::open();
        gateway.own_address(&gateway.device.to_string(), SELF);
        let draft = draft_to(&gateway, &[ALEX, SAM, SELF]);
        gateway
            .client
            .send_compose_draft_for_group(
                draft.draft_id,
                draft.revision,
                group_send(Some("Weekend"), None),
            )
            .unwrap();
        let conversation = draft.conversation_id;
        // Own addresses are not roster members.
        assert_eq!(
            gateway.roster_of(conversation),
            BTreeSet::from([ALEX.to_owned(), SAM.to_owned()])
        );
        assert_eq!(gateway.member_revision(conversation), 1);
        assert_eq!(name_of(&gateway, conversation), Some("Weekend".into()));
        assert_eq!(message_count(&gateway, conversation), 1);
    }

    #[test]
    fn group_send_rolls_back_roster_and_metadata_when_the_command_fails() {
        let gateway = Gateway::open();
        let draft = draft_to(&gateway, &[ALEX, SAM]);
        gateway
            .client
            .lock()
            .unwrap()
            .conn
            .execute_batch(
                "CREATE TRIGGER fail_group_command BEFORE INSERT ON outbox WHEN NEW.purpose='command' BEGIN SELECT RAISE(ABORT,'blocked'); END;",
            )
            .unwrap();
        let conversation = draft.conversation_id;
        assert!(
            gateway
                .client
                .send_compose_draft_for_group(
                    draft.draft_id,
                    draft.revision,
                    group_send(Some("Weekend"), None),
                )
                .is_err()
        );
        assert!(gateway.roster_of(conversation).is_empty());
        assert_eq!(name_of(&gateway, conversation), None);
        assert_eq!(message_count(&gateway, conversation), 0);
        let still_draft = gateway
            .client
            .compose_draft(draft.draft_id)
            .unwrap()
            .unwrap();
        assert_eq!(still_draft.revision, draft.revision);
        assert_eq!(still_draft.text, "hello");
    }

    #[test]
    fn group_send_fields_carry_the_roster_revision_and_the_create_request() {
        let gateway = Gateway::open();
        let draft = draft_to(&gateway, &[ALEX, SAM]);
        let mut store = gateway.client.lock().unwrap();
        let (conn, ctx) = store.parts();
        let tx = conn.transaction().unwrap();
        // First send: the roster is written here and its new revision is the expectation.
        let fields = prepare_group_send(
            &tx,
            &ctx,
            draft.conversation_id,
            &draft.recipients,
            &group_send(Some("Weekend"), None),
            NOW,
        )
        .unwrap()
        .unwrap();
        assert_eq!(fields.expected_member_revision, Some(1));
        assert_eq!(
            fields.group_create,
            Some(GroupCreateRequest {
                name: Some("Weekend".into()),
                confirm_duplicate: false,
            })
        );
        // A later send to the same roster carries the composer's revision and writes nothing.
        let later = prepare_group_send(
            &tx,
            &ctx,
            draft.conversation_id,
            &draft.recipients,
            &group_send(Some("Renamed"), Some(7)),
            NOW + 1,
        )
        .unwrap()
        .unwrap();
        assert_eq!(later.expected_member_revision, Some(7));
        assert_eq!(
            roster(&tx, draft.conversation_id).unwrap(),
            BTreeSet::from([ALEX.to_owned(), SAM.to_owned()])
        );
        assert_eq!(
            tx.query_row(
                "SELECT member_revision FROM conversation_group_meta WHERE conversation_id=?",
                [draft.conversation_id.to_string()],
                |row| row.get::<_, u64>(0),
            )
            .unwrap(),
            1
        );
    }

    #[test]
    fn one_recipient_send_with_group_fields_is_the_one_to_one_path() {
        let gateway = Gateway::open();
        let draft = draft_to(&gateway, &[ALEX]);
        gateway
            .client
            .send_compose_draft_for_group(
                draft.draft_id,
                draft.revision,
                group_send(Some("Ignored"), Some(3)),
            )
            .unwrap();
        let conversation = draft.conversation_id;
        assert!(gateway.roster_of(conversation).is_empty());
        assert_eq!(name_of(&gateway, conversation), None);
        assert_eq!(message_count(&gateway, conversation), 1);
    }

    #[test]
    fn group_send_name_longer_than_the_metadata_limit_is_refused_before_any_write() {
        let gateway = Gateway::open();
        let draft = draft_to(&gateway, &[ALEX, SAM]);
        let long = "n".repeat(GROUP_NAME_MAX_BYTES + 1);
        assert!(matches!(
            gateway.client.send_compose_draft_for_group(
                draft.draft_id,
                draft.revision,
                group_send(Some(&long), None),
            ),
            Err(Error::InvalidRequest("group name"))
        ));
        assert!(gateway.roster_of(draft.conversation_id).is_empty());
    }

    #[test]
    fn send_fields_round_trip_and_a_one_to_one_create_request_is_not_handled() {
        let gateway = Gateway::open();
        let command = CommandId::new();
        let store = gateway.client.lock().unwrap();
        let conn = &store.conn;
        assert_eq!(
            send_fields(conn, command).unwrap(),
            GroupSendFields::default()
        );
        let fields = GroupSendFields {
            expected_member_revision: Some(4),
            group_create: Some(GroupCreateRequest {
                name: None,
                confirm_duplicate: true,
            }),
        };
        record_send_fields(conn, command, &fields).unwrap();
        record_send_fields(conn, CommandId::new(), &GroupSendFields::default()).unwrap();
        assert_eq!(send_fields(conn, command).unwrap(), fields);
        // No roster yet: this is a one-to-one command, so the create gate does not apply.
        assert_eq!(
            crate::rcs_group_create::admit(conn, command, ConversationId::new()).unwrap(),
            crate::rcs_group_create::GroupCreateStart::NotHandled
        );
    }

    fn name_of(gateway: &Gateway, conversation: ConversationId) -> Option<String> {
        gateway
            .client
            .lock()
            .unwrap()
            .conn
            .query_row(
                "SELECT name FROM conversation_group_meta WHERE conversation_id=?",
                [conversation.to_string()],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()
            .unwrap()
            .flatten()
    }

    #[test]
    fn metadata_changes_record_local_notices_for_renames_and_roster_diffs() {
        let gateway = Gateway::open();
        gateway
            .ingest(&snapshot(
                1,
                vec![group("g1", &[ALEX, SAM], "Weekend", NOW)],
            ))
            .unwrap();
        let conversation = gateway.mapping_by_google("g1").unwrap().conversation_id;
        gateway
            .ingest(&snapshot(2, vec![group("g1", &[ALEX, LEE], "Trip", NOW)]))
            .unwrap();
        let notices =
            group_metadata::notices(&gateway.client.lock().unwrap().conn, conversation).unwrap();
        let kinds = notices
            .iter()
            .map(|n| (n.kind, n.name.clone(), n.member.clone()))
            .collect::<Vec<_>>();
        assert_eq!(
            kinds,
            vec![
                (
                    group_metadata::GroupNoticeKind::Renamed,
                    Some("Trip".into()),
                    None
                ),
                (
                    group_metadata::GroupNoticeKind::Added,
                    None,
                    Some(LEE.into())
                ),
                (
                    group_metadata::GroupNoticeKind::Removed,
                    None,
                    Some(SAM.into())
                ),
            ]
        );
    }

    #[test]
    fn metadata_from_another_device_is_applied_and_its_changes_are_noticed() {
        let gateway = Gateway::open();
        let conversation = gateway.seed_roster(&[ALEX, SAM], Some("Weekend"));
        // A remote writer's newer LWW event: name and roster change, applied locally.
        let remote = DeviceId::new();
        let event = group_metadata::ConversationGroupMetadata {
            conversation_id: conversation,
            name: Some("Remote name".into()),
            name_logical_revision: 50,
            name_writer_device_id: remote.to_string(),
            members: vec![ALEX.into(), LEE.into()],
            members_logical_revision: 50,
            members_writer_device_id: remote.to_string(),
            member_revision: 9,
            observed_at_ms: NOW,
            writer_device_id: remote.to_string(),
            link: None,
        };
        let applied =
            group_metadata::apply(&gateway.client.lock().unwrap().conn, &event, remote, 1).unwrap();
        assert!(applied);
        assert_eq!(gateway.member_revision(conversation), 9);
        let notices =
            group_metadata::notices(&gateway.client.lock().unwrap().conn, conversation).unwrap();
        assert_eq!(notices.len(), 3);
    }

    /// The device that first wrote the test group's name (clock 1). Every roster event carries it.
    const NAME_WRITER: &str = "00000000-0000-4000-8000-000000000001";
    /// The device that first wrote the test group's roster (clock 1).
    const SEEDER: &str = "00000000-0000-4000-8000-000000000002";

    fn roster_event(
        conversation: ConversationId,
        writer: DeviceId,
        members: &[&str],
        members_revision: u64,
        member_revision: u64,
    ) -> group_metadata::ConversationGroupMetadata {
        group_metadata::ConversationGroupMetadata {
            conversation_id: conversation,
            name: Some("Weekend".into()),
            name_logical_revision: 1,
            name_writer_device_id: NAME_WRITER.into(),
            members: members.iter().map(|member| (*member).to_owned()).collect(),
            members_logical_revision: members_revision,
            members_writer_device_id: writer.to_string(),
            member_revision,
            observed_at_ms: NOW,
            writer_device_id: writer.to_string(),
            link: None,
        }
    }

    #[test]
    fn a_skipped_intermediate_roster_adopts_the_winning_member_revision() {
        // Seen: A (rev 1). Skipped: B (rev 2). Received: A again as rev 3. The list is unchanged
        // locally, but the winning version's revision must still be adopted.
        let gateway = Gateway::open();
        let conversation = gateway.seed_roster(&[ALEX, SAM], Some("Weekend"));
        assert_eq!(gateway.member_revision(conversation), 1);
        let writer = DeviceId::new();
        let back_to_a = roster_event(conversation, writer, &[ALEX, SAM], 3, 3);
        assert!(
            group_metadata::apply(&gateway.client.lock().unwrap().conn, &back_to_a, writer, 1)
                .unwrap()
        );
        assert_eq!(gateway.member_revision(conversation), 3);
    }

    #[test]
    fn opposite_order_concurrent_rosters_converge_on_the_same_member_revision() {
        let x = DeviceId::new();
        let y = DeviceId::new();
        let winner_revision = if x.to_string() > y.to_string() { 2 } else { 3 };
        let to_lee = |conversation| roster_event(conversation, x, &[ALEX, LEE], 2, 2);
        let to_all = |conversation| roster_event(conversation, y, &[ALEX, SAM, LEE], 2, 3);
        let mut seen = Vec::new();
        for x_first in [true, false] {
            let gateway = Gateway::open();
            let conversation = gateway.seed_roster(&[ALEX, SAM], Some("Weekend"));
            let (first, second) = if x_first {
                (to_lee(conversation), to_all(conversation))
            } else {
                (to_all(conversation), to_lee(conversation))
            };
            for event in [first, second] {
                let writer = event.writer_device_id.parse::<DeviceId>().unwrap();
                group_metadata::apply(&gateway.client.lock().unwrap().conn, &event, writer, 1)
                    .unwrap();
            }
            seen.push((
                gateway.roster_of(conversation),
                gateway.member_revision(conversation),
            ));
        }
        assert_eq!(seen[0], seen[1]);
        assert_eq!(seen[0].1, winner_revision);
    }

    /// Name, roster and link as a device stores them: each register's value, writer and clock.
    type Registers = (
        u64,
        String,
        u64,
        String,
        u64,
        Option<String>,
        BTreeSet<String>,
    );

    fn registers(gateway: &Gateway, conversation: ConversationId) -> Registers {
        let store = gateway.client.lock().unwrap();
        let (name_revision, name_writer, members_revision, members_writer, member_revision, link) =
            store
                .conn
                .query_row(
                    "SELECT name_logical_revision,name_writer_device_id,members_logical_revision,members_writer_device_id,member_revision,link_state FROM conversation_group_meta WHERE conversation_id=?",
                    [conversation.to_string()],
                    |row| {
                        Ok((
                            row.get(0)?,
                            row.get(1)?,
                            row.get(2)?,
                            row.get(3)?,
                            row.get(4)?,
                            row.get(5)?,
                        ))
                    },
                )
                .unwrap();
        (
            name_revision,
            name_writer,
            members_revision,
            members_writer,
            member_revision,
            link,
            roster(&store.conn, conversation).unwrap(),
        )
    }

    fn apply_event(gateway: &Gateway, event: &group_metadata::ConversationGroupMetadata) {
        let writer = event.writer_device_id.parse::<DeviceId>().unwrap();
        assert!(
            group_metadata::apply(&gateway.client.lock().unwrap().conn, event, writer, 1).unwrap()
        );
    }

    /// The same carried state, emitted by `gateway` with a live link authority at `revision`.
    fn link_event(
        carried: &group_metadata::ConversationGroupMetadata,
        gateway: DeviceId,
        logical_revision: u64,
    ) -> group_metadata::ConversationGroupMetadata {
        group_metadata::ConversationGroupMetadata {
            writer_device_id: gateway.to_string(),
            link: Some(group_metadata::GroupLinkAuthority {
                logical_revision,
                writer_device_id: gateway.to_string(),
                state: group_metadata::GroupLinkSync::Live,
                create: group_metadata::GroupCreateSync::None,
            }),
            ..carried.clone()
        }
    }

    fn permutations(items: &[usize]) -> Vec<Vec<usize>> {
        if items.len() <= 1 {
            return vec![items.to_vec()];
        }
        let mut orders = Vec::new();
        for (index, &head) in items.iter().enumerate() {
            let rest = items
                .iter()
                .enumerate()
                .filter(|(other, _)| *other != index)
                .map(|(_, &item)| item)
                .collect::<Vec<_>>();
            for mut tail in permutations(&rest) {
                tail.insert(0, head);
                orders.push(tail);
            }
        }
        orders
    }

    #[test]
    fn a_link_only_event_never_reauthors_the_roster_or_name_it_carries() {
        let gateway = Gateway::open();
        let conversation = ConversationId::new();
        let seeder = SEEDER.parse::<DeviceId>().unwrap();
        let base = roster_event(conversation, seeder, &[ALEX, SAM], 1, 1);
        apply_event(&gateway, &base);
        let before = registers(&gateway, conversation);
        apply_event(&gateway, &link_event(&base, DeviceId::new(), 1));
        let after = registers(&gateway, conversation);
        assert_eq!(after.1, NAME_WRITER);
        assert_eq!(after.3, SEEDER);
        assert_eq!((after.0, after.2, after.4), (1, 1, 1));
        assert_eq!(after.6, before.6);
        assert_eq!(after.5.as_deref(), Some("live"));
    }

    #[test]
    fn a_third_gateways_link_only_event_leaves_concurrent_roster_and_name_winners_unchanged() {
        let conversation = ConversationId::new();
        let seeder = SEEDER.parse::<DeviceId>().unwrap();
        let x = DeviceId::new();
        let y = DeviceId::new();
        let third_gateway = DeviceId::new();
        let base = roster_event(conversation, seeder, &[ALEX, SAM], 1, 1);
        let to_lee = roster_event(conversation, x, &[ALEX, LEE], 2, 2);
        let to_all = roster_event(conversation, y, &[ALEX, SAM, LEE], 2, 3);
        // The third gateway had seen X's roster and emits its link transition carrying it.
        let link = link_event(&to_lee, third_gateway, 1);
        let events = [base, to_lee, to_all, link];
        let fields = |registers: Registers| {
            (
                registers.0,
                registers.1,
                registers.2,
                registers.3,
                registers.4,
                registers.6,
            )
        };
        let mut seen = Vec::new();
        for order in permutations(&[0, 1, 2, 3]) {
            let gateway = Gateway::open();
            for index in order {
                apply_event(&gateway, &events[index]);
            }
            seen.push(registers(&gateway, conversation));
        }
        assert!(
            seen.iter()
                .all(|registers| registers.5.as_deref() == Some("live"))
        );
        let winner_revision = if x.to_string() > y.to_string() { 2 } else { 3 };
        let winner_writer = if x.to_string() > y.to_string() { x } else { y };
        for registers in &seen {
            assert_eq!(registers.3, winner_writer.to_string());
            assert_eq!(registers.4, winner_revision);
            assert_eq!(registers.1, NAME_WRITER);
            assert_eq!(registers.0, 1);
        }
        let converged = seen.into_iter().map(fields).collect::<Vec<_>>();
        assert!(converged.windows(2).all(|pair| pair[0] == pair[1]));
    }

    #[test]
    fn live_authority_survives_a_name_only_change_through_compaction() {
        let gateway = Gateway::open_compacting();
        gateway
            .ingest(&snapshot(
                1,
                vec![group("g1", &[ALEX, SAM], "Weekend", NOW)],
            ))
            .unwrap();
        let conversation = gateway.mapping_by_google("g1").unwrap().conversation_id;
        gateway
            .ingest(&snapshot(
                2,
                vec![group("g1", &[ALEX, SAM], "Weekend trip", NOW)],
            ))
            .unwrap();
        let restored = gateway.restored_from_compaction();
        let view = restored.client.group_view(conversation).unwrap().unwrap();
        assert_eq!(view.state, GroupLinkState::Live);
        assert_eq!(view.name.as_deref(), Some("Weekend trip"));
    }

    #[test]
    fn unknown_create_survives_a_metadata_update_through_compaction() {
        let gateway = Gateway::open_compacting();
        let conversation = gateway.seed_roster(&[ALEX, SAM], Some("Weekend"));
        {
            let mut store = gateway.client.lock().unwrap();
            let (conn, ctx) = store.parts();
            conn.execute(
                "INSERT INTO rcs_group_create_ledger(conversation_id,command_id,state,attempted_at_ms) VALUES(?,?,'unknown',?)",
                params![conversation.to_string(), CommandId::new().to_string(), NOW],
            )
            .unwrap();
            group_metadata::sync_link_state(conn, &ctx, conversation, NOW).unwrap();
            group_metadata::publish(
                conn,
                &ctx,
                conversation,
                Some("Weekend trip"),
                &BTreeSet::from([ALEX.to_owned(), SAM.to_owned()]),
                NOW,
            )
            .unwrap();
        }
        let restored = gateway.restored_from_compaction();
        let view = restored.client.group_view(conversation).unwrap().unwrap();
        assert!(view.create_needs_confirmation);
        assert!(!view.can_create);
        assert_eq!(view.name.as_deref(), Some("Weekend trip"));
    }

    #[test]
    fn group_mapping_stores_go_member_digest_verbatim_and_flags_the_conversation() {
        let gateway = Gateway::open();
        gateway
            .ingest(&snapshot(1, vec![group("g1", &[ALEX, SAM], "", NOW)]))
            .unwrap();
        let conversation = gateway.mapping_by_google("g1").unwrap().conversation_id;
        assert_eq!(
            gateway.mapping(conversation).unwrap().member_set_digest,
            "digest-g1-2"
        );
        assert!(has_group_mapping(&gateway.client.lock().unwrap().conn, conversation).unwrap());
    }

    // Valid numbers only: the native observation canonicalises through libphonenumber.
    const NATIVE_GOOGLE: &str = "g-native";
    const NATIVE_SELF: &str = "+14155550100";
    const NATIVE_ALEX: &str = "+14155552671";
    const NATIVE_SAM: &str = "+14155552672";
    const NATIVE_LEE: &str = "+14155552673";

    fn native_members() -> [&'static str; 2] {
        [NATIVE_ALEX, NATIVE_SAM]
    }

    fn native_digest(members: &[&str]) -> String {
        member_set_digest(&members.iter().map(|m| (*m).to_owned()).collect())
    }

    /// Google's snapshot of the native group, carrying the Go digest over `members`.
    fn native_snapshot(revision: u64, members: &[&str]) -> Value {
        let mut participants = vec![participant(NATIVE_SELF, true, true, "Me")];
        participants.extend(members.iter().map(|m| participant(m, true, false, "")));
        snapshot(
            revision,
            vec![json!({
                "google_conversation_id": NATIVE_GOOGLE,
                "conversation_type": 2,
                "conversation_status": 1,
                "classification": "eligible",
                "group": true,
                "visible_non_self_count": members.len(),
                "last_message_ms": NOW,
                "participants": participants,
                "name": "Weekend",
                "is_group_chat": true,
                "read_only": false,
                "invisible_participant_count": 0,
                "member_digest": native_digest(members),
            })],
        )
    }

    /// A fresh target observation of the native group, with every timestamp at `NOW`.
    fn native_observation(members: &[&str], mapping_revision: u64) -> RcsMappingObservation {
        let mut participants = vec![RcsParticipantObservation {
            identity: NATIVE_SELF.into(),
            google_participant_id: "p-self".into(),
            identifier_type: 1,
            is_visible: true,
            is_me: true,
        }];
        participants.extend(members.iter().map(|member| RcsParticipantObservation {
            identity: (*member).into(),
            google_participant_id: format!("p-{member}"),
            identifier_type: 1,
            is_visible: true,
            is_me: false,
        }));
        let outgoing = NativeRcsOutgoing {
            outgoing_id: "google-outgoing".into(),
            sim_participant_id: "google-outgoing".into(),
            own_identity_raw: NATIVE_SELF.into(),
            sim_payload: NativeRcsSimPayload {
                two: 2,
                sim_number: 7,
            },
            rcs_chats_enabled: true,
        };
        RcsMappingObservation {
            google_conversation_id: NATIVE_GOOGLE.into(),
            google_candidate_count: 1,
            participants,
            conversation_type: RcsConversationType::Rcs,
            status: "eligible".into(),
            send_mode: RcsSendMode::Automatic,
            read_only: false,
            default_outgoing: true,
            selected_sim_id: "android-subscription-7".into(),
            selected_sim_own_identity: NATIVE_SELF.into(),
            carrier_binding: NativeCarrierRouteBinding {
                carrier_route_id: "android-subscription-7".into(),
                android_subscription_id: 7,
                is_current_default_sms: true,
                own_number_raw: NATIVE_SELF.into(),
                own_number_source: NativeOwnNumberSource::SubscriptionManagerPhoneNumber,
                route_observed_at_ms: NOW,
                route_context_revision: 1,
                same_handset_evidence_id: "verified-handset".into(),
            },
            native_evidence: NativeRcsEvidence {
                lifetime: NativeRcsLifetime {
                    runtime_id: "runtime".into(),
                    run_generation: 1,
                    link_generation: 1,
                    session_generation: 1,
                    context_revision: 1,
                },
                lookup_id: "lookup".into(),
                lookup_complete: true,
                google_candidate_count: 1,
                settings_revision: 1,
                settings_provenance: NativeSettingsProvenance::CurrentEvent,
                settings_observed_at_ms: NOW,
                settings_stream_epoch: 1,
                settings_currentness: NativeSettingsCurrentness::ContinuousVerifiedFeed,
                current_settings_sims: vec![outgoing.clone()],
                settings_validated_at_ms: NOW,
                conversation_observed_at_ms: NOW,
            },
            outgoing,
            identity_region: "US".into(),
            observed_at_ms: NOW,
            context_revision: 1,
            link_generation: 1,
            session_generation: 1,
            canonicalizer_revision: 1,
            mapping_revision,
            link_state: RcsLinkState::Linked,
        }
    }

    struct NativeGroup {
        gateway: Gateway,
        conversation: ConversationId,
        revision: u64,
    }

    /// A linked gateway whose snapshot proves the native group, with its roster seeded locally and
    /// its link context already written, as a 1:1 route would have written it.
    fn native_group() -> NativeGroup {
        native_group_with_context(true)
    }

    /// The group-only link: the same proved group, but no link context has ever been written. The
    /// production route step must write it before any group send can prepare.
    fn native_group_without_context() -> NativeGroup {
        native_group_with_context(false)
    }

    fn native_group_with_context(context: bool) -> NativeGroup {
        let gateway = Gateway::open();
        // The identity region is the single contact book region; the harness writes it directly.
        gateway
            .client
            .lock()
            .unwrap()
            .conn
            .execute(
                "INSERT INTO contact_books(id,owner_device_id,generation,state,body,forgotten) VALUES('carrier-region',?,1,'active',?,0)",
                params![gateway.device.to_string(), json!({"region": "US"}).to_string()],
            )
            .unwrap();
        // Drafts to the recipients create the conversation that the snapshot will link.
        let conversation = native_draft(&gateway, "").conversation_id;
        {
            let mut store = gateway.client.lock().unwrap();
            let (conn, ctx) = store.parts();
            let roster = native_members().iter().map(|m| (*m).to_owned()).collect();
            group_metadata::publish(conn, &ctx, conversation, None, &roster, NOW).unwrap();
            if context {
                rcs_mapping::update_context(conn, &native_observation(&native_members(), 1))
                    .unwrap();
            }
        }
        assert_eq!(
            gateway
                .ingest(&native_snapshot(1, &native_members()))
                .unwrap()
                .linked,
            1
        );
        let revision = gateway
            .client
            .proved_group_member_revision(conversation)
            .unwrap()
            .expect("the proved group is live");
        NativeGroup {
            gateway,
            conversation,
            revision,
        }
    }

    /// Queues one text to the native group on its RCS route, with the composer's revision.
    fn native_draft(gateway: &Gateway, text: &str) -> crate::ComposeDraft {
        gateway
            .client
            .save_new_compose_draft(crate::ComposeDraftUpdate {
                group_name: None,
                text: text.to_owned(),
                recipients: native_members().iter().map(|m| (*m).to_owned()).collect(),
                attachment_ids: Vec::new(),
                route: Some(crate::GatewayRoute::Carrier {
                    gateway_device_id: gateway.device,
                    subscription_id: "sub-1".to_owned(),
                }),
            })
            .unwrap()
    }

    struct NativeCommand {
        command_id: CommandId,
    }

    /// Receives one text to the native group on the gateway, as its command ledger records it: an
    /// RCS-route row (empty subscription) with the composer's group fields.
    fn native_send(
        group: &NativeGroup,
        text: &str,
        expected: Option<u64>,
    ) -> Result<NativeCommand, Error> {
        let command_id = CommandId::new();
        let message = MessagePayload {
            record: MessageRecord {
                message_id: MessageId::new(),
                conversation_id: group.conversation,
                source_sequence: SourceSequence(1),
                attachments: Vec::new(),
            },
            source_device_id: group.gateway.device,
            provider_message_id: None,
            sender_address: None,
            recipients: native_members().map(str::to_owned).to_vec(),
            subject: None,
            body: text.to_owned(),
            transport: crate::Transport::Sms,
            direction: crate::Direction::Outgoing,
            imported: false,
            sent_at_ms: None,
            ordered_at_ms: None,
            mms_context: None,
        };
        let store = group.gateway.client.lock().unwrap();
        let epoch = store.active_epoch.unwrap();
        store
            .conn
            .execute(
                "INSERT INTO command_ledger(command_id,key_epoch,subscription_id,payload,historical) VALUES(?,?,'',?,0)",
                params![
                    command_id.to_string(),
                    epoch,
                    serde_json::to_string(&message).unwrap()
                ],
            )
            .unwrap();
        record_send_fields(
            &store.conn,
            command_id,
            &GroupSendFields {
                expected_member_revision: expected,
                group_create: None,
            },
        )?;
        Ok(NativeCommand { command_id })
    }

    fn attempts(group: &NativeGroup, command_id: CommandId) -> i64 {
        group
            .gateway
            .client
            .lock()
            .unwrap()
            .conn
            .query_row(
                "SELECT COUNT(*) FROM attempts WHERE command_id=?",
                [command_id.to_string()],
                |row| row.get(0),
            )
            .unwrap()
    }

    /// Carrier admission for a text (`Sms`) or media (`Mms`) send with the composer's revision.
    fn carrier_label(
        group: &NativeGroup,
        transport: crate::Transport,
        expected: Option<u64>,
    ) -> &'static str {
        let store = group.gateway.client.lock().unwrap();
        admission_label(
            crate::rcs_send::carrier_admission(
                &store.conn,
                group.conversation,
                &native_members().map(str::to_owned),
                transport,
                transport != crate::Transport::Sms,
                "",
                NOW,
                expected,
            )
            .unwrap(),
        )
    }

    #[test]
    fn live_group_text_is_prepared_begun_and_committed_once_with_the_group_digest() {
        let group = native_group();
        let digest = native_digest(&native_members());
        let queued = native_send(&group, "hello", Some(group.revision)).unwrap();
        let preparation = group
            .gateway
            .client
            .prepare_native_rcs_send(queued.command_id, NOW)
            .unwrap();
        assert_eq!(preparation.expected_member_digest(), Some(digest.as_str()));
        assert!(preparation.mapping().is_none());
        let request = serde_json::to_value(preparation.request()).unwrap();
        assert_eq!(request["canonical_recipient"], json!(digest));
        assert_eq!(
            request.as_object().unwrap().len(),
            3,
            "the facts request keeps the 1:1 shape"
        );

        let projection = group
            .gateway
            .client
            .begin_native_rcs_send(
                preparation,
                &native_observation(&native_members(), group.revision),
                NOW,
            )
            .unwrap();
        assert_eq!(projection.target.class, NativeRcsTargetClass::Group);
        assert_eq!(
            projection.expected_member_digest.as_deref(),
            Some(digest.as_str())
        );
        assert_eq!(projection.mapping_revision, group.revision);
        assert_eq!(projection.proof_digest.len(), 32);
        assert_eq!(projection.target.canonical_recipient, digest);
        assert_eq!(projection.message_body, "hello");
        assert_eq!(attempts(&group, queued.command_id), 1);

        // The permit is single use: the same command cannot be prepared or begun again.
        assert!(
            group
                .gateway
                .client
                .prepare_native_rcs_send(queued.command_id, NOW)
                .is_err()
        );
    }

    #[test]
    fn group_proof_digest_is_derived_from_the_stored_proof_and_binds_the_revision() {
        let group = native_group();
        let queued = native_send(&group, "hi", Some(group.revision)).unwrap();
        let preparation = group
            .gateway
            .client
            .prepare_native_rcs_send(queued.command_id, NOW)
            .unwrap();
        let NativeRcsPreparationTarget::Group(authority) = preparation.target() else {
            panic!("a group preparation is expected");
        };
        let digest_at = |revision: u64| {
            group_proof_digest(
                authority.conversation_id,
                &authority.google_conversation_id,
                revision,
                &authority.roster,
                &authority.member_set_digest,
            )
        };
        assert_eq!(
            digest_at(authority.mapping_revision),
            authority.proof_digest
        );
        assert_ne!(
            digest_at(authority.mapping_revision + 1),
            authority.proof_digest
        );
    }

    #[test]
    fn a_roster_that_changed_at_begin_refuses_and_pauses_the_group() {
        let group = native_group();
        let queued = native_send(&group, "hello", Some(group.revision)).unwrap();
        let preparation = group
            .gateway
            .client
            .prepare_native_rcs_send(queued.command_id, NOW)
            .unwrap();
        let changed = native_observation(&[NATIVE_ALEX, NATIVE_SAM, NATIVE_LEE], group.revision);
        assert!(matches!(
            group
                .gateway
                .client
                .begin_native_rcs_send(preparation, &changed, NOW),
            Err(Error::GroupMembersChanged)
        ));
        assert_eq!(
            group.gateway.mapping(group.conversation).unwrap().state,
            GroupMappingState::Paused
        );
        assert_eq!(attempts(&group, queued.command_id), 0);
    }

    /// The same fresh target observation with every timestamp moved to `at`.
    fn observation_at(members: &[&str], mapping_revision: u64, at: i64) -> RcsMappingObservation {
        let mut observation = native_observation(members, mapping_revision);
        observation.observed_at_ms = at;
        observation.carrier_binding.route_observed_at_ms = at;
        observation.native_evidence.settings_observed_at_ms = at;
        observation.native_evidence.settings_validated_at_ms = at;
        observation.native_evidence.conversation_observed_at_ms = at;
        observation
    }

    #[test]
    fn a_fresh_observation_re_proves_the_group_and_the_send_carries_its_time() {
        let group = native_group();
        let later = NOW + 5_000;
        let outcome = group
            .gateway
            .client
            .verify_rcs_group_observation(
                &observation_at(&native_members(), group.revision, later),
                later,
            )
            .unwrap();
        assert_eq!(outcome, GroupProofOutcome::Proven);
        assert_eq!(
            group
                .gateway
                .mapping(group.conversation)
                .unwrap()
                .proof_observed_at_ms,
            later
        );

        let queued = native_send(&group, "hello", Some(group.revision)).unwrap();
        let preparation = group
            .gateway
            .client
            .prepare_native_rcs_send(queued.command_id, later)
            .unwrap();
        let NativeRcsPreparationTarget::Group(authority) = preparation.target() else {
            panic!("a group preparation is expected");
        };
        assert_eq!(authority.verified_at_ms, later);
        let projection = group
            .gateway
            .client
            .begin_native_rcs_send(
                preparation,
                &observation_at(&native_members(), group.revision, later),
                later,
            )
            .unwrap();
        assert_eq!(projection.message_body, "hello");
        assert_eq!(attempts(&group, queued.command_id), 1);
    }

    #[test]
    fn a_re_proof_with_changed_members_pauses_and_refuses_the_next_prepare() {
        let group = native_group();
        let later = NOW + 5_000;
        let changed = observation_at(
            &[NATIVE_ALEX, NATIVE_SAM, NATIVE_LEE],
            group.revision,
            later,
        );
        assert_eq!(
            group
                .gateway
                .client
                .verify_rcs_group_observation(&changed, later)
                .unwrap(),
            GroupProofOutcome::MembersChanged
        );
        assert_eq!(
            group.gateway.mapping(group.conversation).unwrap().state,
            GroupMappingState::Paused
        );
        let queued = native_send(&group, "hello", Some(group.revision)).unwrap();
        assert!(matches!(
            group
                .gateway
                .client
                .prepare_native_rcs_send(queued.command_id, later),
            Err(Error::GroupMembersChanged)
        ));
        assert_eq!(attempts(&group, queued.command_id), 0);
    }

    #[test]
    fn a_re_proof_of_an_unknown_or_stale_group_is_refused_without_a_pause() {
        let group = native_group();
        let mut unknown = observation_at(&native_members(), group.revision, NOW + 5_000);
        unknown.google_conversation_id = "unknown-group".into();
        assert_eq!(
            group
                .gateway
                .client
                .verify_rcs_group_observation(&unknown, NOW + 5_000)
                .unwrap(),
            GroupProofOutcome::Refused
        );
        let stale = observation_at(&native_members(), group.revision, NOW);
        assert_eq!(
            group
                .gateway
                .client
                .verify_rcs_group_observation(&stale, NOW + 60_000)
                .unwrap(),
            GroupProofOutcome::Refused
        );
        assert_eq!(
            group.gateway.mapping(group.conversation).unwrap().state,
            GroupMappingState::Live
        );
    }

    #[test]
    fn a_member_digest_that_changed_at_commit_is_a_definite_refusal_that_pauses() {
        let group = native_group();
        let queued = native_send(&group, "hello", Some(group.revision)).unwrap();
        let preparation = group
            .gateway
            .client
            .prepare_native_rcs_send(queued.command_id, NOW)
            .unwrap();
        let projection = group
            .gateway
            .client
            .begin_native_rcs_send(
                preparation,
                &native_observation(&native_members(), group.revision),
                NOW,
            )
            .unwrap();
        // Go rechecked the live digest before SendMessage and found it changed.
        let state = group
            .gateway
            .client
            .record_native_rcs_pre_effect_refusal(
                queued.command_id,
                &projection.tmp_id,
                &projection.target.google_conversation_id,
                true,
                NOW,
            )
            .unwrap();
        assert_eq!(state, SendState::FailedBeforeSubmission);
        assert_eq!(
            group.gateway.mapping(group.conversation).unwrap().state,
            GroupMappingState::Paused
        );
        assert_eq!(attempts(&group, queued.command_id), 1);
    }

    #[test]
    fn an_unknown_group_outcome_is_never_retried() {
        let group = native_group();
        let queued = native_send(&group, "hello", Some(group.revision)).unwrap();
        let preparation = group
            .gateway
            .client
            .prepare_native_rcs_send(queued.command_id, NOW)
            .unwrap();
        let projection = group
            .gateway
            .client
            .begin_native_rcs_send(
                preparation,
                &native_observation(&native_members(), group.revision),
                NOW,
            )
            .unwrap();
        assert_eq!(
            group
                .gateway
                .client
                .record_native_rcs_outcome_unknown(
                    queued.command_id,
                    &projection.tmp_id,
                    &projection.target.google_conversation_id,
                )
                .unwrap(),
            SendState::OutcomeUnknown
        );
        assert!(
            group
                .gateway
                .client
                .prepare_native_rcs_send(queued.command_id, NOW)
                .is_err()
        );
        assert_eq!(attempts(&group, queued.command_id), 1);
    }

    #[test]
    fn a_stale_composer_revision_is_refused_before_any_permit() {
        let group = native_group();
        let queued = native_send(&group, "hello", Some(group.revision + 1)).unwrap();
        assert_eq!(
            carrier_label(&group, crate::Transport::Sms, Some(group.revision + 1)),
            "members_changed"
        );
        assert!(
            group
                .gateway
                .client
                .prepare_native_rcs_send(queued.command_id, NOW)
                .is_err()
        );
        assert_eq!(attempts(&group, queued.command_id), 0);
    }

    #[test]
    fn text_without_a_revision_gets_update_required_and_media_is_blocked() {
        let group = native_group();
        assert_eq!(
            carrier_label(&group, crate::Transport::Sms, None),
            "update_required"
        );
        assert_eq!(
            carrier_label(&group, crate::Transport::Mms, Some(group.revision)),
            "blocked"
        );
        assert_eq!(
            carrier_label(&group, crate::Transport::Mms, None),
            "blocked"
        );
    }

    /// The same observation after a route-context advance to `revision`, on every field the
    /// context and the attestation read.
    fn advanced(mut observation: RcsMappingObservation, revision: u64) -> RcsMappingObservation {
        observation.context_revision = revision;
        observation.carrier_binding.route_context_revision = revision;
        observation.native_evidence.lifetime.context_revision = revision;
        observation
    }

    /// One group send in the coordinator's order: the route step writes the context, the target is
    /// re-proved, then the freshness-checked preparation is begun.
    fn send_as_coordinator(
        group: &NativeGroup,
        command_id: CommandId,
        route: &RcsMappingObservation,
        target: &RcsMappingObservation,
        now_ms: i64,
    ) -> Result<(), Error> {
        let client = &group.gateway.client;
        assert!(client.verify_rcs_group_route(route, now_ms)?);
        assert_eq!(
            client.verify_rcs_group_observation(target, now_ms)?,
            GroupProofOutcome::Proven
        );
        let preparation = client.prepare_native_rcs_send(command_id, now_ms)?;
        client.begin_native_rcs_send(preparation, target, now_ms)?;
        Ok(())
    }

    #[test]
    fn a_group_only_link_writes_its_route_context_before_the_first_send() {
        let group = native_group_without_context();
        let queued = native_send(&group, "hello", Some(group.revision)).unwrap();
        assert!(
            group
                .gateway
                .client
                .prepare_native_rcs_send(queued.command_id, NOW)
                .is_err(),
            "without a route context there is no confirmed identity to prove against"
        );
        let observation = observation_at(&native_members(), group.revision, NOW);
        send_as_coordinator(&group, queued.command_id, &observation, &observation, NOW).unwrap();
        assert_eq!(attempts(&group, queued.command_id), 1);
    }

    #[test]
    fn a_second_group_send_and_a_route_context_advance_use_the_same_sequence() {
        let group = native_group_without_context();
        let observation = observation_at(&native_members(), group.revision, NOW);
        let first = native_send(&group, "one", Some(group.revision)).unwrap();
        send_as_coordinator(&group, first.command_id, &observation, &observation, NOW).unwrap();

        let second = native_send(&group, "two", Some(group.revision)).unwrap();
        send_as_coordinator(&group, second.command_id, &observation, &observation, NOW).unwrap();
        assert_eq!(attempts(&group, second.command_id), 1);

        // A later context revision on the same link advances the stored context before the proof.
        let later = NOW + 1_000;
        let advanced = advanced(observation_at(&native_members(), group.revision, later), 2);
        let third = native_send(&group, "three", Some(group.revision)).unwrap();
        send_as_coordinator(&group, third.command_id, &advanced, &advanced, later).unwrap();
        assert_eq!(attempts(&group, third.command_id), 1);
    }

    #[test]
    fn an_idle_group_sends_when_its_proof_is_older_than_the_freshness_window() {
        let group = native_group();
        let later = NOW + 30_001;
        assert_eq!(
            group
                .gateway
                .client
                .proved_group_member_digest(group.conversation)
                .unwrap(),
            Some(native_digest(&native_members()))
        );
        let queued = native_send(&group, "idle", Some(group.revision)).unwrap();
        assert!(
            group
                .gateway
                .client
                .prepare_native_rcs_send(queued.command_id, later)
                .is_err(),
            "a stale stored proof cannot prepare on its own"
        );
        let observation = observation_at(&native_members(), group.revision, later);
        send_as_coordinator(&group, queued.command_id, &observation, &observation, later).unwrap();
        assert_eq!(attempts(&group, queued.command_id), 1);
    }

    #[test]
    fn a_group_only_link_with_changed_members_is_refused_and_paused() {
        let group = native_group_without_context();
        let queued = native_send(&group, "hello", Some(group.revision)).unwrap();
        let route = observation_at(&native_members(), group.revision, NOW);
        let changed = observation_at(&[NATIVE_ALEX, NATIVE_SAM, NATIVE_LEE], group.revision, NOW);
        let client = &group.gateway.client;
        assert!(client.verify_rcs_group_route(&route, NOW).unwrap());
        assert_eq!(
            client.verify_rcs_group_observation(&changed, NOW).unwrap(),
            GroupProofOutcome::MembersChanged
        );
        assert_eq!(
            group.gateway.mapping(group.conversation).unwrap().state,
            GroupMappingState::Paused
        );
        assert!(
            client
                .prepare_native_rcs_send(queued.command_id, NOW)
                .is_err()
        );
        assert_eq!(attempts(&group, queued.command_id), 0);
    }

    #[test]
    fn a_retired_or_stale_group_route_does_not_advance_the_context() {
        let group = native_group_without_context();
        let client = &group.gateway.client;
        let mut retired = observation_at(&native_members(), group.revision, NOW);
        retired.context_revision = 0;
        assert!(!client.verify_rcs_group_route(&retired, NOW).unwrap());
        let stale = observation_at(&native_members(), group.revision, NOW);
        assert!(!client.verify_rcs_group_route(&stale, NOW + 30_001).unwrap());
        let fresh = observation_at(&native_members(), group.revision, NOW);
        assert!(client.verify_rcs_group_route(&fresh, NOW).unwrap());
    }
}
