//! Filesystem and JSON helpers shared by runtime modules.

use super::*;

pub(super) fn ensure_empty_directory(path: &Path, label: &str) -> Result<(), MirageError> {
    ensure_no_reparse(path, label)?;
    if !path.is_dir() {
        return Err(MirageError::repository_conflict(format!(
            "{label} is not a directory"
        )));
    }
    if std::fs::read_dir(path)
        .map_err(MirageError::from)?
        .next()
        .is_some()
    {
        return Err(MirageError::repository_conflict(format!(
            "{label} is not empty"
        )));
    }
    Ok(())
}

pub(super) fn canonical_absent_target(path: &Path, label: &str) -> Result<PathBuf, MirageError> {
    let parent = path
        .parent()
        .ok_or_else(|| MirageError::invalid_argument(format!("{label} has no parent directory")))?;
    let leaf = path.file_name().ok_or_else(|| {
        MirageError::invalid_argument(format!("{label} has no final path component"))
    })?;
    if !matches!(
        Path::new(leaf).components().next(),
        Some(Component::Normal(_))
    ) {
        return Err(MirageError::invalid_argument(format!(
            "{label} has an unsafe final path component"
        )));
    }
    let canonical_parent = canonical_directory(parent, label)?;
    Ok(canonical_parent.join(leaf))
}

pub(super) fn validate_label(value: &str, label: &str) -> Result<(), MirageError> {
    if value.is_empty() || value.len() > 256 || value.chars().any(char::is_control) {
        Err(MirageError::invalid_argument(format!("{label} is invalid")))
    } else {
        Ok(())
    }
}

pub(super) fn validate_launcher(root: &Path, relative: &Path) -> Result<PathBuf, MirageError> {
    if relative.as_os_str().is_empty()
        || relative.is_absolute()
        || relative
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
        || relative
            .components()
            .filter_map(|component| match component {
                Component::Normal(value) => value.to_str(),
                _ => None,
            })
            .any(|component| component.contains(':'))
    {
        return Err(MirageError::invalid_argument(
            "launcher path must be a normal repository-relative path without ADS syntax",
        ));
    }
    let launcher = root.join(relative);
    let mut cursor = root.to_path_buf();
    for component in relative.components() {
        cursor.push(component.as_os_str());
        ensure_no_reparse(&cursor, "launcher path")?;
    }
    let canonical = launcher.canonicalize().map_err(MirageError::from)?;
    if !canonical.starts_with(root) || !canonical.is_file() {
        return Err(MirageError::invalid_argument(
            "launcher escapes the registered native root or is not a file",
        ));
    }
    canonical
        .strip_prefix(root)
        .map(Path::to_path_buf)
        .map_err(|_| MirageError::invalid_argument("launcher escapes native root"))
}

pub(super) fn canonical_directory(path: &Path, label: &str) -> Result<PathBuf, MirageError> {
    ensure_no_reparse(path, label)?;
    let canonical = path.canonicalize().map_err(MirageError::from)?;
    if !canonical.is_dir() {
        return Err(MirageError::invalid_argument(format!(
            "{label} is not a directory"
        )));
    }
    ensure_no_reparse(&canonical, label)?;
    Ok(canonical)
}

pub(super) fn ensure_regular_no_reparse(path: &Path, label: &str) -> Result<(), MirageError> {
    ensure_no_reparse(path, label)?;
    if !path.is_file() {
        return Err(MirageError::invalid_argument(format!(
            "{label} is not a regular file"
        )));
    }
    Ok(())
}

pub(super) fn ensure_no_reparse(path: &Path, label: &str) -> Result<(), MirageError> {
    let metadata = std::fs::symlink_metadata(path).map_err(MirageError::from)?;
    if metadata.file_type().is_symlink() || is_windows_reparse(&metadata) {
        return Err(MirageError::invalid_argument(format!(
            "{label} contains a reparse point"
        )));
    }
    Ok(())
}

#[cfg(windows)]
pub(super) fn is_windows_reparse(metadata: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(not(windows))]
pub(super) fn is_windows_reparse(_: &std::fs::Metadata) -> bool {
    false
}

pub(super) fn validate_single_component(value: &str, label: &str) -> Result<(), MirageError> {
    let path = Path::new(value);
    if value.is_empty()
        || value.contains(':')
        || path.is_absolute()
        || path.components().count() != 1
        || !matches!(path.components().next(), Some(Component::Normal(_)))
    {
        return Err(MirageError::invalid_argument(format!(
            "{label} is not a safe file name"
        )));
    }
    Ok(())
}

pub(crate) fn bounded_read(path: &Path, limit: usize) -> Result<Vec<u8>, MirageError> {
    let length = std::fs::metadata(path).map_err(MirageError::from)?.len();
    if length > limit as u64 {
        return Err(MirageError::invalid_argument(
            "artifact exceeds its byte bound",
        ));
    }
    std::fs::read(path).map_err(MirageError::from)
}

pub(super) fn read_json_bounded<T: for<'de> Deserialize<'de>>(
    path: &Path,
    limit: usize,
    label: &str,
) -> Result<T, MirageError> {
    let bytes = bounded_read(path, limit)?;
    serde_json::from_slice(&bytes).map_err(|error| {
        MirageError::integrity_mismatch(format!("{label} is malformed")).with_source(error)
    })
}

pub(crate) fn write_json_atomic(path: &Path, value: &impl Serialize) -> Result<(), MirageError> {
    let bytes = serde_json::to_vec_pretty(value).map_err(|error| {
        MirageError::internal_invariant("runtime artifact serialization failed").with_source(error)
    })?;
    write_atomic(path, &bytes)
}

pub(super) fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), MirageError> {
    mirage_crypto::durable_file::write_atomic(path, bytes)
}

pub(crate) fn now_ns() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| {
            duration.as_nanos().min(i64::MAX as u128) as i64
        })
}
