use crate::{http::Classifications, stats::Latency, verify::Verification};
use chrono::{DateTime, Utc};
use serde::Serialize;
use std::{collections::BTreeMap, path::Path};

pub const SCHEMA_VERSION: u32 = 1;
#[derive(Debug, Serialize)]
pub struct ResultDocument {
    pub schema_version: u32,
    pub generated_at_utc: DateTime<Utc>,
    pub commit_sha: Option<String>,
    pub scenario: &'static str,
    pub seed: u64,
    pub service_urls: Vec<String>,
    pub logical_clients: usize,
    pub concurrency: usize,
    pub warmup_operations: usize,
    pub measured_operations: usize,
    pub request_timeout_secs: u64,
    pub elapsed_measured_ns: u128,
    pub throughput_operations_per_second: f64,
    pub latency: Option<Latency>,
    pub classifications: Classifications,
    pub metrics_before: BTreeMap<String, Result<String, String>>,
    pub metrics_after: BTreeMap<String, Result<String, String>>,
    pub verification: Option<Verification>,
    pub valid: bool,
    pub database_pool_assumptions: Option<String>,
    pub telemetry_mode: Option<String>,
    pub environment: BTreeMap<String, String>,
    pub limitations: Vec<String>,
}
pub fn write(path: &Path, result: &ResultDocument) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, serde_json::to_vec_pretty(result)?)?;
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn result_serialization_includes_the_schema_version() {
        let result = ResultDocument {
            schema_version: SCHEMA_VERSION,
            generated_at_utc: Utc::now(),
            commit_sha: None,
            scenario: "test",
            seed: 1,
            service_urls: vec!["http://localhost".into()],
            logical_clients: 1,
            concurrency: 1,
            warmup_operations: 1,
            measured_operations: 1,
            request_timeout_secs: 1,
            elapsed_measured_ns: 1,
            throughput_operations_per_second: 1.0,
            latency: None,
            classifications: Classifications::default(),
            metrics_before: BTreeMap::new(),
            metrics_after: BTreeMap::new(),
            verification: None,
            valid: true,
            database_pool_assumptions: None,
            telemetry_mode: None,
            environment: BTreeMap::new(),
            limitations: vec![],
        };
        assert_eq!(serde_json::to_value(result).unwrap()["schema_version"], 1);
    }
}
