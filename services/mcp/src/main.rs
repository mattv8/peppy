use peppy_mcp::{config::WorkerConfig, http::WorkerHttp, registry::StateLock};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter("peppy_mcp=info")
        .init();
    let config = WorkerConfig::from_env()?;
    let bind_addr = config.bind_addr;
    // Keep ownership through shutdown, including control-token bootstrap in new().
    let _state_lock = StateLock::acquire(&config.state_dir)?;
    let worker = WorkerHttp::new(config)?;
    worker.start_sync().await;
    let listener = tokio::net::TcpListener::bind(bind_addr).await?;
    axum::serve(listener, worker.clone().router())
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    worker.shutdown().await;
    Ok(())
}
