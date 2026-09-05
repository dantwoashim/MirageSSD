use std::io::Write;
use std::path::{Path, PathBuf};

use mirage_types::MirageError;

/// Writes and syncs a same-directory temporary file, then atomically replaces the target.
/// The target is never deleted before publication, so a crash exposes the old or new file.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), MirageError> {
    let parent = path
        .parent()
        .ok_or_else(|| MirageError::invalid_argument("durable file has no parent directory"))?;
    std::fs::create_dir_all(parent).map_err(MirageError::from)?;
    let temporary = unique_sibling(path)?;
    let result = (|| {
        let mut file = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)
            .map_err(MirageError::from)?;
        file.write_all(bytes).map_err(MirageError::from)?;
        file.sync_all().map_err(MirageError::from)?;
        drop(file);
        replace(&temporary, path)?;
        sync_parent(parent)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

fn unique_sibling(path: &Path) -> Result<PathBuf, MirageError> {
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| MirageError::invalid_argument("durable file name is not valid Unicode"))?;
    for _ in 0..16 {
        let mut random = [0_u8; 8];
        getrandom::fill(&mut random)
            .map_err(|_| MirageError::internal_invariant("temporary file nonce failed"))?;
        let suffix = random
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let candidate = path.with_file_name(format!(".{name}.{suffix}.tmp"));
        if !candidate.exists() {
            return Ok(candidate);
        }
    }
    Err(MirageError::repository_conflict(
        "could not allocate a unique durable temporary file",
    ))
}

#[cfg(windows)]
fn replace(source: &Path, target: &Path) -> Result<(), MirageError> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    };

    let source = source
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let target = target
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    // SAFETY: both paths are NUL-terminated, live for the synchronous call, and the temporary
    // file is on the target's volume. MoveFileExW performs one replace operation in the kernel.
    if unsafe {
        MoveFileExW(
            source.as_ptr(),
            target.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    } == 0
    {
        return Err(MirageError::from(std::io::Error::last_os_error()));
    }
    Ok(())
}

#[cfg(not(windows))]
fn replace(source: &Path, target: &Path) -> Result<(), MirageError> {
    std::fs::rename(source, target).map_err(MirageError::from)
}

#[cfg(windows)]
fn sync_parent(_parent: &Path) -> Result<(), MirageError> {
    // MOVEFILE_WRITE_THROUGH flushes the rename. Directory handles require a separate native
    // access mode on Windows and add no stronger documented guarantee for this operation.
    Ok(())
}

#[cfg(not(windows))]
fn sync_parent(parent: &Path) -> Result<(), MirageError> {
    std::fs::File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(MirageError::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replacement_never_requires_a_delete_gap() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("state.json");
        write_atomic(&target, b"old").unwrap();
        write_atomic(&target, b"new").unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"new");
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
    }
}
