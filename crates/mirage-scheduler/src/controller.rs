#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ControllerInput {
    pub ttfb_ms: u64,
    pub throughput_bytes_per_second: u64,
    pub error: bool,
    pub rate_limited: bool,
    pub active_p0: u32,
    pub cache_write_depth: u32,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ControllerRecommendation {
    pub concurrency: u32,
    pub window_bytes: u64,
    pub speculation_paused: bool,
}
pub struct ConcurrencyController {
    samples: u32,
    ttfb_ewma: u64,
    throughput_ewma: u64,
    concurrency: u32,
    min: u32,
    max: u32,
    window: u64,
}
impl ConcurrencyController {
    #[must_use]
    pub const fn new(min: u32, max: u32, initial_window: u64) -> Self {
        Self {
            samples: 0,
            ttfb_ewma: 0,
            throughput_ewma: 0,
            concurrency: min,
            min,
            max,
            window: initial_window,
        }
    }
    pub fn observe(&mut self, input: ControllerInput) -> ControllerRecommendation {
        self.samples = self.samples.saturating_add(1);
        self.ttfb_ewma = ewma(self.ttfb_ewma, input.ttfb_ms, self.samples);
        self.throughput_ewma = ewma(
            self.throughput_ewma,
            input.throughput_bytes_per_second,
            self.samples,
        );
        let constrained = input.rate_limited
            || input.error
            || input.cache_write_depth > self.concurrency.saturating_mul(2);
        if constrained {
            self.concurrency = self.concurrency.saturating_sub(1).max(self.min);
            self.window = (self.window / 2).max(64 * 1024);
        } else if self.samples >= 8 && input.active_p0 > 0 && self.throughput_ewma > 1024 * 1024 {
            self.concurrency = self.concurrency.saturating_add(1).min(self.max);
            self.window = self.window.saturating_add(64 * 1024).min(32 * 1024 * 1024);
        }
        ControllerRecommendation {
            concurrency: self.concurrency,
            window_bytes: self.window,
            speculation_paused: input.rate_limited
                || input.active_p0 > 0 && self.concurrency == self.min,
        }
    }
}
const fn ewma(old: u64, new: u64, samples: u32) -> u64 {
    if samples == 1 {
        new
    } else {
        old.saturating_mul(7).saturating_add(new) / 8
    }
}
