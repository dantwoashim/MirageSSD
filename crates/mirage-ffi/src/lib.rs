#![allow(unsafe_code)]
pub mod directory_backend;
pub mod handles;
pub mod namespace;
pub mod publisher;
pub mod status;
use handles::{DecodedPageCache, Entry, ViolationLog};
pub use handles::{MirageEngineHandle, MirageFileHandle};
pub use status::MirageStatus;
use std::collections::HashMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::ptr;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};

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
            managed: false,
            dirty: None,
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
            state_root: None,
            managed: false,
            dirty: None,
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
            state_root: Some(state_root.clone()),
            managed: false,
            dirty: None,
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
    contained(|| {
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
    output: *mut *mut MirageEngineHandle,
) -> MirageStatus {
    let debug_stage = |stage: &str, status: MirageStatus| -> MirageStatus {
        if std::env::var_os("MIRAGE_DEBUG_PROVIDER").is_some() {
            eprintln!("managed create failed at {stage}: {status:?}");
        }
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
        let Ok(db) = mirage_db::Database::open(&state_root.join("control.db")) else {
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
            Err(_) => return MirageStatus::IoError,
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
        let journal_dir = state_root.join("journal");
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
                        "let (Some(resident), Some(shard)) = (resident.clone(), shard",
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
                        "let (Some(resident), Some(shard)) = (resident.clone(), shard",
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
                state_root.join("journal"),
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
            extents: Arc::new(std::sync::Mutex::new(Default::default())),
            state_root: Some(state_root),
            managed: true,
            dirty,
            managed_provider,
            publisher,
            remote_payloads,
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
/// # Safety
/// `handle` must be null or a pointer returned by an engine constructor and not
/// previously destroyed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mirage_engine_destroy(handle: *mut MirageEngineHandle) -> MirageStatus {
    contained(|| {
        if !handle.is_null() {
            let engine = unsafe { Box::from_raw(handle) };
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
                    journal.reclaim_pending(&state_root.join("journal"), now_ns_i64())?;
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
        let (Some(db), Some(state_root), Some(dirty)) =
            (&engine.db, &engine.state_root, &engine.dirty)
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
        let journal_dir = state_root.join("journal");
        for (payload_id, length_bytes) in dead {
            // Only after the ledger commit does the file go; a failed delete
            // is left for the startup orphan sweep.
            let mut name = String::with_capacity(40);
            for byte in payload_id {
                name.push_str(&format!("{byte:02x}"));
            }
            name.push_str(".payload");
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
        if let Some(publisher) = &engine_ref.publisher {
            publisher.drain();
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
                let inode = match db.namespace_resolve_components(volume, &components) {
                    Ok(Some(inode)) => inode,
                    Ok(None) => return MirageStatus::NotFound,
                    Err(_) => return MirageStatus::IoError,
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
                        state_root: engine.state_root.clone(),
                        desired_access: mirage_engine::handles::DesiredAccess {
                            read: true,
                            write: true,
                            delete: true,
                        },
                        share_access: mirage_engine::handles::ShareAccess::ALL,
                        managed: engine.managed,
                        dirty: engine.dirty.clone(),
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
                    state_root: engine.state_root.clone(),
                    desired_access: mirage_engine::handles::DesiredAccess {
                        read: true,
                        write: true,
                        delete: true,
                    },
                    share_access: mirage_engine::handles::ShareAccess::ALL,
                    managed: engine.managed,
                    dirty: engine.dirty.clone(),
                })),
            )
        };
        MirageStatus::Ok
    })
}

/// Derives a stable 64-bit index number from an inode for namespace-only
/// entries (the inode is the durable identity; the index has no ordinal for
/// locally created nodes).
fn inode_stable_index(inode: InodeId) -> u64 {
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
            let maps = match extent_map_for(handle, inode) {
                Ok(maps) => maps,
                Err(status) => return status,
            };
            if let Some(map) = maps.get(&inode)
                && (!map.is_plain_base() || handle.node.is_none())
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
            drop(maps);
        }
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
        let handle = unsafe { &*handle };
        let entry = &handle.entry;
        // Report the mutable logical EOF when the inode carries extent
        // history — the committed index size is the base, not the file.
        let size = match handle.inode.and_then(|inode| {
            handle.db.as_ref().and_then(|db| {
                let volume = handle
                    .index
                    .as_ref()
                    .map(|index| index.header().repository_id)?;
                db.extent_head(volume, inode).ok().flatten()
            })
        }) {
            Some((_, eof)) => eof,
            None => entry.size,
        };
        unsafe {
            ptr::write(
                output,
                MirageFileInfo {
                    stable_index: entry.index,
                    size,
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
        let marker = if marker_len == 0 {
            None
        } else {
            let units = unsafe { std::slice::from_raw_parts(marker, marker_len) };
            match String::from_utf16(units) {
                Ok(value) => Some(value),
                Err(_) => return MirageStatus::InvalidArgument,
            }
        };
        // Managed volumes enumerate the durable namespace — it covers the
        // seeded index tree plus every locally created or renamed entry.
        if handle.managed
            && let (Some(inode), Some(db)) = (handle.inode, &handle.db)
        {
            let volume = handle
                .index
                .as_ref()
                .map(|index| index.header().repository_id);
            let Some(volume) = volume else {
                return MirageStatus::IntegrityFailure;
            };
            let children = match db.namespace_list_children(volume, inode, marker.as_deref(), limit)
            {
                Ok(children) => children,
                Err(_) => return MirageStatus::IoError,
            };
            for child in children {
                let name = child.display_name.encode_utf16().collect::<Vec<_>>();
                let size = if child.kind == mirage_db::NamespaceNodeKind::File {
                    db.extent_head(volume, child.inode)
                        .ok()
                        .flatten()
                        .map(|(_, eof)| eof)
                        .unwrap_or(child.size)
                } else {
                    0
                };
                let info = MirageFileInfo {
                    stable_index: inode_stable_index(child.inode),
                    size,
                    directory: u8::from(child.kind == mirage_db::NamespaceNodeKind::Directory),
                    reserved: [0; 7],
                };
                if unsafe { callback(context, name.as_ptr(), name.len(), info) } == 0 {
                    break;
                }
            }
            return MirageStatus::Ok;
        }
        let (Some(index), Some(NodeIndex::Directory(ordinal))) = (&handle.index, handle.node)
        else {
            return MirageStatus::InvalidArgument;
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
        if let Some(publisher) = &file.publisher {
            publisher.notify();
        }
        if let (Some(inode), Some(db), Some(index)) = (file.inode, &file.db, &file.index)
            && let Ok(true) = file
                .handles
                .close(inode, file.desired_access, file.share_access)
        {
            let volume = index.header().repository_id;
            // Finalize by inode identity: the open-time path may be stale
            // after a rename, and a delete-without-read never populated it.
            if let Ok(Some((parent, name))) = db.namespace_entry(volume, inode) {
                let now = now_ns_i64();
                if db.namespace_delete(volume, parent, &name, now).is_ok() {
                    let _ = journal_namespace_operation(
                        db,
                        volume,
                        mirage_db::OperationKind::Delete,
                        name.as_bytes().to_vec(),
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

/// Mutation preconditions: a managed engine (writable mode), a mounted
/// coordinator, plus the control database.
fn mutation_context(
    engine: &MirageEngineHandle,
) -> Result<(&mirage_db::Database, RepositoryId), MirageStatus> {
    if !engine.managed {
        return Err(MirageStatus::AccessDenied);
    }
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

/// Physical-ledger file id for a managed volume's journal: one
/// variable-length "file" tracks every staged dirty payload.
const JOURNAL_FILE_ID: [u8; 16] = mirage_db::payload_remote::MANAGED_JOURNAL_FILE_ID;
/// How long a write waits for uploads to free local budget before it fails
/// with disk full. Kept well below WinFsp's IRP timeout.
const WRITE_BACKPRESSURE_LIMIT: std::time::Duration = std::time::Duration::from_secs(90);
const WRITE_BACKPRESSURE_STEP: std::time::Duration = std::time::Duration::from_millis(250);

/// Splits a mount-relative UTF-16 path into namespace components, accepting
/// both `\` and `/` separators. Rejects empty components, `.`/`..`, and NUL.
/// The volume root yields an empty component list.
fn managed_path_components(units: &[u16]) -> Result<Vec<String>, MirageStatus> {
    if units.contains(&0) {
        return Err(MirageStatus::InvalidArgument);
    }
    let text = String::from_utf16(units).map_err(|_| MirageStatus::InvalidArgument)?;
    let trimmed = text.trim_matches(['/', '\\']);
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }
    let mut components = Vec::new();
    for part in trimmed.split(['/', '\\']) {
        if part.is_empty() || part == "." || part == ".." {
            return Err(MirageStatus::InvalidArgument);
        }
        components.push(part.to_string());
    }
    Ok(components)
}

fn hex_encode32(bytes: &[u8; 32]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn hex16_decode(text: &str) -> Result<[u8; 16], ()> {
    if text.len() != 32 {
        return Err(());
    }
    let mut out = [0_u8; 16];
    for (index, slot) in out.iter_mut().enumerate() {
        *slot = u8::from_str_radix(&text[index * 2..index * 2 + 2], 16).map_err(|_| ())?;
    }
    Ok(out)
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
        let components: Vec<String> = parent_path
            .split('/')
            .filter(|part| !part.is_empty())
            .map(str::to_string)
            .collect();
        db.namespace_resolve_components(volume, &components)
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
        // Windows sharing semantics: renaming over a file that has live
        // open handles must fail, not destroy the victim.
        let source = match db.namespace_lookup(volume, from_parent, &from_name) {
            Ok(source) => source,
            Err(_) => return MirageStatus::IoError,
        };
        match db.namespace_lookup(volume, to_parent, &to_name) {
            Ok(Some(victim))
                if Some(victim.inode) != source.map(|entry| entry.inode)
                    && engine.handles.is_open(victim.inode) =>
            {
                return MirageStatus::Conflict;
            }
            Ok(_) => {}
            Err(_) => return MirageStatus::IoError,
        }
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
        let components: Vec<String> = full
            .split('/')
            .filter(|part| !part.is_empty())
            .map(str::to_string)
            .collect();
        let inode = match db.namespace_resolve_components(volume, &components) {
            Ok(Some(inode)) => inode,
            Ok(None) => return MirageStatus::NotFound,
            Err(_) => return MirageStatus::IoError,
        };
        // Delete-pending semantics: the name unlinks now (the open-handle
        // tombstone keeps identity for existing readers), and the last close
        // finalizes — re-issuing the delete is then a harmless no-op.
        match engine.handles.request_delete(inode) {
            Ok(mirage_engine::handles::DeleteDisposition::Pending)
            | Ok(mirage_engine::handles::DeleteDisposition::Removed) => {}
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
    let extents = map.to_extents(volume, inode, map.version(), now_ns, || {
        let mut id = [0u8; 16];
        let _ = getrandom::fill(&mut id);
        id
    });
    let mut operation_id = [0u8; 16];
    if getrandom::fill(&mut operation_id).is_err() {
        return Err(MirageStatus::Internal);
    }
    let device_seq = db
        .next_operation_seq(volume)
        .map_err(|_| MirageStatus::IoError)?;
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
    db.writer()
        .mutation_commit(
            Some(mirage_db::ExtentMutation {
                volume_id: volume,
                inode,
                version: map.version(),
                eof: map.file_size(),
                extents,
            }),
            mirage_db::OperationRecord {
                operation_id,
                device_seq,
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
            now_ns,
        )
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
        let Some(dirty) = &handle.dirty else {
            return MirageStatus::Internal;
        };
        let mut maps = match extent_map_for(handle, inode) {
            Ok(maps) => maps,
            Err(status) => return status,
        };
        // The dirty mutex serializes budget check + reservation + staging +
        // ledger commit across writers; the maps lock it is taken under
        // serializes extent mutations for the whole engine.
        let mut _dirty_guard = match dirty.mutex.lock() {
            Ok(guard) => guard,
            Err(_) => return MirageStatus::Internal,
        };
        let length = u64::try_from(bytes_len).unwrap_or(u64::MAX);
        let mut waited = std::time::Duration::ZERO;
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
            publisher.notify();
            std::thread::sleep(WRITE_BACKPRESSURE_STEP);
            waited += WRITE_BACKPRESSURE_STEP;
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
                    &pins,
                    length.max(deficit),
                );
                let free = floor_free_space(&handle.floor_free_cache, &journal_dir, true);
                if free.is_some_and(|free| free < length.saturating_add(floor)) {
                    return MirageStatus::DiskFull;
                }
            }
        }
        let mut payload_id = [0_u8; 16];
        if getrandom::fill(&mut payload_id).is_err() {
            return MirageStatus::Internal;
        }
        let slot_index = dirty.next_slot.fetch_add(1, Ordering::AcqRel);
        let mut owner_epoch = [0_u8; 16];
        owner_epoch[..8].copy_from_slice(
            &handle
                .coordinator
                .as_ref()
                .map(|coordinator| coordinator.epoch())
                .unwrap_or(0)
                .to_le_bytes(),
        );
        let release_reservation = |db: &mirage_db::Database| {
            let _ = db.writer().physical_release_extent(payload_id, now);
        };
        if db
            .writer()
            .physical_reserve_extent(
                mirage_db::PhysicalExtentRecord {
                    extent_id: payload_id,
                    file_id: dirty.file_id,
                    slot_index,
                    length_bytes: i64::try_from(length).unwrap_or(i64::MAX),
                    state: mirage_db::PhysicalExtentState::Reserved,
                    page_hash: None,
                    checksum: None,
                    pin_count: 0,
                    generation: 0,
                    updated_ns: now,
                },
                mirage_db::PhysicalReservationRecord {
                    extent_id: payload_id,
                    owner_epoch,
                    expires_ns: now.saturating_add(60_000_000_000),
                },
            )
            .is_err()
        {
            return MirageStatus::IoError;
        }
        let staged = match journal.stage_payload_as(&journal_dir, data, payload_id) {
            Ok(staged) => staged,
            Err(_) => {
                release_reservation(db);
                return MirageStatus::IoError;
            }
        };
        // Mutate a scratch copy: the live map only publishes after the
        // durable commit lands, so a failed write can never poison reads.
        let mut next_map = maps.get(&inode).expect("map just inserted").clone();
        if next_map
            .write(offset, staged.bytes, staged.payload_id)
            .is_err()
        {
            release_reservation(db);
            let _ = std::fs::remove_file(journal_dir.join(&staged.path));
            return MirageStatus::InvalidArgument;
        }
        // The payload's content hash doubles as its ledger page hash — the
        // physical commit lands inside the same transaction as the extent
        // mutation, so the ledger and the journal always agree.
        if commit_extent_mutation(
            handle,
            inode,
            &next_map,
            mirage_db::OperationKind::Write,
            staged.payload_id,
            staged.path.clone(),
            staged.bytes,
            Some(staged.checksum),
            Some(mirage_db::PhysicalCommit {
                extent_id: payload_id,
                page_hash: mirage_types::PageHash::from_bytes(staged.checksum),
                checksum: staged.checksum,
            }),
            now,
        )
        .is_err()
        {
            release_reservation(db);
            // The staged .payload was never committed — remove it so a failed
            // write cannot leak journal bytes.
            let _ = std::fs::remove_file(journal_dir.join(&staged.path));
            return MirageStatus::IoError;
        }
        dirty.used.fetch_add(length, Ordering::AcqRel);
        maps.insert(inode, next_map);
        drop(maps);
        unsafe { *transferred = bytes_len };
        // A committed payload is publishable: wake the publisher.
        if let Some(publisher) = &handle.publisher {
            publisher.notify();
        }
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
        // Mutate a scratch copy: the live map only publishes after the
        // durable commit lands.
        let mut next_map = maps.get(&inode).expect("map just inserted").clone();
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
            Ok(_) => {
                if let Some(publisher) = &handle.publisher {
                    publisher.notify();
                }
                MirageStatus::Ok
            }
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
                use std::io::{Read, Seek, SeekFrom};
                match std::fs::File::open(&path) {
                    Ok(mut file) => {
                        if file.seek(SeekFrom::Start(payload_offset)).is_err() {
                            return MirageStatus::IoError;
                        }
                        let destination =
                            unsafe { std::slice::from_raw_parts_mut(output.add(dst), len) };
                        if file.read_exact(destination).is_err() {
                            return MirageStatus::IoError;
                        }
                    }
                    // An evicted payload is remote-only: first try to re-stage
                    // the whole file (budget/floor permitting) so subsequent
                    // reads are local; otherwise fetch just the needed frames.
                    // No publication row means genuinely unavailable — never
                    // zeros.
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        if restage_evicted_payload(handle, journal_dir, &payload_id).is_ok()
                            && let Ok(mut file) = std::fs::File::open(&path)
                        {
                            if file.seek(SeekFrom::Start(payload_offset)).is_err() {
                                return MirageStatus::IoError;
                            }
                            let destination =
                                unsafe { std::slice::from_raw_parts_mut(output.add(dst), len) };
                            if file.read_exact(destination).is_err() {
                                return MirageStatus::IoError;
                            }
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
                        match remote.read(db, volume, &payload_id, payload_offset, len) {
                            Ok(Some(bytes)) => {
                                let destination =
                                    unsafe { std::slice::from_raw_parts_mut(output.add(dst), len) };
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

/// Re-stages an evicted published payload: fetch and verify the whole remote
/// object, write it back to the journal atomically, revive the physical
/// extent in one writer transaction, and re-add its bytes to the dirty
/// ledger — only when the dirty budget and the disk floor admit it.
/// Callers fall back to the transient frame fetch on any error.
fn restage_evicted_payload(
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

/// Free bytes on the volume holding `path`, read through a 1-second cache
/// unless `force` refreshes it after an eviction pass.
fn floor_free_space(
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
    let (Some(db), Some(dirty), Some(state_root), Some(volume)) = (
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
        &state_root.join("journal"),
        volume,
        &handle.handles,
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
