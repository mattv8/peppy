use crate::{Client, ConversationId, Error};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

const MAX_DETECTORS_PER_CONVERSATION: i64 = 1_024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RcsReceiveOwner {
    Link,
    Provider,
}

impl RcsReceiveOwner {
    fn code(self) -> &'static str {
        match self {
            Self::Link => "link",
            Self::Provider => "provider",
        }
    }

    fn from_code(value: &str) -> Result<Self, Error> {
        match value {
            "link" => Ok(Self::Link),
            "provider" => Ok(Self::Provider),
            _ => Err(Error::Database),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RcsOwnership {
    pub conversation_id: ConversationId,
    pub owner: RcsReceiveOwner,
    pub ownership_generation: u64,
    pub history_token: Option<String>,
    pub detector_floor: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RcsProviderDetector {
    pub conversation_id: ConversationId,
    pub provider_row_id: i64,
    pub provider_thread_id: i64,
    pub provider_type: i64,
    pub observed_at_ms: i64,
    pub observed_generation: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RcsReceiveCode {
    OwnershipStale,
    ConversationUnproven,
    IdentityConflict,
    CursorInvalid,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RcsIdentityOutcome {
    Recorded,
    Duplicate,
    Rejected(RcsReceiveCode),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RcsCutover {
    pub owner: RcsReceiveOwner,
    pub ownership_generation: u64,
    pub freeze_detector_floor: i64,
    pub ambiguous_detector_alert: bool,
}

impl Client {
    /// Records proof-time link ownership without importing pre-proof provider history.
    pub fn begin_rcs_link_ownership(
        &self,
        conversation_id: ConversationId,
        initial_history_token: &str,
        provider_floor: i64,
    ) -> Result<RcsOwnership, Error> {
        if !valid_token(initial_history_token) || provider_floor < 0 {
            return Err(Error::InvalidRequest("invalid RCS ownership checkpoint"));
        }
        let mut store = self.lock()?;
        let tx = store
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let generation = next_generation(&tx, conversation_id)?;
        tx.execute(
            "INSERT INTO rcs_lane_ownership(conversation_id,owner,ownership_generation,history_token,detector_floor,alert_code) VALUES(?,?,?,?,?,NULL)
             ON CONFLICT(conversation_id) DO UPDATE SET owner=excluded.owner,ownership_generation=excluded.ownership_generation,history_token=excluded.history_token,detector_floor=excluded.detector_floor,alert_code=NULL",
            params![conversation_id.to_string(), RcsReceiveOwner::Link.code(), generation, initial_history_token, provider_floor],
        )?;
        tx.commit()?;
        Ok(RcsOwnership {
            conversation_id,
            owner: RcsReceiveOwner::Link,
            ownership_generation: generation as u64,
            history_token: Some(initial_history_token.into()),
            detector_floor: provider_floor,
        })
    }

    /// Persists immutable link identity before R2 associates it with a vault message.
    pub fn record_rcs_link_identity(
        &self,
        conversation_id: ConversationId,
        ownership_generation: u64,
        google_message_id: &str,
        google_conversation_id: &str,
        history_token: Option<&str>,
    ) -> Result<RcsIdentityOutcome, Error> {
        if google_message_id.is_empty() || google_conversation_id.is_empty() {
            return Ok(RcsIdentityOutcome::Rejected(
                RcsReceiveCode::IdentityConflict,
            ));
        }
        if history_token.is_some_and(|token| !valid_token(token)) {
            return Ok(RcsIdentityOutcome::Rejected(RcsReceiveCode::CursorInvalid));
        }
        let mut store = self.lock()?;
        let tx = store
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let Some(ownership) = load_ownership(&tx, conversation_id)? else {
            return Ok(RcsIdentityOutcome::Rejected(
                RcsReceiveCode::ConversationUnproven,
            ));
        };
        if ownership.owner != RcsReceiveOwner::Link
            || ownership.ownership_generation != ownership_generation
        {
            return Ok(RcsIdentityOutcome::Rejected(RcsReceiveCode::OwnershipStale));
        }
        let existing: Option<String> = tx
            .query_row(
                "SELECT conversation_id FROM rcs_message_identities WHERE google_message_id=? AND google_conversation_id=?",
                params![google_message_id, google_conversation_id],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(existing) = existing {
            return Ok(if existing == conversation_id.to_string() {
                RcsIdentityOutcome::Duplicate
            } else {
                RcsIdentityOutcome::Rejected(RcsReceiveCode::IdentityConflict)
            });
        }
        tx.execute(
            "INSERT INTO rcs_message_identities(google_message_id,google_conversation_id,conversation_id,ownership_generation) VALUES(?,?,?,?)",
            params![google_message_id, google_conversation_id, conversation_id.to_string(), ownership_generation],
        )?;
        if let Some(token) = history_token {
            tx.execute(
                "UPDATE rcs_lane_ownership SET history_token=? WHERE conversation_id=? AND ownership_generation=?",
                params![token, conversation_id.to_string(), ownership_generation],
            )?;
        }
        tx.commit()?;
        Ok(RcsIdentityOutcome::Recorded)
    }

    /// Records detector metadata only; this method never writes a vault message.
    pub fn record_rcs_provider_detector(&self, detector: RcsProviderDetector) -> Result<(), Error> {
        if detector.provider_row_id < 0
            || detector.provider_thread_id < 0
            || detector.provider_type < 0
        {
            return Err(Error::InvalidRequest("invalid RCS provider detector"));
        }
        let mut store = self.lock()?;
        let tx = store
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let conversation_id = detector.conversation_id.to_string();
        let exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM rcs_provider_detectors WHERE conversation_id=? AND provider_row_id=?)",
            params![conversation_id, detector.provider_row_id],
            |row| row.get(0),
        )?;
        if exists {
            tx.commit()?;
            return Ok(());
        }
        tx.execute(
            "DELETE FROM rcs_provider_detectors WHERE conversation_id=? AND provider_row_id IN (
                SELECT provider_row_id FROM rcs_provider_detectors
                WHERE conversation_id=? AND resolved=1
                  AND (SELECT COUNT(*) FROM rcs_provider_detectors WHERE conversation_id=?) >= ?
                ORDER BY observed_at_ms,provider_row_id
                LIMIT 1
            )",
            params![
                conversation_id,
                conversation_id,
                conversation_id,
                MAX_DETECTORS_PER_CONVERSATION
            ],
        )?;
        let count: i64 = tx.query_row(
            "SELECT COUNT(*) FROM rcs_provider_detectors WHERE conversation_id=?",
            [conversation_id.as_str()],
            |row| row.get(0),
        )?;
        if count >= MAX_DETECTORS_PER_CONVERSATION {
            tx.execute(
                "UPDATE rcs_lane_ownership SET alert_code='detector_overflow' WHERE conversation_id=?",
                [conversation_id.as_str()],
            )?;
            tx.commit()?;
            return Ok(());
        }
        tx.execute(
            "INSERT INTO rcs_provider_detectors(conversation_id,provider_row_id,provider_thread_id,provider_type,observed_at_ms,observed_generation,resolved) VALUES(?,?,?,?,?,?,0)",
            params![conversation_id, detector.provider_row_id, detector.provider_thread_id, detector.provider_type, detector.observed_at_ms, detector.observed_generation],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Freezes link writing and makes provider ownership effective for rows after this floor.
    pub fn begin_rcs_unlink_cutover(
        &self,
        conversation_id: ConversationId,
    ) -> Result<RcsCutover, Error> {
        let mut store = self.lock()?;
        let tx = store
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let Some(ownership) = load_ownership(&tx, conversation_id)? else {
            return Err(Error::NotFound);
        };
        if ownership.owner != RcsReceiveOwner::Link {
            return Err(Error::InvalidRequest("RCS link is not the current writer"));
        }
        let freeze_detector_floor: i64 = tx.query_row(
            "SELECT COALESCE(MAX(provider_row_id),?) FROM rcs_provider_detectors WHERE conversation_id=?",
            params![ownership.detector_floor, conversation_id.to_string()],
            |row| row.get(0),
        )?;
        let ambiguous_detector_alert: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM rcs_provider_detectors WHERE conversation_id=? AND provider_row_id<=? AND resolved=0)",
            params![conversation_id.to_string(), freeze_detector_floor],
            |row| row.get(0),
        )?;
        let generation = ownership
            .ownership_generation
            .checked_add(1)
            .ok_or(Error::Database)?;
        tx.execute(
            "UPDATE rcs_lane_ownership SET owner=?,ownership_generation=?,history_token=NULL,detector_floor=?,alert_code=? WHERE conversation_id=?",
            params![RcsReceiveOwner::Provider.code(), generation, freeze_detector_floor, ambiguous_detector_alert.then_some("ambiguous_provider_window"), conversation_id.to_string()],
        )?;
        tx.commit()?;
        Ok(RcsCutover {
            owner: RcsReceiveOwner::Provider,
            ownership_generation: generation,
            freeze_detector_floor,
            ambiguous_detector_alert,
        })
    }
}

fn valid_token(token: &str) -> bool {
    !token.is_empty()
        && token.len() <= 4_096
        && token
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

fn next_generation(conn: &Connection, conversation_id: ConversationId) -> Result<i64, Error> {
    let previous: Option<i64> = conn
        .query_row(
            "SELECT ownership_generation FROM rcs_lane_ownership WHERE conversation_id=?",
            [conversation_id.to_string()],
            |row| row.get(0),
        )
        .optional()?;
    previous.unwrap_or(0).checked_add(1).ok_or(Error::Database)
}

fn load_ownership(
    conn: &Connection,
    conversation_id: ConversationId,
) -> Result<Option<RcsOwnership>, Error> {
    let row: Option<(String, i64, Option<String>, i64)> = conn.query_row(
        "SELECT owner,ownership_generation,history_token,detector_floor FROM rcs_lane_ownership WHERE conversation_id=?",
        [conversation_id.to_string()],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
    )
    .optional()?;
    row.map(
        |(owner, ownership_generation, history_token, detector_floor)| {
            Ok(RcsOwnership {
                conversation_id,
                owner: RcsReceiveOwner::from_code(&owner)?,
                ownership_generation: ownership_generation as u64,
                history_token,
                detector_floor,
            })
        },
    )
    .transpose()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ClientConfig, DatabaseKey, DeviceId, VaultId};

    fn config(directory: &tempfile::TempDir) -> ClientConfig {
        ClientConfig {
            database_path: directory.path().join("receive.db"),
            vault_id: VaultId::new(),
            device_id: DeviceId::new(),
        }
    }

    #[test]
    fn version_48_upgrade_preserves_rows_and_rejects_downgrade() {
        let directory = tempfile::TempDir::new().unwrap();
        let config = config(&directory);
        let client = Client::open(config.clone(), DatabaseKey::new(&[17; 32]).unwrap()).unwrap();
        {
            let store = client.lock().unwrap();
            store.conn.execute_batch(
                "INSERT INTO rcs_link_status_projection VALUES('gateway','linked',1,1,1,1,1,1,1);
                 DROP TABLE rcs_lane_ownership;
                 DROP TABLE rcs_message_identities;
                 DROP TABLE rcs_provider_detectors;
                 UPDATE schema_meta SET version=48;",
            ).unwrap();
        }
        drop(client);
        let upgraded = Client::open(config.clone(), DatabaseKey::new(&[17; 32]).unwrap()).unwrap();
        let store = upgraded.lock().unwrap();
        let state: String = store
            .conn
            .query_row(
                "SELECT state FROM rcs_link_status_projection WHERE gateway_device_id='gateway'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(state, "linked");
        let table: String = store
            .conn
            .query_row(
                "SELECT name FROM sqlite_master WHERE type='table' AND name='rcs_lane_ownership'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(table, "rcs_lane_ownership");
        store
            .conn
            .execute("UPDATE schema_meta SET version=50", [])
            .unwrap();
        drop(store);
        drop(upgraded);
        assert!(matches!(
            Client::open(config, DatabaseKey::new(&[17; 32]).unwrap()),
            Err(Error::UnsupportedSchema)
        ));
    }
}
