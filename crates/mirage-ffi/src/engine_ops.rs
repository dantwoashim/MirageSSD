//! Engine lifecycle, compaction, floor, and eviction controls.

use super::mutation::JOURNAL_FILE_ID;
use super::mutation::now_ns_i64;
use super::write::engine_journal_dir;
use super::*;

/// Supplies (or rotates) the Drive bearer token for a managed engine created
/// with `mirage_engine_create_managed_drive`. The first call installs the
/// provider hook on the volume coordinator; later calls swap the backend's
/// credential in place. Token bytes are never logged or retained beyond the
/// backend's zeroizing store.
///
/// # Safety
/// `token` is a UTF-8 byte string valid for `token_len`; `engine` is a live
/// managed engine handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mirage_engine_set_drive_token(
    engine: *mut MirageEngineHandle,
    token: *const u8,
    token_len: usize,
) -> MirageStatus {
    contained(|| {
        if engine.is_null() || token.is_null() || token_len == 0 || token_len > 16 * 1024 {
            return MirageStatus::InvalidArgument;
        }
        let engine = unsafe { &*engine };
        let Some(provider) = &engine.managed_provider else {
            return MirageStatus::InvalidArgument;
        };
        let bytes = unsafe { std::slice::from_raw_parts(token, token_len) };
        let token = zeroize::Zeroizing::new(Vec::from(bytes));
        let Ok(text) = std::str::from_utf8(token.as_slice()) else {
            return MirageStatus::InvalidArgument;
        };
        if provider.set_token(text).is_err() {
            return MirageStatus::BackendUnavailable;
        }
        if let Some(coordinator) = &engine.coordinator
            && let Ok(false) = provider.installed.compare_exchange(
                false,
                true,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
        {
            coordinator.set_provider(provider.hook());
        }
        if let Some(publisher) = &engine.publisher {
            publisher.notify();
        }
        MirageStatus::Ok
    })
}

/// Test-only crash simulation: stops the sealer WITHOUT draining and leaks
/// the engine so the owner record stays live — the next mount adopts it as
/// Recovering. Any unsealed segment payload must be swept at the next mount.
///
/// Test-only: the dirty ledger's currently-counted bytes (0 without one).
///
/// # Safety
/// `engine` must be a live engine handle or null.
#[doc(hidden)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mirage_engine_dirty_used_for_tests(
    engine: *const MirageEngineHandle,
) -> u64 {
    unsafe { engine.as_ref() }
        .and_then(|engine| engine.dirty.as_ref())
        .map(|dirty| dirty.used.load(Ordering::Acquire))
        .unwrap_or(0)
}

/// # Safety
/// `handle` must be null or a live engine handle.
#[doc(hidden)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mirage_engine_abandon_for_tests(
    handle: *mut MirageEngineHandle,
) -> MirageStatus {
    contained(|| {
        if handle.is_null() {
            return MirageStatus::Ok;
        }
        let mut engine = unsafe { Box::from_raw(handle) };
        if let Some(segments) = engine.segments.take() {
            segments.abandon();
        }
        if let Some(coordinator) = &engine.coordinator {
            coordinator.force_release_owner_lock_for_tests();
        }
        // Crash simulation: leak the engine so the owner record stays in its
        // live state — the next mount must adopt it as Recovering.
        std::mem::forget(engine);
        MirageStatus::Ok
    })
}

/// # Safety
/// `handle` must be null or a pointer returned by an engine constructor and not
/// previously destroyed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mirage_engine_destroy(handle: *mut MirageEngineHandle) -> MirageStatus {
    contained(|| {
        if !handle.is_null() {
            let engine = unsafe { Box::from_raw(handle) };
            if let Some(segments) = &engine.segments {
                segments.drain_all();
                segments.stop();
            }
            if let Some(db) = &engine.db {
                // All drained seals are committed — one barrier fsyncs them
                // durable before the handle is freed.
                let _ = db.durability_barrier();
            }
            if let Some(publisher) = &engine.publisher {
                publisher.stop();
                let stats = engine_publication_stats(engine.as_ref());
                eprintln!(
                    "payload publisher stats: pending={} ({}B) published={} ({}B) evicted={} refusals={}",
                    stats.pending_payloads,
                    stats.pending_bytes,
                    stats.published_payloads,
                    stats.published_bytes,
                    stats.evicted_payloads,
                    stats.integrity_refusals
                );
            }
            if let Some(log) = &engine.violations {
                log.summary();
            }
            drop(engine);
        }
        MirageStatus::Ok
    })
}

/// Report the volume ownership epoch for fencing service-side control
/// requests against a restarted host.
///
/// # Safety
/// `engine` must be a live engine handle; `output` must be writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mirage_engine_epoch(
    engine: *const MirageEngineHandle,
    output: *mut u64,
) -> MirageStatus {
    contained(|| {
        if engine.is_null() || output.is_null() {
            return MirageStatus::InvalidArgument;
        }
        let epoch = unsafe { &*engine }
            .coordinator
            .as_ref()
            .map(|coordinator| coordinator.epoch())
            .unwrap_or(0);
        unsafe { ptr::write(output, epoch) };
        MirageStatus::Ok
    })
}

/// Mark the volume mounted once the host's dispatcher is live. A coordinator
/// adopted in `Recovering` is advanced through `Starting` first.
///
/// # Safety
/// `engine` must be a live engine handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mirage_engine_mark_mounted(
    engine: *const MirageEngineHandle,
) -> MirageStatus {
    contained(|| {
        if engine.is_null() {
            return MirageStatus::InvalidArgument;
        }
        let Some(coordinator) = &unsafe { &*engine }.coordinator else {
            return MirageStatus::Ok;
        };
        use mirage_engine::volume::VolumeState;
        let engine = unsafe { &*engine };
        let mark = || -> Result<(), MirageError> {
            if coordinator.state() == VolumeState::Recovering {
                // Startup recovery: pending operations were never
                // acknowledged, so their payloads are safe to reclaim;
                // committed/flushed operations already carry durable extent
                // state and replay on demand. A failed recovery must not
                // report ready — the coordinator stays Recovering.
                if let (Some(db), Some(state_root)) = (&engine.db, &engine.state_root) {
                    let volume = coordinator.repository_id();
                    let journal = mirage_engine::journal::LocalJournal::new(db.clone(), volume);
                    journal.reclaim_pending(
                        engine
                            .journal_dir
                            .as_deref()
                            .unwrap_or(&state_root.join("journal")),
                        now_ns_i64(),
                    )?;
                }
                coordinator.transition(VolumeState::Starting)?;
            }
            if coordinator.state() == VolumeState::Starting {
                coordinator.transition(VolumeState::Mounted)?;
            }
            Ok(())
        };
        match mark() {
            Ok(()) => MirageStatus::Ok,
            Err(_) => MirageStatus::IntegrityFailure,
        }
    })
}

/// Publication counters for one managed engine, read from the durable
/// ledger plus the in-process publisher state.
fn engine_publication_stats(engine: &MirageEngineHandle) -> MiragePublicationStats {
    let mut stats = MiragePublicationStats::default();
    if let (Some(db), Some(index)) = (&engine.db, &engine.index) {
        let volume = index.header().repository_id;
        if let Ok(db_stats) = db.payload_publication_stats(volume, &JOURNAL_FILE_ID) {
            stats.pending_payloads = db_stats.pending_payloads;
            stats.pending_bytes = db_stats.pending_bytes;
            stats.published_payloads = db_stats.published_payloads;
            stats.published_bytes = db_stats.published_bytes;
            stats.evicted_payloads = db_stats.evicted_payloads;
        }
    }
    if let Some(publisher) = &engine.publisher {
        stats.integrity_refusals = publisher.stats.integrity_refusals.load(Ordering::Relaxed);
        if let Ok(slot) = publisher.stats.last_error_class.lock() {
            stats.last_error_class = *slot;
        }
    }
    stats
}

/// Payload publication statistics for a managed volume.
#[repr(C)]
#[derive(Debug, Default, Clone)]
pub struct MiragePublicationStats {
    /// Committed payloads still referenced but not yet published.
    pub pending_payloads: u64,
    /// Plaintext bytes of pending payloads.
    pub pending_bytes: u64,
    /// Payloads with a remote object.
    pub published_payloads: u64,
    /// Plaintext bytes of published payloads.
    pub published_bytes: u64,
    /// Published payloads whose local copy was evicted.
    pub evicted_payloads: u64,
    /// Payloads refused for checksum mismatch.
    pub integrity_refusals: u64,
    /// NUL-padded ASCII error class of the last publish failure.
    pub last_error_class: [u8; 32],
}

/// Reports payload publication statistics for a managed engine.
///
/// # Safety
/// `engine` must be live; `output` must be writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mirage_engine_publication_stats(
    engine: *const MirageEngineHandle,
    output: *mut MiragePublicationStats,
) -> MirageStatus {
    contained(|| {
        if engine.is_null() || output.is_null() {
            return MirageStatus::InvalidArgument;
        }
        let stats = engine_publication_stats(unsafe { &*engine });
        unsafe { ptr::write(output, stats) };
        MirageStatus::Ok
    })
}

/// Remaining dirty-payload budget for a managed volume.
///
/// # Safety
/// `engine` must be a live engine handle; `output` must be writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mirage_engine_dirty_free(
    engine: *const MirageEngineHandle,
    output: *mut u64,
) -> MirageStatus {
    contained(|| {
        if engine.is_null() || output.is_null() {
            return MirageStatus::InvalidArgument;
        }
        let Some(dirty) = &unsafe { &*engine }.dirty else {
            return MirageStatus::InvalidArgument;
        };
        unsafe { ptr::write(output, dirty.free()) };
        MirageStatus::Ok
    })
}

/// Quiesce-time compaction for a managed volume: superseded extent versions
/// drop inside one transaction, payload extents no longer referenced are
/// marked dead and their files deleted, and namespace history is bounded.
/// No-op for engines without managed state.
///
/// # Safety
/// `engine` must be a live engine handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mirage_engine_compact(engine: *const MirageEngineHandle) -> MirageStatus {
    contained(|| {
        if engine.is_null() {
            return MirageStatus::InvalidArgument;
        }
        let engine = unsafe { &*engine };
        let (Some(db), Some(_), Some(dirty)) = (&engine.db, &engine.state_root, &engine.dirty)
        else {
            return MirageStatus::Ok;
        };
        let Some(volume) = engine
            .index
            .as_ref()
            .map(|index| index.header().repository_id)
        else {
            return MirageStatus::IntegrityFailure;
        };
        let now = now_ns_i64();
        let dead = match db
            .writer()
            .extent_compact_volume(volume, dirty.file_id, now)
        {
            Ok(dead) => dead,
            Err(_) => return MirageStatus::IntegrityFailure,
        };
        let journal_dir = engine_journal_dir(engine);
        if !dead.is_empty() {
            // Irreversible deletes: the extent-demotion commit that marked
            // these payloads dead must be power-loss durable first.
            if db.durability_barrier().is_err() {
                return MirageStatus::IoError;
            }
        }
        for (payload_id, length_bytes) in dead {
            // Only after the ledger commit does the file go; a failed delete
            // is left for the startup orphan sweep.
            let mut name = String::with_capacity(40);
            for byte in payload_id {
                name.push_str(&format!("{byte:02x}"));
            }
            name.push_str(".payload");
            crate::handles::invalidate_payload_file(&engine.payload_files, &payload_id);
            let _ = std::fs::remove_file(journal_dir.join(name));
            let length = u64::try_from(length_bytes).unwrap_or(0);
            let _ = dirty
                .used
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |used| {
                    Some(used.saturating_sub(length))
                });
        }
        // Over-high-watermark published payloads become remote-only so the
        // next mount starts under budget; unpublished payloads stay local.
        let used = dirty.used.load(Ordering::Acquire);
        if used > dirty.budget_bytes * 80 / 100 {
            let target = used.saturating_sub(dirty.budget_bytes * 60 / 100);
            let pins = engine
                .pins
                .read()
                .unwrap_or_else(|poison| poison.into_inner());
            let _ = publisher::evict_published(
                db,
                dirty,
                &journal_dir,
                volume,
                &engine.handles,
                &engine.payload_files,
                &pins,
                target,
            );
        }
        // Namespace history is bounded too: deltas older than the newest
        // checkpoint may drop once at least the configured minimum survives.
        let compaction = mirage_engine::compaction::Compaction::new(db, volume);
        let retain_from = match db.namespace_latest_checkpoint_seq(volume) {
            Ok(seq) => seq.unwrap_or(0),
            Err(_) => return MirageStatus::IoError,
        };
        match compaction.bound_namespace_history(retain_from, MANAGED_HISTORY_MIN_KEEP, now) {
            Ok(()) => MirageStatus::Ok,
            Err(_) => MirageStatus::IntegrityFailure,
        }
    })
}

/// Namespace deltas retained per volume by quiesce-time compaction.
const MANAGED_HISTORY_MIN_KEEP: i64 = 4096;

/// Quiesce the volume: stop admitting reads, drain active readers up to
/// `timeout_ms`, then mark the volume unmounted. The coordinator is released
/// by `mirage_engine_destroy`.
///
/// # Safety
/// `engine` must be a live engine handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mirage_engine_quiesce(
    engine: *const MirageEngineHandle,
    timeout_ms: u32,
) -> MirageStatus {
    contained(|| {
        if engine.is_null() {
            return MirageStatus::InvalidArgument;
        }
        let engine_ref = unsafe { &*engine };
        if let Some(segments) = &engine_ref.segments {
            segments.drain_all();
            segments.stop();
        }
        if let Some(publisher) = &engine_ref.publisher {
            publisher.drain();
        }
        // Everything drained is committed; the barrier fsync makes the
        // whole batch power-loss durable in one commit.
        if let Some(db) = &engine_ref.db
            && db.durability_barrier().is_err()
        {
            return MirageStatus::IoError;
        }
        let Some(coordinator) = &engine_ref.coordinator else {
            return MirageStatus::Ok;
        };
        match coordinator.quiesce(std::time::Duration::from_millis(u64::from(timeout_ms))) {
            Ok(()) => MirageStatus::Ok,
            Err(_) => MirageStatus::WouldBlock,
        }
    })
}

/// Free bytes on the volume holding `path`, read through a 1-second cache
/// unless `force` refreshes it after an eviction pass.
pub(super) fn floor_free_space(
    cache_cell: &std::sync::Mutex<(std::time::Instant, u64)>,
    journal_dir: &Path,
    force: bool,
) -> Option<u64> {
    if let Ok(cache) = cache_cell.lock()
        && !force
        && cache.0.elapsed() < std::time::Duration::from_secs(1)
    {
        return Some(cache.1);
    }
    let free = volume_free_bytes(journal_dir)?;
    if let Ok(mut cache) = cache_cell.lock() {
        *cache = (std::time::Instant::now(), free);
    }
    Some(free)
}

#[cfg(windows)]
fn volume_free_bytes(path: &Path) -> Option<u64> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut free = 0_u64;
    let mut total = 0_u64;
    let mut total_free = 0_u64;
    // SAFETY: NUL-terminated input; all output pointers are valid and unique.
    let ok = unsafe { GetDiskFreeSpaceExW(wide.as_ptr(), &mut free, &mut total, &mut total_free) };
    (ok != 0).then_some(free)
}

#[cfg(not(windows))]
fn volume_free_bytes(_path: &Path) -> Option<u64> {
    None
}

/// Configures the write-admission free-space floor for a managed engine
/// (the `--floor` host argument; 0 disables).
/// # Safety
/// `engine` must be a live handle from `mirage_engine_create_*` or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mirage_engine_set_disk_floor(
    engine: *mut MirageEngineHandle,
    floor_bytes: u64,
) -> MirageStatus {
    let Some(handle) = (unsafe { engine.as_ref() }) else {
        return MirageStatus::InvalidArgument;
    };
    handle
        .disk_floor
        .store(floor_bytes, std::sync::atomic::Ordering::Release);
    MirageStatus::Ok
}

/// `EVICT <bytes>` control command on the host's stdin: evicts published
/// payloads oldest-first and reports the freed journal bytes. Only payloads
/// already published remotely are candidates; dirty data is never removed.
/// # Safety
/// `engine` must be a live handle from `mirage_engine_create_*` or null;
/// `freed_bytes` must be a valid writable `u64` or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mirage_engine_evict_published(
    engine: *mut MirageEngineHandle,
    target_bytes: u64,
    freed_bytes: *mut u64,
    blocked_bytes: *mut u64,
) -> MirageStatus {
    let Some(handle) = (unsafe { engine.as_ref() }) else {
        return MirageStatus::InvalidArgument;
    };
    let (Some(db), Some(dirty), Some(_), Some(volume)) = (
        handle.db.as_ref(),
        handle.dirty.as_ref(),
        handle.state_root.as_ref(),
        handle
            .index
            .as_ref()
            .map(|index| index.header().repository_id),
    ) else {
        return MirageStatus::InvalidArgument;
    };
    let pins = handle
        .pins
        .read()
        .unwrap_or_else(|poison| poison.into_inner());
    match publisher::evict_published(
        db,
        dirty,
        &engine_journal_dir(handle),
        volume,
        &handle.handles,
        &handle.payload_files,
        &pins,
        target_bytes,
    ) {
        Ok((freed, blocked)) => {
            if let Some(out) = unsafe { freed_bytes.as_mut() } {
                *out = freed;
            }
            if let Some(out) = unsafe { blocked_bytes.as_mut() } {
                *out = blocked;
            }
            MirageStatus::Ok
        }
        Err(_) => MirageStatus::IoError,
    }
}

/// `PINS-RELOAD` control command: refreshes the pinned-inode set from the
/// durable `namespace_pins` table after a `mirage pin/unpin`.
///
/// # Safety
/// `engine` must be a live managed engine handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mirage_engine_reload_pins(
    engine: *mut MirageEngineHandle,
) -> MirageStatus {
    let Some(handle) = (unsafe { engine.as_ref() }) else {
        return MirageStatus::InvalidArgument;
    };
    let (Some(db), Some(volume)) = (
        handle.db.as_ref(),
        handle
            .index
            .as_ref()
            .map(|index| index.header().repository_id),
    ) else {
        return MirageStatus::InvalidArgument;
    };
    match db.namespace_pins(volume) {
        Ok(inodes) => {
            let mut set = handle
                .pins
                .write()
                .unwrap_or_else(|poison| poison.into_inner());
            *set = inodes.into_iter().collect();
            MirageStatus::Ok
        }
        Err(_) => MirageStatus::IoError,
    }
}
