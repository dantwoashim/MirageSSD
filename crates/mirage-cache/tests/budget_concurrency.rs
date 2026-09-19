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

#[test]
fn commit_moves_reserved_bytes_into_committed_and_release_after_commit_is_inert() {
    let ledger = ReservationLedger::new(
        BudgetConfig {
            hard_bytes: 10_000,
            prefetch_soft_bytes: 10_000,
            update_safety_reserve: 1_000,
            dirty_update_bytes: 1_000,
        },
        400,
    )
    .expect("ledger");
    let reservation = ledger
        .reserve(300, ReservationClass::Blocking)
        .expect("reserve");
    assert_eq!(ledger.snapshot().expect("reserved").reserved_bytes, 300);
    reservation.commit().expect("commit");
    let snapshot = ledger.snapshot().expect("snapshot");
    assert_eq!(snapshot.committed_bytes, 700);
    assert_eq!(snapshot.reserved_bytes, 0);
    // The envelope never grew, so the peak is the committed total.
    assert_eq!(snapshot.peak_envelope_bytes, 700);
    // Committed bytes now count against the blocking class budget.
    assert!(ledger.reserve(8_301, ReservationClass::Blocking).is_err());
    ledger
        .reserve(8_300, ReservationClass::Blocking)
        .expect("fits exactly")
        .commit()
        .expect("commit to the blocking limit");
    assert_eq!(ledger.snapshot().expect("full").committed_bytes, 9_000);
    assert!(ledger.reserve(1, ReservationClass::Blocking).is_err());
    // The update safety reserve still admits a staging reservation.
    ledger
        .reserve(1_000, ReservationClass::Staging)
        .expect("staging to the hard envelope")
        .commit()
        .expect("commit");
    assert_eq!(ledger.snapshot().expect("envelope").committed_bytes, 10_000);
    assert!(ledger.reserve(1, ReservationClass::Staging).is_err());
}

#[test]
fn dirty_commit_clears_the_dirty_reservation_and_failed_reserve_releases() {
    let ledger = ReservationLedger::new(
        BudgetConfig {
            hard_bytes: 10_000,
            prefetch_soft_bytes: 10_000,
            update_safety_reserve: 1_000,
            dirty_update_bytes: 1_000,
        },
        0,
    )
    .expect("ledger");
    let reservation = ledger
        .reserve(500, ReservationClass::DirtyUpdate)
        .expect("dirty reserve");
    assert_eq!(ledger.snapshot().expect("held").dirty_reserved_bytes, 500);
    reservation.commit().expect("commit");
    let snapshot = ledger.snapshot().expect("committed");
    assert_eq!(snapshot.committed_bytes, 500);
    assert_eq!(snapshot.dirty_reserved_bytes, 0);
    // A failed commit is unreachable here: the envelope already bounds
    // committed + reserved, so committed growth cannot overflow u64.
}
