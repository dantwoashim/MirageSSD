//! Namespace mutation exports and journaling helpers.

use super::*;

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

/// Mints the operation id a journaled namespace change will carry.
fn fresh_journal_id() -> Result<[u8; 16], MirageStatus> {
    let mut operation_id = [0u8; 16];
    if getrandom::fill(&mut operation_id).is_err() {
        return Err(MirageStatus::Internal);
    }
    Ok(operation_id)
}

/// Journals a standalone namespace operation with no accompanying namespace
/// change — used by the delete-pending finalize path on file close.
pub(super) fn journal_namespace_operation(
    db: &mirage_db::Database,
    volume: RepositoryId,
    kind: mirage_db::OperationKind,
    payload: Vec<u8>,
    now_ns: i64,
) -> Result<(), MirageStatus> {
    let operation_id = fresh_journal_id()?;
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
pub(super) const JOURNAL_FILE_ID: [u8; 16] = mirage_db::payload_remote::MANAGED_JOURNAL_FILE_ID;

/// Splits a mount-relative UTF-16 path into namespace components, accepting
/// both `\` and `/` separators. Rejects empty components, `.`/`..`, and NUL.
/// The volume root yields an empty component list.
pub(super) fn managed_path_components(units: &[u16]) -> Result<Vec<String>, MirageStatus> {
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

pub(super) fn hex_encode32(bytes: &[u8; 32]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

pub(super) fn hex16_decode(text: &str) -> Result<[u8; 16], ()> {
    if text.len() != 32 {
        return Err(());
    }
    let mut out = [0_u8; 16];
    for (index, slot) in out.iter_mut().enumerate() {
        *slot = u8::from_str_radix(&text[index * 2..index * 2 + 2], 16).map_err(|_| ())?;
    }
    Ok(out)
}

pub(super) fn now_ns_i64() -> i64 {
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
        let operation_id = match fresh_journal_id() {
            Ok(id) => id,
            Err(status) => return status,
        };
        let _ns = trace::Scope::new(trace::Slot::NsCreate);
        match db.namespace_create_journaled(
            volume,
            parent,
            &name,
            kind,
            now,
            mirage_db::JournalEntry {
                operation_id,
                kind: if directory != 0 {
                    mirage_db::OperationKind::Mkdir
                } else {
                    mirage_db::OperationKind::Create
                },
                payload: format!("{parent_path}/{name}").into_bytes(),
                created_ns: now,
            },
        ) {
            Ok(_) => MirageStatus::Ok,
            Err(error) => mutation_status(&error),
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
        let operation_id = match fresh_journal_id() {
            Ok(id) => id,
            Err(status) => return status,
        };
        let _ns = trace::Scope::new(trace::Slot::NsRename);
        match db.namespace_rename_journaled(
            volume,
            from_parent,
            &from_name,
            to_parent,
            &to_name,
            now,
            mirage_db::JournalEntry {
                operation_id,
                kind: mirage_db::OperationKind::Rename,
                payload: format!("{from_parent_path}/{from_name}\n{to_parent_path}/{to_name}")
                    .into_bytes(),
                created_ns: now,
            },
        ) {
            Ok(()) => MirageStatus::Ok,
            Err(error) => mutation_status(&error),
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
        if let Some(segments) = &engine.segments {
            segments.discard_inode(inode);
        }
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
        let operation_id = match fresh_journal_id() {
            Ok(id) => id,
            Err(status) => return status,
        };
        let _ns = trace::Scope::new(trace::Slot::NsDelete);
        match db.namespace_delete_journaled(
            volume,
            parent,
            &name,
            now,
            mirage_db::JournalEntry {
                operation_id,
                kind: mirage_db::OperationKind::Delete,
                payload: full.into_bytes(),
                created_ns: now,
            },
        ) {
            Ok(()) => MirageStatus::Ok,
            Err(error) => mutation_status(&error),
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
