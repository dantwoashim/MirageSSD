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

/// Append-only single-line log with bounded size and N rotated generations
/// (`name.1.log` is newest). Used by the service, filesystem hosts, the UI
/// host, and the logon agent — it never writes secrets.
pub struct RotatingLog {
    path: PathBuf,
    max_bytes: u64,
    keep: usize,
    lock: Mutex<()>,
}

impl RotatingLog {
    pub fn new(path: PathBuf, max_bytes: u64, keep: usize) -> Result<Self, MirageError> {
        if max_bytes == 0 || keep == 0 {
            return Err(MirageError::invalid_argument(
                "rotating log bounds must be nonzero",
            ));
        }
        Ok(Self {
            path,
            max_bytes,
            keep,
            lock: Mutex::new(()),
        })
    }

    /// Appends one line (a newline is added when absent). Rotation renames
    /// generations up before the write; failures are swallowed silently —
    /// logging must never break the caller.
    pub fn write_line(&self, line: &str) {
        if self.lock.lock().is_err() {
            return;
        }
        if let Some(parent) = self.path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        if fs::metadata(&self.path).map(|m| m.len()).unwrap_or(0) >= self.max_bytes {
            self.rotate_generations();
        }
        if let Ok(mut file) = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
        {
            let mut line = line.trim_end_matches(['\r', '\n']).to_owned();
            line.push('\n');
            let _ = file.write_all(line.as_bytes());
        }
    }

    fn rotated_path(&self, generation: usize) -> PathBuf {
        let stem = self
            .path
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_else(|| "log".to_owned());
        let ext = self
            .path
            .extension()
            .map(|ext| format!(".{}", ext.to_string_lossy()))
            .unwrap_or_default();
        self.path
            .with_file_name(format!("{stem}.{generation}{ext}"))
    }

    fn rotate_generations(&self) {
        for generation in (1..self.keep).rev() {
            let from = self.rotated_path(generation);
            if from.exists() {
                let _ = fs::rename(&from, self.rotated_path(generation + 1));
            }
        }
        if self.path.exists() {
            let _ = fs::rename(&self.path, self.rotated_path(1));
        }
    }
}
