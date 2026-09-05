use std::path::{Path, PathBuf};

use mirage_types::{MirageError, MirageErrorKind};

pub(crate) fn sqlite_integer(value: u64, label: &'static str) -> Result<i64, MirageError> {
    i64::try_from(value)
        .map_err(|_| MirageError::invalid_argument(format!("{label} exceeds SQLite INTEGER")))
}

pub(crate) fn nonnegative(value: i64, label: &'static str) -> Result<u64, MirageError> {
    u64::try_from(value).map_err(|_| {
        MirageError::new(
            MirageErrorKind::IntegrityMismatch,
            MirageErrorKind::IntegrityMismatch.default_code(),
            format!("database {label} is negative"),
        )
    })
}

pub(crate) fn fixed<const N: usize>(
    value: Vec<u8>,
    label: &'static str,
) -> Result<[u8; N], MirageError> {
    value.try_into().map_err(|_| {
        MirageError::new(
            MirageErrorKind::IntegrityMismatch,
            MirageErrorKind::IntegrityMismatch.default_code(),
            format!("database {label} has invalid byte width"),
        )
    })
}

pub(crate) fn path_text(path: &Path) -> Result<&str, MirageError> {
    path.to_str()
        .filter(|value| !value.is_empty() && value.len() <= 32_767)
        .ok_or_else(|| MirageError::invalid_argument("database path must be bounded valid UTF-8"))
}

pub(crate) fn stored_path(value: String) -> Result<PathBuf, MirageError> {
    if value.is_empty() || value.len() > 32_767 {
        return Err(MirageError::integrity_mismatch(
            "stored database path is empty or oversized",
        ));
    }
    Ok(PathBuf::from(value))
}

pub(crate) fn bounded_text(
    value: &str,
    minimum: usize,
    maximum: usize,
    label: &'static str,
) -> Result<(), MirageError> {
    if (minimum..=maximum).contains(&value.len()) && !value.chars().any(char::is_control) {
        Ok(())
    } else {
        Err(MirageError::invalid_argument(format!(
            "{label} is empty, oversized, or contains control characters"
        )))
    }
}
