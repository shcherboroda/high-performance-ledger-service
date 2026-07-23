use std::{collections::BTreeMap, process::Command, sync::Arc, time::Instant};

use anyhow::{Context, Result};
use chrono::{Duration as ChronoDuration, Utc};
use clap::Parser;
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use ledger_benchmarks::{
    config::Config,
    dataset,
    http::{self, Classifications},
    result::{self, ResultDocument},
    stats, verify,
};
use serde::Serialize;
use tokio::{sync::Semaphore, task::JoinSet};
use uuid::Uuid;

#[derive(Serialize)]
struct Claims {
    iss: String,
    aud: String,
    sub: String,
    iat: i64,
    exp: i64,
}

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("benchmark failed: {error:#}");
        std::process::exit(1);
    }
}

async fn run() -> Result<()> {
    let config = Config::parse();
    config.validate()?;
    let private_key =
        std::fs::read(&config.jwt_private_key).context("read benchmark JWT private key")?;
    let tokens = tokens(&config, &private_key)?;
    let http = Arc::new(http::Http::new(
        config.service_urls.clone(),
        config.request_timeout(),
    )?);
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .connect(&config.database_url)
        .await?;

    // Phase 1-3: validated configuration, dedicated-database preparation, and preconditions.
    dataset::migrate_and_clean(&pool, config.seed).await?;
    let plans = dataset::plans(
        config.seed,
        config.logical_clients,
        config.warmup_operations,
        config.operations,
    );
    let mut pairs = Vec::with_capacity(plans.len());
    for (index, plan) in plans.iter().enumerate() {
        let token = &tokens[index % config.logical_clients];
        let source = http
            .create_account(
                index * 2,
                token,
                &format!("benchmark-{}-setup-{index}-source", config.seed),
            )
            .await?;
        let destination = http
            .create_account(
                index * 2 + 1,
                token,
                &format!("benchmark-{}-setup-{index}-destination", config.seed),
            )
            .await?;
        let client = (0..config.logical_clients)
            .find(|client| dataset::subject(config.seed, *client) == plan.owner)
            .expect("plan owner is a benchmark client");
        pairs.push((client, source, destination));
    }
    let prepared: i64 = sqlx::query_scalar("SELECT count(*) FROM accounts WHERE owner_id LIKE $1")
        .bind(format!("benchmark-{}-%", config.seed))
        .fetch_one(&pool)
        .await?;
    if prepared != (pairs.len() * 2) as i64 {
        anyhow::bail!(
            "dataset precondition failed: expected {} accounts, found {prepared}",
            pairs.len() * 2
        );
    }

    // Phase 4: warm-up is deliberately excluded from samples and verification.
    let warmup = run_phase(
        &http,
        &tokens,
        &pairs[..config.warmup_operations],
        config.seed,
        "warmup",
        config.concurrency,
    )
    .await;
    let warmup_valid = warmup.iter().all(|operation| operation.valid);
    // Phase 5: process-local metrics snapshot before measured traffic.
    let metrics_before = metrics(&http).await;
    // Phase 6: measured requests only.
    let measured_started = Instant::now();
    let measured = run_phase(
        &http,
        &tokens,
        &pairs[config.warmup_operations..],
        config.seed,
        "measured",
        config.concurrency,
    )
    .await;
    let elapsed = measured_started.elapsed();
    // Phase 7-8: metrics followed by SQL correctness checks.
    let metrics_after = metrics(&http).await;
    let measured_accounts = pairs[config.warmup_operations..]
        .iter()
        .map(|(_, source, destination)| (*source, *destination))
        .collect::<Vec<_>>();
    let verification = verify::verify(&pool, &measured_accounts, config.operations).await?;
    // Phase 9: versioned machine-readable report.
    let mut classifications = Classifications::default();
    let mut samples = Vec::new();
    let mut workload_valid = true;
    for operation in measured {
        if let Some(latency) = operation.latency_ns {
            samples.push(latency);
        }
        http::merge(&mut classifications, &operation.classification);
        workload_valid &= operation.valid;
    }
    let latency = stats::calculate(&mut samples);
    let result = ResultDocument { schema_version: result::SCHEMA_VERSION, generated_at_utc: Utc::now(), commit_sha: git_sha(), scenario: "independent_normal_transfer_smoke", seed: config.seed, service_urls: config.service_urls.clone(), logical_clients: config.logical_clients, concurrency: config.concurrency, warmup_operations: config.warmup_operations, measured_operations: config.operations, request_timeout_secs: config.request_timeout_secs, elapsed_measured_ns: elapsed.as_nanos(), throughput_operations_per_second: config.operations as f64 / elapsed.as_secs_f64(), latency, classifications, metrics_before, metrics_after, verification: Some(verification.clone()), valid: warmup_valid && workload_valid && verification.valid, database_pool_assumptions: config.db_pool_assumptions, telemetry_mode: config.telemetry_mode, environment: environment(&pool).await, limitations: vec!["This smoke scenario is harness validation, not a performance claim.".into(), "Metrics are raw process-local snapshots and are not used for client latency percentiles.".into(), "Service pool size and telemetry mode are operator supplied when recorded.".into()] };
    result::write(&config.output, &result)?;
    if !result.valid {
        anyhow::bail!(
            "benchmark completed but is invalid; inspect {}",
            config.output.display()
        );
    }
    Ok(())
}

fn tokens(config: &Config, private_key: &[u8]) -> Result<Vec<String>> {
    let key = EncodingKey::from_rsa_pem(private_key)?;
    let now = Utc::now();
    (0..config.logical_clients)
        .map(|client| {
            jsonwebtoken::encode(
                &Header::new(Algorithm::RS256),
                &Claims {
                    iss: config.jwt_issuer.clone(),
                    aud: config.jwt_audience.clone(),
                    sub: dataset::subject(config.seed, client),
                    iat: now.timestamp(),
                    exp: (now + ChronoDuration::seconds(config.jwt_lifetime_secs as i64))
                        .timestamp(),
                },
                &key,
            )
            .map_err(Into::into)
        })
        .collect()
}

async fn run_phase(
    http: &Arc<http::Http>,
    tokens: &[String],
    pairs: &[(usize, Uuid, Uuid)],
    seed: u64,
    phase: &str,
    concurrency: usize,
) -> Vec<http::Operation> {
    let semaphore = Arc::new(Semaphore::new(concurrency));
    let mut set = JoinSet::new();
    for (index, (client, source, destination)) in pairs.iter().enumerate() {
        let http = http.clone();
        let permit = semaphore.clone();
        let token = tokens[*client].clone();
        let source = *source;
        let destination = *destination;
        let key = format!("benchmark-{seed}-{phase}-{index}-{client}");
        set.spawn(async move {
            let _permit = permit.acquire_owned().await.expect("semaphore is open");
            http.transfer(index, &token, source, destination, &key)
                .await
        });
    }
    let mut operations = Vec::with_capacity(pairs.len());
    while let Some(result) = set.join_next().await {
        operations.push(result.expect("benchmark task must not panic"));
    }
    operations
}

async fn metrics(http: &http::Http) -> BTreeMap<String, Result<String, String>> {
    let mut snapshots = BTreeMap::new();
    for (index, url) in (0..).zip(http_urls(http)) {
        snapshots.insert(url.to_owned(), http.metrics(index).await);
    }
    snapshots
}
fn http_urls(http: &http::Http) -> Vec<&str> {
    let mut urls = Vec::new();
    let mut index = 0;
    loop {
        let url = http.url_for(index);
        if urls.contains(&url) {
            break;
        }
        urls.push(url);
        index += 1;
    }
    urls
}
async fn environment(pool: &sqlx::PgPool) -> BTreeMap<String, String> {
    let mut values = BTreeMap::new();
    values.insert("os".into(), std::env::consts::OS.into());
    values.insert("architecture".into(), std::env::consts::ARCH.into());
    values.insert(
        "cpu_logical_count".into(),
        std::thread::available_parallelism()
            .map(|count| count.get().to_string())
            .unwrap_or_else(|_| "unknown".into()),
    );
    values.insert(
        "benchmark_build_profile".into(),
        if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        }
        .into(),
    );
    if let Ok(version) = Command::new("rustc").arg("--version").output() {
        values.insert(
            "rustc".into(),
            String::from_utf8_lossy(&version.stdout).trim().into(),
        );
    }
    if let Ok(version) = sqlx::query_scalar::<_, String>("SHOW server_version")
        .fetch_one(pool)
        .await
    {
        values.insert("postgresql".into(), version);
    }
    values
}
fn git_sha() -> Option<String> {
    Command::new("git")
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}
