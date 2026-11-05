use crate::ConversationId;
use crate::Error;
use rusqlite::{Connection, OptionalExtension, params};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RcsMappingClass {
    OneToOne,
    SelfChat,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RcsMappingProof {
    pub canonical_participant: String,
    pub identity_region: String,
    pub own_identity: String,
    pub digest: [u8; 32],
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RcsMapping {
    pub conversation_id: ConversationId,
    pub google_conversation_id: String,
    pub class: RcsMappingClass,
    pub proof_version: u32,
    pub proof: RcsMappingProof,
    pub mapping_revision: u64,
    pub verified_at_ms: i64,
}

pub(crate) fn put(conn: &Connection, mapping: &RcsMapping) -> Result<(), Error> {
    let class = match mapping.class {
        RcsMappingClass::OneToOne => "one_to_one",
        RcsMappingClass::SelfChat => "self_chat",
    };
    conn.execute("INSERT INTO rcs_conversation_mappings(peppy_conversation_id,google_conversation_id,class,proof_version,proof_hash,mapping_revision,verified_at_ms,invalidated_at_ms,invalidation_code) VALUES(?,?,?,?,?,?,?,NULL,NULL) ON CONFLICT(peppy_conversation_id) DO UPDATE SET google_conversation_id=excluded.google_conversation_id,class=excluded.class,proof_version=excluded.proof_version,proof_hash=excluded.proof_hash,mapping_revision=excluded.mapping_revision,verified_at_ms=excluded.verified_at_ms,invalidated_at_ms=NULL,invalidation_code=NULL", params![mapping.conversation_id.to_string(), mapping.google_conversation_id, class, mapping.proof_version, mapping.proof.digest.to_vec(), mapping.mapping_revision, mapping.verified_at_ms])?;
    conn.execute("INSERT INTO rcs_mapping_proofs(peppy_conversation_id,mapping_revision,canonical_participant,identity_region,own_identity) VALUES(?,?,?,?,?) ON CONFLICT(peppy_conversation_id,mapping_revision) DO UPDATE SET canonical_participant=excluded.canonical_participant,identity_region=excluded.identity_region,own_identity=excluded.own_identity", params![mapping.conversation_id.to_string(), mapping.mapping_revision, mapping.proof.canonical_participant, mapping.proof.identity_region, mapping.proof.own_identity])?;
    Ok(())
}

pub(crate) fn get(
    conn: &Connection,
    conversation_id: ConversationId,
) -> Result<Option<RcsMapping>, Error> {
    conn.query_row("SELECT m.google_conversation_id,m.class,m.proof_version,m.proof_hash,m.mapping_revision,m.verified_at_ms,p.canonical_participant,p.identity_region,p.own_identity FROM rcs_conversation_mappings m JOIN rcs_mapping_proofs p ON p.peppy_conversation_id=m.peppy_conversation_id AND p.mapping_revision=m.mapping_revision WHERE m.peppy_conversation_id=? AND m.invalidated_at_ms IS NULL", [conversation_id.to_string()], |r| {
        let digest: Vec<u8> = r.get(3)?; let digest: [u8; 32] = digest.try_into().map_err(|_| rusqlite::Error::InvalidColumnType(3, "proof_hash".into(), rusqlite::types::Type::Blob))?;
        Ok((r.get::<_,String>(0)?, r.get::<_,String>(1)?, r.get::<_,u32>(2)?, digest, r.get::<_,u64>(4)?, r.get::<_,i64>(5)?, r.get::<_,String>(6)?, r.get::<_,String>(7)?, r.get::<_,String>(8)?))
    }).optional()?.map(|(google, class, version, digest, revision, verified, participant, region, own)| Ok(RcsMapping { conversation_id, google_conversation_id: google, class: match class.as_str() { "one_to_one" => RcsMappingClass::OneToOne, "self_chat" => RcsMappingClass::SelfChat, _ => return Err(Error::Database) }, proof_version: version, proof: RcsMappingProof { canonical_participant: participant, identity_region: region, own_identity: own, digest }, mapping_revision: revision, verified_at_ms: verified })).transpose()
}
