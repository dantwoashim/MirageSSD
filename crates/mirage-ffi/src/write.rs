//! Write, truncate, and flush paths with extent-map commits.

use super::engine_ops::floor_free_space;
use super::mutation::now_ns_i64;
use super::*;

/// How long a write waits for uploads to free local budget before it fails
/// with disk full. Kept well below WinFsp's IRP timeout.
const WRITE_BACKPRESSURE_LIMIT: std::time::Duration = std::time::Duration::from_secs(90);

const WRITE_BACKPRESSURE_STEP: std::time::Duration = std::time::Duration::from_millis(250);

/// Loads (or replays) the extent map for `inode` from the durable extent
/// store; a file with no extent history seeds a single base extent covering
/// its committed size so partial writes never need the base first.
/// `wait_slot`/`load_slot` label the dev trace so read and write callers
/// attribute the lock acquisition and the vacant-entry DB load separately.
pub(super) fn extent_map_for(
    handle: &MirageFileHandle,
    inode: InodeId,
    wait_slot: trace::Slot,
    load_slot: trace::Slot,
) -> Result<
    std::sync::MutexGuard<'_, HashMap<InodeId, mirage_engine::extent_map::ExtentMap>>,
    MirageStatus,
> {
    let wait = std::time::Instant::now();
    let mut maps = handle.extents.lock().map_err(|_| MirageStatus::Internal)?;
    trace::record_elapsed(wait_slot, wait);
    if let std::collections::hash_map::Entry::Vacant(slot) = maps.entry(inode) {
        let _load = trace::Scope::new(load_slot);
        let Some(db) = &handle.db else {
            return Err(MirageStatus::BackendUnavailable);
        };
        let volume = handle
            .index
            .as_ref()
            .map(|index| index.header().repository_id)
            .ok_or(MirageStatus::IntegrityFailure)?;
        let map = match db.extent_latest_version(volume, inode) {
            Ok(Some(version)) => {
                let extents = db
                    .extents_at(volume, inode, version)
                    .map_err(|_| MirageStatus::IoError)?;
                let mut map = mirage_engine::extent_map::ExtentMap::replay(volume, inode, &extents);
                // The durable head carries the true EOF and version — an
                // empty extent set (truncate to zero) or a trailing hole is
                // invisible to the row scan alone.
                if let Some((head_version, eof)) = db
                    .extent_head(volume, inode)
                    .map_err(|_| MirageStatus::IoError)?
                {
                    map.set_head(head_version, eof);
                }
                map
            }
            Ok(None) => {
                let mut map = mirage_engine::extent_map::ExtentMap::default();
                if handle.entry.size > 0 && handle.node.is_some() {
                    // Seed one base extent so partial writes preserve the
                    // committed content outside the written range. A
                    // namespace-only file has no committed pages — its
                    // content is entirely dirty extents and holes.
                    map.seed_base(
                        handle.entry.size,
                        mirage_types::PageHash::from_bytes([0; 32]),
                    );
                }
                map
            }
            Err(_) => return Err(MirageStatus::IoError),
        };
        slot.insert(map);
    }
    Ok(maps)
}

/// Atomically commits one extent mutation: the new version's extent set and
/// EOF, the journal operation, and its (already-fsynced) payload record land
/// in a single writer transaction — a crash can never leave a durable
/// reference to an unrecorded version or an operation claiming bytes that
/// were not journaled.
#[allow(clippy::too_many_arguments)]
fn commit_extent_mutation(
    handle: &MirageFileHandle,
    inode: InodeId,
    map: &mirage_engine::extent_map::ExtentMap,
    kind: mirage_db::OperationKind,
    payload_id: [u8; 16],
    payload_path: String,
    payload_bytes: u64,
    payload_checksum: Option<[u8; 32]>,
    physical: Option<mirage_db::PhysicalCommit>,
    now_ns: i64,
) -> Result<(), MirageStatus> {
    let db = handle.db.as_ref().ok_or(MirageStatus::BackendUnavailable)?;
    let volume = handle
        .index
        .as_ref()
        .map(|index| index.header().repository_id)
        .ok_or(MirageStatus::IntegrityFailure)?;
    commit_extent_mutation_parts(
        db,
        volume,
        inode,
        map,
        kind,
        payload_id,
        payload_path,
        payload_bytes,
        payload_checksum,
        physical,
        now_ns,
    )
}

/// The journal payload directory of a managed engine: the configured cache
/// location, or `<state_root>/journal` for engines created before cache
/// placement existed.
pub(super) fn engine_journal_dir(engine: &MirageEngineHandle) -> PathBuf {
    engine.journal_dir.clone().unwrap_or_else(|| {
        engine
            .state_root
            .as_deref()
            .unwrap_or_else(|| Path::new(""))
            .join("journal")
    })
}

/// Same resolution as [`engine_journal_dir`] for a file handle.
pub(super) fn handle_journal_dir(handle: &MirageFileHandle) -> PathBuf {
    handle.journal_dir.clone().unwrap_or_else(|| {
        handle
            .state_root
            .as_deref()
            .unwrap_or_else(|| Path::new(""))
            .join("journal")
    })
}

/// Builds the durable-commit pieces shared by `commit_extent_mutation` and
/// the segment sealer: the extent mutation, the journal operation (whose
/// device sequence the writer assigns inside its transaction), and the
/// already-fsynced payload records.
#[allow(clippy::too_many_arguments)]
pub(super) fn build_extent_mutation_commit(
    volume: RepositoryId,
    inode: InodeId,
    map: &mirage_engine::extent_map::ExtentMap,
    kind: mirage_db::OperationKind,
    payload_id: [u8; 16],
    payload_path: String,
    payload_bytes: u64,
    payload_checksum: Option<[u8; 32]>,
    physical: Option<mirage_db::PhysicalCommit>,
    now_ns: i64,
) -> Result<mirage_db::SequencedMutation, MirageStatus> {
    let extents = map.to_extents(volume, inode, map.version(), now_ns, || {
        let mut id = [0u8; 16];
        let _ = getrandom::fill(&mut id);
        id
    });
    let mut operation_id = [0u8; 16];
    if getrandom::fill(&mut operation_id).is_err() {
        return Err(MirageStatus::Internal);
    }
    let payloads = if payload_path.is_empty() {
        Vec::new()
    } else {
        vec![mirage_db::OperationPayloadRecord {
            payload_id,
            operation_id,
            path: payload_path,
            bytes: i64::try_from(payload_bytes).unwrap_or(i64::MAX),
            checksum: payload_checksum,
            flushed_ns: None,
        }]
    };
    Ok(mirage_db::SequencedMutation {
        reservation: None,
        extents: Some(mirage_db::ExtentMutation {
            volume_id: volume,
            inode,
            version: map.version(),
            eof: map.file_size(),
            extents,
        }),
        operation: mirage_db::OperationRecord {
            operation_id,
            device_seq: 0,
            volume_id: volume,
            base_commit: None,
            kind,
            payload: inode.as_bytes().to_vec(),
            status: mirage_db::OperationStatus::Pending,
            flush_group: None,
            depends_on: None,
            created_ns: now_ns,
        },
        payloads,
        physical,
        modified_ns: None,
        now_ns,
    })
}

/// The transaction body of `commit_extent_mutation` without a file handle —
/// the segment sealer commits closed payloads from its own thread.
#[allow(clippy::too_many_arguments)]
pub(super) fn commit_extent_mutation_parts(
    db: &mirage_db::Database,
    volume: RepositoryId,
    inode: InodeId,
    map: &mirage_engine::extent_map::ExtentMap,
    kind: mirage_db::OperationKind,
    payload_id: [u8; 16],
    payload_path: String,
    payload_bytes: u64,
    payload_checksum: Option<[u8; 32]>,
    physical: Option<mirage_db::PhysicalCommit>,
    now_ns: i64,
) -> Result<(), MirageStatus> {
    let mutation = build_extent_mutation_commit(
        volume,
        inode,
        map,
        kind,
        payload_id,
        payload_path,
        payload_bytes,
        payload_checksum,
        physical,
        now_ns,
    )?;
    db.writer()
        .sequenced_mutation_commit(mutation)
        .map_err(|_| MirageStatus::IoError)
}

/// Writes bytes at `offset` through the versioned extent store: the payload
/// is staged and fsynced, the extent map advances one version, and a journal
/// operation commits — all durable before this call returns success.
///
/// # Safety
/// `handle` must be live; `bytes` must be readable for `bytes_len` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mirage_write(
    handle: *mut MirageFileHandle,
    offset: u64,
    bytes: *const u8,
    bytes_len: usize,
    transferred: *mut usize,
) -> MirageStatus {
    contained(|| {
        if handle.is_null() || transferred.is_null() || (bytes.is_null() && bytes_len != 0) {
            return MirageStatus::InvalidArgument;
        }
        unsafe { *transferred = 0 };
        let handle = unsafe { &*handle };
        let Some(inode) = handle.inode else {
            return MirageStatus::AccessDenied;
        };
        if handle.state_root.is_none() {
            return MirageStatus::BackendUnavailable;
        }
        let data = unsafe { std::slice::from_raw_parts(bytes, bytes_len) };
        let now = now_ns_i64();
        let Some(db) = &handle.db else {
            return MirageStatus::BackendUnavailable;
        };
        let Some(volume) = handle
            .index
            .as_ref()
            .map(|index| index.header().repository_id)
        else {
            return MirageStatus::IntegrityFailure;
        };
        let journal_dir = handle_journal_dir(handle);
        // The journal dir is created once per engine handle, not per write;
        // a directory deleted mid-run is recreated by the payload open's
        // NotFound retry inside the segment append.
        if !handle.journal_dir_ready.swap(true, Ordering::AcqRel) {
            let _mkdir = trace::Scope::new(trace::Slot::WriteCreateDirAll);
            if std::fs::create_dir_all(&journal_dir).is_err() {
                handle.journal_dir_ready.store(false, Ordering::Release);
                return MirageStatus::IoError;
            }
        }
        let Some(dirty) = &handle.dirty else {
            return MirageStatus::Internal;
        };
        let mut maps = match extent_map_for(
            handle,
            inode,
            trace::Slot::WriteLockWait,
            trace::Slot::WriteMapLoadDb,
        ) {
            Ok(maps) => maps,
            Err(status) => return status,
        };
        // Lock order is extents → dirty.mutex → open → state; the dirty mutex
        // serializes the budget check while the maps guard (held first)
        // serializes extent mutations for the whole engine.
        let mut _dirty_guard = match dirty.mutex.lock() {
            Ok(guard) => guard,
            Err(_) => return MirageStatus::Internal,
        };
        let length = u64::try_from(bytes_len).unwrap_or(u64::MAX);
        let mut waited = std::time::Duration::ZERO;
        {
            let _evict_or_floor = trace::Scope::new(trace::Slot::WriteEvictOrFloor);
            while dirty.used.load(Ordering::Acquire).saturating_add(length) > dirty.budget_bytes {
                // Published payloads are reclaimable cloud-backed bytes: evict
                // enough to fit, then re-check the budget.
                {
                    let target = length.max(dirty.budget_bytes / 4);
                    let pins = handle
                        .pins
                        .read()
                        .unwrap_or_else(|poison| poison.into_inner());
                    let _ = publisher::evict_published(
                        db,
                        dirty,
                        &journal_dir,
                        volume,
                        &handle.handles,
                        &handle.payload_files,
                        &pins,
                        target,
                    );
                }
                if dirty.used.load(Ordering::Acquire).saturating_add(length) <= dirty.budget_bytes {
                    break;
                }
                // The budget is full of not-yet-uploaded data. The volume
                // advertises cloud capacity, so a large copy must slow down to
                // upload speed instead of failing: release the ledger, nudge the
                // publisher, and retry for a bounded time before reporting full.
                let Some(publisher) = handle.publisher.as_ref() else {
                    return MirageStatus::DiskFull;
                };
                // Without a Drive token nothing can upload, so waiting is futile.
                let uploading = publisher.stats.state.load(Ordering::Acquire)
                    != publisher::PublisherState::WaitingForToken as u8;
                if !uploading || waited >= WRITE_BACKPRESSURE_LIMIT {
                    return MirageStatus::DiskFull;
                }
                drop(_dirty_guard);
                drop(maps);
                publisher.notify();
                std::thread::sleep(WRITE_BACKPRESSURE_STEP);
                waited += WRITE_BACKPRESSURE_STEP;
                maps = match extent_map_for(
                    handle,
                    inode,
                    trace::Slot::WriteLockWait,
                    trace::Slot::WriteMapLoadDb,
                ) {
                    Ok(maps) => maps,
                    Err(status) => return status,
                };
                _dirty_guard = match dirty.mutex.lock() {
                    Ok(guard) => guard,
                    Err(_) => return MirageStatus::Internal,
                };
            }
            // Real-disk floor: refuse to push the journal volume's actual free
            // space below the configured floor; published payloads are evicted to
            // Drive first, unpublished data is never touched.
            let floor = handle.disk_floor.load(Ordering::Acquire);
            if floor > 0 {
                let free = floor_free_space(&handle.floor_free_cache, &journal_dir, false);
                if free.is_some_and(|free| free < length.saturating_add(floor)) {
                    let deficit = floor
                        .saturating_add(length)
                        .saturating_sub(free.unwrap_or(0));
                    let pins = handle
                        .pins
                        .read()
                        .unwrap_or_else(|poison| poison.into_inner());
                    let _ = publisher::evict_published(
                        db,
                        dirty,
                        &journal_dir,
                        volume,
                        &handle.handles,
                        &handle.payload_files,
                        &pins,
                        length.max(deficit),
                    );
                    let free = floor_free_space(&handle.floor_free_cache, &journal_dir, true);
                    if free.is_some_and(|free| free < length.saturating_add(floor)) {
                        return MirageStatus::DiskFull;
                    }
                }
            }
        }
        // Write-behind: the chunk joins the inode's open segment (or rolls it
        // and starts a new one); the sealer thread performs the durable
        // commit. `dirty.used` grows now so the budget counts unsealed bytes —
        // they are never evictable because they have no rows yet.
        let Some(segments) = &handle.segments else {
            return MirageStatus::Internal;
        };
        {
            let _append = trace::Scope::new(trace::Slot::WriteSegmentAppend);
            match segments.append(inode, offset, data, now, &mut maps) {
                Ok(()) => handle.wrote.store(true, Ordering::Release),
                Err(status) => return status,
            }
        }
        dirty.used.fetch_add(length, Ordering::AcqRel);
        drop(maps);
        unsafe { *transferred = bytes_len };
        MirageStatus::Ok
    })
}

/// Truncates a file's extent map: extents beyond the end drop, the last
/// extent clips, and later extension reads zeros.
///
/// # Safety
/// `handle` must be live.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mirage_truncate(
    handle: *mut MirageFileHandle,
    new_size: u64,
) -> MirageStatus {
    contained(|| {
        if handle.is_null() {
            return MirageStatus::InvalidArgument;
        }
        let handle = unsafe { &*handle };
        let Some(inode) = handle.inode else {
            return MirageStatus::AccessDenied;
        };
        // A truncate must commit over only sealed payloads — seal the inode's
        // open segment before mutating the map.
        if let Some(segments) = &handle.segments
            && segments.drain_inode(inode).is_err()
        {
            return MirageStatus::IoError;
        }
        let now = now_ns_i64();
        let mut maps = match extent_map_for(
            handle,
            inode,
            trace::Slot::WriteLockWait,
            trace::Slot::WriteMapLoadDb,
        ) {
            Ok(maps) => maps,
            Err(status) => return status,
        };
        // Mutate a scratch copy: the live map only publishes after the
        // durable commit lands.
        let mut next_map = maps.get(&inode).expect("map just inserted").clone();
        // Growing a file is how the kernel cache manager pre-allocates before
        // a cached write, so the disk-full answer has to come from here: a
        // file that can never fit the local staging budget, or that would
        // breach the cache disk's free-space floor, is refused up front.
        let current = next_map.file_size();
        if new_size > current
            && let Some(dirty) = &handle.dirty
        {
            let growth = new_size - current;
            if new_size > dirty.budget_bytes {
                return MirageStatus::DiskFull;
            }
            let floor = handle.disk_floor.load(Ordering::Acquire);
            if floor > 0 {
                let journal_dir = handle_journal_dir(handle);
                let free = floor_free_space(&handle.floor_free_cache, &journal_dir, true);
                if free.is_some_and(|free| free < growth.saturating_add(floor)) {
                    return MirageStatus::DiskFull;
                }
            }
        }
        if next_map.truncate(new_size).is_err() {
            return MirageStatus::InvalidArgument;
        }
        if commit_extent_mutation(
            handle,
            inode,
            &next_map,
            mirage_db::OperationKind::Truncate,
            [0; 16],
            String::new(),
            0,
            None,
            None,
            now,
        )
        .is_err()
        {
            return MirageStatus::IoError;
        }
        maps.insert(inode, next_map);
        handle.wrote.store(true, Ordering::Release);
        MirageStatus::Ok
    })
}

/// FlushFileBuffers: opens a flush fence over the volume's committed
/// operations — a local-durability acknowledgement, not cloud completion.
///
/// # Safety
/// `handle` must be live.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mirage_flush(handle: *mut MirageFileHandle) -> MirageStatus {
    contained(|| {
        if handle.is_null() {
            return MirageStatus::InvalidArgument;
        }
        let handle = unsafe { &*handle };
        // Seal any unsealed writes first: a successful flush must leave the
        // file's bytes durable.
        if let (Some(inode), Some(segments)) = (handle.inode, &handle.segments)
            && segments.drain_inode(inode).is_err()
        {
            return MirageStatus::IoError;
        }
        let Some(db) = &handle.db else {
            return MirageStatus::Ok; // nothing durable to fence
        };
        let volume = match handle
            .index
            .as_ref()
            .map(|index| index.header().repository_id)
        {
            Some(volume) => volume,
            None => return MirageStatus::Ok,
        };
        let journal = mirage_engine::journal::LocalJournal::new(db.clone(), volume);
        match journal.flush_fence(now_ns_i64()) {
            Ok(_) => {
                // Group commit made the fence a NORMAL commit — the barrier's
                // FULL commit fsyncs the WAL so the sealed extents, the
                // fence, and everything before them are power-loss durable.
                if handle.group_durability && db.durability_barrier().is_err() {
                    return MirageStatus::IoError;
                }
                if let Some(publisher) = &handle.publisher {
                    publisher.notify();
                }
                MirageStatus::Ok
            }
            Err(_) => MirageStatus::IoError,
        }
    })
}

/// Re-stages an evicted published payload: fetch and verify the whole remote
/// object, write it back to the journal atomically, revive the physical
/// extent in one writer transaction, and re-add its bytes to the dirty
/// ledger — only when the dirty budget and the disk floor admit it.
/// Callers fall back to the transient frame fetch on any error.
pub(super) fn restage_evicted_payload(
    handle: &MirageFileHandle,
    journal_dir: &Path,
    payload_id: &[u8; 16],
) -> Result<(), MirageError> {
    let (Some(remote), Some(db), Some(dirty)) =
        (&handle.remote_payloads, &handle.db, &handle.dirty)
    else {
        return Err(MirageError::backend_unavailable(
            "restage needs publication state",
        ));
    };
    let Some(volume) = handle
        .index
        .as_ref()
        .map(|index| index.header().repository_id)
    else {
        return Err(MirageError::internal_invariant("restage without a volume"));
    };
    // Dedupes concurrent cold reads of the same payload.
    let _slot = remote
        .begin_restage(payload_id)
        .ok_or_else(|| MirageError::repository_conflict("payload restage already in flight"))?;
    let Some(whole) = remote.fetch_whole(db, volume, payload_id)? else {
        return Err(MirageError::backend_unavailable("no publication record"));
    };
    let bytes = whole.bytes;
    let length = bytes.len() as u64;
    // The ledger mutex serializes the budget check with the byte add.
    let _serialize = dirty
        .mutex
        .lock()
        .map_err(|_| MirageError::internal_invariant("dirty ledger lock poisoned"))?;
    if dirty.used.load(Ordering::Acquire).saturating_add(length) > dirty.budget_bytes {
        return Err(MirageError::repository_conflict("dirty budget full"));
    }
    let floor = handle.disk_floor.load(Ordering::Acquire);
    if floor > 0 {
        let free = floor_free_space(&handle.floor_free_cache, journal_dir, false)
            .ok_or_else(|| MirageError::backend_unavailable("free space probe failed"))?;
        if free.saturating_sub(length) < floor {
            return Err(MirageError::repository_conflict("disk floor would breach"));
        }
    }
    let journal = mirage_engine::journal::LocalJournal::new(db.clone(), volume);
    // A cached read handle to a delete-pending name would block recreation.
    handles::invalidate_payload_file(&handle.payload_files, payload_id);
    let staged = journal.stage_payload_as(journal_dir, &bytes, *payload_id)?;
    db.writer().physical_revive_extent(
        *payload_id,
        dirty.file_id,
        dirty.next_slot.fetch_add(1, Ordering::AcqRel),
        i64::try_from(staged.bytes).unwrap_or(i64::MAX),
        mirage_types::PageHash::from_bytes(staged.checksum),
        staged.checksum,
        now_ns_i64(),
    )?;
    dirty.used.fetch_add(length, Ordering::AcqRel);
    // Force the next floor probe to see the re-staged bytes.
    if let Ok(mut cache) = handle.floor_free_cache.lock() {
        cache.0 = std::time::Instant::now() - std::time::Duration::from_secs(2);
    }
    Ok(())
}
