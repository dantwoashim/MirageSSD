use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use mirage_scheduler::{FetchPool, FetchPoolConfig, FetchPriority};
use mirage_types::MirageErrorKind;

fn wait_until(deadline: Instant, condition: impl Fn() -> bool) {
    while !condition() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn pool_runs_queued_jobs_in_priority_order() {
    let pool = FetchPool::new(FetchPoolConfig {
        workers: 1,
        queue_depth: 8,
        speculative_queue_depth: 4,
    })
    .expect("pool");
    let order = Arc::new(Mutex::new(Vec::<u8>::new()));
    let (gate_tx, gate_rx) = std::sync::mpsc::channel::<()>();
    pool.spawn(
        FetchPriority::P0Blocking,
        u64::MAX,
        Box::new(move || {
            let _ = gate_rx.recv();
        }),
    )
    .expect("gate job");
    wait_until(Instant::now() + Duration::from_secs(5), || {
        pool.running() == 1
    });
    for priority in [
        FetchPriority::P5IdleWarm,
        FetchPriority::P0Blocking,
        FetchPriority::P2Capsule,
    ] {
        let order = Arc::clone(&order);
        pool.spawn(
            priority,
            u64::MAX,
            Box::new(move || order.lock().expect("order").push(priority as u8)),
        )
        .expect("queued job");
    }
    gate_tx.send(()).expect("open gate");
    wait_until(Instant::now() + Duration::from_secs(5), || {
        pool.completed() == 4
    });
    assert_eq!(*order.lock().expect("order"), vec![0, 2, 5]);
}

#[test]
fn speculative_credits_never_starve_demand() {
    let pool = FetchPool::new(FetchPoolConfig {
        workers: 1,
        queue_depth: 4,
        speculative_queue_depth: 1,
    })
    .expect("pool");
    let (gate_tx, gate_rx) = std::sync::mpsc::channel::<()>();
    pool.spawn(
        FetchPriority::P0Blocking,
        u64::MAX,
        Box::new(move || {
            let _ = gate_rx.recv();
        }),
    )
    .expect("gate job");
    wait_until(Instant::now() + Duration::from_secs(5), || {
        pool.running() == 1
    });
    pool.spawn(FetchPriority::P4ReadAhead, u64::MAX, Box::new(|| {}))
        .expect("first speculative job fits the credit");
    let error = pool
        .spawn(FetchPriority::P4ReadAhead, u64::MAX, Box::new(|| {}))
        .expect_err("second speculative job exceeds the credit");
    assert_eq!(error.kind, MirageErrorKind::CacheFull);
    pool.spawn(FetchPriority::P0Blocking, u64::MAX, Box::new(|| {}))
        .expect("demand is not starved by spent speculative credits");
    gate_tx.send(()).expect("open gate");
    wait_until(Instant::now() + Duration::from_secs(5), || {
        pool.completed() == 3
    });
    assert_eq!(pool.completed(), 3);
}

#[test]
fn demand_jobs_still_hit_the_queue_depth_bound() {
    let pool = FetchPool::new(FetchPoolConfig {
        workers: 1,
        queue_depth: 1,
        speculative_queue_depth: 0,
    })
    .expect("pool");
    let (gate_tx, gate_rx) = std::sync::mpsc::channel::<()>();
    pool.spawn(
        FetchPriority::P0Blocking,
        u64::MAX,
        Box::new(move || {
            let _ = gate_rx.recv();
        }),
    )
    .expect("gate job");
    wait_until(Instant::now() + Duration::from_secs(5), || {
        pool.running() == 1
    });
    pool.spawn(FetchPriority::P0Blocking, u64::MAX, Box::new(|| {}))
        .expect("one queued demand job");
    let error = pool
        .spawn(FetchPriority::P0Blocking, u64::MAX, Box::new(|| {}))
        .expect_err("demand queue is bounded");
    assert_eq!(error.kind, MirageErrorKind::CacheFull);
    gate_tx.send(()).expect("open gate");
    wait_until(Instant::now() + Duration::from_secs(5), || {
        pool.completed() == 2
    });
    assert_eq!(pool.completed(), 2);
}

#[test]
fn a_panicking_job_never_kills_the_worker_or_leaks_running() {
    let pool = FetchPool::new(FetchPoolConfig {
        workers: 1,
        queue_depth: 4,
        speculative_queue_depth: 0,
    })
    .expect("pool");
    pool.spawn(
        FetchPriority::P0Blocking,
        u64::MAX,
        Box::new(|| panic!("fetch job panic")),
    )
    .expect("panicking job");
    let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
    pool.spawn(
        FetchPriority::P0Blocking,
        u64::MAX,
        Box::new(move || {
            let _ = done_tx.send(());
        }),
    )
    .expect("sentinel job");
    done_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("worker survived the panic");
    wait_until(Instant::now() + Duration::from_secs(5), || {
        pool.running() == 0 && pool.completed() == 2
    });
    assert_eq!(pool.panicked(), 1);
    assert_eq!(pool.completed(), 2);
    assert_eq!(pool.running(), 0);
}
