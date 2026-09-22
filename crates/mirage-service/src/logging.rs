//! Persistent structured line log at `C:\ProgramData\MirageSSD\logs\`.
//! Every entry is one JSON-ish line; callers must never pass secrets.

use std::path::PathBuf;
use std::sync::OnceLock;

use mirage_observability::RotatingLog;

const MAX_LOG_BYTES: u64 = 8 * 1024 * 1024;
const KEEP_GENERATIONS: usize = 5;

/// `C:\ProgramData\MirageSSD\logs` — service-owned, shared by the service
/// log and per-repository filesystem-host logs.
pub fn logs_root() -> PathBuf {
    let program_data = std::env::var_os("ProgramData")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\ProgramData"));
    program_data.join("MirageSSD").join("logs")
}

fn service_log() -> &'static RotatingLog {
    static LOG: OnceLock<RotatingLog> = OnceLock::new();
    LOG.get_or_init(|| {
        RotatingLog::new(
            logs_root().join("service.log"),
            MAX_LOG_BYTES,
            KEEP_GENERATIONS,
        )
        .expect("service log bounds are nonzero")
    })
}

/// One structured line to `service.log` and stderr. Never log tokens or
/// credentials — names and details describe events only.
pub fn log_event(event: &str, detail: &str) {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let detail = detail.replace(['\\', '"', '\n', '\r'], "_");
    let event = event.replace('"', "_");
    service_log().write_line(&format!(
        "{{\"ts\":{seconds},\"event\":\"{event}\",\"detail\":\"{detail}\"}}"
    ));
    eprintln!("MirageSSD {event}: {detail}");
}
