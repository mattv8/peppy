use crate::{
    CommandId, ConversationId, Error, MessagePayload,
    rcs_mapping::{self, RCS_MAPPING_FRESHNESS_MS},
};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

const CARRIER_ONLY_PROOF_PREFIX: &str = "rcs_carrier_only_proof_v1/";

/// A completed, target-scoped local lookup which found no eligible RCS target.
/// This is intentionally a typed host result: unknown, partial, and failed lookups have no
/// representation here and therefore cannot be persisted as carrier authority.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CarrierOnlyProof {
    pub conversation_id: ConversationId,
    pub recipients: Vec<String>,
    pub subscription_id: String,
    pub observed_at_ms: i64,
    pub link_generation: u64,
    pub session_generation: u64,
    pub context_revision: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct StoredCarrierOnlyProof {
    recipients: Vec<String>,
    subscription_id: String,
    observed_at_ms: i64,
    expires_at_ms: i64,
    link_generation: u64,
    session_generation: u64,
    context_revision: u64,
}

pub(crate) fn record_carrier_only_proof(
    conn: &Connection,
    proof: &CarrierOnlyProof,
    now_ms: i64,
) -> Result<(), Error> {
    if now_ms.saturating_sub(proof.observed_at_ms).abs() > RCS_MAPPING_FRESHNESS_MS
        || proof.subscription_id.is_empty()
        || !rcs_mapping::context_is_current(
            conn,
            proof.link_generation,
            proof.session_generation,
            proof.context_revision,
            true,
        )?
    {
        return Err(Error::InvalidRequest("carrier-only proof is not current"));
    }
    let recipients = canonical_recipients(&proof.recipients)?;
    let stored = StoredCarrierOnlyProof {
        recipients,
        subscription_id: proof.subscription_id.clone(),
        observed_at_ms: proof.observed_at_ms,
        expires_at_ms: proof
            .observed_at_ms
            .saturating_add(RCS_MAPPING_FRESHNESS_MS),
        link_generation: proof.link_generation,
        session_generation: proof.session_generation,
        context_revision: proof.context_revision,
    };
    conn.execute(
        "INSERT INTO metadata(k,v) VALUES(?,?) ON CONFLICT(k) DO UPDATE SET v=excluded.v",
        params![
            proof_key(proof.conversation_id),
            serde_json::to_string(&stored).map_err(|_| Error::Database)?
        ],
    )?;
    Ok(())
}

pub(crate) fn carrier_admission(
    conn: &Connection,
    conversation_id: ConversationId,
    recipients: &[String],
    subscription_id: &str,
    now_ms: i64,
) -> Result<bool, Error> {
    let Some(linked) = rcs_mapping::current_link_state(conn)? else {
        return Ok(true);
    };
    if !linked {
        return Ok(false);
    }
    if rcs_mapping::has_proven_mapping(conn, conversation_id)? {
        return Ok(false);
    }
    let Some(value) = conn
        .query_row(
            "SELECT v FROM metadata WHERE k=?",
            [proof_key(conversation_id)],
            |row| row.get::<_, String>(0),
        )
        .optional()?
    else {
        return Ok(false);
    };
    let proof: StoredCarrierOnlyProof =
        serde_json::from_str(&value).map_err(|_| Error::Database)?;
    Ok(proof.expires_at_ms >= now_ms
        && now_ms.saturating_sub(proof.observed_at_ms) <= RCS_MAPPING_FRESHNESS_MS
        && proof.subscription_id == subscription_id
        && proof.recipients == canonical_recipients(recipients)?
        && rcs_mapping::context_is_current(
            conn,
            proof.link_generation,
            proof.session_generation,
            proof.context_revision,
            true,
        )?)
}

fn proof_key(conversation_id: ConversationId) -> String {
    format!("{CARRIER_ONLY_PROOF_PREFIX}{conversation_id}")
}

fn canonical_recipients(recipients: &[String]) -> Result<Vec<String>, Error> {
    let recipients = recipients
        .iter()
        .map(|recipient| rcs_mapping::canonical_recipient(recipient))
        .collect::<Option<BTreeSet<_>>>()
        .ok_or(Error::InvalidRequest("invalid carrier-only recipient"))?;
    if recipients.is_empty() {
        return Err(Error::InvalidRequest("missing carrier-only recipient"));
    }
    Ok(recipients.into_iter().collect())
}

/// Local-only authority to submit one exact RCS command to the linked transport.
/// It is deliberately not serializable: Google identifiers and proof material stay local.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RcsPermit {
    pub command_id: CommandId,
    pub conversation_id: ConversationId,
    pub tmp_id: String,
    pub mapping_revision: u64,
    pub proof_digest: [u8; 32],
    pub google_conversation_id: String,
    pub message: MessagePayload,
}
