use sqlx::PgPool;

pub(crate) const LIVE_TICKET_REAP_BATCH: i64 = 1_000;
const REAP_TICKETS_SQL: &str = "WITH expired AS MATERIALIZED (SELECT ctid FROM live_tickets WHERE expires_at<=now() ORDER BY expires_at LIMIT $1 FOR UPDATE SKIP LOCKED), consumed AS MATERIALIZED (SELECT ctid FROM live_tickets WHERE consumed_at IS NOT NULL AND expires_at>now() ORDER BY consumed_at LIMIT GREATEST($1-(SELECT count(*) FROM expired), 0) FOR UPDATE SKIP LOCKED) DELETE FROM live_tickets WHERE ctid = ANY(ARRAY(SELECT ctid FROM expired UNION ALL SELECT ctid FROM consumed))";
const REAP_ADMISSION_EVENTS_SQL: &str = "WITH expired AS MATERIALIZED (SELECT ctid FROM live_ticket_admission_events WHERE created_at<=now()-interval '60 seconds' ORDER BY created_at LIMIT $1 FOR UPDATE SKIP LOCKED) DELETE FROM live_ticket_admission_events WHERE ctid = ANY(ARRAY(SELECT ctid FROM expired))";

/// Reclaims one bounded batch from each live-ticket retention category.
pub(crate) async fn reap_live_tickets(db: &PgPool, batch: i64) -> Result<u64, sqlx::Error> {
    let terminalized = sqlx::query("UPDATE live_ticket_sessions SET state='terminal', closed_at=now(), lease_until=NULL WHERE ctid IN (SELECT ctid FROM live_ticket_sessions WHERE state='open' AND lease_until<=now() ORDER BY lease_until LIMIT $1 FOR UPDATE SKIP LOCKED)")
        .bind(batch)
        .execute(db)
        .await?
        .rows_affected();
    let tickets = sqlx::query(REAP_TICKETS_SQL)
        .bind(batch)
        .execute(db)
        .await?
        .rows_affected();
    let tombstones = sqlx::query("DELETE FROM live_ticket_sessions WHERE ctid IN (SELECT ctid FROM live_ticket_sessions WHERE state IN ('closed','terminal') AND closed_at<now()-interval '5 minutes' ORDER BY closed_at LIMIT $1 FOR UPDATE SKIP LOCKED)")
        .bind(batch)
        .execute(db)
        .await?
        .rows_affected();
    let admission_events = sqlx::query(REAP_ADMISSION_EVENTS_SQL)
        .bind(batch)
        .execute(db)
        .await?
        .rows_affected();
    Ok(terminalized + tickets + tombstones + admission_events)
}

#[cfg(test)]
mod tests {
    use super::{REAP_ADMISSION_EVENTS_SQL, REAP_TICKETS_SQL, reap_live_tickets};
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

    fn includes_index(plan: &serde_json::Value, index: &str) -> bool {
        match plan {
            serde_json::Value::Object(fields) => {
                fields.get("Index Name").and_then(serde_json::Value::as_str) == Some(index)
                    || fields.values().any(|value| includes_index(value, index))
            }
            serde_json::Value::Array(values) => {
                values.iter().any(|value| includes_index(value, index))
            }
            _ => false,
        }
    }

    fn includes_relation_node(plan: &serde_json::Value, relation: &str, node_type: &str) -> bool {
        match plan {
            serde_json::Value::Object(fields) => {
                fields
                    .get("Relation Name")
                    .and_then(serde_json::Value::as_str)
                    == Some(relation)
                    && fields.get("Node Type").and_then(serde_json::Value::as_str)
                        == Some(node_type)
                    || fields
                        .values()
                        .any(|value| includes_relation_node(value, relation, node_type))
            }
            serde_json::Value::Array(values) => values
                .iter()
                .any(|value| includes_relation_node(value, relation, node_type)),
            _ => false,
        }
    }

    async fn assert_reap_ticket_plan_is_indexed(pool: &PgPool) {
        let plan: serde_json::Value =
            sqlx::query_scalar(&format!("EXPLAIN (FORMAT JSON) {REAP_TICKETS_SQL}"))
                .bind(1_000_i64)
                .fetch_one(pool)
                .await
                .unwrap();
        assert!(
            includes_index(&plan, "live_tickets_expiry_reap_idx"),
            "exact ticket reaper plan did not use live_tickets_expiry_reap_idx: {plan}"
        );
        assert!(
            includes_index(&plan, "live_tickets_consumed_reap_idx"),
            "exact ticket reaper plan did not use live_tickets_consumed_reap_idx: {plan}"
        );
        assert!(
            !includes_relation_node(&plan, "live_tickets", "Seq Scan"),
            "exact ticket reaper plan scans the live ticket population: {plan}"
        );
    }

    async fn assert_reap_admission_event_plan_is_indexed(pool: &PgPool) {
        let plan: serde_json::Value = sqlx::query_scalar(&format!(
            "EXPLAIN (FORMAT JSON) {REAP_ADMISSION_EVENTS_SQL}"
        ))
        .bind(1_000_i64)
        .fetch_one(pool)
        .await
        .unwrap();
        assert!(
            includes_index(&plan, "live_ticket_admission_events_created_at_idx"),
            "exact admission event reaper plan did not use the age index: {plan}"
        );
        assert!(
            !includes_relation_node(&plan, "live_ticket_admission_events", "Seq Scan"),
            "exact admission event reaper plan scans the admission event population: {plan}"
        );
    }

    #[tokio::test]
    async fn live_ticket_reaper_exact_delete_plan_uses_bounded_indexes() {
        let (pool, admin, schema) = test_pool().await;
        let (vault, device) = create_device(&pool).await;
        let session_hash = vec![1; 32];
        insert_session(
            &pool,
            vault,
            device,
            session_hash.clone(),
            "open",
            "now()+interval '1 hour'",
            "NULL",
        )
        .await;
        for (start, expires_at, consumed_at) in [
            (1_i64, "now()-interval '1 second'", "NULL"),
            (
                2_001,
                "now()+interval '1 hour'",
                "now()-interval '1 second'",
            ),
            (4_001, "now()+interval '1 hour'", "NULL"),
        ] {
            sqlx::query(&format!(
                "INSERT INTO live_tickets(ticket_digest, vault_id, device_id, session_id_hash, issued_at, expires_at, consumed_at) SELECT decode(lpad(to_hex(value), 64, '0'), 'hex'), $1, $2, $3, now()-interval '2 hours', {expires_at}, {consumed_at} FROM generate_series({start}, {start} + 1_999) AS value"
            ))
            .bind(vault)
            .bind(device)
            .bind(&session_hash)
            .execute(&pool)
            .await
            .unwrap();
        }
        for (state, start) in [("closed", 6_001), ("terminal", 8_001)] {
            sqlx::query(&format!(
                "INSERT INTO live_ticket_sessions(vault_id, device_id, session_id_hash, state, closed_at) SELECT $1, $2, decode(lpad(to_hex(value), 64, '0'), 'hex'), '{state}', now()-interval '6 minutes' FROM generate_series({start}, {start} + 1_999) AS value"
            ))
            .bind(vault)
            .bind(device)
            .execute(&pool)
            .await
            .unwrap();
        }

        sqlx::query("ANALYZE live_tickets")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("ANALYZE live_ticket_sessions")
            .execute(&pool)
            .await
            .unwrap();
        assert_reap_ticket_plan_is_indexed(&pool).await;
        let tombstone_plan: serde_json::Value = sqlx::query_scalar("EXPLAIN (FORMAT JSON) SELECT ctid FROM live_ticket_sessions WHERE state IN ('closed', 'terminal') AND closed_at<now()-interval '5 minutes' ORDER BY closed_at LIMIT 1_000 FOR UPDATE SKIP LOCKED")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert!(
            includes_index(&tombstone_plan, "live_ticket_sessions_tombstone_reap_idx"),
            "tombstone reaper plan did not use live_ticket_sessions_tombstone_reap_idx: {tombstone_plan}"
        );

        pool.close().await;
        sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
            .execute(&admin)
            .await
            .unwrap();
        admin.close().await;
    }

    #[tokio::test]
    async fn admission_event_reaper_uses_the_age_index_and_retains_the_full_window() {
        let (pool, admin, schema) = test_pool().await;
        let (vault, device) = create_device(&pool).await;
        sqlx::query("INSERT INTO live_ticket_admission_events(event_id, vault_id, device_id, event_kind, created_at) SELECT md5(value::text)::uuid, $1, $2, 'ticket_issue', now()-interval '2 minutes' FROM generate_series(1, 4_000) AS value")
            .bind(vault)
            .bind(device)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("ANALYZE live_ticket_admission_events")
            .execute(&pool)
            .await
            .unwrap();
        assert_reap_admission_event_plan_is_indexed(&pool).await;
        sqlx::query("DELETE FROM live_ticket_admission_events")
            .execute(&pool)
            .await
            .unwrap();

        let locked = Uuid::new_v4();
        let expired = Uuid::new_v4();
        let cutoff = Uuid::new_v4();
        let retained = Uuid::new_v4();
        sqlx::query("INSERT INTO live_ticket_admission_events(event_id, vault_id, device_id, event_kind, created_at) VALUES($1, $2, $3, 'session_create', now()-interval '2 minutes')")
            .bind(locked)
            .bind(vault)
            .bind(device)
            .execute(&pool)
            .await
            .unwrap();
        let mut held = pool.begin().await.unwrap();
        sqlx::query("SELECT ctid FROM live_ticket_admission_events WHERE event_id=$1 FOR UPDATE")
            .bind(locked)
            .fetch_one(&mut *held)
            .await
            .unwrap();

        let mut boundary = pool.begin().await.unwrap();
        for (event_id, age) in [
            (expired, "2 minutes"),
            (cutoff, "60 seconds"),
            (retained, "59 seconds"),
        ] {
            sqlx::query(&format!("INSERT INTO live_ticket_admission_events(event_id, vault_id, device_id, event_kind, created_at) VALUES($1, $2, $3, 'session_create', now()-interval '{age}')"))
                .bind(event_id)
                .bind(vault)
                .bind(device)
                .execute(&mut *boundary)
                .await
                .unwrap();
        }
        assert_eq!(
            sqlx::query(REAP_ADMISSION_EVENTS_SQL)
                .bind(2_i64)
                .execute(&mut *boundary)
                .await
                .unwrap()
                .rows_affected(),
            2
        );
        let mut remaining: Vec<Uuid> = sqlx::query_scalar(
            "SELECT event_id FROM live_ticket_admission_events WHERE event_id = ANY($1)",
        )
        .bind(vec![locked, expired, cutoff, retained])
        .fetch_all(&mut *boundary)
        .await
        .unwrap();
        remaining.sort();
        let mut expected = vec![locked, retained];
        expected.sort();
        assert_eq!(remaining, expected);
        boundary.commit().await.unwrap();
        held.rollback().await.unwrap();

        pool.close().await;
        sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
            .execute(&admin)
            .await
            .unwrap();
        admin.close().await;
    }

    #[tokio::test]
    async fn admission_events_outlive_ticket_and_session_reaping() {
        let (pool, admin, schema) = test_pool().await;
        let (vault, device) = create_device(&pool).await;
        let session_hash = vec![12_u8; 32];
        insert_session(
            &pool,
            vault,
            device,
            session_hash.clone(),
            "closed",
            "NULL",
            "now()-interval '6 minutes'",
        )
        .await;
        sqlx::query("INSERT INTO live_tickets(ticket_digest, vault_id, device_id, session_id_hash, issued_at, expires_at) VALUES($1, $2, $3, $4, now()-interval '2 minutes', now()-interval '1 minute')")
            .bind(vec![13_u8; 32])
            .bind(vault)
            .bind(device)
            .bind(&session_hash)
            .execute(&pool)
            .await
            .unwrap();
        let event_id = Uuid::new_v4();
        sqlx::query("INSERT INTO live_ticket_admission_events(event_id, vault_id, device_id, event_kind, created_at) VALUES($1, $2, $3, 'ticket_issue', now())")
            .bind(event_id)
            .bind(vault)
            .bind(device)
            .execute(&pool)
            .await
            .unwrap();

        assert_eq!(reap_live_tickets(&pool, 1).await.unwrap(), 2);
        let retained: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM live_ticket_admission_events WHERE event_id=$1",
        )
        .bind(event_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(retained, 1);

        pool.close().await;
        sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
            .execute(&admin)
            .await
            .unwrap();
        admin.close().await;
    }

    #[tokio::test]
    async fn live_ticket_reaper_skips_locked_expired_ticket_and_fills_the_remainder() {
        let (pool, admin, schema) = test_pool().await;
        let (vault, device) = create_device(&pool).await;
        let session_hash = vec![7; 32];
        insert_session(
            &pool,
            vault,
            device,
            session_hash.clone(),
            "open",
            "now()+interval '1 hour'",
            "NULL",
        )
        .await;
        let locked_expired = vec![8_u8; 32];
        let unlocked_expired = vec![9_u8; 32];
        let consumed = vec![10_u8; 32];
        let live = vec![11_u8; 32];
        for (digest, expires_at, consumed_at) in [
            (locked_expired.clone(), "now()-interval '1 second'", "NULL"),
            (
                unlocked_expired.clone(),
                "now()-interval '1 second'",
                "NULL",
            ),
            (
                consumed.clone(),
                "now()+interval '1 hour'",
                "now()-interval '1 second'",
            ),
            (live.clone(), "now()+interval '1 hour'", "NULL"),
        ] {
            sqlx::query(&format!(
                "INSERT INTO live_tickets(ticket_digest, vault_id, device_id, session_id_hash, issued_at, expires_at, consumed_at) VALUES($1, $2, $3, $4, now()-interval '2 minutes', {expires_at}, {consumed_at})"
            ))
            .bind(digest)
            .bind(vault)
            .bind(device)
            .bind(&session_hash)
            .execute(&pool)
            .await
            .unwrap();
        }
        let mut locked = pool.begin().await.unwrap();
        sqlx::query("SELECT ctid FROM live_tickets WHERE ticket_digest=$1 FOR UPDATE")
            .bind(&locked_expired)
            .fetch_one(&mut *locked)
            .await
            .unwrap();

        assert_eq!(reap_live_tickets(&pool, 2).await.unwrap(), 2);
        let remaining: Vec<Vec<u8>> =
            sqlx::query_scalar("SELECT ticket_digest FROM live_tickets ORDER BY ticket_digest")
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(remaining, vec![locked_expired.clone(), live]);
        locked.rollback().await.unwrap();

        pool.close().await;
        sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
            .execute(&admin)
            .await
            .unwrap();
        admin.close().await;
    }

    #[tokio::test]
    async fn live_ticket_reaper_keeps_ticket_deletion_within_one_combined_batch() {
        let (pool, admin, schema) = test_pool().await;
        let (vault, device) = create_device(&pool).await;
        let session_hash = vec![2; 32];
        insert_session(
            &pool,
            vault,
            device,
            session_hash.clone(),
            "open",
            "now()+interval '1 hour'",
            "NULL",
        )
        .await;
        for (digest, expires_at, consumed_at) in [
            (vec![3_u8; 32], "now()-interval '1 second'", "NULL"),
            (
                vec![4_u8; 32],
                "now()-interval '1 second'",
                "now()-interval '2 seconds'",
            ),
            (
                vec![5_u8; 32],
                "now()+interval '1 hour'",
                "now()-interval '1 second'",
            ),
            (vec![6_u8; 32], "now()+interval '1 hour'", "NULL"),
        ] {
            sqlx::query(&format!(
                "INSERT INTO live_tickets(ticket_digest, vault_id, device_id, session_id_hash, issued_at, expires_at, consumed_at) VALUES($1, $2, $3, $4, now()-interval '2 minutes', {expires_at}, {consumed_at})"
            ))
            .bind(digest)
            .bind(vault)
            .bind(device)
            .bind(&session_hash)
            .execute(&pool)
            .await
            .unwrap();
        }

        assert_eq!(reap_live_tickets(&pool, 2).await.unwrap(), 2);
        let remaining: (i64, i64, i64) = sqlx::query_as(
            "SELECT count(*), count(*) FILTER (WHERE consumed_at IS NOT NULL), count(*) FILTER (WHERE consumed_at IS NULL) FROM live_tickets",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(remaining, (2, 1, 1));
        assert_eq!(reap_live_tickets(&pool, 2).await.unwrap(), 1);
        let live: i64 = sqlx::query_scalar("SELECT count(*) FROM live_tickets")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(live, 1);

        pool.close().await;
        sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
            .execute(&admin)
            .await
            .unwrap();
        admin.close().await;
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
