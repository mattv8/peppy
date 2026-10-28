mod common;

use common::*;

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
    assert_eq!(version, 37);
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
