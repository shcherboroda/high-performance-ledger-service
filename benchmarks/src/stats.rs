use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct Latency {
    pub operation_count: usize,
    pub min_ns: u128,
    pub max_ns: u128,
    pub mean_ns: f64,
    pub p50_ns: u128,
    pub p95_ns: u128,
    pub p99_ns: u128,
}

pub fn calculate(samples: &mut [u128]) -> Option<Latency> {
    if samples.is_empty() {
        return None;
    }
    samples.sort_unstable();
    let len = samples.len();
    let sum: u128 = samples.iter().sum();
    Some(Latency {
        operation_count: len,
        min_ns: samples[0],
        max_ns: samples[len - 1],
        mean_ns: sum as f64 / len as f64,
        p50_ns: percentile(samples, 0.50),
        p95_ns: percentile(samples, 0.95),
        p99_ns: percentile(samples, 0.99),
    })
}
pub fn calculate_measured(_warmup: &[u128], measured: &mut [u128]) -> Option<Latency> {
    calculate(measured)
}
fn percentile(sorted: &[u128], percentile: f64) -> u128 {
    sorted[((sorted.len() as f64 * percentile).ceil() as usize).saturating_sub(1)]
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn percentile_stats_are_sample_based() {
        let mut values = (1..=100).collect::<Vec<_>>();
        let stats = calculate(&mut values).unwrap();
        assert_eq!((stats.p50_ns, stats.p95_ns, stats.p99_ns), (50, 95, 99));
    }

    #[test]
    fn warmup_samples_are_not_included_in_measured_statistics() {
        let warmup = [1, 2];
        let mut measured = [100, 200];
        let stats = calculate_measured(&warmup, &mut measured).unwrap();
        assert_eq!(stats.operation_count, 2);
        assert_eq!(stats.min_ns, 100);
    }
}
