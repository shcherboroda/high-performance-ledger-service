use crate::{http::Classifications, stats::Latency, verify::Verification};
use chrono::{DateTime, Utc};
use serde::Serialize;
use std::{collections::BTreeMap, path::Path};
pub const SCHEMA_VERSION: u32 = 4;
#[derive(Debug, Serialize)]
pub struct LevelResult {
    pub scenario: String,
    pub seed: u64,
    pub concurrency: usize,
    pub warmup_operations: usize,
    pub measured_operations: usize,
    pub account_pool_size: Option<usize>,
    pub phase_count: Option<usize>,
    pub phase_model: Option<String>,
    pub completed_measured_operations: usize,
    pub elapsed_measured_ns: u128,
    pub throughput_operations_per_second: f64,
    pub latency: Option<Latency>,
    pub classifications: Classifications,
    pub measured_requests_per_url: BTreeMap<String, usize>,
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
pub struct TopologyLevelResult {
    pub configured_instances: usize,
    pub service_urls: Vec<String>,
    pub readiness: BTreeMap<String, Result<(), String>>,
    pub workload_skipped_reason: Option<String>,
    pub workload_failures: Vec<WorkloadFailure>,
    pub levels: Vec<LevelResult>,
    pub valid: bool,
}
#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct WorkloadFailure {
    pub concurrency: usize,
    pub reason: String,
}
impl TopologyLevelResult {
    pub fn readiness_failure(
        configured_instances: usize,
        service_urls: Vec<String>,
        readiness: BTreeMap<String, Result<(), String>>,
    ) -> Self {
        Self {
            configured_instances,
            service_urls,
            readiness,
            workload_skipped_reason: Some(
                "workload skipped because at least one configured instance was not ready".into(),
            ),
            workload_failures: Vec::new(),
            levels: Vec::new(),
            valid: false,
        }
    }
    pub fn workload_failure(
        configured_instances: usize,
        service_urls: Vec<String>,
        readiness: BTreeMap<String, Result<(), String>>,
        concurrency: usize,
        reason: impl Into<String>,
    ) -> Self {
        Self {
            configured_instances,
            service_urls,
            readiness,
            workload_skipped_reason: None,
            workload_failures: vec![WorkloadFailure {
                concurrency,
                reason: reason.into(),
            }],
            levels: Vec::new(),
            valid: false,
        }
    }
}
#[derive(Debug, Serialize)]
pub struct TopologySummary {
    pub adjacent_throughput_ratios: Vec<TopologyThroughputComparison>,
}
#[derive(Debug, Serialize, PartialEq)]
pub struct TopologyThroughputComparison {
    pub from_instances: usize,
    pub to_instances: usize,
    pub concurrency: usize,
    pub throughput_ratio: Option<f64>,
}
#[derive(Debug, Serialize)]
pub struct ResultDocument {
    pub schema_version: u32,
    pub generated_at_utc: DateTime<Utc>,
    pub commit_sha: Option<String>,
    pub scenario: String,
    pub seed: u64,
    pub topology_levels: Vec<TopologyLevelResult>,
    pub logical_clients: usize,
    pub request_timeout_secs: u64,
    pub summary: TopologySummary,
    pub database_pool_assumptions: Option<String>,
    pub telemetry_mode: Option<String>,
    pub environment: BTreeMap<String, String>,
    pub limitations: Vec<String>,
}
pub fn summarize_topologies(levels: &[TopologyLevelResult]) -> TopologySummary {
    TopologySummary {
        adjacent_throughput_ratios: levels
            .windows(2)
            .flat_map(|pair| {
                pair[0].levels.iter().filter_map(|before| {
                    pair[1]
                        .levels
                        .iter()
                        .find(|after| after.concurrency == before.concurrency)
                        .map(|after| TopologyThroughputComparison {
                            from_instances: pair[0].configured_instances,
                            to_instances: pair[1].configured_instances,
                            concurrency: before.concurrency,
                            throughput_ratio: (before.throughput_operations_per_second > 0.0).then(
                                || {
                                    after.throughput_operations_per_second
                                        / before.throughput_operations_per_second
                                },
                            ),
                        })
                })
            })
            .collect(),
    }
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
    use crate::progress::ProgressReporter;
    use crate::verify::Verification;
    use std::io::{self, Write};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    #[derive(Clone)]
    struct Capture(Arc<Mutex<Vec<u8>>>);

    impl Write for Capture {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    #[test]
    fn matrix_aggregates_levels() {
        let l = |concurrency, throughput, valid| LevelResult {
            scenario: "x".into(),
            seed: 1,
            concurrency,
            warmup_operations: 0,
            measured_operations: 1,
            account_pool_size: None,
            phase_count: None,
            phase_model: None,
            completed_measured_operations: 1,
            elapsed_measured_ns: 1,
            throughput_operations_per_second: throughput,
            latency: None,
            classifications: Classifications::default(),
            measured_requests_per_url: BTreeMap::new(),
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
    #[test]
    fn runner_stderr_progress_does_not_alter_the_json_report() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("result.json");
        let stderr = Arc::new(Mutex::new(Vec::new()));
        {
            let mut progress = ProgressReporter::stderr_with(Capture(stderr.clone()));
            let mut measured = progress.phase(2, "measured", 20);
            for _ in 0..20 {
                measured.complete_operation();
            }
            measured.finish(Duration::from_millis(1));
            write(
                &path,
                &ResultDocument {
                    schema_version: SCHEMA_VERSION,
                    generated_at_utc: Utc::now(),
                    commit_sha: None,
                    scenario: "independent".into(),
                    seed: 1,
                    topology_levels: vec![],
                    logical_clients: 2,
                    request_timeout_secs: 10,
                    summary: TopologySummary {
                        adjacent_throughput_ratios: vec![],
                    },
                    database_pool_assumptions: None,
                    telemetry_mode: None,
                    environment: BTreeMap::new(),
                    limitations: vec![],
                },
            )
            .unwrap();
        }
        let stderr = String::from_utf8(stderr.lock().unwrap().clone()).unwrap();
        let report: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert!(stderr.contains("benchmark: concurrency=2 measured started"));
        assert!(stderr.contains("benchmark: concurrency=2 measured progress 2/20"));
        assert_eq!(report["schema_version"], SCHEMA_VERSION);
        assert!(!report.as_object().unwrap().contains_key("progress"));
    }
    #[test]
    fn topology_summary_serializes_versioned_levels() {
        let topology = TopologyLevelResult {
            configured_instances: 2,
            service_urls: vec!["http://a".into(), "http://b".into()],
            readiness: BTreeMap::from([("http://a".into(), Ok(())), ("http://b".into(), Ok(()))]),
            workload_skipped_reason: None,
            workload_failures: Vec::new(),
            levels: vec![],
            valid: true,
        };
        let summary = summarize_topologies(&[topology]);
        assert_eq!(summary.adjacent_throughput_ratios, vec![]);
        assert!(serde_json::to_value(summary).is_ok());
    }
    #[test]
    fn account_pool_metadata_is_serialized() {
        let level = LevelResult {
            scenario: "account-pool".into(),
            seed: 7,
            concurrency: 2,
            warmup_operations: 1,
            measured_operations: 3,
            account_pool_size: Some(2),
            phase_count: Some(2),
            phase_model: Some("ring phases".into()),
            completed_measured_operations: 3,
            elapsed_measured_ns: 1,
            throughput_operations_per_second: 3.0,
            latency: None,
            classifications: Classifications::default(),
            measured_requests_per_url: BTreeMap::new(),
            metrics_before: BTreeMap::new(),
            metrics_after: BTreeMap::new(),
            verification: Verification {
                checks: vec![],
                valid: true,
            },
            valid: true,
        };
        let value = serde_json::to_value(level).unwrap();
        assert_eq!(value["account_pool_size"], 2);
        assert_eq!(value["phase_count"], 2);
        assert_eq!(value["phase_model"], "ring phases");
        assert!(!value.as_object().unwrap().contains_key("progress"));
    }
    #[test]
    fn topology_summary_compares_each_matching_concurrency_level() {
        let level = |concurrency, throughput| LevelResult {
            scenario: "independent".into(),
            seed: 1,
            concurrency,
            warmup_operations: 0,
            measured_operations: 1,
            account_pool_size: None,
            phase_count: None,
            phase_model: None,
            completed_measured_operations: 1,
            elapsed_measured_ns: 1,
            throughput_operations_per_second: throughput,
            latency: None,
            classifications: Classifications::default(),
            measured_requests_per_url: BTreeMap::new(),
            metrics_before: BTreeMap::new(),
            metrics_after: BTreeMap::new(),
            verification: Verification {
                checks: vec![],
                valid: true,
            },
            valid: true,
        };
        let topology = |instances, levels| TopologyLevelResult {
            configured_instances: instances,
            service_urls: vec![],
            readiness: BTreeMap::new(),
            workload_skipped_reason: None,
            workload_failures: Vec::new(),
            levels,
            valid: true,
        };
        assert_eq!(
            summarize_topologies(&[
                topology(1, vec![level(1, 10.), level(2, 20.)]),
                topology(2, vec![level(1, 15.), level(2, 30.)])
            ])
            .adjacent_throughput_ratios,
            vec![
                TopologyThroughputComparison {
                    from_instances: 1,
                    to_instances: 2,
                    concurrency: 1,
                    throughput_ratio: Some(1.5)
                },
                TopologyThroughputComparison {
                    from_instances: 1,
                    to_instances: 2,
                    concurrency: 2,
                    throughput_ratio: Some(1.5)
                }
            ]
        );
    }
    #[test]
    fn readiness_failure_is_retained_as_an_invalid_topology_result() {
        let result = TopologyLevelResult::readiness_failure(
            2,
            vec!["http://a".into(), "http://b".into()],
            BTreeMap::from([
                ("http://a".into(), Ok(())),
                ("http://b".into(), Err("offline".into())),
            ]),
        );
        assert!(!result.valid);
        assert!(result.levels.is_empty());
        assert_eq!(result.readiness.len(), 2);
        assert!(result.workload_skipped_reason.is_some());
    }
    #[test]
    fn workload_failure_is_retained_without_fabricating_measurements() {
        let result = TopologyLevelResult::workload_failure(
            2,
            vec!["http://a".into(), "http://b".into()],
            BTreeMap::from([("http://a".into(), Ok(())), ("http://b".into(), Ok(()))]),
            4,
            "account setup returned HTTP 503",
        );
        assert!(!result.valid);
        assert!(result.levels.is_empty());
        assert_eq!(
            result.workload_failures,
            vec![WorkloadFailure {
                concurrency: 4,
                reason: "account setup returned HTTP 503".into()
            }]
        );
    }
}
