#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriveFault {
    Unauthorized,
    RateLimited { retry_after_seconds: u32 },
    Disconnect,
    ShortBody,
    Success,
}

#[derive(Debug, Clone)]
pub struct DriveScenario {
    seed: u64,
    cursor: u64,
}

impl DriveScenario {
    #[must_use]
    pub const fn new(seed: u64) -> Self {
        Self { seed, cursor: 0 }
    }

    #[must_use]
    pub const fn resume(seed: u64, cursor: u64) -> Self {
        Self { seed, cursor }
    }

    #[must_use]
    pub const fn cursor(&self) -> u64 {
        self.cursor
    }

    /// Produces a deterministic sparse failure schedule. Every injected failure
    /// is followed by success so retry behavior remains bounded and observable.
    pub fn next_action(&mut self) -> DriveFault {
        let operation = self.cursor;
        self.cursor = self.cursor.saturating_add(1);
        if operation != 0 && operation.is_multiple_of(4093) {
            DriveFault::ShortBody
        } else if operation != 0 && operation.is_multiple_of(2053) {
            DriveFault::Disconnect
        } else if operation != 0 && operation.is_multiple_of(1021) {
            DriveFault::RateLimited {
                retry_after_seconds: 1 + (self.seed as u32 % 5),
            }
        } else if operation != 0 && operation.is_multiple_of(509) {
            DriveFault::Unauthorized
        } else {
            DriveFault::Success
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schedule_replays_and_resumes_exactly() {
        let mut uninterrupted = DriveScenario::new(77);
        let expected: Vec<_> = (0..10_000).map(|_| uninterrupted.next_action()).collect();
        let mut first = DriveScenario::new(77);
        let mut resumed = Vec::new();
        resumed.extend((0..4_321).map(|_| first.next_action()));
        let mut second = DriveScenario::resume(77, first.cursor());
        resumed.extend((4_321..10_000).map(|_| second.next_action()));
        assert_eq!(resumed, expected);
        assert!(expected.contains(&DriveFault::Unauthorized));
        assert!(expected.contains(&DriveFault::Disconnect));
        assert!(expected.contains(&DriveFault::ShortBody));
    }
}
