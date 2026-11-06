use super::*;

const CLOCK: &str = "conversation_folder_clock";

impl ConversationFolder {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            ConversationFolder::Inbox => "inbox",
            ConversationFolder::Archived => "archived",
            ConversationFolder::Spam => "spam",
        }
    }

    pub(super) fn from_db(value: &str) -> Result<Self, Error> {
        match value {
            "inbox" => Ok(ConversationFolder::Inbox),
            "archived" => Ok(ConversationFolder::Archived),
            "spam" => Ok(ConversationFolder::Spam),
            _ => Err(Error::Database),
        }
    }
}

/// The single effective-folder rule, as a SQL `CASE` yielding `'spam'`, `'archived'` or `'inbox'`.
/// Aliases: `f` is the `conversation_folders` row (LEFT JOIN, may be NULL); `latest` is the
/// conversation's newest `sort_at_ms` expression. Archive resurfaces once a newer message arrives.
pub(super) fn effective_folder_sql(latest: &str) -> String {
    format!(
        "CASE WHEN f.folder='spam' THEN 'spam' WHEN f.folder='archived' AND {latest}<=f.archived_through_ms THEN 'archived' ELSE 'inbox' END"
    )
}

pub(super) fn valid_event(
    conversation_id: ConversationId,
    folder: ConversationFolder,
    archived_through_ms: Option<i64>,
    logical_revision: u64,
    writer: &str,
    producer: &str,
) -> bool {
    !conversation_id.0.is_nil()
        && uuid::Uuid::parse_str(writer).is_ok()
        && writer == producer
        && logical_revision != 0
        && logical_revision < i64::MAX as u64
        && (archived_through_ms.is_some() == (folder == ConversationFolder::Archived))
}

#[allow(clippy::too_many_arguments)]
pub(super) fn apply(
    conn: &Connection,
    conversation_id: ConversationId,
    folder: ConversationFolder,
    archived_through_ms: Option<i64>,
    revision: u64,
    writer: &str,
    producer: &str,
    sequence: u64,
) -> Result<(), Error> {
    let revision = i64::try_from(revision)
        .map_err(|_| Error::InvalidRequest("conversation folder revision"))?;
    let sequence = i64::try_from(sequence)
        .map_err(|_| Error::InvalidRequest("conversation folder sequence"))?;
    let clock = get_meta(conn, CLOCK)?
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or(0);
    set_meta(conn, CLOCK, &clock.max(revision).to_string())?;
    let changed = conn.execute("INSERT INTO conversation_folders(conversation_id,folder,archived_through_ms,logical_revision,writer_device_id,producer_device_id,source_sequence) VALUES(?,?,?,?,?,?,?) ON CONFLICT(conversation_id) DO UPDATE SET folder=excluded.folder,archived_through_ms=excluded.archived_through_ms,logical_revision=excluded.logical_revision,writer_device_id=excluded.writer_device_id,producer_device_id=excluded.producer_device_id,source_sequence=excluded.source_sequence WHERE excluded.logical_revision>conversation_folders.logical_revision OR (excluded.logical_revision=conversation_folders.logical_revision AND excluded.writer_device_id>conversation_folders.writer_device_id)", params![conversation_id.to_string(), folder.as_str(), archived_through_ms, revision, writer, producer, sequence])?;
    if changed > 0 && folder == ConversationFolder::Spam {
        conn.execute("DELETE FROM banner_candidates WHERE acknowledged=0 AND kind='message' AND conversation_id=?", [conversation_id.to_string()])?;
    }
    Ok(())
}

impl Client {
    pub fn set_conversation_folder(
        &self,
        conversation_id: ConversationId,
        folder: ConversationFolder,
    ) -> Result<(), Error> {
        let mut guard = self.lock()?;
        let (conn, ctx) = guard.parts();
        if ctx
            .active_epoch
            .is_none_or(|epoch| !ctx.keys.contains_key(&epoch))
        {
            return Err(Error::KeysUnavailable);
        }
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let archived_through_ms: Option<i64> = tx.query_row(
            "SELECT MAX(sort_at_ms) FROM messages WHERE conversation_id=?",
            [conversation_id.to_string()],
            |row| row.get(0),
        )?;
        let Some(archived_through_ms) = archived_through_ms else {
            return Err(Error::InvalidRequest("conversation folder"));
        };
        let archived_through_ms =
            (folder == ConversationFolder::Archived).then_some(archived_through_ms);
        let clock = get_meta(&tx, CLOCK)?
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(0);
        let existing: Option<i64> = tx
            .query_row(
                "SELECT logical_revision FROM conversation_folders WHERE conversation_id=?",
                [conversation_id.to_string()],
                |row| row.get(0),
            )
            .optional()?;
        let base_revision = clock.max(
            existing
                .and_then(|value| u64::try_from(value).ok())
                .unwrap_or(0),
        );
        if base_revision >= i64::MAX as u64 - 1 {
            return Err(Error::InvalidRequest("conversation folder revision"));
        }
        let revision = base_revision + 1;
        set_meta(&tx, CLOCK, &revision.to_string())?;
        let (_, sequence) = enqueue(
            &tx,
            &ctx,
            EnvelopePurpose::Event,
            None,
            None,
            &PrivatePayload::ConversationFolder {
                conversation_id,
                folder,
                archived_through_ms,
                logical_revision: revision,
                writer_device_id: ctx.device_id.to_string(),
            },
        )?;
        apply(
            &tx,
            conversation_id,
            folder,
            archived_through_ms,
            revision,
            &ctx.device_id.to_string(),
            &ctx.device_id.to_string(),
            sequence,
        )?;
        Ok(tx.commit()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use peppy_crypto::{create_vault_check_header, derive_root_key};

    #[test]
    fn archived_events_require_a_watermark() {
        let id = ConversationId::new();
        let writer = DeviceId::new().to_string();
        assert!(valid_event(
            id,
            ConversationFolder::Archived,
            Some(1),
            1,
            &writer,
            &writer
        ));
        assert!(!valid_event(
            id,
            ConversationFolder::Archived,
            None,
            1,
            &writer,
            &writer
        ));
        assert!(!valid_event(
            id,
            ConversationFolder::Inbox,
            Some(1),
            1,
            &writer,
            &writer
        ));
    }

    #[test]
    fn revision_ceiling_rejects_without_mutating_state_or_outbox() {
        let dir = tempfile::TempDir::new().unwrap();
        let vault_id = VaultId::new();
        let profile = KeyProfile::new(vault_id.0, 1).unwrap();
        let root = derive_root_key("correct horse battery staple", &profile).unwrap();
        let header = create_vault_check_header(&root, profile.clone()).unwrap();
        let client = Client::open(
            ClientConfig {
                database_path: dir.path().join("folder-ceiling.db"),
                vault_id,
                device_id: DeviceId::new(),
            },
            DatabaseKey::new(&[7; 32]).unwrap(),
        )
        .unwrap();
        client
            .unlock(&profile, &header, "correct horse battery staple")
            .unwrap();
        let conversation = client
            .capture_incoming(IncomingSms {
                conversation_id: None,
                sender_address: "+15555550100".into(),
                body: "ceiling".into(),
                provider_message_id: Some("folder-ceiling".into()),
                imported: false,
            })
            .unwrap()
            .conversation_id;
        let store = client.lock().unwrap();
        set_meta(&store.conn, CLOCK, &(i64::MAX - 1).to_string()).unwrap();
        let outbox_before: i64 = store
            .conn
            .query_row("SELECT COUNT(*) FROM outbox", [], |row| row.get(0))
            .unwrap();
        drop(store);
        assert!(matches!(
            client.set_conversation_folder(conversation, ConversationFolder::Archived),
            Err(Error::InvalidRequest("conversation folder revision"))
        ));
        let store = client.lock().unwrap();
        let outbox_after: i64 = store
            .conn
            .query_row("SELECT COUNT(*) FROM outbox", [], |row| row.get(0))
            .unwrap();
        assert_eq!(outbox_before, outbox_after);
        assert!(
            store
                .conn
                .query_row(
                    "SELECT 1 FROM conversation_folders WHERE conversation_id=?",
                    [conversation.to_string()],
                    |_| Ok(())
                )
                .optional()
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn malformed_authenticated_folder_events_are_quarantined_as_invalid_payload() {
        let dir = tempfile::TempDir::new().unwrap();
        let vault_id = VaultId::new();
        let profile = KeyProfile::new(vault_id.0, 1).unwrap();
        let root = derive_root_key("correct horse battery staple", &profile).unwrap();
        let header = create_vault_check_header(&root, profile.clone()).unwrap();
        let event_key = derive_purpose_key(&root, &profile, KeyPurpose::Event).unwrap();
        let producer = DeviceId::new();
        let config = ClientConfig {
            database_path: dir.path().join("invalid-folder.db"),
            vault_id,
            device_id: DeviceId::new(),
        };
        let client = Client::open(config, DatabaseKey::new(&[10; 32]).unwrap()).unwrap();
        client
            .unlock(&profile, &header, "correct horse battery staple")
            .unwrap();

        let seal_plain = |plain: Vec<u8>, sequence| {
            let mut envelope = Envelope {
                protocol_version: PROTOCOL_VERSION,
                envelope_id: EnvelopeId::new(),
                command_id: None,
                vault_id,
                producer_device_id: producer,
                producer_sequence: SourceSequence(sequence),
                key_epoch: 1,
                crypto_suite: profile.crypto_suite,
                profile_fingerprint: profile.fingerprint().unwrap(),
                purpose: EnvelopePurpose::Event,
                route: None,
                compaction: None,
                origin_tag: None,
                ciphertext: Vec::new(),
            };
            envelope.ciphertext =
                seal_frame(&encrypt(&event_key, &envelope.aad_bytes().unwrap(), &plain).unwrap());
            envelope
        };
        let seal = |payload: PrivatePayload, sequence| {
            seal_plain(serde_json::to_vec(&payload).unwrap(), sequence)
        };
        let conversation = ConversationId::new();
        let folder_event = |folder, archived_through_ms, logical_revision, writer: String| {
            PrivatePayload::ConversationFolder {
                conversation_id: conversation,
                folder,
                archived_through_ms,
                logical_revision,
                writer_device_id: writer,
            }
        };
        let producer_writer = producer.to_string();
        let invalid = [
            folder_event(
                ConversationFolder::Inbox,
                None,
                1,
                DeviceId::new().to_string(),
            ),
            folder_event(ConversationFolder::Inbox, None, 0, producer_writer.clone()),
            folder_event(
                ConversationFolder::Inbox,
                None,
                i64::MAX as u64,
                producer_writer.clone(),
            ),
            folder_event(
                ConversationFolder::Archived,
                None,
                1,
                producer_writer.clone(),
            ),
            folder_event(
                ConversationFolder::Inbox,
                Some(1),
                1,
                producer_writer.clone(),
            ),
            folder_event(
                ConversationFolder::Spam,
                Some(1),
                1,
                producer_writer.clone(),
            ),
        ];
        for (index, payload) in invalid.into_iter().enumerate() {
            client
                .ingest(&seal(payload, index as u64 + 1), Cursor(index as u64 + 1))
                .unwrap();
        }
        let malformed_shape = serde_json::json!({
            "kind": "conversation_folder",
            "conversation_id": "not-a-uuid",
            "folder": "inbox",
            "archived_through_ms": null,
            "logical_revision": 1,
            "writer_device_id": producer.to_string(),
        });
        client
            .ingest(
                &seal_plain(serde_json::to_vec(&malformed_shape).unwrap(), 7),
                Cursor(7),
            )
            .unwrap();

        let report = client.apply_pending(20).unwrap();
        assert_eq!((report.applied, report.quarantined), (0, 7));
        assert!(
            client
                .quarantined()
                .unwrap()
                .iter()
                .all(|record| record.reason == QuarantineReason::InvalidPayload)
        );
        let store = client.lock().unwrap();
        let folders: i64 = store
            .conn
            .query_row("SELECT COUNT(*) FROM conversation_folders", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(folders, 0);
    }

    #[test]
    fn decoder_recovery_requeues_quarantined_folder_events_and_reapplies_them() {
        let dir = tempfile::TempDir::new().unwrap();
        let vault_id = VaultId::new();
        let profile = KeyProfile::new(vault_id.0, 1).unwrap();
        let root = derive_root_key("correct horse battery staple", &profile).unwrap();
        let header = create_vault_check_header(&root, profile.clone()).unwrap();
        let event_key = derive_purpose_key(&root, &profile, KeyPurpose::Event).unwrap();
        let producer = DeviceId::new();
        let config = ClientConfig {
            database_path: dir.path().join("folder-decoder-recovery.db"),
            vault_id,
            device_id: DeviceId::new(),
        };
        let client = Client::open(config.clone(), DatabaseKey::new(&[12; 32]).unwrap()).unwrap();
        client
            .unlock(&profile, &header, "correct horse battery staple")
            .unwrap();
        let conversation = client
            .capture_incoming(IncomingSms {
                conversation_id: None,
                sender_address: "+15555550100".into(),
                body: "folder recovery".into(),
                provider_message_id: Some("folder-recovery".into()),
                imported: false,
            })
            .unwrap()
            .conversation_id;

        let mut envelope = Envelope {
            protocol_version: PROTOCOL_VERSION,
            envelope_id: EnvelopeId::new(),
            command_id: None,
            vault_id,
            producer_device_id: producer,
            producer_sequence: SourceSequence(1),
            key_epoch: 1,
            crypto_suite: profile.crypto_suite,
            profile_fingerprint: profile.fingerprint().unwrap(),
            purpose: EnvelopePurpose::Event,
            route: None,
            compaction: None,
            origin_tag: None,
            ciphertext: Vec::new(),
        };
        let plain = serde_json::to_vec(&PrivatePayload::ConversationFolder {
            conversation_id: conversation,
            folder: ConversationFolder::Spam,
            archived_through_ms: None,
            logical_revision: 1,
            writer_device_id: producer.to_string(),
        })
        .unwrap();
        envelope.ciphertext =
            seal_frame(&encrypt(&event_key, &envelope.aad_bytes().unwrap(), &plain).unwrap());
        client.ingest(&envelope, Cursor(1)).unwrap();
        assert_eq!(client.apply_pending(10).unwrap().applied, 1);
        {
            // Simulate a client whose decoder consumed the folder event as invalid payload.
            let store = client.lock().unwrap();
            store
                .conn
                .execute(
                    "UPDATE journal SET status='quarantined',reason='invalid_payload' WHERE cursor=1",
                    [],
                )
                .unwrap();
            store
                .conn
                .execute("DELETE FROM conversation_folders", [])
                .unwrap();
            set_meta(&store.conn, "decoder_revision:1", "8").unwrap();
        }
        drop(client);

        let reopened = Client::open(config, DatabaseKey::new(&[12; 32]).unwrap()).unwrap();
        reopened
            .unlock(&profile, &header, "correct horse battery staple")
            .unwrap();
        reopened.apply_pending(10).unwrap();
        let store = reopened.lock().unwrap();
        let state: (String, i64) = store
            .conn
            .query_row(
                "SELECT folder,logical_revision FROM conversation_folders WHERE conversation_id=?",
                [conversation.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(state, ("spam".to_string(), 1));
        let status: String = store
            .conn
            .query_row("SELECT status FROM journal WHERE cursor=1", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(status, "applied");
        assert_eq!(
            get_meta(&store.conn, "decoder_revision:1")
                .unwrap()
                .as_deref(),
            Some(DECODER_REVISION)
        );
    }

    #[test]
    fn decoder_recovery_replays_folder_lww_across_epochs_and_relearns_compaction() {
        let dir = tempfile::TempDir::new().unwrap();
        let vault_id = VaultId::new();
        let p1 = KeyProfile::new(vault_id.0, 1).unwrap();
        let p2 = KeyProfile::new(vault_id.0, 2).unwrap();
        let root1 = derive_root_key("correct horse battery staple", &p1).unwrap();
        let root2 = derive_root_key("correct horse battery staple", &p2).unwrap();
        let h1 = create_vault_check_header(&root1, p1.clone()).unwrap();
        let h2 = create_vault_check_header(&root2, p2.clone()).unwrap();
        let source_cfg = ClientConfig {
            database_path: dir.path().join("folder-recovery-source.db"),
            vault_id,
            device_id: DeviceId::new(),
        };
        let target_cfg = ClientConfig {
            database_path: dir.path().join("folder-recovery-target.db"),
            vault_id,
            device_id: DeviceId::new(),
        };
        let source_device_id = source_cfg.device_id;
        let source = Client::open(source_cfg, DatabaseKey::new(&[15; 32]).unwrap()).unwrap();
        let target =
            Client::open(target_cfg.clone(), DatabaseKey::new(&[16; 32]).unwrap()).unwrap();
        for client in [&source, &target] {
            client
                .unlock(&p1, &h1, "correct horse battery staple")
                .unwrap();
            client
                .unlock(&p2, &h2, "correct horse battery staple")
                .unwrap();
            client.set_server_compaction_state(true, true).unwrap();
        }
        let conversation = source
            .capture_incoming(IncomingSms {
                conversation_id: None,
                sender_address: "+15555550100".into(),
                body: "folder recovery".into(),
                provider_message_id: Some("folder-lww-recovery".into()),
                imported: false,
            })
            .unwrap()
            .conversation_id;
        source
            .set_conversation_folder(conversation, ConversationFolder::Archived)
            .unwrap();
        source.activate_epoch(2).unwrap();
        source
            .set_conversation_folder(conversation, ConversationFolder::Inbox)
            .unwrap();
        let folders = source.pending_outbox().unwrap();
        assert_eq!(folders.len(), 3);
        for (cursor, envelope) in folders.iter().skip(1).enumerate() {
            target.ingest(envelope, Cursor(cursor as u64 + 1)).unwrap();
        }
        assert_eq!(target.apply_pending(10).unwrap().applied, 2);
        {
            let store = target.lock().unwrap();
            store
                .conn
                .execute("DELETE FROM conversation_folders", [])
                .unwrap();
            store
                .conn
                .execute("DELETE FROM compaction_frontier", [])
                .unwrap();
            store
                .conn
                .execute(
                    "DELETE FROM metadata WHERE k IN ('decoder_revision:1','decoder_revision:2')",
                    [],
                )
                .unwrap();
        }
        drop(target);

        let recovered = Client::open(target_cfg, DatabaseKey::new(&[16; 32]).unwrap()).unwrap();
        recovered
            .unlock(&p2, &h2, "correct horse battery staple")
            .unwrap();
        recovered
            .unlock(&p1, &h1, "correct horse battery staple")
            .unwrap();
        {
            let store = recovered.lock().unwrap();
            let state: (String, i64) = store
                .conn
                .query_row(
                    "SELECT folder,logical_revision FROM conversation_folders WHERE conversation_id=?",
                    [conversation.to_string()],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .unwrap();
            assert_eq!(state, ("inbox".to_string(), 2));
            let learned: i64 = store
                .conn
                .query_row(
                    "SELECT COUNT(*) FROM compaction_frontier WHERE identity=?",
                    [format!("conversation-folder:{conversation}")],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(
                learned, 1,
                "only the winning recovered folder event remains in the frontier"
            );
        }
        recovered
            .capture_incoming(IncomingSms {
                conversation_id: Some(conversation),
                sender_address: "+15555550100".into(),
                body: "local".into(),
                provider_message_id: Some("folder-lww-recovery-local".into()),
                imported: false,
            })
            .unwrap();
        recovered
            .set_conversation_folder(conversation, ConversationFolder::Spam)
            .unwrap();
        let next = recovered.pending_outbox().unwrap().pop().unwrap();
        assert!(next.compaction.unwrap().supersedes.iter().any(|reference| {
            reference.producer_device_id == source_device_id && reference.producer_sequence.0 == 3
        }));
        let store = recovered.lock().unwrap();
        let revision: i64 = store
            .conn
            .query_row(
                "SELECT logical_revision FROM conversation_folders WHERE conversation_id=?",
                [conversation.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(revision, 3);
    }

    #[test]
    fn projection_gc_removes_folder_rows_for_superseded_generations_only() {
        let dir = tempfile::TempDir::new().unwrap();
        let client = Client::open(
            ClientConfig {
                database_path: dir.path().join("projection-folder-gc.db"),
                vault_id: VaultId::new(),
                device_id: DeviceId::new(),
            },
            DatabaseKey::new(&[17; 32]).unwrap(),
        )
        .unwrap();
        let store = client.lock().unwrap();
        let conversation = ConversationId::new();
        let producer = DeviceId::new().to_string();
        let writer = DeviceId::new().to_string();
        // `promoted` rows are collected too (their data is live); only non-terminal generations keep rows.
        for (generation, state) in [(7_i64, "superseded"), (8, "draining")] {
            store
                .conn
                .execute(
                    "INSERT INTO projection_generations(generation,high_water,state) VALUES(?,0,?)",
                    params![generation, state],
                )
                .unwrap();
            store
                .conn
                .execute(
                    "INSERT INTO projection_conversation_folders(generation,conversation_id,folder,archived_through_ms,source_sequence,producer_device_id,logical_revision,writer_device_id) VALUES(?,?,'spam',NULL,1,?,1,?)",
                    params![generation, conversation.to_string(), producer, writer],
                )
                .unwrap();
        }
        snapshot_projection::collect_garbage(&store.conn).unwrap();
        let folder_rows = |generation: i64| -> i64 {
            store
                .conn
                .query_row(
                    "SELECT COUNT(*) FROM projection_conversation_folders WHERE generation=?",
                    [generation],
                    |row| row.get(0),
                )
                .unwrap()
        };
        assert_eq!(folder_rows(7), 0);
        assert_eq!(
            folder_rows(8),
            1,
            "a non-terminal generation keeps its folder rows"
        );
        let generations: i64 = store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM projection_generations WHERE generation IN (7,8)",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            generations, 1,
            "only the superseded generation row is collected"
        );
    }

    #[test]
    fn migration_43_adds_folder_table_with_check_constraint_from_schema_42() {
        let directory = tempfile::TempDir::new().unwrap();
        let config = ClientConfig {
            database_path: directory.path().join("folder-migration.db"),
            vault_id: VaultId::new(),
            device_id: DeviceId::new(),
        };
        let key = || DatabaseKey::new(&[18; 32]).unwrap();
        let client = Client::open(config.clone(), key()).unwrap();
        drop(client);
        let conn = Connection::open(&config.database_path).unwrap();
        apply_database_key(&conn, &key()).unwrap();
        conn.execute_batch(
            "DROP TABLE conversation_folders;
              DROP TABLE rcs_mapping_proofs;
              DROP TABLE rcs_conversation_mappings;
              DROP TABLE rcs_send_ledger;
              DROP TABLE rcs_link_status_projection;
              DROP TABLE rcs_link_eligibility_projection;
              DROP TABLE rcs_capability_published;
              DROP TABLE rcs_provider_detectors;
              DROP TABLE rcs_message_identities;
              DROP TABLE rcs_lane_ownership;
              ALTER TABLE attempts DROP COLUMN finalized_transport;
              UPDATE schema_meta SET version=42;",
        )
        .unwrap();
        drop(conn);

        let upgraded = Client::open(config.clone(), key()).unwrap();
        drop(upgraded);
        let conn = Connection::open(&config.database_path).unwrap();
        apply_database_key(&conn, &key()).unwrap();
        let version: i64 = conn
            .query_row("SELECT version FROM schema_meta", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
        let table: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='conversation_folders')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(table);
        let insert = |folder: &str| {
            conn.execute(
                "INSERT INTO conversation_folders(conversation_id,folder,archived_through_ms,logical_revision,writer_device_id,producer_device_id,source_sequence) VALUES(?,?,NULL,1,'writer','producer',1)",
                params![ConversationId::new().to_string(), folder],
            )
        };
        assert!(insert("other").is_err());
        assert!(insert("spam").is_ok());
    }
}
