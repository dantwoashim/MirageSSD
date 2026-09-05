use mirage_types::{ContentHash, MirageError};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeSnapshotRecord {
    pub relative_path: PathBuf,
    pub snapshot_path: PathBuf,
    pub byte_length: u64,
    pub content_hash: ContentHash,
    pub readonly: bool,
}

pub fn create_native_snapshots(
    game_root: &Path,
    rollback_root: &Path,
    relative_paths: &[PathBuf],
) -> Result<Vec<NativeSnapshotRecord>, MirageError> {
    let root = game_root.canonicalize().map_err(MirageError::from)?;
    std::fs::create_dir_all(rollback_root).map_err(MirageError::from)?;
    let rollback = rollback_root.canonicalize().map_err(MirageError::from)?;
    if rollback.starts_with(&root) {
        return Err(MirageError::invalid_argument(
            "rollback root must be outside game root",
        ));
    }
    let mut paths = relative_paths.to_vec();
    paths.sort();
    paths.dedup();
    let mut records = Vec::new();
    for relative in paths {
        if relative.is_absolute()
            || relative.components().any(|c| {
                matches!(
                    c,
                    std::path::Component::ParentDir | std::path::Component::Prefix(_)
                )
            })
        {
            return Err(MirageError::invalid_argument(
                "native snapshot path escapes game root",
            ));
        }
        let source = root
            .join(&relative)
            .canonicalize()
            .map_err(MirageError::from)?;
        if !source.starts_with(&root) || !source.is_file() {
            return Err(MirageError::invalid_argument(
                "native snapshot source is outside game root or not a file",
            ));
        }
        let bytes = std::fs::read(&source).map_err(MirageError::from)?;
        let hash = ContentHash::from_bytes(*blake3::hash(&bytes).as_bytes());
        let destination = rollback.join(&relative);
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent).map_err(MirageError::from)?;
        }
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&destination)
            .map_err(MirageError::from)?;
        file.write_all(&bytes).map_err(MirageError::from)?;
        file.sync_all().map_err(MirageError::from)?;
        records.push(NativeSnapshotRecord {
            relative_path: relative,
            snapshot_path: destination,
            byte_length: bytes.len() as u64,
            content_hash: hash,
            readonly: source
                .metadata()
                .map_err(MirageError::from)?
                .permissions()
                .readonly(),
        });
    }
    Ok(records)
}

pub fn restore_native_snapshots(
    game_root: &Path,
    records: &[NativeSnapshotRecord],
) -> Result<(), MirageError> {
    let root = game_root.canonicalize().map_err(MirageError::from)?;
    for record in records.iter().rev() {
        let bytes = std::fs::read(&record.snapshot_path).map_err(MirageError::from)?;
        if bytes.len() as u64 != record.byte_length
            || blake3::hash(&bytes).as_bytes() != record.content_hash.as_bytes()
        {
            return Err(MirageError::integrity_mismatch(
                "native rollback snapshot is corrupt",
            ));
        }
        let target = root.join(&record.relative_path);
        let parent = target
            .parent()
            .ok_or_else(|| MirageError::invalid_argument("native rollback target has no parent"))?;
        std::fs::create_dir_all(parent).map_err(MirageError::from)?;
        let temporary = target.with_extension("mirage-restore");
        std::fs::write(&temporary, &bytes).map_err(MirageError::from)?;
        if target.exists() {
            std::fs::remove_file(&target).map_err(MirageError::from)?;
        }
        std::fs::rename(&temporary, &target).map_err(MirageError::from)?;
        let mut permissions = target.metadata().map_err(MirageError::from)?.permissions();
        permissions.set_readonly(record.readonly);
        std::fs::set_permissions(&target, permissions).map_err(MirageError::from)?;
    }
    Ok(())
}
