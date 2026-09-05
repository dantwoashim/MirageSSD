use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Default)]
pub struct Metrics {
    read_misses: AtomicU64,
    seal_violations: AtomicU64,
    backend_retries: AtomicU64,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MetricSnapshot {
    pub read_misses: u64,
    pub seal_violations: u64,
    pub backend_retries: u64,
}
impl Metrics {
    pub fn read_miss(&self) {
        self.read_misses.fetch_add(1, Ordering::Relaxed);
    }
    pub fn seal_violation(&self) {
        self.seal_violations.fetch_add(1, Ordering::Relaxed);
    }
    pub fn backend_retry(&self) {
        self.backend_retries.fetch_add(1, Ordering::Relaxed);
    }
    #[must_use]
    pub fn snapshot(&self) -> MetricSnapshot {
        MetricSnapshot {
            read_misses: self.read_misses.load(Ordering::Relaxed),
            seal_violations: self.seal_violations.load(Ordering::Relaxed),
            backend_retries: self.backend_retries.load(Ordering::Relaxed),
        }
    }
}
