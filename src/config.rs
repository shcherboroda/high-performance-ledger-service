use std::{collections::HashMap, net::SocketAddr, time::Duration};

use anyhow::{Context, Result, bail};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub database_url: String,
    pub bind_address: SocketAddr,
    pub pool: PoolConfig,
    pub log_filter: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PoolConfig {
    pub max_connections: u32,
    pub min_connections: u32,
    pub acquire_timeout: Duration,
    pub connect_timeout: Duration,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        dotenvy::dotenv().ok();
        Self::from_values(std::env::vars())
    }

    pub fn from_values(values: impl IntoIterator<Item = (String, String)>) -> Result<Self> {
        let values = values.into_iter().collect::<HashMap<_, _>>();
        let database_url = required(&values, "DATABASE_URL")?;
        let bind_address = optional(&values, "BIND_ADDRESS", "0.0.0.0:3000")
            .parse()
            .context("BIND_ADDRESS must be a valid socket address")?;
        let max_connections = parse_u32(&values, "DB_MAX_CONNECTIONS", 10)?;
        let min_connections = parse_u32(&values, "DB_MIN_CONNECTIONS", 0)?;
        if min_connections > max_connections {
            bail!("DB_MIN_CONNECTIONS must not exceed DB_MAX_CONNECTIONS");
        }

        Ok(Self {
            database_url,
            bind_address,
            pool: PoolConfig {
                max_connections,
                min_connections,
                acquire_timeout: parse_seconds(&values, "DB_ACQUIRE_TIMEOUT_SECS", 5)?,
                connect_timeout: parse_seconds(&values, "DB_CONNECT_TIMEOUT_SECS", 5)?,
            },
            log_filter: optional(&values, "RUST_LOG", "info").to_owned(),
        })
    }
}

fn required(values: &HashMap<String, String>, name: &str) -> Result<String> {
    values
        .get(name)
        .filter(|value| !value.trim().is_empty())
        .cloned()
        .with_context(|| format!("{name} must be set"))
}

fn optional<'a>(values: &'a HashMap<String, String>, name: &str, default: &'a str) -> &'a str {
    values.get(name).map_or(default, String::as_str)
}

fn parse_u32(values: &HashMap<String, String>, name: &str, default: u32) -> Result<u32> {
    optional(values, name, &default.to_string())
        .parse()
        .with_context(|| format!("{name} must be a non-negative integer"))
}

fn parse_seconds(values: &HashMap<String, String>, name: &str, default: u64) -> Result<Duration> {
    let seconds: u64 = optional(values, name, &default.to_string())
        .parse()
        .with_context(|| format!("{name} must be a positive number of seconds"))?;
    if seconds == 0 {
        bail!("{name} must be greater than zero");
    }
    Ok(Duration::from_secs(seconds))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn values(items: &[(&str, &str)]) -> Vec<(String, String)> {
        items
            .iter()
            .map(|(key, value)| ((*key).into(), (*value).into()))
            .collect()
    }

    #[test]
    fn parses_defaults() {
        let config = Config::from_values(values(&[(
            "DATABASE_URL",
            "postgres://secret@localhost/ledger",
        )]))
        .expect("configuration should parse");
        assert_eq!(config.bind_address, "0.0.0.0:3000".parse().unwrap());
        assert_eq!(config.pool.max_connections, 10);
        assert_eq!(config.pool.min_connections, 0);
        assert_eq!(config.pool.acquire_timeout, Duration::from_secs(5));
    }

    #[test]
    fn rejects_invalid_values_without_disclosing_database_url() {
        let error = Config::from_values(values(&[
            ("DATABASE_URL", "postgres://user:password@localhost/ledger"),
            ("DB_MAX_CONNECTIONS", "many"),
        ]))
        .unwrap_err()
        .to_string();
        assert!(error.contains("DB_MAX_CONNECTIONS"));
        assert!(!error.contains("password"));
    }

    #[test]
    fn requires_database_url() {
        let error = Config::from_values(values(&[])).unwrap_err();
        assert!(error.to_string().contains("DATABASE_URL must be set"));
    }

    #[test]
    fn rejects_invalid_pool_range_and_bind_address() {
        let range = Config::from_values(values(&[
            ("DATABASE_URL", "postgres://localhost/ledger"),
            ("DB_MIN_CONNECTIONS", "2"),
            ("DB_MAX_CONNECTIONS", "1"),
        ]))
        .unwrap_err();
        assert!(range.to_string().contains("DB_MIN_CONNECTIONS"));

        let address = Config::from_values(values(&[
            ("DATABASE_URL", "postgres://localhost/ledger"),
            ("BIND_ADDRESS", "not-an-address"),
        ]))
        .unwrap_err();
        assert!(address.to_string().contains("BIND_ADDRESS"));
    }
}
