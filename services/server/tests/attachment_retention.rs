use peppy_server::attachment_retention::retention_storage_status_on_connection;
use sqlx::postgres::PgPoolOptions;
use url::Url;
use uuid::Uuid;

#[tokio::test]
async fn status_keeps_unpurged_removal_debt_and_reports_protection() {
    let database_url = std::env::var("TEST_DATABASE_URL")
        .expect("TEST_DATABASE_URL is required for retention integration tests");
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(&database_url)
        .await
        .unwrap();
    let schema = format!("peppy_retention_test_{}", Uuid::new_v4().simple());
    sqlx::query(&format!("CREATE SCHEMA {schema}"))
        .execute(&admin)
        .await
        .unwrap();
    let mut isolated: Url = database_url.parse().unwrap();
    isolated
        .query_pairs_mut()
        .append_pair("options", &format!("-csearch_path={schema}"));
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .connect(isolated.as_str())
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();

    let vault = Uuid::new_v4();
    let device = Uuid::new_v4();
    let protected = Uuid::new_v4();
    let removed = Uuid::new_v4();
    sqlx::query("INSERT INTO vaults(vault_id,public_key_profile,encrypted_vault_check_header,key_epoch,profile_fingerprint) VALUES($1,'{}',$2,1,$3)")
        .bind(vault).bind(vec![0_u8]).bind("0".repeat(64)).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO devices(vault_id,device_id,role,public_key,profile_fingerprint,key_epoch) VALUES($1,$2,'device','{}',$3,1)")
        .bind(vault).bind(device).bind("0".repeat(64)).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO attachments(vault_id,attachment_id,object_key,ciphertext_bytes,ciphertext_sha256,created_by_device_id) VALUES($1,$2,'private/protected',11,$3,$4)")
        .bind(vault).bind(protected).bind(vec![0_u8; 32]).bind(device).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO attachment_removals(vault_id,attachment_id,object_key,ciphertext_bytes) VALUES($1,$2,'private/removed',7)")
        .bind(vault).bind(removed).execute(&pool).await.unwrap();

    let mut connection = pool.acquire().await.unwrap();
    let facts = retention_storage_status_on_connection(&mut connection, vault)
        .await
        .unwrap();
    assert_eq!(facts.protected_bytes, 11);
    assert_eq!(facts.removed_bytes, 7);
    assert!(!facts.all_devices_ready);
    assert!(facts.cleanup_pending);

    drop(connection);
    sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
        .execute(&admin)
        .await
        .unwrap();
}

#[tokio::test]
async fn vault_lock_serializes_readiness_with_destructive_selection() {
    let database_url = std::env::var("TEST_DATABASE_URL")
        .expect("TEST_DATABASE_URL is required for retention integration tests");
    let admin = PgPoolOptions::new()
        .max_connections(2)
        .connect(&database_url)
        .await
        .unwrap();
    let schema = format!("peppy_retention_lock_test_{}", Uuid::new_v4().simple());
    sqlx::query(&format!("CREATE SCHEMA {schema}"))
        .execute(&admin)
        .await
        .unwrap();
    let mut isolated: Url = database_url.parse().unwrap();
    isolated
        .query_pairs_mut()
        .append_pair("options", &format!("-csearch_path={schema}"));
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(isolated.as_str())
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();

    let vault = Uuid::new_v4();
    let device = Uuid::new_v4();
    sqlx::query("INSERT INTO vaults(vault_id,public_key_profile,encrypted_vault_check_header,key_epoch,profile_fingerprint) VALUES($1,'{}',$2,1,$3)")
        .bind(vault).bind(vec![0_u8]).bind("0".repeat(64)).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO devices(vault_id,device_id,role,public_key,profile_fingerprint,key_epoch) VALUES($1,$2,'device','{}',$3,1)")
        .bind(vault).bind(device).bind("0".repeat(64)).execute(&pool).await.unwrap();
    sqlx::query("UPDATE attachment_retention_devices SET cleanup_ready=TRUE,reconciliation_needed=FALSE WHERE vault_id=$1 AND device_id=$2")
        .bind(vault).bind(device).execute(&pool).await.unwrap();

    let mut selection = pool.begin().await.unwrap();
    sqlx::query("SELECT 1 FROM vaults WHERE vault_id=$1 FOR UPDATE")
        .bind(vault)
        .execute(&mut *selection)
        .await
        .unwrap();
    let competing_pool = pool.clone();
    let withdrawal = tokio::spawn(async move {
        let mut tx = competing_pool.begin().await.unwrap();
        sqlx::query("SELECT 1 FROM vaults WHERE vault_id=$1 FOR UPDATE")
            .bind(vault)
            .execute(&mut *tx)
            .await
            .unwrap();
        peppy_server::attachment_retention::withdraw_retention_readiness_on_connection(
            &mut tx, vault, device,
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
    });
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(100), async {
            while !withdrawal.is_finished() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .is_err()
    );
    selection.commit().await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(2), withdrawal)
        .await
        .unwrap()
        .unwrap();

    let state: (bool, bool) = sqlx::query_as("SELECT cleanup_ready,reconciliation_needed FROM attachment_retention_devices WHERE vault_id=$1 AND device_id=$2")
        .bind(vault).bind(device).fetch_one(&pool).await.unwrap();
    assert_eq!(state, (false, true));

    pool.close().await;
    sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
        .execute(&admin)
        .await
        .unwrap();
}

#[tokio::test]
async fn missing_cleanup_queue_is_repaired_without_clearing_debt() {
    let database_url = std::env::var("TEST_DATABASE_URL").unwrap();
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(&database_url)
        .await
        .unwrap();
    let schema = format!("peppy_retention_repair_test_{}", Uuid::new_v4().simple());
    sqlx::query(&format!("CREATE SCHEMA {schema}"))
        .execute(&admin)
        .await
        .unwrap();
    let mut isolated: Url = database_url.parse().unwrap();
    isolated
        .query_pairs_mut()
        .append_pair("options", &format!("-csearch_path={schema}"));
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .connect(isolated.as_str())
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    let vault = Uuid::new_v4();
    sqlx::query("INSERT INTO vaults(vault_id,public_key_profile,encrypted_vault_check_header,key_epoch,profile_fingerprint) VALUES($1,'{}',$2,1,$3)")
        .bind(vault).bind(vec![0_u8]).bind("0".repeat(64)).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO attachment_removals(vault_id,attachment_id,object_key,ciphertext_bytes) VALUES($1,$2,'private/missing-queue',7)")
        .bind(vault).bind(Uuid::new_v4()).execute(&pool).await.unwrap();
    assert_eq!(
        peppy_server::attachment_retention::repair_retention_cleanup_queue(&pool, 1)
            .await
            .unwrap(),
        1
    );
    let reason: String = sqlx::query_scalar(
        "SELECT reason FROM storage_deletions WHERE object_key='private/missing-queue'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(reason, "retention_cleanup");
    let debt_retained: bool = sqlx::query_scalar(
        "SELECT purged_at IS NULL FROM attachment_removals WHERE object_key='private/missing-queue'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(debt_retained);
    pool.close().await;
    sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
        .execute(&admin)
        .await
        .unwrap();
}
