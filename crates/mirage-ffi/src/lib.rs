#![allow(unsafe_code)]
pub mod handles;
pub mod namespace;
pub mod status;
use handles::{DecodedPageCache, Entry, ViolationLog};
pub use handles::{MirageEngineHandle, MirageFileHandle};
pub use status::MirageStatus;
use std::collections::HashMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;
use std::ptr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use mirage_cache::{ArenaShard, CacheLayout, ResidentIndex};
use mirage_index::{FileView, MountIndex, NodeIndex, PageView, ResolvedSpan};
use mirage_pack::{PackReadEncryption, PackReader, PlainPage};
use mirage_scheduler::{FetchPriority, FlightFailure, FlightMap, PageFlight};
use mirage_types::{
    ByteCount, FetchFailureCause, InodeId, MirageError, MirageErrorKind, PageHash, RepositoryId,
};

fn contained(operation: impl FnOnce() -> MirageStatus) -> MirageStatus {
    catch_unwind(AssertUnwindSafe(operation)).unwrap_or(MirageStatus::Internal)
}

fn safe_object_id(value: &str) -> bool {
    let path = std::path::Path::new(value);
    if value.is_empty()
        || value.len() > 512
        || value.contains([':', '/', '\\'])
        || value.ends_with(['.', ' '])
        || path.is_absolute()
        || path.components().count() != 1
        || !matches!(
            path.components().next(),
            Some(std::path::Component::Normal(_))
        )
    {
        return false;
    }
    let stem = value
        .split('.')
        .next()
        .unwrap_or_default()
        .to_ascii_uppercase();
    !matches!(
        stem.as_str(),
        "CON"
            | "PRN"
            | "AUX"
            | "NUL"
            | "CLOCK$"
            | "CONIN$"
            | "CONOUT$"
            | "COM1"
            | "COM2"
            | "COM3"
            | "COM4"
            | "COM5"
            | "COM6"
            | "COM7"
            | "COM8"
            | "COM9"
            | "LPT1"
            | "LPT2"
            | "LPT3"
            | "LPT4"
            | "LPT5"
            | "LPT6"
            | "LPT7"
            | "LPT8"
            | "LPT9"
    )
}
#[unsafe(no_mangle)]
/// # Safety
/// `output` must be writable for one engine-handle pointer.
pub unsafe extern "C" fn mirage_engine_create_empty(
    output: *mut *mut MirageEngineHandle,
) -> MirageStatus {
    contained(|| {
        if output.is_null() {
            return MirageStatus::InvalidArgument;
        }
        let handle = Box::into_raw(Box::new(MirageEngineHandle::empty()));
        unsafe { ptr::write(output, handle) };
        MirageStatus::Ok
    })
}
/// # Safety
/// Path and output pointers must be valid for their explicit lengths and writable result.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mirage_engine_create_index(
    path: *const u16,
    path_len: usize,
    output: *mut *mut MirageEngineHandle,
) -> MirageStatus {
    contained(|| {
        if output.is_null() || path.is_null() || path_len == 0 {
            return MirageStatus::InvalidArgument;
        }
        let units = unsafe { std::slice::from_raw_parts(path, path_len) };
        let Ok(path) = String::from_utf16(units) else {
            return MirageStatus::InvalidArgument;
        };
        let Ok(index) = MountIndex::open(std::path::Path::new(&path)) else {
            return MirageStatus::IntegrityFailure;
        };
        let handle = MirageEngineHandle {
            entries: Default::default(),
            index: Some(Arc::new(index)),
            object_root: None,
            encryption: None,
            readers: Arc::new(std::sync::Mutex::new(Default::default())),
            pages: Arc::new(std::sync::Mutex::new(DecodedPageCache::bounded(128))),
            origin_flights: FlightMap::default(),
            origin_decodes: Arc::new(AtomicU64::new(0)),
            resident: None,
            shard: None,
            coalesced: Arc::new(handles::CoalescedReads::default()),
            violations: None,
            trace_lookups: false,
            coordinator: None,
            provider: None,
            db: None,
            handles: Arc::new(mirage_engine::handles::HandleTable::default()),
            extents: Arc::new(std::sync::Mutex::new(Default::default())),
            state_root: None,
        };
        unsafe { ptr::write(output, Box::into_raw(Box::new(handle))) };
        MirageStatus::Ok
    })
}
/// Construct an index-backed engine with an immutable local object directory.
///
/// # Safety
/// Both UTF-16 paths and `output` must be valid for their explicit lengths.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mirage_engine_create_local(
    index_path: *const u16,
    index_path_len: usize,
    object_root: *const u16,
    object_root_len: usize,
    output: *mut *mut MirageEngineHandle,
) -> MirageStatus {
    contained(|| {
        if output.is_null()
            || index_path.is_null()
            || index_path_len == 0
            || object_root.is_null()
            || object_root_len == 0
        {
            return MirageStatus::InvalidArgument;
        }
        let index_units = unsafe { std::slice::from_raw_parts(index_path, index_path_len) };
        let root_units = unsafe { std::slice::from_raw_parts(object_root, object_root_len) };
        let (Ok(index_path), Ok(object_root)) = (
            String::from_utf16(index_units),
            String::from_utf16(root_units),
        ) else {
            return MirageStatus::InvalidArgument;
        };
        let Ok(index) = MountIndex::open(std::path::Path::new(&index_path)) else {
            return MirageStatus::IntegrityFailure;
        };
        let root = std::path::PathBuf::from(object_root);
        if !root.is_dir() {
            return MirageStatus::InvalidArgument;
        }
        let encryption = {
            let key_path = root.join("repository-key.dpapi");
            if key_path.is_file() {
                let Ok(key) = mirage_crypto::repository_key_store::load_repository_key(
                    &key_path,
                    index.header().repository_id,
                ) else {
                    return MirageStatus::IntegrityFailure;
                };
                Some(PackReadEncryption {
                    repository_id: index.header().repository_id,
                    key: Arc::new(key),
                })
            } else {
                None
            }
        };
        let handle = MirageEngineHandle {
            entries: Default::default(),
            index: Some(Arc::new(index)),
            object_root: Some(Arc::new(root)),
            encryption,
            readers: Arc::new(std::sync::Mutex::new(Default::default())),
            pages: Arc::new(std::sync::Mutex::new(DecodedPageCache::bounded(128))),
            origin_flights: FlightMap::default(),
            origin_decodes: Arc::new(AtomicU64::new(0)),
            resident: None,
            shard: None,
            coalesced: Arc::new(handles::CoalescedReads::default()),
            violations: None,
            trace_lookups: false,
            coordinator: None,
            provider: None,
            db: None,
            handles: Arc::new(mirage_engine::handles::HandleTable::default()),
            extents: Arc::new(std::sync::Mutex::new(Default::default())),
            state_root: None,
        };
        unsafe { ptr::write(output, Box::into_raw(Box::new(handle))) };
        MirageStatus::Ok
    })
}
/// Construct an index-backed engine that reads exclusively from the verified
/// local sparse cache. This path never contacts a cloud provider.
///
/// # Safety
/// Both UTF-16 paths and `output` must be valid for their explicit lengths.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mirage_engine_create_cache(
    index_path: *const u16,
    index_path_len: usize,
    state_root: *const u16,
    state_root_len: usize,
    output: *mut *mut MirageEngineHandle,
) -> MirageStatus {
    unsafe {
        create_cache_impl(
            index_path,
            index_path_len,
            state_root,
            state_root_len,
            &[],
            0,
            output,
        )
    }
}
/// Like [`mirage_engine_create_cache`] but with a reachable local origin: on a
/// cache miss the read falls through to the immutable pack directory
/// (`origin_root`) instead of failing, and the violation record carries the
/// outcome. Offline semantics are unchanged when no origin is configured.
///
/// # Safety
/// Same contract as [`mirage_engine_create_cache`]; `origin_root` is a UTF-16
/// path valid for `origin_root_len` units.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mirage_engine_create_cache_with_origin(
    index_path: *const u16,
    index_path_len: usize,
    state_root: *const u16,
    state_root_len: usize,
    origin_root: *const u16,
    origin_root_len: usize,
    output: *mut *mut MirageEngineHandle,
) -> MirageStatus {
    unsafe {
        create_cache_impl(
            index_path,
            index_path_len,
            state_root,
            state_root_len,
            if origin_root.is_null() {
                &[]
            } else {
                std::slice::from_raw_parts(origin_root, origin_root_len)
            },
            origin_root_len,
            output,
        )
    }
}

unsafe fn create_cache_impl(
    index_path: *const u16,
    index_path_len: usize,
    state_root: *const u16,
    state_root_len: usize,
    origin_units: &[u16],
    origin_root_len: usize,
    output: *mut *mut MirageEngineHandle,
) -> MirageStatus {
    contained(|| {
        if output.is_null()
            || index_path.is_null()
            || index_path_len == 0
            || state_root.is_null()
            || state_root_len == 0
        {
            return MirageStatus::InvalidArgument;
        }
        let index_units = unsafe { std::slice::from_raw_parts(index_path, index_path_len) };
        let root_units = unsafe { std::slice::from_raw_parts(state_root, state_root_len) };
        let (Ok(index_path), Ok(state_root)) = (
            String::from_utf16(index_units),
            String::from_utf16(root_units),
        ) else {
            return MirageStatus::InvalidArgument;
        };
        let Ok(index) = MountIndex::open(std::path::Path::new(&index_path)) else {
            return MirageStatus::IntegrityFailure;
        };
        let state_root = std::path::PathBuf::from(state_root);
        let Ok(snapshot) = mirage_db::load_cache_snapshot(&state_root.join("control.db")) else {
            return MirageStatus::IntegrityFailure;
        };
        let [spec] = snapshot.shards.as_slice() else {
            return MirageStatus::IntegrityFailure;
        };
        if spec.shard_id != 0 {
            return MirageStatus::IntegrityFailure;
        }
        let layout = CacheLayout {
            page_size: spec.page_size,
            slot_count: spec.slot_count,
            db_journal_allowance: ByteCount::ZERO,
            filesystem_reserve: ByteCount::ZERO,
        };
        let Ok(shard) = ArenaShard::open_read_unbuffered(
            &state_root.join("cache").join(&spec.relative_path),
            layout,
        ) else {
            return MirageStatus::IntegrityFailure;
        };
        let shard = Arc::new(shard);
        let Ok(resident) =
            ResidentIndex::from_resident_records(snapshot.resident_slots, Arc::clone(&shard))
        else {
            return MirageStatus::IntegrityFailure;
        };
        // Acquire single-owner volume coordination before serving state: a
        // second host on the same state root fails here instead of racing.
        let coordinator = match mirage_engine::volume::VolumeCoordinator::acquire(
            &state_root,
            index.header().repository_id,
        ) {
            Ok(coordinator) => Arc::new(coordinator),
            Err(error) => {
                return match error.kind {
                    MirageErrorKind::RepositoryConflict => MirageStatus::Conflict,
                    _ => MirageStatus::IntegrityFailure,
                };
            }
        };
        let (object_root, encryption) = if origin_root_len != 0 {
            let Ok(origin_root) = String::from_utf16(origin_units) else {
                return MirageStatus::InvalidArgument;
            };
            let root = std::path::PathBuf::from(origin_root);
            if !root.is_dir() {
                return MirageStatus::InvalidArgument;
            }
            let key_path = root.join("repository-key.dpapi");
            let encryption = if key_path.is_file() {
                let Ok(key) = mirage_crypto::repository_key_store::load_repository_key(
                    &key_path,
                    index.header().repository_id,
                ) else {
                    return MirageStatus::IntegrityFailure;
                };
                Some(PackReadEncryption {
                    repository_id: index.header().repository_id,
                    key: Arc::new(key),
                })
            } else {
                None
            };
            (Some(Arc::new(root)), encryption)
        } else {
            (None, None)
        };
        let handle = MirageEngineHandle {
            entries: Default::default(),
            index: Some(Arc::new(index)),
            object_root,
            encryption,
            readers: Arc::new(std::sync::Mutex::new(Default::default())),
            pages: Arc::new(std::sync::Mutex::new(DecodedPageCache::bounded(
                if origin_root_len != 0 { 128 } else { 1 },
            ))),
            origin_flights: FlightMap::default(),
            origin_decodes: Arc::new(AtomicU64::new(0)),
            resident: Some(Arc::new(resident)),
            shard: Some(shard),
            coalesced: Arc::new(handles::CoalescedReads::default()),
            violations: Some(Arc::new(ViolationLog::new(
                state_root.join("seal-violations.log"),
            ))),
            trace_lookups: std::env::var_os("MIRAGE_TRACE_LOOKUPS").as_deref()
                == Some(std::ffi::OsStr::new("1")),
            coordinator: Some(Arc::clone(&coordinator)),
            provider: {
                let coordinator = Arc::clone(&coordinator);
                Some(Arc::new(move |hash| coordinator.provide_page(hash))
                    as Arc<handles::PageProviderHook>)
            },
            db: mirage_db::Database::open(&state_root.join("control.db")).ok(),
            handles: Arc::new(mirage_engine::handles::HandleTable::default()),
            extents: Arc::new(std::sync::Mutex::new(Default::default())),
            state_root: None,
        };
        unsafe { ptr::write(output, Box::into_raw(Box::new(handle))) };
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
        let mark = || -> Result<(), MirageError> {
            if coordinator.state() == VolumeState::Recovering {
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
        let Some(coordinator) = &unsafe { &*engine }.coordinator else {
            return MirageStatus::Ok;
        };
        match coordinator.quiesce(std::time::Duration::from_millis(u64::from(timeout_ms))) {
            Ok(()) => MirageStatus::Ok,
            Err(_) => MirageStatus::WouldBlock,
        }
    })
}
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
            let node = match index.lookup_path(&path) {
                Ok(Some(node)) => node,
                Ok(None) => return MirageStatus::NotFound,
                Err(_) => return MirageStatus::IntegrityFailure,
            };
            if engine.trace_lookups
                && let (NodeIndex::File(ordinal), Some(log)) = (node, &engine.violations)
            {
                log.lookup(u64::from(ordinal), &path);
            }
            let namespace_inode = engine.db.as_ref().and_then(|db| {
                db.namespace_resolve_path(index.header().repository_id, &path)
                    .ok()
                    .flatten()
            });
            let entry = match node {
                NodeIndex::Directory(ordinal) => Entry {
                    index: u64::from(ordinal),
                    size: 0,
                    directory: true,
                },
                NodeIndex::File(ordinal) => match index.file_by_index(ordinal) {
                    Ok(file) => Entry {
                        index: u64::from(ordinal),
                        size: file.logical_size(),
                        directory: false,
                    },
                    Err(_) => return MirageStatus::IntegrityFailure,
                },
            };
            unsafe {
                ptr::write(
                    output,
                    Box::into_raw(Box::new(MirageFileHandle {
                        entry,
                        index: Some(Arc::clone(index)),
                        node: Some(node),
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
                        logical_path: Default::default(),
                        caller_image: Default::default(),
                        inode: namespace_inode,
                        db: engine.db.clone(),
                        handles: Arc::clone(&engine.handles),
                        extents: Arc::clone(&engine.extents),
                        state_root: engine.state_root.clone(),
                        desired_access: mirage_engine::handles::DesiredAccess {
                            read: true,
                            write: false,
                            delete: false,
                        },
                        share_access: mirage_engine::handles::ShareAccess::ALL,
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
                    logical_path: Default::default(),
                    caller_image: Default::default(),
                    inode: None,
                    db: engine.db.clone(),
                    handles: Arc::clone(&engine.handles),
                    extents: Arc::clone(&engine.extents),
                    state_root: engine.state_root.clone(),
                    desired_access: mirage_engine::handles::DesiredAccess {
                        read: true,
                        write: false,
                        delete: false,
                    },
                    share_access: mirage_engine::handles::ShareAccess::ALL,
                })),
            )
        };
        MirageStatus::Ok
    })
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
        let (Some(index), Some(NodeIndex::File(file_ordinal))) = (&handle.index, handle.node)
        else {
            return MirageStatus::InvalidArgument;
        };
        let Ok(file) = index.file_by_index(file_ordinal) else {
            return MirageStatus::IntegrityFailure;
        };
        if !extent_handled && let Some(inode) = handle.inode {
            // A file with no durable extent history takes the immutable page
            // path; seeded maps contain a single unwritten base extent and
            // fall through the same way.
            if let Ok(maps) = extent_map_for(handle, inode)
                && let Some(map) = maps.get(&inode)
                && !map.is_plain_base()
            {
                let status = unsafe {
                    read_via_extents(
                        handle,
                        map,
                        offset,
                        output,
                        output_len,
                        transferred,
                        record_violations,
                        caller_pid,
                    )
                };
                drop(maps);
                return status;
            }
        }
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

#[cfg(test)]
mod tests {
    use super::safe_object_id;

    #[test]
    fn local_object_ids_are_single_safe_windows_components() {
        assert!(safe_object_id("pack-0123456789abcdef.bin"));
        for hostile in [
            "",
            "..",
            "../pack.bin",
            "sub\\pack.bin",
            "pack.bin/",
            "pack.bin\\",
            "C:pack.bin",
            "pack.bin:$DATA",
            "CON",
            "nul.bin",
            "pack.bin.",
            "pack.bin ",
        ] {
            assert!(
                !safe_object_id(hostile),
                "accepted hostile object ID {hostile:?}"
            );
        }
    }
}
#[derive(Clone, Copy)]
#[repr(C)]
pub struct MirageFileInfo {
    pub stable_index: u64,
    pub size: u64,
    pub directory: u8,
    pub reserved: [u8; 7],
}
/// # Safety
/// `handle` and `output` must be live readable/writable pointers.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mirage_file_stat(
    handle: *const MirageFileHandle,
    output: *mut MirageFileInfo,
) -> MirageStatus {
    contained(|| {
        if handle.is_null() || output.is_null() {
            return MirageStatus::InvalidArgument;
        }
        let entry = &unsafe { &*handle }.entry;
        unsafe {
            ptr::write(
                output,
                MirageFileInfo {
                    stable_index: entry.index,
                    size: entry.size,
                    directory: u8::from(entry.directory),
                    reserved: [0; 7],
                },
            )
        };
        MirageStatus::Ok
    })
}

pub type MirageEnumerateCallback =
    unsafe extern "C" fn(*mut core::ffi::c_void, *const u16, usize, MirageFileInfo) -> u8;
/// # Safety
/// Handle, marker, callback, and callback context must remain valid for the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mirage_enumerate(
    handle: *const MirageFileHandle,
    marker: *const u16,
    marker_len: usize,
    limit: usize,
    context: *mut core::ffi::c_void,
    callback: Option<MirageEnumerateCallback>,
) -> MirageStatus {
    contained(|| {
        if handle.is_null() || limit > 4096 || (marker.is_null() && marker_len != 0) {
            return MirageStatus::InvalidArgument;
        }
        let Some(callback) = callback else {
            return MirageStatus::InvalidArgument;
        };
        let handle = unsafe { &*handle };
        let (Some(index), Some(NodeIndex::Directory(ordinal))) = (&handle.index, handle.node)
        else {
            return MirageStatus::InvalidArgument;
        };
        let marker = if marker_len == 0 {
            None
        } else {
            let units = unsafe { std::slice::from_raw_parts(marker, marker_len) };
            match String::from_utf16(units) {
                Ok(value) => Some(value),
                Err(_) => return MirageStatus::InvalidArgument,
            }
        };
        let directory = match index.directory_by_index(ordinal) {
            Ok(value) => value,
            Err(_) => return MirageStatus::IntegrityFailure,
        };
        let children = match index.children_after_marker(directory, marker.as_deref(), limit) {
            Ok(value) => value,
            Err(_) => return MirageStatus::IntegrityFailure,
        };
        for child in children {
            let name = child.name().encode_utf16().collect::<Vec<_>>();
            let info = if child.is_directory() {
                MirageFileInfo {
                    stable_index: u64::from(child.index()),
                    size: 0,
                    directory: 1,
                    reserved: [0; 7],
                }
            } else {
                let file = match index.file_by_index(child.index()) {
                    Ok(value) => value,
                    Err(_) => return MirageStatus::IntegrityFailure,
                };
                MirageFileInfo {
                    stable_index: u64::from(child.index()),
                    size: file.logical_size(),
                    directory: 0,
                    reserved: [0; 7],
                }
            };
            if unsafe { callback(context, name.as_ptr(), name.len(), info) } == 0 {
                break;
            }
        }
        MirageStatus::Ok
    })
}
/// # Safety
/// `handle` must be null or a live file handle returned by this library.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mirage_file_close(handle: *mut MirageFileHandle) -> MirageStatus {
    contained(|| {
        if handle.is_null() {
            return MirageStatus::Ok;
        }
        let file = unsafe { Box::from_raw(handle) };
        // Release share-mode accounting; the last close of a delete-pending
        // tombstone finalizes the durable delete.
        if let (Some(inode), Some(db)) = (file.inode, &file.db)
            && let Ok(true) = file
                .handles
                .close(inode, file.desired_access, file.share_access)
            && let (Some(index), Some(path)) = (&file.index, file.logical_path.get().cloned())
        {
            let volume = index.header().repository_id;
            let normalized = path.replace('\\', "/");
            let trimmed = normalized.trim_matches('/');
            let (parent_path, name) = trimmed
                .rsplit_once('/')
                .map(|(parent, name)| (parent.to_string(), name.to_string()))
                .unwrap_or_else(|| (String::new(), trimmed.to_string()));
            if let Ok(parent) = resolve_parent(db, volume, &parent_path) {
                let now = now_ns_i64();
                if db.namespace_delete(volume, parent, &name, now).is_ok() {
                    let _ = journal_namespace_operation(
                        db,
                        volume,
                        mirage_db::OperationKind::Delete,
                        trimmed.as_bytes().to_vec(),
                        now,
                    );
                }
            }
        }
        MirageStatus::Ok
    })
}

/// Splits a UTF-16 mount-relative path into its parent path (as a namespace
/// `/`-joined string) and final component name.
fn split_utf16_path(path: *const u16, path_len: usize) -> Result<(String, String), MirageStatus> {
    if path.is_null() || path_len == 0 {
        return Err(MirageStatus::InvalidArgument);
    }
    let units = unsafe { std::slice::from_raw_parts(path, path_len) };
    let full = String::from_utf16(units).map_err(|_| MirageStatus::InvalidArgument)?;
    let normalized = full.replace('\\', "/");
    let trimmed = normalized.trim_matches('/');
    let (parent, name) = trimmed
        .rsplit_once('/')
        .map(|(parent, name)| (parent.to_string(), name.to_string()))
        .unwrap_or_else(|| (String::new(), trimmed.to_string()));
    if name.is_empty() {
        return Err(MirageStatus::InvalidArgument);
    }
    Ok((parent, name))
}

/// Mutation preconditions: a mounted coordinator plus the control database.
fn mutation_context(
    engine: &MirageEngineHandle,
) -> Result<(&mirage_db::Database, RepositoryId), MirageStatus> {
    match &engine.coordinator {
        Some(coordinator) if coordinator.state() == mirage_engine::volume::VolumeState::Mounted => {
        }
        _ => return Err(MirageStatus::Conflict),
    }
    let Some(db) = &engine.db else {
        return Err(MirageStatus::BackendUnavailable);
    };
    let Some(index) = &engine.index else {
        return Err(MirageStatus::IntegrityFailure);
    };
    Ok((db, index.header().repository_id))
}

fn journal_namespace_operation(
    db: &mirage_db::Database,
    volume: RepositoryId,
    kind: mirage_db::OperationKind,
    payload: Vec<u8>,
    now_ns: i64,
) -> Result<(), MirageStatus> {
    let mut operation_id = [0u8; 16];
    if getrandom::fill(&mut operation_id).is_err() {
        return Err(MirageStatus::Internal);
    }
    let device_seq = db
        .next_operation_seq(volume)
        .map_err(|_| MirageStatus::IoError)?;
    db.writer()
        .operation_begin(
            mirage_db::OperationRecord {
                operation_id,
                device_seq,
                volume_id: volume,
                base_commit: None,
                kind,
                payload,
                status: mirage_db::OperationStatus::Pending,
                flush_group: None,
                depends_on: None,
                created_ns: now_ns,
            },
            Vec::new(),
        )
        .map_err(|_| MirageStatus::IoError)?;
    db.writer()
        .operation_commit(operation_id)
        .map_err(|_| MirageStatus::IoError)
}

fn now_ns_i64() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos() as i64)
        .unwrap_or(0)
}

fn resolve_parent(
    db: &mirage_db::Database,
    volume: RepositoryId,
    parent_path: &str,
) -> Result<InodeId, MirageStatus> {
    if parent_path.is_empty() {
        db.namespace_root(volume)
            .map_err(|_| MirageStatus::IoError)?
            .ok_or(MirageStatus::NotFound)
    } else {
        db.namespace_resolve_path(volume, parent_path)
            .map_err(|_| MirageStatus::IoError)?
            .ok_or(MirageStatus::NotFound)
    }
}

/// Creates a file or directory in the durable namespace; works fully offline
/// and journals the mutation for later remote publication.
///
/// # Safety
/// `engine` must be live; `path` must point to `path_len` UTF-16 units.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mirage_namespace_create(
    engine: *mut MirageEngineHandle,
    path: *const u16,
    path_len: usize,
    directory: u8,
) -> MirageStatus {
    contained(|| {
        if engine.is_null() {
            return MirageStatus::InvalidArgument;
        }
        let engine = unsafe { &*engine };
        let (parent_path, name) = match split_utf16_path(path, path_len) {
            Ok(parts) => parts,
            Err(status) => return status,
        };
        let (db, volume) = match mutation_context(engine) {
            Ok(context) => context,
            Err(status) => return status,
        };
        let now = now_ns_i64();
        let parent = match resolve_parent(db, volume, &parent_path) {
            Ok(inode) => inode,
            Err(status) => return status,
        };
        let kind = if directory != 0 {
            mirage_db::NamespaceNodeKind::Directory
        } else {
            mirage_db::NamespaceNodeKind::File
        };
        match db.namespace_create(volume, parent, &name, kind, now) {
            Ok(_) => {}
            Err(error) => return mutation_status(&error),
        }
        match journal_namespace_operation(
            db,
            volume,
            if directory != 0 {
                mirage_db::OperationKind::Mkdir
            } else {
                mirage_db::OperationKind::Create
            },
            format!("{parent_path}/{name}").into_bytes(),
            now,
        ) {
            Ok(()) => MirageStatus::Ok,
            Err(status) => status,
        }
    })
}

/// Renames or moves a namespace entry; inode identity survives the move and
/// the mutation works without touching cloud content.
///
/// # Safety
/// `engine` must be live; both paths must be valid UTF-16 slices.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mirage_namespace_rename(
    engine: *mut MirageEngineHandle,
    from_path: *const u16,
    from_len: usize,
    to_path: *const u16,
    to_len: usize,
) -> MirageStatus {
    contained(|| {
        if engine.is_null() {
            return MirageStatus::InvalidArgument;
        }
        let engine = unsafe { &*engine };
        let (from_parent_path, from_name) = match split_utf16_path(from_path, from_len) {
            Ok(parts) => parts,
            Err(status) => return status,
        };
        let (to_parent_path, to_name) = match split_utf16_path(to_path, to_len) {
            Ok(parts) => parts,
            Err(status) => return status,
        };
        let (db, volume) = match mutation_context(engine) {
            Ok(context) => context,
            Err(status) => return status,
        };
        let from_parent = match resolve_parent(db, volume, &from_parent_path) {
            Ok(inode) => inode,
            Err(status) => return status,
        };
        let to_parent = match resolve_parent(db, volume, &to_parent_path) {
            Ok(inode) => inode,
            Err(status) => return status,
        };
        let now = now_ns_i64();
        match db.namespace_rename(volume, from_parent, &from_name, to_parent, &to_name, now) {
            Ok(()) => {}
            Err(error) => return mutation_status(&error),
        }
        match journal_namespace_operation(
            db,
            volume,
            mirage_db::OperationKind::Rename,
            format!("{from_parent_path}/{from_name}\n{to_parent_path}/{to_name}").into_bytes(),
            now,
        ) {
            Ok(()) => MirageStatus::Ok,
            Err(status) => status,
        }
    })
}

/// Deletes a namespace entry; open handles hold the inode as a
/// delete-pending tombstone until the last close.
///
/// # Safety
/// `engine` must be live; `path` must point to `path_len` UTF-16 units.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mirage_namespace_delete(
    engine: *mut MirageEngineHandle,
    path: *const u16,
    path_len: usize,
) -> MirageStatus {
    contained(|| {
        if engine.is_null() {
            return MirageStatus::InvalidArgument;
        }
        let engine = unsafe { &*engine };
        let (parent_path, name) = match split_utf16_path(path, path_len) {
            Ok(parts) => parts,
            Err(status) => return status,
        };
        let (db, volume) = match mutation_context(engine) {
            Ok(context) => context,
            Err(status) => return status,
        };
        let full = if parent_path.is_empty() {
            name.clone()
        } else {
            format!("{parent_path}/{name}")
        };
        let inode = match db.namespace_resolve_path(volume, &full) {
            Ok(Some(inode)) => inode,
            Ok(None) => return MirageStatus::NotFound,
            Err(_) => return MirageStatus::IoError,
        };
        // Open handles hold the entry as a delete-pending tombstone; the row
        // survives until the last close re-issues the delete.
        match engine.handles.request_delete(inode) {
            Ok(mirage_engine::handles::DeleteDisposition::Pending) => {
                return MirageStatus::Ok;
            }
            Ok(mirage_engine::handles::DeleteDisposition::Removed) => {}
            Err(_) => return MirageStatus::AccessDenied,
        }
        let parent = match resolve_parent(db, volume, &parent_path) {
            Ok(inode) => inode,
            Err(status) => return status,
        };
        let now = now_ns_i64();
        match db.namespace_delete(volume, parent, &name, now) {
            Ok(()) => {}
            Err(error) => return mutation_status(&error),
        }
        match journal_namespace_operation(
            db,
            volume,
            mirage_db::OperationKind::Delete,
            full.into_bytes(),
            now,
        ) {
            Ok(()) => MirageStatus::Ok,
            Err(status) => status,
        }
    })
}

fn mutation_status(error: &MirageError) -> MirageStatus {
    match error.kind {
        MirageErrorKind::RepositoryConflict => MirageStatus::Conflict,
        MirageErrorKind::InvalidArgument => MirageStatus::InvalidArgument,
        MirageErrorKind::IntegrityMismatch => MirageStatus::IntegrityFailure,
        _ => MirageStatus::IoError,
    }
}

/// Loads (or replays) the extent map for `inode` from the durable extent
/// store; a file with no extent history seeds a single base extent covering
/// its committed size so partial writes never need the base first.
fn extent_map_for(
    handle: &MirageFileHandle,
    inode: InodeId,
) -> Result<
    std::sync::MutexGuard<'_, HashMap<InodeId, mirage_engine::extent_map::ExtentMap>>,
    MirageStatus,
> {
    let mut maps = handle.extents.lock().map_err(|_| MirageStatus::Internal)?;
    if let std::collections::hash_map::Entry::Vacant(slot) = maps.entry(inode) {
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
                mirage_engine::extent_map::ExtentMap::replay(volume, inode, &extents)
            }
            Ok(None) => {
                let mut map = mirage_engine::extent_map::ExtentMap::default();
                if handle.entry.size > 0 {
                    // Seed one base extent so partial writes preserve the
                    // committed content outside the written range.
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

fn persist_extent_map(
    handle: &MirageFileHandle,
    inode: InodeId,
    map: &mirage_engine::extent_map::ExtentMap,
    now_ns: i64,
) -> Result<(), MirageStatus> {
    let db = handle.db.as_ref().ok_or(MirageStatus::BackendUnavailable)?;
    let volume = handle
        .index
        .as_ref()
        .map(|index| index.header().repository_id)
        .ok_or(MirageStatus::IntegrityFailure)?;
    let extents = map.to_extents(volume, inode, map.version(), now_ns, || {
        let mut id = [0u8; 16];
        let _ = getrandom::fill(&mut id);
        id
    });
    db.writer()
        .extent_replace(volume, inode, map.version(), extents, now_ns)
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
        let Some(state_root) = &handle.state_root else {
            return MirageStatus::BackendUnavailable;
        };
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
        let journal = mirage_engine::journal::LocalJournal::new(db.clone(), volume);
        let journal_dir = state_root.join("journal");
        if std::fs::create_dir_all(&journal_dir).is_err() {
            return MirageStatus::IoError;
        }
        let staged = match journal.stage_payload(&journal_dir, data) {
            Ok(staged) => staged,
            Err(_) => return MirageStatus::IoError,
        };
        let mut maps = match extent_map_for(handle, inode) {
            Ok(maps) => maps,
            Err(status) => return status,
        };
        let map = maps.get_mut(&inode).expect("map just inserted");
        if map.write(offset, staged.bytes, staged.payload_id).is_err() {
            return MirageStatus::InvalidArgument;
        }
        if persist_extent_map(handle, inode, map, now).is_err() {
            return MirageStatus::IoError;
        }
        if journal_namespace_operation(
            db,
            volume,
            mirage_db::OperationKind::Write,
            inode.as_bytes().to_vec(),
            now,
        )
        .is_err()
        {
            return MirageStatus::IoError;
        }
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
        let now = now_ns_i64();
        let mut maps = match extent_map_for(handle, inode) {
            Ok(maps) => maps,
            Err(status) => return status,
        };
        let map = maps.get_mut(&inode).expect("map just inserted");
        if map.truncate(new_size).is_err() {
            return MirageStatus::InvalidArgument;
        }
        if persist_extent_map(handle, inode, map, now).is_err() {
            return MirageStatus::IoError;
        }
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
            Ok(_) => MirageStatus::Ok,
            Err(_) => MirageStatus::IoError,
        }
    })
}

/// Extent-aware read: dirty slices come from journaled payload files, zero
/// slices memset, and base slices recurse through the immutable page path
/// with the extent branch disabled. An unreadable base slice fails honestly
/// — never a silent zero.
#[allow(clippy::too_many_arguments)]
unsafe fn read_via_extents(
    handle: &MirageFileHandle,
    map: &mirage_engine::extent_map::ExtentMap,
    offset: u64,
    output: *mut u8,
    output_len: usize,
    transferred: *mut usize,
    record_violations: bool,
    caller_pid: u32,
) -> MirageStatus {
    let slices = match map.read(offset, output_len as u64) {
        Ok(slices) => slices,
        Err(_) => return MirageStatus::IntegrityFailure,
    };
    let journal_dir = handle.state_root.as_ref().map(|root| root.join("journal"));
    for slice in slices {
        let dst = (slice.start() - offset) as usize;
        let len = slice.length() as usize;
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
                let Ok(file) = std::fs::File::open(&path) else {
                    return MirageStatus::IoError;
                };
                use std::io::{Read, Seek, SeekFrom};
                let mut file = file;
                if file.seek(SeekFrom::Start(payload_offset)).is_err() {
                    return MirageStatus::IoError;
                }
                let destination = unsafe { std::slice::from_raw_parts_mut(output.add(dst), len) };
                if file.read_exact(destination).is_err() {
                    return MirageStatus::IoError;
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
    unsafe { *transferred = output_len };
    MirageStatus::Ok
}
