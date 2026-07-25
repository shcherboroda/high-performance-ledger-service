use anyhow::{Result, bail};
use clap::{Parser, ValueEnum};
use std::{path::PathBuf, time::Duration};
use url::Url;

pub const MAX_CONCURRENCY: usize = 256;
pub const MAX_INSTANCES: usize = 16;
pub const MAX_ACCOUNT_POOL_SIZE: usize = 10_000;
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Scenario {
    Independent,
    HotAccount,
    IdempotentReplay,
    AccountPool,
    FxIndependent,
}
impl Scenario {
    pub fn name(self) -> &'static str {
        match self {
            Self::Independent => "independent",
            Self::HotAccount => "hot-account",
            Self::IdempotentReplay => "idempotent-replay",
            Self::AccountPool => "account-pool",
            Self::FxIndependent => "fx-independent",
        }
    }
}

#[derive(Debug, Clone, Parser)]
#[command(about = "Run deterministic ledger transfer write benchmarks")]
pub struct Config {
    #[arg(long, env = "SERVICE_URLS", value_delimiter = ',', required = true)]
    pub service_urls: Vec<String>,
    #[arg(long, env = "BENCHMARK_INSTANCE_LEVELS", value_delimiter = ',')]
    pub instance_levels: Vec<usize>,
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
    #[arg(long, env = "BENCHMARK_ACCOUNT_POOL_SIZE", default_value_t = 1000)]
    pub account_pool_size: usize,
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
    parse_levels(values, MAX_CONCURRENCY, "concurrency")
}
pub fn parse_instance_levels(values: &[usize]) -> Result<Vec<usize>> {
    parse_levels(values, MAX_INSTANCES, "instance")
}
fn parse_levels(values: &[usize], maximum: usize, name: &str) -> Result<Vec<usize>> {
    let mut levels = values.to_vec();
    if levels.iter().any(|level| *level == 0 || *level > maximum) {
        bail!("{name} levels must be between 1 and {maximum}")
    }
    levels.sort_unstable();
    if levels.windows(2).any(|pair| pair[0] == pair[1]) {
        bail!("{name} levels must not contain duplicates")
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
            || self.service_urls.iter().any(|url| {
                Url::parse(url).map_or(true, |parsed| !matches!(parsed.scheme(), "http" | "https"))
            })
        {
            bail!("at least one valid service URL is required")
        }
        let mut urls = self.service_urls.clone();
        urls.sort();
        if urls.windows(2).any(|pair| pair[0] == pair[1]) {
            bail!("service URLs must not contain duplicates")
        }
        let instance_levels = parse_instance_levels(&self.instance_levels)?;
        if let Some(&largest) = instance_levels.last()
            && self.service_urls.len() != largest
        {
            bail!("service URL count must equal the largest declared instance level")
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
        if self.scenario == Scenario::AccountPool
            && !(2..=MAX_ACCOUNT_POOL_SIZE).contains(&self.account_pool_size)
        {
            bail!("account pool size must be between 2 and {MAX_ACCOUNT_POOL_SIZE}")
        }
        if self.scenario == Scenario::AccountPool && self.operations <= self.account_pool_size {
            bail!("account-pool operations must exceed account pool size to reuse accounts")
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
    pub fn instance_levels(&self) -> Result<Vec<usize>> {
        if self.instance_levels.is_empty() {
            Ok(vec![self.service_urls.len()])
        } else {
            parse_instance_levels(&self.instance_levels)
        }
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
    use clap::Parser;

    fn valid() -> Config {
        Config {
            service_urls: vec!["http://localhost:3000".into()],
            instance_levels: vec![],
            database_url: "postgres://localhost/ledger_benchmark".into(),
            scenario: Scenario::Independent,
            logical_clients: 1,
            concurrency: 1,
            concurrency_levels: vec![],
            operations: 1,
            account_pool_size: 2,
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
    fn levels_sort_and_reject_invalid_values() {
        assert_eq!(parse_concurrency_levels(&[8, 1, 4]).unwrap(), vec![1, 4, 8]);
        for values in [&[0][..], &[1, 1][..], &[MAX_CONCURRENCY + 1][..]] {
            assert!(parse_concurrency_levels(values).is_err())
        }
    }
    #[test]
    fn instance_levels_sort_and_validate_url_count() {
        assert_eq!(parse_instance_levels(&[2, 1]).unwrap(), vec![1, 2]);
        assert!(parse_instance_levels(&[0]).is_err());
        assert!(parse_instance_levels(&[1, 1]).is_err());
        let mut config = valid();
        config.instance_levels = vec![1, 2];
        assert!(config.validate().is_err());
        config.service_urls.push("http://localhost:3001".into());
        assert!(config.validate().is_ok());
        config.service_urls[1] = config.service_urls[0].clone();
        assert!(config.validate().is_err());
    }
    #[test]
    fn clap_parses_scenarios_and_rejects_invalid_values() {
        let parsed = Config::try_parse_from([
            "ledger-benchmark",
            "--service-urls",
            "http://localhost:3000",
            "--database-url",
            "postgres://localhost/ledger_benchmark",
            "--jwt-issuer",
            "issuer",
            "--jwt-audience",
            "audience",
            "--jwt-private-key",
            "key.pem",
            "--scenario",
            "hot-account",
        ])
        .unwrap();
        assert_eq!(parsed.scenario, Scenario::HotAccount);
        assert!(
            Config::try_parse_from([
                "ledger-benchmark",
                "--service-urls",
                "http://localhost:3000",
                "--database-url",
                "postgres://localhost/ledger_benchmark",
                "--jwt-issuer",
                "issuer",
                "--jwt-audience",
                "audience",
                "--jwt-private-key",
                "key.pem",
                "--scenario",
                "not-a-scenario",
            ])
            .is_err()
        );
    }
    #[test]
    fn account_pool_size_is_bounded_only_for_the_account_pool_scenario() {
        let mut config = valid();
        config.scenario = Scenario::AccountPool;
        config.account_pool_size = 1;
        assert!(config.validate().is_err());
        config.account_pool_size = MAX_ACCOUNT_POOL_SIZE + 1;
        assert!(config.validate().is_err());
        config.account_pool_size = 2;
        config.operations = 2;
        assert!(config.validate().is_err());
        config.operations = 3;
        config.jwt_lifetime_secs = 64;
        assert!(config.validate().is_ok());
        config.scenario = Scenario::Independent;
        config.account_pool_size = 1;
        assert!(config.validate().is_ok());
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
    #[test]
    fn validation_fails_closed_for_unsafe_or_incomplete_configuration() {
        let mut config = valid();
        config.allow_destructive = false;
        assert!(config.validate().is_err());

        let mut config = valid();
        config.concurrency = 0;
        assert!(config.validate().is_err());

        let mut config = valid();
        config.concurrency = MAX_CONCURRENCY + 1;
        assert!(config.validate().is_err());

        let mut config = valid();
        config.jwt_lifetime_secs = 1;
        assert!(config.validate().is_err());
    }
}
