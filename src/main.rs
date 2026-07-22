#[cfg(not(unix))]
use std::future::pending;

use anyhow::Result;
use rust_backend_technical_assessment::{app, auth::AuthVerifier, config::Config, db};
use tracing::{error, info};

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("application failed to start: {error}");
        error!(error = %error, "application startup failed");
        std::process::exit(1);
    }
}

async fn run() -> Result<()> {
    let config = Config::from_env()?;
    let _log_guard =
        rust_backend_technical_assessment::observability::init_logging(&config.log_filter)?;
    let auth = AuthVerifier::new(&config.auth)?;

    let pool = db::create_pool(&config.database_url, &config.pool).await?;
    let listener = tokio::net::TcpListener::bind(config.bind_address).await?;
    info!(address = %config.bind_address, "server listening");
    axum::serve(
        listener,
        app::router_with_idempotency_retention(pool, auth, config.idempotency_retention),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await?;
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler")
    };
    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };
    #[cfg(not(unix))]
    let terminate = pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
}
