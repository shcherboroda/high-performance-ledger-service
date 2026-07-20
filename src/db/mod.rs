use anyhow::{Context, Result};
use sqlx::{PgPool, postgres::PgPoolOptions};

use crate::config::PoolConfig;

pub async fn create_pool(database_url: &str, config: &PoolConfig) -> Result<PgPool> {
    let connect = PgPoolOptions::new()
        .max_connections(config.max_connections)
        .min_connections(config.min_connections)
        .acquire_timeout(config.acquire_timeout)
        .connect(database_url);
    tokio::time::timeout(config.connect_timeout, connect)
        .await
        .context("PostgreSQL connection attempt timed out")?
        .context("failed to establish PostgreSQL connectivity")
}
