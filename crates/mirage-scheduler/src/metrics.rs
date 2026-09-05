#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SchedulerMetrics {
    pub attempts: u64,
    pub retries: u64,
    pub permanent_failures: u64,
    pub rate_limits: u64,
}
