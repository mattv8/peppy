use super::*;

const CLOCK: &str = "notification_preferences_clock";
const ADOPTED_REVISION: u64 = 1;
pub(super) const IDENTITY: &str = "notification-preferences";

pub(super) fn valid_event(revision: u64, writer: &str, producer: &str) -> bool {
    uuid::Uuid::parse_str(writer).is_ok()
        && writer == producer
        && revision != 0
        && revision < i64::MAX as u64
}

pub(super) fn preview_code(preview: NotificationPreview) -> &'static str {
    match preview {
        NotificationPreview::Full => "full",
        NotificationPreview::Hidden => "hidden",
    }
}

pub(super) fn preview_from_code(code: &str) -> Result<NotificationPreview, Error> {
    match code {
        "full" => Ok(NotificationPreview::Full),
        "hidden" => Ok(NotificationPreview::Hidden),
        _ => Err(Error::InvalidRequest("notification preferences preview")),
    }
}

pub(super) fn apply(
    conn: &Connection,
    preferences: &NotificationPreferences,
    revision: u64,
    writer: &str,
    producer: &str,
    sequence: u64,
) -> Result<(), Error> {
    let revision = i64::try_from(revision)
        .map_err(|_| Error::InvalidRequest("notification preferences revision"))?;
    let sequence = i64::try_from(sequence)
        .map_err(|_| Error::InvalidRequest("notification preferences sequence"))?;
    let clock = get_meta(conn, CLOCK)?
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or(0);
    set_meta(conn, CLOCK, &clock.max(revision).to_string())?;
    conn.execute("INSERT INTO notification_preferences(id,message_banners,mirrored_banners,preview,mute_with_heads,logical_revision,writer_device_id,producer_device_id,source_sequence) VALUES(1,?,?,?,?,?,?,?,?) ON CONFLICT(id) DO UPDATE SET message_banners=excluded.message_banners,mirrored_banners=excluded.mirrored_banners,preview=excluded.preview,mute_with_heads=excluded.mute_with_heads,logical_revision=excluded.logical_revision,writer_device_id=excluded.writer_device_id,producer_device_id=excluded.producer_device_id,source_sequence=excluded.source_sequence WHERE excluded.logical_revision>notification_preferences.logical_revision OR (excluded.logical_revision=notification_preferences.logical_revision AND excluded.writer_device_id>notification_preferences.writer_device_id)", params![preferences.message_banners, preferences.mirrored_banners, preview_code(preferences.preview), preferences.mute_with_heads, revision, writer, producer, sequence])?;
    Ok(())
}

fn read(conn: &Connection) -> Result<Option<(NotificationPreferences, u64)>, Error> {
    let row: Option<(bool, bool, String, bool, i64)> = conn.query_row("SELECT message_banners,mirrored_banners,preview,mute_with_heads,logical_revision FROM notification_preferences WHERE id=1", [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?))).optional()?;
    row.map(
        |(message_banners, mirrored_banners, preview, mute_with_heads, revision)| {
            Ok((
                NotificationPreferences {
                    message_banners,
                    mirrored_banners,
                    preview: preview_from_code(&preview)?,
                    mute_with_heads,
                },
                revision.max(0) as u64,
            ))
        },
    )
    .transpose()
}

fn publish(
    conn: &Connection,
    ctx: &Ctx<'_>,
    preferences: &NotificationPreferences,
    revision: u64,
) -> Result<(), Error> {
    let device_id = ctx.device_id.to_string();
    let (_, sequence) = enqueue(
        conn,
        ctx,
        EnvelopePurpose::Event,
        None,
        None,
        &PrivatePayload::NotificationPreferences {
            preferences: preferences.clone(),
            logical_revision: revision,
            writer_device_id: device_id.clone(),
        },
    )?;
    apply(
        conn,
        preferences,
        revision,
        &device_id,
        &device_id,
        sequence,
    )
}

impl Client {
    pub fn notification_preferences(&self) -> Result<Option<NotificationPreferences>, Error> {
        Ok(read(&self.lock()?.conn)?.map(|(preferences, _)| preferences))
    }

    pub fn set_notification_preferences(
        &self,
        preferences: NotificationPreferences,
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
        let current = read(&tx)?;
        if current
            .as_ref()
            .is_some_and(|(value, revision)| value == &preferences && *revision > ADOPTED_REVISION)
        {
            return Ok(());
        }
        let clock = get_meta(&tx, CLOCK)?
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(0);
        let base = clock
            .max(current.map_or(0, |(_, revision)| revision))
            .max(ADOPTED_REVISION);
        if base >= i64::MAX as u64 - 1 {
            return Err(Error::InvalidRequest("notification preferences revision"));
        }
        publish(&tx, &ctx, &preferences, base + 1)?;
        Ok(tx.commit()?)
    }

    pub fn adopt_legacy_notification_preferences(
        &self,
        preferences: NotificationPreferences,
    ) -> Result<bool, Error> {
        if preferences == NotificationPreferences::default() {
            return Ok(false);
        }
        let mut guard = self.lock()?;
        let (conn, ctx) = guard.parts();
        if ctx
            .active_epoch
            .is_none_or(|epoch| !ctx.keys.contains_key(&epoch))
        {
            return Ok(false);
        }
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let clock = get_meta(&tx, CLOCK)?
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(0);
        if read(&tx)?.is_some() || clock != 0 || frontier_len(&tx, Some(IDENTITY))? != 0 {
            return Ok(false);
        }
        publish(&tx, &ctx, &preferences, ADOPTED_REVISION)?;
        tx.commit()?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use peppy_crypto::{create_vault_check_header, derive_root_key};

    #[test]
    fn validation_rejects_forged_writer_and_invalid_revisions() {
        let device = DeviceId::new().to_string();
        assert!(valid_event(1, &device, &device));
        assert!(!valid_event(0, &device, &device));
        assert!(!valid_event(i64::MAX as u64, &device, &device));
        assert!(!valid_event(1, &device, &DeviceId::new().to_string()));
        assert!(!valid_event(1, "not-a-uuid", "not-a-uuid"));
        assert!(matches!(
            preview_from_code("unexpected"),
            Err(Error::InvalidRequest("notification preferences preview"))
        ));
    }

    #[test]
    fn adoption_does_not_reseed_an_observed_clock() {
        let dir = tempfile::TempDir::new().unwrap();
        let vault_id = VaultId::new();
        let profile = KeyProfile::new(vault_id.0, 1).unwrap();
        let root = derive_root_key("correct horse battery staple", &profile).unwrap();
        let header = create_vault_check_header(&root, profile.clone()).unwrap();
        let client = Client::open(
            ClientConfig {
                database_path: dir.path().join("clock.db"),
                vault_id,
                device_id: DeviceId::new(),
            },
            DatabaseKey::new(&[5; 32]).unwrap(),
        )
        .unwrap();
        client
            .unlock(&profile, &header, "correct horse battery staple")
            .unwrap();
        client
            .set_notification_preferences(NotificationPreferences {
                preview: NotificationPreview::Hidden,
                ..Default::default()
            })
            .unwrap();
        {
            let store = client.lock().unwrap();
            store
                .conn
                .execute("DELETE FROM notification_preferences", [])
                .unwrap();
        }
        assert!(
            !client
                .adopt_legacy_notification_preferences(NotificationPreferences {
                    message_banners: false,
                    ..Default::default()
                })
                .unwrap()
        );
    }

    #[test]
    fn decoder_recovery_replays_preferences_and_relearns_compaction() {
        let dir = tempfile::TempDir::new().unwrap();
        let vault_id = VaultId::new();
        let profile = KeyProfile::new(vault_id.0, 1).unwrap();
        let root = derive_root_key("correct horse battery staple", &profile).unwrap();
        let header = create_vault_check_header(&root, profile.clone()).unwrap();
        let source_config = ClientConfig {
            database_path: dir.path().join("source.db"),
            vault_id,
            device_id: DeviceId::new(),
        };
        let target_config = ClientConfig {
            database_path: dir.path().join("target.db"),
            vault_id,
            device_id: DeviceId::new(),
        };
        let source = Client::open(source_config, DatabaseKey::new(&[3; 32]).unwrap()).unwrap();
        let target =
            Client::open(target_config.clone(), DatabaseKey::new(&[4; 32]).unwrap()).unwrap();
        for client in [&source, &target] {
            client
                .unlock(&profile, &header, "correct horse battery staple")
                .unwrap();
            client.set_server_compaction_state(true, true).unwrap();
        }
        let preferences = NotificationPreferences {
            preview: NotificationPreview::Hidden,
            ..Default::default()
        };
        source
            .set_notification_preferences(preferences.clone())
            .unwrap();
        let event = source.pending_outbox().unwrap().pop().unwrap();
        target.ingest(&event, Cursor(1)).unwrap();
        target.apply_pending(10).unwrap();
        {
            let store = target.lock().unwrap();
            store
                .conn
                .execute("DELETE FROM notification_preferences", [])
                .unwrap();
            store
                .conn
                .execute("DELETE FROM compaction_frontier", [])
                .unwrap();
            store
                .conn
                .execute("DELETE FROM metadata WHERE k='decoder_revision:1'", [])
                .unwrap();
        }
        drop(target);
        let recovered = Client::open(target_config, DatabaseKey::new(&[4; 32]).unwrap()).unwrap();
        recovered
            .unlock(&profile, &header, "correct horse battery staple")
            .unwrap();
        assert_eq!(
            recovered.notification_preferences().unwrap(),
            Some(preferences)
        );
        let store = recovered.lock().unwrap();
        let learned: i64 = store.conn.query_row("SELECT COUNT(*) FROM compaction_frontier WHERE identity='notification-preferences'", [], |row| row.get(0)).unwrap();
        assert_eq!(learned, 1);
    }

    #[test]
    fn recovery_quarantines_tampered_outer_compaction_metadata() {
        let dir = tempfile::TempDir::new().unwrap();
        let vault_id = VaultId::new();
        let profile = KeyProfile::new(vault_id.0, 1).unwrap();
        let root = derive_root_key("correct horse battery staple", &profile).unwrap();
        let header = create_vault_check_header(&root, profile.clone()).unwrap();
        let source = Client::open(
            ClientConfig {
                database_path: dir.path().join("source.db"),
                vault_id,
                device_id: DeviceId::new(),
            },
            DatabaseKey::new(&[6; 32]).unwrap(),
        )
        .unwrap();
        let target_config = ClientConfig {
            database_path: dir.path().join("target.db"),
            vault_id,
            device_id: DeviceId::new(),
        };
        let target =
            Client::open(target_config.clone(), DatabaseKey::new(&[7; 32]).unwrap()).unwrap();
        for client in [&source, &target] {
            client
                .unlock(&profile, &header, "correct horse battery staple")
                .unwrap();
            client.set_server_compaction_state(true, true).unwrap();
        }
        source
            .set_notification_preferences(NotificationPreferences {
                preview: NotificationPreview::Hidden,
                ..Default::default()
            })
            .unwrap();
        let mut event = source.pending_outbox().unwrap().pop().unwrap();
        assert!(event.compaction.take().is_some());
        target.ingest(&event, Cursor(1)).unwrap();
        {
            let store = target.lock().unwrap();
            store
                .conn
                .execute(
                    "UPDATE journal SET status='applied',reason=NULL WHERE cursor=1",
                    [],
                )
                .unwrap();
            store
                .conn
                .execute("DELETE FROM metadata WHERE k='decoder_revision:1'", [])
                .unwrap();
        }
        drop(target);
        let recovered = Client::open(target_config, DatabaseKey::new(&[7; 32]).unwrap()).unwrap();
        recovered
            .unlock(&profile, &header, "correct horse battery staple")
            .unwrap();
        assert!(recovered.notification_preferences().unwrap().is_none());
        assert!(
            recovered
                .quarantined()
                .unwrap()
                .iter()
                .any(|record| record.cursor == Cursor(1)
                    && record.reason == QuarantineReason::AuthenticationFailed)
        );
        let store = recovered.lock().unwrap();
        let frontier: i64 = store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM compaction_frontier WHERE identity=?",
                [IDENTITY],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(frontier, 0);
    }

    #[test]
    fn recovery_then_interrupted_projection_promotion_keeps_preferences() {
        let dir = tempfile::TempDir::new().unwrap();
        let vault_id = VaultId::new();
        let profile = KeyProfile::new(vault_id.0, 1).unwrap();
        let root = derive_root_key("correct horse battery staple", &profile).unwrap();
        let header = create_vault_check_header(&root, profile.clone()).unwrap();
        let source = Client::open(
            ClientConfig {
                database_path: dir.path().join("source.db"),
                vault_id,
                device_id: DeviceId::new(),
            },
            DatabaseKey::new(&[8; 32]).unwrap(),
        )
        .unwrap();
        let target_config = ClientConfig {
            database_path: dir.path().join("target.db"),
            vault_id,
            device_id: DeviceId::new(),
        };
        let target =
            Client::open(target_config.clone(), DatabaseKey::new(&[9; 32]).unwrap()).unwrap();
        for client in [&source, &target] {
            client
                .unlock(&profile, &header, "correct horse battery staple")
                .unwrap();
            client.set_server_compaction_state(true, true).unwrap();
        }
        let preferences = NotificationPreferences {
            preview: NotificationPreview::Hidden,
            ..Default::default()
        };
        source
            .set_notification_preferences(preferences.clone())
            .unwrap();
        let event = source.pending_outbox().unwrap().pop().unwrap();
        target.ingest(&event, Cursor(1)).unwrap();
        target.apply_pending(10).unwrap();
        {
            let store = target.lock().unwrap();
            store
                .conn
                .execute("DELETE FROM notification_preferences", [])
                .unwrap();
            store.conn.execute("INSERT INTO projection_generations(generation,high_water,state,progress) VALUES(1,1,'staging',1)", []).unwrap();
            store
                .conn
                .execute("DELETE FROM metadata WHERE k='decoder_revision:1'", [])
                .unwrap();
        }
        drop(target);
        let recovered = Client::open(target_config, DatabaseKey::new(&[9; 32]).unwrap()).unwrap();
        recovered
            .unlock(&profile, &header, "correct horse battery staple")
            .unwrap();
        recovered.apply_pending(10).unwrap();
        assert_eq!(
            recovered.notification_preferences().unwrap(),
            Some(preferences)
        );
    }
}
