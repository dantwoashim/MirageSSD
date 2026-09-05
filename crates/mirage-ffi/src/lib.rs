#![allow(unsafe_code)]
pub mod handles;
pub mod namespace;
pub mod status;
use handles::{DecodedPageCache, Entry};
pub use handles::{MirageEngineHandle, MirageFileHandle};
pub use status::MirageStatus;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr;
use std::sync::Arc;

use mirage_cache::{ArenaShard, CacheLayout, ResidentIndex};
use mirage_index::{MountIndex, NodeIndex};
use mirage_pack::{PackReadEncryption, PackReader};
use mirage_types::ByteCount;

fn contained(operation: impl FnOnce() -> MirageStatus) -> MirageStatus {
    catch_unwind(AssertUnwindSafe(operation)).unwrap_or(MirageStatus::Internal)
}

fn safe_object_id(value: &str) -> bool {
    let path = std::path::Path::new(value);
    if value.is_empty()
        || value.len() > 512
        || value.contains(':')
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
            resident: None,
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
            resident: None,
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
        let Ok(shard) =
            ArenaShard::open(&state_root.join("cache").join(&spec.relative_path), layout)
        else {
            return MirageStatus::IntegrityFailure;
        };
        let Ok(resident) =
            ResidentIndex::from_resident_records(snapshot.resident_slots, Arc::new(shard))
        else {
            return MirageStatus::IntegrityFailure;
        };
        let handle = MirageEngineHandle {
            entries: Default::default(),
            index: Some(Arc::new(index)),
            object_root: None,
            encryption: None,
            readers: Arc::new(std::sync::Mutex::new(Default::default())),
            pages: Arc::new(std::sync::Mutex::new(DecodedPageCache::bounded(1))),
            resident: Some(Arc::new(resident)),
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
            unsafe {
                drop(Box::from_raw(handle));
            }
        }
        MirageStatus::Ok
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
                        resident: engine.resident.clone(),
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
                    resident: engine.resident.clone(),
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
        let Ok(spans) = mirage_index::resolve_range(file, offset, output_len) else {
            return MirageStatus::IntegrityFailure;
        };
        let output = unsafe { std::slice::from_raw_parts_mut(output, output_len) };
        for span in &spans {
            let Ok(page) = index.page_by_ordinal(span.page_ordinal.as_u32()) else {
                return MirageStatus::IntegrityFailure;
            };
            let hash = page.plaintext_hash();
            if let Some(resident) = &handle.resident {
                let Ok(Some(guard)) = resident.acquire(hash) else {
                    return MirageStatus::BackendUnavailable;
                };
                let dst = span.dst_offset as usize;
                let Some(dst_end) = dst.checked_add(span.len as usize) else {
                    return MirageStatus::IntegrityFailure;
                };
                let Some(destination) = output.get_mut(dst..dst_end) else {
                    return MirageStatus::IntegrityFailure;
                };
                if guard.read_exact(span.page_offset, destination).is_err() {
                    return MirageStatus::IoError;
                }
                continue;
            }
            let Some(root) = &handle.object_root else {
                return MirageStatus::InvalidArgument;
            };
            let Ok(location) = page.remote_location() else {
                return MirageStatus::IntegrityFailure;
            };
            let Ok(object_id) = location.provider_object_id() else {
                return MirageStatus::IntegrityFailure;
            };
            if !safe_object_id(object_id) {
                return MirageStatus::IntegrityFailure;
            }
            let cached = match handle.pages.lock() {
                Ok(cache) => cache.get(hash),
                Err(_) => return MirageStatus::Internal,
            };
            let page_bytes = if let Some(bytes) = cached {
                bytes
            } else {
                let Ok(mut readers) = handle.readers.lock() else {
                    return MirageStatus::Internal;
                };
                if !readers.contains_key(object_id) {
                    let opened = match &handle.encryption {
                        Some(encryption) => PackReader::open_verified_encrypted(
                            &root.join(object_id),
                            encryption.clone(),
                        ),
                        None => PackReader::open_verified(&root.join(object_id)),
                    };
                    let Ok(reader) = opened else {
                        return MirageStatus::IoError;
                    };
                    readers.insert(object_id.to_owned(), reader);
                }
                let Some(reader) = readers.get_mut(object_id) else {
                    return MirageStatus::Internal;
                };
                let Ok(decoded) = reader.read_page(hash) else {
                    return MirageStatus::IntegrityFailure;
                };
                let bytes: Arc<[u8]> = Arc::from(decoded.page.bytes.to_vec());
                drop(readers);
                let Ok(mut cache) = handle.pages.lock() else {
                    return MirageStatus::Internal;
                };
                cache.insert(hash, Arc::clone(&bytes));
                bytes
            };
            let start = span.page_offset as usize;
            let Some(end) = start.checked_add(span.len as usize) else {
                return MirageStatus::IntegrityFailure;
            };
            let dst = span.dst_offset as usize;
            let Some(dst_end) = dst.checked_add(span.len as usize) else {
                return MirageStatus::IntegrityFailure;
            };
            let Some(source) = page_bytes.get(start..end) else {
                return MirageStatus::IntegrityFailure;
            };
            let Some(destination) = output.get_mut(dst..dst_end) else {
                return MirageStatus::IntegrityFailure;
            };
            destination.copy_from_slice(source);
        }
        let bytes = spans.iter().map(|span| span.len as usize).sum();
        unsafe { ptr::write(transferred, bytes) };
        MirageStatus::Ok
    })
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
        if !handle.is_null() {
            unsafe {
                drop(Box::from_raw(handle));
            }
        }
        MirageStatus::Ok
    })
}
