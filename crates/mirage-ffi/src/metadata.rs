//! File metadata, enumeration, and handle-close exports.

use super::mutation::journal_namespace_operation;
use super::mutation::now_ns_i64;
use super::read::inode_stable_index;
use super::*;

#[derive(Clone, Copy)]
#[repr(C)]
pub struct MirageFileInfo {
    pub stable_index: u64,
    pub size: u64,
    pub directory: u8,
    pub reserved: [u8; 7],
    /// Unix-ns creation time; 0 = unknown.
    pub created_ns: i64,
    /// Unix-ns last-modified time; 0 = unknown.
    pub modified_ns: i64,
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
        // Report the live logical EOF: the in-memory extent map is cheapest
        // (it already reflects unsealed writes), then the durable extent
        // head, then the committed index size.
        let size = match handle.inode.and_then(|inode| {
            let maps = handle.extents.lock().ok()?;
            if let Some(map) = maps.get(&inode) {
                return Some(map.file_size());
            }
            drop(maps);
            handle.db.as_ref().and_then(|db| {
                let volume = handle
                    .index
                    .as_ref()
                    .map(|index| index.header().repository_id)?;
                db.extent_head(volume, inode)
                    .ok()
                    .flatten()
                    .map(|(_, eof)| eof)
            })
        }) {
            Some(size) => size,
            None => entry.size,
        };
        let (created_ns, modified_ns) = handle
            .inode
            .and_then(|inode| handle.segments.as_ref().and_then(|s| s.times(inode)))
            .map(|(created, modified)| {
                let created = if created == 0 {
                    handle.created_ns
                } else {
                    created
                };
                (created, modified)
            })
            .unwrap_or((
                handle.created_ns,
                handle.modified_ns.load(Ordering::Acquire),
            ));
        unsafe {
            ptr::write(
                output,
                MirageFileInfo {
                    stable_index: entry.index,
                    size,
                    directory: u8::from(entry.directory),
                    reserved: [0; 7],
                    created_ns,
                    modified_ns,
                },
            )
        };
        MirageStatus::Ok
    })
}

/// Sets explicit file times (unix ns; 0 = leave unchanged). Durable via the
/// namespace, live in the segment writer's mtime table, and marks the
/// inode's mtime explicit so the sealer stops overwriting it.
///
/// # Safety
/// `handle` must be live.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mirage_set_times(
    handle: *mut MirageFileHandle,
    created_ns: i64,
    modified_ns: i64,
) -> MirageStatus {
    contained(|| {
        if handle.is_null() {
            return MirageStatus::InvalidArgument;
        }
        let handle = unsafe { &*handle };
        if created_ns == 0 && modified_ns == 0 {
            return MirageStatus::Ok;
        }
        let Some(inode) = handle.inode else {
            return MirageStatus::AccessDenied;
        };
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
        let created = (created_ns != 0).then_some(created_ns);
        let modified = (modified_ns != 0).then_some(modified_ns);
        if db
            .writer()
            .namespace_set_times(volume, inode, created, modified)
            .is_err()
        {
            return MirageStatus::IoError;
        }
        if let Some(segments) = &handle.segments {
            segments.set_times(inode, created, modified);
        }
        if let Some(modified) = modified {
            handle.modified_ns.store(modified, Ordering::Release);
            handle.explicit_mtime.store(true, Ordering::Release);
        }
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
            let live_maps = handle.extents.lock().ok();
            for child in children {
                let name = child.display_name.encode_utf16().collect::<Vec<_>>();
                let size = if child.kind == mirage_db::NamespaceNodeKind::File {
                    live_maps
                        .as_ref()
                        .and_then(|maps| maps.get(&child.inode).map(|map| map.file_size()))
                        .or_else(|| {
                            db.extent_head(volume, child.inode)
                                .ok()
                                .flatten()
                                .map(|(_, eof)| eof)
                        })
                        .unwrap_or(child.size)
                } else {
                    0
                };
                let live = handle.segments.as_ref().and_then(|s| s.times(child.inode));
                let info = MirageFileInfo {
                    stable_index: inode_stable_index(child.inode),
                    size,
                    directory: u8::from(child.kind == mirage_db::NamespaceNodeKind::Directory),
                    reserved: [0; 7],
                    created_ns: live
                        .map(|(c, _)| c)
                        .filter(|c| *c != 0)
                        .unwrap_or(child.created_ns),
                    modified_ns: live
                        .map(|(_, m)| m)
                        .filter(|m| *m != 0)
                        .unwrap_or(child.modified_ns),
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
                    created_ns: 0,
                    modified_ns: 0,
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
                    created_ns: 0,
                    modified_ns: 0,
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
        // Close is a durability boundary: a write-capable handle seals its
        // pending segment before share accounting releases the inode.
        if let (Some(inode), Some(segments)) = (file.inode, &file.segments)
            && file.wrote.swap(false, Ordering::AcqRel)
        {
            let _drain = trace::Scope::new(trace::Slot::CloseDrainWait);
            if file.group_durability {
                // Group-commit close: the seal lands on the sealer thread;
                // seal_async only waits when the queue is over the bound.
                let _ = segments.seal_async(inode);
            } else if segments.drain_inode(inode).is_err() {
                eprintln!("close-time seal failed for inode {inode:?}");
            }
        }
        if let (Some(inode), Some(segments)) = (file.inode, &file.segments)
            && file.explicit_mtime.load(Ordering::Acquire)
        {
            segments.clear_explicit(inode);
        }
        // Release share-mode accounting; the last close of a delete-pending
        // tombstone finalizes the durable delete.
        if let Some(publisher) = &file.publisher {
            publisher.notify();
        }
        if let (Some(inode), Some(db), Some(index)) = (file.inode, &file.db, &file.index) {
            let closed = {
                let _close_handles = trace::Scope::new(trace::Slot::CloseHandlesClose);
                file.handles
                    .close(inode, file.desired_access, file.share_access)
            };
            if let Ok(true) = closed {
                if let Some(segments) = &file.segments {
                    segments.discard_inode(inode);
                }
                let volume = index.header().repository_id;
                // Finalize by inode identity: the open-time path may be stale
                // after a rename, and a delete-without-read never populated it.
                if let Ok(Some((parent, name))) = db.namespace_entry(volume, inode) {
                    let now = now_ns_i64();
                    let _finalize = trace::Scope::new(trace::Slot::CloseDeleteFinalize);
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
        }
        MirageStatus::Ok
    })
}
