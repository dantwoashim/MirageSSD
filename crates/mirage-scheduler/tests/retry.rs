use mirage_backend::{BackendError, BackendErrorClass};
use mirage_scheduler::{
    ConcurrencyController, ControllerInput, FetchPriority, RetryDecision, RetryPolicy,
};
use std::time::Duration;

#[test]
fn retry_is_deterministic_bounded_and_honors_retry_after() {
    let error = BackendError::new(BackendErrorClass::TransientTransport, "temporary");
    let mut a = RetryPolicy::new(7);
    let mut b = RetryPolicy::new(7);
    assert_eq!(
        a.decide(FetchPriority::P0Blocking, 0, Duration::ZERO, &error),
        b.decide(FetchPriority::P0Blocking, 0, Duration::ZERO, &error)
    );
    assert_eq!(
        a.decide(FetchPriority::P0Blocking, 3, Duration::ZERO, &error),
        RetryDecision::Stop
    );
    let rate = BackendError::new(BackendErrorClass::RateLimit, "slow")
        .with_retry_after(Duration::from_secs(9));
    assert_eq!(
        a.decide(FetchPriority::P4ReadAhead, 0, Duration::ZERO, &rate),
        RetryDecision::RetryAfter(Duration::from_secs(9))
    );
    let permanent = BackendError::new(BackendErrorClass::Permanent, "no");
    assert_eq!(
        a.decide(FetchPriority::P0Blocking, 0, Duration::ZERO, &permanent),
        RetryDecision::Stop
    );
}

#[test]
fn controller_starts_conservative_and_rate_limit_reduces_work() {
    let mut controller = ConcurrencyController::new(2, 8, 1024 * 1024);
    let normal = ControllerInput {
        ttfb_ms: 10,
        throughput_bytes_per_second: 10 * 1024 * 1024,
        error: false,
        rate_limited: false,
        active_p0: 1,
        cache_write_depth: 0,
    };
    for _ in 0..7 {
        assert_eq!(controller.observe(normal).concurrency, 2);
    }
    assert_eq!(controller.observe(normal).concurrency, 3);
    let limited = ControllerInput {
        rate_limited: true,
        ..normal
    };
    let result = controller.observe(limited);
    assert_eq!(result.concurrency, 2);
    assert!(result.speculation_paused);
}
