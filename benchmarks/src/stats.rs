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
}
