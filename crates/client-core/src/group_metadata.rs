use std::collections::BTreeSet;

use peppy_domain::{ConversationId, DeviceId};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::{
    Ctx, EnvelopePurpose, Error, PrivatePayload, enqueue,
    rcs_group::{self, GroupMappingState},
    rcs_group_create::{self, GroupCreateState, GroupLinkState, GroupView},
};

/// Synced group projection event. Name, roster and link are three independent LWW registers; each
/// carries its own `(logical_revision, writer_device_id)`, so a field is only ever re-clocked by
/// the device that actually wrote it. Every event carries the full current state of all three
/// registers, so the newest retained event (compaction keeps one per conversation) reconstructs
/// them all. `writer_device_id` is the emitting device and must be the authenticated producer; the
/// field writers may be older devices whose values the emitter merely carries forward. The roster
/// revision (`member_revision`) is the winning roster version's revision, adopted verbatim on every
/// device that accepts that roster. No Google identifiers ever travel.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct ConversationGroupMetadata {
    pub conversation_id: ConversationId,
    pub name: Option<String>,
    pub name_logical_revision: u64,
    pub name_writer_device_id: String,
    pub members: Vec<String>,
    pub members_logical_revision: u64,
    pub members_writer_device_id: String,
    pub member_revision: u64,
    pub observed_at_ms: i64,
    pub writer_device_id: String,
    /// The gateway's sanitized link authority, or absent when this device never synced one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<GroupLinkAuthority>,
}

/// The gateway's link state for a group, as synced. `logical_revision` follows the same LWW rule as
/// the name and roster fields within the link register: greater wins, equal revisions go to the
/// greater writer device id.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupLinkAuthority {
    pub logical_revision: u64,
    pub writer_device_id: String,
    pub state: GroupLinkSync,
    pub create: GroupCreateSync,
}

/// Synced link state. Mirrors `GroupMappingState`; `Unlinked` is also the default when none is synced.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GroupLinkSync {
    Unlinked,
    Proving,
    Live,
    Paused,
    Withdrawn,
}

/// Synced create state: `none` when no create is recorded or the create is settled into a link.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GroupCreateSync {
    None,
    Pending,
    Unknown,
    NotRcs,
    Linked,
    Refused,
}

impl GroupLinkSync {
    fn code(self) -> &'static str {
        match self {
            Self::Unlinked => "unlinked",
            Self::Proving => "proving",
            Self::Live => "live",
            Self::Paused => "paused",
            Self::Withdrawn => "withdrawn",
        }
    }

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
}

impl GroupCreateSync {
    fn code(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Pending => "pending",
            Self::Unknown => "unknown",
            Self::NotRcs => "not_rcs",
            Self::Linked => "linked",
            Self::Refused => "refused",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "none" => Self::None,
            "pending" => Self::Pending,
            "unknown" => Self::Unknown,
            "not_rcs" => Self::NotRcs,
            "linked" => Self::Linked,
            "refused" => Self::Refused,
            _ => return None,
        })
    }
}

impl From<GroupMappingState> for GroupLinkSync {
    fn from(state: GroupMappingState) -> Self {
        match state {
            GroupMappingState::Unlinked => Self::Unlinked,
            GroupMappingState::Proving => Self::Proving,
            GroupMappingState::Live => Self::Live,
            GroupMappingState::Paused => Self::Paused,
            GroupMappingState::Withdrawn => Self::Withdrawn,
        }
    }
}

impl From<GroupCreateState> for GroupCreateSync {
    fn from(state: GroupCreateState) -> Self {
        match state {
            GroupCreateState::None => Self::None,
            GroupCreateState::Begun | GroupCreateState::Pending => Self::Pending,
            GroupCreateState::Unknown => Self::Unknown,
            GroupCreateState::NotRcs => Self::NotRcs,
            GroupCreateState::Linked => Self::Linked,
            GroupCreateState::Refused(_) => Self::Refused,
        }
    }
}

impl From<GroupLinkSync> for GroupLinkState {
    fn from(state: GroupLinkSync) -> Self {
        match state {
            GroupLinkSync::Unlinked => Self::Unlinked,
            GroupLinkSync::Proving => Self::Proving,
            GroupLinkSync::Live => Self::Live,
            GroupLinkSync::Paused => Self::Paused,
            GroupLinkSync::Withdrawn => Self::Withdrawn,
        }
    }
}

/// Local-only timeline facts derived from accepted metadata changes. Never synced.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GroupNoticeKind {
    Renamed,
    Added,
    Removed,
}

impl GroupNoticeKind {
    fn code(self) -> &'static str {
        match self {
            Self::Renamed => "renamed",
            Self::Added => "added",
            Self::Removed => "removed",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "renamed" => Self::Renamed,
            "added" => Self::Added,
            "removed" => Self::Removed,
            _ => return None,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupNotice {
    pub kind: GroupNoticeKind,
    pub name: Option<String>,
    pub member: Option<String>,
    pub at_ms: i64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Stored {
    name: Option<String>,
    name_revision: u64,
    name_writer: String,
    members: Vec<String>,
    members_revision: u64,
    members_writer: String,
    member_revision: u64,
    link: Option<GroupLinkAuthority>,
}

impl Stored {
    /// The link LWW clock: revision 0 when no link authority was ever synced.
    fn link_clock(&self) -> (u64, &str) {
        self.link.as_ref().map_or((0, ""), |link| {
            (link.logical_revision, link.writer_device_id.as_str())
        })
    }
}

pub(crate) fn apply(
    conn: &Connection,
    metadata: &ConversationGroupMetadata,
    producer: DeviceId,
    _sequence: u64,
) -> Result<bool, Error> {
    if !valid(metadata, producer) {
        return Ok(false);
    }
    let id = metadata.conversation_id.to_string();
    let previous = stored(conn, metadata.conversation_id)?;
    let existing = previous.is_some();
    let stored = previous.unwrap_or_default();
    // Each register compares against its own clock. A carried field keeps its original writer, so
    // it wins or loses exactly as the write that produced it would have.
    let name_wins = !existing
        || lww_wins(
            metadata.name_logical_revision,
            &metadata.name_writer_device_id,
            stored.name_revision,
            &stored.name_writer,
        );
    let members_wins = !existing
        || lww_wins(
            metadata.members_logical_revision,
            &metadata.members_writer_device_id,
            stored.members_revision,
            &stored.members_writer,
        );
    let link_wins = metadata.link.as_ref().is_some_and(|link| {
        let (current_revision, current_writer) = stored.link_clock();
        lww_wins(
            link.logical_revision,
            &link.writer_device_id,
            current_revision,
            current_writer,
        )
    });
    if !name_wins && !members_wins && !link_wins {
        return Ok(true);
    }
    let members_changed = members_wins && stored.members != metadata.members;
    // The winning roster version carries its own revision: adopting it keeps every device on the
    // same token for the same roster, whichever intermediate events it saw or in what order.
    let member_revision = if members_wins {
        metadata.member_revision
    } else {
        stored.member_revision
    };
    let link = if link_wins {
        metadata.link.clone()
    } else {
        stored.link.clone()
    };
    let (name, name_revision, name_writer) = if name_wins {
        (
            metadata.name.clone(),
            metadata.name_logical_revision,
            metadata.name_writer_device_id.clone(),
        )
    } else {
        (
            stored.name.clone(),
            stored.name_revision,
            stored.name_writer.clone(),
        )
    };
    let (members_revision, members_writer) = if members_wins {
        (
            metadata.members_logical_revision,
            metadata.members_writer_device_id.clone(),
        )
    } else {
        (stored.members_revision, stored.members_writer.clone())
    };
    conn.execute(
        "INSERT INTO conversation_group_meta(conversation_id,name,name_logical_revision,name_writer_device_id,members_logical_revision,members_writer_device_id,member_revision,observed_at_ms,source,link_logical_revision,link_writer_device_id,link_state,link_create_state) VALUES(?,?,?,?,?,?,?,?, 'event',?,?,?,?) ON CONFLICT(conversation_id) DO UPDATE SET name=excluded.name,name_logical_revision=excluded.name_logical_revision,name_writer_device_id=excluded.name_writer_device_id,members_logical_revision=excluded.members_logical_revision,members_writer_device_id=excluded.members_writer_device_id,member_revision=excluded.member_revision,observed_at_ms=excluded.observed_at_ms,source=excluded.source,link_logical_revision=excluded.link_logical_revision,link_writer_device_id=excluded.link_writer_device_id,link_state=excluded.link_state,link_create_state=excluded.link_create_state",
        params![
            id,
            name,
            name_revision,
            name_writer,
            members_revision,
            members_writer,
            member_revision,
            metadata.observed_at_ms,
            link.as_ref().map(|link| link.logical_revision),
            link.as_ref().map(|link| link.writer_device_id.as_str()),
            link.as_ref().map(|link| link.state.code()),
            link.as_ref().map(|link| link.create.code()),
        ],
    )?;
    if members_changed {
        conn.execute(
            "DELETE FROM conversation_group_members WHERE conversation_id=?",
            [&id],
        )?;
        for member in &metadata.members {
            conn.execute(
                "INSERT INTO conversation_group_members(conversation_id,canonical_address) VALUES(?,?)",
                params![id, member],
            )?;
        }
        // A first roster fill is not a membership change: legacy MMS threads stay mapped.
        if !stored.members.is_empty() {
            conn.execute(
                "DELETE FROM mms_thread_conversations WHERE conversation_id=?",
                [&id],
            )?;
        }
    }
    if existing {
        record_notices(conn, metadata, &stored, name_wins, members_changed)?;
    }
    Ok(true)
}

fn record_notices(
    conn: &Connection,
    metadata: &ConversationGroupMetadata,
    stored: &Stored,
    name_wins: bool,
    members_changed: bool,
) -> Result<(), Error> {
    if name_wins && metadata.name.is_some() && metadata.name != stored.name {
        insert_notice(
            conn,
            metadata,
            GroupNoticeKind::Renamed,
            metadata.name.as_deref(),
            None,
        )?;
    }
    if members_changed && !stored.members.is_empty() {
        let before = stored.members.iter().collect::<BTreeSet<_>>();
        let after = metadata.members.iter().collect::<BTreeSet<_>>();
        for member in after.difference(&before) {
            insert_notice(conn, metadata, GroupNoticeKind::Added, None, Some(member))?;
        }
        for member in before.difference(&after) {
            insert_notice(conn, metadata, GroupNoticeKind::Removed, None, Some(member))?;
        }
    }
    Ok(())
}

fn insert_notice(
    conn: &Connection,
    metadata: &ConversationGroupMetadata,
    kind: GroupNoticeKind,
    name: Option<&str>,
    member: Option<&str>,
) -> Result<(), Error> {
    conn.execute(
        "INSERT INTO conversation_group_notices(conversation_id,kind,name,member,at_ms) VALUES(?,?,?,?,?)",
        params![
            metadata.conversation_id.to_string(),
            kind.code(),
            name,
            member,
            metadata.observed_at_ms,
        ],
    )?;
    Ok(())
}

/// Local, best-effort notices for a conversation, oldest first. Compaction of the synced event
/// means devices that skipped intermediate changes will not see them.
pub(crate) fn notices(
    conn: &Connection,
    conversation_id: ConversationId,
) -> Result<Vec<GroupNotice>, Error> {
    let mut statement = conn.prepare(
        "SELECT kind,name,member,at_ms FROM conversation_group_notices WHERE conversation_id=? ORDER BY id",
    )?;
    let rows = statement.query_map([conversation_id.to_string()], |row| {
        let kind: String = row.get(0)?;
        Ok(GroupNotice {
            kind: GroupNoticeKind::parse(&kind).ok_or(rusqlite::Error::InvalidQuery)?,
            name: row.get(1)?,
            member: row.get(2)?,
            at_ms: row.get(3)?,
        })
    })?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

/// Writes the gateway's current view of a group as one synced event, then applies it locally.
/// An absent Google name (`None`) keeps the stored name: the gateway never clears a name.
/// Returns the member revision after the write.
pub(crate) fn publish(
    conn: &Connection,
    ctx: &Ctx<'_>,
    conversation_id: ConversationId,
    observed_name: Option<&str>,
    members: &BTreeSet<String>,
    now_ms: i64,
) -> Result<u64, Error> {
    let current = stored(conn, conversation_id)?;
    let existing = current.is_some();
    let current = current.unwrap_or_default();
    let name = match observed_name {
        Some(name) if !name.is_empty() => Some(name.to_owned()),
        _ => current.name.clone(),
    };
    let members = members.iter().cloned().collect::<Vec<_>>();
    let name_changed = !existing || name != current.name;
    let members_changed = !existing || members != current.members;
    if !name_changed && !members_changed {
        return Ok(current.member_revision);
    }
    // Seed from the highest applied logical revision so a local write always wins LWW.
    let clock = current
        .name_revision
        .max(current.members_revision)
        .checked_add(1)
        .ok_or(Error::Database)?;
    let writer = ctx.device_id.to_string();
    let metadata = ConversationGroupMetadata {
        conversation_id,
        name,
        name_logical_revision: if name_changed {
            clock
        } else {
            current.name_revision
        },
        name_writer_device_id: if name_changed {
            writer.clone()
        } else {
            current.name_writer.clone()
        },
        members,
        members_logical_revision: if members_changed {
            clock
        } else {
            current.members_revision
        },
        members_writer_device_id: if members_changed {
            writer.clone()
        } else {
            current.members_writer.clone()
        },
        member_revision: if members_changed {
            current
                .member_revision
                .checked_add(1)
                .ok_or(Error::Database)?
        } else {
            current.member_revision
        },
        observed_at_ms: now_ms,
        writer_device_id: writer,
        // Carried unchanged so this event never supersedes the link authority at compaction.
        link: current.link.clone(),
    };
    let (_, sequence) = enqueue(
        conn,
        ctx,
        EnvelopePurpose::Event,
        None,
        None,
        &PrivatePayload::ConversationGroupMetadata(metadata.clone()),
    )?;
    apply(conn, &metadata, ctx.device_id, sequence)?;
    Ok(metadata.member_revision)
}

/// Emits the gateway's current link authority when it differs from the last synced one. Callers run
/// it after every transition; a conversation without a synced roster has nothing to attach to, so
/// it emits nothing. Returns whether an event was written.
pub(crate) fn sync_link_state(
    conn: &Connection,
    ctx: &Ctx<'_>,
    conversation_id: ConversationId,
    now_ms: i64,
) -> Result<bool, Error> {
    let Some(current) = stored(conn, conversation_id)? else {
        return Ok(false);
    };
    // Only the owning gateway publishes link authority. Any other device has neither a mapping
    // nor a create row, and must never overwrite the gateway's synced state with its own absence.
    let owns_mapping = rcs_group::group_mapping(conn, conversation_id)?
        .is_some_and(|mapping| mapping.gateway_device_id == ctx.device_id);
    let (state, create) = local_link(conn, conversation_id)?;
    let writer = ctx.device_id.to_string();
    let owner = owns_mapping
        || create != GroupCreateSync::None
        || current
            .link
            .as_ref()
            .is_some_and(|link| link.writer_device_id == writer);
    if !owner {
        return Ok(false);
    }
    let synced = current
        .link
        .as_ref()
        .map_or((GroupLinkSync::Unlinked, GroupCreateSync::None), |link| {
            (link.state, link.create)
        });
    if synced == (state, create) {
        return Ok(false);
    }
    // Only the link register's own clock advances; name and roster keep their writers and clocks.
    let logical_revision = current
        .link_clock()
        .0
        .checked_add(1)
        .ok_or(Error::Database)?;
    let metadata = ConversationGroupMetadata {
        conversation_id,
        name: current.name,
        name_logical_revision: current.name_revision,
        name_writer_device_id: current.name_writer,
        members: current.members,
        members_logical_revision: current.members_revision,
        members_writer_device_id: current.members_writer,
        member_revision: current.member_revision,
        observed_at_ms: now_ms,
        writer_device_id: writer.clone(),
        link: Some(GroupLinkAuthority {
            logical_revision,
            writer_device_id: writer,
            state,
            create,
        }),
    };
    let (_, sequence) = enqueue(
        conn,
        ctx,
        EnvelopePurpose::Event,
        None,
        None,
        &PrivatePayload::ConversationGroupMetadata(metadata.clone()),
    )?;
    apply(conn, &metadata, ctx.device_id, sequence)?;
    Ok(true)
}

/// Runs `sync_link_state` for every synced group. Used after a batch that may move several groups.
pub(crate) fn sync_all_link_states(
    conn: &Connection,
    ctx: &Ctx<'_>,
    now_ms: i64,
) -> Result<(), Error> {
    let ids = conn
        .prepare("SELECT conversation_id FROM conversation_group_meta ORDER BY conversation_id")?
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    for id in ids {
        sync_link_state(conn, ctx, crate::parse(&id)?, now_ms)?;
    }
    Ok(())
}

/// Link and create state as this device's own tables state it. Only the gateway holds these rows.
fn local_link(
    conn: &Connection,
    conversation_id: ConversationId,
) -> Result<(GroupLinkSync, GroupCreateSync), Error> {
    let state = rcs_group::group_mapping(conn, conversation_id)?
        .map_or(GroupLinkSync::Unlinked, |mapping| mapping.state.into());
    let create = rcs_group_create::state(conn, conversation_id)?.into();
    Ok((state, create))
}

/// Group state for desktop and native callers. A device holding the gateway's mapping or create row
/// reports those rows; every other device derives link state, create state, and the create prompt
/// from the synced authority. `view` is the local projection the caller already built.
pub(crate) fn with_synced_authority(
    conn: &Connection,
    conversation_id: ConversationId,
    view: Option<GroupView>,
) -> Result<Option<GroupView>, Error> {
    let Some(mut view) = view else {
        return Ok(None);
    };
    let local_create = rcs_group_create::state(conn, conversation_id)?;
    if rcs_group::group_mapping(conn, conversation_id)?.is_some()
        || local_create != GroupCreateState::None
    {
        return Ok(Some(view));
    }
    let (state, create) = stored(conn, conversation_id)?
        .and_then(|current| current.link)
        .map_or((GroupLinkSync::Unlinked, GroupCreateSync::None), |link| {
            (link.state, link.create)
        });
    view.state = state.into();
    view.create_needs_confirmation = create == GroupCreateSync::Unknown;
    view.can_create = state == GroupLinkSync::Unlinked
        && view.members.len() >= 2
        && create == GroupCreateSync::None;
    Ok(Some(view))
}

pub(crate) fn member_revision(
    conn: &Connection,
    conversation_id: ConversationId,
) -> Result<Option<u64>, Error> {
    Ok(stored(conn, conversation_id)?.map(|metadata| metadata.member_revision))
}

fn stored(conn: &Connection, conversation_id: ConversationId) -> Result<Option<Stored>, Error> {
    let row = conn
        .query_row(
            "SELECT name,name_logical_revision,name_writer_device_id,members_logical_revision,members_writer_device_id,member_revision,link_logical_revision,link_writer_device_id,link_state,link_create_state FROM conversation_group_meta WHERE conversation_id=?",
            [conversation_id.to_string()],
            |row| {
                Ok((
                    row.get::<_, Option<String>>(0)?,
                    row.get::<_, u64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, u64>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, u64>(5)?,
                    row.get::<_, Option<u64>>(6)?,
                    row.get::<_, Option<String>>(7)?,
                    row.get::<_, Option<String>>(8)?,
                    row.get::<_, Option<String>>(9)?,
                ))
            },
        )
        .optional()?;
    let Some((
        name,
        name_revision,
        name_writer,
        members_revision,
        members_writer,
        member_revision,
        link_revision,
        link_writer,
        link_state,
        link_create,
    )) = row
    else {
        return Ok(None);
    };
    let link = match (link_revision, link_writer, link_state, link_create) {
        (Some(logical_revision), Some(writer_device_id), Some(state), Some(create)) => {
            Some(GroupLinkAuthority {
                logical_revision,
                writer_device_id,
                state: GroupLinkSync::parse(&state).ok_or(Error::Database)?,
                create: GroupCreateSync::parse(&create).ok_or(Error::Database)?,
            })
        }
        _ => None,
    };
    let mut statement = conn.prepare(
        "SELECT canonical_address FROM conversation_group_members WHERE conversation_id=? ORDER BY canonical_address",
    )?;
    let members = statement
        .query_map([conversation_id.to_string()], |row| row.get(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Some(Stored {
        name,
        name_revision,
        name_writer,
        members,
        members_revision,
        members_writer,
        member_revision,
        link,
    }))
}

fn lww_wins(revision: u64, writer: &str, current_revision: u64, current_writer: &str) -> bool {
    revision > current_revision || (revision == current_revision && writer > current_writer)
}

fn valid(metadata: &ConversationGroupMetadata, producer: DeviceId) -> bool {
    let valid_revision = |revision| (1..=i64::MAX as u64).contains(&revision);
    valid_revision(metadata.name_logical_revision)
        && valid_revision(metadata.members_logical_revision)
        && valid_revision(metadata.member_revision)
        && metadata.observed_at_ms >= 0
        && metadata.link.as_ref().is_none_or(|link| {
            valid_revision(link.logical_revision) && canonical_device(&link.writer_device_id)
        })
        && metadata.writer_device_id == producer.to_string()
        && canonical_device(&metadata.writer_device_id)
        && canonical_device(&metadata.name_writer_device_id)
        && canonical_device(&metadata.members_writer_device_id)
        && metadata
            .members
            .windows(2)
            .all(|members| members[0] < members[1])
        && metadata
            .members
            .iter()
            .all(|member| canonical_member(member))
        && metadata
            .name
            .as_ref()
            .is_none_or(|name| !name.is_empty() && name.len() <= 256)
}

#[cfg(test)]
mod validation_tests {
    use super::*;

    #[test]
    fn rejects_logical_revisions_that_sqlite_cannot_represent() {
        let producer = DeviceId::new();
        let metadata = ConversationGroupMetadata {
            conversation_id: ConversationId::new(),
            name: None,
            name_logical_revision: i64::MAX as u64 + 1,
            name_writer_device_id: producer.to_string(),
            members: Vec::new(),
            members_logical_revision: 1,
            members_writer_device_id: producer.to_string(),
            member_revision: 1,
            observed_at_ms: 0,
            writer_device_id: producer.to_string(),
            link: None,
        };
        assert!(!valid(&metadata, producer));
    }
}

/// A writer field must be the canonical text of a device id, so equal writers always compare equal.
fn canonical_device(value: &str) -> bool {
    value
        .parse::<DeviceId>()
        .is_ok_and(|device| device.to_string() == value)
}

fn canonical_member(member: &str) -> bool {
    let Some(digits) = member.strip_prefix('+') else {
        return false;
    };
    (1..=15).contains(&digits.len())
        && matches!(digits.as_bytes().first(), Some(b'1'..=b'9'))
        && digits.bytes().all(|digit| digit.is_ascii_digit())
}
