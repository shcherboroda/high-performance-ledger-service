use std::{path::PathBuf, time::Duration};

use anyhow::{Result, bail};
use clap::Parser;
use url::Url;

#[derive(Debug, Clone, Parser)]
#[command(about = "Run the ledger HTTP benchmark smoke scenario")]
pub struct Config {
    /// Service URLs, supplied repeatedly or as a comma-separated SERVICE_URLS value.
    #[arg(long, env = "SERVICE_URLS", value_delimiter = ',', required = true)]
    pub service_urls: Vec<String>,
    #[arg(long, env = "BENCHMARK_DATABASE_URL")]
    pub database_url: String,
    #[arg(long, env = "BENCHMARK_LOGICAL_CLIENTS", default_value_t = 2)]
    pub logical_clients: usize,
    #[arg(long, env = "BENCHMARK_CONCURRENCY", default_value_t = 2)]
    pub concurrency: usize,
    #[arg(long, env = "BENCHMARK_OPERATIONS", default_value_t = 20)]
    pub operations: usize,
    #[arg(long, env = "BENCHMARK_WARMUP_OPERATIONS", default_value_t = 4)]
    pub warmup_operations: usize,
    #[arg(long, env = "BENCHMARK_SEED", default_value_t = 1)]
    pub seed: u64,
    #[arg(long, env = "BENCHMARK_REQUEST_TIMEOUT_SECS", default_value_t = 10)]
    pub request_timeout_secs: u64,
    #[arg(
        long,
        env = "BENCHMARK_OUTPUT",
        default_value = "benchmark-results/smoke.json"
    )]
    pub output: PathBuf,
    /// Required acknowledgement: this benchmark migrates and removes benchmark-owned data.
    #[arg(
        long,
        env = "BENCHMARK_ALLOW_DESTRUCTIVE",
        default_value_t = false,
        value_parser = parse_destructive_acknowledgement
    )]
    pub allow_destructive: bool,
    #[arg(long, env = "BENCHMARK_JWT_ISSUER")]
    pub jwt_issuer: String,
    #[arg(long, env = "BENCHMARK_JWT_AUDIENCE")]
    pub jwt_audience: String,
    #[arg(long, env = "BENCHMARK_JWT_PRIVATE_KEY")]
    pub jwt_private_key: PathBuf,
    #[arg(long, env = "BENCHMARK_JWT_LIFETIME_SECS", default_value_t = 3600)]
    pub jwt_lifetime_secs: u64,
    /// Operator-supplied service pool description; never inferred.
    #[arg(long, env = "BENCHMARK_DB_POOL_ASSUMPTIONS")]
    pub db_pool_assumptions: Option<String>,
    /// Operator-supplied telemetry/logging mode; never inferred.
    #[arg(long, env = "BENCHMARK_TELEMETRY_MODE")]
    pub telemetry_mode: Option<String>,
}

fn parse_destructive_acknowledgement(value: &str) -> std::result::Result<bool, String> {
    match value {
        "1" | "true" | "TRUE" | "True" => Ok(true),
        "0" | "false" | "FALSE" | "False" => Ok(false),
        _ => Err("must be one of 1, 0, true, or false".into()),
    }
}

impl Config {
    pub fn validate(&self) -> Result<()> {
        if !self.allow_destructive {
            bail!("BENCHMARK_ALLOW_DESTRUCTIVE=1 (or --allow-destructive) is required");
        }
        database_name(&self.database_url)?;
        if self.service_urls.is_empty()
            || self.service_urls.iter().any(|url| Url::parse(url).is_err())
        {
            bail!("at least one valid service URL is required");
        }
        if self.logical_clients == 0 || self.concurrency == 0 || self.operations == 0 {
            bail!("logical clients, concurrency, and operations must be greater than zero");
        }
        if self.request_timeout_secs == 0 || self.jwt_lifetime_secs == 0 {
            bail!("request timeout and JWT lifetime must be greater than zero");
        }
        if self.jwt_issuer.trim().is_empty() || self.jwt_audience.trim().is_empty() {
            bail!("JWT issuer and audience must not be blank");
        }
        if !self.jwt_private_key.is_file() {
            bail!("BENCHMARK_JWT_PRIVATE_KEY must name a readable private-key file");
        }
        let minimum_lifetime = self
            .request_timeout_secs
            .saturating_mul(
                ((self.operations + self.warmup_operations) as u64)
                    .div_ceil(self.concurrency as u64),
            )
            .saturating_add(60);
        if self.jwt_lifetime_secs < minimum_lifetime {
            bail!(
                "JWT lifetime is shorter than the conservative configured run duration ({minimum_lifetime}s)"
            );
        }
        Ok(())
    }

    pub fn request_timeout(&self) -> Duration {
        Duration::from_secs(self.request_timeout_secs)
    }
}

pub fn database_name(database_url: &str) -> Result<String> {
    let url = Url::parse(database_url)
        .map_err(|_| anyhow::anyhow!("benchmark database URL cannot be parsed safely"))?;
    if !matches!(url.scheme(), "postgres" | "postgresql") || url.path().contains('%') {
        bail!("benchmark database URL cannot be parsed safely");
    }
    let name = url
        .path()
        .strip_prefix('/')
        .filter(|name| !name.is_empty() && !name.contains('/'))
        .ok_or_else(|| anyhow::anyhow!("benchmark database name cannot be parsed safely"))?;
    if !name.ends_with("_benchmark")
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    {
        bail!("benchmark database name must be an ASCII identifier ending in _benchmark");
    }
    Ok(name.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn valid() -> Config {
        Config {
            service_urls: vec!["http://localhost:3000".into()],
            database_url: "postgres://localhost/ledger_benchmark".into(),
            logical_clients: 1,
            concurrency: 1,
            operations: 1,
            warmup_operations: 0,
            seed: 1,
            request_timeout_secs: 1,
            output: "out.json".into(),
            allow_destructive: true,
            jwt_issuer: "issuer".into(),
            jwt_audience: "audience".into(),
            jwt_private_key: PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../tests/fixtures/jwt-test-private.pem"),
            jwt_lifetime_secs: 61,
            db_pool_assumptions: None,
            telemetry_mode: None,
        }
    }
    #[test]
    fn database_guard_fails_closed() {
        assert_eq!(
            database_name("postgres://localhost/ledger_benchmark").unwrap(),
            "ledger_benchmark"
        );
        for url in [
            "postgres://localhost/ledger",
            "postgres://localhost/ledger_benchmark/extra",
            "not a url",
            "postgres://localhost/%6cedger_benchmark",
        ] {
            assert!(database_name(url).is_err(), "{url}");
        }
    }

    #[test]
    fn validation_rejects_unsafe_or_incomplete_configuration() {
        let mut config = valid();
        config.allow_destructive = false;
        assert!(config.validate().is_err());
        let mut config = valid();
        config.concurrency = 0;
        assert!(config.validate().is_err());
        let mut config = valid();
        config.jwt_lifetime_secs = 1;
        assert!(config.validate().is_err());
    }

    #[test]
    fn destructive_acknowledgement_accepts_documented_environment_values() {
        assert!(parse_destructive_acknowledgement("1").unwrap());
        assert!(parse_destructive_acknowledgement("true").unwrap());
        assert!(!parse_destructive_acknowledgement("0").unwrap());
        assert!(!parse_destructive_acknowledgement("false").unwrap());
        assert!(parse_destructive_acknowledgement("yes").is_err());
    }
}
