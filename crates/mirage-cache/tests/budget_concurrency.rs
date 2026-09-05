use mirage_cache::{BudgetConfig, ReservationClass, ReservationLedger};
use std::sync::{Arc, Barrier};

#[test]
fn concurrent_reservations_never_cross_hard_envelope_and_cancel_cleanly() {
    let ledger = Arc::new(
        ReservationLedger::new(
            BudgetConfig {
                hard_bytes: 10_000,
                prefetch_soft_bytes: 7_000,
                update_safety_reserve: 1_000,
                dirty_update_bytes: 1_000,
            },
            0,
        )
        .expect("ledger"),
    );
    let barrier = Arc::new(Barrier::new(101));
    let mut workers = Vec::new();
    for _ in 0..100 {
        let ledger = Arc::clone(&ledger);
        let barrier = Arc::clone(&barrier);
        workers.push(std::thread::spawn(move || {
            barrier.wait();
            ledger.reserve(100, ReservationClass::Blocking).ok()
        }));
    }
    barrier.wait();
    let reservations = workers
        .into_iter()
        .filter_map(|worker| worker.join().expect("worker"))
        .collect::<Vec<_>>();
    let held = ledger.snapshot().expect("snapshot");
    assert!(held.committed_bytes + held.reserved_bytes <= 9_000);
    assert_eq!(held.reserved_bytes, reservations.len() as u64 * 100);
    drop(reservations);
    assert_eq!(ledger.snapshot().expect("released").reserved_bytes, 0);
    assert!(held.peak_envelope_bytes <= 9_000);
}

#[test]
fn priority_reserves_soft_watermarks_and_overflow_are_enforced() {
    let ledger = ReservationLedger::new(
        BudgetConfig {
            hard_bytes: 1000,
            prefetch_soft_bytes: 500,
            update_safety_reserve: 200,
            dirty_update_bytes: 200,
        },
        400,
    )
    .expect("ledger");
    assert!(ledger.reserve(101, ReservationClass::Prefetch).is_err());
    let blocking = ledger
        .reserve(400, ReservationClass::Blocking)
        .expect("blocking to boundary");
    assert!(ledger.reserve(1, ReservationClass::Blocking).is_err());
    let dirty = ledger
        .reserve(200, ReservationClass::DirtyUpdate)
        .expect("dirty reserve");
    assert!(ledger.reserve(1, ReservationClass::DirtyUpdate).is_err());
    drop((blocking, dirty));
    assert!(
        ReservationLedger::new(
            BudgetConfig {
                hard_bytes: u64::MAX,
                prefetch_soft_bytes: u64::MAX,
                update_safety_reserve: 0,
                dirty_update_bytes: 0
            },
            u64::MAX - 1
        )
        .expect("large")
        .reserve(2, ReservationClass::Capsule)
        .is_err()
    );
}
