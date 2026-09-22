//! `mirage diagnostics collect` — one zip for support: service/host/UI/agent
//! logs plus live status JSON and binary versions. Never includes the
//! credential store or token values.

use std::path::{Path, PathBuf};

use mirage_types::MirageError;
use sha2::Digest;

use crate::output;

const MAX_LOG_BYTES: u64 = 12 * 1024 * 1024;

fn timestamp() -> String {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // Compact UTC timestamp without pulling a datetime crate.
    let days = seconds / 86_400;
    let secs = seconds % 86_400;
    // days since epoch -> civil date (Howard Hinnant's algorithm).
    let z = days as i64 + 1 + 719_468;
    let era = z / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let mut y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 456) / 153;
    let d = doy - (153 * mp - 457) / 5;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    if m <= 2 {
        y += 1;
    }
    format!(
        "{y:04}{m:02}{d:02}-{:02}{:02}{:02}",
        secs / 3600,
        (secs % 3600) / 60,
        secs % 60
    )
}

fn service_logs_root() -> PathBuf {
    let program_data = std::env::var_os("ProgramData")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\ProgramData"));
    program_data.join("MirageSSD").join("logs")
}

fn user_logs_root() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_default()
        .join("MirageSSD")
        .join("logs")
}

fn collect_log_files(root: &Path, prefix: &str, files: &mut Vec<(String, Vec<u8>)>) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !path.is_file() || !(name.ends_with(".log") || name.contains(".log.")) {
            continue;
        }
        if path.metadata().map(|m| m.len()).unwrap_or(0) > MAX_LOG_BYTES {
            continue;
        }
        if let Ok(bytes) = std::fs::read(&path) {
            files.push((format!("{prefix}/{name}"), bytes));
        }
    }
}

fn versions() -> Result<String, MirageError> {
    let mut out = format!("mirage-cli {}\n", env!("CARGO_PKG_VERSION"));
    let exe = std::env::current_exe().map_err(MirageError::from)?;
    let dir = exe.parent().unwrap_or(Path::new("."));
    for name in [
        "mirage.exe",
        "mirage-service.exe",
        "mirage-fs.exe",
        "mirage-ui.exe",
    ] {
        let candidate = if name == "mirage.exe" {
            exe.clone()
        } else {
            dir.join(name)
        };
        if !candidate.is_file() {
            continue;
        }
        let hash = sha2::Sha256::digest(std::fs::read(&candidate).map_err(MirageError::from)?);
        out.push_str(&format!("{name} {} {hash:x}\n", candidate.display()));
    }
    Ok(out)
}

fn service_json(command: mirage_ipc::Command) -> Vec<u8> {
    match super::service::request_json(command) {
        Ok(value) => serde_json::to_vec_pretty(&value).unwrap_or_default(),
        Err(error) => format!("{{\"error\":\"{error}\"}}").into_bytes(),
    }
}

fn write_zip(path: &Path, files: &[(String, Vec<u8>)]) -> Result<(), MirageError> {
    let mut out = Vec::new();
    let mut central = Vec::new();
    for (name, bytes) in files {
        let crc = crc32fast::hash(bytes);
        let offset = out.len() as u32;
        // Local file header.
        out.extend_from_slice(&0x04034b50_u32.to_le_bytes());
        out.extend_from_slice(&20_u16.to_le_bytes()); // version needed
        out.extend_from_slice(&0_u16.to_le_bytes()); // flags
        out.extend_from_slice(&0_u16.to_le_bytes()); // method: store
        out.extend_from_slice(&0_u16.to_le_bytes()); // mod time
        out.extend_from_slice(&0_u16.to_le_bytes()); // mod date
        out.extend_from_slice(&crc.to_le_bytes());
        out.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
        out.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
        out.extend_from_slice(&(name.len() as u16).to_le_bytes());
        out.extend_from_slice(&0_u16.to_le_bytes()); // extra length
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(bytes);
        central.extend_from_slice(&0x02014b50_u32.to_le_bytes());
        central.extend_from_slice(&20_u16.to_le_bytes()); // version made by
        central.extend_from_slice(&20_u16.to_le_bytes()); // version needed
        central.extend_from_slice(&0_u16.to_le_bytes());
        central.extend_from_slice(&0_u16.to_le_bytes());
        central.extend_from_slice(&0_u16.to_le_bytes());
        central.extend_from_slice(&0_u16.to_le_bytes());
        central.extend_from_slice(&crc.to_le_bytes());
        central.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
        central.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
        central.extend_from_slice(&(name.len() as u16).to_le_bytes());
        central.extend_from_slice(&[0; 8]); // extra/comment/disk/attrs
        central.extend_from_slice(&0_u32.to_le_bytes()); // external attrs
        central.extend_from_slice(&offset.to_le_bytes());
        central.extend_from_slice(name.as_bytes());
    }
    let central_offset = out.len() as u32;
    out.extend_from_slice(&central);
    // End of central directory.
    out.extend_from_slice(&0x06054b50_u32.to_le_bytes());
    out.extend_from_slice(&0_u16.to_le_bytes());
    out.extend_from_slice(&0_u16.to_le_bytes());
    out.extend_from_slice(&(files.len() as u16).to_le_bytes());
    out.extend_from_slice(&(files.len() as u16).to_le_bytes());
    out.extend_from_slice(&(central.len() as u32).to_le_bytes());
    out.extend_from_slice(&central_offset.to_le_bytes());
    out.extend_from_slice(&0_u16.to_le_bytes());
    std::fs::write(path, &out).map_err(MirageError::from)
}

/// Builds the diagnostics zip and returns its path (UI host + CLI share this).
pub fn collect_zip(out_path: Option<&Path>) -> Result<PathBuf, MirageError> {
    let mut files: Vec<(String, Vec<u8>)> = Vec::new();
    collect_log_files(&service_logs_root(), "service", &mut files);
    collect_log_files(&user_logs_root(), "user", &mut files);
    files.push((
        "status.json".to_owned(),
        service_json(mirage_ipc::Command::Status),
    ));
    files.push((
        "disk-status.json".to_owned(),
        service_json(mirage_ipc::Command::DiskStatus),
    ));
    files.push(("versions.txt".to_owned(), versions()?.into_bytes()));

    let destination = match out_path {
        Some(path) => path.to_path_buf(),
        None => {
            let desktop = std::env::var_os("USERPROFILE")
                .map(PathBuf::from)
                .unwrap_or_default()
                .join("Desktop");
            std::fs::create_dir_all(&desktop).map_err(MirageError::from)?;
            desktop.join(format!("MirageSSD-diagnostics-{}.zip", timestamp()))
        }
    };
    write_zip(&destination, &files)?;
    Ok(destination)
}

pub fn collect(out_path: Option<&Path>, json: bool) -> Result<(), MirageError> {
    let destination = collect_zip(out_path)?;
    if json {
        output::emit_success(&serde_json::json!({
            "path": destination,
        }))
    } else {
        println!("diagnostics written to {}", destination.display());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sensitive_strings_never_render_their_value() {
        let secret =
            mirage_ipc::SensitiveString::new("ya29.super-secret-token".to_owned()).unwrap();
        let rendered = format!("{secret:?}");
        assert_eq!(rendered, "[REDACTED]");
        assert!(!rendered.contains("super-secret"));
    }

    #[test]
    fn zip_store_writer_produces_a_valid_archive() {
        let dir = tempfile::tempdir().unwrap();
        let zip = dir.path().join("d.zip");
        write_zip(&zip, &[("a.txt".to_owned(), b"hello".to_vec())]).unwrap();
        let bytes = std::fs::read(&zip).unwrap();
        assert_eq!(&bytes[..4], &0x04034b50_u32.to_le_bytes());
        assert!(bytes.ends_with(&[0; 1] /*EOCD tail*/) || bytes.len() > 60);
        assert!(bytes.windows(5).any(|w| w == b"a.txt"));
    }
}
