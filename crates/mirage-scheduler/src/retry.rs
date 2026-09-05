use std::time::Duration;

use mirage_backend::{BackendError, BackendErrorClass};

use crate::FetchPriority;
use crate::backoff::Jitter;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryDecision {
    RetryAfter(Duration),
    Stop,
}
pub struct RetryPolicy {
    jitter: Jitter,
}
impl RetryPolicy {
    #[must_use]
    pub const fn new(seed: u64) -> Self {
        Self {
            jitter: Jitter::new(seed),
        }
    }
    pub fn decide(
        &mut self,
        class: FetchPriority,
        attempt: u32,
        elapsed: Duration,
        error: &BackendError,
    ) -> RetryDecision {
        let retryable = matches!(
            error.class,
            BackendErrorClass::RateLimit | BackendErrorClass::TransientTransport
        );
        let (max_attempts, max_elapsed) = if class == FetchPriority::P0Blocking {
            (3, Duration::from_secs(2))
        } else if class.speculative() {
            (5, Duration::from_secs(30))
        } else {
            (6, Duration::from_secs(60))
        };
        if !retryable || attempt >= max_attempts || elapsed >= max_elapsed {
            return RetryDecision::Stop;
        }
        let jitter = self
            .jitter
            .delay(attempt, Duration::from_millis(50), Duration::from_secs(5));
        RetryDecision::RetryAfter(error.retry_after.unwrap_or(jitter).max(jitter))
    }
}
