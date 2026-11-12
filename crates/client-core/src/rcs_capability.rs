use crate::{ConversationId, DeviceId, Error};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

pub const RCS_CAPABILITY_STALE_MS: i64 = 30 * 60_000;
pub const RCS_CAPABILITY_KEEPALIVE_MS: i64 = 10 * 60_000;
pub const RCS_CAPABILITY_PUBLISH_BATCH: usize = 100;
/// An unchanged eligibility is not published again sooner than this. It only stops a caller that
/// publishes on every attempt from producing an event each time.
pub const RCS_ELIGIBILITY_REPUBLISH_FLOOR_MS: i64 = 60_000;

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RcsCapabilityState {
    Linked,
    Unlinked,
    NeedsRelink,
    SignedOut,
    Unavailable,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RcsEligibility {
    Eligible,
    NotEligible,
    Unknown,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RcsLinkStatusEvent {
    pub gateway_device_id: DeviceId,
    pub state: RcsCapabilityState,
    pub availability: bool,
    pub health: bool,
    pub phone_responsive: bool,
    pub observed_at_ms: i64,
    pub capability_revision: u64,
    pub feature_bits: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub linked_since_ms: Option<i64>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RcsEligibilityProjection {
    pub conversation_id: ConversationId,
    pub eligibility: RcsEligibility,
    pub feature_bits: u32,
    pub mapping_revision: u64,
    pub observed_at_ms: i64,
    pub gateway_device_id: DeviceId,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RcsLinkStatusPayload {
    pub status: RcsLinkStatusEvent,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RcsLinkEligibilityPayload {
    pub eligibility: RcsEligibilityProjection,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RcsLinkStatusProjection {
    pub status: RcsLinkStatusEvent,
    pub current: bool,
}

pub(crate) fn valid_status(value: &RcsLinkStatusEvent) -> bool {
    value.capability_revision > 0
        && value.capability_revision <= i64::MAX as u64
        && (0..i64::MAX).contains(&value.observed_at_ms)
}

pub(crate) fn valid_eligibility(value: &RcsEligibilityProjection) -> bool {
    value.mapping_revision > 0
        && value.mapping_revision <= i64::MAX as u64
        && (0..i64::MAX).contains(&value.observed_at_ms)
}

pub(crate) fn apply_status(
    conn: &Connection,
    value: &RcsLinkStatusEvent,
    producer: DeviceId,
    sequence: u64,
) -> Result<bool, Error> {
    if producer != value.gateway_device_id || !valid_status(value) {
        return Ok(false);
    }
    let Ok(sequence) = i64::try_from(sequence) else {
        return Ok(false);
    };
    let applied = conn.execute(
        "INSERT INTO rcs_link_status_projection(gateway_device_id,state,availability,health,phone_responsive,observed_at_ms,capability_revision,feature_bits,producer_sequence) VALUES(?,?,?,?,?,?,?,?,?) ON CONFLICT(gateway_device_id) DO UPDATE SET state=excluded.state,availability=excluded.availability,health=excluded.health,phone_responsive=excluded.phone_responsive,observed_at_ms=excluded.observed_at_ms,capability_revision=excluded.capability_revision,feature_bits=excluded.feature_bits,producer_sequence=excluded.producer_sequence WHERE excluded.capability_revision > rcs_link_status_projection.capability_revision OR (excluded.capability_revision=rcs_link_status_projection.capability_revision AND excluded.producer_sequence > rcs_link_status_projection.producer_sequence)",
        params![value.gateway_device_id.to_string(), capability_state(value.state), value.availability, value.health, value.phone_responsive, value.observed_at_ms, value.capability_revision, value.feature_bits, sequence],
    )?;
    if applied == 0 {
        return Ok(true);
    }
    if value.state == RcsCapabilityState::Linked {
        let Some(linked_since_ms) = value.linked_since_ms else {
            conn.execute(
                "DELETE FROM metadata WHERE k=?",
                [linked_since_key(value.gateway_device_id)],
            )?;
            return Ok(true);
        };
        conn.execute(
            "INSERT INTO metadata(k,v) VALUES(?,?) ON CONFLICT(k) DO UPDATE SET v=excluded.v",
            params![linked_since_key(value.gateway_device_id), linked_since_ms],
        )?;
    } else {
        conn.execute(
            "DELETE FROM metadata WHERE k=?",
            [linked_since_key(value.gateway_device_id)],
        )?;
    }
    Ok(true)
}

pub(crate) fn apply_eligibility(
    conn: &Connection,
    value: &RcsEligibilityProjection,
    producer: DeviceId,
    sequence: u64,
) -> Result<bool, Error> {
    if producer != value.gateway_device_id || !valid_eligibility(value) {
        return Ok(false);
    }
    let Ok(sequence) = i64::try_from(sequence) else {
        return Ok(false);
    };
    conn.execute(
        "INSERT INTO rcs_link_eligibility_projection(gateway_device_id,conversation_id,eligibility,feature_bits,mapping_revision,observed_at_ms,producer_sequence) VALUES(?,?,?,?,?,?,?) ON CONFLICT(gateway_device_id,conversation_id) DO UPDATE SET eligibility=excluded.eligibility,feature_bits=excluded.feature_bits,mapping_revision=excluded.mapping_revision,observed_at_ms=excluded.observed_at_ms,producer_sequence=excluded.producer_sequence WHERE excluded.mapping_revision > rcs_link_eligibility_projection.mapping_revision OR (excluded.mapping_revision=rcs_link_eligibility_projection.mapping_revision AND excluded.producer_sequence > rcs_link_eligibility_projection.producer_sequence)",
        params![value.gateway_device_id.to_string(), value.conversation_id.to_string(), eligibility(value.eligibility), value.feature_bits, value.mapping_revision, value.observed_at_ms, sequence],
    )?;
    Ok(true)
}

pub(crate) fn current_gateway(
    conn: &Connection,
    gateway: DeviceId,
    now_ms: i64,
) -> Result<bool, Error> {
    let row: Option<(String, bool, bool, bool, i64)> = conn.query_row("SELECT state,availability,health,phone_responsive,observed_at_ms FROM rcs_link_status_projection WHERE gateway_device_id=?", [gateway.to_string()], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?))).optional()?;
    Ok(row.is_some_and(
        |(state, availability, health, phone_responsive, observed)| {
            state == "linked"
                && availability
                && health
                && phone_responsive
                && fresh_at(now_ms, observed)
        },
    ))
}

pub(crate) fn projection(
    conn: &Connection,
    conversation: ConversationId,
    gateway: DeviceId,
    now_ms: i64,
) -> Result<Option<RcsEligibilityProjection>, Error> {
    let row: Option<(String, u32, u64, i64, i64)> = conn.query_row("SELECT eligibility,feature_bits,mapping_revision,observed_at_ms,producer_sequence FROM rcs_link_eligibility_projection WHERE gateway_device_id=? AND conversation_id=?", params![gateway.to_string(), conversation.to_string()], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?))).optional()?;
    row.map(
        |(kind, feature_bits, mapping_revision, observed_at_ms, _)| {
            Ok(RcsEligibilityProjection {
                conversation_id: conversation,
                eligibility: if !current_gateway(conn, gateway, now_ms)?
                    || observed_at_ms > now_ms
                    || !eligibility_follows_linked_since(conn, gateway, observed_at_ms)?
                {
                    RcsEligibility::Unknown
                } else {
                    parse_eligibility(&kind)?
                },
                feature_bits,
                mapping_revision,
                observed_at_ms,
                gateway_device_id: gateway,
            })
        },
    )
    .transpose()
}

fn eligibility_follows_linked_since(
    conn: &Connection,
    gateway: DeviceId,
    eligibility_observed_at_ms: i64,
) -> Result<bool, Error> {
    let linked_since_ms = conn
        .query_row(
            "SELECT v FROM metadata WHERE k=?",
            [linked_since_key(gateway)],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .map(|value| value.parse::<i64>().map_err(|_| Error::Database))
        .transpose()?;
    Ok(
        linked_since_ms
            .is_some_and(|linked_since_ms| eligibility_observed_at_ms >= linked_since_ms),
    )
}

pub(crate) fn linked_since(conn: &Connection, gateway: DeviceId) -> Result<Option<i64>, Error> {
    conn.query_row(
        "SELECT v FROM metadata WHERE k=?",
        [linked_since_key(gateway)],
        |row| row.get::<_, String>(0),
    )
    .optional()?
    .map(|value| value.parse::<i64>().map_err(|_| Error::Database))
    .transpose()
}

pub(crate) fn record_linked_since(
    conn: &Connection,
    gateway: DeviceId,
    linked_since_ms: i64,
) -> Result<(), Error> {
    conn.execute(
        "INSERT INTO metadata(k,v) VALUES(?,?) ON CONFLICT(k) DO UPDATE SET v=excluded.v",
        params![linked_since_key(gateway), linked_since_ms],
    )?;
    Ok(())
}

pub(crate) fn next_linked_since(
    conn: &Connection,
    gateway: DeviceId,
    observed_at_ms: i64,
) -> Result<i64, Error> {
    Ok(observed_at_ms.max(
        observed_high_watermark(conn, gateway)?
            .map(|timestamp| timestamp.checked_add(1).ok_or(Error::Database))
            .transpose()?
            .unwrap_or(0),
    ))
}

pub(crate) fn record_observed_at(
    conn: &Connection,
    gateway: DeviceId,
    observed_at_ms: i64,
) -> Result<(), Error> {
    let timestamp = observed_high_watermark(conn, gateway)?
        .map_or(observed_at_ms, |current| current.max(observed_at_ms));
    conn.execute(
        "INSERT INTO metadata(k,v) VALUES(?,?) ON CONFLICT(k) DO UPDATE SET v=excluded.v",
        params![observed_high_watermark_key(gateway), timestamp],
    )?;
    Ok(())
}

fn observed_high_watermark(conn: &Connection, gateway: DeviceId) -> Result<Option<i64>, Error> {
    conn.query_row(
        "SELECT v FROM metadata WHERE k=?",
        [observed_high_watermark_key(gateway)],
        |row| row.get::<_, String>(0),
    )
    .optional()?
    .map(|value| value.parse::<i64>().map_err(|_| Error::Database))
    .transpose()
}

fn linked_since_key(gateway: DeviceId) -> String {
    format!("rcs_linked_since_v1/{gateway}")
}

fn observed_high_watermark_key(gateway: DeviceId) -> String {
    format!("rcs_capability_observed_high_watermark_v1/{gateway}")
}

pub(crate) fn status_projection(
    conn: &Connection,
    gateway: DeviceId,
    now_ms: i64,
) -> Result<Option<RcsLinkStatusProjection>, Error> {
    let row: Option<(String, bool, bool, bool, i64, u64, u32)> = conn
        .query_row("SELECT state,availability,health,phone_responsive,observed_at_ms,capability_revision,feature_bits FROM rcs_link_status_projection WHERE gateway_device_id=?", [gateway.to_string()], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?, row.get(6)?)))
        .optional()?;
    row.map(
        |(
            state,
            availability,
            health,
            phone_responsive,
            observed_at_ms,
            capability_revision,
            feature_bits,
        )| {
            Ok(RcsLinkStatusProjection {
                current: fresh_at(now_ms, observed_at_ms),
                status: RcsLinkStatusEvent {
                    gateway_device_id: gateway,
                    state: parse_state(&state)?,
                    availability,
                    health,
                    phone_responsive,
                    observed_at_ms,
                    capability_revision,
                    feature_bits,
                    linked_since_ms: linked_since(conn, gateway)?,
                },
            })
        },
    )
    .transpose()
}

fn fresh_at(now_ms: i64, observed_at_ms: i64) -> bool {
    now_ms >= observed_at_ms && now_ms - observed_at_ms <= RCS_CAPABILITY_STALE_MS
}

pub(crate) fn capability_state(value: RcsCapabilityState) -> &'static str {
    match value {
        RcsCapabilityState::Linked => "linked",
        RcsCapabilityState::Unlinked => "unlinked",
        RcsCapabilityState::NeedsRelink => "needs_relink",
        RcsCapabilityState::SignedOut => "signed_out",
        RcsCapabilityState::Unavailable => "unavailable",
    }
}
fn eligibility(value: RcsEligibility) -> &'static str {
    match value {
        RcsEligibility::Eligible => "eligible",
        RcsEligibility::NotEligible => "not_eligible",
        RcsEligibility::Unknown => "unknown",
    }
}
fn parse_eligibility(value: &str) -> Result<RcsEligibility, Error> {
    match value {
        "eligible" => Ok(RcsEligibility::Eligible),
        "not_eligible" => Ok(RcsEligibility::NotEligible),
        "unknown" => Ok(RcsEligibility::Unknown),
        _ => Err(Error::Database),
    }
}

fn parse_state(value: &str) -> Result<RcsCapabilityState, Error> {
    match value {
        "linked" => Ok(RcsCapabilityState::Linked),
        "unlinked" => Ok(RcsCapabilityState::Unlinked),
        "needs_relink" => Ok(RcsCapabilityState::NeedsRelink),
        "signed_out" => Ok(RcsCapabilityState::SignedOut),
        "unavailable" => Ok(RcsCapabilityState::Unavailable),
        _ => Err(Error::Database),
    }
}
