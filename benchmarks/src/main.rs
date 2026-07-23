use anyhow::{Context, Result};
use chrono::{Duration as ChronoDuration, Utc};
use clap::Parser;
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use ledger_benchmarks::{
    config::{Config, Scenario},
    dataset::{self, ScenarioPlan, TransferPlan},
    http,
    result::{self, LevelResult, ResultDocument, TopologyLevelResult},
    stats, verify,
};
use serde::Serialize;
use std::{collections::BTreeMap, process::Command, sync::Arc, time::Instant};
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
        std::process::exit(1)
    }
}
async fn run() -> Result<()> {
    let config = Config::parse();
    config.validate()?;
    let private_key =
        std::fs::read(&config.jwt_private_key).context("read benchmark JWT private key")?;
    let tokens = tokens(&config, &private_key)?;
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .connect(&config.database_url)
        .await?;
    let mut topology_levels = Vec::new();
    for instances in config.instance_levels()? {
        let urls = config.service_urls[..instances].to_vec();
        let http = Arc::new(http::Http::new(urls.clone(), config.request_timeout())?);
        let readiness = readiness(&http).await;
        let mut levels = Vec::new();
        for concurrency in config.levels()? {
            levels.push(run_level(&config, &pool, &http, &tokens, concurrency).await?)
        }
        let ready = readiness.values().all(Result::is_ok);
        topology_levels.push(TopologyLevelResult {
            configured_instances: instances,
            service_urls: urls,
            readiness,
            valid: ready && levels.iter().all(|level| level.valid),
            levels,
        });
    }
    let document=ResultDocument{schema_version:result::SCHEMA_VERSION,generated_at_utc:Utc::now(),commit_sha:git_sha(),scenario:config.scenario.name().into(),seed:config.seed,logical_clients:config.logical_clients,request_timeout_secs:config.request_timeout_secs,summary:result::summarize_topologies(&topology_levels),topology_levels,database_pool_assumptions:config.db_pool_assumptions.clone(),telemetry_mode:config.telemetry_mode.clone(),environment:environment(&pool).await,limitations:vec!["Results are environment-specific and are not production performance guarantees.".into(),"Metrics are raw process-local snapshots and are not used for client latency percentiles.".into()]};
    result::write(&config.output, &document)?;
    if document.topology_levels.iter().any(|level| !level.valid) {
        anyhow::bail!(
            "benchmark completed but is invalid; inspect {}",
            config.output.display()
        )
    }
    Ok(())
}
async fn readiness(http: &http::Http) -> BTreeMap<String, Result<(), String>> {
    let mut results = BTreeMap::new();
    for (index, url) in http.urls().iter().enumerate() {
        results.insert(url.clone(), http.ready(index).await);
    }
    results
}
async fn run_level(
    config: &Config,
    pool: &sqlx::PgPool,
    http: &Arc<http::Http>,
    tokens: &[String],
    concurrency: usize,
) -> Result<LevelResult> {
    dataset::migrate_and_clean(pool, config.seed).await?;
    let scenario = match config.scenario {
        Scenario::Independent => ScenarioPlan::Independent,
        Scenario::HotAccount => ScenarioPlan::HotAccount,
        Scenario::IdempotentReplay => ScenarioPlan::IdempotentReplay,
    };
    let plans = dataset::plans(
        scenario,
        config.seed,
        config.logical_clients,
        config.warmup_operations,
        config.operations,
    );
    let accounts = prepare(config, http, tokens, &plans).await?;
    let (warmup, prepared_ids, replay_before) = if config.scenario == Scenario::IdempotentReplay {
        let originals = run_phase(http, tokens, &plans, &accounts, concurrency).await;
        let prepared_ids = transfer_ids_by_key(&originals)?;
        let warmup = run_phase(
            http,
            tokens,
            &plans[..config.warmup_operations],
            &accounts[..config.warmup_operations],
            concurrency,
        )
        .await;
        let keys = plans
            .iter()
            .map(|plan| plan.key.clone())
            .collect::<Vec<_>>();
        let before = verify::replay_snapshot(
            pool,
            &accounts,
            &format!("benchmark-{}-%", config.seed),
            &keys,
        )
        .await?;
        (warmup, prepared_ids, Some(before))
    } else {
        (
            run_phase(
                http,
                tokens,
                &plans[..config.warmup_operations],
                &accounts[..config.warmup_operations],
                concurrency,
            )
            .await,
            BTreeMap::new(),
            None,
        )
    };
    let metrics_before = metrics(http).await;
    let started = Instant::now();
    let measured = run_phase(
        http,
        tokens,
        &plans[config.warmup_operations..],
        &accounts[config.warmup_operations..],
        concurrency,
    )
    .await;
    let elapsed = started.elapsed();
    let metrics_after = metrics(http).await;
    let metrics_valid =
        metrics_before.values().all(Result::is_ok) && metrics_after.values().all(Result::is_ok);
    let warmup_valid = warmup.iter().all(|operation| operation.valid);
    let (classifications, mut samples, workload_valid) =
        http::summarize_measured(&http::PhaseOperations {
            warmup,
            measured: measured.clone(),
        });
    let verification = match config.scenario {
        Scenario::Independent => {
            verify::independent(
                pool,
                &accounts[config.warmup_operations..],
                config.operations,
            )
            .await?
        }
        Scenario::HotAccount => {
            verify::hot_account(
                pool,
                accounts[config.warmup_operations].0,
                &accounts[config.warmup_operations..]
                    .iter()
                    .map(|(_, d)| *d)
                    .collect::<Vec<_>>(),
                config.operations,
            )
            .await?
        }
        Scenario::IdempotentReplay => {
            let replay_ids = transfer_ids_by_key(&measured)?;
            let expected_replay_ids = plans[config.warmup_operations..]
                .iter()
                .map(|plan| (plan.key.clone(), prepared_ids[&plan.key]))
                .collect::<BTreeMap<_, _>>();
            let keys = plans
                .iter()
                .map(|plan| plan.key.clone())
                .collect::<Vec<_>>();
            let after = verify::replay_snapshot(
                pool,
                &accounts,
                &format!("benchmark-{}-%", config.seed),
                &keys,
            )
            .await?;
            verify::evaluate_replay(
                replay_before.as_ref().expect("replay snapshot exists"),
                &after,
                plans.len(),
                replay_ids == expected_replay_ids,
            )
        }
    };
    let measured_requests_per_url = http::measured_requests_per_url(&measured);
    let distribution_valid = http::every_instance_received_measured_traffic(
        http.urls(),
        config.operations,
        &measured_requests_per_url,
    );
    Ok(LevelResult {
        scenario: config.scenario.name().into(),
        seed: config.seed,
        concurrency,
        warmup_operations: config.warmup_operations,
        measured_operations: config.operations,
        completed_measured_operations: samples.len(),
        elapsed_measured_ns: elapsed.as_nanos(),
        throughput_operations_per_second: config.operations as f64 / elapsed.as_secs_f64(),
        latency: stats::calculate(&mut samples),
        classifications,
        measured_requests_per_url,
        metrics_before,
        metrics_after,
        valid: warmup_valid
            && workload_valid
            && metrics_valid
            && distribution_valid
            && verification.valid,
        verification,
    })
}
fn transfer_ids_by_key(operations: &[http::Operation]) -> Result<BTreeMap<String, Uuid>> {
    let mut ids = BTreeMap::new();
    for operation in operations {
        let key = operation
            .request_key
            .clone()
            .ok_or_else(|| anyhow::anyhow!("operation is missing its request key"))?;
        let id = operation
            .transfer_id
            .ok_or_else(|| anyhow::anyhow!("operation did not return a transfer ID"))?;
        if ids.insert(key, id).is_some() {
            anyhow::bail!("operation request keys must be unique");
        }
    }
    Ok(ids)
}
async fn prepare(
    config: &Config,
    http: &http::Http,
    tokens: &[String],
    plans: &[TransferPlan],
) -> Result<Vec<(Uuid, Uuid)>> {
    let count = plans.len();
    let mut result = Vec::with_capacity(count);
    if config.scenario == Scenario::HotAccount {
        let max_source = plans.iter().map(|p| p.source).max().unwrap_or(0);
        let mut source_ids = Vec::new();
        for index in 0..=max_source {
            source_ids.push(
                http.create_account(
                    index,
                    &tokens[0],
                    &format!("benchmark-{}-source-{index}", config.seed),
                    &format!(
                        "{}.00",
                        if index == 0 {
                            config.warmup_operations + 10
                        } else {
                            config.operations + 10
                        }
                    ),
                )
                .await?,
            );
        }
        for (index, plan) in plans.iter().enumerate() {
            let destination = http
                .create_account(
                    1000 + index,
                    &tokens[plan.client],
                    &format!("benchmark-{}-destination-{index}", config.seed),
                    "10.00",
                )
                .await?;
            result.push((source_ids[plan.source], destination))
        }
    } else {
        for (index, plan) in plans.iter().enumerate() {
            let source = http
                .create_account(
                    index * 2,
                    &tokens[plan.client],
                    &format!("benchmark-{}-source-{index}", config.seed),
                    "10.00",
                )
                .await?;
            let destination = http
                .create_account(
                    index * 2 + 1,
                    &tokens[plan.client],
                    &format!("benchmark-{}-destination-{index}", config.seed),
                    "10.00",
                )
                .await?;
            result.push((source, destination))
        }
    }
    Ok(result)
}
async fn run_phase(
    http: &Arc<http::Http>,
    tokens: &[String],
    plans: &[TransferPlan],
    accounts: &[(Uuid, Uuid)],
    concurrency: usize,
) -> Vec<http::Operation> {
    let sem = Arc::new(Semaphore::new(concurrency));
    let mut set = JoinSet::new();
    for (index, plan) in plans.iter().enumerate() {
        let http = http.clone();
        let permit = sem.clone();
        let token = tokens[plan.client].clone();
        let (source, destination) = accounts[index];
        let key = plan.key.clone();
        set.spawn(async move {
            let _permit = permit.acquire_owned().await.expect("semaphore open");
            let mut operation = http
                .transfer(index, &token, source, destination, &key)
                .await;
            operation.request_key = Some(key);
            operation
        });
    }
    let mut operations = Vec::with_capacity(plans.len());
    while let Some(operation) = set.join_next().await {
        operations.push(operation.expect("benchmark task must not panic"))
    }
    operations
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
async fn metrics(http: &http::Http) -> BTreeMap<String, Result<String, String>> {
    let mut snapshots = BTreeMap::new();
    for (index, url) in http.urls().iter().enumerate() {
        let url = url.clone();
        snapshots.insert(url, http.metrics(index).await);
    }
    snapshots
}
async fn environment(pool: &sqlx::PgPool) -> BTreeMap<String, String> {
    let mut values = BTreeMap::new();
    values.insert("os".into(), std::env::consts::OS.into());
    values.insert("architecture".into(), std::env::consts::ARCH.into());
    values.insert(
        "cpu_logical_count".into(),
        std::thread::available_parallelism()
            .map(|c| c.get().to_string())
            .unwrap_or_else(|_| "unknown".into()),
    );
    if let Ok(v) = sqlx::query_scalar::<_, String>("SHOW server_version")
        .fetch_one(pool)
        .await
    {
        values.insert("postgresql".into(), v);
    }
    values
}
fn git_sha() -> Option<String> {
    Command::new("git")
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
}
