//! Write-behind segment writer for managed volumes.
//!
//! WinFsp delivers writes in small cache-manager chunks; committing each
//! chunk means a WAL-fsynced reservation plus a whole-extent-set rewrite, so
//! sequential copies degraded to a few MB/s. Instead, contiguous writes are
//! appended into one open "segment" payload file (≤ `SEGMENT_MAX_BYTES`),
//! and a background sealer thread performs the durable commit: fsync the
//! file, reserve the physical ledger row, and commit the extent mutation —
//! using a map snapshot taken at close time so a durable version can never
//! reference an unsealed payload.
//!
//! Lock order (everywhere in this module and its callers):
//! `extents` → `dirty.mutex` → `open` → `state`. Callers reach `append` with
//! `extents` and `dirty.mutex` already held; the sealer's idle tick and the
//! `drain_*` methods therefore lock `extents` BEFORE `open` so no path ever
//! waits on `extents` while holding `open`. `state` may be taken alone.

use std::collections::{HashMap, HashSet, VecDeque};
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use mirage_engine::extent_map::ExtentMap;
use mirage_engine::volume::VolumeCoordinator;
use mirage_types::InodeId;

use crate::MirageStatus;
use crate::handles::DirtyLedger;

/// One payload file per up-to-32 MiB of contiguous data.
pub const SEGMENT_MAX_BYTES: u64 = 32 << 20;
/// An open segment seals itself after this much write silence.
pub const SEGMENT_IDLE_SEAL: Duration = Duration::from_secs(2);
const SEAL_TICK: Duration = Duration::from_millis(500);

/// Test hook: shrink the segment ceiling so roll-over tests write MiBs, not
/// 32 MiB. Release builds honor it too — the FFI test binary links release.
pub fn segment_max_bytes() -> u64 {
    std::env::var("MIRAGE_TEST_SEGMENT_MAX")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(SEGMENT_MAX_BYTES)
}

struct OpenSegment {
    payload_id: [u8; 16],
    path: PathBuf,
    file: std::fs::File,
    hasher: blake3::Hasher,
    /// Logical offset in the inode where the segment starts.
    start: u64,
    len: u64,
    last_write: Instant,
    last_write_ns: i64,
}

struct SealJob {
    inode: InodeId,
    segment: OpenSegment,
    /// Extent map cloned at close time — references only sealed or
    /// being-sealed payloads.
    snapshot: ExtentMap,
}

#[derive(Default)]
struct Shared {
    queue: VecDeque<SealJob>,
    pending: HashMap<InodeId, usize>,
    failed: HashSet<InodeId>,
    /// Inodes whose mtime the caller set explicitly — the sealer must not
    /// overwrite it with the last write time.
    explicit_mtime: HashSet<InodeId>,
    /// Live mtimes for the read path (file stat / enumerate).
    mtimes: HashMap<InodeId, (i64, i64)>,
}

pub struct SegmentWriter {
    db: mirage_db::Database,
    volume: mirage_types::RepositoryId,
    journal_dir: PathBuf,
    dirty: Arc<DirtyLedger>,
    extents: Arc<Mutex<HashMap<InodeId, ExtentMap>>>,
    coordinator: Option<Arc<VolumeCoordinator>>,
    publisher: Mutex<Option<Arc<crate::publisher::Publisher>>>,
    open: Mutex<HashMap<InodeId, OpenSegment>>,
    state: Mutex<Shared>,
    wake: Condvar,
    stop: AtomicBool,
    thread: Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl SegmentWriter {
    /// Creates the writer and spawns its sealer thread. `extents` must be the
    /// engine's shared extent-map table — the same map the read path serves.
    pub fn new(
        db: mirage_db::Database,
        volume: mirage_types::RepositoryId,
        journal_dir: PathBuf,
        dirty: Arc<DirtyLedger>,
        extents: Arc<Mutex<HashMap<InodeId, ExtentMap>>>,
        coordinator: Option<Arc<VolumeCoordinator>>,
        publisher: Option<Arc<crate::publisher::Publisher>>,
    ) -> Arc<Self> {
        let writer = Arc::new(Self {
            db,
            volume,
            journal_dir,
            dirty,
            extents,
            coordinator,
            publisher: Mutex::new(publisher),
            open: Mutex::new(HashMap::new()),
            state: Mutex::new(Shared::default()),
            wake: Condvar::new(),
            stop: AtomicBool::new(false),
            thread: Mutex::new(None),
        });
        let worker = Arc::clone(&writer);
        let handle = std::thread::Builder::new()
            .name("mirage-sealer".to_owned())
            .spawn(move || worker.run())
            .expect("sealer thread");
        *writer.thread.lock().unwrap_or_else(|p| p.into_inner()) = Some(handle);
        writer
    }

    /// Late publisher wiring when the engine constructs the publisher after
    /// the writer (construction-order flexibility).
    pub fn set_publisher(&self, publisher: Arc<crate::publisher::Publisher>) {
        *self.publisher.lock().unwrap_or_else(|p| p.into_inner()) = Some(publisher);
    }

    /// Appends `data` at `offset` to the inode's open segment — or rolls the
    /// open segment into the seal queue and starts a fresh one. The caller
    /// holds `dirty.mutex` and the `extents` lock (`maps` is that guard's
    /// content) so the in-memory map mutation and the segment bookkeeping
    /// stay one atomic step.
    pub fn append(
        &self,
        inode: InodeId,
        offset: u64,
        data: &[u8],
        now_ns: i64,
        maps: &mut HashMap<InodeId, ExtentMap>,
    ) -> Result<(), MirageStatus> {
        if self.stop.load(Ordering::Acquire) {
            return Err(MirageStatus::IoError);
        }
        let failed = self
            .state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .failed
            .contains(&inode);
        if failed {
            return Err(MirageStatus::IoError);
        }
        let mut open = self.open.lock().unwrap_or_else(|p| p.into_inner());
        let max = segment_max_bytes();
        let contiguous = open
            .get(&inode)
            .is_some_and(|seg| offset == seg.start + seg.len && seg.len + data.len() as u64 <= max);
        if contiguous {
            let seg = open.get_mut(&inode).expect("checked above");
            seg.file
                .write_all(data)
                .map_err(|_| MirageStatus::IoError)?;
            seg.hasher.update(data);
            let payload_id = seg.payload_id;
            let at = seg.len;
            seg.len += data.len() as u64;
            seg.last_write = Instant::now();
            seg.last_write_ns = now_ns;
            maps.get_mut(&inode)
                .ok_or(MirageStatus::IntegrityFailure)?
                .write_at(offset, data.len() as u64, payload_id, at)
                .map_err(|_| MirageStatus::InvalidArgument)?;
        } else {
            // Seal the current segment first: the snapshot for its durable
            // commit is taken BEFORE the new write mutates the map.
            if open.contains_key(&inode) {
                self.enqueue_open(inode, &mut open, maps)?;
            }
            let mut payload_id = [0_u8; 16];
            if getrandom::fill(&mut payload_id).is_err() {
                return Err(MirageStatus::Internal);
            }
            let path = self
                .journal_dir
                .join(format!("{}.payload", hex16(payload_id)));
            let mut file = std::fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&path)
                .map_err(|_| MirageStatus::IoError)?;
            file.write_all(data).map_err(|_| MirageStatus::IoError)?;
            let mut hasher = blake3::Hasher::new();
            hasher.update(data);
            open.insert(
                inode,
                OpenSegment {
                    payload_id,
                    path,
                    file,
                    hasher,
                    start: offset,
                    len: data.len() as u64,
                    last_write: Instant::now(),
                    last_write_ns: now_ns,
                },
            );
            maps.get_mut(&inode)
                .ok_or(MirageStatus::IntegrityFailure)?
                .write_at(offset, data.len() as u64, payload_id, 0)
                .map_err(|_| MirageStatus::InvalidArgument)?;
        }
        {
            let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
            let explicit = state.explicit_mtime.contains(&inode);
            state
                .mtimes
                .entry(inode)
                .and_modify(|times| {
                    if !explicit {
                        times.1 = now_ns;
                    }
                })
                .or_insert((0, now_ns));
        }
        self.wake.notify_one();
        Ok(())
    }

    /// Moves the inode's open segment (if any) onto the seal queue. Caller
    /// holds `open`; the snapshot is cloned from `maps` before any further
    /// mutation.
    fn enqueue_open(
        &self,
        inode: InodeId,
        open: &mut MutexGuard<'_, HashMap<InodeId, OpenSegment>>,
        maps: &mut HashMap<InodeId, ExtentMap>,
    ) -> Result<(), MirageStatus> {
        let Some(segment) = open.remove(&inode) else {
            return Ok(());
        };
        let snapshot = maps
            .get(&inode)
            .cloned()
            .ok_or(MirageStatus::IntegrityFailure)?;
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        state.queue.push_back(SealJob {
            inode,
            segment,
            snapshot,
        });
        *state.pending.entry(inode).or_insert(0) += 1;
        drop(state);
        self.wake.notify_one();
        Ok(())
    }

    /// Durable seal point: enqueue any open segment, then wait until the
    /// inode's pending seals finish. Returns `IoError` when a seal failed.
    /// Callers must NOT hold `dirty.mutex`, `extents`, `open`, or `state`.
    pub fn drain_inode(&self, inode: InodeId) -> Result<(), MirageStatus> {
        {
            let mut maps = self.extents.lock().unwrap_or_else(|p| p.into_inner());
            let mut open = self.open.lock().unwrap_or_else(|p| p.into_inner());
            if open.contains_key(&inode) {
                self.enqueue_open(inode, &mut open, &mut maps)?;
            }
        }
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        loop {
            if state.pending.get(&inode).copied().unwrap_or(0) == 0 {
                break;
            }
            // A stopped sealer can never drain the queue — fail fast instead
            // of spinning on the condvar.
            if self.stop.load(Ordering::Acquire) {
                return Err(MirageStatus::IoError);
            }
            state = self
                .wake
                .wait_timeout(state, Duration::from_secs(30))
                .unwrap_or_else(|p| p.into_inner())
                .0;
        }
        if state.failed.remove(&inode) {
            return Err(MirageStatus::IoError);
        }
        Ok(())
    }

    /// Drops the inode's open segment without committing (delete path): the
    /// payload file is removed and its bytes come off the dirty ledger.
    pub fn discard_inode(&self, inode: InodeId) {
        let mut open = self.open.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(segment) = open.remove(&inode) {
            let _ = std::fs::remove_file(&segment.path);
            self.dirty.used.fetch_sub(segment.len, Ordering::AcqRel);
        }
        drop(open);
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        state.failed.remove(&inode);
        state.explicit_mtime.remove(&inode);
        state.mtimes.remove(&inode);
    }

    /// Seals every open segment and waits for the queue to empty (quiesce /
    /// destroy). Failures are logged, not raised — the mount-time orphan
    /// sweep and the pending-scan keep state consistent.
    pub fn drain_all(&self) {
        if self.stop.load(Ordering::Acquire) {
            return;
        }
        {
            let mut maps = self.extents.lock().unwrap_or_else(|p| p.into_inner());
            let mut open = self.open.lock().unwrap_or_else(|p| p.into_inner());
            let inodes: Vec<InodeId> = open.keys().copied().collect();
            for inode in inodes {
                let _ = self.enqueue_open(inode, &mut open, &mut maps);
            }
        }
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        loop {
            if state.queue.is_empty() && state.pending.values().all(|count| *count == 0) {
                break;
            }
            if self.stop.load(Ordering::Acquire) {
                return;
            }
            state = self
                .wake
                .wait_timeout(state, Duration::from_secs(30))
                .unwrap_or_else(|p| p.into_inner())
                .0;
        }
    }

    /// Signals the sealer thread to exit and joins it. Call after drain_all.
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Release);
        self.wake.notify_all();
        if let Some(handle) = self.thread.lock().unwrap_or_else(|p| p.into_inner()).take() {
            let _ = handle.join();
        }
    }

    /// Abandon without draining — test-only crash simulation.
    #[doc(hidden)]
    pub fn abandon(&self) {
        self.stop();
    }

    /// Live (created_ns, modified_ns) for the stat path; `modified_ns` is 0
    /// until the first write/set_times.
    pub fn times(&self, inode: InodeId) -> Option<(i64, i64)> {
        self.state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .mtimes
            .get(&inode)
            .copied()
    }

    /// Records an explicit mtime; the sealer stops touching this inode's
    /// `modified_ns` until `clear_explicit` (delete/discard) runs.
    pub fn set_times(&self, inode: InodeId, created: Option<i64>, modified: Option<i64>) {
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        let entry = state.mtimes.entry(inode).or_insert((0, 0));
        if let Some(created) = created {
            entry.0 = created;
        }
        if let Some(modified) = modified {
            entry.1 = modified;
            state.explicit_mtime.insert(inode);
        }
    }

    /// Drops the explicit-mtime flag (file close / discard).
    pub fn clear_explicit(&self, inode: InodeId) {
        self.state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .explicit_mtime
            .remove(&inode);
    }

    fn run(&self) {
        loop {
            // Seal queued jobs first.
            loop {
                let job = {
                    let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
                    state.queue.pop_front()
                };
                let Some(job) = job else { break };
                self.seal(job);
            }
            if self.stop.load(Ordering::Acquire) {
                return;
            }
            // Idle tick: seal segments that have been quiet for 2 s. Lock
            // order is extents → open — the same order writers hold them in
            // — and idleness is evaluated under both locks because a writer
            // may have appended while the sealer waited on `extents`.
            {
                let mut maps = self.extents.lock().unwrap_or_else(|p| p.into_inner());
                let mut open = self.open.lock().unwrap_or_else(|p| p.into_inner());
                let idle: Vec<InodeId> = open
                    .iter()
                    .filter(|(_, seg)| seg.last_write.elapsed() >= SEGMENT_IDLE_SEAL)
                    .map(|(inode, _)| *inode)
                    .collect();
                for inode in idle {
                    let _ = self.enqueue_open(inode, &mut open, &mut maps);
                }
            }
            let state = self.state.lock().unwrap_or_else(|p| p.into_inner());
            let _ = self
                .wake
                .wait_timeout(state, SEAL_TICK)
                .unwrap_or_else(|p| p.into_inner());
        }
    }

    /// The durable commit for one closed segment: fsync the payload, reserve
    /// the physical ledger row, commit the snapshot's extent mutation, and
    /// bump the durable mtime unless the user set it explicitly.
    fn seal(&self, job: SealJob) {
        let SealJob {
            inode,
            segment,
            snapshot,
        } = job;
        let result = (|| -> Result<(), MirageStatus> {
            segment.file.sync_all().map_err(|_| MirageStatus::IoError)?;
            let checksum: [u8; 32] = *segment.hasher.finalize().as_bytes();
            let now = crate::now_ns_i64();
            let slot_index = self.dirty.next_slot.fetch_add(1, Ordering::AcqRel);
            let mut owner_epoch = [0_u8; 16];
            owner_epoch[..8].copy_from_slice(
                &self
                    .coordinator
                    .as_ref()
                    .map(|coordinator| coordinator.epoch())
                    .unwrap_or(0)
                    .to_le_bytes(),
            );
            self.db
                .writer()
                .physical_reserve_extent(
                    mirage_db::PhysicalExtentRecord {
                        extent_id: segment.payload_id,
                        file_id: self.dirty.file_id,
                        slot_index,
                        length_bytes: i64::try_from(segment.len).unwrap_or(i64::MAX),
                        state: mirage_db::PhysicalExtentState::Reserved,
                        page_hash: None,
                        checksum: None,
                        pin_count: 0,
                        generation: 0,
                        updated_ns: now,
                    },
                    mirage_db::PhysicalReservationRecord {
                        extent_id: segment.payload_id,
                        owner_epoch,
                        expires_ns: now.saturating_add(60_000_000_000),
                    },
                )
                .map_err(|_| MirageStatus::IoError)?;
            if crate::commit_extent_mutation_parts(
                &self.db,
                self.volume,
                inode,
                &snapshot,
                mirage_db::OperationKind::Write,
                segment.payload_id,
                segment
                    .path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or_default()
                    .to_owned(),
                segment.len,
                Some(checksum),
                Some(mirage_db::PhysicalCommit {
                    extent_id: segment.payload_id,
                    page_hash: mirage_types::PageHash::from_bytes(checksum),
                    checksum,
                }),
                now,
            )
            .is_err()
            {
                let _ = self
                    .db
                    .writer()
                    .physical_release_extent(segment.payload_id, now);
                return Err(MirageStatus::IoError);
            }
            let (explicit, explicit_mtime) = {
                let state = self.state.lock().unwrap_or_else(|p| p.into_inner());
                (
                    state.explicit_mtime.contains(&inode),
                    state.mtimes.get(&inode).map(|times| times.1),
                )
            };
            // The mutation commit touches inodes.modified_ns itself, so the
            // truthful stamp must be written after it lands.
            let modified = if explicit {
                explicit_mtime
            } else {
                Some(segment.last_write_ns)
            };
            if let Some(modified) = modified {
                let _ =
                    self.db
                        .writer()
                        .namespace_set_times(self.volume, inode, None, Some(modified));
            }
            Ok(())
        })();
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        if let Err(error) = result {
            eprintln!("segment seal failed for inode {inode:?}: {error:?}");
            state.failed.insert(inode);
        }
        if let Some(count) = state.pending.get_mut(&inode) {
            *count -= 1;
            if *count == 0 {
                state.pending.remove(&inode);
            }
        }
        drop(state);
        self.wake.notify_all();
        if let Some(publisher) = self
            .publisher
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
        {
            publisher.notify();
        }
    }
}

fn hex16(bytes: [u8; 16]) -> String {
    let mut out = String::with_capacity(32);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}
