use anyhow::{Result, bail};
use clap::{Parser, ValueEnum};
use std::{path::PathBuf, time::Duration};
use url::Url;

pub const MAX_CONCURRENCY: usize = 256;
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Scenario {
    Independent,
    HotAccount,
    IdempotentReplay,
}
impl Scenario {
    pub fn name(self) -> &'static str {
        match self {
            Self::Independent => "independent",
            Self::HotAccount => "hot-account",
            Self::IdempotentReplay => "idempotent-replay",
        }
    }
}

#[derive(Debug, Clone, Parser)]
#[command(about = "Run deterministic ledger transfer write benchmarks")]
pub struct Config {
    #[arg(long, env = "SERVICE_URLS", value_delimiter = ',', required = true)]
    pub service_urls: Vec<String>,
    #[arg(long, env = "BENCHMARK_DATABASE_URL")]
    pub database_url: String,
    #[arg(long, value_enum, env="BENCHMARK_SCENARIO", default_value_t=Scenario::Independent)]
    pub scenario: Scenario,
    #[arg(long, env = "BENCHMARK_LOGICAL_CLIENTS", default_value_t = 2)]
    pub logical_clients: usize,
    #[arg(long, env = "BENCHMARK_CONCURRENCY", default_value_t = 2)]
    pub concurrency: usize,
    #[arg(long, env = "BENCHMARK_CONCURRENCY_LEVELS", value_delimiter = ',')]
    pub concurrency_levels: Vec<usize>,
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
        default_value = "benchmark-results/result.json"
    )]
    pub output: PathBuf,
    #[arg(long, env="BENCHMARK_ALLOW_DESTRUCTIVE", default_value_t=false, value_parser=parse_destructive_acknowledgement)]
    pub allow_destructive: bool,
    #[arg(long, env = "BENCHMARK_JWT_ISSUER")]
    pub jwt_issuer: String,
    #[arg(long, env = "BENCHMARK_JWT_AUDIENCE")]
    pub jwt_audience: String,
    #[arg(long, env = "BENCHMARK_JWT_PRIVATE_KEY")]
    pub jwt_private_key: PathBuf,
    #[arg(long, env = "BENCHMARK_JWT_LIFETIME_SECS", default_value_t = 3600)]
    pub jwt_lifetime_secs: u64,
    #[arg(long, env = "BENCHMARK_DB_POOL_ASSUMPTIONS")]
    pub db_pool_assumptions: Option<String>,
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
pub fn parse_concurrency_levels(values: &[usize]) -> Result<Vec<usize>> {
    let mut levels = values.to_vec();
    if levels
        .iter()
        .any(|level| *level == 0 || *level > MAX_CONCURRENCY)
    {
        bail!("concurrency levels must be between 1 and {MAX_CONCURRENCY}")
    }
    levels.sort_unstable();
    if levels.windows(2).any(|pair| pair[0] == pair[1]) {
        bail!("concurrency levels must not contain duplicates")
    }
    Ok(levels)
}
impl Config {
    pub fn validate(&self) -> Result<()> {
        if !self.allow_destructive {
            bail!("BENCHMARK_ALLOW_DESTRUCTIVE=1 (or --allow-destructive) is required")
        }
        database_name(&self.database_url)?;
        if self.service_urls.is_empty()
            || self.service_urls.iter().any(|url| Url::parse(url).is_err())
        {
            bail!("at least one valid service URL is required")
        }
        if self.logical_clients == 0
            || self.concurrency == 0
            || self.concurrency > MAX_CONCURRENCY
            || self.operations == 0
        {
            bail!(
                "logical clients, operations, and concurrency (maximum {MAX_CONCURRENCY}) must be greater than zero"
            )
        }
        parse_concurrency_levels(&self.concurrency_levels)?;
        if self.request_timeout_secs == 0 || self.jwt_lifetime_secs == 0 {
            bail!("request timeout and JWT lifetime must be greater than zero")
        }
        if self.jwt_issuer.trim().is_empty() || self.jwt_audience.trim().is_empty() {
            bail!("JWT issuer and audience must not be blank")
        }
        if !self.jwt_private_key.is_file() {
            bail!("BENCHMARK_JWT_PRIVATE_KEY must name a readable private-key file")
        }
        let longest = *self
            .concurrency_levels
            .iter()
            .min()
            .unwrap_or(&self.concurrency);
        let minimum = self
            .request_timeout_secs
            .saturating_mul(
                ((self.operations + self.warmup_operations) as u64).div_ceil(longest as u64),
            )
            .saturating_add(60);
        if self.jwt_lifetime_secs < minimum {
            bail!(
                "JWT lifetime is shorter than the conservative configured run duration ({minimum}s)"
            )
        }
        Ok(())
    }
    pub fn levels(&self) -> Result<Vec<usize>> {
        if self.concurrency_levels.is_empty() {
            Ok(vec![self.concurrency])
        } else {
            parse_concurrency_levels(&self.concurrency_levels)
        }
    }
    pub fn request_timeout(&self) -> Duration {
        Duration::from_secs(self.request_timeout_secs)
    }
}
pub fn database_name(database_url: &str) -> Result<String> {
    let url = Url::parse(database_url)
        .map_err(|_| anyhow::anyhow!("benchmark database URL cannot be parsed safely"))?;
    if !matches!(url.scheme(), "postgres" | "postgresql") || url.path().contains('%') {
        bail!("benchmark database URL cannot be parsed safely")
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
        bail!("benchmark database name must be an ASCII identifier ending in _benchmark")
    }
    Ok(name.to_owned())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn levels_sort_and_reject_invalid_values() {
        assert_eq!(parse_concurrency_levels(&[8, 1, 4]).unwrap(), vec![1, 4, 8]);
        for values in [&[0][..], &[1, 1][..], &[MAX_CONCURRENCY + 1][..]] {
            assert!(parse_concurrency_levels(values).is_err())
        }
    }
    #[test]
    fn scenario_values_are_stable() {
        assert_eq!(Scenario::HotAccount.name(), "hot-account");
        assert_eq!(Scenario::IdempotentReplay.name(), "idempotent-replay")
    }
    #[test]
    fn database_guard_fails_closed() {
        assert!(database_name("postgres://localhost/ledger_benchmark").is_ok());
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
    fn destructive_acknowledgement_accepts_only_documented_values() {
        assert!(parse_destructive_acknowledgement("1").unwrap());
        assert!(parse_destructive_acknowledgement("true").unwrap());
        assert!(!parse_destructive_acknowledgement("0").unwrap());
        assert!(!parse_destructive_acknowledgement("false").unwrap());
        assert!(parse_destructive_acknowledgement("yes").is_err());
    }
}
