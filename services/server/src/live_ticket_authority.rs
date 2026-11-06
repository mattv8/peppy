//! Durable, per-device live-ticket lifetime allocation.
//!
//! Callers must lock the matching `devices` row before calling these helpers.
//! They deliberately do not create sessions or tickets; lifecycle activation is
//! a later change.

use sqlx::{Postgres, Transaction};
use uuid::Uuid;

const LAST_ALLOCATION_ID: i64 = i64::MAX;

#[derive(Debug, thiserror::Error)]
pub(crate) enum AllocationError {
    #[error("live ticket allocation is exhausted")]
    Exhausted,
    #[error(transparent)]
    Database(#[from] sqlx::Error),
}

#[derive(Clone, Copy)]
enum Counter {
    SessionIncarnation,
    ConnectionId,
}

impl Counter {
    const fn column(self) -> &'static str {
        match self {
            Self::SessionIncarnation => "next_session_incarnation",
            Self::ConnectionId => "next_connection_id",
        }
    }
}

/// Allocates an exact session lifetime after the caller owns the device fence.
#[allow(dead_code, reason = "lifecycle activation is intentionally deferred")]
pub(crate) async fn allocate_session_incarnation(
    tx: &mut Transaction<'_, Postgres>,
    vault_id: Uuid,
    device_id: Uuid,
) -> Result<i64, AllocationError> {
    allocate(tx, vault_id, device_id, Counter::SessionIncarnation).await
}

/// Allocates an exact connection lifetime after the caller owns the device fence.
#[allow(dead_code, reason = "lifecycle activation is intentionally deferred")]
pub(crate) async fn allocate_connection_id(
    tx: &mut Transaction<'_, Postgres>,
    vault_id: Uuid,
    device_id: Uuid,
) -> Result<i64, AllocationError> {
    allocate(tx, vault_id, device_id, Counter::ConnectionId).await
}

async fn allocate(
    tx: &mut Transaction<'_, Postgres>,
    vault_id: Uuid,
    device_id: Uuid,
    counter: Counter,
) -> Result<i64, AllocationError> {
    sqlx::query(
        "INSERT INTO live_ticket_device_authority(vault_id, device_id, next_session_incarnation, next_connection_id) \
         VALUES($1, $2, 0, 0) ON CONFLICT DO NOTHING",
    )
    .bind(vault_id)
    .bind(device_id)
    .execute(&mut **tx)
    .await?;

    let query = format!(
        "UPDATE live_ticket_device_authority SET {column}={column}+1 \
         WHERE vault_id=$1 AND device_id=$2 AND {column}<$3 RETURNING {column}",
        column = counter.column(),
    );
    sqlx::query_scalar(&query)
        .bind(vault_id)
        .bind(device_id)
        .bind(LAST_ALLOCATION_ID)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or(AllocationError::Exhausted)
}

#[cfg(test)]
mod tests {
    use super::{AllocationError, allocate_connection_id, allocate_session_incarnation};
    use sha2::{Digest, Sha256};
    use sqlx::{PgPool, postgres::PgPoolOptions};
    use uuid::Uuid;

    async fn test_pool() -> (PgPool, PgPool, String) {
        let database_url = std::env::var("TEST_DATABASE_URL")
            .expect("TEST_DATABASE_URL is required; authority tests never silently skip");
        let admin = PgPoolOptions::new()
            .max_connections(2)
            .connect(&database_url)
            .await
            .unwrap();
        let schema = format!("peppy_ticket_authority_{}", Uuid::new_v4().simple());
        sqlx::query(&format!("CREATE SCHEMA {schema}"))
            .execute(&admin)
            .await
            .unwrap();
        let mut isolated_url: url::Url = database_url.parse().unwrap();
        isolated_url
            .query_pairs_mut()
            .append_pair("options", &format!("-csearch_path={schema}"));
        let pool = PgPoolOptions::new()
            .max_connections(8)
            .connect(isolated_url.as_str())
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        (pool, admin, schema)
    }

    async fn cleanup(pool: PgPool, admin: PgPool, schema: String) {
        pool.close().await;
        sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
            .execute(&admin)
            .await
            .unwrap();
        admin.close().await;
    }

    fn role_url(base: &str, role: &str, password: &str, schema: &str) -> String {
        let mut url: url::Url = base.parse().unwrap();
        url.set_username(role).unwrap();
        url.set_password(Some(password)).unwrap();
        url.query_pairs_mut()
            .append_pair("options", &format!("-csearch_path={schema}"));
        url.to_string()
    }

    async fn create_device(pool: &PgPool) -> (Uuid, Uuid) {
        let vault = Uuid::new_v4();
        let profile = serde_json::json!({
            "crypto_suite": 1,
            "salt": (0_u8..16).collect::<Vec<_>>(),
            "vault_id": vault,
            "key_epoch": 1,
        });
        let mut digest = Sha256::new();
        digest.update(b"peppy-key-profile-v1\0");
        digest.update(1_u16.to_be_bytes());
        digest.update((0_u8..16).collect::<Vec<_>>());
        digest.update(vault.as_bytes());
        digest.update(1_u32.to_be_bytes());
        let owner = crate::api::create_owner(
            pool,
            profile,
            vec![7, 8, 9],
            hex::encode(digest.finalize()),
            1,
        )
        .await
        .unwrap();
        (owner.vault_id, owner.device_id)
    }

    async fn allocate_session(pool: &PgPool, vault: Uuid, device: Uuid) -> i64 {
        let mut tx = crate::scope::begin(pool, vault).await.unwrap();
        sqlx::query("SELECT 1 FROM devices WHERE vault_id=$1 AND device_id=$2 FOR UPDATE")
            .bind(vault)
            .bind(device)
            .fetch_one(&mut *tx)
            .await
            .unwrap();
        let value = allocate_session_incarnation(&mut tx, vault, device)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        value
    }

    #[tokio::test]
    async fn concurrent_fenced_allocations_are_unique_and_monotonic() {
        let (pool, admin, schema) = test_pool().await;
        let (vault, device) = create_device(&pool).await;
        let (first, second) = tokio::join!(
            allocate_session(&pool, vault, device),
            allocate_session(&pool, vault, device)
        );
        assert_eq!(
            [first, second]
                .into_iter()
                .collect::<std::collections::BTreeSet<_>>(),
            [1, 2].into_iter().collect()
        );
        cleanup(pool, admin, schema).await;
    }

    #[tokio::test]
    async fn exhaustion_does_not_wrap_or_mutate() {
        let (pool, admin, schema) = test_pool().await;
        let (vault, device) = create_device(&pool).await;
        sqlx::query("INSERT INTO live_ticket_device_authority VALUES($1,$2,$3,0)")
            .bind(vault)
            .bind(device)
            .bind(i64::MAX - 1)
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(allocate_session(&pool, vault, device).await, i64::MAX);
        let mut tx = crate::scope::begin(&pool, vault).await.unwrap();
        sqlx::query("SELECT 1 FROM devices WHERE vault_id=$1 AND device_id=$2 FOR UPDATE")
            .bind(vault)
            .bind(device)
            .fetch_one(&mut *tx)
            .await
            .unwrap();
        assert!(matches!(
            allocate_session_incarnation(&mut tx, vault, device).await,
            Err(AllocationError::Exhausted)
        ));
        tx.commit().await.unwrap();
        let highwater: i64 = sqlx::query_scalar("SELECT next_session_incarnation FROM live_ticket_device_authority WHERE vault_id=$1 AND device_id=$2").bind(vault).bind(device).fetch_one(&pool).await.unwrap();
        assert_eq!(highwater, i64::MAX);
        cleanup(pool, admin, schema).await;
    }

    #[tokio::test]
    async fn rollback_does_not_advance_committed_highwater() {
        let (pool, admin, schema) = test_pool().await;
        let (vault, device) = create_device(&pool).await;
        let mut tx = crate::scope::begin(&pool, vault).await.unwrap();
        sqlx::query("SELECT 1 FROM devices WHERE vault_id=$1 AND device_id=$2 FOR UPDATE")
            .bind(vault)
            .bind(device)
            .fetch_one(&mut *tx)
            .await
            .unwrap();
        assert_eq!(
            allocate_session_incarnation(&mut tx, vault, device)
                .await
                .unwrap(),
            1
        );
        tx.rollback().await.unwrap();
        assert_eq!(allocate_session(&pool, vault, device).await, 1);
        cleanup(pool, admin, schema).await;
    }

    #[tokio::test]
    async fn counters_are_tenant_scoped_and_survive_ticket_reaping() {
        let (pool, admin, schema) = test_pool().await;
        let (first_vault, first_device) = create_device(&pool).await;
        let (second_vault, second_device) = create_device(&pool).await;
        assert_eq!(allocate_session(&pool, first_vault, first_device).await, 1);
        assert_eq!(
            allocate_session(&pool, second_vault, second_device).await,
            1
        );
        sqlx::query("INSERT INTO live_ticket_sessions(vault_id,device_id,session_id_hash,state,closed_at) VALUES($1,$2,$3,'terminal',now()-interval '6 minutes')").bind(first_vault).bind(first_device).bind(vec![1_u8; 32]).execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO live_tickets(ticket_digest,vault_id,device_id,session_id_hash,issued_at,expires_at,consumed_at) VALUES($1,$2,$3,$4,now()-interval '2 minutes',now()-interval '1 minute',now())").bind(vec![2_u8; 32]).bind(first_vault).bind(first_device).bind(vec![1_u8; 32]).execute(&pool).await.unwrap();
        crate::live_ticket_maintenance::reap_live_tickets(&pool, 10)
            .await
            .unwrap();
        assert_eq!(allocate_session(&pool, first_vault, first_device).await, 2);
        cleanup(pool, admin, schema).await;
    }

    #[tokio::test]
    async fn connection_ids_are_independent_and_monotonic() {
        let (pool, admin, schema) = test_pool().await;
        let (vault, device) = create_device(&pool).await;
        let mut tx = crate::scope::begin(&pool, vault).await.unwrap();
        sqlx::query("SELECT 1 FROM devices WHERE vault_id=$1 AND device_id=$2 FOR UPDATE")
            .bind(vault)
            .bind(device)
            .fetch_one(&mut *tx)
            .await
            .unwrap();
        assert_eq!(
            allocate_connection_id(&mut tx, vault, device)
                .await
                .unwrap(),
            1
        );
        tx.commit().await.unwrap();
        cleanup(pool, admin, schema).await;
    }

    #[tokio::test]
    async fn consumed_ticket_reaping_preserves_the_session_connection_fence() {
        let (pool, admin, schema) = test_pool().await;
        let (vault, device) = create_device(&pool).await;
        sqlx::query("INSERT INTO live_ticket_sessions(vault_id,device_id,session_id_hash,active_connection_id,state,lease_until) VALUES($1,$2,$3,7,'open',now()+interval '1 hour')")
            .bind(vault).bind(device).bind(vec![3_u8; 32]).execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO live_tickets(ticket_digest,vault_id,device_id,session_id_hash,issued_at,expires_at,consumed_at) VALUES($1,$2,$3,$4,now()-interval '2 minutes',now()-interval '1 minute',now())")
            .bind(vec![4_u8; 32]).bind(vault).bind(device).bind(vec![3_u8; 32]).execute(&pool).await.unwrap();
        crate::live_ticket_maintenance::reap_live_tickets(&pool, 10)
            .await
            .unwrap();
        let connection: Option<i64> = sqlx::query_scalar("SELECT active_connection_id FROM live_ticket_sessions WHERE vault_id=$1 AND device_id=$2 AND session_id_hash=$3")
            .bind(vault).bind(device).bind(vec![3_u8; 32]).fetch_one(&pool).await.unwrap();
        assert_eq!(connection, Some(7));
        cleanup(pool, admin, schema).await;
    }

    #[tokio::test]
    async fn runtime_role_cannot_read_or_allocate_another_vault() {
        let database_url = std::env::var("TEST_DATABASE_URL").unwrap();
        let (pool, admin, schema) = test_pool().await;
        let (first_vault, first_device) = create_device(&pool).await;
        let (second_vault, second_device) = create_device(&pool).await;
        let suffix = Uuid::new_v4().simple();
        let role = format!("ticket_r_{suffix}");
        let password = format!("ticket_p_{suffix}");
        sqlx::query(&format!(
            "CREATE ROLE {role} LOGIN PASSWORD '{password}' NOSUPERUSER NOBYPASSRLS NOCREATEROLE"
        ))
        .execute(&admin)
        .await
        .unwrap();
        crate::rls::grant_runtime_privileges(&pool, &role)
            .await
            .unwrap();
        let runtime = PgPoolOptions::new()
            .max_connections(1)
            .connect(&role_url(&database_url, &role, &password, &schema))
            .await
            .unwrap();

        let mut first = crate::scope::begin(&runtime, first_vault).await.unwrap();
        let visible: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM live_ticket_device_authority WHERE vault_id=$1",
        )
        .bind(second_vault)
        .fetch_one(&mut *first)
        .await
        .unwrap();
        assert_eq!(visible, 0);
        sqlx::query("SELECT 1 FROM devices WHERE vault_id=$1 AND device_id=$2 FOR UPDATE")
            .bind(first_vault)
            .bind(first_device)
            .fetch_one(&mut *first)
            .await
            .unwrap();
        assert_eq!(
            allocate_session_incarnation(&mut first, first_vault, first_device)
                .await
                .unwrap(),
            1
        );
        first.commit().await.unwrap();

        let mut second = crate::scope::begin(&runtime, first_vault).await.unwrap();
        assert!(
            allocate_session_incarnation(&mut second, second_vault, second_device)
                .await
                .is_err()
        );
        second.rollback().await.unwrap();
        runtime.close().await;
        pool.close().await;
        sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
            .execute(&admin)
            .await
            .unwrap();
        sqlx::query(&format!("DROP ROLE {role}"))
            .execute(&admin)
            .await
            .unwrap();
        admin.close().await;
    }
}
