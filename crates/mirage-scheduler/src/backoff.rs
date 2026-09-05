use std::time::Duration;

pub struct Jitter {
    state: u64,
}
impl Jitter {
    #[must_use]
    pub const fn new(seed: u64) -> Self {
        Self {
            state: if seed == 0 { 1 } else { seed },
        }
    }
    pub fn delay(&mut self, attempt: u32, base: Duration, cap: Duration) -> Duration {
        self.state ^= self.state << 13;
        self.state ^= self.state >> 7;
        self.state ^= self.state << 17;
        let ceiling = base
            .as_millis()
            .saturating_mul(1_u128 << attempt.min(20))
            .min(cap.as_millis());
        if ceiling == 0 {
            return Duration::ZERO;
        }
        Duration::from_millis((u128::from(self.state) % (ceiling + 1)) as u64)
    }
}
