use crate::{CarrierOnlyProof, ConversationId, conversation_match::canonical_address};
use phonenumber::parse;
use rusqlite::{Connection, OptionalExtension};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};

/// Regions with supported short-code rules. NANP short codes are five or six digits beginning 2–9.
const SHORT_CODE_REGIONS: &[&str] = &["US", "CA"];
const SMS_CONVERSATION_TYPE: i32 = 1;
const RCS_CONVERSATION_TYPE: i32 = 2;
const ACTIVE_CONVERSATION_STATUS: i32 = 1;
const PHONE_IDENTIFIER_TYPE: i32 = 1;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValidatedCarrierRouteContext {
    subscription_id: String,
    link_generation: u64,
    session_generation: u64,
    context_revision: u64,
    identity_region: String,
    canonicalizer_revision: u32,
    // This stays absent until the route layer can supply current accepted native evidence.
    confirmed_own_identity: Option<String>,
}

impl ValidatedCarrierRouteContext {
    pub(crate) fn new(
        subscription_id: String,
        link_generation: u64,
        session_generation: u64,
        context_revision: u64,
        identity_region: String,
        canonicalizer_revision: u32,
        confirmed_own_identity: Option<String>,
    ) -> Self {
        Self {
            subscription_id,
            link_generation,
            session_generation,
            context_revision,
            identity_region,
            canonicalizer_revision,
            confirmed_own_identity,
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
    if is_short_code(participant_address, region.as_deref()) {
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

    match classify_records(
        records.values(),
        &canonical_target,
        region.as_deref(),
        route_context.confirmed_own_identity.as_deref(),
    ) {
        RecordSummary::Absent => RcsTargetClassification::NotFoundComplete {
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
        RecordSummary::Found(google_conversation_id) => RcsTargetClassification::Found {
            google_conversation_id,
            snapshot_token: token,
        },
        RecordSummary::Ambiguous => RcsTargetClassification::Ambiguous {
            snapshot_token: token,
        },
        RecordSummary::Unknown => unknown(token),
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
    // An empty proof region is the region-independent (plus-prefixed) form: any region matches.
    conn.execute(
        "UPDATE rcs_conversation_mappings SET invalidated_at_ms=?, invalidation_code='complete_absence' WHERE peppy_conversation_id=? AND invalidated_at_ms IS NULL AND EXISTS(SELECT 1 FROM rcs_mapping_proofs p WHERE p.peppy_conversation_id=rcs_conversation_mappings.peppy_conversation_id AND p.mapping_revision=rcs_conversation_mappings.mapping_revision AND p.canonical_participant=? AND (?='' OR p.identity_region=?))",
        rusqlite::params![
            now_ms,
            proof.conversation_id.to_string(),
            proof.canonical_recipient,
            proof.identity_region,
            proof.identity_region
        ],
    )?;
    crate::rcs_send::record_carrier_only_proof(conn, proof, now_ms)
}

enum RecordClass {
    Match,
    NonMatch,
    Unsupported,
    Unknown,
}

enum RecordSummary {
    Found(String),
    Ambiguous,
    Absent,
    Unknown,
}

fn classify_records<'a>(
    records: impl IntoIterator<Item = &'a ConversationCandidateRecord>,
    target: &str,
    region: Option<&str>,
    confirmed_own_identity: Option<&str>,
) -> RecordSummary {
    let mut matches = BTreeSet::new();
    let mut has_unknown = false;
    for record in records {
        match classify_record(record, target, region, confirmed_own_identity) {
            RecordClass::Match => {
                matches.insert(record.google_conversation_id.clone());
            }
            RecordClass::Unknown => has_unknown = true,
            RecordClass::NonMatch | RecordClass::Unsupported => {}
        }
    }
    match matches.len() {
        0 if has_unknown => RecordSummary::Unknown,
        0 => RecordSummary::Absent,
        1 => RecordSummary::Found(matches.into_iter().next().unwrap_or_default()),
        _ => RecordSummary::Ambiguous,
    }
}

/// Go's `indexRevision` is bound inside its own token and never compared with core's independent
/// `context_revision`; core's route binding is checked by the proof and admission paths.
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
        && !route.subscription_id.is_empty()
}

pub(crate) fn resolved_region(conn: &Connection) -> Result<Option<String>, ()> {
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
    confirmed_own_identity: Option<&str>,
) -> RecordClass {
    if !participant_facts_consistent(record)
        || matches!(record.classification, CandidateClassification::Unknown)
        || !known_conversation_type(record.conversation_type)
        || !known_conversation_status(record.conversation_status)
        || !self_participants_confirmed(record, region, confirmed_own_identity)
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
        if record.conversation_type != RCS_CONVERSATION_TYPE
            || record.conversation_status != ACTIVE_CONVERSATION_STATUS
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
        || record.conversation_type != RCS_CONVERSATION_TYPE
        || record.conversation_status != ACTIVE_CONVERSATION_STATUS
    {
        return RecordClass::Unsupported;
    }
    RecordClass::Unknown
}

fn participant_facts_consistent(record: &ConversationCandidateRecord) -> bool {
    record.participants.iter().all(|participant| {
        participant.participant_id_present == !participant.participant_id.is_empty()
            && participant.number_present == !participant.raw_number.is_empty()
            && participant.participant_id_present
            && participant.number_present
            && known_identifier_type(participant.identifier_type)
    })
}

fn known_conversation_type(conversation_type: i32) -> bool {
    matches!(
        conversation_type,
        SMS_CONVERSATION_TYPE | RCS_CONVERSATION_TYPE
    )
}

fn known_conversation_status(conversation_status: i32) -> bool {
    matches!(conversation_status, 1 | 2 | 3 | 5..=8)
}

fn known_identifier_type(identifier_type: i32) -> bool {
    (PHONE_IDENTIFIER_TYPE..=23).contains(&identifier_type)
}

fn self_participants_confirmed(
    record: &ConversationCandidateRecord,
    region: Option<&str>,
    confirmed_own_identity: Option<&str>,
) -> bool {
    record
        .participants
        .iter()
        .filter(|participant| participant.self_)
        .all(|participant| {
            participant.identifier_type == PHONE_IDENTIFIER_TYPE
                && confirmed_own_identity.is_some_and(|own_identity| {
                    canonical_address(own_identity, region)
                        == canonical_address(&participant.raw_number, region)
                })
        })
}

fn telephone_peer(participant: &ConversationCandidateParticipant, region: Option<&str>) -> bool {
    participant.number_present
        && participant.identifier_type == PHONE_IDENTIFIER_TYPE
        && is_valid_phone(&canonical_address(&participant.raw_number, region))
}

fn is_valid_phone(address: &str) -> bool {
    address.starts_with('+') && parse(None, address).is_ok_and(|number| number.is_valid())
}

/// The single short-code recognizer, shared by classification and carrier admission. A short code
/// needs an explicitly supported singleton region and is never also a valid regular number.
pub(crate) fn is_short_code(address: &str, region: Option<&str>) -> bool {
    let trimmed = address.trim();
    let Some(region) = region else {
        return false;
    };
    SHORT_CODE_REGIONS.contains(&region)
        && (5..=6).contains(&trimmed.len())
        && trimmed.bytes().all(|byte| byte.is_ascii_digit())
        && matches!(trimmed.as_bytes().first(), Some(b'2'..=b'9'))
        && !is_valid_phone(&canonical_address(trimmed, Some(region)))
}

fn unknown(snapshot_token: String) -> RcsTargetClassification {
    RcsTargetClassification::Unknown {
        snapshot_token: Some(snapshot_token),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TARGET: &str = "+12025550123";

    fn participant(number: &str, self_: bool) -> ConversationCandidateParticipant {
        ConversationCandidateParticipant {
            participant_id: "participant".into(),
            identifier_type: 1,
            raw_number: number.into(),
            formatted_number: String::new(),
            participant_id_present: true,
            number_present: !number.is_empty(),
            visible: true,
            self_,
        }
    }

    fn record(
        id: &str,
        classification: CandidateClassification,
        participants: Vec<ConversationCandidateParticipant>,
    ) -> ConversationCandidateRecord {
        let visible_non_self_count = participants
            .iter()
            .filter(|participant| participant.visible && !participant.self_)
            .count();
        ConversationCandidateRecord {
            google_conversation_id: id.into(),
            conversation_type: 2,
            conversation_status: 1,
            classification,
            group: visible_non_self_count > 1,
            visible_non_self_count,
            participants,
        }
    }

    #[test]
    fn unknown_records_do_not_hide_exact_matches() {
        let exact = record(
            "exact",
            CandidateClassification::Eligible,
            vec![participant(TARGET, false)],
        );
        let mut unknown = record(
            "unknown",
            CandidateClassification::Eligible,
            vec![participant("+12025550124", false)],
        );
        unknown.conversation_type = 99;

        assert!(matches!(
            classify_records([&exact, &unknown], TARGET, None, None),
            RecordSummary::Found(_)
        ));
    }

    #[test]
    fn numberless_or_forged_self_records_are_unknown() {
        let numberless_peer = record(
            "numberless-peer",
            CandidateClassification::Eligible,
            vec![participant("", false)],
        );
        let numberless_self = record(
            "numberless-self",
            CandidateClassification::Eligible,
            vec![participant(TARGET, false), participant("", true)],
        );
        let forged_self = record(
            "forged-self",
            CandidateClassification::Eligible,
            vec![
                participant("+12025550124", false),
                participant(TARGET, true),
            ],
        );

        assert!(matches!(
            classify_record(&numberless_peer, TARGET, None, None),
            RecordClass::Unknown
        ));
        assert!(matches!(
            classify_record(&numberless_self, TARGET, None, Some("+12025550999")),
            RecordClass::Unknown
        ));
        assert!(matches!(
            classify_record(&forged_self, TARGET, None, Some("+12025550999")),
            RecordClass::Unknown
        ));
    }

    #[test]
    fn strict_enum_and_format_equivalence_rules_apply() {
        let mut unknown_enum = record(
            "unknown-enum",
            CandidateClassification::Eligible,
            vec![participant(TARGET, false)],
        );
        unknown_enum.conversation_status = 99;
        let mut unknown_identifier = record(
            "unknown-identifier",
            CandidateClassification::Eligible,
            vec![participant(TARGET, false)],
        );
        unknown_identifier.participants[0].identifier_type = 99;
        let formatted = record(
            "formatted",
            CandidateClassification::Eligible,
            vec![participant("(202) 555-0123", false)],
        );
        let unrelated_sms = ConversationCandidateRecord {
            conversation_type: 1,
            classification: CandidateClassification::Unsupported,
            ..record(
                "sms",
                CandidateClassification::Unsupported,
                vec![participant("+12025550124", false)],
            )
        };

        assert!(matches!(
            classify_record(&unknown_enum, TARGET, Some("US"), None),
            RecordClass::Unknown
        ));
        assert!(matches!(
            classify_record(&unknown_identifier, TARGET, Some("US"), None),
            RecordClass::Unknown
        ));
        assert!(matches!(
            classify_record(&formatted, TARGET, Some("US"), None),
            RecordClass::Match
        ));
        assert!(matches!(
            classify_record(&unrelated_sms, TARGET, Some("US"), None),
            RecordClass::Unsupported
        ));
    }

    #[test]
    fn multiple_matches_remain_ambiguous() {
        let first = record(
            "first",
            CandidateClassification::Eligible,
            vec![participant(TARGET, false)],
        );
        let second = record(
            "second",
            CandidateClassification::Eligible,
            vec![participant(TARGET, false)],
        );

        assert!(matches!(
            classify_records([&first, &second], TARGET, None, None),
            RecordSummary::Ambiguous
        ));
    }

    #[test]
    fn snapshot_binding_requires_exact_current_values() {
        let route = ValidatedCarrierRouteContext::new("sim".into(), 1, 2, 3, "US".into(), 1, None);
        let snapshot = ConversationCandidateSnapshot {
            code: "conversation_candidate_snapshot".into(),
            snapshot_token: "1:2:3".into(),
            generation: 1,
            stream_epoch: 2,
            revision: 3,
            complete: true,
            conversations: Vec::new(),
        };

        assert!(valid_snapshot(&snapshot, &route));
        let other_index_revision = ConversationCandidateSnapshot {
            revision: 4,
            snapshot_token: "1:2:4".into(),
            ..snapshot.clone()
        };
        assert!(valid_snapshot(&other_index_revision, &route));
        let other_link = ConversationCandidateSnapshot {
            generation: 9,
            snapshot_token: "9:2:3".into(),
            ..snapshot.clone()
        };
        assert!(!valid_snapshot(&other_link, &route));
        let malformed = ConversationCandidateSnapshot {
            snapshot_token: "not-a-token".into(),
            ..snapshot
        };
        assert!(!valid_snapshot(&malformed, &route));
    }

    #[test]
    fn classifier_parses_a_trusted_fixture_and_sanitizes_malformed_json() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE contact_books(body TEXT NOT NULL, forgotten INTEGER NOT NULL);
             CREATE TABLE addresses(address TEXT NOT NULL, conversation_id TEXT NOT NULL);",
        )
        .unwrap();
        let conversation_id = ConversationId::new();
        conn.execute(
            "INSERT INTO addresses(address, conversation_id) VALUES(?, ?)",
            rusqlite::params![TARGET, conversation_id.to_string()],
        )
        .unwrap();
        let route = ValidatedCarrierRouteContext::new("sim".into(), 1, 2, 3, "US".into(), 1, None);
        let snapshot = r#"{
            "code":"conversation_candidate_snapshot",
            "snapshot_token":"1:2:3",
            "generation":1,
            "stream_epoch":2,
            "revision":3,
            "complete":true,
            "conversations":[{
                "google_conversation_id":"google-id",
                "conversation_type":2,
                "conversation_status":1,
                "classification":"eligible",
                "group":false,
                "visible_non_self_count":1,
                "participants":[{
                    "participant_id":"peer",
                    "identifier_type":1,
                    "raw_number":"+12025550123",
                    "formatted_number":"(202) 555-0123",
                    "participant_id_present":true,
                    "number_present":true,
                    "visible":true,
                    "self":false
                }]
            }]
        }"#;

        assert!(matches!(
            classify_rcs_target(&conn, conversation_id, TARGET, route, snapshot, 1_000),
            RcsTargetClassification::Found { .. }
        ));
        assert!(matches!(
            classify_rcs_target(
                &conn,
                conversation_id,
                TARGET,
                ValidatedCarrierRouteContext::new("sim".into(), 1, 2, 3, "US".into(), 1, None),
                "{not-json}",
                1_000,
            ),
            RcsTargetClassification::Unknown {
                snapshot_token: None
            }
        ));
    }

    #[test]
    fn confirmed_own_identity_makes_a_self_record_unsupported() {
        let self_record = record(
            "self",
            CandidateClassification::Unsupported,
            vec![participant("+12025550999", true)],
        );

        assert!(matches!(
            classify_record(&self_record, TARGET, None, Some("+12025550999")),
            RecordClass::Unsupported
        ));
    }
}
