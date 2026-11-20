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

pub(super) fn winner_sql(conversation: &str) -> String {
    format!(
        "SELECT f2.conversation_id FROM conversation_folders f2 WHERE COALESCE((SELECT target_id FROM conversation_merges WHERE source_id=f2.conversation_id),f2.conversation_id)={conversation} ORDER BY f2.logical_revision DESC,f2.writer_device_id DESC,f2.source_sequence DESC,f2.conversation_id DESC LIMIT 1"
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
        && (archived_through_ms.is_none() || folder == ConversationFolder::Archived)
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
    let changed = conn.execute("INSERT INTO conversation_folders(conversation_id,folder,archived_through_ms,logical_revision,writer_device_id,producer_device_id,source_sequence) VALUES(?,?,?,?,?,?,?) ON CONFLICT(conversation_id) DO UPDATE SET folder=excluded.folder,archived_through_ms=excluded.archived_through_ms,logical_revision=excluded.logical_revision,writer_device_id=excluded.writer_device_id,producer_device_id=excluded.producer_device_id,source_sequence=excluded.source_sequence WHERE excluded.logical_revision>conversation_folders.logical_revision OR (excluded.logical_revision=conversation_folders.logical_revision AND (excluded.writer_device_id>conversation_folders.writer_device_id OR (excluded.writer_device_id=conversation_folders.writer_device_id AND excluded.source_sequence>conversation_folders.source_sequence)))", params![conversation_id.to_string(), folder.as_str(), archived_through_ms, revision, writer, producer, sequence])?;
    if changed > 0 && folder == ConversationFolder::Spam {
        let survivor = conversation_merge::survivor(conn, conversation_id)?;
        conn.execute(&format!("DELETE FROM banner_candidates WHERE acknowledged=0 AND kind='message' AND COALESCE((SELECT target_id FROM conversation_merges WHERE source_id=banner_candidates.conversation_id),conversation_id)=?1 AND (SELECT folder FROM conversation_folders WHERE conversation_id=({}))='spam'", winner_sql("?1")), [survivor.to_string()])?;
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
        ensure_recovery_complete(&guard)?;
        let (conn, ctx) = guard.parts();
        if ctx
            .active_epoch
            .is_none_or(|epoch| !ctx.keys.contains_key(&epoch))
        {
            return Err(Error::KeysUnavailable);
        }
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let archived_through_ms: Option<i64> = tx.query_row(
            "SELECT MAX(sort_at_ms) FROM messages WHERE effective_conversation_id=?",
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
                "SELECT MAX(logical_revision) FROM conversation_folders WHERE COALESCE((SELECT target_id FROM conversation_merges WHERE source_id=conversation_folders.conversation_id),conversation_id)=?",
                [conversation_id.to_string()],
                |row| row.get(0),
            )?;
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
    fn only_archived_events_allow_a_watermark() {
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
        assert!(valid_event(
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
                &seal_plain(serde_json::to_vec(&malformed_shape).unwrap(), 6),
                Cursor(6),
            )
            .unwrap();

        let report = client.apply_pending(20).unwrap();
        assert_eq!((report.applied, report.quarantined), (0, 6));
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
    fn authenticated_archive_without_cutoff_is_accepted() {
        let dir = tempfile::TempDir::new().unwrap();
        let vault_id = VaultId::new();
        let profile = KeyProfile::new(vault_id.0, 1).unwrap();
        let root = derive_root_key("correct horse battery staple", &profile).unwrap();
        let header = create_vault_check_header(&root, profile.clone()).unwrap();
        let event_key = derive_purpose_key(&root, &profile, KeyPurpose::Event).unwrap();
        let producer = DeviceId::new();
        let client = Client::open(
            ClientConfig {
                database_path: dir.path().join("empty-archive.db"),
                vault_id,
                device_id: DeviceId::new(),
            },
            DatabaseKey::new(&[10; 32]).unwrap(),
        )
        .unwrap();
        client
            .unlock(&profile, &header, "correct horse battery staple")
            .unwrap();
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
        let conversation = ConversationId::new();
        let plain = serde_json::to_vec(&serde_json::json!({
            "kind": "conversation_folder",
            "conversation_id": conversation.to_string(),
            "folder": "archived",
            "logical_revision": 1,
            "writer_device_id": producer.to_string(),
        }))
        .unwrap();
        envelope.ciphertext =
            seal_frame(&encrypt(&event_key, &envelope.aad_bytes().unwrap(), &plain).unwrap());
        client.ingest(&envelope, Cursor(1)).unwrap();
        let report = client.apply_pending(10).unwrap();
        assert_eq!((report.applied, report.quarantined), (1, 0));
        let store = client.lock().unwrap();
        for latest in [None, Some(123_i64)] {
            let folder: String = store
                .conn
                .query_row(
                    &format!(
                        "SELECT {} FROM conversation_folders f WHERE f.conversation_id=?1",
                        effective_folder_sql("?2")
                    ),
                    params![conversation.to_string(), latest],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(folder, "inbox");
        }
        drop(store);
        client
            .capture_incoming(IncomingSms {
                conversation_id: Some(conversation),
                sender_address: "+15555550100".into(),
                body: "later".into(),
                provider_message_id: Some("later".into()),
                imported: false,
            })
            .unwrap();
        let store = client.lock().unwrap();
        let folder: String = store
            .conn
            .query_row(
                &format!(
                    "SELECT {} FROM conversation_folders f WHERE f.conversation_id=?1",
                    effective_folder_sql(
                        "(SELECT MAX(sort_at_ms) FROM messages WHERE effective_conversation_id=?1)"
                    )
                ),
                [conversation.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(folder, "inbox");
    }

    #[test]
    fn decoder_upgrade_failed_folder_projection_requests_authenticated_repair() {
        let dir = tempfile::TempDir::new().unwrap();
        let vault_id = VaultId::new();
        let profile = KeyProfile::new(vault_id.0, 1).unwrap();
        let root = derive_root_key("correct horse battery staple", &profile).unwrap();
        let header = create_vault_check_header(&root, profile.clone()).unwrap();
        let event_key = derive_purpose_key(&root, &profile, KeyPurpose::Event).unwrap();
        let producer = DeviceId::new();
        let client = Client::open(
            ClientConfig {
                database_path: dir.path().join("recovery.db"),
                vault_id,
                device_id: DeviceId::new(),
            },
            DatabaseKey::new(&[10; 32]).unwrap(),
        )
        .unwrap();
        client
            .unlock(&profile, &header, "correct horse battery staple")
            .unwrap();
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
        let plain = serde_json::to_vec(&serde_json::json!({
            "kind": "conversation_folder", "conversation_id": ConversationId::new().to_string(),
            "folder": "archived", "logical_revision": 1, "writer_device_id": producer.to_string(),
        }))
        .unwrap();
        envelope.ciphertext =
            seal_frame(&encrypt(&event_key, &envelope.aad_bytes().unwrap(), &plain).unwrap());
        client.ingest(&envelope, Cursor(1)).unwrap();
        {
            let mut store = client.lock().unwrap();
            let (conn, ctx) = store.parts();
            conn.execute(
                "UPDATE journal SET status='quarantined',reason='invalid_payload' WHERE cursor=1",
                [],
            )
            .unwrap();
            conn.execute("INSERT INTO projection_generations(generation,high_water,state,progress,reason) VALUES(99,1,'failed',0,'invalid_payload')", []).unwrap();
            conn.execute("DELETE FROM metadata WHERE k='contact_repair_required'", [])
                .unwrap();
            set_meta(conn, "decoder_revision:1", "14").unwrap();
            conn.execute_batch("CREATE TRIGGER reject_repair BEFORE INSERT ON metadata WHEN NEW.k='contact_repair_required' BEGIN SELECT RAISE(ABORT,'injected failure'); END;").unwrap();
            assert!(recover_decoder_revision(conn, &ctx, 1, &mut |_| {}).is_err());
            let unchanged: (String, Option<String>, bool) = conn
                .query_row(
                    "SELECT status,reason,historical FROM journal WHERE cursor=1",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .unwrap();
            assert_eq!(
                unchanged,
                ("quarantined".into(), Some("invalid_payload".into()), false)
            );
            assert!(get_meta(conn, "contact_repair_required").unwrap().is_none());
            conn.execute_batch("DROP TRIGGER reject_repair").unwrap();
            recover_decoder_revision(conn, &ctx, 1, &mut |_| {}).unwrap();
            let row: (String, Option<String>, bool) = conn
                .query_row(
                    "SELECT status,reason,historical FROM journal WHERE cursor=1",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .unwrap();
            assert_eq!(row, ("pending".into(), None, true));
            assert!(get_meta(conn, "contact_repair_required").unwrap().is_some());
            recover_decoder_revision(conn, &ctx, 1, &mut |_| {}).unwrap();
            assert!(get_meta(conn, "contact_repair_required").unwrap().is_some());
            for (state, reason, progress, high_water) in [
                ("promoted", "invalid_payload", 0, 1),
                ("staging", "invalid_payload", 0, 1),
                ("failed", "authentication_failed", 0, 1),
                ("failed", "invalid_payload", 1, 1),
                ("failed", "invalid_payload", 0, 0),
            ] {
                conn.execute(
                    "UPDATE projection_generations SET state=?,reason=?,progress=?,high_water=? WHERE generation=99",
                    params![state, reason, progress, high_water],
                )
                .unwrap();
                conn.execute("UPDATE journal SET status='quarantined',reason='invalid_payload',historical=0 WHERE cursor=1", []).unwrap();
                conn.execute("DELETE FROM metadata WHERE k='contact_repair_required'", [])
                    .unwrap();
                set_meta(conn, "decoder_revision:1", "14").unwrap();
                recover_decoder_revision(conn, &ctx, 1, &mut |_| {}).unwrap();
                let status: String = conn
                    .query_row("SELECT status FROM journal WHERE cursor=1", [], |r| {
                        r.get(0)
                    })
                    .unwrap();
                assert_eq!(status, "pending");
                assert!(get_meta(conn, "contact_repair_required").unwrap().is_none());
            }
            conn.execute("UPDATE projection_generations SET state='failed',reason='invalid_payload',progress=0,high_water=1 WHERE generation=99", []).unwrap();
            conn.execute(
                "UPDATE journal SET status='quarantined',reason='invalid_payload' WHERE cursor=1",
                [],
            )
            .unwrap();
            set_meta(conn, "contact_repair_required", "requested").unwrap();
            set_meta(conn, "decoder_revision:1", "14").unwrap();
            recover_decoder_revision(conn, &ctx, 1, &mut |_| {}).unwrap();
            assert_eq!(
                get_meta(conn, "contact_repair_required")
                    .unwrap()
                    .as_deref(),
                Some("requested")
            );
        }
    }

    #[test]
    fn recovered_folder_requires_fresh_snapshot_without_releasing_omitted_rcs() {
        let dir = tempfile::TempDir::new().unwrap();
        let vault_id = VaultId::new();
        let profile = KeyProfile::new(vault_id.0, 1).unwrap();
        let root = derive_root_key("correct horse battery staple", &profile).unwrap();
        let header = create_vault_check_header(&root, profile.clone()).unwrap();
        let key = derive_purpose_key(&root, &profile, KeyPurpose::Event).unwrap();
        let producer = DeviceId::new();
        let config = ClientConfig {
            database_path: dir.path().join("repaired-snapshot.db"),
            vault_id,
            device_id: DeviceId::new(),
        };
        let client = Client::open(config.clone(), DatabaseKey::new(&[10; 32]).unwrap()).unwrap();
        client
            .unlock(&profile, &header, "correct horse battery staple")
            .unwrap();
        let book = serde_json::json!({"id":"book1","owner_device_id":producer.to_string(),"generation":"1","state":"active"});
        let contact =
            serde_json::json!({"id":"c1","book_id":"book1","revision":"1","display_name":"Ada"});
        let conversation = ConversationId::new();
        let bodies = [
            serde_json::json!({"kind":"conversation_folder","conversation_id":conversation.to_string(),"folder":"archived","logical_revision":1,"writer_device_id":producer.to_string()}),
            serde_json::json!({"kind":"contact_upserted","book":book,"contact":contact}),
            serde_json::json!({"kind":"rcs_message_status","message_id":MessageId::new(),"status":"read","rcs_reported":true}),
        ];
        let mut envelopes = Vec::new();
        for (index, mut body) in bodies.into_iter().enumerate() {
            let mut envelope = Envelope {
                protocol_version: PROTOCOL_VERSION,
                envelope_id: EnvelopeId::new(),
                command_id: None,
                vault_id,
                producer_device_id: producer,
                producer_sequence: SourceSequence(index as u64 + 1),
                key_epoch: 1,
                crypto_suite: profile.crypto_suite,
                profile_fingerprint: profile.fingerprint().unwrap(),
                purpose: EnvelopePurpose::Event,
                route: None,
                compaction: Some(CompactionMetadata {
                    key: vec![index as u8 + 1; 32],
                    terminal: false,
                    supersedes: vec![],
                    checkpoint: false,
                }),
                origin_tag: None,
                ciphertext: Vec::new(),
            };
            body["compaction"] = serde_json::to_value(&envelope.compaction).unwrap();
            envelope.ciphertext = seal_frame(
                &encrypt(
                    &key,
                    &envelope.aad_bytes().unwrap(),
                    &serde_json::to_vec(&body).unwrap(),
                )
                .unwrap(),
            );
            client.ingest(&envelope, Cursor(index as u64 + 1)).unwrap();
            envelopes.push(envelope);
        }
        {
            let mut store = client.lock().unwrap();
            let (conn, ctx) = store.parts();
            conn.execute(
                "UPDATE journal SET status='quarantined',reason='invalid_payload' WHERE cursor=1",
                [],
            )
            .unwrap();
            conn.execute("UPDATE journal SET reason='snapshot_rcs_isolated',historical=1 WHERE cursor IN (2,3)", []).unwrap();
            conn.execute("INSERT INTO projection_generations(generation,high_water,state,progress,reason) VALUES(1,3,'failed',0,'invalid_payload')", []).unwrap();
            set_meta(conn, "snapshot_generation", "1").unwrap();
            set_meta(conn, "snapshot_import_origin:2", "1").unwrap();
            set_meta(conn, "snapshot_import_origin:3", "1").unwrap();
            set_meta(conn, "decoder_revision:1", "14").unwrap();
            conn.execute("DELETE FROM metadata WHERE k='contact_repair_required'", [])
                .unwrap();
            recover_decoder_revision(conn, &ctx, 1, &mut |_| {}).unwrap();
            for cursor in [2, 3] {
                let reason: String = conn
                    .query_row("SELECT reason FROM journal WHERE cursor=?", [cursor], |r| {
                        r.get(0)
                    })
                    .unwrap();
                assert_eq!(reason, "snapshot_rcs_isolated");
            }
            let count: i64 = conn
                .query_row("SELECT COUNT(*) FROM contacts", [], |r| r.get(0))
                .unwrap();
            assert_eq!(count, 0);
        }
        assert!(client.contact_repair_required().unwrap());
        drop(client);
        let client = Client::open(config, DatabaseKey::new(&[10; 32]).unwrap()).unwrap();
        client
            .unlock(&profile, &header, "correct horse battery staple")
            .unwrap();
        assert!(client.contact_repair_required().unwrap());
        client
            .capture_incoming(IncomingSms {
                conversation_id: Some(conversation),
                sender_address: "+15555550100".into(),
                body: "local".into(),
                provider_message_id: Some("recovery-lww".into()),
                imported: false,
            })
            .unwrap();
        {
            let store = client.lock().unwrap();
            set_meta(&store.conn, CLOCK, "1").unwrap();
        }
        client
            .set_conversation_folder(conversation, ConversationFolder::Inbox)
            .unwrap();
        client.apply_pending(20).unwrap();
        {
            let store = client.lock().unwrap();
            let status: String = store
                .conn
                .query_row("SELECT status FROM journal WHERE cursor=1", [], |row| {
                    row.get(0)
                })
                .unwrap();
            assert_eq!(status, "applied");
            let row: (String, i64) = store.conn.query_row(
                "SELECT folder,logical_revision FROM conversation_folders WHERE conversation_id=?",
                [conversation.to_string()], |row| Ok((row.get(0)?,row.get(1)?)),
            ).unwrap();
            assert_eq!(row, ("inbox".into(), 2));
        }
        let session = client
            .begin_snapshot_with_compaction(Cursor(3), 2, SnapshotPurpose::Resync, Some(2))
            .unwrap();
        client
            .append_snapshot_page(
                session.generation,
                &[
                    SnapshotRecord {
                        cursor: Cursor(1),
                        envelope: envelopes[0].clone(),
                    },
                    SnapshotRecord {
                        cursor: Cursor(2),
                        envelope: envelopes[1].clone(),
                    },
                ],
            )
            .unwrap();
        client.finish_snapshot(session.generation).unwrap();
        for _ in 0..5 {
            client.apply_pending(20).unwrap();
        }
        assert_eq!(
            client.snapshot_projection_status().unwrap().unwrap().state,
            SnapshotProjectionState::Promoted
        );
        assert!(!client.contact_repair_required().unwrap());
        let view: serde_json::Value = serde_json::from_str(
            &client
                .contact_book_view(
                    &serde_json::json!({"schema_version":1,"book_id":"book1","limit":20})
                        .to_string(),
                )
                .unwrap(),
        )
        .unwrap();
        assert_eq!(view["contacts"].as_array().unwrap().len(), 1);
        let store = client.lock().unwrap();
        let folder: (String, i64) = store
            .conn
            .query_row(
                "SELECT folder,logical_revision FROM conversation_folders WHERE conversation_id=?",
                [conversation.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(folder, ("inbox".into(), 2));
        let isolated: (String, String) = store
            .conn
            .query_row(
                "SELECT status,reason FROM journal WHERE cursor=3",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(isolated, ("pending".into(), "snapshot_rcs_isolated".into()));
    }

    #[test]
    fn history_block_reports_only_an_isolated_blocked_frontier() {
        let dir = tempfile::TempDir::new().unwrap();
        let vault_id = VaultId::new();
        let profile = KeyProfile::new(vault_id.0, 1).unwrap();
        let root = derive_root_key("correct horse battery staple", &profile).unwrap();
        let header = create_vault_check_header(&root, profile.clone()).unwrap();
        let client = Client::open(
            ClientConfig {
                database_path: dir.path().join("history.db"),
                vault_id,
                device_id: DeviceId::new(),
            },
            DatabaseKey::new(&[10; 32]).unwrap(),
        )
        .unwrap();
        client
            .unlock(&profile, &header, "correct horse battery staple")
            .unwrap();
        {
            let store = client.lock().unwrap();
            set_meta(&store.conn, "receive_cursor", "1").unwrap();
            set_meta(&store.conn, "processed_cursor", "0").unwrap();
            store.conn.execute("INSERT INTO journal(cursor,wire,status,reason,historical) VALUES(1,X'00','pending','snapshot_rcs_isolated',1)", []).unwrap();
        }
        assert_eq!(
            client.history_block().unwrap(),
            Some(HistoryBlock::Withheld)
        );
        {
            let store = client.lock().unwrap();
            store.conn.execute("INSERT INTO projection_generations(generation,high_water,state,progress,reason) VALUES(1,1,'failed',0,'invalid_payload')", []).unwrap();
        }
        assert_eq!(client.history_block().unwrap(), Some(HistoryBlock::Failed));
        client.request_contact_repair().unwrap();
        assert_eq!(
            client.history_block().unwrap(),
            Some(HistoryBlock::Repairing)
        );
        {
            let store = client.lock().unwrap();
            set_meta(&store.conn, "processed_cursor", "1").unwrap();
        }
        assert_eq!(client.history_block().unwrap(), None);
        {
            let store = client.lock().unwrap();
            set_meta(&store.conn, "processed_cursor", "0").unwrap();
            set_meta(&store.conn, "receive_cursor", "2").unwrap();
            store.conn.execute("INSERT INTO journal(cursor,wire,status,historical) VALUES(2,X'00','pending',1)", []).unwrap();
        }
        assert_eq!(client.history_block().unwrap(), None);
        {
            let store = client.lock().unwrap();
            store
                .conn
                .execute("DELETE FROM journal WHERE cursor=2", [])
                .unwrap();
            set_meta(&store.conn, "receive_cursor", "1").unwrap();
            store
                .conn
                .execute(
                    "UPDATE journal SET reason=NULL,key_epoch=1 WHERE cursor=1",
                    [],
                )
                .unwrap();
        }
        assert_eq!(client.history_block().unwrap(), None);
        {
            let store = client.lock().unwrap();
            store
                .conn
                .execute(
                    "UPDATE journal SET reason='snapshot_rcs_isolated' WHERE cursor=1",
                    [],
                )
                .unwrap();
            store
                .conn
                .execute(
                    "UPDATE projection_generations SET state='draining' WHERE generation=1",
                    [],
                )
                .unwrap();
        }
        assert_eq!(client.history_block().unwrap(), None);
        {
            let store = client.lock().unwrap();
            store
                .conn
                .execute(
                    "UPDATE journal SET reason='snapshot_rcs_isolated' WHERE cursor=1",
                    [],
                )
                .unwrap();
            store
                .conn
                .execute(
                    "UPDATE projection_generations SET state='staging' WHERE generation=1",
                    [],
                )
                .unwrap();
        }
        assert_eq!(client.history_block().unwrap(), None);
    }

    #[test]
    fn decoder_revision_declares_scan_scope() {
        assert!(decoder_revision_needs_applied(DECODER_REVISION.parse().unwrap()).is_some());
    }

    #[test]
    fn decoder_upgrade_scans_only_required_statuses() {
        for (marker, expected_total, applied_status) in [
            (Some("14"), 1, "applied"),
            (Some("13"), 1, "applied"),
            (Some("12"), 2, "quarantined"),
            (None, 2, "quarantined"),
            (Some("garbage"), 2, "quarantined"),
        ] {
            let dir = tempfile::TempDir::new().unwrap();
            let vault_id = VaultId::new();
            let profile = KeyProfile::new(vault_id.0, 1).unwrap();
            let root = derive_root_key("correct horse battery staple", &profile).unwrap();
            let header = create_vault_check_header(&root, profile.clone()).unwrap();
            let event_key = derive_purpose_key(&root, &profile, KeyPurpose::Event).unwrap();
            let producer = DeviceId::new();
            let client = Client::open(
                ClientConfig {
                    database_path: dir.path().join("scope.db"),
                    vault_id,
                    device_id: DeviceId::new(),
                },
                DatabaseKey::new(&[10; 32]).unwrap(),
            )
            .unwrap();
            client
                .unlock(&profile, &header, "correct horse battery staple")
                .unwrap();
            for sequence in 1..=2 {
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
                let plain = br#"{"kind":"conversation_folder","conversation_id":"bad"}"#;
                envelope.ciphertext = seal_frame(
                    &encrypt(&event_key, &envelope.aad_bytes().unwrap(), plain).unwrap(),
                );
                client.ingest(&envelope, Cursor(sequence)).unwrap();
            }
            let mut store = client.lock().unwrap();
            let (conn, ctx) = store.parts();
            conn.execute("UPDATE journal SET status='applied' WHERE cursor=1", [])
                .unwrap();
            conn.execute(
                "UPDATE journal SET status='quarantined',reason='invalid_payload' WHERE cursor=2",
                [],
            )
            .unwrap();
            if let Some(marker) = marker {
                set_meta(conn, "decoder_revision:1", marker).unwrap();
            } else {
                conn.execute("DELETE FROM metadata WHERE k='decoder_revision:1'", [])
                    .unwrap();
            }
            let mut progress = Vec::new();
            recover_decoder_revision(conn, &ctx, 1, &mut |value| progress.push(value)).unwrap();
            let status: String = conn
                .query_row("SELECT status FROM journal WHERE cursor=1", [], |r| {
                    r.get(0)
                })
                .unwrap();
            assert_eq!(status, applied_status, "marker {marker:?}");
            assert_eq!(
                progress.last().unwrap().total,
                expected_total,
                "marker {marker:?}"
            );
        }
    }

    #[test]
    fn decoder_upgrade_rejects_invalid_folder_recovery() {
        for case in [
            "vault",
            "authentication",
            "compaction",
            "writer",
            "revision",
            "cutoff",
            "sequence",
            "reference_overflow",
            "profile",
        ] {
            let dir = tempfile::TempDir::new().unwrap();
            let vault_id = VaultId::new();
            let profile = KeyProfile::new(vault_id.0, 1).unwrap();
            let root = derive_root_key("correct horse battery staple", &profile).unwrap();
            let header = create_vault_check_header(&root, profile.clone()).unwrap();
            let event_key = derive_purpose_key(&root, &profile, KeyPurpose::Event).unwrap();
            let producer = DeviceId::new();
            let client = Client::open(
                ClientConfig {
                    database_path: dir.path().join("negative.db"),
                    vault_id,
                    device_id: DeviceId::new(),
                },
                DatabaseKey::new(&[10; 32]).unwrap(),
            )
            .unwrap();
            client
                .unlock(&profile, &header, "correct horse battery staple")
                .unwrap();
            let mut envelope = Envelope {
                protocol_version: PROTOCOL_VERSION,
                envelope_id: EnvelopeId::new(),
                command_id: None,
                vault_id: if case == "vault" {
                    VaultId::new()
                } else {
                    vault_id
                },
                producer_device_id: producer,
                producer_sequence: SourceSequence(if case == "sequence" {
                    i64::MAX as u64 + 1
                } else {
                    1
                }),
                key_epoch: 1,
                crypto_suite: profile.crypto_suite,
                profile_fingerprint: if case == "profile" {
                    "0".repeat(64)
                } else {
                    profile.fingerprint().unwrap()
                },
                purpose: EnvelopePurpose::Event,
                route: None,
                compaction: None,
                origin_tag: None,
                ciphertext: Vec::new(),
            };
            let mut plain = serde_json::json!({
                "kind": "conversation_folder", "conversation_id": ConversationId::new().to_string(),
                "folder": "archived", "logical_revision": 1, "writer_device_id": producer.to_string(),
            });
            match case {
                "writer" => {
                    plain["writer_device_id"] = serde_json::json!(DeviceId::new().to_string())
                }
                "revision" => plain["logical_revision"] = serde_json::json!(0),
                "cutoff" => {
                    plain["folder"] = serde_json::json!("inbox");
                    plain["archived_through_ms"] = serde_json::json!(123);
                }
                _ => {}
            }
            if case == "reference_overflow" {
                envelope.compaction = Some(CompactionMetadata {
                    key: vec![1; 32],
                    terminal: false,
                    supersedes: vec![CompactionReference {
                        producer_device_id: DeviceId::new(),
                        producer_sequence: SourceSequence(i64::MAX as u64 + 1),
                    }],
                    checkpoint: false,
                });
                plain["compaction"] = serde_json::to_value(&envelope.compaction).unwrap();
            }
            envelope.ciphertext = seal_frame(
                &encrypt(
                    &event_key,
                    &envelope.aad_bytes().unwrap(),
                    &serde_json::to_vec(&plain).unwrap(),
                )
                .unwrap(),
            );
            if case == "authentication" {
                *envelope.ciphertext.last_mut().unwrap() ^= 1;
            }
            if case == "compaction" {
                envelope.compaction = Some(CompactionMetadata {
                    key: vec![1; 32],
                    terminal: false,
                    supersedes: vec![],
                    checkpoint: false,
                });
            }
            let wire = typed_wire(&envelope).unwrap();
            let mut store = client.lock().unwrap();
            let (conn, ctx) = store.parts();
            conn.execute("INSERT INTO journal(cursor,key_epoch,wire,status,reason,historical) VALUES(1,1,?,'quarantined','invalid_payload',0)", [wire]).unwrap();
            conn.execute("INSERT INTO projection_generations(generation,high_water,state,progress,reason) VALUES(99,1,'failed',0,'invalid_payload')", []).unwrap();
            conn.execute("DELETE FROM metadata WHERE k='contact_repair_required'", [])
                .unwrap();
            set_meta(conn, "decoder_revision:1", "14").unwrap();
            recover_decoder_revision(conn, &ctx, 1, &mut |_| {}).unwrap();
            let status: String = conn
                .query_row("SELECT status FROM journal WHERE cursor=1", [], |r| {
                    r.get(0)
                })
                .unwrap();
            assert_eq!(status, "quarantined", "{case}");
            assert!(
                get_meta(conn, "contact_repair_required").unwrap().is_none(),
                "{case}"
            );
        }
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
              DROP TABLE rcs_message_status;
              DROP TABLE rcs_lane_ownership;
              DROP TABLE rcs_conversation_metadata;
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
