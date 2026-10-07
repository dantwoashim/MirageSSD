//! Dev-only internal timing trace for the FFI layer. Enabled when
//! `MIRAGE_FS_TRACE` is set (the same env var as the WinFsp adapter trace —
//! its value is a file path); a background thread rewrites
//! `<path>.ffi.tsv` every 2 s with per-slot count/total/max in
//! microseconds. When unset, the only cost per scope is one cached
//! `OnceLock` check. Zero behavior change: nothing here touches engine
//! state.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{OnceLock, atomic::AtomicBool};
use std::time::{Duration, Instant};

#[derive(Clone, Copy)]
#[repr(usize)]
pub(crate) enum Slot {
    ReadLockWait,
    ReadMapLoadDb,
    ReadPlan,
    ReadViaExtentsTotal,
    ReadPayloadOpenMiss,
    ReadPayloadCacheHit,
    ReadSeekRead,
    ReadRestage,
    ReadRemote,
    ReadIndexPath,
    WriteLockWait,
    WriteMapLoadDb,
    WriteCreateDirAll,
    WriteSegmentAppend,
    WriteNewPayloadFile,
    WriteEvictOrFloor,
    CloseDrainWait,
    CloseHandlesClose,
    CloseDeleteFinalize,
    SealQueueLatency,
    SealFsync,
    SealCommit,
    LookupTotal,
    LookupResolveDb,
    LookupMapLoad,
    NsCreate,
    NsDelete,
    NsRename,
}

const SLOT_COUNT: usize = Slot::NsRename as usize + 1;

const NAMES: [&str; SLOT_COUNT] = [
    "read.lock_wait",
    "read.map_load_db",
    "read.plan",
    "read.via_extents_total",
    "read.payload_open_miss",
    "read.payload_cache_hit",
    "read.seek_read",
    "read.restage",
    "read.remote",
    "read.index_path",
    "write.lock_wait",
    "write.map_load_db",
    "write.create_dir_all",
    "write.segment_append",
    "write.new_payload_file",
    "write.evict_or_floor",
    "close.drain_wait",
    "close.handles_close",
    "close.delete_finalize",
    "seal.queue_latency",
    "seal.fsync",
    "seal.commit",
    "lookup.total",
    "lookup.resolve_db",
    "lookup.map_load",
    "ns.create",
    "ns.delete",
    "ns.rename",
];

// Intentionally interior-mutable: each array element needs its own atomic —
// the const is only a fresh-value template for the array initializer.
#[allow(clippy::declare_interior_mutable_const)]
const ZERO: AtomicU64 = AtomicU64::new(0);
static COUNT: [AtomicU64; SLOT_COUNT] = [ZERO; SLOT_COUNT];
static TOTAL_NS: [AtomicU64; SLOT_COUNT] = [ZERO; SLOT_COUNT];
static MAX_NS: [AtomicU64; SLOT_COUNT] = [ZERO; SLOT_COUNT];

static DUMPING: AtomicBool = AtomicBool::new(false);

/// The trace target is `MIRAGE_FS_TRACE`'s value with `.ffi.tsv` appended.
/// On first enablement the background flusher thread is spawned here — it
/// calls `target()` itself, which blocks until this init completes.
fn target() -> Option<&'static std::path::Path> {
    TRACE_TARGET.get_or_init(|| {
        let target = std::env::var_os("MIRAGE_FS_TRACE").map(std::path::PathBuf::from);
        if target.is_some() {
            let _ = std::thread::Builder::new()
                .name("mirage-ffi-trace".into())
                .spawn(flusher_loop);
        }
        target
    });
    TRACE_TARGET.get().and_then(Option::as_deref)
}
static TRACE_TARGET: OnceLock<Option<std::path::PathBuf>> = OnceLock::new();

pub(crate) fn enabled() -> bool {
    target().is_some()
}

pub(crate) fn record_elapsed(slot: Slot, since: Instant) {
    if !enabled() {
        return;
    }
    record(
        slot,
        since.elapsed().as_nanos().min(u64::MAX as u128) as u64,
    );
}

pub(crate) fn record(slot: Slot, elapsed_ns: u64) {
    if !enabled() {
        return;
    }
    let slot = slot as usize;
    COUNT[slot].fetch_add(1, Ordering::Relaxed);
    TOTAL_NS[slot].fetch_add(elapsed_ns, Ordering::Relaxed);
    MAX_NS[slot].fetch_max(elapsed_ns, Ordering::Relaxed);
}

/// RAII scope timer: records `elapsed()` into `slot` on drop.
pub(crate) struct Scope {
    slot: Slot,
    start: Option<Instant>,
}

impl Scope {
    pub(crate) fn new(slot: Slot) -> Self {
        Self {
            slot,
            start: if enabled() {
                Some(Instant::now())
            } else {
                None
            },
        }
    }
}

impl Drop for Scope {
    fn drop(&mut self) {
        if let Some(start) = self.start.take() {
            let slot = self.slot as usize;
            let elapsed = start.elapsed().as_nanos().min(u64::MAX as u128) as u64;
            COUNT[slot].fetch_add(1, Ordering::Relaxed);
            TOTAL_NS[slot].fetch_add(elapsed, Ordering::Relaxed);
            MAX_NS[slot].fetch_max(elapsed, Ordering::Relaxed);
        }
    }
}

fn flusher_loop() {
    loop {
        std::thread::sleep(Duration::from_secs(2));
        dump();
    }
}

fn dump() {
    let Some(target) = target() else { return };
    // Single writer (only the flusher thread calls this), but serialize
    // defensively anyway.
    if DUMPING.swap(true, Ordering::AcqRel) {
        return;
    }
    struct Guard;
    impl Drop for Guard {
        fn drop(&mut self) {
            DUMPING.store(false, Ordering::Release);
        }
    }
    let _guard = Guard;

    let mut rows: Vec<(&'static str, u64, u64, u64)> = Vec::with_capacity(SLOT_COUNT);
    for slot in 0..SLOT_COUNT {
        let count = COUNT[slot].load(Ordering::Relaxed);
        if count == 0 {
            continue;
        }
        let total = TOTAL_NS[slot].load(Ordering::Relaxed);
        let max = MAX_NS[slot].load(Ordering::Relaxed);
        rows.push((NAMES[slot], count, total, max));
    }
    rows.sort_by(|a, b| b.2.cmp(&a.2));
    let mut path = target.as_os_str().to_owned();
    path.push(".ffi.tsv");
    let mut out = String::from("kind\tname\tcount\ttotal_us\tmean_us\tmax_us\n");
    for (name, count, total, max) in rows {
        let total_us = total / 1000;
        let max_us = max / 1000;
        let mean_us = (total_us + count / 2) / count;
        out.push_str(&format!(
            "ffi-inner\t{name}\t{count}\t{total_us}\t{mean_us}\t{max_us}\n"
        ));
    }
    let _ = std::fs::write(std::path::PathBuf::from(path), out);
}
