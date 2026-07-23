#[cfg(not(unix))]
use std::future::pending;

use anyhow::Result;
use rust_backend_technical_assessment::{app, auth::AuthVerifier, config::Config, db};
use tracing::info;

#[tokio::main]
async fn main() {
    if run().await.is_err() {
        eprintln!("application failed to start");
        std::process::exit(1);
    }
}

async fn run() -> Result<()> {
    let config = Config::from_env()?;
    let _log_guard =
        rust_backend_technical_assessment::observability::init_logging(&config.log_filter)?;
    finish_post_logging_startup(start(config).await)
}

fn finish_post_logging_startup<T>(result: Result<T>) -> Result<T> {
    if result.is_err() {
        rust_backend_technical_assessment::observability::log_startup_failure();
    }
    result
}

async fn start(config: Config) -> Result<()> {
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

#[cfg(test)]
mod tests {
    use std::{
        io::{self, Write},
        sync::{Arc, Mutex},
    };

    use super::*;

    #[derive(Clone)]
    struct TestWriter(Arc<Mutex<Vec<u8>>>);

    impl Write for TestWriter {
        fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buffer);
            Ok(buffer.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn post_logging_startup_failures_emit_only_the_bounded_category() {
        let output = Arc::new(Mutex::new(Vec::new()));
        let writer = output.clone();
        let subscriber = tracing_subscriber::fmt()
            .json()
            .with_writer(move || TestWriter(writer.clone()))
            .finish();
        let secret = "postgres://ledger_user:ledger_password@db.example/ledger";
        let raw_error = "seeded startup database failure";

        tracing::subscriber::with_default(subscriber, || {
            let result: Result<()> = Err(anyhow::anyhow!("{secret} {raw_error}"));
            assert!(finish_post_logging_startup(result).is_err());
        });

        let logs = String::from_utf8(output.lock().unwrap().clone()).unwrap();
        assert!(logs.contains("application startup failed"));
        assert!(logs.contains("startup_failure"));
        assert!(!logs.contains(secret));
        assert!(!logs.contains(raw_error));
    }
}
