use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use mirage_scheduler::{FetchPool, FetchPoolConfig, FetchPriority, FlightFailure, FlightMap};
use mirage_types::{FetchFailureCause, MirageErrorKind, PageHash};
use tokio_util::sync::CancellationToken;

fn failure() -> FlightFailure {
    FlightFailure {
        cause: FetchFailureCause::Internal,
        code: "MIRAGE_REMOTE_UNAVAILABLE".into(),
    }
}

#[test]
fn thousand_waiters_share_one_owner_and_observe_completion() {
    let map = Arc::new(FlightMap::default());
    let owners = Arc::new(AtomicUsize::new(0));
    let hash = PageHash::from_bytes([8; 32]);
    let barrier = Arc::new(std::sync::Barrier::new(1001));
    let threads = (0..1000)
        .map(|_| {
            let map = Arc::clone(&map);
            let owners = Arc::clone(&owners);
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                let acquired = map
                    .acquire(hash, FetchPriority::P4ReadAhead, 100, true)
                    .expect("acquire");
                owners.fetch_add(usize::from(acquired.owner), Ordering::Relaxed);
                barrier.wait();
                acquired.handle.flight().wait()
            })
        })
        .collect::<Vec<_>>();
    barrier.wait();
    assert_eq!(owners.load(Ordering::Relaxed), 1);
    assert!(map.complete(hash, Ok(())).expect("complete"));
    for thread in threads {
        assert_eq!(thread.join().expect("thread"), Ok(()));
    }
    assert!(map.is_empty());
}

#[test]
fn errors_fan_out_and_retry_gets_new_owner() {
    let map = FlightMap::default();
    let hash = PageHash::from_bytes([9; 32]);
    let first = map
        .acquire(hash, FetchPriority::P5IdleWarm, 500, false)
        .expect("first");
    let second = map
        .acquire(hash, FetchPriority::P0Blocking, 10, true)
        .expect("second");
    assert!(first.owner);
    assert!(!second.owner);
    assert_eq!(
        first.handle.flight().priority(),
        FetchPriority::P0Blocking as u8
    );
    assert_eq!(first.handle.flight().earliest_deadline_ns(), 10);
    let failure = failure();
    assert!(map.complete(hash, Err(failure.clone())).expect("complete"));
    assert_eq!(second.handle.flight().wait(), Err(failure));
    assert!(
        map.acquire(hash, FetchPriority::P0Blocking, 5, true)
            .expect("retry")
            .owner
    );
}

#[test]
fn wait_or_cancel_returns_none_when_cancelled_first() {
    let map = FlightMap::default();
    let hash = PageHash::from_bytes([10; 32]);
    let acquired = map
        .acquire(hash, FetchPriority::P0Blocking, 10, true)
        .expect("acquire");
    let token = CancellationToken::new();
    token.cancel();
    assert_eq!(
        futures_executor::block_on(acquired.handle.flight().wait_or_cancel(&token)),
        None
    );
}

#[test]
fn acquire_after_cancelled_flight_creates_new_owner() {
    let map = FlightMap::default();
    let hash = PageHash::from_bytes([11; 32]);
    {
        let acquired = map
            .acquire(hash, FetchPriority::P0Blocking, 10, true)
            .expect("acquire");
        assert!(acquired.owner);
        let flight = Arc::clone(acquired.handle.flight());
        drop(acquired);
        assert!(flight.cancellation().is_cancelled());
    }
    let next = map
        .acquire(hash, FetchPriority::P0Blocking, 10, true)
        .expect("re-acquire");
    assert!(next.owner);
    assert_eq!(map.len(), 1);
}

#[test]
fn subscriber_limit_is_enforced() {
    let map = FlightMap::with_limits(2);
    let hash = PageHash::from_bytes([12; 32]);
    let _first = map
        .acquire(hash, FetchPriority::P0Blocking, 10, true)
        .expect("first");
    let _second = map
        .acquire(hash, FetchPriority::P0Blocking, 10, true)
        .expect("second");
    let error = match map.acquire(hash, FetchPriority::P0Blocking, 10, true) {
        Ok(_) => panic!("third subscriber must be rejected"),
        Err(error) => error,
    };
    assert_eq!(error.kind, MirageErrorKind::CacheFull);
}

#[test]
fn complete_owned_ignores_a_replaced_flight() {
    let map = FlightMap::default();
    let hash = PageHash::from_bytes([13; 32]);
    let stale = {
        let acquired = map
            .acquire(hash, FetchPriority::P0Blocking, 10, true)
            .expect("stale owner");
        let flight = Arc::clone(acquired.handle.flight());
        // Dropping the only waiter cancels the flight; the next acquire
        // replaces the map entry with a fresh owner flight.
        drop(acquired);
        flight
    };
    assert!(stale.cancellation().is_cancelled());
    let current = map
        .acquire(hash, FetchPriority::P0Blocking, 10, true)
        .expect("current owner");
    let current_flight = Arc::clone(current.handle.flight());
    assert!(current.owner);
    assert!(!Arc::ptr_eq(&stale, &current_flight));

    // Completing a flight the map no longer references still resolves the
    // flight but leaves the current entry alone.
    assert!(
        !map.complete_owned(hash, &stale, Ok(()))
            .expect("complete stale")
    );
    assert!(stale.try_result().is_some());
    assert_eq!(map.len(), 1);
    assert!(current_flight.try_result().is_none());
    assert!(
        map.complete_owned(hash, &current_flight, Ok(()))
            .expect("complete current")
    );
    assert!(map.is_empty());
}

#[test]
fn fetch_pool_reports_saturation() {
    let pool = FetchPool::new(FetchPoolConfig {
        workers: 1,
        queue_depth: 1,
        speculative_queue_depth: 0,
        max_in_flight_bytes: 0,
    })
    .expect("pool");
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    pool.spawn(
        FetchPriority::P0Blocking,
        u64::MAX,
        Box::new(move || {
            let _ = release_rx.recv();
        }),
    )
    .expect("blocking job");
    let deadline = Instant::now() + Duration::from_secs(5);
    while pool.running() == 0 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(pool.running(), 1);
    let mut accepted = 1_usize;
    loop {
        match pool.spawn(FetchPriority::P0Blocking, u64::MAX, Box::new(|| {})) {
            Ok(()) => accepted += 1,
            Err(error) => {
                assert_eq!(error.kind, MirageErrorKind::CacheFull);
                break;
            }
        }
        assert!(accepted <= 16, "pool never saturated");
    }
    release_tx.send(()).expect("release worker");
    let deadline = Instant::now() + Duration::from_secs(5);
    while pool.completed() < accepted as u64 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(pool.completed(), accepted as u64);
}

#[test]
fn expired_deadline_jobs_never_run() {
    use std::sync::mpsc;
    let (release, hold) = mpsc::channel::<()>();
    let pool = FetchPool::new(FetchPoolConfig {
        workers: 1,
        queue_depth: 8,
        speculative_queue_depth: 0,
        max_in_flight_bytes: 0,
    })
    .expect("pool");
    // Block the only worker so the expired job sits in the queue.
    pool.spawn(
        mirage_scheduler::FetchPriority::P0Blocking,
        u64::MAX,
        Box::new(move || {
            let _ = hold.recv();
        }),
    )
    .expect("blocker");
    let ran = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flag = std::sync::Arc::clone(&ran);
    pool.spawn(
        mirage_scheduler::FetchPriority::P0Blocking,
        1, // already in the past
        Box::new(move || {
            flag.store(true, Ordering::SeqCst);
        }),
    )
    .expect("expired job");
    let start = std::time::Instant::now();
    while pool.queued() < 1 && start.elapsed() < std::time::Duration::from_secs(5) {
        std::thread::yield_now();
    }
    release.send(()).ok();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while pool.completed() < 1 && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(!ran.load(Ordering::SeqCst));
    assert_eq!(pool.expired(), 1);
}

#[test]
fn speculative_workers_cannot_occupy_every_slot() {
    use std::sync::mpsc;
    let (hold_tx, hold_rx) = mpsc::channel::<()>();
    let hold_rx = std::sync::Arc::new(std::sync::Mutex::new(hold_rx));
    let pool = FetchPool::new(FetchPoolConfig {
        workers: 2,
        queue_depth: 8,
        speculative_queue_depth: 4,
        max_in_flight_bytes: 0,
    })
    .expect("pool");
    // Two speculative jobs occupy both workers.
    for _ in 0..2 {
        let hold_rx = std::sync::Arc::clone(&hold_rx);
        pool.spawn(
            mirage_scheduler::FetchPriority::P4ReadAhead,
            u64::MAX,
            Box::new(move || {
                let _ = hold_rx.lock().expect("hold lock").recv();
            }),
        )
        .expect("speculative");
    }
    // A third speculative job must park: the protected demand slot is not
    // consumed even though a worker appears free.
    let third_ran = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flag = std::sync::Arc::clone(&third_ran);
    pool.spawn(
        mirage_scheduler::FetchPriority::P4ReadAhead,
        u64::MAX,
        Box::new(move || {
            flag.store(true, Ordering::SeqCst);
        }),
    )
    .expect("third speculative");
    // A demand job still runs while speculative work saturates.
    let (done_tx, done_rx) = mpsc::channel::<()>();
    pool.spawn(
        mirage_scheduler::FetchPriority::P0Blocking,
        u64::MAX,
        Box::new(move || {
            done_tx.send(()).ok();
        }),
    )
    .expect("demand job");
    assert!(
        done_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .is_ok(),
        "demand read starved behind speculative workers"
    );
    drop(hold_tx);
}

#[test]
fn speculative_byte_budget_rejects_over_limit() {
    let pool = FetchPool::new(FetchPoolConfig {
        workers: 1,
        queue_depth: 8,
        speculative_queue_depth: 8,
        max_in_flight_bytes: 1024,
    })
    .expect("pool");
    pool.spawn_metered(
        mirage_scheduler::FetchPriority::P4ReadAhead,
        u64::MAX,
        800,
        Box::new(|| {}),
    )
    .expect("within budget");
    let error = pool
        .spawn_metered(
            mirage_scheduler::FetchPriority::P4ReadAhead,
            u64::MAX,
            800,
            Box::new(|| {}),
        )
        .expect_err("over budget");
    assert_eq!(error.kind, mirage_types::MirageErrorKind::CacheFull);
}
