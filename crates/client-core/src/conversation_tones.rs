use super::*;

const CLOCK: &str = "conversation_tone_clock";
const MAX_PALETTE_INDEX: u8 = 119;

pub(super) fn valid_event(
    seed: &str,
    palette_index: Option<u8>,
    logical_revision: u64,
    writer: &str,
    producer: &str,
) -> bool {
    !seed.is_empty()
        && seed.len() <= 2_048
        && !seed.chars().any(char::is_control)
        && !seed.chars().next().is_some_and(char::is_whitespace)
        && !seed.chars().next_back().is_some_and(char::is_whitespace)
        && palette_index.is_none_or(|index| index <= MAX_PALETTE_INDEX)
        && uuid::Uuid::parse_str(writer).is_ok()
        && writer == producer
        && logical_revision != 0
        && logical_revision < i64::MAX as u64
}

pub(super) fn apply(
    conn: &Connection,
    seed: &str,
    palette_index: Option<u8>,
    revision: u64,
    writer: &str,
    producer: &str,
    sequence: u64,
) -> Result<(), Error> {
    let revision =
        i64::try_from(revision).map_err(|_| Error::InvalidRequest("conversation tone revision"))?;
    let sequence =
        i64::try_from(sequence).map_err(|_| Error::InvalidRequest("conversation tone sequence"))?;
    let clock = get_meta(conn, CLOCK)?
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or(0);
    set_meta(conn, CLOCK, &clock.max(revision).to_string())?;
    conn.execute("INSERT INTO conversation_tones(seed,palette_index,logical_revision,writer_device_id,producer_device_id,source_sequence) VALUES(?,?,?,?,?,?) ON CONFLICT(seed) DO UPDATE SET palette_index=excluded.palette_index,logical_revision=excluded.logical_revision,writer_device_id=excluded.writer_device_id,producer_device_id=excluded.producer_device_id,source_sequence=excluded.source_sequence WHERE excluded.logical_revision>conversation_tones.logical_revision OR (excluded.logical_revision=conversation_tones.logical_revision AND excluded.writer_device_id>conversation_tones.writer_device_id)", params![seed, palette_index, revision, writer, producer, sequence])?;
    Ok(())
}

impl Client {
    pub fn set_conversation_tone(
        &self,
        seed: &str,
        palette_index: Option<u8>,
    ) -> Result<(), Error> {
        let mut guard = self.lock()?;
        let (conn, ctx) = guard.parts();
        if ctx
            .active_epoch
            .is_none_or(|epoch| !ctx.keys.contains_key(&epoch))
        {
            return Err(Error::KeysUnavailable);
        }
        let device_id = ctx.device_id.to_string();
        if !valid_event(seed, palette_index, 1, &device_id, &device_id) {
            return Err(Error::InvalidRequest("conversation tone"));
        }
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let clock = get_meta(&tx, CLOCK)?
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(0);
        let existing: Option<i64> = tx
            .query_row(
                "SELECT logical_revision FROM conversation_tones WHERE seed=?",
                [seed],
                |row| row.get(0),
            )
            .optional()?;
        let base_revision = clock.max(
            existing
                .and_then(|value| u64::try_from(value).ok())
                .unwrap_or(0),
        );
        if base_revision >= i64::MAX as u64 - 1 {
            return Err(Error::InvalidRequest("conversation tone revision"));
        }
        let revision = base_revision + 1;
        set_meta(&tx, CLOCK, &revision.to_string())?;
        let (_, sequence) = enqueue(
            &tx,
            &ctx,
            EnvelopePurpose::Event,
            None,
            None,
            &PrivatePayload::ConversationTone {
                seed: seed.to_owned(),
                palette_index,
                logical_revision: revision,
                writer_device_id: device_id.clone(),
            },
        )?;
        apply(
            &tx,
            seed,
            palette_index,
            revision,
            &device_id,
            &device_id,
            sequence,
        )?;
        Ok(tx.commit()?)
    }

    pub fn conversation_tones(&self) -> Result<BTreeMap<String, u8>, Error> {
        let store = self.lock()?;
        let mut query = store.conn.prepare(
            "SELECT seed,palette_index FROM conversation_tones WHERE palette_index IS NOT NULL ORDER BY seed",
        )?;
        let rows = query
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, u8>(1)?))
            })?
            .collect::<Result<BTreeMap<_, _>, _>>()?;
        Ok(rows)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use peppy_crypto::{create_vault_check_header, derive_root_key};

    #[test]
    fn tone_override_is_visible_and_automatic_is_a_retained_tombstone() {
        let dir = tempfile::TempDir::new().unwrap();
        let vault_id = VaultId::new();
        let profile = KeyProfile::new(vault_id.0, 1).unwrap();
        let root = derive_root_key("correct horse battery staple", &profile).unwrap();
        let header = create_vault_check_header(&root, profile.clone()).unwrap();
        let client = Client::open(
            ClientConfig {
                database_path: dir.path().join("tones.db"),
                vault_id,
                device_id: DeviceId::new(),
            },
            DatabaseKey::new(&[1; 32]).unwrap(),
        )
        .unwrap();
        client
            .unlock(&profile, &header, "correct horse battery staple")
            .unwrap();

        client
            .set_conversation_tone("+15555550100", Some(17))
            .unwrap();
        assert_eq!(
            client.conversation_tones().unwrap().get("+15555550100"),
            Some(&17)
        );

        client.set_conversation_tone("+15555550100", None).unwrap();
        assert!(client.conversation_tones().unwrap().is_empty());
        let store = client.lock().unwrap();
        let tombstone: bool = store
            .conn
            .query_row(
                "SELECT palette_index IS NULL FROM conversation_tones WHERE seed=?",
                ["+15555550100"],
                |row| row.get(0),
            )
            .unwrap();
        assert!(tombstone);
    }

    #[test]
    fn validation_rejects_noncanonical_seeds_indices_and_writer_metadata() {
        let writer = DeviceId::new().to_string();
        assert!(valid_event(
            "group:alice bob",
            Some(119),
            1,
            &writer,
            &writer
        ));
        for seed in ["", " seed", "seed ", "seed\n"] {
            assert!(!valid_event(seed, Some(0), 1, &writer, &writer));
        }
        assert!(!valid_event(
            &"x".repeat(2_049),
            Some(0),
            1,
            &writer,
            &writer
        ));
        assert!(!valid_event("seed", Some(120), 1, &writer, &writer));
        assert!(!valid_event("seed", Some(0), 0, &writer, &writer));
        assert!(!valid_event(
            "seed",
            Some(0),
            i64::MAX as u64,
            &writer,
            &writer
        ));
        assert!(!valid_event("seed", Some(0), 1, "not-a-uuid", "not-a-uuid"));
        assert!(!valid_event(
            "seed",
            Some(0),
            1,
            &writer,
            "different-producer"
        ));
    }

    #[test]
    fn projection_gc_removes_tone_rows_for_superseded_generations() {
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
                "INSERT INTO projection_conversation_tones(generation,seed,palette_index,source_sequence,producer_device_id,logical_revision,writer_device_id) VALUES(7,?,17,1,?,1,?)",
                params!["+15555550100", DeviceId::new().to_string(), DeviceId::new().to_string()],
            )
            .unwrap();
        snapshot_projection::collect_garbage(&store.conn).unwrap();
        let remaining: i64 = store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM projection_conversation_tones WHERE generation=7",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(remaining, 0);
    }

    #[test]
    fn tone_revision_ceiling_rejects_without_mutating_state_or_outbox() {
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
        let store = client.lock().unwrap();
        set_meta(&store.conn, CLOCK, &(i64::MAX - 1).to_string()).unwrap();
        let outbox_before: i64 = store
            .conn
            .query_row("SELECT COUNT(*) FROM outbox", [], |row| row.get(0))
            .unwrap();
        drop(store);

        assert!(matches!(
            client.set_conversation_tone("+15555550109", Some(19)),
            Err(Error::InvalidRequest("conversation tone revision"))
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
                    "SELECT 1 FROM conversation_tones WHERE seed=?",
                    ["+15555550109"],
                    |_| Ok(())
                )
                .optional()
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn malformed_authenticated_tone_events_are_quarantined_live_and_by_decoder_recovery() {
        let dir = tempfile::TempDir::new().unwrap();
        let vault_id = VaultId::new();
        let profile = KeyProfile::new(vault_id.0, 1).unwrap();
        let root = derive_root_key("correct horse battery staple", &profile).unwrap();
        let header = create_vault_check_header(&root, profile.clone()).unwrap();
        let event_key = derive_purpose_key(&root, &profile, KeyPurpose::Event).unwrap();
        let producer = DeviceId::new();
        let config = ClientConfig {
            database_path: dir.path().join("invalid-tone.db"),
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
        let invalid = [
            PrivatePayload::ConversationTone {
                seed: " seed".into(),
                palette_index: Some(17),
                logical_revision: 1,
                writer_device_id: producer.to_string(),
            },
            PrivatePayload::ConversationTone {
                seed: "+15555550110".into(),
                palette_index: Some(120),
                logical_revision: 1,
                writer_device_id: producer.to_string(),
            },
            PrivatePayload::ConversationTone {
                seed: "+15555550111".into(),
                palette_index: Some(17),
                logical_revision: 0,
                writer_device_id: producer.to_string(),
            },
        ];
        for (index, payload) in invalid.into_iter().enumerate() {
            client
                .ingest(&seal(payload, index as u64 + 1), Cursor(index as u64 + 1))
                .unwrap();
        }
        let malformed_shape = serde_json::json!({
            "kind": "conversation_tone",
            "seed": "+15555550112",
            "palette_index": 17,
            "logical_revision": 1,
            "writer_device_id": "not-a-uuid",
        });
        client
            .ingest(
                &seal_plain(serde_json::to_vec(&malformed_shape).unwrap(), 4),
                Cursor(4),
            )
            .unwrap();
        let report = client.apply_pending(10).unwrap();
        assert_eq!((report.applied, report.quarantined), (0, 4));

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
        assert!(replayed.quarantined().unwrap().iter().any(|record| {
            record.cursor == Cursor(5) && record.reason == QuarantineReason::InvalidPayload
        }));
    }

    #[test]
    fn decoder_recovery_replays_tone_lww_across_epochs_and_relearns_compaction() {
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
        let seed = "+15555550113";
        source.set_conversation_tone(seed, Some(42)).unwrap();
        source.activate_epoch(2).unwrap();
        source.set_conversation_tone(seed, None).unwrap();
        let tones = source.pending_outbox().unwrap();
        assert_eq!(tones.len(), 2);
        for (cursor, envelope) in tones.iter().enumerate() {
            target.ingest(envelope, Cursor(cursor as u64 + 1)).unwrap();
        }
        assert_eq!(target.apply_pending(10).unwrap().applied, 2);
        {
            let store = target.lock().unwrap();
            store
                .conn
                .execute("DELETE FROM conversation_tones", [])
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
        assert!(recovered.conversation_tones().unwrap().is_empty());
        {
            let store = recovered.lock().unwrap();
            let state: (Option<u8>, i64) = store
                .conn
                .query_row(
                    "SELECT palette_index,logical_revision FROM conversation_tones WHERE seed=?",
                    [seed],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .unwrap();
            assert_eq!(state, (None, 2));
            let learned: i64 = store
                .conn
                .query_row(
                    "SELECT COUNT(*) FROM compaction_frontier WHERE identity=?",
                    [format!("conversation-tone:{seed}")],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(learned, 1);
        }
        recovered.set_conversation_tone(seed, Some(43)).unwrap();
        let next = recovered.pending_outbox().unwrap().pop().unwrap();
        assert!(next.compaction.unwrap().supersedes.iter().any(|reference| {
            reference.producer_device_id == source_device_id && reference.producer_sequence.0 == 2
        }));
    }

    #[test]
    fn compacted_tone_envelope_uses_an_opaque_hmac_key() {
        let dir = tempfile::TempDir::new().unwrap();
        let vault_id = VaultId::new();
        let profile = KeyProfile::new(vault_id.0, 1).unwrap();
        let root = derive_root_key("correct horse battery staple", &profile).unwrap();
        let header = create_vault_check_header(&root, profile.clone()).unwrap();
        let client = Client::open(
            ClientConfig {
                database_path: dir.path().join("opaque-compaction-key.db"),
                vault_id,
                device_id: DeviceId::new(),
            },
            DatabaseKey::new(&[7; 32]).unwrap(),
        )
        .unwrap();
        client
            .unlock(&profile, &header, "correct horse battery staple")
            .unwrap();
        client.set_server_compaction_state(true, true).unwrap();
        let seed = "group:alice,bob";
        client.set_conversation_tone(seed, Some(17)).unwrap();
        let envelope = client.pending_outbox().unwrap().pop().unwrap();
        let key = envelope.compaction.unwrap().key;
        assert_ne!(key, seed.as_bytes());
        assert!(
            !key.windows(seed.len())
                .any(|window| window == seed.as_bytes())
        );
    }
}
