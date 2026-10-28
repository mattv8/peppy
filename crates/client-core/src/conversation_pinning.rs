use super::*;

const CLOCK: &str = "conversation_pin_clock";

pub(super) fn valid_event(
    conversation_id: ConversationId,
    logical_revision: u64,
    writer: &str,
    producer: &str,
) -> bool {
    !conversation_id.0.is_nil()
        && uuid::Uuid::parse_str(writer).is_ok()
        && writer == producer
        && logical_revision != 0
        && logical_revision < i64::MAX as u64
}

pub(super) fn apply(
    conn: &Connection,
    conversation_id: ConversationId,
    pinned: bool,
    revision: u64,
    writer: &str,
    producer: &str,
    sequence: u64,
) -> Result<(), Error> {
    let revision =
        i64::try_from(revision).map_err(|_| Error::InvalidRequest("conversation pin revision"))?;
    let sequence =
        i64::try_from(sequence).map_err(|_| Error::InvalidRequest("conversation pin sequence"))?;
    let clock = get_meta(conn, CLOCK)?
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or(0);
    set_meta(conn, CLOCK, &clock.max(revision).to_string())?;
    conn.execute("INSERT INTO conversation_pins(conversation_id,pinned,logical_revision,writer_device_id,producer_device_id,source_sequence) VALUES(?,?,?,?,?,?) ON CONFLICT(conversation_id) DO UPDATE SET pinned=excluded.pinned,logical_revision=excluded.logical_revision,writer_device_id=excluded.writer_device_id,producer_device_id=excluded.producer_device_id,source_sequence=excluded.source_sequence WHERE excluded.logical_revision>conversation_pins.logical_revision OR (excluded.logical_revision=conversation_pins.logical_revision AND excluded.writer_device_id>conversation_pins.writer_device_id)", params![conversation_id.to_string(), pinned, revision, writer, producer, sequence])?;
    Ok(())
}

impl Client {
    pub fn set_conversation_pinned(
        &self,
        conversation_id: ConversationId,
        pinned: bool,
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
        let exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM messages WHERE conversation_id=?)",
            [conversation_id.to_string()],
            |row| row.get(0),
        )?;
        if !exists {
            return Err(Error::InvalidRequest("conversation pin"));
        }
        let clock = get_meta(&tx, CLOCK)?
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(0);
        let existing: Option<i64> = tx
            .query_row(
                "SELECT logical_revision FROM conversation_pins WHERE conversation_id=?",
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
            return Err(Error::InvalidRequest("conversation pin revision"));
        }
        let revision = base_revision + 1;
        set_meta(&tx, CLOCK, &revision.to_string())?;
        let (_, sequence) = enqueue(
            &tx,
            &ctx,
            EnvelopePurpose::Event,
            None,
            None,
            &PrivatePayload::ConversationPin {
                conversation_id,
                pinned,
                logical_revision: revision,
                writer_device_id: ctx.device_id.to_string(),
            },
        )?;
        apply(
            &tx,
            conversation_id,
            pinned,
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
    fn malformed_authenticated_pin_events_are_quarantined_as_invalid_payload() {
        let dir = tempfile::TempDir::new().unwrap();
        let vault_id = VaultId::new();
        let profile = KeyProfile::new(vault_id.0, 1).unwrap();
        let root = derive_root_key("correct horse battery staple", &profile).unwrap();
        let header = create_vault_check_header(&root, profile.clone()).unwrap();
        let event_key = derive_purpose_key(&root, &profile, KeyPurpose::Event).unwrap();
        let producer = DeviceId::new();
        let config = ClientConfig {
            database_path: dir.path().join("invalid-pin.db"),
            vault_id,
            device_id: DeviceId::new(),
        };
        let client = Client::open(config.clone(), DatabaseKey::new(&[9; 32]).unwrap()).unwrap();
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
        let invalid = [
            PrivatePayload::ConversationPin {
                conversation_id: conversation,
                pinned: true,
                logical_revision: 1,
                writer_device_id: DeviceId::new().to_string(),
            },
            PrivatePayload::ConversationPin {
                conversation_id: conversation,
                pinned: true,
                logical_revision: 0,
                writer_device_id: producer.to_string(),
            },
            PrivatePayload::ConversationPin {
                conversation_id: conversation,
                pinned: true,
                logical_revision: i64::MAX as u64,
                writer_device_id: producer.to_string(),
            },
        ];
        for (index, payload) in invalid.into_iter().enumerate() {
            client
                .ingest(&seal(payload, index as u64 + 1), Cursor(index as u64 + 1))
                .unwrap();
        }
        let malformed_shape = serde_json::json!({
            "kind": "conversation_pin",
            "conversation_id": "not-a-uuid",
            "pinned": true,
            "logical_revision": 1,
            "writer_device_id": producer.to_string(),
        });
        client
            .ingest(
                &seal_plain(serde_json::to_vec(&malformed_shape).unwrap(), 4),
                Cursor(4),
            )
            .unwrap();

        let report = client.apply_pending(10).unwrap();
        assert_eq!((report.applied, report.quarantined), (0, 4));
        assert!(
            client
                .quarantined()
                .unwrap()
                .iter()
                .all(|record| record.reason == QuarantineReason::InvalidPayload)
        );

        client
            .ingest(
                &seal_plain(serde_json::to_vec(&malformed_shape).unwrap(), 5),
                Cursor(5),
            )
            .unwrap();
        {
            let store = client.lock().unwrap();
            store
                .conn
                .execute(
                    "UPDATE journal SET status='applied',reason=NULL WHERE cursor=5",
                    [],
                )
                .unwrap();
            store
                .conn
                .execute("DELETE FROM metadata WHERE k='decoder_revision:1'", [])
                .unwrap();
        }
        drop(client);
        let replayed = Client::open(config, DatabaseKey::new(&[9; 32]).unwrap()).unwrap();
        replayed
            .unlock(&profile, &header, "correct horse battery staple")
            .unwrap();
        assert!(
            replayed
                .quarantined()
                .unwrap()
                .iter()
                .any(|record| record.cursor == Cursor(5)
                    && record.reason == QuarantineReason::InvalidPayload)
        );
    }

    #[test]
    fn pin_revision_ceiling_rejects_without_mutating_state_or_outbox() {
        let dir = tempfile::TempDir::new().unwrap();
        let vault_id = VaultId::new();
        let profile = KeyProfile::new(vault_id.0, 1).unwrap();
        let root = derive_root_key("correct horse battery staple", &profile).unwrap();
        let header = create_vault_check_header(&root, profile.clone()).unwrap();
        let client = Client::open(
            ClientConfig {
                database_path: dir.path().join("revision-ceiling.db"),
                vault_id,
                device_id: DeviceId::new(),
            },
            DatabaseKey::new(&[8; 32]).unwrap(),
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
                provider_message_id: Some("pin-ceiling".into()),
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
            client.set_conversation_pinned(conversation, true),
            Err(Error::InvalidRequest("conversation pin revision"))
        ));
        let store = client.lock().unwrap();
        let outbox_after: i64 = store
            .conn
            .query_row("SELECT COUNT(*) FROM outbox", [], |row| row.get(0))
            .unwrap();
        assert_eq!(outbox_after, outbox_before);
        assert!(
            store
                .conn
                .query_row(
                    "SELECT 1 FROM conversation_pins WHERE conversation_id=?",
                    [conversation.to_string()],
                    |_| Ok(())
                )
                .optional()
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn projection_gc_removes_pin_rows_for_superseded_generations() {
        let dir = tempfile::TempDir::new().unwrap();
        let client = Client::open(
            ClientConfig {
                database_path: dir.path().join("projection-gc.db"),
                vault_id: VaultId::new(),
                device_id: DeviceId::new(),
            },
            DatabaseKey::new(&[6; 32]).unwrap(),
        )
        .unwrap();
        let store = client.lock().unwrap();
        let conversation = ConversationId::new();
        store
            .conn
            .execute(
                "INSERT INTO projection_generations(generation,high_water,state) VALUES(7,0,'superseded')",
                [],
            )
            .unwrap();
        store
            .conn
            .execute(
                "INSERT INTO projection_conversation_pins(generation,conversation_id,pinned,source_sequence,producer_device_id,logical_revision,writer_device_id) VALUES(7,?,1,1,?,1,?)",
                params![conversation.to_string(), DeviceId::new().to_string(), DeviceId::new().to_string()],
            )
            .unwrap();
        snapshot_projection::collect_garbage(&store.conn).unwrap();
        let remaining: i64 = store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM projection_conversation_pins WHERE generation=7",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(remaining, 0);
    }

    #[test]
    fn decoder_recovery_replays_pin_lww_across_epochs_and_relearns_compaction() {
        let dir = tempfile::TempDir::new().unwrap();
        let vault_id = VaultId::new();
        let p1 = KeyProfile::new(vault_id.0, 1).unwrap();
        let p2 = KeyProfile::new(vault_id.0, 2).unwrap();
        let root1 = derive_root_key("correct horse battery staple", &p1).unwrap();
        let root2 = derive_root_key("correct horse battery staple", &p2).unwrap();
        let h1 = create_vault_check_header(&root1, p1.clone()).unwrap();
        let h2 = create_vault_check_header(&root2, p2.clone()).unwrap();
        let source_cfg = ClientConfig {
            database_path: dir.path().join("recovery-source.db"),
            vault_id,
            device_id: DeviceId::new(),
        };
        let target_cfg = ClientConfig {
            database_path: dir.path().join("recovery-target.db"),
            vault_id,
            device_id: DeviceId::new(),
        };
        let source_device_id = source_cfg.device_id;
        let source = Client::open(source_cfg, DatabaseKey::new(&[3; 32]).unwrap()).unwrap();
        let target = Client::open(target_cfg.clone(), DatabaseKey::new(&[4; 32]).unwrap()).unwrap();
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
                body: "recovery".into(),
                provider_message_id: Some("pin-recovery".into()),
                imported: false,
            })
            .unwrap()
            .conversation_id;
        source.set_conversation_pinned(conversation, true).unwrap();
        source.activate_epoch(2).unwrap();
        source.set_conversation_pinned(conversation, false).unwrap();
        let pins = source.pending_outbox().unwrap();
        assert_eq!(pins.len(), 3);
        for (cursor, envelope) in pins.iter().skip(1).enumerate() {
            target.ingest(envelope, Cursor(cursor as u64 + 1)).unwrap();
        }
        assert_eq!(target.apply_pending(10).unwrap().applied, 2);
        {
            let store = target.lock().unwrap();
            store
                .conn
                .execute("DELETE FROM conversation_pins", [])
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

        let recovered = Client::open(target_cfg, DatabaseKey::new(&[4; 32]).unwrap()).unwrap();
        recovered
            .unlock(&p2, &h2, "correct horse battery staple")
            .unwrap();
        recovered
            .unlock(&p1, &h1, "correct horse battery staple")
            .unwrap();
        {
            let store = recovered.lock().unwrap();
            let state: (bool, i64) = store
                .conn
                .query_row(
                    "SELECT pinned,logical_revision FROM conversation_pins WHERE conversation_id=?",
                    [conversation.to_string()],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .unwrap();
            assert_eq!(state, (false, 2));
            let learned: i64 = store
                .conn
                .query_row(
                    "SELECT COUNT(*) FROM compaction_frontier WHERE identity=?",
                    [format!("conversation-pin:{conversation}")],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(
                learned, 1,
                "only the winning recovered pin remains in the frontier"
            );
        }
        recovered
            .capture_incoming(IncomingSms {
                conversation_id: Some(conversation),
                sender_address: "+15555550100".into(),
                body: "local".into(),
                provider_message_id: Some("pin-recovery-local".into()),
                imported: false,
            })
            .unwrap();
        recovered
            .set_conversation_pinned(conversation, true)
            .unwrap();
        let next = recovered.pending_outbox().unwrap().pop().unwrap();
        assert!(next.compaction.unwrap().supersedes.iter().any(|reference| {
            reference.producer_device_id == source_device_id && reference.producer_sequence.0 == 3
        }));
        let store = recovered.lock().unwrap();
        let revision: i64 = store
            .conn
            .query_row(
                "SELECT logical_revision FROM conversation_pins WHERE conversation_id=?",
                [conversation.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(revision, 3);
    }
}
