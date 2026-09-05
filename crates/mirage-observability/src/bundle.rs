use mirage_types::{MirageError, MirageErrorKind};
use std::{
    fs,
    path::{Path, PathBuf},
};

/// Copies an explicit allowlist into an atomic diagnostic directory under a hard byte cap.
pub fn export_allowlisted(
    destination: &Path,
    files: &[PathBuf],
    max_bytes: u64,
) -> Result<(), MirageError> {
    let temp = destination.with_extension("partial");
    if temp.exists() {
        fs::remove_dir_all(&temp).map_err(|e| io("clear partial bundle failed", e))?;
    }
    fs::create_dir_all(&temp).map_err(|e| io("create bundle failed", e))?;
    let mut total = 0_u64;
    for (index, source) in files.iter().enumerate() {
        let bytes = fs::read(source).map_err(|e| io("read bundle input failed", e))?;
        total = total
            .checked_add(bytes.len() as u64)
            .ok_or_else(|| MirageError::invalid_argument("bundle size overflow"))?;
        if total > max_bytes {
            return Err(MirageError::invalid_argument(
                "diagnostic bundle exceeds size bound",
            ));
        }
        fs::write(temp.join(format!("{index}.log")), bytes)
            .map_err(|e| io("write bundle failed", e))?;
    }
    if destination.exists() {
        return Err(MirageError::repository_conflict(
            "diagnostic destination exists",
        ));
    }
    fs::rename(temp, destination).map_err(|e| io("publish bundle failed", e))
}

fn io(message: &'static str, error: std::io::Error) -> MirageError {
    MirageError::new(
        MirageErrorKind::Io,
        MirageErrorKind::Io.default_code(),
        message,
    )
    .with_source(error)
}
