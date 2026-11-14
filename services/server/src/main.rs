use std::net::SocketAddr;

use peppy_server::{ServerBuilder, config::Config};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    tracing_subscriber::fmt()
        .with_env_filter("peppy_server=info")
        .init();
    let command = std::env::args().nth(1);
    let config = Config::from_env()?;
    if command.as_deref() == Some("migrate") {
        let rls = standalone_rls_config(true)?;
        let migration_db = sqlx::PgPool::connect(
            rls.migration_database_url
                .as_deref()
                .expect("standalone migration config was validated"),
        )
        .await?;
        let runtime_db = sqlx::PgPool::connect(&config.database_url).await?;
        let maintenance_db = sqlx::PgPool::connect(&rls.maintenance_database_url).await?;
        let migration_role: String = sqlx::query_scalar("SELECT current_user")
            .fetch_one(&migration_db)
            .await?;
        let runtime_role: String = sqlx::query_scalar("SELECT current_user")
            .fetch_one(&runtime_db)
            .await?;
        let maintenance_role: String = sqlx::query_scalar("SELECT current_user")
            .fetch_one(&maintenance_db)
            .await?;
        ensure_distinct_database_roles(&migration_role, &runtime_role, &maintenance_role)?;
        sqlx::migrate!().run(&migration_db).await?;
        peppy_server::rls::grant_runtime_privileges(&migration_db, &runtime_role).await?;
        peppy_server::rls::grant_maintenance_privileges(&migration_db, &maintenance_role).await?;
        return Ok(());
    }
    if command.as_deref() == Some("storage-check") {
        return peppy_server::health::storage_contract_check(&config).await;
    }
    if command.as_deref() == Some("healthcheck") {
        return healthcheck(config.bind_addr).await;
    }
    let rls = standalone_rls_config(false)?;
    let bind_addr = config.bind_addr;
    let web_client = config.web_client.clone();
    let runtime_db = sqlx::PgPool::connect(&config.database_url).await?;
    let maintenance_db = sqlx::PgPool::connect(&rls.maintenance_database_url).await?;
    let mut server = ServerBuilder::new(config, runtime_db)
        .maintenance_database(maintenance_db)
        .rls_enforced(true)
        .build()
        .await?;
    server.start_maintenance()?;
    let listener = tokio::net::TcpListener::bind(bind_addr).await?;
    tracing::info!(address = %bind_addr, "peppy server listening");
    let router = match web_client {
        Some(config) => peppy_server::web_client::wrap(server.router(), config),
        None => server.router(),
    };
    peppy_server::serve(listener, router, shutdown_signal()).await?;
    Ok(())
}

struct StandaloneRlsConfig {
    migration_database_url: Option<String>,
    maintenance_database_url: String,
}

fn standalone_rls_config(
    migration: bool,
) -> Result<StandaloneRlsConfig, Box<dyn std::error::Error + Send + Sync>> {
    standalone_rls_config_from(|name| std::env::var(name).ok(), migration)
}

fn standalone_rls_config_from(
    get: impl Fn(&str) -> Option<String>,
    migration: bool,
) -> Result<StandaloneRlsConfig, Box<dyn std::error::Error + Send + Sync>> {
    let maintenance_database_url = get("MAINTENANCE_DATABASE_URL")
        .filter(|value| !value.is_empty())
        .ok_or("MAINTENANCE_DATABASE_URL is required for the standalone server")?;
    let migration_database_url = get("DATABASE_MIGRATION_URL").filter(|value| !value.is_empty());
    if migration && migration_database_url.is_none() {
        return Err("DATABASE_MIGRATION_URL is required for migrate".into());
    }
    Ok(StandaloneRlsConfig {
        migration_database_url,
        maintenance_database_url,
    })
}

fn ensure_distinct_database_roles(
    migration_role: &str,
    runtime_role: &str,
    maintenance_role: &str,
) -> Result<(), std::io::Error> {
    if migration_role == runtime_role
        || migration_role == maintenance_role
        || runtime_role == maintenance_role
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "migration, runtime, and maintenance database roles must be distinct",
        ));
    }
    Ok(())
}

async fn healthcheck(
    bind_addr: SocketAddr,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let url = format!("http://127.0.0.1:{}/healthz", bind_addr.port());
    reqwest::get(url).await?.error_for_status()?;
    Ok(())
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .expect("SIGTERM handler can be installed");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {},
            _ = terminate.recv() => {},
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

#[cfg(test)]
mod tests {
    use super::{ensure_distinct_database_roles, standalone_rls_config_from};

    #[test]
    fn standalone_requires_separated_database_urls() {
        assert!(standalone_rls_config_from(|_| None, false).is_err());
        let serve = standalone_rls_config_from(
            |name| match name {
                "MAINTENANCE_DATABASE_URL" => Some("postgres://maintenance/db".into()),
                _ => None,
            },
            false,
        );
        assert!(serve.is_ok());
        assert!(
            standalone_rls_config_from(
                |name| match name {
                    "MAINTENANCE_DATABASE_URL" => Some("postgres://maintenance/db".into()),
                    _ => None,
                },
                true,
            )
            .is_err()
        );
        assert!(
            standalone_rls_config_from(
                |name| match name {
                    "MAINTENANCE_DATABASE_URL" => Some("postgres://maintenance/db".into()),
                    "DATABASE_MIGRATION_URL" => Some("postgres://migration/db".into()),
                    _ => None,
                },
                true,
            )
            .is_ok()
        );
    }

    #[test]
    fn standalone_migration_rejects_reused_database_roles() {
        assert!(ensure_distinct_database_roles("owner", "runtime", "maintenance").is_ok());
        assert!(ensure_distinct_database_roles("owner", "owner", "maintenance").is_err());
        assert!(ensure_distinct_database_roles("owner", "runtime", "runtime").is_err());
    }
}
