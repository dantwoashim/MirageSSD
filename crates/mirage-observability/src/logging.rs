use mirage_types::{MirageError, MirageErrorKind};
use serde::Serialize;
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::Mutex,
};

#[derive(Serialize)]
pub struct Event<'a> {
    pub name: &'a str,
    pub repository: &'a str,
    pub detail: &'a str,
}

pub struct BoundedJsonLog {
    path: PathBuf,
    max_bytes: u64,
    lock: Mutex<()>,
}
impl BoundedJsonLog {
    pub fn new(path: PathBuf, max_bytes: u64) -> Result<Self, MirageError> {
        if max_bytes == 0 {
            return Err(MirageError::invalid_argument("log bound must be nonzero"));
        }
        Ok(Self {
            path,
            max_bytes,
            lock: Mutex::new(()),
        })
    }
    pub fn append(&self, event: &Event<'_>) -> Result<(), MirageError> {
        let _guard = self
            .lock
            .lock()
            .map_err(|_| MirageError::internal_invariant("log mutex poisoned"))?;
        if fs::metadata(&self.path).map(|m| m.len()).unwrap_or(0) >= self.max_bytes {
            rotate(&self.path)?;
        }
        let mut line = serde_json::to_vec(event)
            .map_err(|_| MirageError::internal_invariant("event serialization failed"))?;
        line.push(b'\n');
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .map_err(|error| io("open diagnostic log failed", error))?;
        file.write_all(&line)
            .map_err(|error| io("write diagnostic log failed", error))?;
        file.sync_data()
            .map_err(|error| io("flush diagnostic log failed", error))
    }
}
fn rotate(path: &Path) -> Result<(), MirageError> {
    let previous = path.with_extension("previous.log");
    if previous.exists() {
        fs::remove_file(&previous).map_err(|e| io("remove old rotated log failed", e))?;
    }
    if path.exists() {
        fs::rename(path, previous).map_err(|e| io("rotate diagnostic log failed", e))?;
    }
    Ok(())
}

fn io(message: &'static str, error: std::io::Error) -> MirageError {
    MirageError::new(
        MirageErrorKind::Io,
        MirageErrorKind::Io.default_code(),
        message,
    )
    .with_source(error)
}
