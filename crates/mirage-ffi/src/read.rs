//! Lookup and read paths including origin fetch and readahead.

use super::mutation::managed_path_components;
use super::write::extent_map_for;
use super::write::handle_journal_dir;
use super::write::restage_evicted_payload;
use super::*;

/// # Safety
/// Pointers must be valid for their explicit lengths; output is writable and the engine outlives the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mirage_lookup(
    engine: *const MirageEngineHandle,
    path: *const u16,
    path_len: usize,
    output: *mut *mut MirageFileHandle,
) -> MirageStatus {
    contained(|| {
        let _total = trace::Scope::new(trace::Slot::LookupTotal);
        if engine.is_null() || output.is_null() || (path.is_null() && path_len != 0) {
            return MirageStatus::InvalidArgument;
        }
        let units = if path_len == 0 {
            &[]
        } else {
            unsafe { std::slice::from_raw_parts(path, path_len) }
        };
        let Some(key) = namespace::normalize(units) else {
            return MirageStatus::InvalidArgument;
        };
        let engine = unsafe { &*engine };
        if let Some(index) = &engine.index {
            let Ok(path) = String::from_utf16(&key) else {
                return MirageStatus::InvalidArgument;
            };
            let volume = index.header().repository_id;
            // Managed volumes resolve names through the durable namespace:
            // it is authoritative for which paths exist, and the index only
            // supplies committed content via the inode's legacy binding.
            // Path components come from the raw UTF-16 path so both `\` and
            // `/` separators resolve the same namespace entries.
            let (node, namespace_inode, directory, namespace_size) = if engine.managed
                && let Some(db) = &engine.db
            {
                let components = match managed_path_components(&key) {
                    Ok(components) => components,
                    Err(status) => return status,
                };
                let inode = {
                    let _resolve = trace::Scope::new(trace::Slot::LookupResolveDb);
                    match db.namespace_resolve_components(volume, &components) {
                        Ok(Some(inode)) => inode,
                        Ok(None) => return MirageStatus::NotFound,
                        Err(_) => return MirageStatus::IoError,
                    }
                };
                let stat = match db.namespace_stat(volume, inode) {
                    Ok(Some(stat)) => stat,
                    Ok(None) => return MirageStatus::IntegrityFailure,
                    Err(_) => return MirageStatus::IoError,
                };
                let node = match db.namespace_legacy_path(volume, inode) {
                    Ok(Some(legacy)) => match index.lookup_path(&legacy) {
                        Ok(node) => node,
                        Err(_) => return MirageStatus::IntegrityFailure,
                    },
                    Ok(None) => None,
                    Err(_) => return MirageStatus::IoError,
                };
                (
                    node,
                    Some(inode),
                    matches!(stat.kind, mirage_db::NamespaceNodeKind::Directory),
                    Some(stat.size),
                )
            } else {
                let node = match index.lookup_path(&path) {
                    Ok(Some(node)) => node,
                    Ok(None) => return MirageStatus::NotFound,
                    Err(_) => return MirageStatus::IntegrityFailure,
                };
                (
                    Some(node),
                    engine.db.as_ref().and_then(|db| {
                        managed_path_components(&key).ok().and_then(|components| {
                            db.namespace_resolve_components(volume, &components)
                                .ok()
                                .flatten()
                        })
                    }),
                    matches!(node, NodeIndex::Directory(_)),
                    None,
                )
            };
            if engine.trace_lookups
                && let (Some(NodeIndex::File(ordinal)), Some(log)) = (node, &engine.violations)
            {
                log.lookup(u64::from(ordinal), &path);
            }
            // Register the open before the handle exists so share accounting,
            // delete-pending, and published-payload eviction protection see it.
            if let Some(inode) = namespace_inode
                && engine
                    .handles
                    .open(
                        inode,
                        mirage_engine::handles::DesiredAccess {
                            read: true,
                            write: true,
                            delete: true,
                        },
                        mirage_engine::handles::ShareAccess::ALL,
                    )
                    .is_err()
            {
                return MirageStatus::Conflict;
            }
            // Index ordinals identify committed content; inode-derived ids
            // keep identity stable for namespace-only files across renames.
            let stable_index = node
                .map(|node| match node {
                    NodeIndex::Directory(ordinal) | NodeIndex::File(ordinal) => u64::from(ordinal),
                })
                .or_else(|| namespace_inode.map(|inode: InodeId| inode_stable_index(inode)))
                .unwrap_or(0);
            let entry = if directory {
                Entry {
                    index: stable_index,
                    size: 0,
                    directory: true,
                }
            } else {
                // The mutable extent EOF supersedes the committed size;
                // index content is the base, namespace stat the fallback.
                let extent_eof = namespace_inode.and_then(|inode| {
                    let _load = trace::Scope::new(trace::Slot::LookupMapLoad);
                    engine
                        .db
                        .as_ref()
                        .and_then(|db| db.extent_head(volume, inode).ok().flatten())
                });
                let size = extent_eof
                    .map(|(_, eof)| eof)
                    .or_else(|| {
                        node.and_then(|node| match node {
                            NodeIndex::File(ordinal) => index
                                .file_by_index(ordinal)
                                .ok()
                                .map(|file| file.logical_size()),
                            NodeIndex::Directory(_) => None,
                        })
                    })
                    .or(namespace_size)
                    .unwrap_or(0);
                Entry {
                    index: stable_index,
                    size,
                    directory: false,
                }
            };
            let namespace_times = namespace_inode
                .and_then(|inode| {
                    engine
                        .db
                        .as_ref()
                        .and_then(|db| db.namespace_stat(volume, inode).ok().flatten())
                })
                .map(|stat| (stat.created_ns, stat.modified_ns));
            unsafe {
                ptr::write(
                    output,
                    Box::into_raw(Box::new(MirageFileHandle {
                        entry,
                        index: Some(Arc::clone(index)),
                        node,
                        object_root: engine.object_root.clone(),
                        encryption: engine.encryption.clone(),
                        readers: Arc::clone(&engine.readers),
                        pages: Arc::clone(&engine.pages),
                        origin_flights: engine.origin_flights.clone(),
                        origin_decodes: Arc::clone(&engine.origin_decodes),
                        resident: engine.resident.clone(),
                        shard: engine.shard.clone(),
                        coalesced: Arc::clone(&engine.coalesced),
                        violations: engine.violations.clone(),
                        coordinator: engine.coordinator.clone(),
                        provider: engine.provider.clone(),
                        prefetch: engine
                            .managed_provider
                            .as_ref()
                            .map(|provider| provider.prefetch()),
                        publisher: engine.publisher.clone(),
                        remote_payloads: engine.remote_payloads.clone(),
                        disk_floor: engine.disk_floor.clone(),
                        floor_free_cache: engine.floor_free_cache.clone(),
                        pins: Arc::clone(&engine.pins),
                        prefetch_state: Arc::new(handles::PrefetchState {
                            hashes: std::sync::OnceLock::new(),
                            last: AtomicI64::new(-1),
                        }),
                        logical_path: Default::default(),
                        caller_image: Default::default(),
                        inode: namespace_inode,
                        db: engine.db.clone(),
                        handles: Arc::clone(&engine.handles),
                        extents: Arc::clone(&engine.extents),
                        payload_files: Arc::clone(&engine.payload_files),
                        journal_dir_ready: Arc::clone(&engine.journal_dir_ready),
                        state_root: engine.state_root.clone(),
                        journal_dir: engine.journal_dir.clone(),
                        desired_access: mirage_engine::handles::DesiredAccess {
                            read: true,
                            write: true,
                            delete: true,
                        },
                        share_access: mirage_engine::handles::ShareAccess::ALL,
                        managed: engine.managed,
                        dirty: engine.dirty.clone(),
                        segments: engine.segments.clone(),
                        created_ns: namespace_times.map(|(created, _)| created).unwrap_or(0),
                        modified_ns: AtomicI64::new(
                            namespace_times.map(|(_, modified)| modified).unwrap_or(0),
                        ),
                        explicit_mtime: AtomicBool::new(false),
                        wrote: AtomicBool::new(false),
                        group_durability: engine.group_durability,
                    })),
                )
            };
            return MirageStatus::Ok;
        }
        let Some(entry) = engine.entries.get(&key) else {
            return MirageStatus::NotFound;
        };
        unsafe {
            ptr::write(
                output,
                Box::into_raw(Box::new(MirageFileHandle {
                    entry: entry.clone(),
                    index: None,
                    node: None,
                    object_root: None,
                    encryption: None,
                    readers: Arc::clone(&engine.readers),
                    pages: Arc::clone(&engine.pages),
                    origin_flights: engine.origin_flights.clone(),
                    origin_decodes: Arc::clone(&engine.origin_decodes),
                    resident: engine.resident.clone(),
                    shard: engine.shard.clone(),
                    coalesced: Arc::clone(&engine.coalesced),
                    violations: engine.violations.clone(),
                    coordinator: engine.coordinator.clone(),
                    provider: engine.provider.clone(),
                    prefetch: engine
                        .managed_provider
                        .as_ref()
                        .map(|provider| provider.prefetch()),
                    publisher: engine.publisher.clone(),
                    remote_payloads: engine.remote_payloads.clone(),
                    disk_floor: engine.disk_floor.clone(),
                    floor_free_cache: engine.floor_free_cache.clone(),
                    pins: Arc::clone(&engine.pins),
                    prefetch_state: Arc::new(handles::PrefetchState {
                        hashes: std::sync::OnceLock::new(),
                        last: AtomicI64::new(-1),
                    }),
                    logical_path: Default::default(),
                    caller_image: Default::default(),
                    inode: None,
                    db: engine.db.clone(),
                    handles: Arc::clone(&engine.handles),
                    extents: Arc::clone(&engine.extents),
                    payload_files: Arc::clone(&engine.payload_files),
                    journal_dir_ready: Arc::clone(&engine.journal_dir_ready),
                    state_root: engine.state_root.clone(),
                    journal_dir: engine.journal_dir.clone(),
                    desired_access: mirage_engine::handles::DesiredAccess {
                        read: true,
                        write: true,
                        delete: true,
                    },
                    share_access: mirage_engine::handles::ShareAccess::ALL,
                    managed: engine.managed,
                    dirty: engine.dirty.clone(),
                    segments: engine.segments.clone(),
                    created_ns: 0,
                    modified_ns: AtomicI64::new(0),
                    explicit_mtime: AtomicBool::new(false),
                    wrote: AtomicBool::new(false),
                    group_durability: engine.group_durability,
                })),
            )
        };
        MirageStatus::Ok
    })
}

/// Derives a stable 64-bit index number from an inode for namespace-only
/// entries (the inode is the durable identity; the index has no ordinal for
/// locally created nodes).
pub(super) fn inode_stable_index(inode: InodeId) -> u64 {
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&inode.as_bytes()[..8]);
    u64::from_le_bytes(bytes) | (1 << 63)
}

/// Read exact immutable bytes from a local pack-backed file handle.
///
/// # Safety
/// `handle` must be live, and `output` must be writable for `output_len` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mirage_read(
    handle: *const MirageFileHandle,
    offset: u64,
    output: *mut u8,
    output_len: usize,
    transferred: *mut usize,
) -> MirageStatus {
    unsafe {
        read_impl(
            handle,
            offset,
            output,
            output_len,
            transferred,
            true,
            0,
            false,
        )
    }
}

/// Same as [`mirage_read`] but records the calling process id in seal-violation
/// records so violations can be attributed to the process that issued the read.
///
/// # Safety
/// Same contract as [`mirage_read`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mirage_read_ex(
    handle: *const MirageFileHandle,
    offset: u64,
    output: *mut u8,
    output_len: usize,
    transferred: *mut usize,
    caller_pid: u32,
) -> MirageStatus {
    unsafe {
        read_impl(
            handle,
            offset,
            output,
            output_len,
            transferred,
            true,
            caller_pid,
            false,
        )
    }
}

/// Speculative read issued by the host's own read-ahead rather than by an application.
///
/// Identical to [`mirage_read`] except that a non-resident page is not recorded as a seal
/// violation: only application-initiated reads count against ADR 0002's zero-violation gate.
///
/// # Safety
/// Same contract as [`mirage_read`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mirage_read_speculative(
    handle: *const MirageFileHandle,
    offset: u64,
    output: *mut u8,
    output_len: usize,
    transferred: *mut usize,
) -> MirageStatus {
    unsafe {
        read_impl(
            handle,
            offset,
            output,
            output_len,
            transferred,
            false,
            0,
            false,
        )
    }
}

/// Best-effort `a/b/file` path for a mount-index file, resolved only on the
/// violation path so a record can be attributed to the file that was actually
/// opened rather than just its ordinal.
fn file_path(index: &MountIndex, file: FileView<'_>) -> String {
    let mut parts = vec![file.name().unwrap_or("?").to_owned()];
    let mut dir = file.parent_index();
    for _ in 0..64 {
        let Ok(view) = index.directory_by_index(dir) else {
            break;
        };
        if let Ok(name) = view.name()
            && !name.is_empty()
        {
            parts.push(name.to_owned());
        }
        let parent = view.parent_index();
        if parent == dir {
            break;
        }
        dir = parent;
    }
    parts.reverse();
    parts.join("/")
}

/// Sequential readahead: when a provider fetch serves the page right after
/// the last served miss, speculative fetches for the file's next four pages
/// queue behind it. Best-effort only — never blocks or fails the read.
/// Files at or below this size are prefetched wholesale on their first
/// provider miss: the fetch pool's speculative queue pulls every non-resident
/// page in the background (single-flight dedupes with demand reads, resident
/// pages no-op), so a cold small file needs one miss instead of one per page.
/// Larger files keep windowed readahead only.
const WHOLE_FILE_PREFETCH_BYTES: u64 = 64 * 1024 * 1024;

fn readahead_prefetch(
    handle: &MirageFileHandle,
    index: &MountIndex,
    file: mirage_index::FileView<'_>,
    hash: PageHash,
) {
    let _ = index;
    let Some(prefetch) = &handle.prefetch else {
        return;
    };
    let hashes = handle
        .prefetch_state
        .hashes
        .get_or_init(|| file_page_hashes(file));
    let Some(position) = hashes.iter().position(|candidate| *candidate == hash) else {
        return;
    };
    let last = handle
        .prefetch_state
        .last
        .swap(position as i64, Ordering::Relaxed);
    // First miss on a small file: schedule the whole file. Resident pages
    // short-circuit inside the provider fetch; the current page is already
    // in flight and shares the flight map.
    if last < 0 && file.logical_size() <= WHOLE_FILE_PREFETCH_BYTES {
        for next in &hashes[position + 1..] {
            prefetch(*next);
        }
        for previous in &hashes[..position] {
            prefetch(*previous);
        }
        return;
    }
    if last >= 0 && last + 1 != position as i64 {
        return;
    }
    let end = (position + 5).min(hashes.len());
    for next in &hashes[position + 1..end] {
        prefetch(*next);
    }
}

/// A file's committed page hashes in logical order (consecutive duplicates
/// collapsed — deduplicated pages still resolve to one position).
fn file_page_hashes(file: mirage_index::FileView<'_>) -> Vec<PageHash> {
    let mut hashes: Vec<PageHash> = Vec::new();
    for extent in 0..file.extent_count() {
        let Ok(extent) = file.extent(extent) else {
            break;
        };
        for page in 0..extent.page_count() {
            let Ok(page) = extent.page(page) else {
                break;
            };
            let hash = page.plaintext_hash();
            if hashes.last() != Some(&hash) {
                hashes.push(hash);
            }
        }
    }
    hashes
}

#[allow(clippy::too_many_arguments)]
unsafe fn read_impl(
    handle: *const MirageFileHandle,
    offset: u64,
    output: *mut u8,
    output_len: usize,
    transferred: *mut usize,
    record_violations: bool,
    caller_pid: u32,
    extent_handled: bool,
) -> MirageStatus {
    contained(|| {
        if handle.is_null() || transferred.is_null() || (output.is_null() && output_len != 0) {
            return MirageStatus::InvalidArgument;
        }
        unsafe { ptr::write(transferred, 0) };
        if output_len == 0 {
            return MirageStatus::Ok;
        }
        let handle = unsafe { &*handle };
        if !extent_handled && let Some(inode) = handle.inode {
            // A file with durable extent history must resolve it — failure to
            // load the map fails closed rather than falling back to the
            // committed base and serving bytes the mutation replaced. For a
            // namespace-only file (created on a managed volume) the extents
            // are the only content: there is no index node at all.
            // Only the slice plan is computed under the engine-wide `extents`
            // lock; all payload/index I/O below runs after the guard drops so
            // one slow read (an AV scan on payload open, a cold restage, a
            // remote fetch) cannot stall every other file on the drive.
            let planned = {
                let maps = match extent_map_for(
                    handle,
                    inode,
                    trace::Slot::ReadLockWait,
                    trace::Slot::ReadMapLoadDb,
                ) {
                    Ok(maps) => maps,
                    Err(status) => return status,
                };
                match maps.get(&inode) {
                    Some(map) if !map.is_plain_base() || handle.node.is_none() => {
                        let _plan = trace::Scope::new(trace::Slot::ReadPlan);
                        match map.read(offset, output_len as u64) {
                            Ok(slices) => Some(slices),
                            Err(_) => return MirageStatus::IntegrityFailure,
                        }
                    }
                    _ => None,
                }
            };
            if let Some(slices) = planned {
                return unsafe {
                    read_via_extents(
                        handle,
                        slices,
                        offset,
                        output,
                        transferred,
                        record_violations,
                        caller_pid,
                    )
                };
            }
        }
        let _index_path = trace::Scope::new(trace::Slot::ReadIndexPath);
        let (Some(index), Some(NodeIndex::File(file_ordinal))) = (&handle.index, handle.node)
        else {
            return MirageStatus::InvalidArgument;
        };
        let Ok(file) = index.file_by_index(file_ordinal) else {
            return MirageStatus::IntegrityFailure;
        };
        let Ok(spans) = mirage_index::resolve_range(file, offset, output_len) else {
            return MirageStatus::IntegrityFailure;
        };
        let output = unsafe { std::slice::from_raw_parts_mut(output, output_len) };
        // Pages read under a mounted coordinator are leased for the duration
        // of this call so live eviction can never take a slot mid-read.
        let mut reader_leases = Vec::new();
        let mut span_index = 0;
        while span_index < spans.len() {
            let span = &spans[span_index];
            let Ok(page) = index.page_by_ordinal(span.page_ordinal.as_u32()) else {
                return MirageStatus::IntegrityFailure;
            };
            let hash = page.plaintext_hash();
            if let Some(coordinator) = &handle.coordinator
                && coordinator.state() == mirage_engine::volume::VolumeState::Mounted
            {
                match coordinator.begin_read(hash) {
                    Ok(lease) => reader_leases.push(lease),
                    Err(_) => return MirageStatus::Conflict,
                }
            }
            if let Some(resident) = &handle.resident {
                match resident.acquire(hash) {
                    Ok(Some(first_guard)) => {
                        // Extend a run of spans that are all resident, adjacent
                        // in the arena, and contiguous in both the output and
                        // page addressing; serve the run with one arena read.
                        let mut guards = vec![first_guard];
                        let mut run_len = 1;
                        while span_index + run_len < spans.len() {
                            let previous = &spans[span_index + run_len - 1];
                            let next = &spans[span_index + run_len];
                            if next.page_offset != 0
                                || next.dst_offset != previous.dst_offset + previous.len
                            {
                                break;
                            }
                            let previous_guard = guards.last().expect("run is non-empty");
                            if previous_guard.logical_length()
                                != previous.page_offset + previous.len
                            {
                                break;
                            }
                            let Ok(next_page) = index.page_by_ordinal(next.page_ordinal.as_u32())
                            else {
                                break;
                            };
                            let Ok(Some(next_guard)) = resident.acquire(next_page.plaintext_hash())
                            else {
                                break;
                            };
                            if Some(next_guard.slot_index())
                                != previous_guard.slot_index().checked_add(1)
                            {
                                drop(next_guard);
                                break;
                            }
                            guards.push(next_guard);
                            run_len += 1;
                        }
                        let last = &spans[span_index + run_len - 1];
                        let dst = span.dst_offset as usize;
                        let Some(dst_end) =
                            usize::try_from(u64::from(last.dst_offset) + u64::from(last.len)).ok()
                        else {
                            return MirageStatus::IntegrityFailure;
                        };
                        let Some(window) = output.get_mut(dst..dst_end) else {
                            return MirageStatus::IntegrityFailure;
                        };
                        if run_len == 1 {
                            if guards[0].read_exact(span.page_offset, window).is_err() {
                                return MirageStatus::IoError;
                            }
                        } else {
                            let Some(shard) = &handle.shard else {
                                return MirageStatus::Internal;
                            };
                            if mirage_cache::read_contiguous(
                                shard,
                                &guards,
                                span.page_offset,
                                window,
                            )
                            .is_err()
                            {
                                return MirageStatus::IoError;
                            }
                            handle.coalesced.runs.fetch_add(1, Ordering::Relaxed);
                            handle
                                .coalesced
                                .pages
                                .fetch_add(run_len as u64, Ordering::Relaxed);
                        }
                        span_index += run_len;
                        continue;
                    }
                    _ => {
                        if !record_violations {
                            return MirageStatus::BackendUnavailable;
                        }
                        // Managed path: a non-resident page is fetched through
                        // the coordinator-backed PageProvider — single-flight,
                        // authenticated, hash-verified, then admitted or served
                        // transiently when the arena is full.
                        if let Some(provider) = &handle.provider {
                            match provider(hash) {
                                Ok(mirage_engine::ProviderPage::Placed(guard)) => {
                                    let dst = span.dst_offset as usize;
                                    let start = span.page_offset;
                                    let end = start + span.len;
                                    let Some(window) =
                                        output.get_mut(dst..(dst + span.len as usize))
                                    else {
                                        return MirageStatus::IntegrityFailure;
                                    };
                                    if guard.read_exact(start, window).is_err() {
                                        return MirageStatus::IoError;
                                    }
                                    let _ = end;
                                    readahead_prefetch(handle, index, file, hash);
                                    span_index += 1;
                                    continue;
                                }
                                Ok(mirage_engine::ProviderPage::Transient(page)) => {
                                    let start = span.page_offset as usize;
                                    let Some(end) = start.checked_add(span.len as usize) else {
                                        return MirageStatus::IntegrityFailure;
                                    };
                                    let dst = span.dst_offset as usize;
                                    let Some(dst_end) = dst.checked_add(span.len as usize) else {
                                        return MirageStatus::IntegrityFailure;
                                    };
                                    let Some(source) = page.bytes.get(start..end) else {
                                        return MirageStatus::IntegrityFailure;
                                    };
                                    let Some(destination) = output.get_mut(dst..dst_end) else {
                                        return MirageStatus::IntegrityFailure;
                                    };
                                    destination.copy_from_slice(source);
                                    readahead_prefetch(handle, index, file, hash);
                                    span_index += 1;
                                    continue;
                                }
                                Err(error) => {
                                    // No provider installed falls through to
                                    // the local-origin path for tests and
                                    // legacy restore.
                                    if !matches!(error.kind, MirageErrorKind::ProviderUnavailable) {
                                        return provider_failure_status(&error);
                                    }
                                }
                            }
                        }
                        // Degraded read-through: a non-resident page falls back to
                        // the immutable origin when one is configured, and the
                        // violation record carries the outcome.
                        let result = read_span_from_origin(handle, page, span, output);
                        let outcome = if result.is_ok() { "origin" } else { "failed" };
                        if let Some(log) = &handle.violations {
                            log.record(
                                file_ordinal,
                                span.page_ordinal.as_u32(),
                                offset,
                                output_len,
                                caller_pid,
                                &handle.caller_image(caller_pid),
                                handle.logical_path.get_or_init(|| file_path(index, file)),
                                outcome,
                            );
                        }
                        if let Err(status) = result {
                            return status;
                        }
                        span_index += 1;
                        continue;
                    }
                }
            }
            let served = if let Some(provider) = &handle.provider {
                match provider(hash) {
                    Ok(mirage_engine::ProviderPage::Placed(guard)) => {
                        let dst = span.dst_offset as usize;
                        let Some(window) = output.get_mut(dst..(dst + span.len as usize)) else {
                            return MirageStatus::IntegrityFailure;
                        };
                        if guard.read_exact(span.page_offset, window).is_err() {
                            return MirageStatus::IoError;
                        }
                        true
                    }
                    Ok(mirage_engine::ProviderPage::Transient(page)) => {
                        let start = span.page_offset as usize;
                        let end = start + span.len as usize;
                        let dst = span.dst_offset as usize;
                        let dst_end = dst + span.len as usize;
                        let Some(source) = page.bytes.get(start..end) else {
                            return MirageStatus::IntegrityFailure;
                        };
                        let Some(destination) = output.get_mut(dst..dst_end) else {
                            return MirageStatus::IntegrityFailure;
                        };
                        destination.copy_from_slice(source);
                        true
                    }
                    Err(error) => {
                        if matches!(error.kind, MirageErrorKind::ProviderUnavailable) {
                            false
                        } else {
                            return provider_failure_status(&error);
                        }
                    }
                }
            } else {
                false
            };
            if !served && let Err(status) = read_span_from_origin(handle, page, span, output) {
                return status;
            }
            span_index += 1;
        }
        let bytes = spans.iter().map(|span| span.len as usize).sum();
        unsafe { ptr::write(transferred, bytes) };
        MirageStatus::Ok
    })
}

/// Maps a provider failure to a distinct status: offline/unavailable,
/// cancellation, corruption, and end-of-file never collapse into one code.
fn provider_failure_status(error: &MirageError) -> MirageStatus {
    if std::env::var_os("MIRAGE_DEBUG_PROVIDER").is_some() {
        eprintln!("provider failure: {error:?}");
    }
    match error.kind {
        MirageErrorKind::Cancelled => MirageStatus::Cancelled,
        MirageErrorKind::IntegrityMismatch | MirageErrorKind::ManifestInvalid => {
            MirageStatus::IntegrityFailure
        }
        MirageErrorKind::BackendUnavailable
        | MirageErrorKind::BackendRateLimited
        | MirageErrorKind::BackendUnauthenticated
        | MirageErrorKind::BackendPermissionDenied
        | MirageErrorKind::ProviderUnavailable
        | MirageErrorKind::RemoteObjectMissing => MirageStatus::BackendUnavailable,
        MirageErrorKind::CacheFull => MirageStatus::WouldBlock,
        _ => MirageStatus::IoError,
    }
}

/// Serve one resolved span from the immutable origin pack directory. Shared by
/// local-engine reads and cache-mode degraded read-through on a resident miss.
fn read_span_from_origin(
    handle: &MirageFileHandle,
    page: PageView<'_>,
    span: &ResolvedSpan,
    output: &mut [u8],
) -> Result<(), MirageStatus> {
    let Some(root) = &handle.object_root else {
        return Err(MirageStatus::BackendUnavailable);
    };
    let hash = page.plaintext_hash();
    let location = page
        .remote_location()
        .map_err(|_| MirageStatus::IntegrityFailure)?;
    let object_id = location
        .provider_object_id()
        .map_err(|_| MirageStatus::IntegrityFailure)?;
    if !safe_object_id(object_id) {
        return Err(MirageStatus::IntegrityFailure);
    }
    let cached = match handle.pages.lock() {
        Ok(cache) => cache.get(hash),
        Err(_) => return Err(MirageStatus::Internal),
    };
    let page_bytes = match cached {
        Some(bytes) => bytes,
        None => fetch_origin_page(handle, hash, object_id, root)?,
    };
    let start = span.page_offset as usize;
    let end = start
        .checked_add(span.len as usize)
        .ok_or(MirageStatus::IntegrityFailure)?;
    let dst = span.dst_offset as usize;
    let dst_end = dst
        .checked_add(span.len as usize)
        .ok_or(MirageStatus::IntegrityFailure)?;
    let source = page_bytes
        .bytes
        .get(start..end)
        .ok_or(MirageStatus::IntegrityFailure)?;
    let destination = output
        .get_mut(dst..dst_end)
        .ok_or(MirageStatus::IntegrityFailure)?;
    destination.copy_from_slice(source);
    Ok(())
}

/// Completes an in-flight origin decode with `Internal` if the owner leaves
/// without completing it; `complete_owned` is idempotent so the owner's own
/// completion wins.
struct OriginFlightGuard {
    flights: FlightMap,
    hash: PageHash,
    flight: Arc<PageFlight>,
}

impl Drop for OriginFlightGuard {
    fn drop(&mut self) {
        let _ = self.flights.complete_owned(
            self.hash,
            &self.flight,
            Err(FlightFailure {
                cause: FetchFailureCause::Internal,
                code: "MIRAGE_INTERNAL_INVARIANT".into(),
            }),
        );
    }
}

/// Decodes one page from the immutable origin pack. Reader-open failures are
/// `ProviderUnavailable` (mapped to `IoError`); decode failures are
/// `IntegrityMismatch` (mapped to `IntegrityFailure`) — the same status mapping
/// the decode path used before flights existed.
fn decode_origin_page(
    handle: &MirageFileHandle,
    hash: PageHash,
    object_id: &str,
    root: &Path,
) -> Result<Arc<PlainPage>, MirageError> {
    let mut readers = handle
        .readers
        .lock()
        .map_err(|_| MirageError::internal_invariant("origin reader table lock poisoned"))?;
    if !readers.contains_key(object_id) {
        let opened = match &handle.encryption {
            Some(encryption) => {
                PackReader::open_indexed_encrypted(&root.join(object_id), encryption.clone())
            }
            None => PackReader::open_indexed(&root.join(object_id)),
        };
        let reader = opened
            .map_err(|_| MirageError::provider_unavailable("origin pack could not be opened"))?;
        readers.insert(object_id.to_owned(), Arc::new(reader));
    }
    let reader = Arc::clone(
        readers
            .get(object_id)
            .ok_or_else(|| MirageError::internal_invariant("origin reader missing after open"))?,
    );
    drop(readers);
    let decoded = reader
        .read_page(hash)
        .map_err(|_| MirageError::integrity_mismatch("origin page decode failed"))?;
    Ok(Arc::new(decoded.page))
}

/// Maps a typed origin-decode failure to the status the caller observed before
/// flights existed: decode failures are integrity, infrastructure failures are
/// internal, and everything else is an I/O failure.
fn origin_failure_status(error: &MirageError) -> MirageStatus {
    match error.kind {
        MirageErrorKind::IntegrityMismatch
        | MirageErrorKind::ManifestInvalid
        | MirageErrorKind::UnsupportedLayout => MirageStatus::IntegrityFailure,
        MirageErrorKind::InternalInvariant => MirageStatus::Internal,
        _ => MirageStatus::IoError,
    }
}

/// Resolves a cache-missed page through the shared origin-decode flight: the
/// first caller decodes and publishes, concurrent subscribers share the result.
/// Reads here are synchronous by contract (WinFsp dispatcher threads), so the
/// subscriber path blocks on `wait()`.
fn fetch_origin_page(
    handle: &MirageFileHandle,
    hash: PageHash,
    object_id: &str,
    root: &Path,
) -> Result<Arc<PlainPage>, MirageStatus> {
    let acquired = handle
        .origin_flights
        .acquire(hash, FetchPriority::P0Blocking, u64::MAX, true)
        .map_err(|_| MirageStatus::Internal)?;
    if !acquired.owner {
        let mut waiter = acquired.handle;
        let result = waiter.flight().wait();
        waiter.detach();
        return match result {
            Ok(()) => match handle.pages.lock() {
                Ok(cache) => match cache.get(hash) {
                    Some(bytes) => Ok(bytes),
                    // Already evicted from the bounded cache: decode locally
                    // rather than racing a second flight.
                    None => decode_origin_page(handle, hash, object_id, root)
                        .map_err(|error| origin_failure_status(&error)),
                },
                Err(_) => Err(MirageStatus::Internal),
            },
            Err(failure) => Err(match failure.cause {
                FetchFailureCause::ChecksumMismatch => MirageStatus::IntegrityFailure,
                FetchFailureCause::Internal => MirageStatus::Internal,
                _ => MirageStatus::IoError,
            }),
        };
    }
    let _complete_on_drop = OriginFlightGuard {
        flights: handle.origin_flights.clone(),
        hash,
        flight: Arc::clone(acquired.handle.flight()),
    };
    let result = (|| -> Result<Arc<PlainPage>, MirageError> {
        // A previous owner may have completed and inserted between this
        // thread's cache miss and its ownership of a fresh flight.
        if let Ok(cache) = handle.pages.lock()
            && let Some(bytes) = cache.get(hash)
        {
            return Ok(bytes);
        }
        let bytes = decode_origin_page(handle, hash, object_id, root)?;
        handle.origin_decodes.fetch_add(1, Ordering::Relaxed);
        handle
            .pages
            .lock()
            .map_err(|_| MirageError::internal_invariant("decoded page cache lock poisoned"))?
            .insert(hash, Arc::clone(&bytes));
        Ok(bytes)
    })();
    let flight_result = match &result {
        Ok(_) => Ok(()),
        Err(error) => Err(FlightFailure::from_error(error)),
    };
    let _ = handle
        .origin_flights
        .complete_owned(hash, acquired.handle.flight(), flight_result);
    result.map_err(|error| origin_failure_status(&error))
}

/// Positional read on a shared payload handle — `seek_read`/`read_exact_at`
/// never move the file cursor permanently, but on a synchronous Windows
/// handle they implement offset I/O by moving then restoring it, so every
/// call runs under the entry's `io` lock: a reader racing a writer's
/// positional append (or another reader) must not interleave between the
/// seek and the I/O call.
#[cfg(any(windows, unix))]
fn read_payload_positioned(
    entry: &handles::PayloadEntry,
    buf: &mut [u8],
    offset: u64,
) -> std::io::Result<()> {
    let _io = entry
        .io
        .lock()
        .map_err(|_| std::io::Error::other("payload io lock poisoned"))?;
    #[cfg(windows)]
    {
        use std::os::windows::fs::FileExt;
        let mut filled = 0usize;
        while filled < buf.len() {
            match entry
                .file
                .seek_read(&mut buf[filled..], offset + filled as u64)
            {
                Ok(0) => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::UnexpectedEof,
                        "short payload read",
                    ));
                }
                Ok(n) => filled += n,
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }
    #[cfg(unix)]
    {
        std::os::unix::fs::FileExt::read_exact_at(&entry.file, buf, offset)
    }
}

/// Get-or-open a journal payload in the shared handle cache, then fill `buf`
/// with `buf.len()` bytes at `offset`. The cache lock is held only for the
/// map get/insert — never across the open or the read. Payloads are
/// immutable once the extent map points at them (the append lands before the
/// map update, both under `extents`), and a slice that references a payload
/// deleted after the plan was taken fails the open with NotFound → the
/// caller's restage/remote path.
#[cfg(any(windows, unix))]
fn read_payload_slice(
    handle: &MirageFileHandle,
    path: &Path,
    payload_id: &[u8; 16],
    payload_offset: u64,
    buf: &mut [u8],
) -> std::io::Result<()> {
    let entry = {
        let cached = handle
            .payload_files
            .lock()
            .map_err(|_| std::io::Error::other("payload file cache lock poisoned"))?
            .get(payload_id);
        match cached {
            Some(entry) => {
                let _hit = trace::Scope::new(trace::Slot::ReadPayloadCacheHit);
                return read_payload_positioned(&entry, buf, payload_offset);
            }
            None => {
                let _miss = trace::Scope::new(trace::Slot::ReadPayloadOpenMiss);
                let entry = Arc::new(handles::PayloadEntry {
                    file: std::fs::File::open(path)?,
                    io: std::sync::Mutex::new(()),
                });
                handle
                    .payload_files
                    .lock()
                    .map_err(|_| std::io::Error::other("payload file cache lock poisoned"))?
                    .insert(*payload_id, Arc::clone(&entry));
                entry
            }
        }
    };
    let _read = trace::Scope::new(trace::Slot::ReadSeekRead);
    read_payload_positioned(&entry, buf, payload_offset)
}

/// Portable fallback for targets without a positional-read API: open the
/// payload per slice (the cache still dedupes nothing on those targets —
/// kept simple rather than serializing a seek on a shared cursor).
#[cfg(not(any(windows, unix)))]
fn read_payload_slice(
    handle: &MirageFileHandle,
    path: &Path,
    payload_id: &[u8; 16],
    payload_offset: u64,
    buf: &mut [u8],
) -> std::io::Result<()> {
    use std::io::{Read, Seek, SeekFrom};
    let _ = (handle, payload_id);
    let mut file = std::fs::File::open(path)?;
    file.seek(SeekFrom::Start(payload_offset))?;
    file.read_exact(buf)
}

/// Extent-aware read: dirty slices come from journaled payload files, zero
/// slices memset, and base slices recurse through the immutable page path
/// with the extent branch disabled. An unreadable base slice fails honestly
/// — never a silent zero.
#[allow(clippy::too_many_arguments)]
unsafe fn read_via_extents(
    handle: &MirageFileHandle,
    slices: Vec<mirage_engine::extent_map::ExtentSlice>,
    offset: u64,
    output: *mut u8,
    transferred: *mut usize,
    record_violations: bool,
    caller_pid: u32,
) -> MirageStatus {
    let _total = trace::Scope::new(trace::Slot::ReadViaExtentsTotal);
    let journal_dir = handle
        .state_root
        .as_ref()
        .map(|_| handle_journal_dir(handle));
    // Bytes actually covered by slices; a read reaching past EOF reports
    // short rather than filling the request with fabricated zeros.
    let mut covered_end = offset;
    for slice in slices {
        let dst = (slice.start() - offset) as usize;
        let len = slice.length() as usize;
        covered_end = covered_end.max(slice.start() + slice.length());
        match slice {
            mirage_engine::extent_map::ExtentSlice::Zero { .. } => {
                let destination = unsafe { std::slice::from_raw_parts_mut(output.add(dst), len) };
                destination.fill(0);
            }
            mirage_engine::extent_map::ExtentSlice::Dirty {
                payload_id,
                payload_offset,
                ..
            } => {
                let Some(journal_dir) = &journal_dir else {
                    return MirageStatus::BackendUnavailable;
                };
                let mut hex = String::with_capacity(32);
                for byte in payload_id {
                    hex.push_str(&format!("{byte:02x}"));
                }
                let path = journal_dir.join(format!("{hex}.payload"));
                let destination = unsafe { std::slice::from_raw_parts_mut(output.add(dst), len) };
                match read_payload_slice(handle, &path, &payload_id, payload_offset, destination) {
                    Ok(()) => {}
                    // An evicted payload is remote-only: first try to re-stage
                    // the whole file (budget/floor permitting) so subsequent
                    // reads are local; otherwise fetch just the needed frames.
                    // No publication row means genuinely unavailable — never
                    // zeros.
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        let restaged = {
                            let _restage = trace::Scope::new(trace::Slot::ReadRestage);
                            restage_evicted_payload(handle, journal_dir, &payload_id)
                        };
                        if restaged.is_ok()
                            && read_payload_slice(
                                handle,
                                &path,
                                &payload_id,
                                payload_offset,
                                destination,
                            )
                            .is_ok()
                        {
                            continue;
                        }
                        let (Some(remote), Some(db)) = (&handle.remote_payloads, &handle.db) else {
                            return MirageStatus::IoError;
                        };
                        let Some(volume) = handle
                            .index
                            .as_ref()
                            .map(|index| index.header().repository_id)
                        else {
                            return MirageStatus::IoError;
                        };
                        let remote_read = {
                            let _remote = trace::Scope::new(trace::Slot::ReadRemote);
                            remote.read(db, volume, &payload_id, payload_offset, len)
                        };
                        match remote_read {
                            Ok(Some(bytes)) => {
                                destination.copy_from_slice(&bytes);
                            }
                            Ok(None) => return MirageStatus::IoError,
                            Err(fetch) => {
                                return match fetch.kind {
                                    MirageErrorKind::IntegrityMismatch => {
                                        MirageStatus::IntegrityFailure
                                    }
                                    MirageErrorKind::BackendUnavailable
                                    | MirageErrorKind::BackendUnauthenticated
                                    | MirageErrorKind::BackendPermissionDenied => {
                                        MirageStatus::BackendUnavailable
                                    }
                                    _ => MirageStatus::IoError,
                                };
                            }
                        }
                    }
                    Err(_) => return MirageStatus::IoError,
                }
            }
            mirage_engine::extent_map::ExtentSlice::Base { start, length, .. } => {
                let mut inner_transferred = 0usize;
                let status = unsafe {
                    read_impl(
                        handle,
                        start,
                        output.add(dst),
                        length as usize,
                        &mut inner_transferred,
                        record_violations,
                        caller_pid,
                        true,
                    )
                };
                if status != MirageStatus::Ok {
                    return status;
                }
            }
        }
    }
    unsafe { *transferred = (covered_end - offset) as usize };
    MirageStatus::Ok
}
