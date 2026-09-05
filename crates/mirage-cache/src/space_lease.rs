use std::cmp::Reverse;
use std::collections::BTreeSet;

use mirage_types::MirageError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ReclaimId([u8; 32]);

impl ReclaimId {
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// One physically resident unit that may be reclaimed to satisfy a local space lease.
///
/// `physical_bytes` is the actual allocated-byte contribution, not the logical page length.
/// A unit is reclaimable only when its remote copy is verified and it is clean, unpinned, and
/// free of active readers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReclaimCandidate {
    pub reclaim_id: ReclaimId,
    pub physical_bytes: u64,
    pub last_access_sequence: u64,
    pub remote_verified: bool,
    pub dirty: bool,
    pub pinned: bool,
    pub active_read_leases: u32,
}

impl ReclaimCandidate {
    #[must_use]
    pub const fn reclaimable(self) -> bool {
        self.physical_bytes != 0
            && self.remote_verified
            && !self.dirty
            && !self.pinned
            && self.active_read_leases == 0
    }
}

/// Point-in-time inputs used to decide whether a new local-space promise is safe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapacitySnapshot {
    /// Free bytes reported by the physical filesystem now.
    pub physical_free_bytes: u64,
    /// Free space that Mirage must leave untouched for the filesystem and recovery.
    pub filesystem_reserve_bytes: u64,
    /// Previously granted promises not yet consumed or released.
    pub outstanding_space_lease_bytes: u64,
    /// Physically resident units that can potentially be reclaimed.
    pub candidates: Vec<ReclaimCandidate>,
}

/// Blocked byte counters are diagnostic and may overlap. `unique_bytes` counts each blocked
/// candidate exactly once and is therefore the only additive blocked total.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BlockedCapacity {
    pub unique_bytes: u64,
    pub unverified_bytes: u64,
    pub dirty_bytes: u64,
    pub pinned_bytes: u64,
    pub active_read_bytes: u64,
}

/// Deterministic, side-effect-free decision for one requested Space Lease.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpaceLeasePlan {
    pub requested_bytes: u64,
    pub immediately_available_bytes: u64,
    pub reclaim_required_bytes: u64,
    pub total_reclaimable_bytes: u64,
    pub selected_reclaim_bytes: u64,
    pub available_after_selected_reclaim_bytes: u64,
    pub shortfall_bytes: u64,
    pub selected: Vec<ReclaimCandidate>,
    pub blocked: BlockedCapacity,
}

impl SpaceLeasePlan {
    /// A plan is grantable only when physical free space plus selected verified-clean reclaim
    /// covers the complete request after reserves and earlier leases.
    #[must_use]
    pub const fn grantable(&self) -> bool {
        self.shortfall_bytes == 0
            && self.available_after_selected_reclaim_bytes >= self.requested_bytes
    }
}

/// Plans a local Space Lease without mutating residency.
///
/// Selection is deterministic: least recently accessed first, then largest physical allocation
/// to reduce churn, then content hash. Remote quota and logical namespace size are deliberately
/// absent from the inputs because neither can satisfy a physical local-space promise.
pub fn plan_space_lease(
    snapshot: &CapacitySnapshot,
    requested_bytes: u64,
) -> Result<SpaceLeasePlan, MirageError> {
    if requested_bytes == 0 {
        return Err(MirageError::invalid_argument(
            "Space Lease request must be greater than zero bytes",
        ));
    }

    let immediately_available_bytes = snapshot
        .physical_free_bytes
        .saturating_sub(snapshot.filesystem_reserve_bytes)
        .saturating_sub(snapshot.outstanding_space_lease_bytes);
    let reclaim_required_bytes = requested_bytes.saturating_sub(immediately_available_bytes);

    let mut seen = BTreeSet::new();
    let mut reclaimable = Vec::new();
    let mut blocked = BlockedCapacity::default();

    for candidate in &snapshot.candidates {
        if !seen.insert(candidate.reclaim_id) {
            return Err(MirageError::invalid_argument(
                "capacity snapshot contains a duplicate reclaim candidate",
            ));
        }
        if candidate.physical_bytes == 0 {
            continue;
        }
        if candidate.reclaimable() {
            reclaimable.push(*candidate);
            continue;
        }

        blocked.unique_bytes = checked_add(
            blocked.unique_bytes,
            candidate.physical_bytes,
            "blocked capacity",
        )?;
        if !candidate.remote_verified {
            blocked.unverified_bytes = checked_add(
                blocked.unverified_bytes,
                candidate.physical_bytes,
                "unverified capacity",
            )?;
        }
        if candidate.dirty {
            blocked.dirty_bytes = checked_add(
                blocked.dirty_bytes,
                candidate.physical_bytes,
                "dirty capacity",
            )?;
        }
        if candidate.pinned {
            blocked.pinned_bytes = checked_add(
                blocked.pinned_bytes,
                candidate.physical_bytes,
                "pinned capacity",
            )?;
        }
        if candidate.active_read_leases != 0 {
            blocked.active_read_bytes = checked_add(
                blocked.active_read_bytes,
                candidate.physical_bytes,
                "active-read capacity",
            )?;
        }
    }

    reclaimable.sort_by_key(|candidate| {
        (
            candidate.last_access_sequence,
            Reverse(candidate.physical_bytes),
            candidate.reclaim_id,
        )
    });

    let mut total_reclaimable_bytes = 0_u64;
    for candidate in &reclaimable {
        total_reclaimable_bytes = checked_add(
            total_reclaimable_bytes,
            candidate.physical_bytes,
            "total reclaimable capacity",
        )?;
    }

    let maximum_available = checked_add(
        immediately_available_bytes,
        total_reclaimable_bytes,
        "maximum available capacity",
    )?;
    let shortfall_bytes = requested_bytes.saturating_sub(maximum_available);

    let mut selected = Vec::new();
    let mut selected_reclaim_bytes = 0_u64;
    if reclaim_required_bytes != 0 {
        for candidate in reclaimable {
            selected_reclaim_bytes = checked_add(
                selected_reclaim_bytes,
                candidate.physical_bytes,
                "selected reclaim capacity",
            )?;
            selected.push(candidate);
            if selected_reclaim_bytes >= reclaim_required_bytes {
                break;
            }
        }
    }
    let available_after_selected_reclaim_bytes = checked_add(
        immediately_available_bytes,
        selected_reclaim_bytes,
        "available capacity after reclaim",
    )?;

    Ok(SpaceLeasePlan {
        requested_bytes,
        immediately_available_bytes,
        reclaim_required_bytes,
        total_reclaimable_bytes,
        selected_reclaim_bytes,
        available_after_selected_reclaim_bytes,
        shortfall_bytes,
        selected,
        blocked,
    })
}

fn checked_add(left: u64, right: u64, label: &'static str) -> Result<u64, MirageError> {
    left.checked_add(right)
        .ok_or_else(|| MirageError::invalid_argument(format!("{label} overflows u64")))
}
