use peppy_server::scope;
use sqlx::{Row, postgres::PgPoolOptions};
use uuid::Uuid;

#[tokio::test]
async fn begin_sets_a_transaction_local_vault_scope() {
    let database_url = std::env::var("TEST_DATABASE_URL")
        .expect("TEST_DATABASE_URL is required; integration tests never silently skip");
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .connect(&database_url)
        .await
        .unwrap();
    let vault = Uuid::new_v4();

    let mut tx = scope::begin(&pool, vault).await.unwrap();
    let row = sqlx::query("SELECT current_setting('peppy.vault_id', true) AS vault")
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    let expected = vault.to_string();
    assert_eq!(
        row.get::<Option<String>, _>("vault").as_deref(),
        Some(expected.as_str())
    );
    tx.rollback().await.unwrap();

    let row = sqlx::query("SELECT current_setting('peppy.vault_id', true) AS vault")
        .fetch_one(&pool)
        .await
        .unwrap();
    // PostgreSQL can retain the custom GUC name with an empty value after a
    // local setting rolls back. Neither absent nor empty confers vault access.
    assert!(matches!(
        row.get::<Option<String>, _>("vault").as_deref(),
        None | Some("")
    ));
}
