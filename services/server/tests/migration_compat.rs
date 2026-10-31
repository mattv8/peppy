use sqlx::{PgPool, postgres::PgPoolOptions};
use url::Url;
use uuid::Uuid;

const DOWN_0019: &str = include_str!("../migrations/down/0019_attachment_retention.down.sql");

// This is the Release A predecessor's vault-delete sequence, copied verbatim
// from the production source before 0019. It deletes devices only as part of a
// whole-vault deletion; standalone device deletion was never supported there.
const LEGACY_DELETE_TABLES: &[&str] = &[
    "device_wake_jobs",
    "device_wake_routes",
    "command_receipts",
    "commands",
    "device_cursors",
    "device_capabilities",
    "attachment_record_references",
    "record_supersessions",
    "record_compaction",
    "compacted_records",
    "event_log",
    "encrypted_records",
    "public_attachment_copies",
    "attachments",
    "upload_reservations",
    "outbox_jobs",
    "vault_key_profiles",
    "phone_invitations",
    "computer_enrollments",
    "device_credentials",
    "devices",
];

#[tokio::test]
async fn attachment_retention_migration_preserves_legacy_vault_delete_contract() {
    let database_url = std::env::var("TEST_DATABASE_URL")
        .expect("TEST_DATABASE_URL is required; migration compatibility uses a disposable fixture");
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(&database_url)
        .await
        .unwrap();
    let (schema, pool) = migrated_schema(&admin, &database_url).await;
    // A sibling migrated schema on the shared database must not leak its catalog rows into this one.
    let (control_schema, control_pool) = migrated_schema(&admin, &database_url).await;
    assert_retention_foreign_keys(&control_pool).await;
    assert_retention_foreign_keys(&pool).await;
    assert_trigger_seeds_protected_metadata(&pool).await;
    assert_legacy_delete_succeeds_without_debt(&pool).await;
    assert_legacy_delete_fails_closed_with_debt(&pool).await;
    assert_current_delete_queues_removal_key(&pool).await;
    assert_down_refuses_debt_and_applies_when_empty(&pool).await;

    pool.close().await;
    control_pool.close().await;
    sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
        .execute(&admin)
        .await
        .unwrap();
    sqlx::query(&format!("DROP SCHEMA {control_schema} CASCADE"))
        .execute(&admin)
        .await
        .unwrap();
}

async fn migrated_schema(admin: &PgPool, database_url: &str) -> (String, PgPool) {
    let schema = format!("migration_compat_{}", Uuid::new_v4().simple());
    sqlx::query(&format!("CREATE SCHEMA {schema}"))
        .execute(admin)
        .await
        .unwrap();
    let pool = isolated_pool(database_url, &schema).await;
    peppy_server::MIGRATOR.run(&pool).await.unwrap();
    (schema, pool)
}

async fn isolated_pool(database_url: &str, schema: &str) -> PgPool {
    let mut isolated: Url = database_url.parse().unwrap();
    isolated
        .query_pairs_mut()
        .append_pair("options", &format!("-csearch_path={schema}"));
    PgPoolOptions::new()
        .max_connections(1)
        .connect(isolated.as_str())
        .await
        .unwrap()
}

async fn assert_retention_foreign_keys(pool: &PgPool) {
    let constraints: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT source.relname,target.relname,foreign_key.confdeltype::text FROM pg_constraint foreign_key JOIN pg_class source ON source.oid=foreign_key.conrelid JOIN pg_class target ON target.oid=foreign_key.confrelid WHERE foreign_key.contype='f' AND foreign_key.connamespace=current_schema()::regnamespace AND target.relnamespace=current_schema()::regnamespace AND source.relname IN ('attachment_retention_uses','attachment_retention_devices','attachment_removals') ORDER BY source.relname,target.relname,foreign_key.confdeltype",
    )
    .fetch_all(pool)
    .await
    .unwrap();
    assert_eq!(
        constraints,
        vec![
            ("attachment_removals".into(), "vaults".into(), "r".into()),
            (
                "attachment_retention_devices".into(),
                "devices".into(),
                "c".into()
            ),
            (
                "attachment_retention_devices".into(),
                "vaults".into(),
                "c".into()
            ),
            (
                "attachment_retention_uses".into(),
                "devices".into(),
                "c".into()
            ),
            (
                "attachment_retention_uses".into(),
                "vaults".into(),
                "c".into()
            ),
        ]
    );
    let indexed: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM pg_indexes WHERE schemaname=current_schema() AND tablename='attachment_retention_uses' AND indexdef LIKE '%(vault_id, device_id)%')",
    )
    .fetch_one(pool)
    .await
    .unwrap();
    assert!(indexed);
}

async fn assert_trigger_seeds_protected_metadata(pool: &PgPool) {
    let (vault, device) = seed_vault_and_device(pool).await;
    let seeded: (bool, bool, i64) = sqlx::query_as(
        "SELECT cleanup_ready,reconciliation_needed,(SELECT count(*) FROM attachment_removals WHERE vault_id=$1) FROM attachment_retention_devices WHERE vault_id=$1 AND device_id=$2",
    )
    .bind(vault)
    .bind(device)
    .fetch_one(pool)
    .await
    .unwrap();
    assert_eq!(
        seeded,
        (false, true, 0),
        "cleanup defaults off and writes no removal debt"
    );
}

async fn assert_legacy_delete_succeeds_without_debt(pool: &PgPool) {
    let (vault, device) = seed_vault_and_device(pool).await;
    let attachment = seed_attachment(pool, vault, device, "legacy/first").await;
    seed_attachment(pool, vault, device, "legacy/second").await;
    sqlx::query("INSERT INTO attachment_retention_uses(vault_id,use_id,attachment_id,device_id,kind) VALUES($1,$2,$3,$4,'message_history')")
        .bind(vault)
        .bind(Uuid::new_v4())
        .bind(attachment)
        .bind(device)
        .execute(pool)
        .await
        .unwrap();

    let mut transaction = pool.begin().await.unwrap();
    legacy_delete_vault(&mut transaction, vault).await.unwrap();
    transaction.commit().await.unwrap();

    let keys: Vec<String> = sqlx::query_scalar(
        "SELECT object_key FROM storage_deletions WHERE vault_id=$1 ORDER BY object_key",
    )
    .bind(vault)
    .fetch_all(pool)
    .await
    .unwrap();
    assert_eq!(
        keys,
        vec!["legacy/first".to_owned(), "legacy/second".to_owned()]
    );
}

async fn assert_legacy_delete_fails_closed_with_debt(pool: &PgPool) {
    let (vault, device) = seed_vault_and_device(pool).await;
    let attachment = seed_attachment(pool, vault, device, "legacy/debt-attachment").await;
    sqlx::query("INSERT INTO attachment_retention_uses(vault_id,use_id,attachment_id,device_id,kind) VALUES($1,$2,$3,$4,'message_history')")
        .bind(vault)
        .bind(Uuid::new_v4())
        .bind(attachment)
        .bind(device)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO attachment_removals(vault_id,attachment_id,object_key,ciphertext_bytes) VALUES($1,$2,'legacy/debt',1)")
        .bind(vault)
        .bind(Uuid::new_v4())
        .execute(pool)
        .await
        .unwrap();

    let mut transaction = pool.begin().await.unwrap();
    assert!(legacy_delete_vault(&mut transaction, vault).await.is_err());
    transaction.rollback().await.unwrap();

    let counts: (i64, i64, i64, i64, i64, i64) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM vaults WHERE vault_id=$1), (SELECT count(*) FROM devices WHERE vault_id=$1), (SELECT count(*) FROM attachment_retention_devices WHERE vault_id=$1), (SELECT count(*) FROM attachment_retention_uses WHERE vault_id=$1), (SELECT count(*) FROM attachment_removals WHERE vault_id=$1), (SELECT count(*) FROM storage_deletions WHERE vault_id=$1)",
    )
    .bind(vault)
    .fetch_one(pool)
    .await
    .unwrap();
    assert_eq!(counts, (1, 1, 1, 1, 1, 0));
}

async fn assert_current_delete_queues_removal_key(pool: &PgPool) {
    let (vault, _) = seed_vault_and_device(pool).await;
    sqlx::query("INSERT INTO attachment_removals(vault_id,attachment_id,object_key,ciphertext_bytes) VALUES($1,$2,'current/removal',1)")
        .bind(vault)
        .bind(Uuid::new_v4())
        .execute(pool)
        .await
        .unwrap();
    let mut transaction = pool.begin().await.unwrap();
    peppy_server::api::delete_vault_in_transaction(&mut transaction, vault)
        .await
        .unwrap();
    transaction.commit().await.unwrap();

    let reason: String = sqlx::query_scalar(
        "SELECT reason FROM storage_deletions WHERE object_key='current/removal'",
    )
    .fetch_one(pool)
    .await
    .unwrap();
    assert_eq!(reason, "vault_deleted");
}

async fn assert_down_refuses_debt_and_applies_when_empty(pool: &PgPool) {
    let (vault, _) = seed_vault_and_device(pool).await;
    sqlx::query("INSERT INTO attachment_removals(vault_id,attachment_id,object_key,ciphertext_bytes) VALUES($1,$2,'down/debt',1)")
        .bind(vault)
        .bind(Uuid::new_v4())
        .execute(pool)
        .await
        .unwrap();
    let mut transaction = pool.begin().await.unwrap();
    assert!(
        sqlx::raw_sql(DOWN_0019)
            .execute(&mut *transaction)
            .await
            .is_err()
    );
    transaction.rollback().await.unwrap();
    sqlx::query("DELETE FROM attachment_removals")
        .execute(pool)
        .await
        .unwrap();

    sqlx::raw_sql(DOWN_0019).execute(pool).await.unwrap();
    let retention_table_exists: Option<String> =
        sqlx::query_scalar("SELECT to_regclass('attachment_retention_devices')::text")
            .fetch_one(pool)
            .await
            .unwrap();
    assert!(retention_table_exists.is_none());
}

async fn seed_vault_and_device(pool: &PgPool) -> (Uuid, Uuid) {
    let vault = Uuid::new_v4();
    let device = Uuid::new_v4();
    sqlx::query("INSERT INTO vaults(vault_id,public_key_profile,encrypted_vault_check_header,key_epoch,profile_fingerprint) VALUES($1,'{}',$2,1,$3)")
        .bind(vault)
        .bind(vec![0_u8])
        .bind("0".repeat(64))
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO devices(vault_id,device_id,role,public_key,profile_fingerprint,key_epoch) VALUES($1,$2,'device','{}',$3,1)")
        .bind(vault)
        .bind(device)
        .bind("0".repeat(64))
        .execute(pool)
        .await
        .unwrap();
    (vault, device)
}

async fn seed_attachment(pool: &PgPool, vault: Uuid, device: Uuid, object_key: &str) -> Uuid {
    let attachment = Uuid::new_v4();
    sqlx::query("INSERT INTO attachments(vault_id,attachment_id,object_key,ciphertext_bytes,ciphertext_sha256,created_by_device_id) VALUES($1,$2,$3,1,$4,$5)")
        .bind(vault)
        .bind(attachment)
        .bind(object_key)
        .bind(vec![0_u8; 32])
        .bind(device)
        .execute(pool)
        .await
        .unwrap();
    attachment
}

async fn legacy_delete_vault(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    vault: Uuid,
) -> Result<(), sqlx::Error> {
    sqlx::query("SELECT 1 FROM vaults WHERE vault_id=$1 FOR UPDATE")
        .bind(vault)
        .execute(&mut **transaction)
        .await?;
    sqlx::query("INSERT INTO storage_deletions(object_key,vault_id,reason) SELECT object_key,$1,'vault_deleted' FROM (SELECT object_key FROM upload_reservations WHERE vault_id=$1 UNION SELECT object_key FROM attachments WHERE vault_id=$1 UNION SELECT object_key FROM public_attachment_copies WHERE vault_id=$1) keys ON CONFLICT(object_key) DO UPDATE SET reason='vault_deleted',not_before=GREATEST(storage_deletions.not_before,now())")
        .bind(vault)
        .execute(&mut **transaction)
        .await?;
    sqlx::query("SET LOCAL peppy.compaction = 'on'")
        .execute(&mut **transaction)
        .await?;
    sqlx::query("SET LOCAL peppy.vault_delete = 'on'")
        .execute(&mut **transaction)
        .await?;
    for table in LEGACY_DELETE_TABLES {
        sqlx::query(&format!("DELETE FROM {table} WHERE vault_id=$1"))
            .bind(vault)
            .execute(&mut **transaction)
            .await?;
    }
    sqlx::query("DELETE FROM vaults WHERE vault_id=$1")
        .bind(vault)
        .execute(&mut **transaction)
        .await?;
    Ok(())
}
