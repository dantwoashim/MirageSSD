use mirage_cache::{CapacitySnapshot, ReclaimCandidate, ReclaimId, plan_space_lease};
use mirage_types::MirageErrorKind;

fn hash(value: u8) -> ReclaimId {
    ReclaimId::from_bytes([value; 32])
}

fn candidate(value: u8, physical_bytes: u64, last_access_sequence: u64) -> ReclaimCandidate {
    ReclaimCandidate {
        reclaim_id: hash(value),
        physical_bytes,
        last_access_sequence,
        remote_verified: true,
        dirty: false,
        pinned: false,
        active_read_leases: 0,
    }
}

#[test]
fn physical_free_space_satisfies_a_lease_without_eviction() {
    let plan = plan_space_lease(
        &CapacitySnapshot {
            physical_free_bytes: 100,
            filesystem_reserve_bytes: 20,
            outstanding_space_lease_bytes: 10,
            candidates: vec![candidate(1, 40, 1)],
        },
        60,
    )
    .expect("plan");

    assert!(plan.grantable());
    assert_eq!(plan.immediately_available_bytes, 70);
    assert_eq!(plan.reclaim_required_bytes, 0);
    assert_eq!(plan.total_reclaimable_bytes, 40);
    assert!(plan.selected.is_empty());
}

#[test]
fn only_verified_clean_unpinned_idle_bytes_are_selected() {
    let mut dirty = candidate(2, 20, 1);
    dirty.dirty = true;
    let mut unverified = candidate(3, 30, 2);
    unverified.remote_verified = false;
    let mut pinned = candidate(4, 40, 3);
    pinned.pinned = true;
    let mut active = candidate(5, 50, 4);
    active.active_read_leases = 1;

    let plan = plan_space_lease(
        &CapacitySnapshot {
            physical_free_bytes: 25,
            filesystem_reserve_bytes: 10,
            outstanding_space_lease_bytes: 5,
            candidates: vec![dirty, unverified, pinned, active, candidate(6, 70, 5)],
        },
        80,
    )
    .expect("plan");

    assert!(plan.grantable());
    assert_eq!(plan.immediately_available_bytes, 10);
    assert_eq!(plan.selected_reclaim_bytes, 70);
    assert_eq!(plan.selected.len(), 1);
    assert_eq!(plan.selected[0].reclaim_id, hash(6));
    assert_eq!(plan.blocked.unique_bytes, 140);
    assert_eq!(plan.blocked.dirty_bytes, 20);
    assert_eq!(plan.blocked.unverified_bytes, 30);
    assert_eq!(plan.blocked.pinned_bytes, 40);
    assert_eq!(plan.blocked.active_read_bytes, 50);
}

#[test]
fn selection_is_oldest_then_largest_then_hash_deterministic() {
    let plan = plan_space_lease(
        &CapacitySnapshot {
            physical_free_bytes: 0,
            filesystem_reserve_bytes: 0,
            outstanding_space_lease_bytes: 0,
            candidates: vec![
                candidate(3, 20, 10),
                candidate(2, 30, 10),
                candidate(1, 30, 10),
                candidate(4, 5, 5),
            ],
        },
        36,
    )
    .expect("plan");

    let selected: Vec<_> = plan.selected.iter().map(|entry| entry.reclaim_id).collect();
    assert_eq!(selected, vec![hash(4), hash(1), hash(2)]);
    assert_eq!(plan.selected_reclaim_bytes, 65);
}

#[test]
fn denial_reports_exact_shortfall_without_counting_blocked_bytes() {
    let mut blocked = candidate(1, 500, 1);
    blocked.remote_verified = false;
    let plan = plan_space_lease(
        &CapacitySnapshot {
            physical_free_bytes: 50,
            filesystem_reserve_bytes: 20,
            outstanding_space_lease_bytes: 10,
            candidates: vec![blocked, candidate(2, 25, 2)],
        },
        100,
    )
    .expect("plan");

    assert!(!plan.grantable());
    assert_eq!(plan.immediately_available_bytes, 20);
    assert_eq!(plan.total_reclaimable_bytes, 25);
    assert_eq!(plan.shortfall_bytes, 55);
    assert_eq!(plan.selected_reclaim_bytes, 25);
}

#[test]
fn duplicate_identity_and_accounting_overflow_fail_closed() {
    let duplicate = candidate(1, 1, 0);
    let error = plan_space_lease(
        &CapacitySnapshot {
            physical_free_bytes: 0,
            filesystem_reserve_bytes: 0,
            outstanding_space_lease_bytes: 0,
            candidates: vec![duplicate, duplicate],
        },
        1,
    )
    .expect_err("duplicate must fail");
    assert_eq!(error.kind, MirageErrorKind::InvalidArgument);

    let error = plan_space_lease(
        &CapacitySnapshot {
            physical_free_bytes: 1,
            filesystem_reserve_bytes: 0,
            outstanding_space_lease_bytes: 0,
            candidates: vec![candidate(1, u64::MAX, 0)],
        },
        2,
    )
    .expect_err("sum overflow must fail");
    assert_eq!(error.kind, MirageErrorKind::InvalidArgument);
}

#[test]
fn zero_byte_requests_are_rejected() {
    let error = plan_space_lease(
        &CapacitySnapshot {
            physical_free_bytes: 1,
            filesystem_reserve_bytes: 0,
            outstanding_space_lease_bytes: 0,
            candidates: Vec::new(),
        },
        0,
    )
    .expect_err("zero-byte lease must fail");
    assert_eq!(error.kind, MirageErrorKind::InvalidArgument);
}
