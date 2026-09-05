#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThroughputSample {
    pub bytes: u64,
    pub elapsed_ns: u64,
}
impl ThroughputSample {
    #[must_use]
    pub fn bytes_per_second(self) -> u64 {
        if self.elapsed_ns == 0 {
            return 0;
        }
        ((u128::from(self.bytes) * 1_000_000_000) / u128::from(self.elapsed_ns))
            .min(u128::from(u64::MAX)) as u64
    }
}
