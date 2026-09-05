use std::time::{SystemTime, UNIX_EPOCH};

use mirage_cache::{CapacitySnapshot, ReclaimCandidate, SpaceLeasePlan, plan_space_lease};
use mirage_db::{Database, NewSpaceLease, SpaceLeaseEvent, SpaceLeaseRecord, SpaceLeaseState};
use mirage_types::{MirageError, RepositoryId, SpaceLeaseId};

/// Supplies authoritative physical capacity and performs revalidated reclaim operations.
///
/// Implementations must re-check that the candidate is still clean, remotely recoverable,
/// unpinned, and idle immediately before deallocation. A stale plan must fail rather than evict.
pub trait SpaceLeaseSource {
    fn target_volume_id(&self) -> Result<String, MirageError>;
    fn physical_free_bytes(&self) -> Result<u64, MirageError>;
    fn filesystem_reserve_bytes(&self) -> Result<u64, MirageError>;
    fn reclaim_candidates(&self) -> Result<Vec<ReclaimCandidate>, MirageError>;
    fn reclaim_verified_clean(&self, candidate: ReclaimCandidate) -> Result<(), MirageError>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpaceLeaseRequest {
    pub lease_id: SpaceLeaseId,
    pub repository_id: RepositoryId,
    pub requested_bytes: u64,
    pub expires_at_ns: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedSpaceLease {
    pub record: SpaceLeaseRecord,
    pub plan: SpaceLeasePlan,
    pub physical_free_after_reclaim_bytes: u64,
    pub active_promised_bytes: u64,
}

/// Creates and prepares one crash-durable Space Lease.
///
/// The durable lease is inserted before any eviction. Readiness is published only after all
/// selected candidates are revalidated and reclaimed and the physical filesystem is measured
/// again. Concurrent prepares may make one another fail conservatively, but can never over-promise.
pub fn prepare_space_lease(
    database: &Database,
    source: &impl SpaceLeaseSource,
    request: SpaceLeaseRequest,
) -> Result<PreparedSpaceLease, MirageError> {
    let created_at_ns = now_ns()?;
    if request.expires_at_ns <= created_at_ns {
        return Err(MirageError::invalid_argument(
            "Space Lease expiry must be in the future",
        ));
    }

    let target_volume_id = source.target_volume_id()?;
    let outstanding_space_lease_bytes =
        database.active_space_lease_bytes_for_volume(&target_volume_id, created_at_ns)?;
    let snapshot = CapacitySnapshot {
        physical_free_bytes: source.physical_free_bytes()?,
        filesystem_reserve_bytes: source.filesystem_reserve_bytes()?,
        outstanding_space_lease_bytes,
        candidates: source.reclaim_candidates()?,
    };
    let plan = plan_space_lease(&snapshot, request.requested_bytes)?;
    if !plan.grantable() {
        return Err(MirageError::cache_full(format!(
            "Space Lease is short by {} physical bytes",
            plan.shortfall_bytes
        )));
    }

    database.create_space_lease(NewSpaceLease {
        lease_id: request.lease_id,
        repository_id: request.repository_id,
        target_volume_id: target_volume_id.clone(),
        requested_bytes: request.requested_bytes,
        planned_reclaim_bytes: plan.selected_reclaim_bytes,
        created_at_ns,
        expires_at_ns: request.expires_at_ns,
    })?;

    let prepare_result = (|| {
        for candidate in &plan.selected {
            source.reclaim_verified_clean(*candidate)?;
        }

        let physical_free_after_reclaim_bytes = source.physical_free_bytes()?;
        let filesystem_reserve_bytes = source.filesystem_reserve_bytes()?;
        let active_promised_bytes =
            database.active_space_lease_bytes_for_volume(&target_volume_id, now_ns()?)?;
        let promise_envelope = filesystem_reserve_bytes
            .checked_add(active_promised_bytes)
            .ok_or_else(|| MirageError::invalid_argument("Space Lease envelope overflows u64"))?;
        if physical_free_after_reclaim_bytes < promise_envelope {
            return Err(MirageError::cache_full(format!(
                "physical recheck is short by {} bytes",
                promise_envelope - physical_free_after_reclaim_bytes
            )));
        }

        let ready_at_ns = now_ns()?;
        database.transition_space_lease(
            request.lease_id,
            SpaceLeaseState::Preparing,
            SpaceLeaseEvent::ReclaimFinished,
            ready_at_ns,
        )?;
        let record = database
            .load_space_lease(request.lease_id)?
            .ok_or_else(|| MirageError::internal_invariant("prepared Space Lease disappeared"))?;
        Ok(PreparedSpaceLease {
            record,
            plan: plan.clone(),
            physical_free_after_reclaim_bytes,
            active_promised_bytes,
        })
    })();

    if prepare_result.is_err() {
        let failed_at_ns = now_ns().unwrap_or(created_at_ns);
        let _ = database.transition_space_lease(
            request.lease_id,
            SpaceLeaseState::Preparing,
            SpaceLeaseEvent::Fail,
            failed_at_ns,
        );
    }
    prepare_result
}

pub fn release_space_lease(
    database: &Database,
    lease_id: SpaceLeaseId,
) -> Result<SpaceLeaseState, MirageError> {
    let record = database
        .load_space_lease(lease_id)?
        .ok_or_else(|| MirageError::invalid_argument("Space Lease does not exist"))?;
    if !record.state.active() {
        return Err(MirageError::repository_conflict(
            "Space Lease is already terminal",
        ));
    }
    database.transition_space_lease(lease_id, record.state, SpaceLeaseEvent::Release, now_ns()?)
}

pub fn consume_space_lease(
    database: &Database,
    lease_id: SpaceLeaseId,
) -> Result<SpaceLeaseState, MirageError> {
    let record = database
        .load_space_lease(lease_id)?
        .ok_or_else(|| MirageError::invalid_argument("Space Lease does not exist"))?;
    if record.state != SpaceLeaseState::Ready {
        return Err(MirageError::repository_conflict(
            "only a ready Space Lease can be consumed",
        ));
    }
    database.transition_space_lease(
        lease_id,
        SpaceLeaseState::Ready,
        SpaceLeaseEvent::Consume,
        now_ns()?,
    )
}

fn now_ns() -> Result<i64, MirageError> {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| {
            MirageError::internal_invariant("system clock predates Unix epoch").with_source(error)
        })?;
    i64::try_from(duration.as_nanos())
        .map_err(|_| MirageError::internal_invariant("system time exceeds i64 nanoseconds"))
}
