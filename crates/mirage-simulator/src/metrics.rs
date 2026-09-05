use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReplayMetrics {
    pub accesses: u64,
    pub hits: u64,
    pub misses: u64,
    pub hit_rate: f64,
    pub blocking_ns: u64,
    pub p95_stall_ns: u64,
    pub p99_stall_ns: u64,
    pub remote_bytes: u64,
    pub cache_writes: u64,
    pub eviction_before_reuse: u64,
    pub peak_resident_pages: u64,
}

pub(crate) fn percentile(values: &mut [u64], percentile: u64) -> u64 {
    if values.is_empty() {
        return 0;
    }
    values.sort_unstable();
    let rank = (values.len() as u64 * percentile)
        .div_ceil(100)
        .saturating_sub(1) as usize;
    values[rank.min(values.len() - 1)]
}
