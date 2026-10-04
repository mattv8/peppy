use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

/// Starts a transaction whose vault scope is confined to that transaction.
///
/// This deliberately uses `set_config(..., true)` rather than a session SET so
/// a connection returned to the pool cannot retain a previous caller's scope.
pub async fn begin(
    pool: &PgPool,
    vault: Uuid,
) -> Result<Transaction<'static, Postgres>, sqlx::Error> {
    let mut tx = pool.begin().await?;
    set(&mut tx, vault).await?;
    Ok(tx)
}

/// Starts a fixed-cut read-only snapshot before applying its vault scope.
pub async fn begin_read_only_repeatable(
    pool: &PgPool,
    vault: Uuid,
) -> Result<Transaction<'static, Postgres>, sqlx::Error> {
    let mut tx = pool.begin().await?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
        .execute(&mut *tx)
        .await?;
    set(&mut tx, vault).await?;
    Ok(tx)
}

/// Applies the vault scope to a caller-provided transaction, such as hosted
/// provisioning's atomic grant-consumption transaction.
pub async fn set(tx: &mut Transaction<'_, Postgres>, vault: Uuid) -> Result<(), sqlx::Error> {
    sqlx::query("SELECT set_config('peppy.vault_id',$1,true)")
        .bind(vault.to_string())
        .execute(&mut **tx)
        .await?;
    Ok(())
}
