//! Each source phone's dismissal availability. One row per phone holds its newest status, and
//! computers read the blocked rows as state. Publishing the status is a separate step.

use super::*;

/// The latest observation time accepted: 9999-12-31T23:59:59.999Z.
const MAX_OBSERVED_AT_MS: i64 = 253_402_300_799_999;

const NOTIFICATION_DISMISSAL_STATUS_MIGRATION_66: &str = "
CREATE TABLE IF NOT EXISTS notification_dismissal_statuses(
 device_id TEXT PRIMARY KEY, blocked_reason TEXT, observed_at_ms INTEGER NOT NULL,
 key_epoch INTEGER NOT NULL, producer_sequence INTEGER NOT NULL,
 dismissal_watermark INTEGER NOT NULL DEFAULT 0);
";

/// A phone that cannot dismiss mirrored notifications right now, as a computer shows it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NotificationDismissalBlock {
    pub source_device_id: String,
    pub reason: NotificationDismissalBlockReason,
    pub observed_at_ms: i64,
}

/// Schema 66: one dismissal-availability row per source phone. Idempotent for every upgrade path.
pub(crate) fn apply_notification_dismissal_status_migration_66(
    conn: &Connection,
) -> Result<(), Error> {
    conn.execute_batch(NOTIFICATION_DISMISSAL_STATUS_MIGRATION_66)?;
    Ok(())
}

/// A status is trusted only from the phone that wrote it, with a representable observation time.
pub(crate) fn valid_status(
    source_device_id: DeviceId,
    producer: DeviceId,
    observed_at_ms: i64,
) -> bool {
    source_device_id == producer && (0..=MAX_OBSERVED_AT_MS).contains(&observed_at_ms)
}

/// Stores a status only when it is newer than the stored row for its phone, by `key_epoch` and
/// then `producer_sequence`. The dismissal watermark is left alone, so only the publisher moves it.
pub(crate) fn apply_status(
    conn: &Connection,
    source_device_id: DeviceId,
    blocked: Option<NotificationDismissalBlockReason>,
    observed_at_ms: i64,
    key_epoch: u32,
    producer_sequence: u64,
) -> Result<(), Error> {
    conn.execute(
        "INSERT INTO notification_dismissal_statuses(device_id,blocked_reason,observed_at_ms,key_epoch,producer_sequence) \
         VALUES(?,?,?,?,?) ON CONFLICT(device_id) DO UPDATE SET \
         blocked_reason=excluded.blocked_reason,observed_at_ms=excluded.observed_at_ms, \
         key_epoch=excluded.key_epoch,producer_sequence=excluded.producer_sequence \
         WHERE excluded.key_epoch > notification_dismissal_statuses.key_epoch \
         OR (excluded.key_epoch = notification_dismissal_statuses.key_epoch \
         AND excluded.producer_sequence > notification_dismissal_statuses.producer_sequence)",
        params![
            source_device_id.to_string(),
            blocked.map(NotificationDismissalBlockReason::code),
            observed_at_ms,
            key_epoch,
            producer_sequence as i64
        ],
    )?;
    Ok(())
}

/// Phones that cannot dismiss right now, ordered by device. A stored reason this build does not
/// know reads as `Other`.
pub(crate) fn snapshot_blocks(conn: &Connection) -> Result<Vec<NotificationDismissalBlock>, Error> {
    let mut query = conn.prepare(
        "SELECT device_id,blocked_reason,observed_at_ms FROM notification_dismissal_statuses \
         WHERE blocked_reason IS NOT NULL ORDER BY device_id",
    )?;
    let blocks = query
        .query_map([], |row| {
            let code: String = row.get(1)?;
            Ok(NotificationDismissalBlock {
                source_device_id: row.get(0)?,
                reason: NotificationDismissalBlockReason::from_code(&code),
                observed_at_ms: row.get(2)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(blocks)
}

/// This phone's dismissals that are still waiting on it: live, dismissible, current instance.
/// Returns the count and the newest row id (0 when none).
const PENDING_OWN_DISMISSALS_SQL: &str = "SELECT COUNT(*),COALESCE(MAX(d.rowid),0) FROM notification_dismissals d JOIN notifications n ON n.source_device_id=d.source_device_id AND n.notification_key=d.notification_key AND n.lifetime=d.lifetime AND n.instance=d.instance WHERE d.completed=0 AND d.historical=0 AND d.source_device_id=? AND d.key_epoch=? AND n.removed=0 AND n.dismissible=1";

/// Decides whether this phone publishes now. `stored` is its own row as (reason, watermark) and
/// `status` is its current block code (`None` = it can dismiss).
fn should_publish(
    stored: Option<&(Option<String>, i64)>,
    status: Option<&str>,
    pending: i64,
    max_pending: i64,
) -> bool {
    let stored_reason = stored.and_then(|(reason, _)| reason.as_deref());
    let watermark = stored.map_or(0, |(_, watermark)| *watermark);
    let changed = stored.is_none() || stored_reason != status;
    // A changed status is announced once dismissals wait for it. A phone that was blocked and is
    // not any longer announces that even with nothing pending, so computers clear the block.
    (changed && pending > 0)
        || (status.is_none() && stored_reason.is_some())
        || (status.is_some() && !changed && max_pending > watermark)
}

impl Client {
    /// Publishes this phone's dismissal availability when a computer needs to know it. Android only:
    /// any other platform returns `Ok(false)` and writes nothing. Returns whether an event was queued.
    pub fn publish_notification_dismissal_status(
        &self,
        platform: GatewayPlatform,
        facts: GatewayHostFacts,
        now_ms: i64,
    ) -> Result<bool, Error> {
        if platform != GatewayPlatform::Android {
            return Ok(false);
        }
        let facts = GatewayHostFacts {
            notification_is_silent: false,
            ..facts
        };
        let mut store = self.lock()?;
        let (conn, ctx) = store.parts();
        // A reopened client keeps its active epoch but holds no keys until unlocked; sealing would
        // then store nothing, so refuse before any query or write.
        let epoch = ctx
            .active_epoch
            .filter(|epoch| ctx.keys.contains_key(epoch))
            .ok_or(Error::KeysUnavailable)?;
        let device = ctx.device_id.to_string();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let settings = crate::gateway_settings::load(&tx)?;
        let capabilities = Client::gateway_capabilities(platform, facts);
        let status =
            crate::gateway_settings::notification_policy_block(&settings, &capabilities, facts);
        let (pending, max_pending): (i64, i64) =
            tx.query_row(PENDING_OWN_DISMISSALS_SQL, params![device, epoch], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })?;
        let stored: Option<(Option<String>, i64)> = tx
            .query_row(
                "SELECT blocked_reason,dismissal_watermark FROM notification_dismissal_statuses WHERE device_id=?",
                [&device],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let code = status.map(NotificationDismissalBlockReason::code);
        if !should_publish(stored.as_ref(), code, pending, max_pending) {
            return Ok(false);
        }
        let payload = PrivatePayload::NotificationDismissalStatus {
            source_device_id: ctx.device_id,
            blocked: status,
            observed_at_ms: now_ms,
        };
        let (_, sequence) = enqueue(&tx, &ctx, EnvelopePurpose::Event, None, None, &payload)?;
        // The watermark is set here, and only here for this phone's own row: replays of its own
        // event carry the same sequence, so `apply_status` leaves the row alone.
        tx.execute(
            "INSERT INTO notification_dismissal_statuses(device_id,blocked_reason,observed_at_ms,key_epoch,producer_sequence,dismissal_watermark) \
             VALUES(?,?,?,?,?,?) ON CONFLICT(device_id) DO UPDATE SET \
             blocked_reason=excluded.blocked_reason,observed_at_ms=excluded.observed_at_ms, \
             key_epoch=excluded.key_epoch,producer_sequence=excluded.producer_sequence, \
             dismissal_watermark=excluded.dismissal_watermark",
            params![device, code, now_ms, epoch, sequence as i64, max_pending],
        )?;
        tx.commit()?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use peppy_crypto::{create_vault_check_header, derive_purpose_key, derive_root_key, encrypt};

    const PASSPHRASE: &str = "correct horse battery staple";
    const STORAGE_KEY: [u8; 32] = [37; 32];

    struct Fixture {
        _dir: tempfile::TempDir,
        config: ClientConfig,
        client: Client,
        profile: KeyProfile,
        header: VaultCheckHeader,
        event_key: PurposeKey,
    }

    impl Fixture {
        fn new() -> Self {
            let vault_id = VaultId::new();
            let profile = KeyProfile::new(vault_id.0, 1).unwrap();
            let dir = tempfile::TempDir::new().unwrap();
            let root = derive_root_key(PASSPHRASE, &profile).unwrap();
            let header = create_vault_check_header(&root, profile.clone()).unwrap();
            let config = ClientConfig {
                database_path: dir.path().join("dismissal-status.db"),
                vault_id,
                device_id: DeviceId::new(),
            };
            let client =
                Client::open(config.clone(), DatabaseKey::new(&STORAGE_KEY).unwrap()).unwrap();
            client.unlock(&profile, &header, PASSPHRASE).unwrap();
            let event_key = derive_purpose_key(&root, &profile, KeyPurpose::Event).unwrap();
            Self {
                _dir: dir,
                config,
                client,
                profile,
                header,
                event_key,
            }
        }

        fn envelope(
            &self,
            producer: DeviceId,
            purpose: EnvelopePurpose,
            sequence: u64,
            payload: &PrivatePayload,
        ) -> Envelope {
            let mut envelope = Envelope {
                protocol_version: PROTOCOL_VERSION,
                envelope_id: EnvelopeId::new(),
                command_id: (purpose == EnvelopePurpose::Command).then(CommandId::new),
                vault_id: self.config.vault_id,
                producer_device_id: producer,
                producer_sequence: SourceSequence(sequence),
                key_epoch: 1,
                crypto_suite: self.profile.crypto_suite,
                profile_fingerprint: self.profile.fingerprint().unwrap(),
                purpose,
                route: (purpose == EnvelopePurpose::Command).then(|| GatewayRoute::Carrier {
                    gateway_device_id: self.config.device_id,
                    subscription_id: "sim-1".into(),
                }),
                compaction: None,
                origin_tag: None,
                ciphertext: Vec::new(),
            };
            let plain = serde_json::to_vec(payload).unwrap();
            envelope.ciphertext = seal_frame(
                &encrypt(&self.event_key, &envelope.aad_bytes().unwrap(), &plain).unwrap(),
            );
            envelope
        }

        fn apply(&self, envelope: &Envelope, cursor: u64) -> ApplyReport {
            self.client.ingest(envelope, Cursor(cursor)).unwrap();
            self.client.apply_pending(100).unwrap()
        }
    }

    fn status(
        source: DeviceId,
        blocked: Option<NotificationDismissalBlockReason>,
        observed_at_ms: i64,
    ) -> PrivatePayload {
        PrivatePayload::NotificationDismissalStatus {
            source_device_id: source,
            blocked,
            observed_at_ms,
        }
    }

    /// (blocked_reason, observed_at_ms, key_epoch, producer_sequence) of one phone's row.
    fn stored(conn: &Connection, device: DeviceId) -> (Option<String>, i64, u32, i64) {
        conn.query_row(
            "SELECT blocked_reason,observed_at_ms,key_epoch,producer_sequence FROM notification_dismissal_statuses WHERE device_id=?",
            [device.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap()
    }

    #[test]
    fn notification_dismissal_status_applies_newest_by_epoch_then_sequence() {
        let fixture = Fixture::new();
        let phone = DeviceId::new();
        let store = fixture.client.lock().unwrap();
        let conn = &store.conn;

        apply_status(
            conn,
            phone,
            Some(NotificationDismissalBlockReason::MirroringOff),
            10,
            1,
            5,
        )
        .unwrap();
        apply_status(conn, phone, None, 20, 1, 4).unwrap();
        assert_eq!(
            stored(conn, phone),
            (Some("mirroring_off".into()), 10, 1, 5),
            "a stale sequence in the same epoch never regresses"
        );

        apply_status(conn, phone, None, 30, 1, 6).unwrap();
        assert_eq!(stored(conn, phone), (None, 30, 1, 6));

        apply_status(
            conn,
            phone,
            Some(NotificationDismissalBlockReason::WaitingForWifi),
            40,
            2,
            1,
        )
        .unwrap();
        assert_eq!(
            stored(conn, phone),
            (Some("waiting_for_wifi".into()), 40, 2, 1),
            "a later epoch wins even with a lower sequence"
        );

        apply_status(conn, phone, None, 50, 1, 99).unwrap();
        assert_eq!(
            stored(conn, phone),
            (Some("waiting_for_wifi".into()), 40, 2, 1),
            "an older epoch never wins"
        );
    }

    #[test]
    fn notification_dismissal_status_from_another_device_is_quarantined() {
        let fixture = Fixture::new();
        let producer = DeviceId::new();
        let other = DeviceId::new();
        let mut envelopes = vec![
            fixture.envelope(
                producer,
                EnvelopePurpose::Event,
                1,
                &status(
                    other,
                    Some(NotificationDismissalBlockReason::MirroringOff),
                    1,
                ),
            ),
            fixture.envelope(
                producer,
                EnvelopePurpose::Event,
                2,
                &status(producer, None, -1),
            ),
            fixture.envelope(
                producer,
                EnvelopePurpose::Event,
                3,
                &status(producer, None, MAX_OBSERVED_AT_MS + 1),
            ),
        ];
        envelopes.push(fixture.envelope(
            producer,
            EnvelopePurpose::Command,
            4,
            &status(producer, None, 1),
        ));

        for (index, envelope) in envelopes.iter().enumerate() {
            let cursor = index as u64 + 1;
            let report = fixture.apply(envelope, cursor);
            assert_eq!(report.quarantined, 1, "case {cursor} must be quarantined");
            assert_eq!(report.applied, 0, "case {cursor} must not apply");
        }
        let store = fixture.client.lock().unwrap();
        let rows: i64 = store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM notification_dismissal_statuses",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(rows, 0);
    }

    #[test]
    fn unknown_dismissal_block_reason_reads_as_other() {
        let fixture = Fixture::new();
        let phone = DeviceId::new();
        {
            let store = fixture.client.lock().unwrap();
            store
                .conn
                .execute(
                    "INSERT INTO notification_dismissal_statuses(device_id,blocked_reason,observed_at_ms,key_epoch,producer_sequence) VALUES(?,'from_a_future_build',7,1,1)",
                    [phone.to_string()],
                )
                .unwrap();
        }
        let blocks = fixture
            .client
            .notification_snapshot()
            .unwrap()
            .dismissal_blocks;
        assert_eq!(
            blocks,
            vec![NotificationDismissalBlock {
                source_device_id: phone.to_string(),
                reason: NotificationDismissalBlockReason::Other,
                observed_at_ms: 7,
            }]
        );
    }

    #[test]
    fn decoder_recovery_applies_dismissal_status_as_state() {
        let fixture = Fixture::new();
        let producer = DeviceId::new();
        let envelope = fixture.envelope(
            producer,
            EnvelopePurpose::Event,
            1,
            &status(
                producer,
                Some(NotificationDismissalBlockReason::WaitingForWifi),
                5,
            ),
        );
        assert_eq!(fixture.apply(&envelope, 1).applied, 1);
        let cache = fixture.client.export_native_key_cache(1).unwrap();
        {
            let store = fixture.client.lock().unwrap();
            store
                .conn
                .execute("DELETE FROM notification_dismissal_statuses", [])
                .unwrap();
            set_meta(&store.conn, "decoder_revision:1", "11").unwrap();
        }
        let config = fixture.config.clone();
        drop(fixture.client);

        let recovered = Client::open(config, DatabaseKey::new(&STORAGE_KEY).unwrap()).unwrap();
        recovered.import_native_key_cache(&cache).unwrap();
        let store = recovered.lock().unwrap();
        assert_eq!(
            stored(&store.conn, producer).0,
            Some("waiting_for_wifi".into())
        );
    }

    #[test]
    fn notification_snapshot_lists_only_blocked_phones() {
        let fixture = Fixture::new();
        let (blocked_first, available, blocked_second) =
            (DeviceId::new(), DeviceId::new(), DeviceId::new());
        {
            let store = fixture.client.lock().unwrap();
            apply_status(
                &store.conn,
                blocked_first,
                Some(NotificationDismissalBlockReason::MirroringOff),
                10,
                1,
                1,
            )
            .unwrap();
            apply_status(&store.conn, available, None, 11, 1, 1).unwrap();
            apply_status(
                &store.conn,
                blocked_second,
                Some(NotificationDismissalBlockReason::NotificationAccessOff),
                12,
                1,
                1,
            )
            .unwrap();
        }
        let mut expected = vec![
            (
                blocked_first.to_string(),
                NotificationDismissalBlockReason::MirroringOff,
                10,
            ),
            (
                blocked_second.to_string(),
                NotificationDismissalBlockReason::NotificationAccessOff,
                12,
            ),
        ];
        expected.sort_by(|left, right| left.0.cmp(&right.0));

        let blocks = fixture
            .client
            .notification_snapshot()
            .unwrap()
            .dismissal_blocks;
        assert_eq!(
            blocks
                .into_iter()
                .map(|block| (block.source_device_id, block.reason, block.observed_at_ms))
                .collect::<Vec<_>>(),
            expected
        );
    }

    fn android_facts() -> GatewayHostFacts {
        GatewayHostFacts {
            wifi_connected: true,
            notification_listener_available: true,
            ..GatewayHostFacts::default()
        }
    }

    fn set_mirroring(fixture: &Fixture, mirroring_enabled: bool) {
        fixture
            .client
            .set_gateway_settings(GatewaySettings {
                mirroring_enabled,
                ..GatewaySettings::default()
            })
            .unwrap();
    }

    fn publish(fixture: &Fixture, facts: GatewayHostFacts) -> bool {
        fixture
            .client
            .publish_notification_dismissal_status(GatewayPlatform::Android, facts, 1_000)
            .unwrap()
    }

    /// A dismissal waiting on this phone, shaped as the apply path stores it: a live, dismissible
    /// row of its own and an uncompleted dismissal for it.
    fn dismiss_own(fixture: &Fixture, key: &str) {
        let device = fixture.config.device_id.to_string();
        let store = fixture.client.lock().unwrap();
        store
            .conn
            .execute(
                "INSERT INTO notifications(source_device_id,notification_key,lifetime,instance,package_name,app_name,title,text,posted_at,dismissible,seen,removed,source_sequence) VALUES(?,?,'life','inst','com.example','Example','t','x',1,1,0,0,1)",
                params![device, key],
            )
            .unwrap();
        store
            .conn
            .execute(
                "INSERT INTO notification_dismissals(id,source_device_id,notification_key,lifetime,instance,historical,completed,key_epoch) VALUES(?,?,?,'life','inst',0,0,1)",
                params![uuid::Uuid::new_v4().to_string(), device, key],
            )
            .unwrap();
    }

    fn complete_own_dismissals(fixture: &Fixture) {
        fixture
            .client
            .lock()
            .unwrap()
            .conn
            .execute("UPDATE notification_dismissals SET completed=1", [])
            .unwrap();
    }

    fn max_dismissal_rowid(fixture: &Fixture) -> i64 {
        fixture
            .client
            .lock()
            .unwrap()
            .conn
            .query_row(
                "SELECT COALESCE(MAX(rowid),0) FROM notification_dismissals",
                [],
                |row| row.get(0),
            )
            .unwrap()
    }

    /// This phone's own row as (blocked_reason, dismissal_watermark).
    fn own_row(fixture: &Fixture) -> Option<(Option<String>, i64)> {
        fixture
            .client
            .lock()
            .unwrap()
            .conn
            .query_row(
                "SELECT blocked_reason,dismissal_watermark FROM notification_dismissal_statuses WHERE device_id=?",
                [fixture.config.device_id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .unwrap()
    }

    fn outbox(fixture: &Fixture) -> usize {
        fixture.client.pending_outbox_count().unwrap()
    }

    #[test]
    fn blocked_phone_without_pending_dismissals_publishes_nothing() {
        let fixture = Fixture::new();
        set_mirroring(&fixture, false);

        assert!(!publish(&fixture, android_facts()));
        assert_eq!(outbox(&fixture), 0);
        assert_eq!(own_row(&fixture), None);
    }

    #[test]
    fn blocked_phone_publishes_once_when_a_dismissal_arrives() {
        let fixture = Fixture::new();
        set_mirroring(&fixture, false);
        dismiss_own(&fixture, "k1");

        assert!(publish(&fixture, android_facts()));
        assert_eq!(
            own_row(&fixture),
            Some((Some("mirroring_off".into()), max_dismissal_rowid(&fixture)))
        );
        assert_eq!(outbox(&fixture), 1);
    }

    #[test]
    fn unchanged_block_is_not_republished_without_new_dismissals() {
        let fixture = Fixture::new();
        set_mirroring(&fixture, false);
        dismiss_own(&fixture, "k1");
        assert!(publish(&fixture, android_facts()));

        assert!(!publish(&fixture, android_facts()));
        assert!(!publish(&fixture, android_facts()));
        assert_eq!(outbox(&fixture), 1);
    }

    #[test]
    fn blocked_phone_republishes_when_new_dismissals_arrive() {
        let fixture = Fixture::new();
        set_mirroring(&fixture, false);
        dismiss_own(&fixture, "k1");
        assert!(publish(&fixture, android_facts()));

        dismiss_own(&fixture, "k2");
        assert!(publish(&fixture, android_facts()));
        assert_eq!(outbox(&fixture), 2);
        assert_eq!(
            own_row(&fixture).map(|(_, watermark)| watermark),
            Some(max_dismissal_rowid(&fixture))
        );
    }

    #[test]
    fn recovered_phone_publishes_available_without_pending_dismissals() {
        let fixture = Fixture::new();
        set_mirroring(&fixture, false);
        dismiss_own(&fixture, "k1");
        assert!(publish(&fixture, android_facts()));
        complete_own_dismissals(&fixture);

        set_mirroring(&fixture, true);
        assert!(publish(&fixture, android_facts()));
        assert_eq!(own_row(&fixture).map(|(reason, _)| reason), Some(None));
        assert!(!publish(&fixture, android_facts()));
    }

    #[test]
    fn available_phone_without_prior_row_publishes_nothing() {
        let fixture = Fixture::new();
        set_mirroring(&fixture, true);

        assert!(!publish(&fixture, android_facts()));
        assert_eq!(outbox(&fixture), 0);
        assert_eq!(own_row(&fixture), None);
    }

    #[test]
    fn non_android_platform_never_publishes() {
        let fixture = Fixture::new();
        set_mirroring(&fixture, false);
        dismiss_own(&fixture, "k1");

        assert!(
            !fixture
                .client
                .publish_notification_dismissal_status(GatewayPlatform::Ios, android_facts(), 1_000)
                .unwrap()
        );
        assert_eq!(outbox(&fixture), 0);
        assert_eq!(own_row(&fixture), None);
    }

    #[test]
    fn own_replayed_status_keeps_the_watermark() {
        let fixture = Fixture::new();
        set_mirroring(&fixture, false);
        dismiss_own(&fixture, "k1");
        assert!(publish(&fixture, android_facts()));
        let row = own_row(&fixture);
        let envelope = fixture.client.pending_outbox().unwrap().remove(0);

        let report = fixture.apply(&envelope, 1);

        assert_eq!(report.quarantined, 0);
        assert_eq!(
            own_row(&fixture),
            row,
            "the replay changes neither reason nor watermark"
        );
        assert!(!publish(&fixture, android_facts()));
    }

    /// Reopens the database with no keys. `Client::open` shares a live store per path, so the old
    /// unlocked client must be dropped first.
    fn reopen_locked(fixture: Fixture) -> Fixture {
        let Fixture {
            _dir,
            config,
            client,
            profile,
            header,
            event_key,
        } = fixture;
        drop(client);
        let client = Client::open(config.clone(), DatabaseKey::new(&STORAGE_KEY).unwrap()).unwrap();
        Fixture {
            _dir,
            config,
            client,
            profile,
            header,
            event_key,
        }
    }

    #[test]
    fn locked_reopened_phone_with_pending_work_is_keys_unavailable() {
        let fixture = Fixture::new();
        set_mirroring(&fixture, false);
        dismiss_own(&fixture, "k1");
        let fixture = reopen_locked(fixture);

        assert_eq!(
            fixture.client.publish_notification_dismissal_status(
                GatewayPlatform::Android,
                android_facts(),
                1_000
            ),
            Err(Error::KeysUnavailable)
        );
        assert_eq!(own_row(&fixture), None);
        fixture
            .client
            .unlock(&fixture.profile, &fixture.header, PASSPHRASE)
            .unwrap();
        assert_eq!(outbox(&fixture), 0, "no plaintext event was queued");
    }

    #[test]
    fn locked_reopened_phone_without_pending_work_is_keys_unavailable() {
        let fixture = Fixture::new();
        set_mirroring(&fixture, false);
        dismiss_own(&fixture, "k1");
        assert!(publish(&fixture, android_facts()));
        complete_own_dismissals(&fixture);
        set_mirroring(&fixture, true);
        let row = own_row(&fixture);
        let fixture = reopen_locked(fixture);

        // Available now and blocked before: publishing would change the row, so it must refuse.
        assert_eq!(
            fixture.client.publish_notification_dismissal_status(
                GatewayPlatform::Android,
                android_facts(),
                2_000
            ),
            Err(Error::KeysUnavailable)
        );
        assert_eq!(own_row(&fixture), row, "the status row is unchanged");
        fixture
            .client
            .unlock(&fixture.profile, &fixture.header, PASSPHRASE)
            .unwrap();
        assert_eq!(outbox(&fixture), 1, "only the earlier publish was queued");
    }
}
