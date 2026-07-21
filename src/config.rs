use std::{collections::HashMap, fmt, net::SocketAddr, time::Duration};

use anyhow::{Context, Result, bail};
use jsonwebtoken::DecodingKey;

#[derive(Clone, PartialEq, Eq)]
pub struct Config {
    pub database_url: String,
    pub bind_address: SocketAddr,
    pub pool: PoolConfig,
    pub log_filter: String,
    pub auth: AuthConfig,
    pub idempotency_retention: Duration,
}

impl fmt::Debug for Config {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Config")
            .field("database_url", &"[REDACTED]")
            .field("bind_address", &self.bind_address)
            .field("pool", &self.pool)
            .field("log_filter", &self.log_filter)
            .field("auth", &"[REDACTED]")
            .field("idempotency_retention", &self.idempotency_retention)
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct AuthConfig {
    pub issuer: String,
    pub audience: String,
    public_key_pem: String,
}

impl AuthConfig {
    pub fn new(
        issuer: impl Into<String>,
        audience: impl Into<String>,
        public_key_pem: impl Into<String>,
    ) -> Result<Self> {
        let issuer = nonblank(issuer.into(), "JWT_ISSUER")?;
        let audience = nonblank(audience.into(), "JWT_AUDIENCE")?;
        let public_key_pem = public_key_pem.into();
        DecodingKey::from_rsa_pem(public_key_pem.as_bytes())
            .context("JWT_PUBLIC_KEY_PEM must contain a valid RSA public key")?;
        Ok(Self {
            issuer,
            audience,
            public_key_pem,
        })
    }

    pub(crate) fn public_key_pem(&self) -> &str {
        &self.public_key_pem
    }
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
        let auth = AuthConfig::new(
            required(&values, "JWT_ISSUER")?,
            required(&values, "JWT_AUDIENCE")?,
            required(&values, "JWT_PUBLIC_KEY_PEM")?,
        )?;

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
            auth,
            idempotency_retention: parse_seconds(
                &values,
                "IDEMPOTENCY_RETENTION_SECS",
                24 * 60 * 60,
            )?,
        })
    }
}

fn nonblank(value: String, name: &str) -> Result<String> {
    if value.trim().is_empty() {
        bail!("{name} must be set");
    }
    Ok(value)
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

    const TEST_PUBLIC_KEY: &str = include_str!("../tests/fixtures/jwt-test-public.pem");

    fn required_values(items: &[(&str, &str)]) -> Vec<(String, String)> {
        let mut values = values(items);
        values.extend([
            ("JWT_ISSUER".into(), "https://issuer.example".into()),
            ("JWT_AUDIENCE".into(), "ledger".into()),
            ("JWT_PUBLIC_KEY_PEM".into(), TEST_PUBLIC_KEY.into()),
        ]);
        values
    }

    #[test]
    fn parses_defaults() {
        let config = Config::from_values(required_values(&[(
            "DATABASE_URL",
            "postgres://secret@localhost/ledger",
        )]))
        .expect("configuration should parse");
        assert_eq!(config.bind_address, "0.0.0.0:3000".parse().unwrap());
        assert_eq!(config.pool.max_connections, 10);
        assert_eq!(config.pool.min_connections, 0);
        assert_eq!(config.pool.acquire_timeout, Duration::from_secs(5));
        assert_eq!(config.idempotency_retention, Duration::from_secs(86_400));
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

    #[test]
    fn rejects_invalid_idempotency_retention() {
        let error = Config::from_values(required_values(&[
            ("DATABASE_URL", "postgres://localhost/ledger"),
            ("IDEMPOTENCY_RETENTION_SECS", "0"),
        ]))
        .unwrap_err();
        assert!(error.to_string().contains("IDEMPOTENCY_RETENTION_SECS"));
    }
}
