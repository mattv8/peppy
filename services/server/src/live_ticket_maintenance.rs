use sqlx::PgPool;

pub(crate) const LIVE_TICKET_REAP_BATCH: i64 = 1_000;

/// Reclaims one bounded batch from each live-ticket retention category.
pub(crate) async fn reap_live_tickets(db: &PgPool, batch: i64) -> Result<u64, sqlx::Error> {
    let terminalized = sqlx::query("UPDATE live_ticket_sessions SET state='terminal', closed_at=now(), lease_until=NULL WHERE ctid IN (SELECT ctid FROM live_ticket_sessions WHERE state='open' AND lease_until<=now() ORDER BY lease_until LIMIT $1 FOR UPDATE SKIP LOCKED)")
        .bind(batch)
        .execute(db)
        .await?
        .rows_affected();
    let tickets = sqlx::query("DELETE FROM live_tickets WHERE ctid IN (SELECT ctid FROM live_tickets WHERE expires_at<=now() OR consumed_at IS NOT NULL ORDER BY expires_at LIMIT $1 FOR UPDATE SKIP LOCKED)")
        .bind(batch)
        .execute(db)
        .await?
        .rows_affected();
    let tombstones = sqlx::query("DELETE FROM live_ticket_sessions WHERE ctid IN (SELECT ctid FROM live_ticket_sessions WHERE state IN ('closed','terminal') AND closed_at<now()-interval '5 minutes' ORDER BY closed_at LIMIT $1 FOR UPDATE SKIP LOCKED)")
        .bind(batch)
        .execute(db)
        .await?
        .rows_affected();
    Ok(terminalized + tickets + tombstones)
}

#[cfg(test)]
mod tests {
    use super::reap_live_tickets;
    use sha2::{Digest, Sha256};
    use sqlx::{PgPool, postgres::PgPoolOptions};
    use uuid::Uuid;

    async fn test_pool() -> (PgPool, PgPool, String) {
        let database_url = std::env::var("TEST_DATABASE_URL")
            .expect("TEST_DATABASE_URL is required; live ticket tests never silently skip");
        let admin = PgPoolOptions::new()
            .max_connections(2)
            .connect(&database_url)
            .await
            .unwrap();
        let schema = format!("peppy_live_ticket_reaper_{}", Uuid::new_v4().simple());
        sqlx::query(&format!("CREATE SCHEMA {schema}"))
            .execute(&admin)
            .await
            .unwrap();
        let mut isolated_url: url::Url = database_url.parse().unwrap();
        isolated_url
            .query_pairs_mut()
            .append_pair("options", &format!("-csearch_path={schema}"));
        let pool = PgPoolOptions::new()
            .max_connections(2)
            .connect(isolated_url.as_str())
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        (pool, admin, schema)
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

    async fn insert_session(
        pool: &PgPool,
        vault: Uuid,
        device: Uuid,
        session_hash: Vec<u8>,
        state: &str,
        lease_until: &str,
        closed_at: &str,
    ) {
        sqlx::query(&format!(
            "INSERT INTO live_ticket_sessions(vault_id, device_id, session_id_hash, state, lease_until, closed_at) VALUES($1, $2, $3, $4, {lease_until}, {closed_at})"
        ))
        .bind(vault)
        .bind(device)
        .bind(session_hash)
        .bind(state)
        .execute(pool)
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn live_ticket_reaper_limits_each_category_to_one_batch_and_drains_it() {
        let (pool, admin, schema) = test_pool().await;
        let (vault, device) = create_device(&pool).await;
        let batch = 2;
        let eligible = batch + 1;

        for value in 0..eligible {
            insert_session(
                &pool,
                vault,
                device,
                vec![value as u8; 32],
                "open",
                "now()-interval '1 second'",
                "NULL",
            )
            .await;
            insert_session(
                &pool,
                vault,
                device,
                vec![32 + value as u8; 32],
                "open",
                "now()+interval '1 hour'",
                "NULL",
            )
            .await;
            sqlx::query("INSERT INTO live_tickets(ticket_digest, vault_id, device_id, session_id_hash, issued_at, expires_at) VALUES($1, $2, $3, $4, now()-interval '2 seconds', now()-interval '1 second')")
                .bind(vec![64 + value as u8; 32])
                .bind(vault)
                .bind(device)
                .bind(vec![32 + value as u8; 32])
                .execute(&pool)
                .await
                .unwrap();
            insert_session(
                &pool,
                vault,
                device,
                vec![96 + value as u8; 32],
                "closed",
                "NULL",
                "now()-interval '6 minutes'",
            )
            .await;
        }

        let live_session = vec![128; 32];
        insert_session(
            &pool,
            vault,
            device,
            live_session.clone(),
            "open",
            "now()+interval '1 hour'",
            "NULL",
        )
        .await;
        let recent_tombstone = vec![129; 32];
        insert_session(
            &pool,
            vault,
            device,
            recent_tombstone.clone(),
            "closed",
            "NULL",
            "now()-interval '1 minute'",
        )
        .await;

        assert_eq!(
            reap_live_tickets(&pool, batch).await.unwrap(),
            batch as u64 * 3
        );
        let terminalized: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM live_ticket_sessions WHERE state='terminal' AND closed_at>now()-interval '1 minute'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        let expired_tickets: i64 =
            sqlx::query_scalar("SELECT count(*) FROM live_tickets WHERE expires_at<=now()")
                .fetch_one(&pool)
                .await
                .unwrap();
        let old_tombstones: i64 = sqlx::query_scalar("SELECT count(*) FROM live_ticket_sessions WHERE state='closed' AND closed_at<now()-interval '5 minutes'")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(terminalized, batch);
        assert_eq!(expired_tickets, 1);
        assert_eq!(old_tombstones, 1);

        assert_eq!(reap_live_tickets(&pool, batch).await.unwrap(), 3);
        for query in [
            "SELECT count(*) FROM live_ticket_sessions WHERE state='open' AND lease_until<=now()",
            "SELECT count(*) FROM live_tickets WHERE expires_at<=now()",
            "SELECT count(*) FROM live_ticket_sessions WHERE state='closed' AND closed_at<now()-interval '5 minutes'",
        ] {
            let remaining: i64 = sqlx::query_scalar(query).fetch_one(&pool).await.unwrap();
            assert_eq!(remaining, 0);
        }
        for session_hash in [live_session, recent_tombstone] {
            let retained: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM live_ticket_sessions WHERE session_id_hash=$1",
            )
            .bind(session_hash)
            .fetch_one(&pool)
            .await
            .unwrap();
            assert_eq!(retained, 1);
        }

        pool.close().await;
        sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
            .execute(&admin)
            .await
            .unwrap();
        admin.close().await;
    }
}
