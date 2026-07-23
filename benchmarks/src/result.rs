use crate::{http::Classifications, stats::Latency, verify::Verification};
use chrono::{DateTime, Utc};
use serde::Serialize;
use std::{collections::BTreeMap, path::Path};
pub const SCHEMA_VERSION: u32 = 2;
#[derive(Debug, Serialize)]
pub struct LevelResult {
    pub scenario: String,
    pub seed: u64,
    pub concurrency: usize,
    pub warmup_operations: usize,
    pub measured_operations: usize,
    pub completed_measured_operations: usize,
    pub elapsed_measured_ns: u128,
    pub throughput_operations_per_second: f64,
    pub latency: Option<Latency>,
    pub classifications: Classifications,
    pub metrics_before: BTreeMap<String, Result<String, String>>,
    pub metrics_after: BTreeMap<String, Result<String, String>>,
    pub verification: Verification,
    pub valid: bool,
}
#[derive(Debug, Serialize)]
pub struct MatrixSummary {
    pub highest_valid_tested_level: Option<usize>,
    pub adjacent_throughput_changes: Vec<f64>,
}
#[derive(Debug, Serialize)]
pub struct ResultDocument {
    pub schema_version: u32,
    pub generated_at_utc: DateTime<Utc>,
    pub commit_sha: Option<String>,
    pub scenario: String,
    pub seed: u64,
    pub service_urls: Vec<String>,
    pub logical_clients: usize,
    pub request_timeout_secs: u64,
    pub levels: Vec<LevelResult>,
    pub summary: MatrixSummary,
    pub database_pool_assumptions: Option<String>,
    pub telemetry_mode: Option<String>,
    pub environment: BTreeMap<String, String>,
    pub limitations: Vec<String>,
}
pub fn summarize(levels: &[LevelResult]) -> MatrixSummary {
    let adjacent_throughput_changes = levels
        .windows(2)
        .map(|pair| {
            if pair[0].throughput_operations_per_second == 0.0 {
                0.0
            } else {
                pair[1].throughput_operations_per_second / pair[0].throughput_operations_per_second
                    - 1.0
            }
        })
        .collect();
    MatrixSummary {
        highest_valid_tested_level: levels
            .iter()
            .filter(|level| level.valid)
            .map(|level| level.concurrency)
            .max(),
        adjacent_throughput_changes,
    }
}
pub fn write(path: &Path, result: &ResultDocument) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?
    }
    std::fs::write(path, serde_json::to_vec_pretty(result)?)?;
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::Classifications;
    use crate::verify::Verification;
    #[test]
    fn matrix_aggregates_levels() {
        let l = |concurrency, throughput, valid| LevelResult {
            scenario: "x".into(),
            seed: 1,
            concurrency,
            warmup_operations: 0,
            measured_operations: 1,
            completed_measured_operations: 1,
            elapsed_measured_ns: 1,
            throughput_operations_per_second: throughput,
            latency: None,
            classifications: Classifications::default(),
            metrics_before: BTreeMap::new(),
            metrics_after: BTreeMap::new(),
            verification: Verification {
                checks: vec![],
                valid,
            },
            valid,
        };
        let s = summarize(&[l(1, 10., true), l(2, 15., true)]);
        assert_eq!(s.highest_valid_tested_level, Some(2));
        assert_eq!(s.adjacent_throughput_changes, vec![0.5]);
    }
}
