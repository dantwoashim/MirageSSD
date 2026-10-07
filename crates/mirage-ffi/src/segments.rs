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
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use mirage_engine::extent_map::ExtentMap;
use mirage_engine::volume::VolumeCoordinator;
use mirage_types::InodeId;

use crate::MirageStatus;
use crate::handles::{DirtyLedger, PayloadEntry, PayloadFileCache, invalidate_payload_file};
use crate::trace;

/// Positional append on the shared payload handle. `seek_write` on a
/// synchronous handle moves the file cursor (seek/save-restore around the
/// write) — the `io` lock keeps the pair atomic against concurrent readers
/// sharing this entry.
#[cfg(windows)]
fn append_at(entry: &PayloadEntry, data: &[u8], offset: u64) -> std::io::Result<()> {
    use std::os::windows::fs::FileExt;
    let _io = entry
        .io
        .lock()
        .map_err(|_| std::io::Error::other("payload io lock poisoned"))?;
    let mut written = 0usize;
    while written < data.len() {
        match entry
            .file
            .seek_write(&data[written..], offset + written as u64)
        {
            Ok(0) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::WriteZero,
                    "short payload write",
                ));
            }
            Ok(n) => written += n,
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

#[cfg(unix)]
fn append_at(entry: &PayloadEntry, data: &[u8], offset: u64) -> std::io::Result<()> {
    let _io = entry
        .io
        .lock()
        .map_err(|_| std::io::Error::other("payload io lock poisoned"))?;
    std::os::unix::fs::FileExt::write_all_at(&entry.file, data, offset)
}

#[cfg(not(any(windows, unix)))]
fn append_at(entry: &PayloadEntry, data: &[u8], offset: u64) -> std::io::Result<()> {
    use std::io::{Seek, SeekFrom, Write};
    let _io = entry
        .io
        .lock()
        .map_err(|_| std::io::Error::other("payload io lock poisoned"))?;
    let mut file = entry.file.try_clone()?;
    file.seek(SeekFrom::Start(offset))?;
    file.write_all(data)
}

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
    /// The payload file, shared with the engine's read cache — reads of the
    /// open segment never re-open it (each CreateFile can pay an AV scan).
    /// Writes are positional because readers share the file's cursor.
    entry: Arc<PayloadEntry>,
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
    /// Enqueue time — dev trace measures queue latency into seal().
    enqueued: Instant,
}

#[derive(Default)]
struct Shared {
    queue: VecDeque<SealJob>,
    pending: HashMap<InodeId, usize>,
    failed: HashSet<InodeId>,
    /// Inodes deleted while a seal job is in flight — a seal that fails on a
    /// discarded inode is cleaned up, not recorded as a failure.
    discarded: HashSet<InodeId>,
    /// Inodes whose mtime the caller set explicitly — the sealer must not
    /// overwrite it with the last write time.
    explicit_mtime: HashSet<InodeId>,
    /// Live mtimes for the read path (file stat / enumerate).
    mtimes: HashMap<InodeId, (i64, i64)>,
}

/// Close-time seal enqueues block only above this queue depth — bounds both
/// memory and the group-commit power-loss window.
const SEAL_QUEUE_BACKPRESSURE: usize = 256;

/// Test hook: delay before each seal so a test can deterministically land a
/// delete between enqueue and seal start.
fn seal_delay() -> Option<Duration> {
    std::env::var("MIRAGE_TEST_SEAL_DELAY_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|delay| *delay > 0)
        .map(Duration::from_millis)
}

pub struct SegmentWriter {
    db: mirage_db::Database,
    volume: mirage_types::RepositoryId,
    journal_dir: PathBuf,
    dirty: Arc<DirtyLedger>,
    extents: Arc<Mutex<HashMap<InodeId, ExtentMap>>>,
    payload_files: Arc<Mutex<PayloadFileCache>>,
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
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        db: mirage_db::Database,
        volume: mirage_types::RepositoryId,
        journal_dir: PathBuf,
        dirty: Arc<DirtyLedger>,
        extents: Arc<Mutex<HashMap<InodeId, ExtentMap>>>,
        payload_files: Arc<Mutex<PayloadFileCache>>,
        coordinator: Option<Arc<VolumeCoordinator>>,
        publisher: Option<Arc<crate::publisher::Publisher>>,
    ) -> Arc<Self> {
        let writer = Arc::new(Self {
            db,
            volume,
            journal_dir,
            dirty,
            extents,
            payload_files,
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
            append_at(&seg.entry, data, seg.len).map_err(|_| MirageStatus::IoError)?;
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
            let entry = {
                let _create = trace::Scope::new(trace::Slot::WriteNewPayloadFile);
                // The dir is created once per engine; a vanished dir is
                // recreated here so a NotFound does not fail the write.
                let open = || {
                    std::fs::OpenOptions::new()
                        .create_new(true)
                        .read(true)
                        .write(true)
                        .open(&path)
                };
                let file = match open() {
                    Ok(file) => file,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        let _ = std::fs::create_dir_all(&self.journal_dir);
                        open().map_err(|_| MirageStatus::IoError)?
                    }
                    Err(_) => return Err(MirageStatus::IoError),
                };
                Arc::new(PayloadEntry {
                    file,
                    io: Mutex::new(()),
                })
            };
            // Reads of the open segment go through the shared cache — never
            // a fresh CreateFile for a payload this process just created.
            if let Ok(mut cache) = self.payload_files.lock() {
                cache.insert(payload_id, Arc::clone(&entry));
            }
            append_at(&entry, data, 0).map_err(|_| MirageStatus::IoError)?;
            let mut hasher = blake3::Hasher::new();
            hasher.update(data);
            open.insert(
                inode,
                OpenSegment {
                    payload_id,
                    path,
                    entry,
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
        // notify_all: the condvar also wakes drain_inode/drain_all waiters —
        // notify_one can pick a waiter and leave the sealer sleeping a whole
        // SEAL_TICK.
        self.wake.notify_all();
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
            enqueued: Instant::now(),
        });
        *state.pending.entry(inode).or_insert(0) += 1;
        drop(state);
        self.wake.notify_all();
        Ok(())
    }

    /// Enqueue the inode's open segment without waiting (group-commit
    /// close): the seal lands on the sealer thread. Waits only when the
    /// queue exceeds the backpressure bound — keeps the power-loss window
    /// and queued-segment memory bounded.
    /// Callers must NOT hold `dirty.mutex`, `extents`, `open`, or `state`.
    pub fn seal_async(&self, inode: InodeId) -> Result<(), MirageStatus> {
        {
            let mut maps = self.extents.lock().unwrap_or_else(|p| p.into_inner());
            let mut open = self.open.lock().unwrap_or_else(|p| p.into_inner());
            if open.contains_key(&inode) {
                self.enqueue_open(inode, &mut open, &mut maps)?;
            }
        }
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        while state.queue.len() > SEAL_QUEUE_BACKPRESSURE {
            if self.stop.load(Ordering::Acquire) {
                return Err(MirageStatus::IoError);
            }
            state = self
                .wake
                .wait_timeout(state, Duration::from_secs(30))
                .unwrap_or_else(|p| p.into_inner())
                .0;
        }
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

    /// Drops the inode's open segment AND any queued (not yet started) seal
    /// jobs without committing (delete path): payload files are removed and
    /// their bytes come off the dirty ledger. A seal already in flight sees
    /// the inode in `discarded` and runs the same cleanup on failure.
    pub fn discard_inode(&self, inode: InodeId) {
        {
            let mut open = self.open.lock().unwrap_or_else(|p| p.into_inner());
            if let Some(segment) = open.remove(&inode) {
                invalidate_payload_file(&self.payload_files, &segment.payload_id);
                let _ = std::fs::remove_file(&segment.path);
                self.dirty.used.fetch_sub(segment.len, Ordering::AcqRel);
            }
        }
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        let mut dropped = 0usize;
        state.queue.retain(|job| {
            if job.inode != inode {
                return true;
            }
            invalidate_payload_file(&self.payload_files, &job.segment.payload_id);
            let _ = std::fs::remove_file(&job.segment.path);
            self.dirty.used.fetch_sub(job.segment.len, Ordering::AcqRel);
            dropped += 1;
            false
        });
        if dropped > 0
            && let Some(count) = state.pending.get_mut(&inode)
        {
            *count = count.saturating_sub(dropped);
            if *count == 0 {
                state.pending.remove(&inode);
            }
        }
        // A still-running seal must see the discard: its failure then runs
        // the same cleanup instead of recording a failure.
        if state.pending.contains_key(&inode) {
            state.discarded.insert(inode);
        }
        state.failed.remove(&inode);
        state.explicit_mtime.remove(&inode);
        state.mtimes.remove(&inode);
        drop(state);
        self.wake.notify_all();
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
                // Test hook: hold the job in-flight (not queued) so a test
                // can land a delete between pop and seal deterministically.
                if let Some(delay) = seal_delay() {
                    std::thread::sleep(delay);
                }
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
        trace::record_elapsed(trace::Slot::SealQueueLatency, job.enqueued);
        let SealJob {
            inode,
            segment,
            snapshot,
            enqueued: _,
        } = job;
        let result = (|| -> Result<(), MirageStatus> {
            {
                let _fsync = trace::Scope::new(trace::Slot::SealFsync);
                segment
                    .entry
                    .file
                    .sync_all()
                    .map_err(|_| MirageStatus::IoError)?;
            }
            // A delete can land while this seal was in flight: committing
            // extents for a gone inode would reference a payload nobody can
            // reach — discard it instead. `delete_node_in` removes the inode
            // row, so namespace_entry is the definitive existence check.
            match self.db.namespace_entry(self.volume, inode) {
                Ok(Some(_)) => {}
                Ok(None) => {
                    invalidate_payload_file(&self.payload_files, &segment.payload_id);
                    let _ = std::fs::remove_file(&segment.path);
                    self.dirty.used.fetch_sub(segment.len, Ordering::AcqRel);
                    return Ok(());
                }
                Err(_) => return Err(MirageStatus::IoError),
            }
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
            let (explicit, explicit_mtime) = {
                let state = self.state.lock().unwrap_or_else(|p| p.into_inner());
                (
                    state.explicit_mtime.contains(&inode),
                    state.mtimes.get(&inode).map(|times| times.1),
                )
            };
            // The mutation commit touches inodes.modified_ns itself, so the
            // truthful stamp lands after the commit body — inside the same
            // transaction.
            let modified = if explicit {
                explicit_mtime
            } else {
                Some(segment.last_write_ns)
            };
            let mut mutation = crate::build_extent_mutation_commit(
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
            )?;
            mutation.reservation = Some((
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
            ));
            mutation.modified_ns = modified.map(|stamp| (inode, stamp));
            let _commit = trace::Scope::new(trace::Slot::SealCommit);
            self.db
                .writer()
                .sequenced_mutation_commit(mutation)
                .map_err(|_| MirageStatus::IoError)
        })();
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        if let Err(error) = result {
            if state.discarded.contains(&inode) {
                // A delete landed while this seal was in flight — the same
                // cleanup discard_inode runs for queued jobs, not a failure.
                invalidate_payload_file(&self.payload_files, &segment.payload_id);
                let _ = std::fs::remove_file(&segment.path);
                self.dirty.used.fetch_sub(segment.len, Ordering::AcqRel);
            } else {
                eprintln!("segment seal failed for inode {inode:?}: {error:?}");
                state.failed.insert(inode);
            }
        }
        if let Some(count) = state.pending.get_mut(&inode) {
            *count -= 1;
            if *count == 0 {
                state.pending.remove(&inode);
                state.discarded.remove(&inode);
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
