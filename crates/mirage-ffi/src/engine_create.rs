//! Engine constructors and managed-provider assembly.

use super::mutation::JOURNAL_FILE_ID;
use super::mutation::hex_encode32;
use super::mutation::hex16_decode;
use super::mutation::now_ns_i64;
use super::*;

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
            payload_files: Arc::new(std::sync::Mutex::new(handles::PayloadFileCache::default())),
            journal_dir_ready: Arc::new(AtomicBool::new(false)),
            state_root: None,
            journal_dir: None,
            managed: false,
            dirty: None,
            segments: None,
            group_durability: false,
            managed_provider: None,
            publisher: None,
            remote_payloads: None,
            disk_floor: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
            floor_free_cache: std::sync::Arc::new(std::sync::Mutex::new((
                std::time::Instant::now() - std::time::Duration::from_secs(60),
                0,
            ))),
            pins: std::sync::Arc::new(std::sync::RwLock::new(Default::default())),
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
            payload_files: Arc::new(std::sync::Mutex::new(handles::PayloadFileCache::default())),
            journal_dir_ready: Arc::new(AtomicBool::new(false)),
            state_root: None,
            journal_dir: None,
            managed: false,
            dirty: None,
            segments: None,
            group_durability: false,
            managed_provider: None,
            publisher: None,
            remote_payloads: None,
            disk_floor: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
            floor_free_cache: std::sync::Arc::new(std::sync::Mutex::new((
                std::time::Instant::now() - std::time::Duration::from_secs(60),
                0,
            ))),
            pins: std::sync::Arc::new(std::sync::RwLock::new(Default::default())),
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
            payload_files: Arc::new(std::sync::Mutex::new(handles::PayloadFileCache::default())),
            journal_dir_ready: Arc::new(AtomicBool::new(false)),
            state_root: Some(state_root.clone()),
            journal_dir: None,
            managed: false,
            dirty: None,
            segments: None,
            group_durability: false,
            managed_provider: None,
            publisher: None,
            remote_payloads: None,
            disk_floor: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
            floor_free_cache: std::sync::Arc::new(std::sync::Mutex::new((
                std::time::Instant::now() - std::time::Duration::from_secs(60),
                0,
            ))),
            pins: std::sync::Arc::new(std::sync::RwLock::new(Default::default())),
        };
        unsafe { ptr::write(output, Box::into_raw(Box::new(handle))) };
        MirageStatus::Ok
    })
}

/// Construct the managed writable volume: the durable namespace in
/// `control.db` is authoritative for names, mutations are journaled, and a
/// cache shard is used when one is provisioned — the volume is correct
/// without it (non-resident pages resolve through the provider hook).
/// The namespace is seeded from the committed index tree on first mount;
/// seeding is idempotent across restarts.
///
/// # Safety
/// `index_path`/`state_root` are UTF-16 paths valid for their lengths;
/// `output` must be writable for one engine-handle pointer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mirage_engine_create_managed(
    index_path: *const u16,
    index_path_len: usize,
    state_root: *const u16,
    state_root_len: usize,
    object_root: *const u16,
    object_root_len: usize,
    dirty_budget_bytes: u64,
    output: *mut *mut MirageEngineHandle,
) -> MirageStatus {
    unsafe {
        mirage_engine_create_managed_drive(
            index_path,
            index_path_len,
            state_root,
            state_root_len,
            object_root,
            object_root_len,
            dirty_budget_bytes,
            std::ptr::null(),
            0,
            std::ptr::null(),
            0,
            output,
        )
    }
}

/// Drive-capable managed engine: when `drive_manifest_path` and
/// `repository_key_path` are supplied, a `PageProvider` is prepared for
/// on-demand fetches and installed on the coordinator's provider slot at the
/// first `mirage_engine_set_drive_token` call; before that, non-resident page
/// reads fail with the provider-unavailable status exactly as without it.
///
/// # Safety
/// Same pointer contract as `mirage_engine_create_managed`; the drive paths
/// are UTF-16 paths valid for their lengths (or null with length 0).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mirage_engine_create_managed_drive(
    index_path: *const u16,
    index_path_len: usize,
    state_root: *const u16,
    state_root_len: usize,
    object_root: *const u16,
    object_root_len: usize,
    dirty_budget_bytes: u64,
    drive_manifest_path: *const u16,
    drive_manifest_len: usize,
    repository_key_path: *const u16,
    repository_key_len: usize,
    output: *mut *mut MirageEngineHandle,
) -> MirageStatus {
    unsafe {
        mirage_engine_create_managed_drive_at(
            index_path,
            index_path_len,
            state_root,
            state_root_len,
            object_root,
            object_root_len,
            dirty_budget_bytes,
            drive_manifest_path,
            drive_manifest_len,
            repository_key_path,
            repository_key_len,
            std::ptr::null(),
            0,
            output,
        )
    }
}

/// Same as `mirage_engine_create_managed_drive` with an explicit journal
/// payload directory (`journal_root`, UTF-16) — the user's chosen cache disk.
/// Null/empty keeps the default `<state_root>/journal`.
///
/// # Safety
/// Same pointer contract as `mirage_engine_create_managed_drive`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mirage_engine_create_managed_drive_at(
    index_path: *const u16,
    index_path_len: usize,
    state_root: *const u16,
    state_root_len: usize,
    object_root: *const u16,
    object_root_len: usize,
    dirty_budget_bytes: u64,
    drive_manifest_path: *const u16,
    drive_manifest_len: usize,
    repository_key_path: *const u16,
    repository_key_len: usize,
    journal_root: *const u16,
    journal_root_len: usize,
    output: *mut *mut MirageEngineHandle,
) -> MirageStatus {
    contained(|| {
        if journal_root.is_null() && journal_root_len != 0 {
            return MirageStatus::InvalidArgument;
        }
        let journal_root = if journal_root_len == 0 {
            None
        } else {
            let units = unsafe { std::slice::from_raw_parts(journal_root, journal_root_len) };
            match String::from_utf16(units) {
                Ok(path) if !path.is_empty() => Some(PathBuf::from(path)),
                _ => return MirageStatus::InvalidArgument,
            }
        };
        let drive = if drive_manifest_len != 0 {
            Some(ManagedProviderSpec::Drive)
        } else {
            None
        };
        create_managed_impl(
            index_path,
            index_path_len,
            state_root,
            state_root_len,
            object_root,
            object_root_len,
            dirty_budget_bytes,
            drive_manifest_path,
            drive_manifest_len,
            repository_key_path,
            repository_key_len,
            std::ptr::null(),
            0,
            drive,
            journal_root,
            output,
        )
    })
}

/// Managed engine whose on-demand provider reads `<root>/<provider_object_id>`
/// — the pack-mirror layout `repo import` writes. Diagnostic/test seam:
/// `mirage_engine_set_drive_token` still gates provider installation.
///
/// # Safety
/// Same pointer contract as `mirage_engine_create_managed`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mirage_engine_create_managed_local_provider(
    index_path: *const u16,
    index_path_len: usize,
    state_root: *const u16,
    state_root_len: usize,
    object_root: *const u16,
    object_root_len: usize,
    dirty_budget_bytes: u64,
    provider_root: *const u16,
    provider_root_len: usize,
    output: *mut *mut MirageEngineHandle,
) -> MirageStatus {
    contained(|| {
        if provider_root.is_null() || provider_root_len == 0 {
            return MirageStatus::InvalidArgument;
        }
        create_managed_impl(
            index_path,
            index_path_len,
            state_root,
            state_root_len,
            object_root,
            object_root_len,
            dirty_budget_bytes,
            std::ptr::null(),
            0,
            std::ptr::null(),
            0,
            provider_root,
            provider_root_len,
            Some(ManagedProviderSpec::Local),
            None,
            output,
        )
    })
}

/// Which on-demand provider a managed engine should prepare.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ManagedProviderSpec {
    Drive,
    Local,
}

#[allow(clippy::too_many_arguments)]
fn create_managed_impl(
    index_path: *const u16,
    index_path_len: usize,
    state_root: *const u16,
    state_root_len: usize,
    object_root: *const u16,
    object_root_len: usize,
    dirty_budget_bytes: u64,
    drive_manifest_path: *const u16,
    drive_manifest_len: usize,
    repository_key_path: *const u16,
    repository_key_len: usize,
    provider_root: *const u16,
    provider_root_len: usize,
    provider_spec: Option<ManagedProviderSpec>,
    journal_root: Option<PathBuf>,
    output: *mut *mut MirageEngineHandle,
) -> MirageStatus {
    let debug_stage = |stage: &str, status: MirageStatus| -> MirageStatus {
        eprintln!("managed create failed at {stage}: {status:?}");
        status
    };
    contained(|| {
        if output.is_null()
            || index_path.is_null()
            || index_path_len == 0
            || state_root.is_null()
            || state_root_len == 0
            || (object_root.is_null() && object_root_len != 0)
            || (drive_manifest_path.is_null() && drive_manifest_len != 0)
            || (repository_key_path.is_null() && repository_key_len != 0)
            || (drive_manifest_len == 0) != (repository_key_len == 0)
            || (provider_root.is_null() && provider_root_len != 0)
            || (provider_spec == Some(ManagedProviderSpec::Local)) != (provider_root_len != 0)
        {
            return debug_stage("{", MirageStatus::InvalidArgument);
        }
        let utf16_path = |pointer: *const u16, len: usize| -> Result<PathBuf, MirageStatus> {
            let units = unsafe { std::slice::from_raw_parts(pointer, len) };
            String::from_utf16(units)
                .map(PathBuf::from)
                .map_err(|_| MirageStatus::InvalidArgument)
        };
        let drive_config = if drive_manifest_len != 0 {
            let (Ok(manifest_path), Ok(key_path)) = (
                utf16_path(drive_manifest_path, drive_manifest_len),
                utf16_path(repository_key_path, repository_key_len),
            ) else {
                return debug_stage(") else {", MirageStatus::InvalidArgument);
            };
            if !manifest_path.is_file() || !key_path.is_file() {
                return debug_stage(
                    "if !manifest_path.is_file() || !key_path.is_file() {",
                    MirageStatus::InvalidArgument,
                );
            }
            Some((manifest_path, key_path))
        } else {
            None
        };
        let local_provider_root = if provider_root_len != 0 {
            let Ok(root) = utf16_path(provider_root, provider_root_len) else {
                return debug_stage(
                    "let Ok(root) = utf16_path(provider_root, provider_root_len) ",
                    MirageStatus::InvalidArgument,
                );
            };
            if !root.is_dir() {
                return debug_stage("if !root.is_dir() {", MirageStatus::InvalidArgument);
            }
            Some(root)
        } else {
            None
        };
        let index_units = unsafe { std::slice::from_raw_parts(index_path, index_path_len) };
        let root_units = unsafe { std::slice::from_raw_parts(state_root, state_root_len) };
        let (Ok(index_path), Ok(state_root)) = (
            String::from_utf16(index_units),
            String::from_utf16(root_units),
        ) else {
            return debug_stage(") else {", MirageStatus::InvalidArgument);
        };
        let Ok(index) = MountIndex::open(std::path::Path::new(&index_path)) else {
            return debug_stage(
                "let Ok(index) = MountIndex::open(std::path::Path::new(&index",
                MirageStatus::IntegrityFailure,
            );
        };
        let state_root = std::path::PathBuf::from(state_root);
        // Journal payloads live on the user's chosen cache disk; the state
        // root is only the default so existing volumes keep their location.
        let journal_dir = journal_root.unwrap_or_else(|| state_root.join("journal"));
        // The local pack directory that mirrors committed remote content;
        // absent it, committed-page reads resolve through the provider hook.
        let (object_root, encryption) = if object_root_len != 0 {
            let object_units = unsafe { std::slice::from_raw_parts(object_root, object_root_len) };
            let Ok(root) = String::from_utf16(object_units) else {
                return debug_stage(
                    "let Ok(root) = String::from_utf16(object_units) else {",
                    MirageStatus::InvalidArgument,
                );
            };
            let root = std::path::PathBuf::from(root);
            if !root.is_dir() {
                return debug_stage("if !root.is_dir() {", MirageStatus::InvalidArgument);
            }
            let key_path = root.join("repository-key.dpapi");
            let encryption = if key_path.is_file() {
                let Ok(key) = mirage_crypto::repository_key_store::load_repository_key(
                    &key_path,
                    index.header().repository_id,
                ) else {
                    return debug_stage(") else {", MirageStatus::IntegrityFailure);
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
        if std::fs::create_dir_all(&state_root).is_err() {
            return debug_stage(
                "if std::fs::create_dir_all(&state_root).is_err() {",
                MirageStatus::IoError,
            );
        }
        let volume = index.header().repository_id;
        // The OS lock comes before any mutable-state access: a second host
        // on the same state root fails here rather than racing the DB.
        let coordinator =
            match mirage_engine::volume::VolumeCoordinator::acquire(&state_root, volume) {
                Ok(coordinator) => Arc::new(coordinator),
                Err(error) => {
                    eprintln!("managed create coordinator acquire: {error:?}");
                    return match error.kind {
                        MirageErrorKind::RepositoryConflict => MirageStatus::Conflict,
                        _ => MirageStatus::IntegrityFailure,
                    };
                }
            };
        // Managed writable engines use group commit (synchronous=NORMAL +
        // 250 ms writer barrier) unless MIRAGE_DURABILITY=strict restores
        // per-commit FULL fsyncs.
        let group_durability = std::env::var("MIRAGE_DURABILITY")
            .map(|value| value != "strict")
            .unwrap_or(true);
        let durability = if group_durability {
            mirage_db::Durability::Group
        } else {
            mirage_db::Durability::Strict
        };
        let Ok(db) =
            mirage_db::Database::open_with_durability(&state_root.join("control.db"), durability)
        else {
            return debug_stage(
                "let Ok(db) = mirage_db::Database::open(&state_root.join('con",
                MirageStatus::IntegrityFailure,
            );
        };
        // Seed the durable namespace from the committed index tree exactly
        // once: the seed and its durable marker commit in one transaction, so
        // a crash mid-seed leaves no marker and reseeds cleanly next start.
        // Once the marker exists the namespace — not the index — is
        // authoritative for names, so renames and deletes survive restart and
        // a different index hash must not reseed.
        match db.writer().namespace_seed_managed(
            volume,
            seed_nodes_from_index(&index),
            index.header().index_hash,
            now_ns_i64(),
        ) {
            Ok(None) => {}
            Ok(Some(seeded_hash)) => {
                if seeded_hash != index.header().index_hash {
                    eprintln!(
                        "managed namespace seeded from {}, index now {}; generation change not applied",
                        hex_encode32(&seeded_hash),
                        hex_encode32(&index.header().index_hash)
                    );
                }
            }
            Err(error) => {
                eprintln!("managed create failed at seed_namespace: {error}");
                return MirageStatus::IoError;
            }
        }
        // Dirty-payload ledger: one variable-length physical file tracks
        // every staged journal payload so managed writes stay bounded by
        // the configured budget across restarts. Registration is idempotent
        // — the extents referencing it forbid a replace.
        let now = now_ns_i64();
        if db.writer().physical_reap_reservations(now).is_err() {
            return debug_stage(
                "if db.writer().physical_reap_reservations(now).is_err() {",
                MirageStatus::IoError,
            );
        }
        let Ok((physical_files, physical_extents, _)) = db.load_physical_state() else {
            return debug_stage(
                "let Ok((physical_files, physical_extents, _)) = db.load_phys",
                MirageStatus::IoError,
            );
        };
        if !physical_files
            .iter()
            .any(|file| file.file_id == JOURNAL_FILE_ID)
            && db
                .writer()
                .physical_register_file(mirage_db::PhysicalFileRecord {
                    file_id: JOURNAL_FILE_ID,
                    path: "journal".into(),
                    zone: 1,
                    extent_bytes: 0,
                    extent_count: 0,
                    created_ns: now,
                })
                .is_err()
        {
            return debug_stage("{", MirageStatus::IoError);
        }
        let mut dirty_used = 0_u64;
        let mut next_slot = 0_i64;
        let mut live_payloads = std::collections::BTreeSet::new();
        for extent in &physical_extents {
            if extent.file_id != JOURNAL_FILE_ID {
                continue;
            }
            next_slot = next_slot.max(extent.slot_index + 1);
            if matches!(
                extent.state,
                mirage_db::PhysicalExtentState::Reserved | mirage_db::PhysicalExtentState::Alive
            ) {
                dirty_used = dirty_used.saturating_add(extent.length_bytes as u64);
                live_payloads.insert(extent.extent_id);
            }
        }
        // Orphan sweep: a `*.payload` file may only be deleted once the
        // ledger says it is neither a live reservation nor referenced by
        // any remaining extent row.
        if journal_dir.is_dir() {
            let Ok(referenced) = db.referenced_payload_ids(volume) else {
                return debug_stage(
                    "let Ok(referenced) = db.referenced_payload_ids(volume) else ",
                    MirageStatus::IoError,
                );
            };
            if let Ok(entries) = std::fs::read_dir(&journal_dir) {
                for entry in entries.flatten() {
                    let name = entry.file_name();
                    let Some(stem) = name.to_str().and_then(|name| name.strip_suffix(".payload"))
                    else {
                        continue;
                    };
                    let Ok(payload_id) = hex16_decode(stem) else {
                        continue;
                    };
                    if !live_payloads.contains(&payload_id) && !referenced.contains(&payload_id) {
                        let _ = std::fs::remove_file(entry.path());
                    }
                }
            }
        }
        let dirty = Some(Arc::new(handles::DirtyLedger {
            file_id: JOURNAL_FILE_ID,
            budget_bytes: dirty_budget_bytes,
            used: AtomicU64::new(dirty_used),
            next_slot: AtomicI64::new(next_slot),
            mutex: std::sync::Mutex::new(()),
        }));
        // Optional cache shard: a managed volume is correct without resident
        // pages — misses go to the provider hook.
        let (resident, shard, resident_committed, cache_capacity) =
            mirage_db::load_cache_snapshot(&state_root.join("control.db"))
                .ok()
                .and_then(|snapshot| {
                    let [spec] = snapshot.shards.as_slice() else {
                        return None;
                    };
                    let layout = CacheLayout {
                        page_size: spec.page_size,
                        slot_count: spec.slot_count,
                        db_journal_allowance: ByteCount::ZERO,
                        filesystem_reserve: ByteCount::ZERO,
                    };
                    let shard_path = state_root.join("cache").join(&spec.relative_path);
                    // A provider-backed volume admits fetched pages into the
                    // shard, which needs a writable handle — the unbuffered
                    // read handle is read-only.
                    let shard = if provider_spec.is_some() {
                        ArenaShard::open(&shard_path, layout)
                    } else {
                        ArenaShard::open_read_unbuffered(&shard_path, layout)
                    }
                    .ok()?;
                    let committed = snapshot
                        .resident_slots
                        .iter()
                        .map(|slot| u64::from(slot.logical_length))
                        .sum();
                    let capacity = spec
                        .page_size
                        .as_u64()
                        .saturating_mul(u64::from(spec.slot_count));
                    let shard = Arc::new(shard);
                    let resident = ResidentIndex::from_resident_records(
                        snapshot.resident_slots,
                        Arc::clone(&shard),
                    )
                    .ok()?;
                    Some((Arc::new(resident), shard, committed, capacity))
                })
                .map_or(
                    (None, None, 0, 0),
                    |(resident, shard, committed, capacity)| {
                        (Some(resident), Some(shard), committed, capacity)
                    },
                );
        // A Drive-capable provider is prepared from the publication manifest
        // and repository key but stays uninstalled until the first token.
        let managed_provider = match provider_spec {
            Some(ManagedProviderSpec::Drive) => {
                let Some((manifest_path, key_path)) = &drive_config else {
                    return debug_stage(
                        "let Some((manifest_path, key_path)) = &drive_config else {",
                        MirageStatus::InvalidArgument,
                    );
                };
                let (Some(resident), Some(shard)) = (resident.clone(), shard.clone()) else {
                    return debug_stage(
                        "provider cache arena is unavailable; provision or repair the service cache before mounting",
                        MirageStatus::BackendUnavailable,
                    );
                };
                let provider_result = build_managed_drive_provider(
                    &index,
                    volume,
                    manifest_path,
                    key_path,
                    &db,
                    resident,
                    shard,
                    resident_committed,
                    cache_capacity,
                );
                match provider_result {
                    Ok(provider) => Some(Arc::new(provider)),
                    Err(status) => {
                        eprintln!("managed drive provider build failed: {status:?}");
                        return status;
                    }
                }
            }
            Some(ManagedProviderSpec::Local) => {
                let Some(root) = &local_provider_root else {
                    return debug_stage(
                        "let Some(root) = &local_provider_root else {",
                        MirageStatus::InvalidArgument,
                    );
                };
                let (Some(resident), Some(shard)) = (resident.clone(), shard.clone()) else {
                    return debug_stage(
                        "provider cache arena is unavailable; provision or repair the service cache before mounting",
                        MirageStatus::BackendUnavailable,
                    );
                };
                match build_managed_local_provider(&index, volume, root, &db, resident, shard) {
                    Ok(provider) => Some(Arc::new(provider)),
                    Err(status) => return status,
                }
            }
            None => None,
        };
        // The payload publisher idles until the provider is installed (a
        // Drive backend has a token) and then uploads committed payloads.
        let publisher = managed_provider.as_ref().map(|provider| {
            Arc::new(publisher::Publisher::spawn(
                db.clone(),
                volume,
                journal_dir.clone(),
                Arc::clone(provider),
                Arc::clone(&provider.publication_key),
            ))
        });
        // Evicted-payload reads share the provider's backend + content key.
        let remote_payloads = managed_provider.as_ref().map(|provider| {
            Arc::new(publisher::RemotePayloadStore::new(
                provider.backend(),
                Arc::clone(&provider.installed),
                Arc::clone(&provider.publication_key),
                volume,
            ))
        });
        let extents: Arc<std::sync::Mutex<HashMap<InodeId, mirage_engine::extent_map::ExtentMap>>> =
            Arc::new(std::sync::Mutex::new(Default::default()));
        let payload_files: Arc<std::sync::Mutex<handles::PayloadFileCache>> =
            Arc::new(std::sync::Mutex::new(Default::default()));
        let journal_dir_ready = Arc::new(AtomicBool::new(false));
        let segments = dirty.as_ref().map(|dirty| {
            segments::SegmentWriter::new(
                db.clone(),
                volume,
                journal_dir.clone(),
                Arc::clone(dirty),
                Arc::clone(&extents),
                Arc::clone(&payload_files),
                Some(Arc::clone(&coordinator)),
                publisher.clone(),
            )
        });
        let handle = MirageEngineHandle {
            entries: Default::default(),
            index: Some(Arc::new(index)),
            object_root,
            encryption,
            readers: Arc::new(std::sync::Mutex::new(Default::default())),
            pages: Arc::new(std::sync::Mutex::new(DecodedPageCache::bounded(128))),
            origin_flights: FlightMap::default(),
            origin_decodes: Arc::new(AtomicU64::new(0)),
            resident,
            shard,
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
            pins: std::sync::Arc::new(std::sync::RwLock::new(
                db.namespace_pins(volume)
                    .map(|inodes| inodes.into_iter().collect())
                    .unwrap_or_default(),
            )),
            db: Some(db),
            handles: Arc::new(mirage_engine::handles::HandleTable::default()),
            extents,
            payload_files,
            journal_dir_ready,
            state_root: Some(state_root),
            journal_dir: Some(journal_dir),
            managed: true,
            dirty,
            managed_provider,
            publisher,
            remote_payloads,
            segments,
            group_durability,
            disk_floor: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
            floor_free_cache: std::sync::Arc::new(std::sync::Mutex::new((
                std::time::Instant::now() - std::time::Duration::from_secs(60),
                0,
            ))),
        };
        unsafe { ptr::write(output, Box::into_raw(Box::new(handle))) };
        MirageStatus::Ok
    })
}

/// Builds the Drive-capable page provider for a managed engine: pack content
/// hash → Drive object mapping from `drive-manifest.cbor`, the mount index's
/// page locations remapped onto those objects, and the repository content key
/// for frame authentication. The provider is returned uninstalled — the
/// coordinator's provider slot fills at the first `mirage_engine_set_drive_token`.
#[allow(clippy::too_many_arguments)]
fn build_managed_drive_provider(
    index: &MountIndex,
    volume: RepositoryId,
    manifest_path: &Path,
    key_path: &Path,
    db: &mirage_db::Database,
    resident: Arc<ResidentIndex>,
    shard: Arc<ArenaShard>,
    resident_committed: u64,
    cache_capacity: u64,
) -> Result<handles::ManagedProvider, MirageStatus> {
    let bytes = std::fs::read(manifest_path).map_err(|error| {
        eprintln!("drive provider manifest read failed: {error}");
        MirageStatus::IoError
    })?;
    let manifest =
        mirage_manifest::decode_manifest_bounded(&bytes, mirage_manifest::DecodeLimits::default())
            .map_err(|error| {
                if std::env::var_os("MIRAGE_DEBUG_PROVIDER").is_some() {
                    eprintln!("drive provider manifest decode: {error:?}");
                }
                MirageStatus::IntegrityFailure
            })?;
    let mut objects: std::collections::BTreeMap<[u8; 32], mirage_backend::RemoteObjectRef> =
        std::collections::BTreeMap::new();
    for location in &manifest.remote_locations {
        if location.object.backend_id.as_str() != "drive"
            || location.object.kind != mirage_backend::ObjectKind::Pack
        {
            eprintln!(
                "drive provider object rejected: backend={} kind={:?}",
                location.object.backend_id.as_str(),
                location.object.kind
            );
            return Err(MirageStatus::IntegrityFailure);
        }
        objects.insert(
            *location.object.content_hash.as_bytes(),
            location.object.clone(),
        );
    }
    // Equivalent of the service's manifest-vs-manifest check, applied to the
    // mount index: the index orders remote locations by hash, so compare as a
    // set on (content hash, pack offset, encoded length, codec).
    let drive_locations: std::collections::HashSet<([u8; 32], u64, u64, mirage_manifest::Codec)> =
        manifest
            .remote_locations
            .iter()
            .map(|location| {
                (
                    *location.object.content_hash.as_bytes(),
                    location.offset,
                    location.encoded_length.as_u64(),
                    location.codec,
                )
            })
            .collect();
    if drive_locations.len() as u64 != index.remote_location_count() {
        eprintln!(
            "drive provider location count: manifest={} index={}",
            drive_locations.len(),
            index.remote_location_count()
        );
        return Err(MirageStatus::IntegrityFailure);
    }
    for i in 0..index.remote_location_count() {
        let Ok(ordinal) = u32::try_from(i) else {
            return Err(MirageStatus::IntegrityFailure);
        };
        let Ok(record) = index.remote_location_by_index(ordinal) else {
            return Err(MirageStatus::IntegrityFailure);
        };
        if !drive_locations.contains(&(
            record.object_hash(),
            record.pack_offset(),
            record.encoded_length(),
            record.codec(),
        )) {
            eprintln!(
                "drive provider location {i} absent from publication: hash={:?} offset={} length={} codec={:?}",
                record.object_hash(),
                record.pack_offset(),
                record.encoded_length(),
                record.codec(),
            );
            return Err(MirageStatus::IntegrityFailure);
        }
    }
    let locations = mirage_engine::PageLocationMap::build(index)
        .and_then(|map| map.remap_objects(&objects))
        .map_err(|error| {
            eprintln!("drive provider location map: {error:?}");
            MirageStatus::IntegrityFailure
        })?;
    let key = mirage_crypto::repository_key_store::load_repository_key(key_path, volume).map_err(
        |error| {
            eprintln!("drive provider key load: {error:?}");
            MirageStatus::IntegrityFailure
        },
    )?;
    let encryption = PackReadEncryption {
        repository_id: volume,
        key: Arc::new(key),
    };
    let backend = Arc::new(
        mirage_backend_drive::RefreshableDriveBackend::new(
            zeroize::Zeroizing::new(String::new()),
            volume,
        )
        .map_err(|error| {
            eprintln!("drive provider backend build: {error:?}");
            MirageStatus::BackendUnavailable
        })?,
    );
    let hard_bytes = cache_capacity.max(1);
    let ledger = mirage_cache::ReservationLedger::new(
        mirage_cache::BudgetConfig {
            hard_bytes,
            prefetch_soft_bytes: hard_bytes,
            update_safety_reserve: 0,
            dirty_update_bytes: 0,
        },
        resident_committed,
    )
    .map_err(|error| {
        eprintln!("drive provider reservation ledger: {error:?}");
        MirageStatus::IoError
    })?;
    let provider = mirage_engine::PageProvider::new(
        Arc::clone(&backend),
        db.clone(),
        shard,
        resident,
        Arc::new(locations),
        ledger,
        MANAGED_FETCH_MAX_WINDOW,
    )
    .with_encryption(encryption.clone());
    Ok(handles::ManagedProvider::drive(
        backend,
        Arc::new(provider),
        Arc::clone(&encryption.key),
    ))
}

/// Directory-backed provider builder for the test/diagnostic seam: pack
/// locations come straight from the index (local pack ids map onto files
/// under `root`), unencrypted frames only.
fn build_managed_local_provider(
    index: &MountIndex,
    volume: RepositoryId,
    root: &Path,
    db: &mirage_db::Database,
    resident: Arc<ResidentIndex>,
    shard: Arc<ArenaShard>,
) -> Result<handles::ManagedProvider, MirageStatus> {
    let backend = crate::directory_backend::DirectoryObjectBackend::new(root, volume)
        .map_err(|_| MirageStatus::BackendUnavailable)?;
    let locations =
        mirage_engine::PageLocationMap::build(index).map_err(|_| MirageStatus::IntegrityFailure)?;
    let hard_bytes = u64::MAX / 4;
    let ledger = mirage_cache::ReservationLedger::new(
        mirage_cache::BudgetConfig {
            hard_bytes,
            prefetch_soft_bytes: hard_bytes,
            update_safety_reserve: 0,
            dirty_update_bytes: 0,
        },
        0,
    )
    .map_err(|_| MirageStatus::IoError)?;
    let provider = mirage_engine::PageProvider::new(
        Arc::new(backend),
        db.clone(),
        shard,
        resident,
        Arc::new(locations),
        ledger,
        MANAGED_FETCH_MAX_WINDOW,
    );
    Ok(handles::ManagedProvider::local(Arc::new(provider)))
}

/// Largest backend read window for an on-demand managed fetch.
const MANAGED_FETCH_MAX_WINDOW: u64 = 64 * 1024 * 1024;

/// Translates the compiled index tree into namespace seed nodes: every
/// directory and file path with its committed size. The seed records the
/// index path as the legacy binding for each inode.
fn seed_nodes_from_index(index: &MountIndex) -> Vec<mirage_db::NamespaceSeedNode> {
    let mut nodes = Vec::new();
    let mut pending = vec![(0_u32, String::new())];
    while let Some((ordinal, prefix)) = pending.pop() {
        let Ok(directory) = index.directory_by_index(ordinal) else {
            continue;
        };
        let mut marker: Option<String> = None;
        loop {
            let Ok(children) = index.children_after_marker(directory, marker.as_deref(), 4096)
            else {
                break;
            };
            if children.is_empty() {
                break;
            }
            marker = children.last().map(|child| child.name().to_owned());
            for child in &children {
                let name = child.name();
                let path = if prefix.is_empty() {
                    name.to_owned()
                } else {
                    format!("{prefix}/{name}")
                };
                if child.is_directory() {
                    nodes.push(mirage_db::NamespaceSeedNode {
                        path: path.clone(),
                        is_directory: true,
                        size: 0,
                        version_root: None,
                    });
                    pending.push((child.index(), path));
                } else {
                    let size = index
                        .file_by_index(child.index())
                        .map(|file| file.logical_size())
                        .unwrap_or(0);
                    nodes.push(mirage_db::NamespaceSeedNode {
                        path,
                        is_directory: false,
                        size,
                        version_root: None,
                    });
                }
            }
            if children.len() < 4096 {
                break;
            }
        }
    }
    nodes
}
