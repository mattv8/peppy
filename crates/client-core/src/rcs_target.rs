use crate::{CarrierOnlyProof, ConversationId, conversation_match::canonical_address};
use phonenumber::parse;
use rusqlite::{Connection, OptionalExtension};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};

/// Regions with supported short-code rules. NANP short codes are five or six digits beginning 2–9.
const SHORT_CODE_REGIONS: &[&str] = &["US", "CA"];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValidatedCarrierRouteContext {
    subscription_id: String,
    link_generation: u64,
    session_generation: u64,
    context_revision: u64,
    identity_region: String,
    canonicalizer_revision: u32,
}

impl ValidatedCarrierRouteContext {
    pub(crate) fn new(
        subscription_id: String,
        link_generation: u64,
        session_generation: u64,
        context_revision: u64,
        identity_region: String,
        canonicalizer_revision: u32,
    ) -> Self {
        Self {
            subscription_id,
            link_generation,
            session_generation,
            context_revision,
            identity_region,
            canonicalizer_revision,
        }
    }
}

/// Local-only RCS target result. It is intentionally not serializable because it may carry a
/// Google conversation identifier or carrier-proof material.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RcsTargetClassification {
    Found {
        google_conversation_id: String,
        snapshot_token: String,
    },
    Ambiguous {
        snapshot_token: String,
    },
    NotFoundComplete {
        carrier_only_proof: CarrierOnlyProof,
    },
    Unknown {
        snapshot_token: Option<String>,
    },
    Unsupported {
        snapshot_token: String,
    },
}

pub type RcsTargetDecision = RcsTargetClassification;

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct ConversationCandidateSnapshot {
    code: String,
    snapshot_token: String,
    generation: u64,
    stream_epoch: u64,
    revision: u64,
    complete: bool,
    conversations: Vec<ConversationCandidateRecord>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct ConversationCandidateRecord {
    google_conversation_id: String,
    conversation_type: i32,
    conversation_status: i32,
    classification: CandidateClassification,
    group: bool,
    visible_non_self_count: usize,
    participants: Vec<ConversationCandidateParticipant>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum CandidateClassification {
    Eligible,
    Unsupported,
    Unknown,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct ConversationCandidateParticipant {
    participant_id: String,
    identifier_type: i32,
    raw_number: String,
    formatted_number: String,
    participant_id_present: bool,
    number_present: bool,
    visible: bool,
    #[serde(rename = "self")]
    self_: bool,
}

/// Classifies one locally supplied Go snapshot without persisting participant facts or parsing
/// failures. `route_context` is core-validated input, not a JSON caller parameter.
pub(crate) fn classify_rcs_target(
    conn: &Connection,
    peppy_conversation_id: ConversationId,
    participant_address: &str,
    route_context: ValidatedCarrierRouteContext,
    snapshot_json: &str,
    now_ms: i64,
) -> RcsTargetClassification {
    let Ok(snapshot) = serde_json::from_str::<ConversationCandidateSnapshot>(snapshot_json) else {
        return RcsTargetClassification::Unknown {
            snapshot_token: None,
        };
    };
    let token = snapshot.snapshot_token.clone();
    if !valid_snapshot(&snapshot, &route_context) {
        return unknown(token);
    }

    let region = match resolved_region(conn) {
        Ok(region) => region,
        Err(()) => return unknown(token),
    };
    if !participant_address.trim().starts_with('+') && region.is_none() {
        return unknown(token);
    }
    let canonical_target = canonical_address(participant_address, region.as_deref());
    if is_short_code(participant_address, region.as_deref(), &canonical_target) {
        return RcsTargetClassification::Unsupported {
            snapshot_token: token,
        };
    }
    if !is_valid_phone(&canonical_target)
        || !address_belongs_to_conversation(
            conn,
            participant_address,
            peppy_conversation_id,
            region.as_deref(),
        )
    {
        return unknown(token);
    }

    let mut records = BTreeMap::new();
    for record in snapshot.conversations {
        if record.google_conversation_id.is_empty() {
            return unknown(token);
        }
        if let Some(previous) =
            records.insert(record.google_conversation_id.clone(), record.clone())
            && previous != record
        {
            return unknown(token);
        }
    }

    let mut matches = BTreeSet::new();
    for record in records.values() {
        match classify_record(record, &canonical_target, region.as_deref()) {
            RecordClass::Match => {
                matches.insert(record.google_conversation_id.clone());
            }
            RecordClass::Unsupported => {}
            RecordClass::NonMatch => {}
            RecordClass::Unknown => return unknown(token),
        }
    }
    match matches.len() {
        0 => RcsTargetClassification::NotFoundComplete {
            carrier_only_proof: CarrierOnlyProof {
                conversation_id: peppy_conversation_id,
                recipients: vec![canonical_target.clone()],
                subscription_id: route_context.subscription_id,
                observed_at_ms: now_ms,
                link_generation: route_context.link_generation,
                session_generation: route_context.session_generation,
                context_revision: route_context.context_revision,
                canonical_recipient: canonical_target,
                identity_region: region.unwrap_or_default(),
                canonicalizer_revision: route_context.canonicalizer_revision,
            },
        },
        1 => RcsTargetClassification::Found {
            google_conversation_id: matches.into_iter().next().unwrap_or_default(),
            snapshot_token: token,
        },
        _ => RcsTargetClassification::Ambiguous {
            snapshot_token: token,
        },
    }
}

pub(crate) fn commit_complete_absence(
    conn: &Connection,
    proof: &CarrierOnlyProof,
    now_ms: i64,
) -> Result<(), crate::Error> {
    if !crate::rcs_mapping::context_is_current(
        conn,
        proof.link_generation,
        proof.session_generation,
        proof.context_revision,
        true,
    )? {
        return Err(crate::Error::InvalidRequest(
            "carrier-only proof is not current",
        ));
    }
    conn.execute(
        "UPDATE rcs_conversation_mappings SET invalidated_at_ms=?, invalidation_code='complete_absence' WHERE peppy_conversation_id=? AND invalidated_at_ms IS NULL AND EXISTS(SELECT 1 FROM rcs_mapping_proofs p WHERE p.peppy_conversation_id=rcs_conversation_mappings.peppy_conversation_id AND p.mapping_revision=rcs_conversation_mappings.mapping_revision AND p.canonical_participant=? AND p.identity_region=?)",
        rusqlite::params![now_ms, proof.conversation_id.to_string(), proof.canonical_recipient, proof.identity_region],
    )?;
    crate::rcs_send::record_carrier_only_proof(conn, proof, now_ms)
}

enum RecordClass {
    Match,
    NonMatch,
    Unsupported,
    Unknown,
}

fn valid_snapshot(
    snapshot: &ConversationCandidateSnapshot,
    route: &ValidatedCarrierRouteContext,
) -> bool {
    snapshot.code == "conversation_candidate_snapshot"
        && snapshot.complete
        && snapshot.snapshot_token
            == format!(
                "{}:{}:{}",
                snapshot.generation, snapshot.stream_epoch, snapshot.revision
            )
        && snapshot.generation == route.link_generation
        && snapshot.stream_epoch == route.session_generation
        && snapshot.revision == route.context_revision
        && !route.subscription_id.is_empty()
}

fn resolved_region(conn: &Connection) -> Result<Option<String>, ()> {
    let mut statement = conn
        .prepare("SELECT body FROM contact_books WHERE forgotten=0")
        .map_err(|_| ())?;
    let rows = statement
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|_| ())?;
    let regions = rows
        .filter_map(Result::ok)
        .filter_map(|body| crate::contact_resolution::region(&body))
        .collect::<BTreeSet<_>>();
    Ok((regions.len() == 1).then(|| regions.into_iter().next().unwrap_or_default()))
}

fn address_belongs_to_conversation(
    conn: &Connection,
    address: &str,
    conversation_id: ConversationId,
    region: Option<&str>,
) -> bool {
    let canonical = canonical_address(address, region);
    [address.trim(), canonical.as_str()]
        .into_iter()
        .any(|candidate| {
            conn.query_row(
                "SELECT conversation_id FROM addresses WHERE address=?",
                [candidate],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .ok()
            .flatten()
            .is_some_and(|stored| stored == conversation_id.to_string())
        })
}

fn classify_record(
    record: &ConversationCandidateRecord,
    target: &str,
    region: Option<&str>,
) -> RecordClass {
    if !participant_facts_consistent(record)
        || matches!(record.classification, CandidateClassification::Unknown)
    {
        return RecordClass::Unknown;
    }
    let peers = record
        .participants
        .iter()
        .filter(|participant| participant.visible && !participant.self_)
        .collect::<Vec<_>>();
    if peers.len() != record.visible_non_self_count || record.group != (peers.len() > 1) {
        return RecordClass::Unknown;
    }
    if matches!(record.classification, CandidateClassification::Eligible) {
        if record.conversation_type != 2
            || record.conversation_status != 1
            || record.group
            || peers.len() != 1
            || !telephone_peer(peers[0], region)
        {
            return RecordClass::Unknown;
        }
        return if canonical_address(&peers[0].raw_number, region) == target {
            RecordClass::Match
        } else {
            RecordClass::NonMatch
        };
    }
    if record.group
        || peers.is_empty()
        || record.conversation_type != 2
        || record.conversation_status != 1
    {
        return RecordClass::Unsupported;
    }
    RecordClass::Unknown
}

fn participant_facts_consistent(record: &ConversationCandidateRecord) -> bool {
    record.participants.iter().all(|participant| {
        participant.participant_id_present == !participant.participant_id.is_empty()
            && participant.number_present == !participant.raw_number.is_empty()
            && (!participant.number_present || participant.identifier_type == 1)
    })
}

fn telephone_peer(participant: &ConversationCandidateParticipant, region: Option<&str>) -> bool {
    participant.number_present
        && participant.identifier_type == 1
        && is_valid_phone(&canonical_address(&participant.raw_number, region))
}

fn is_valid_phone(address: &str) -> bool {
    address.starts_with('+') && parse(None, address).is_ok_and(|number| number.is_valid())
}

fn is_short_code(address: &str, region: Option<&str>, canonical: &str) -> bool {
    let trimmed = address.trim();
    let Some(region) = region else {
        return false;
    };
    SHORT_CODE_REGIONS.contains(&region)
        && (5..=6).contains(&trimmed.len())
        && trimmed.bytes().all(|byte| byte.is_ascii_digit())
        && matches!(trimmed.as_bytes().first(), Some(b'2'..=b'9'))
        && !is_valid_phone(canonical)
}

fn unknown(snapshot_token: String) -> RcsTargetClassification {
    RcsTargetClassification::Unknown {
        snapshot_token: Some(snapshot_token),
    }
}
