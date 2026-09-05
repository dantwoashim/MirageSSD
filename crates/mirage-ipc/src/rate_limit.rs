/// Deterministic per-principal token bucket for expensive IPC commands.
pub struct TokenBucket {
    capacity: u32,
    tokens: u32,
    refill_every_ms: u64,
    last_refill_ms: u64,
}
impl TokenBucket {
    #[must_use]
    pub const fn new(capacity: u32, refill_every_ms: u64, now_ms: u64) -> Self {
        Self {
            capacity,
            tokens: capacity,
            refill_every_ms,
            last_refill_ms: now_ms,
        }
    }
    pub fn allow_at(&mut self, now_ms: u64) -> bool {
        if self.capacity == 0 || self.refill_every_ms == 0 {
            return false;
        }
        let elapsed = now_ms.saturating_sub(self.last_refill_ms);
        let refill = elapsed / self.refill_every_ms;
        if refill != 0 {
            self.tokens = self.capacity.min(
                self.tokens
                    .saturating_add(u32::try_from(refill).unwrap_or(u32::MAX)),
            );
            self.last_refill_ms = self
                .last_refill_ms
                .saturating_add(refill.saturating_mul(self.refill_every_ms));
        }
        if self.tokens == 0 {
            false
        } else {
            self.tokens -= 1;
            true
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn enforces_burst_and_refill() {
        let mut bucket = TokenBucket::new(2, 100, 0);
        assert!(bucket.allow_at(0));
        assert!(bucket.allow_at(0));
        assert!(!bucket.allow_at(99));
        assert!(bucket.allow_at(100));
    }
}
