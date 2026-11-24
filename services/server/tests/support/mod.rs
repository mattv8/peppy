// Integration test binaries include only the helpers they need.
#![allow(dead_code)]

use std::{sync::mpsc, thread, time::Duration};

use sqlx::{Connection, PgConnection, PgPool, postgres::PgPoolOptions};
use url::Url;
use uuid::Uuid;

pub struct SchemaFixture {
    pub admin: PgPool,
    pub pool: Option<PgPool>,
    pub schema: String,
    pub url: Url,
    database_url: String,
    retry_deadlock: bool,
    cleaned: bool,
}

impl SchemaFixture {
    pub async fn new(
        database_url: &str,
        prefix: &str,
        admin_connections: u32,
        pool_connections: u32,
        retry_deadlock: bool,
    ) -> Self {
        let admin = PgPoolOptions::new()
            .max_connections(admin_connections)
            .connect(database_url)
            .await
            .unwrap();
        let schema = format!("{prefix}{}", Uuid::new_v4().simple());
        let mut url: Url = database_url.parse().unwrap();
        url.query_pairs_mut().append_pair(
            "options",
            &format!("-csearch_path={schema} -capplication_name={schema}"),
        );
        sqlx::query(&format!("CREATE SCHEMA {schema}"))
            .execute(&admin)
            .await
            .unwrap();
        let mut fixture = Self {
            admin,
            pool: None,
            schema,
            url,
            database_url: database_url.to_owned(),
            retry_deadlock,
            cleaned: false,
        };
        fixture.pool = Some(
            PgPoolOptions::new()
                .max_connections(pool_connections)
                .connect(fixture.url.as_str())
                .await
                .unwrap(),
        );
        peppy_server::MIGRATOR.run(fixture.pool()).await.unwrap();
        fixture
    }

    pub fn pool(&self) -> &PgPool {
        self.pool.as_ref().expect("schema pool initialized")
    }

    pub async fn cleanup(mut self) {
        self.pool().close().await;
        let drop_schema = format!("DROP SCHEMA {} CASCADE", self.schema);
        let first = sqlx::query(&drop_schema).execute(&self.admin).await;
        if self.retry_deadlock
            && matches!(
                &first,
                Err(sqlx::Error::Database(error)) if error.code().as_deref() == Some("40P01")
            )
        {
            sqlx::query(&drop_schema)
                .execute(&self.admin)
                .await
                .unwrap();
        } else {
            first.unwrap();
        }
        self.cleaned = true;
        self.admin.close().await;
    }
}

impl Drop for SchemaFixture {
    fn drop(&mut self) {
        if self.cleaned {
            return;
        }
        let database_url = self.database_url.clone();
        let schema = self.schema.clone();
        let (tx, rx) = mpsc::channel();
        let spawned = thread::Builder::new().spawn(move || {
            let result = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|error| error.to_string())
                .and_then(|runtime| {
                    runtime.block_on(async {
                        let mut conn = PgConnection::connect(&database_url)
                            .await
                            .map_err(|error| error.to_string())?;
                        sqlx::query("SET lock_timeout = '5s'")
                            .execute(&mut conn)
                            .await
                            .map_err(|error| error.to_string())?;
                        let terminated: Vec<bool> = sqlx::query_scalar("SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE application_name = $1 AND pid <> pg_backend_pid()")
                            .bind(&schema)
                            .fetch_all(&mut conn)
                            .await
                            .map_err(|error| error.to_string())?;
                        if terminated.contains(&false) {
                            return Err("failed to terminate a fixture backend".to_owned());
                        }
                        sqlx::query(&format!("DROP SCHEMA IF EXISTS \"{schema}\" CASCADE"))
                            .execute(&mut conn)
                            .await
                            .map_err(|error| error.to_string())?;
                        Ok(())
                    })
                });
            let _ = tx.send(result);
        });
        let result = match spawned {
            Ok(_) => rx
                .recv_timeout(Duration::from_secs(15))
                .map_err(|error| error.to_string())
                .and_then(|result| result),
            Err(error) => Err(error.to_string()),
        };
        if let Err(error) = result {
            eprintln!("failed to clean schema {}: {error}", self.schema);
        }
    }
}

pub async fn wait_blocked_by(
    pool: &PgPool,
    holder_pid: i32,
    statement_hint: Option<&str>,
    timeout: Duration,
) {
    let wait = async {
        loop {
            let blocked: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE $1 = ANY(pg_blocking_pids(pid)) AND ($2::text IS NULL OR position($2 in query) > 0))",
            )
            .bind(holder_pid)
            .bind(statement_hint)
            .fetch_one(pool)
            .await
            .expect("failed to inspect blocked PostgreSQL backends");
            if blocked {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    };
    tokio::time::timeout(timeout, wait).await.unwrap_or_else(|_| {
        panic!("timed out after {timeout:?} waiting for a backend blocked by {holder_pid}, statement hint {statement_hint:?}")
    });
}
