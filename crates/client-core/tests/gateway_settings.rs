use crate::common::*;

fn roll_back_migrations_after_version_25(conn: &rusqlite::Connection) {
    conn.execute_batch(
        "DROP TRIGGER IF EXISTS messages_search_insert;
         DROP TRIGGER IF EXISTS messages_search_update;
         DROP TRIGGER IF EXISTS messages_search_delete;
         DROP TRIGGER IF EXISTS contacts_search_insert;
         DROP TRIGGER IF EXISTS contacts_search_update;
         DROP TRIGGER IF EXISTS contacts_search_delete;
         DROP TRIGGER IF EXISTS contact_books_search_insert;
         DROP TRIGGER IF EXISTS contact_books_search_update;
         DROP TRIGGER IF EXISTS contact_books_search_delete;
         DROP TRIGGER IF EXISTS contact_address_search_insert;
         DROP TRIGGER IF EXISTS contact_address_search_update;
         DROP TRIGGER IF EXISTS contact_address_search_delete;
         DROP TRIGGER IF EXISTS addresses_search_insert;
         DROP TRIGGER IF EXISTS addresses_search_update;
         DROP TRIGGER IF EXISTS addresses_search_delete;
         DROP INDEX IF EXISTS messages_search_metadata;
         DROP INDEX IF EXISTS messages_latest_mms;
         DROP INDEX IF EXISTS messages_latest_mms_context;
         DROP TABLE IF EXISTS local_search_fts;
         DELETE FROM metadata WHERE k IN ('search_remaining','search_generation','search_backfill_highwater','search_backfill_watermark');
         DROP TABLE mms_backfill_remote_capacity;
         CREATE TABLE mms_provider_identity_conflicts(
           source_generation TEXT NOT NULL,
           subscription_id TEXT NOT NULL,
           provider_message_id TEXT NOT NULL,
           direction TEXT NOT NULL,
           reason TEXT NOT NULL,
           PRIMARY KEY(source_generation,subscription_id,provider_message_id,direction)
         );
         ALTER TABLE mms_acquisition_parts DROP COLUMN metadata_recorded;
         ALTER TABLE mms_acquisition_parts DROP COLUMN preview_state;
         DROP TABLE media_cache_entries;
         DROP TABLE mms_provider_identity_quarantines;
         ALTER TABLE mms_acquisitions RENAME TO mms_acquisitions_new;
         CREATE TABLE mms_acquisitions(
           acquisition_id TEXT PRIMARY KEY,
           source_generation TEXT NOT NULL,
           subscription_id TEXT NOT NULL,
           provider_message_id TEXT NOT NULL,
           direction TEXT NOT NULL,
           conversation_id TEXT NOT NULL,
           input TEXT NOT NULL,
           state TEXT NOT NULL,
           reason TEXT,
           message_id TEXT NOT NULL,
           source_sequence INTEGER NOT NULL,
           local_order INTEGER NOT NULL UNIQUE,
           UNIQUE(source_generation,subscription_id,provider_message_id,direction)
         );
         INSERT INTO mms_acquisitions(
           acquisition_id,source_generation,subscription_id,provider_message_id,direction,
           conversation_id,input,state,reason,message_id,source_sequence,local_order
         )
         SELECT acquisition_id,source_generation,subscription_id,provider_message_id,direction,
                conversation_id,input,state,reason,message_id,source_sequence,local_order
         FROM mms_acquisitions_new;
         DROP TABLE mms_acquisitions_new;
         CREATE INDEX mms_acquisitions_pending ON mms_acquisitions(state) WHERE state!='complete';
         CREATE INDEX mms_acquisitions_reserved_sequence
           ON mms_acquisitions(conversation_id,source_sequence) WHERE state!='complete';
         ALTER TABLE mms_acquisition_parts DROP COLUMN thumbnail_attachment_id;
         ALTER TABLE mms_acquisition_parts DROP COLUMN duration_ms;
         ALTER TABLE mms_acquisition_parts DROP COLUMN height;
         ALTER TABLE mms_acquisition_parts DROP COLUMN width;
         DROP TABLE attachment_demand;
         DROP TABLE message_media_meta;
          ALTER TABLE send_status DROP COLUMN status_at_ms;",
    )
    .unwrap();
    if column_exists(conn, "attempts", "finalized_transport") {
        conn.execute_batch("ALTER TABLE attempts DROP COLUMN finalized_transport;")
            .unwrap();
    }
}

const NOTIFICATION_REPLY_SPIKE_DDL: &str = "
    CREATE TABLE notification_reply_submissions(
      command_id TEXT PRIMARY KEY,
      message_id TEXT NOT NULL,
      conversation_id TEXT NOT NULL,
      normalized_body TEXT NOT NULL,
      submitted_at_ms INTEGER NOT NULL,
      resolved INTEGER NOT NULL DEFAULT 0
    );
    CREATE INDEX notification_reply_submissions_match
      ON notification_reply_submissions(resolved,conversation_id,normalized_body,submitted_at_ms);
    ALTER TABLE gateway_settings ADD COLUMN reply_via_google_messages_enabled INTEGER NOT NULL DEFAULT 0;
";

fn encrypted_connection(config: &ClientConfig) -> rusqlite::Connection {
    let conn = rusqlite::Connection::open(&config.database_path).unwrap();
    conn.execute_batch(
        "PRAGMA key = \"x'0707070707070707070707070707070707070707070707070707070707070707'\";",
    )
    .unwrap();
    conn
}

fn table_exists(conn: &rusqlite::Connection, table: &str) -> bool {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?)",
        [table],
        |row| row.get(0),
    )
    .unwrap()
}

fn index_exists(conn: &rusqlite::Connection, index: &str) -> bool {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='index' AND name=?)",
        [index],
        |row| row.get(0),
    )
    .unwrap()
}

fn assert_rcs_mapping_schema(conn: &rusqlite::Connection) {
    for table in [
        "rcs_conversation_mappings",
        "rcs_mapping_proofs",
        "rcs_send_ledger",
    ] {
        assert!(table_exists(conn, table), "missing {table}");
    }
    assert!(index_exists(conn, "rcs_mappings_google_active"));
    for table in [
        "rcs_link_status_projection",
        "rcs_link_eligibility_projection",
        "rcs_capability_published",
    ] {
        assert!(table_exists(conn, table), "missing {table}");
    }
}

fn remove_rcs_mapping_schema(conn: &rusqlite::Connection) {
    conn.execute_batch(
        "DROP TABLE rcs_send_ledger;
         DROP TABLE rcs_mapping_proofs;
         DROP TABLE rcs_conversation_mappings;
         ALTER TABLE attempts DROP COLUMN finalized_transport;
         DROP TABLE rcs_capability_published;
         DROP TABLE rcs_link_eligibility_projection;
         DROP TABLE rcs_link_status_projection;",
    )
    .unwrap();
}

fn column_exists(conn: &rusqlite::Connection, table: &str, column: &str) -> bool {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM pragma_table_info(?1) WHERE name=?2)",
        rusqlite::params![table, column],
        |row| row.get(0),
    )
    .unwrap()
}

fn add_notification_reply_spike(conn: &rusqlite::Connection) {
    conn.execute_batch(NOTIFICATION_REPLY_SPIKE_DDL).unwrap();
    conn.execute(
        "INSERT INTO notification_reply_submissions(command_id,message_id,conversation_id,normalized_body,submitted_at_ms,resolved) VALUES('command','message','conversation','body',1,0)",
        [],
    )
    .unwrap();
    conn.execute(
        "UPDATE gateway_settings SET reply_via_google_messages_enabled=1 WHERE id=1",
        [],
    )
    .unwrap();
}

fn assert_notification_reply_spike_removed(conn: &rusqlite::Connection) {
    assert!(!table_exists(conn, "notification_reply_submissions"));
    assert!(!column_exists(
        conn,
        "gateway_settings",
        "reply_via_google_messages_enabled"
    ));
    assert!(!index_exists(conn, "notification_reply_submissions_match"));
}

#[test]
fn notification_reply_spike_version_43_repairs_and_upgrades() {
    let dir = tempfile::tempdir().unwrap();
    let vault = Vault::new();
    let config = config(&dir, "notification-reply-v43", &vault);
    let client = open(&config);
    let settings = GatewaySettings {
        mirroring_enabled: true,
        mirroring_wifi_only: true,
        skip_silent: false,
        sms_sync_enabled: false,
        mms_sync_enabled: false,
        media_wifi_only: false,
        phone_read_enabled: true,
    };
    client.set_gateway_settings(settings.clone()).unwrap();
    drop(client);

    let conn = encrypted_connection(&config);
    remove_rcs_mapping_schema(&conn);
    conn.execute_batch(
        "DROP TABLE conversation_folders;
         DROP INDEX sender_unblock_requests_conversation;
         DROP TABLE sender_unblock_attempts;
         DROP TABLE sender_unblock_requests;
         DROP TABLE sender_blocklists;
         DROP TABLE provider_archive_observed;
         DROP TABLE provider_folder_writes;
         DROP TABLE sender_blocklist_unmapped;
         DROP INDEX mms_thread_conversations_thread;
         UPDATE schema_meta SET version=42;",
    )
    .unwrap();
    add_notification_reply_spike(&conn);
    conn.execute("UPDATE schema_meta SET version=43", [])
        .unwrap();
    drop(conn);

    let upgraded = open(&config);
    assert_eq!(upgraded.gateway_settings().unwrap(), settings);
    upgraded.set_gateway_settings(settings.clone()).unwrap();
    drop(upgraded);

    let conn = encrypted_connection(&config);
    assert_eq!(
        conn.query_row("SELECT version FROM schema_meta", [], |row| row
            .get::<_, i64>(0))
            .unwrap(),
        48
    );
    assert!(table_exists(&conn, "conversation_folders"));
    assert!(table_exists(&conn, "sender_blocklists"));
    assert!(column_exists(&conn, "sender_unblock_requests", "key_epoch"));
    assert_notification_reply_spike_removed(&conn);
    assert_rcs_mapping_schema(&conn);
}

#[test]
fn notification_reply_spike_repairs_already_upgraded_database() {
    let dir = tempfile::tempdir().unwrap();
    let vault = Vault::new();
    let config = config(&dir, "notification-reply-v45", &vault);
    drop(open(&config));

    let conn = encrypted_connection(&config);
    conn.execute_batch("DROP TABLE conversation_folders;")
        .unwrap();
    add_notification_reply_spike(&conn);
    drop(conn);

    drop(open(&config));

    let conn = encrypted_connection(&config);
    assert_eq!(
        conn.query_row("SELECT version FROM schema_meta", [], |row| row
            .get::<_, i64>(0))
            .unwrap(),
        48
    );
    assert!(table_exists(&conn, "conversation_folders"));
    assert_notification_reply_spike_removed(&conn);
    assert_rcs_mapping_schema(&conn);
}

#[test]
fn notification_reply_repair_leaves_normal_database_untouched() {
    let dir = tempfile::tempdir().unwrap();
    let vault = Vault::new();
    let config = config(&dir, "notification-reply-normal", &vault);
    drop(open(&config));

    let conn = encrypted_connection(&config);
    conn.execute(
        "INSERT INTO conversation_folders(conversation_id,folder,archived_through_ms,logical_revision,writer_device_id,producer_device_id,source_sequence) VALUES('conversation','archived',NULL,1,'writer','producer',1)",
        [],
    )
    .unwrap();
    drop(conn);

    drop(open(&config));

    let conn = encrypted_connection(&config);
    assert_eq!(
        conn.query_row(
            "SELECT folder FROM conversation_folders WHERE conversation_id='conversation'",
            [],
            |row| row.get::<_, String>(0),
        )
        .unwrap(),
        "archived"
    );
    assert_rcs_mapping_schema(&conn);
}

#[test]
fn version_45_upgrade_adds_rcs_mapping_schema_without_resetting_preferences() {
    let dir = tempfile::tempdir().unwrap();
    let vault = Vault::new();
    let config = config(&dir, "gateway-v45-upgrade", &vault);
    let client = open(&config);
    let settings = GatewaySettings {
        mirroring_enabled: true,
        mirroring_wifi_only: true,
        skip_silent: false,
        sms_sync_enabled: false,
        mms_sync_enabled: true,
        media_wifi_only: false,
        phone_read_enabled: true,
    };
    client.set_gateway_settings(settings.clone()).unwrap();
    drop(client);

    let conn = encrypted_connection(&config);
    remove_rcs_mapping_schema(&conn);
    conn.execute("UPDATE schema_meta SET version=45", [])
        .unwrap();
    drop(conn);

    let upgraded = open(&config);
    assert_eq!(upgraded.gateway_settings().unwrap(), settings);
    drop(upgraded);

    let conn = encrypted_connection(&config);
    assert_eq!(
        conn.query_row("SELECT version FROM schema_meta", [], |row| row
            .get::<_, i64>(0))
            .unwrap(),
        48
    );
    assert_eq!(
        conn.query_row("SELECT v FROM metadata WHERE k='device_id'", [], |row| {
            row.get::<_, String>(0)
        })
        .unwrap(),
        config.device_id.to_string()
    );
    assert_rcs_mapping_schema(&conn);
}

#[test]
fn version_47_upgrade_adds_only_capability_tables_and_preserves_rcs_state() {
    let rcs_command = CommandId(uuid::Uuid::from_u128(0x4701)).to_string();
    let carrier_command = CommandId(uuid::Uuid::from_u128(0x4702)).to_string();
    let conversation = ConversationId(uuid::Uuid::from_u128(0x4703)).to_string();
    let tmp_id = uuid::Uuid::from_u128(0x4704).to_string();
    let proof_hash = [0x47_u8; 32];
    let proof_digest = [0x48_u8; 32];
    // Explicit column lists include every durable v47 field, including nullable outcomes.
    let state_rows = |conn: &rusqlite::Connection| {
        let mappings = conn
            .prepare("SELECT peppy_conversation_id,google_conversation_id,class,proof_version,proof_hash,mapping_revision,verified_at_ms,invalidated_at_ms,invalidation_code FROM rcs_conversation_mappings ORDER BY peppy_conversation_id")
            .unwrap()
            .query_map([], |row| Ok((
                row.get::<_, String>(0)?, row.get::<_, String>(1)?,
                row.get::<_, String>(2)?, row.get::<_, i64>(3)?,
                row.get::<_, Vec<u8>>(4)?, row.get::<_, i64>(5)?,
                row.get::<_, i64>(6)?, row.get::<_, Option<i64>>(7)?,
                row.get::<_, Option<String>>(8)?,
            )))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        let proofs = conn
            .prepare("SELECT peppy_conversation_id,mapping_revision,canonical_participant,identity_region,own_identity FROM rcs_mapping_proofs ORDER BY peppy_conversation_id,mapping_revision")
            .unwrap()
            .query_map([], |row| Ok((
                row.get::<_, String>(0)?, row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?, row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
            )))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        let commands = conn
            .prepare("SELECT local_order,command_id,key_epoch,subscription_id,payload,retired,historical FROM command_ledger ORDER BY local_order")
            .unwrap()
            .query_map([], |row| Ok((
                row.get::<_, i64>(0)?, row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?, row.get::<_, String>(3)?,
                row.get::<_, String>(4)?, row.get::<_, i64>(5)?,
                row.get::<_, i64>(6)?,
            )))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        let attempts = conn
            .prepare("SELECT command_id,state,attempted_at_ms,finalized_transport FROM attempts ORDER BY command_id")
            .unwrap()
            .query_map([], |row| Ok((
                row.get::<_, String>(0)?, row.get::<_, String>(1)?,
                row.get::<_, Option<i64>>(2)?, row.get::<_, Option<String>>(3)?,
            )))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        let sends = conn
            .prepare("SELECT command_id,tmp_id,mapping_revision,proof_digest,google_conversation_id,attempted_at_ms,transport_ack_at_ms,transport_ack_code,retry_of_command_id FROM rcs_send_ledger ORDER BY command_id")
            .unwrap()
            .query_map([], |row| Ok((
                row.get::<_, String>(0)?, row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?, row.get::<_, Vec<u8>>(3)?,
                row.get::<_, String>(4)?, row.get::<_, i64>(5)?,
                row.get::<_, Option<i64>>(6)?, row.get::<_, Option<String>>(7)?,
                row.get::<_, Option<String>>(8)?,
            )))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        (mappings, proofs, commands, attempts, sends)
    };
    let dir = tempfile::tempdir().unwrap();
    let vault = Vault::new();
    let config = config(&dir, "gateway-v47-upgrade", &vault);
    let client = open(&config);
    let settings = GatewaySettings {
        mirroring_enabled: true,
        ..GatewaySettings::default()
    };
    client.set_gateway_settings(settings.clone()).unwrap();
    drop(client);
    let conn = encrypted_connection(&config);
    conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
    conn.execute(
        "INSERT INTO rcs_conversation_mappings(peppy_conversation_id,google_conversation_id,class,proof_version,proof_hash,mapping_revision,verified_at_ms) VALUES(?,?,?,?,?,?,?)",
        rusqlite::params![conversation, "fixture-google", "one_to_one", 2, &proof_hash[..], 7, 1000],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO rcs_mapping_proofs(peppy_conversation_id,mapping_revision,canonical_participant,identity_region,own_identity) VALUES(?,?,?,?,?)",
        rusqlite::params![conversation, 7, "raw:fixture-peer", "ZZ", "raw:fixture-own"],
    )
    .unwrap();
    for (command, subscription, state, transport, attempted_at_ms) in [
        (&rcs_command, "", "delivered", "rcs", 1100_i64),
        (
            &carrier_command,
            "android-subscription-1",
            "sent",
            "carrier",
            1200_i64,
        ),
    ] {
        conn.execute(
            "INSERT INTO command_ledger(command_id,key_epoch,subscription_id,payload) VALUES(?,?,?,?)",
            rusqlite::params![command, 1, subscription, "{}"],
        )
        .unwrap();
        // Terminal states prevent open-time recovery from converting these to outcome_unknown.
        conn.execute(
            "INSERT INTO attempts(command_id,state,attempted_at_ms,finalized_transport) VALUES(?,?,?,?)",
            rusqlite::params![command, state, attempted_at_ms, transport],
        )
        .unwrap();
    }
    conn.execute(
        "INSERT INTO rcs_send_ledger(command_id,tmp_id,mapping_revision,proof_digest,google_conversation_id,attempted_at_ms,transport_ack_at_ms,transport_ack_code) VALUES(?,?,?,?,?,?,?,?)",
        rusqlite::params![rcs_command, tmp_id, 7, &proof_digest[..], "fixture-google", 1100, 1150, "fixture-ack-delivered"],
    )
    .unwrap();
    let before = state_rows(&conn);
    assert_eq!(
        (
            before.0.len(),
            before.1.len(),
            before.2.len(),
            before.3.len(),
            before.4.len()
        ),
        (1, 1, 2, 2, 1),
        "the v47 preservation fixture must contain existing RCS and carrier state"
    );
    conn.execute_batch(
        "DROP TABLE rcs_capability_published;
         DROP TABLE rcs_link_eligibility_projection;
         DROP TABLE rcs_link_status_projection;
         UPDATE schema_meta SET version=47;",
    )
    .unwrap();
    drop(conn);
    let upgraded = open(&config);
    assert_eq!(upgraded.gateway_settings().unwrap(), settings);
    drop(upgraded);
    let conn = encrypted_connection(&config);
    assert_eq!(
        conn.query_row("SELECT version FROM schema_meta", [], |row| row
            .get::<_, i64>(0))
            .unwrap(),
        48
    );
    assert_rcs_mapping_schema(&conn);
    let after = state_rows(&conn);
    assert_eq!(
        (
            after.0.len(),
            after.1.len(),
            after.2.len(),
            after.3.len(),
            after.4.len()
        ),
        (1, 1, 2, 2, 1),
        "migration must not delete or create existing v47 state"
    );
    assert_eq!(
        after, before,
        "migration must preserve every v47 field exactly"
    );
}

#[test]
fn version_49_database_is_rejected_without_resetting_preferences() {
    let dir = tempfile::tempdir().unwrap();
    let vault = Vault::new();
    let config = config(&dir, "gateway-v49-rejected", &vault);
    let client = open(&config);
    let settings = GatewaySettings {
        mirroring_enabled: true,
        ..GatewaySettings::default()
    };
    client.set_gateway_settings(settings.clone()).unwrap();
    drop(client);

    let conn = encrypted_connection(&config);
    conn.execute("UPDATE schema_meta SET version=49", [])
        .unwrap();
    drop(conn);

    assert!(matches!(
        Client::open(config.clone(), DatabaseKey::new(&DB_KEY).unwrap()),
        Err(Error::UnsupportedSchema)
    ));

    let conn = encrypted_connection(&config);
    assert_eq!(
        conn.query_row("SELECT version FROM schema_meta", [], |row| row
            .get::<_, i64>(0))
            .unwrap(),
        49
    );
    assert_eq!(
        conn.query_row(
            "SELECT mirroring_enabled FROM gateway_settings WHERE id=1",
            [],
            |row| row.get::<_, bool>(0),
        )
        .unwrap(),
        settings.mirroring_enabled
    );
    assert_rcs_mapping_schema(&conn);
}

#[test]
fn gateway_settings_defaults_persist_and_gate_host_facts() {
    let dir = tempfile::tempdir().unwrap();
    let vault = Vault::new();
    let config = config(&dir, "gateway-settings", &vault);
    let client = open(&config);

    assert_eq!(
        client.gateway_settings().unwrap(),
        GatewaySettings::default()
    );
    client
        .set_gateway_settings(GatewaySettings {
            mirroring_enabled: true,
            mirroring_wifi_only: true,
            skip_silent: true,
            sms_sync_enabled: false,
            mms_sync_enabled: true,
            media_wifi_only: true,
            phone_read_enabled: false,
        })
        .unwrap();
    let decision = client
        .gateway_policy_decision(
            GatewayPlatform::Android,
            GatewayHostFacts {
                notification_listener_available: true,
                sms_available: true,
                notification_is_silent: true,
                ..GatewayHostFacts::default()
            },
        )
        .unwrap();
    assert!(
        !decision.capture_notification,
        "Wi-Fi and silent policies apply at effect time"
    );
    assert!(!decision.capture_sms);
    assert!(!decision.capture_mms);
    assert!(!decision.transfer_media);
    drop(client);

    let reopened = open(&config);
    assert!(reopened.gateway_settings().unwrap().mirroring_enabled);
    let ios = reopened
        .gateway_policy_decision(
            GatewayPlatform::Ios,
            GatewayHostFacts {
                notification_listener_available: true,
                sms_available: true,
                wifi_connected: true,
                ..GatewayHostFacts::default()
            },
        )
        .unwrap();
    assert!(!ios.capture_notification);
    assert!(!ios.capture_sms);
    assert!(!ios.rcs_supported);
}

#[test]
fn policy_is_not_a_destructive_sms_operation() {
    let dir = tempfile::tempdir().unwrap();
    let vault = Vault::new();
    let config = config(&dir, "gateway-sms", &vault);
    let client = unlocked(&config, &vault);
    let captured = client
        .capture_incoming(incoming("kept", "provider-1"))
        .unwrap();
    let mut settings = client.gateway_settings().unwrap();
    settings.sms_sync_enabled = false;
    client.set_gateway_settings(settings).unwrap();
    assert_eq!(client.messages(captured.conversation_id).unwrap().len(), 1);
}

#[test]
fn phone_read_capability_and_policy_follow_the_platform_and_sms_truth_table() {
    for (platform, facts, supported) in [
        (
            GatewayPlatform::Android,
            GatewayHostFacts {
                sms_available: true,
                phone_read_available: true,
                ..GatewayHostFacts::default()
            },
            true,
        ),
        (
            GatewayPlatform::Android,
            GatewayHostFacts {
                phone_read_available: true,
                ..GatewayHostFacts::default()
            },
            false,
        ),
        (
            GatewayPlatform::Ios,
            GatewayHostFacts {
                sms_available: true,
                phone_read_available: true,
                ..GatewayHostFacts::default()
            },
            false,
        ),
    ] {
        assert_eq!(
            Client::gateway_capabilities(platform, facts).phone_read_supported,
            supported
        );
    }

    let dir = tempfile::tempdir().unwrap();
    let vault = Vault::new();
    let client = open(&config(&dir, "phone-read-policy", &vault));
    let facts = GatewayHostFacts {
        sms_available: true,
        notification_listener_available: false,
        ..GatewayHostFacts::default()
    };
    let mut settings = client.gateway_settings().unwrap();
    settings.phone_read_enabled = true;
    client.set_gateway_settings(settings.clone()).unwrap();
    assert!(
        !client
            .gateway_policy_decision(GatewayPlatform::Android, facts)
            .unwrap()
            .apply_phone_read,
        "outbound notification actions remain separately unavailable"
    );
    assert!(
        client
            .gateway_policy_decision(GatewayPlatform::Android, facts)
            .unwrap()
            .ingest_phone_read,
        "provider read ingestion relies on SMS permission, not notification access"
    );

    let outbound_facts = GatewayHostFacts {
        phone_read_available: true,
        ..facts
    };
    assert!(
        client
            .gateway_policy_decision(GatewayPlatform::Android, outbound_facts)
            .unwrap()
            .apply_phone_read
    );

    settings.sms_sync_enabled = false;
    client.set_gateway_settings(settings.clone()).unwrap();
    assert!(
        !client
            .gateway_policy_decision(GatewayPlatform::Android, facts)
            .unwrap()
            .apply_phone_read
    );
    assert!(
        !client
            .gateway_policy_decision(GatewayPlatform::Android, facts)
            .unwrap()
            .ingest_phone_read
    );
    assert!(
        !client
            .gateway_policy_decision(GatewayPlatform::Ios, facts)
            .unwrap()
            .apply_phone_read
    );
    assert!(
        !client
            .gateway_policy_decision(GatewayPlatform::Ios, facts)
            .unwrap()
            .ingest_phone_read
    );

    settings.sms_sync_enabled = true;
    settings.phone_read_enabled = false;
    client.set_gateway_settings(settings).unwrap();
    assert!(
        !client
            .gateway_policy_decision(GatewayPlatform::Android, facts)
            .unwrap()
            .ingest_phone_read
    );
    assert!(
        !client
            .gateway_policy_decision(
                GatewayPlatform::Android,
                GatewayHostFacts {
                    sms_available: false,
                    ..facts
                },
            )
            .unwrap()
            .ingest_phone_read
    );
}

#[test]
fn version_25_upgrade_adds_null_status_time_without_resetting_status() {
    let dir = tempfile::tempdir().unwrap();
    let vault = Vault::new();
    let config = config(&dir, "gateway-v25-upgrade", &vault);
    let client = unlocked(&config, &vault);
    let command_id = CommandId::new();
    let producer_device_id = DeviceId::new();
    drop(client);

    let conn = rusqlite::Connection::open(&config.database_path).unwrap();
    conn.execute_batch(
        "PRAGMA key = \"x'0707070707070707070707070707070707070707070707070707070707070707'\";",
    )
    .unwrap();
    conn.execute(
        "INSERT INTO send_status(command_id,producer_device_id,producer_sequence,state) VALUES(?,?,?,?)",
        rusqlite::params![
            command_id.to_string(),
            producer_device_id.to_string(),
            42_i64,
            "sent",
        ],
    )
    .unwrap();
    remove_rcs_mapping_schema(&conn);
    roll_back_migrations_after_version_25(&conn);
    conn.execute("UPDATE schema_meta SET version=25", [])
        .unwrap();
    drop(conn);

    let upgraded = open(&config);
    drop(upgraded);

    let conn = rusqlite::Connection::open(&config.database_path).unwrap();
    conn.execute_batch(
        "PRAGMA key = \"x'0707070707070707070707070707070707070707070707070707070707070707'\";",
    )
    .unwrap();
    let version: i64 = conn
        .query_row("SELECT version FROM schema_meta", [], |row| row.get(0))
        .unwrap();
    let status: (i64, String, Option<i64>) = conn
        .query_row(
            "SELECT producer_sequence,state,status_at_ms FROM send_status WHERE command_id=? AND producer_device_id=?",
            rusqlite::params![command_id.to_string(), producer_device_id.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(version, 48);
    assert_eq!(status, (42, "sent".into(), None));
}

#[test]
fn version_17_upgrade_seeds_legacy_policy_only_once() {
    let dir = tempfile::tempdir().unwrap();
    let vault = Vault::new();
    let config = config(&dir, "gateway-upgrade", &vault);
    let client = open(&config);
    drop(client);

    // Simulate the final pre-settings schema without touching any SMS tables. Later migrations are
    // undone too, because an upgrade from version 17 replays every one of them.
    let conn = rusqlite::Connection::open(&config.database_path).unwrap();
    conn.execute_batch(
        "PRAGMA key = \"x'0707070707070707070707070707070707070707070707070707070707070707'\";",
    )
    .unwrap();
    remove_rcs_mapping_schema(&conn);
    roll_back_migrations_after_version_25(&conn);
    conn.execute_batch(
        "DROP TABLE gateway_settings; DROP TABLE outbox_attachments; \
         DROP TABLE compose_draft_redirects; ALTER TABLE compose_drafts DROP COLUMN draft_only; \
         DROP TABLE sms_acquisitions; DROP TABLE sms_provider_aliases; \
         DROP TABLE sms_scan_checkpoints; DROP TABLE sms_reconciliation_scans; \
         DROP TABLE sms_broadcast_aliases; DROP INDEX messages_sort; \
         ALTER TABLE messages DROP COLUMN sort_at_ms; ALTER TABLE messages DROP COLUMN actual_sent_at_ms; \
         DROP INDEX attempts_time; ALTER TABLE attempts DROP COLUMN attempted_at_ms; \
         UPDATE schema_meta SET version=17;",
    )
    .unwrap();
    drop(conn);

    let upgraded = open(&config);
    let seeded = upgraded.gateway_settings().unwrap();
    // Mirroring must default to false on upgrade. Native host separately imports explicit legacy consent.
    assert!(!seeded.mirroring_enabled);
    assert!(!seeded.skip_silent);
    assert!(!seeded.media_wifi_only);

    // Verify explicit set persists on reopen
    upgraded
        .set_gateway_settings(GatewaySettings {
            mirroring_enabled: true,
            ..seeded
        })
        .unwrap();
    drop(upgraded);

    let reopened = open(&config);
    assert!(reopened.gateway_settings().unwrap().mirroring_enabled);
}
