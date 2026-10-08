use std::env;

use sqlx::{Connection, PgConnection};
use uuid::Uuid;

const UP: [&str; 13] = [
    include_str!("../migrations/0001_foundation.sql"),
    include_str!("../migrations/0002_messaging.sql"),
    include_str!("../migrations/0003_pairing_proofs.sql"),
    include_str!("../migrations/0004_device_capabilities.sql"),
    include_str!("../migrations/0005_attachments.sql"),
    include_str!("../migrations/0006_attachment_shares.sql"),
    include_str!("../migrations/0007_snapshot_retention.sql"),
    include_str!("../migrations/0008_storage_hardening.sql"),
    include_str!("../migrations/0009_compaction.sql"),
    include_str!("../migrations/0010_attachment_references.sql"),
    include_str!("../migrations/0011_mobile_gateway.sql"),
    include_str!("../migrations/0012_durable_wake_leases.sql"),
    include_str!("../migrations/0013_pairing_join_requests.sql"),
];

const DOWN: [&str; 13] = [
    include_str!("../migrations/down/0001_foundation.down.sql"),
    include_str!("../migrations/down/0002_messaging.down.sql"),
    include_str!("../migrations/down/0003_pairing_proofs.down.sql"),
    include_str!("../migrations/down/0004_device_capabilities.down.sql"),
    include_str!("../migrations/down/0005_attachments.down.sql"),
    include_str!("../migrations/down/0006_attachment_shares.down.sql"),
    include_str!("../migrations/down/0007_snapshot_retention.down.sql"),
    include_str!("../migrations/down/0008_storage_hardening.down.sql"),
    include_str!("../migrations/down/0009_compaction.down.sql"),
    include_str!("../migrations/down/0010_attachment_references.down.sql"),
    include_str!("../migrations/down/0011_mobile_gateway.down.sql"),
    include_str!("../migrations/down/0012_durable_wake_leases.down.sql"),
    include_str!("../migrations/down/0013_pairing_join_requests.down.sql"),
];

#[tokio::test]
async fn migrations_round_trip_in_an_isolated_schema() {
    let database_url = env::var("TEST_DATABASE_URL")
        .expect("TEST_DATABASE_URL is required; integration tests never silently skip");
    let schema = format!("migration_round_trip_{}", Uuid::new_v4().simple());
    let mut database = PgConnection::connect(&database_url).await.unwrap();
    let quoted_schema = quote_identifier(&schema);
    sqlx::query(&format!("CREATE SCHEMA {quoted_schema}"))
        .execute(&mut database)
        .await
        .unwrap();
    sqlx::query(&format!("SET search_path TO {quoted_schema}"))
        .execute(&mut database)
        .await
        .unwrap();

    for (version, migration) in UP.iter().enumerate() {
        let predecessor = schema_fingerprint(&mut database, &schema).await;
        run(&mut database, migration).await;
        seed_populated_downgrade_case(&mut database, version).await;
        run(&mut database, DOWN[version]).await;
        assert_eq!(
            schema_fingerprint(&mut database, &schema).await,
            predecessor
        );
        run(&mut database, UP[version]).await;
    }
    assert_latest_schema(&mut database).await;
    let latest_schema = schema_fingerprint(&mut database, &schema).await;

    run(&mut database, "BEGIN").await;
    for migration in DOWN.iter().rev() {
        run(&mut database, migration).await;
    }
    assert_eq!(schema_fingerprint(&mut database, &schema).await, "");
    run(&mut database, "ROLLBACK").await;
    assert_eq!(
        schema_fingerprint(&mut database, &schema).await,
        latest_schema
    );

    run(&mut database, "BEGIN").await;
    assert!(run_result(&mut database, DOWN[6]).await.is_err());
    run(&mut database, "ROLLBACK").await;
    assert_eq!(
        schema_fingerprint(&mut database, &schema).await,
        latest_schema
    );

    sqlx::query(&format!("DROP SCHEMA {quoted_schema} CASCADE"))
        .execute(&mut database)
        .await
        .unwrap();
}

async fn run(database: &mut PgConnection, migration: &str) {
    run_result(database, migration).await.unwrap();
}

async fn assert_latest_schema(database: &mut PgConnection) {
    assert!(table_exists(database, "pairing_join_requests").await);
    assert!(index_exists(database, "device_wake_jobs_lease_due").await);
    assert!(constraint_exists(database, "storage_deletions_reason_check").await);
    assert!(function_exists(database, "peppy_reject_mutation").await);
    assert!(trigger_exists(database, "encrypted_records_immutable").await);
}

async fn seed_populated_downgrade_case(database: &mut PgConnection, version: usize) {
    match version {
        6 => {
            sqlx::query(
                "INSERT INTO outbox_jobs(vault_id, cursor, kind, payload) VALUES ('00000000-0000-0000-0000-000000000001', 1, 'legacy', NULL)",
            )
            .execute(&mut *database)
            .await
            .unwrap();
        }
        8 => {
            run(
                database,
                "
                INSERT INTO vaults(vault_id, public_key_profile, encrypted_vault_check_header, key_epoch, profile_fingerprint)
                VALUES ('00000000-0000-0000-0000-000000000010', '{}', '\\x01', 1, repeat('a', 64));
                INSERT INTO devices(vault_id, device_id, role, public_key, profile_fingerprint, key_epoch)
                VALUES ('00000000-0000-0000-0000-000000000010', '00000000-0000-0000-0000-000000000011', 'device', '{}', repeat('a', 64), 1);
                INSERT INTO attachments(vault_id, attachment_id, object_key, ciphertext_bytes, ciphertext_sha256, created_by_device_id)
                VALUES ('00000000-0000-0000-0000-000000000010', '00000000-0000-0000-0000-000000000012', 'private', 1, decode(repeat('01', 32), 'hex'), '00000000-0000-0000-0000-000000000011');
                INSERT INTO public_attachment_copies(share_id, vault_id, attachment_id, object_key, token_digest, safe_name, media_type, byte_count, created_by_device_id)
                VALUES ('00000000-0000-0000-0000-000000000013', '00000000-0000-0000-0000-000000000010', '00000000-0000-0000-0000-000000000012', 'public', decode(repeat('02', 32), 'hex'), 'file', 'text/plain', 1, '00000000-0000-0000-0000-000000000011');
                DELETE FROM attachments WHERE attachment_id = '00000000-0000-0000-0000-000000000012';
                INSERT INTO storage_deletions(object_key, vault_id, reason)
                VALUES ('private', '00000000-0000-0000-0000-000000000010', 'finalized_attachment');
                ",
            )
            .await;
        }
        10 => {
            sqlx::query(
                "INSERT INTO storage_deletions(object_key, vault_id, reason) VALUES ('deleted-vault', '00000000-0000-0000-0000-000000000010', 'vault_deleted')",
            )
            .execute(&mut *database)
            .await
            .unwrap();
        }
        _ => {}
    }
}

async fn schema_fingerprint(database: &mut PgConnection, schema: &str) -> String {
    let fingerprint: String = sqlx::query_scalar(
        "
        SELECT coalesce(string_agg(definition, E'\\n' ORDER BY definition), '')
        FROM (
          SELECT 'column|' || c.relname || '|' || a.attname || '|' || pg_catalog.format_type(a.atttypid, a.atttypmod) || '|' || a.attnotnull::text || '|' || coalesce(pg_get_expr(d.adbin, d.adrelid), '') AS definition
          FROM pg_attribute a JOIN pg_class c ON c.oid = a.attrelid LEFT JOIN pg_attrdef d ON d.adrelid = a.attrelid AND d.adnum = a.attnum
          WHERE c.relnamespace = current_schema()::regnamespace AND c.relkind IN ('r', 'p') AND a.attnum > 0 AND NOT a.attisdropped
          UNION ALL
          SELECT 'constraint|' || c.relname || '|' || con.conname || '|' || pg_get_constraintdef(con.oid, true)
          FROM pg_constraint con JOIN pg_class c ON c.oid = con.conrelid WHERE con.connamespace = current_schema()::regnamespace
          UNION ALL
          SELECT 'index|' || pg_get_indexdef(i.indexrelid)
          FROM pg_index i JOIN pg_class c ON c.oid = i.indexrelid WHERE c.relnamespace = current_schema()::regnamespace
          UNION ALL
          SELECT 'function|' || p.proname || '|' || pg_get_functiondef(p.oid)
          FROM pg_proc p WHERE p.pronamespace = current_schema()::regnamespace
          UNION ALL
          SELECT 'trigger|' || c.relname || '|' || pg_get_triggerdef(t.oid, true)
          FROM pg_trigger t JOIN pg_class c ON c.oid = t.tgrelid
          WHERE c.relnamespace = current_schema()::regnamespace AND NOT t.tgisinternal
        ) catalog",
    )
    .fetch_one(&mut *database)
    .await
    .unwrap();
    fingerprint.replace(schema, "<schema>")
}

async fn run_result(database: &mut PgConnection, migration: &str) -> Result<(), sqlx::Error> {
    sqlx::raw_sql(migration).execute(database).await.map(|_| ())
}

async fn table_exists(database: &mut PgConnection, table: &str) -> bool {
    relation_exists(database, "r", table).await
}

async fn index_exists(database: &mut PgConnection, index: &str) -> bool {
    relation_exists(database, "i", index).await
}

async fn relation_exists(database: &mut PgConnection, kind: &str, name: &str) -> bool {
    sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM pg_class WHERE relnamespace = current_schema()::regnamespace AND relkind::text = $1 AND relname = $2)",
    )
    .bind(kind)
    .bind(name)
    .fetch_one(database)
    .await
    .unwrap()
}

async fn constraint_exists(database: &mut PgConnection, constraint: &str) -> bool {
    sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM pg_constraint WHERE connamespace = current_schema()::regnamespace AND conname = $1)",
    )
    .bind(constraint)
    .fetch_one(database)
    .await
    .unwrap()
}

async fn function_exists(database: &mut PgConnection, function: &str) -> bool {
    sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM pg_proc WHERE pronamespace = current_schema()::regnamespace AND proname = $1)",
    )
    .bind(function)
    .fetch_one(database)
    .await
    .unwrap()
}

async fn trigger_exists(database: &mut PgConnection, trigger: &str) -> bool {
    sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM pg_trigger WHERE tgrelid = 'encrypted_records'::regclass AND tgname = $1 AND NOT tgisinternal)",
    )
    .bind(trigger)
    .fetch_one(database)
    .await
    .unwrap()
}

fn quote_identifier(identifier: &str) -> String {
    format!("\"{}\"", identifier.replace('"', "\"\""))
}
