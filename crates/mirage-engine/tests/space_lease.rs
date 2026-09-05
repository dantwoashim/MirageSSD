use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use mirage_cache::{ReclaimCandidate, ReclaimId};
use mirage_db::{Database, NewRepository, SpaceLeaseState};
use mirage_engine::space_lease::{
    SpaceLeaseRequest, SpaceLeaseSource, consume_space_lease, prepare_space_lease,
    release_space_lease,
};
use mirage_types::{MirageError, MirageErrorKind, RepositoryId, RepositoryState, SpaceLeaseId};
use tempfile::tempdir;

struct FakeSource {
    free: Mutex<u64>,
    reserve: u64,
    candidates: Vec<ReclaimCandidate>,
    fail_hash: Option<ReclaimId>,
}

impl SpaceLeaseSource for FakeSource {
    fn target_volume_id(&self) -> Result<String, MirageError> {
        Ok("volume-a".to_owned())
    }

    fn physical_free_bytes(&self) -> Result<u64, MirageError> {
        self.free
            .lock()
            .map(|value| *value)
            .map_err(|_| MirageError::internal_invariant("fake free-space lock poisoned"))
    }

    fn filesystem_reserve_bytes(&self) -> Result<u64, MirageError> {
        Ok(self.reserve)
    }

    fn reclaim_candidates(&self) -> Result<Vec<ReclaimCandidate>, MirageError> {
        Ok(self.candidates.clone())
    }

    fn reclaim_verified_clean(&self, candidate: ReclaimCandidate) -> Result<(), MirageError> {
        if self.fail_hash == Some(candidate.reclaim_id) {
            return Err(MirageError::cache_full("candidate changed before reclaim"));
        }
        if !candidate.reclaimable() {
            return Err(MirageError::internal_invariant(
                "planner exposed unsafe reclaim candidate",
            ));
        }
        let mut free = self
            .free
            .lock()
            .map_err(|_| MirageError::internal_invariant("fake free-space lock poisoned"))?;
        *free = free
            .checked_add(candidate.physical_bytes)
            .ok_or_else(|| MirageError::invalid_argument("fake free space overflows"))?;
        Ok(())
    }
}

fn candidate(value: u8, bytes: u64, age: u64) -> ReclaimCandidate {
    ReclaimCandidate {
        reclaim_id: ReclaimId::from_bytes([value; 32]),
        physical_bytes: bytes,
        last_access_sequence: age,
        remote_verified: true,
        dirty: false,
        pinned: false,
        active_read_leases: 0,
    }
}

fn setup() -> (tempfile::TempDir, Database, RepositoryId) {
    let directory = tempdir().expect("temp directory");
    let database = Database::open(&directory.path().join("control-plane.db")).expect("database");
    let repository_id = RepositoryId::from_bytes([1; 16]);
    database
        .create_repository(NewRepository {
            repository_id,
            display_name: "Space Lease Engine".to_owned(),
            local_root: directory.path().join("native"),
            owner_sid: "S-1-5-18".to_owned(),
            content_encrypted: true,
            initial_state: RepositoryState::ReadyUnmounted,
            created_at_ns: 1,
        })
        .expect("repository");
    (directory, database, repository_id)
}

fn future_ns() -> i64 {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    i64::try_from(now).expect("now") + 60_000_000_000
}

#[test]
fn prepare_reclaims_rechecks_persists_and_releases() {
    let (_directory, database, repository_id) = setup();
    let source = FakeSource {
        free: Mutex::new(30),
        reserve: 10,
        candidates: vec![candidate(1, 20, 1), candidate(2, 50, 2)],
        fail_hash: None,
    };
    let lease_id = SpaceLeaseId::from_bytes([2; 16]);

    let prepared = prepare_space_lease(
        &database,
        &source,
        SpaceLeaseRequest {
            lease_id,
            repository_id,
            requested_bytes: 60,
            expires_at_ns: future_ns(),
        },
    )
    .expect("prepare lease");

    assert_eq!(prepared.record.state, SpaceLeaseState::Ready);
    assert_eq!(prepared.plan.selected_reclaim_bytes, 70);
    assert_eq!(prepared.physical_free_after_reclaim_bytes, 100);
    assert_eq!(prepared.active_promised_bytes, 60);
    assert_eq!(
        database.active_space_lease_bytes(repository_id).unwrap(),
        60
    );

    assert_eq!(
        consume_space_lease(&database, lease_id).expect("consume"),
        SpaceLeaseState::Consumed
    );
    assert_eq!(
        release_space_lease(&database, lease_id).expect("release"),
        SpaceLeaseState::Released
    );
    assert_eq!(database.active_space_lease_bytes(repository_id).unwrap(), 0);
}

#[test]
fn denied_plan_creates_no_durable_promise() {
    let (_directory, database, repository_id) = setup();
    let mut unsafe_candidate = candidate(1, 1_000, 1);
    unsafe_candidate.remote_verified = false;
    let source = FakeSource {
        free: Mutex::new(20),
        reserve: 10,
        candidates: vec![unsafe_candidate],
        fail_hash: None,
    };
    let lease_id = SpaceLeaseId::from_bytes([3; 16]);
    let error = prepare_space_lease(
        &database,
        &source,
        SpaceLeaseRequest {
            lease_id,
            repository_id,
            requested_bytes: 100,
            expires_at_ns: future_ns(),
        },
    )
    .expect_err("deny");

    assert_eq!(error.kind, MirageErrorKind::CacheFull);
    assert!(database.load_space_lease(lease_id).unwrap().is_none());
}

#[test]
fn stale_candidate_failure_marks_the_durable_lease_failed() {
    let (_directory, database, repository_id) = setup();
    let selected = candidate(1, 100, 1);
    let source = FakeSource {
        free: Mutex::new(10),
        reserve: 10,
        candidates: vec![selected],
        fail_hash: Some(selected.reclaim_id),
    };
    let lease_id = SpaceLeaseId::from_bytes([4; 16]);
    let error = prepare_space_lease(
        &database,
        &source,
        SpaceLeaseRequest {
            lease_id,
            repository_id,
            requested_bytes: 50,
            expires_at_ns: future_ns(),
        },
    )
    .expect_err("reclaim must fail");

    assert_eq!(error.kind, MirageErrorKind::CacheFull);
    assert_eq!(
        database.load_space_lease(lease_id).unwrap().unwrap().state,
        SpaceLeaseState::Failed
    );
    assert_eq!(database.active_space_lease_bytes(repository_id).unwrap(), 0);
}

#[test]
fn concurrent_promises_are_included_in_the_physical_recheck() {
    let (_directory, database, repository_id) = setup();
    database
        .create_space_lease(mirage_db::NewSpaceLease {
            lease_id: SpaceLeaseId::from_bytes([8; 16]),
            repository_id,
            target_volume_id: "volume-a".to_owned(),
            requested_bytes: 70,
            planned_reclaim_bytes: 0,
            created_at_ns: 1,
            expires_at_ns: future_ns(),
        })
        .expect("existing promise");
    let source = FakeSource {
        free: Mutex::new(100),
        reserve: 10,
        candidates: Vec::new(),
        fail_hash: None,
    };
    let lease_id = SpaceLeaseId::from_bytes([9; 16]);

    let error = prepare_space_lease(
        &database,
        &source,
        SpaceLeaseRequest {
            lease_id,
            repository_id,
            requested_bytes: 30,
            expires_at_ns: future_ns(),
        },
    )
    .expect_err("existing promise leaves only 20 bytes");
    assert_eq!(error.kind, MirageErrorKind::CacheFull);
    assert!(database.load_space_lease(lease_id).unwrap().is_none());
}
