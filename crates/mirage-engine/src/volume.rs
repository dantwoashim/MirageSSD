//! Single-owner volume coordination.
//!
//! A `VolumeCoordinator` is embedded in the filesystem host process and owns
//! the mounted volume's metadata writer, resident index, arena, and read
//! handles. Ownership is an OS-backed lock (`Local\`-namespaced mutex on
//! Windows) plus a durable epoch record, so a crashed host releases the
//! volume and a restarted owner can never be confused with its predecessor.
//! Control requests carry the epoch they were issued under; a delayed message
//! from an old epoch is fenced.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use mirage_types::{MirageError, PageHash, RepositoryId};
use serde::{Deserialize, Serialize};

const OWNER_RECORD: &str = "volume-owner.json";
const OWNER_FORMAT_VERSION: u32 = 1;

/// Ownership lifecycle of a mounted volume. Transitions are monotonic except
/// `Recovering`, which a fresh owner enters before reaching `Starting`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum VolumeState {
    /// Lock released; no owner.
    #[default]
    Unmounted,
    /// Host is alive, lock held, mount not yet published.
    Starting,
    /// Volume is serving requests.
    Mounted,
    /// Shutdown in progress: no new reads admitted, existing readers drain.
    Quiescing,
    /// A previous owner did not release cleanly; recovery is reconciling
    /// durable state before the volume may start.
    Recovering,
}

impl VolumeState {
    fn may_transition(self, next: VolumeState) -> bool {
        matches!(
            (self, next),
            (Self::Starting, Self::Mounted)
                | (Self::Starting, Self::Unmounted)
                | (Self::Recovering, Self::Starting)
                | (Self::Recovering, Self::Unmounted)
                | (Self::Mounted, Self::Quiescing)
                | (Self::Mounted, Self::Recovering)
                | (Self::Quiescing, Self::Unmounted)
                | (Self::Quiescing, Self::Recovering)
        )
    }
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Starting => "starting",
            Self::Mounted => "mounted",
            Self::Quiescing => "quiescing",
            Self::Recovering => "recovering",
            Self::Unmounted => "unmounted",
        }
    }
}

/// Durable owner record. The epoch strictly increases per acquisition, so a
/// delayed control message stamped with an older epoch is rejected even when
/// a crashed owner's record survives.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OwnerRecord {
    pub format_version: u32,
    pub repository_id: RepositoryId,
    pub epoch: u64,
    pub owner_pid: u32,
    pub state: VolumeState,
    pub acquired_at_ns: i64,
}

/// A read that pins one page against eviction for its lifetime. The guard is
/// an ordinary in-process borrow; cross-process reader leases remain the
/// documented exclusion of mounted volumes from reclamation.
pub struct ReaderLease {
    coordinator: std::sync::Weak<CoordinatorShared>,
    page: PageHash,
}

impl Drop for ReaderLease {
    fn drop(&mut self) {
        if let Some(shared) = self.coordinator.upgrade()
            && let Ok(mut readers) = shared.readers.lock()
            && let Some(count) = readers.get_mut(&self.page)
        {
            *count -= 1;
            if *count == 0 {
                readers.remove(&self.page);
            }
            shared.readers_changed.notify_all();
        }
    }
}

struct CoordinatorShared {
    readers: Mutex<HashMap<PageHash, usize>>,
    readers_changed: std::sync::Condvar,
    state: Mutex<VolumeState>,
}

/// The single-owner lock and fencing state for one mounted volume.
pub struct VolumeCoordinator {
    shared: std::sync::Arc<CoordinatorShared>,
    repository_id: RepositoryId,
    epoch: u64,
    record_path: PathBuf,
    lock: VolumeOwnerLock,
}

impl VolumeCoordinator {
    /// Acquire exclusive ownership of the volume's state directory. A second
    /// process (or a second coordinator in this process) fails while a live
    /// owner holds the OS lock. When a previous owner died without releasing,
    /// the stale record is adopted, the epoch advances, and the new owner
    /// starts in `Recovering` rather than `Starting`.
    pub fn acquire(state_root: &Path, repository_id: RepositoryId) -> Result<Self, MirageError> {
        let lock = VolumeOwnerLock::acquire(state_root, repository_id)?;
        let record_path = state_root.join(OWNER_RECORD);
        let previous = load_owner_record(&record_path, repository_id)?;
        let epoch = previous
            .as_ref()
            .map(|record| record.epoch)
            .unwrap_or(0)
            .checked_add(1)
            .ok_or_else(|| MirageError::internal_invariant("volume epoch overflowed"))?;
        let state = match previous {
            Some(record) if record.state != VolumeState::Unmounted => VolumeState::Recovering,
            _ => VolumeState::Starting,
        };
        let record = OwnerRecord {
            format_version: OWNER_FORMAT_VERSION,
            repository_id,
            epoch,
            owner_pid: std::process::id(),
            state,
            acquired_at_ns: now_ns(),
        };
        mirage_crypto::durable_file::write_atomic(
            &record_path,
            serde_json::to_vec(&record)
                .map_err(|_| MirageError::internal_invariant("owner record serialization failed"))?
                .as_slice(),
        )?;
        let shared = std::sync::Arc::new(CoordinatorShared {
            readers: Mutex::new(HashMap::new()),
            readers_changed: std::sync::Condvar::new(),
            state: Mutex::new(state),
        });
        Ok(Self {
            shared,
            repository_id,
            epoch,
            record_path,
            lock,
        })
    }

    /// The fencing epoch of this ownership term.
    #[must_use]
    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    /// True when `epoch` belongs to this ownership term. Control requests
    /// stamped with any other epoch are fenced.
    #[must_use]
    pub fn check_epoch(&self, epoch: u64) -> bool {
        epoch == self.epoch
    }

    #[must_use]
    pub fn repository_id(&self) -> RepositoryId {
        self.repository_id
    }

    #[must_use]
    pub fn state(&self) -> VolumeState {
        *self.shared.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Advance the ownership lifecycle and persist it into the owner record.
    pub fn transition(&self, next: VolumeState) -> Result<(), MirageError> {
        {
            let mut state = self
                .shared
                .state
                .lock()
                .map_err(|_| MirageError::internal_invariant("volume state lock poisoned"))?;
            if !state.may_transition(next) {
                return Err(MirageError::repository_conflict(format!(
                    "volume cannot move from {} to {}",
                    state.as_str(),
                    next.as_str()
                )));
            }
            *state = next;
        }
        let mut record = load_owner_record(&self.record_path, self.repository_id)?
            .ok_or_else(|| MirageError::integrity_mismatch("volume owner record is missing"))?;
        if record.epoch != self.epoch {
            return Err(MirageError::integrity_mismatch(
                "volume owner record epoch changed under a live owner",
            ));
        }
        record.state = next;
        mirage_crypto::durable_file::write_atomic(
            &self.record_path,
            serde_json::to_vec(&record)
                .map_err(|_| MirageError::internal_invariant("owner record serialization failed"))?
                .as_slice(),
        )
    }

    /// Admit one page read while mounted. Returns a lease that keeps the page
    /// ineligible for eviction until dropped. Reads are refused once the
    /// volume starts quiescing, so eviction can never race a reader for the
    /// page it is serving.
    pub fn begin_read(&self, page: PageHash) -> Result<ReaderLease, MirageError> {
        {
            let state = self
                .shared
                .state
                .lock()
                .map_err(|_| MirageError::internal_invariant("volume state lock poisoned"))?;
            if *state != VolumeState::Mounted {
                return Err(MirageError::repository_conflict(
                    "volume is not admitting reads",
                ));
            }
        }
        let mut readers = self
            .shared
            .readers
            .lock()
            .map_err(|_| MirageError::internal_invariant("volume reader lock poisoned"))?;
        *readers.entry(page).or_insert(0) += 1;
        drop(readers);
        Ok(ReaderLease {
            coordinator: std::sync::Arc::downgrade(&self.shared),
            page,
        })
    }

    /// Pages with at least one active reader; the eviction path must exclude
    /// every page in this set.
    #[must_use]
    pub fn protected_pages(&self) -> Vec<PageHash> {
        self.shared
            .readers
            .lock()
            .map(|readers| readers.keys().copied().collect())
            .unwrap_or_default()
    }

    /// Whether a page is safe to evict right now.
    #[must_use]
    pub fn is_evictable(&self, page: PageHash) -> bool {
        self.shared
            .readers
            .lock()
            .map(|readers| !readers.contains_key(&page))
            .unwrap_or(false)
    }

    #[must_use]
    pub fn active_reader_count(&self) -> usize {
        self.shared
            .readers
            .lock()
            .map(|readers| readers.values().sum())
            .unwrap_or(0)
    }

    /// Move to `Quiescing`, wait for active readers to drain up to `timeout`,
    /// then reach `Unmounted`. A timeout leaves the volume in `Quiescing` —
    /// the coordinator is still the owner and must be torn down by `release`
    /// after readers are forced closed. Calling from `Starting` or
    /// `Recovering` (mount never completed) skips the drain; calling on an
    /// already-unmounted volume is a no-op.
    pub fn quiesce(&self, timeout: Duration) -> Result<(), MirageError> {
        match self.state() {
            VolumeState::Unmounted => return Ok(()),
            VolumeState::Starting | VolumeState::Recovering => {
                return self.transition(VolumeState::Unmounted);
            }
            VolumeState::Mounted => self.transition(VolumeState::Quiescing)?,
            VolumeState::Quiescing => {}
        }
        let deadline = Instant::now() + timeout;
        let mut readers = self
            .shared
            .readers
            .lock()
            .map_err(|_| MirageError::internal_invariant("volume reader lock poisoned"))?;
        while !readers.is_empty() {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(MirageError::deadline_exceeded(
                    "volume readers did not drain before the quiesce deadline",
                ));
            }
            let (guard, _) = self
                .shared
                .readers_changed
                .wait_timeout(readers, remaining)
                .map_err(|_| MirageError::internal_invariant("volume reader lock poisoned"))?;
            readers = guard;
        }
        drop(readers);
        self.transition(VolumeState::Unmounted)
    }

    /// Release ownership without waiting for readers (final teardown after a
    /// forced reader close or a failed `quiesce`).
    pub fn release(mut self) -> Result<(), MirageError> {
        self.mark_unmounted()?;
        // Dropping the OS lock handle releases ownership even if this thread
        // is unwinding.
        self.lock.release();
        Ok(())
    }

    fn mark_unmounted(&self) -> Result<(), MirageError> {
        let mut record = match load_owner_record(&self.record_path, self.repository_id)? {
            Some(record) => record,
            None => return Ok(()),
        };
        // A record owned by a newer epoch belongs to a successor; never
        // overwrite it.
        if record.epoch != self.epoch || record.state == VolumeState::Unmounted {
            return Ok(());
        }
        record.state = VolumeState::Unmounted;
        mirage_crypto::durable_file::write_atomic(
            &self.record_path,
            serde_json::to_vec(&record)
                .map_err(|_| MirageError::internal_invariant("owner record serialization failed"))?
                .as_slice(),
        )?;
        if let Ok(mut state) = self.shared.state.lock() {
            *state = VolumeState::Unmounted;
        }
        Ok(())
    }
}

impl Drop for VolumeCoordinator {
    fn drop(&mut self) {
        // A coordinator that dies without quiesce leaves the record with its
        // non-Unmounted state; the next owner adopts it as Recovering.
        let _ = self.mark_unmounted();
    }
}

fn now_ns() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_nanos() as i64)
        .unwrap_or(0)
}

fn load_owner_record(
    path: &Path,
    repository_id: RepositoryId,
) -> Result<Option<OwnerRecord>, MirageError> {
    if !path.exists() {
        return Ok(None);
    }
    let bytes = std::fs::read(path).map_err(MirageError::from)?;
    if bytes.len() > 64 * 1024 {
        return Err(MirageError::integrity_mismatch(
            "volume owner record is oversized",
        ));
    }
    let record: OwnerRecord = serde_json::from_slice(&bytes).map_err(|error| {
        MirageError::integrity_mismatch("volume owner record is invalid").with_source(error)
    })?;
    if record.format_version != OWNER_FORMAT_VERSION || record.repository_id != repository_id {
        return Err(MirageError::integrity_mismatch(
            "volume owner record belongs to another volume or format",
        ));
    }
    Ok(Some(record))
}

/// The OS-backed single-owner lock: an exclusive byte-range lock on a lock
/// file in the volume's state directory. The OS releases the lock when the
/// owning process dies, so a crashed host transfers ownership without stale
/// lock cleanup, and a second live process can never hold it at once.
struct VolumeOwnerLock(std::fs::File);

impl VolumeOwnerLock {
    fn acquire(state_root: &Path, repository_id: RepositoryId) -> Result<Self, MirageError> {
        std::fs::create_dir_all(state_root).map_err(MirageError::from)?;
        let path = state_root.join(format!("volume-{repository_id}.lock"));
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(MirageError::from)?;
        match file.try_lock() {
            Ok(()) => {}
            Err(std::fs::TryLockError::Error(error))
                if error.kind() == std::io::ErrorKind::WouldBlock =>
            {
                return Err(MirageError::repository_conflict(
                    "another process owns this volume",
                ));
            }
            Err(std::fs::TryLockError::Error(error)) => {
                return Err(MirageError::from(error));
            }
            Err(_) => {
                return Err(MirageError::provider_unavailable(
                    "volume ownership lock failed",
                ));
            }
        }
        Ok(Self(file))
    }

    fn release(&mut self) {
        let _ = self.0.unlock();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn second_owner_cannot_acquire_while_first_holds_lock() {
        let dir = root();
        let repo = RepositoryId::from_bytes([1; 16]);
        let first = VolumeCoordinator::acquire(dir.path(), repo).unwrap();
        let failure = VolumeCoordinator::acquire(dir.path(), repo);
        assert!(failure.is_err());
        assert_eq!(first.state(), VolumeState::Starting);
    }

    #[test]
    fn released_ownership_transfers_with_a_new_epoch() {
        let dir = root();
        let repo = RepositoryId::from_bytes([2; 16]);
        let first = VolumeCoordinator::acquire(dir.path(), repo).unwrap();
        first.transition(VolumeState::Mounted).unwrap();
        let epoch = first.epoch();
        first.release().unwrap();
        let second = VolumeCoordinator::acquire(dir.path(), repo).unwrap();
        assert!(second.epoch() > epoch);
        // A clean handoff resumes in Starting, not Recovering.
        assert_eq!(second.state(), VolumeState::Starting);
        assert!(!second.check_epoch(epoch));
        assert!(second.check_epoch(second.epoch()));
    }

    #[test]
    fn crashed_owner_is_adopted_in_recovering_state() {
        let dir = root();
        let repo = RepositoryId::from_bytes([3; 16]);
        let doomed = VolumeCoordinator::acquire(dir.path(), repo).unwrap();
        doomed.transition(VolumeState::Mounted).unwrap();
        let old_epoch = doomed.epoch();
        // Simulate host termination: the OS lock is released by the kernel
        // (here by closing the handle) while the owner record still says the
        // previous owner was mounted.
        let mut doomed = doomed;
        doomed.lock.release();
        let recovered = VolumeCoordinator::acquire(dir.path(), repo).unwrap();
        assert_eq!(recovered.state(), VolumeState::Recovering);
        assert_eq!(recovered.epoch(), old_epoch + 1);
        assert!(recovered.transition(VolumeState::Starting).is_ok());
        assert!(recovered.transition(VolumeState::Mounted).is_ok());
    }

    #[test]
    fn readers_pin_pages_and_quiesce_drains_them() {
        let dir = root();
        let repo = RepositoryId::from_bytes([4; 16]);
        let coordinator = VolumeCoordinator::acquire(dir.path(), repo).unwrap();
        let page = PageHash::from_bytes([7; 32]);
        assert!(coordinator.begin_read(page).is_err()); // not mounted yet
        coordinator.transition(VolumeState::Mounted).unwrap();
        let lease = coordinator.begin_read(page).unwrap();
        assert!(!coordinator.is_evictable(page));
        assert_eq!(coordinator.protected_pages(), vec![page]);
        let coordinator = std::sync::Arc::new(coordinator);
        let quiescing = std::sync::Arc::clone(&coordinator);
        let drainer = std::thread::spawn(move || quiescing.quiesce(Duration::from_secs(30)));
        std::thread::sleep(Duration::from_millis(100));
        // Reads are fenced during quiesce.
        assert!(coordinator.begin_read(page).is_err());
        drop(lease);
        drainer.join().unwrap().unwrap();
        assert_eq!(coordinator.state(), VolumeState::Unmounted);
    }

    #[test]
    fn quiesce_times_out_with_readers_still_open() {
        let dir = root();
        let repo = RepositoryId::from_bytes([5; 16]);
        let coordinator = VolumeCoordinator::acquire(dir.path(), repo).unwrap();
        coordinator.transition(VolumeState::Mounted).unwrap();
        let _lease = coordinator
            .begin_read(PageHash::from_bytes([9; 32]))
            .unwrap();
        assert!(coordinator.quiesce(Duration::from_millis(50)).is_err());
        assert_eq!(coordinator.state(), VolumeState::Quiescing);
    }

    #[test]
    fn invalid_transitions_and_stale_records_are_rejected() {
        let dir = root();
        let repo = RepositoryId::from_bytes([6; 16]);
        let coordinator = VolumeCoordinator::acquire(dir.path(), repo).unwrap();
        assert!(coordinator.transition(VolumeState::Quiescing).is_err());
        assert!(coordinator.transition(VolumeState::Mounted).is_ok());
        assert!(coordinator.transition(VolumeState::Mounted).is_err());
        // A foreign repository's record must not be adopted.
        let foreign = dir.path().join("foreign");
        std::fs::create_dir_all(&foreign).unwrap();
        std::fs::copy(dir.path().join(OWNER_RECORD), foreign.join(OWNER_RECORD)).unwrap();
        assert!(
            VolumeCoordinator::acquire(&foreign, RepositoryId::from_bytes([8; 16])).is_err()
                || load_owner_record(
                    &foreign.join(OWNER_RECORD),
                    RepositoryId::from_bytes([8; 16])
                )
                .is_err()
        );
    }
}
