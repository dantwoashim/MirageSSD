//! Offload a local folder into a managed drive: copy every file, read each
//! one back through the drive and compare hashes, wait until the drive has
//! published everything to Google Drive, and only then (and only on request)
//! delete the local copy. This is how a nearly-full disk gets its space back
//! — the free-space floor cannot create room, it can only guard it.
//!
//! Safety rules: the source is never touched until every file has been
//! verified byte-for-byte through the drive AND the drive reports zero
//! unpublished bytes; a single failure leaves the source intact and is
//! reported by path.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use mirage_types::{MirageError, RepositoryId};

use super::service;
use crate::client::ServiceTransport;

#[derive(Debug, Clone)]
pub struct OffloadSpec {
    pub repository_id: RepositoryId,
    /// Folder on a local disk to move into the drive.
    pub source: PathBuf,
    /// Folder inside the mounted drive that receives the copy; defaults to
    /// `<mount root>\<source folder name>`.
    pub destination: Option<PathBuf>,
    /// Remove the source only after verification and publication.
    pub delete_source: bool,
    /// How long to wait for the drive to finish publishing (0 = don't wait;
    /// the source is then never deleted).
    pub wait_publish: Duration,
}

#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct OffloadReport {
    pub destination: String,
    pub files: u64,
    pub bytes: u64,
    pub copied: u64,
    pub skipped_identical: u64,
    pub verified: u64,
    pub published: bool,
    pub unpublished_bytes_remaining: u64,
    pub source_deleted: bool,
    pub failures: Vec<String>,
}

/// Runs the whole offload. `progress` receives short step labels.
pub fn run(
    spec: &OffloadSpec,
    transport: Option<&dyn ServiceTransport>,
    progress: &mut dyn FnMut(&str),
) -> Result<OffloadReport, MirageError> {
    let source = validate_source(&spec.source)?;
    let detail = request(
        transport,
        mirage_ipc::Command::RepositoryDetail {
            repository_id: spec.repository_id,
        },
    )?;
    if detail["state"].as_str() != Some("ready_mounted") {
        return Err(MirageError::repository_conflict(
            "the drive must be mounted to offload into it",
        ));
    }
    let mount_root = detail["mount_path"]
        .as_str()
        .map(mount_root_of)
        .ok_or_else(|| MirageError::invalid_argument("the drive has no mount path"))?;
    let destination = match &spec.destination {
        Some(destination) => destination.clone(),
        None => mount_root.join(
            source
                .file_name()
                .ok_or_else(|| MirageError::invalid_argument("source folder has no name"))?,
        ),
    };
    if !starts_with_ci(&destination, &mount_root) {
        return Err(MirageError::invalid_argument(
            "the destination must be inside the mounted drive",
        ));
    }
    if starts_with_ci(&destination, &source) || starts_with_ci(&source, &destination) {
        return Err(MirageError::invalid_argument(
            "source and destination folders overlap",
        ));
    }
    let mut report = OffloadReport {
        destination: destination.to_string_lossy().into_owned(),
        ..OffloadReport::default()
    };

    progress("Scanning the folder");
    let files = walk_files(&source)?;
    report.files = files.len() as u64;
    report.bytes = files.iter().map(|(_, len)| *len).sum();

    for (index, (relative, len)) in files.iter().enumerate() {
        let from = source.join(relative);
        let to = destination.join(relative);
        progress(&format!(
            "Copying {} ({}/{})",
            relative.display(),
            index + 1,
            files.len()
        ));
        if let Err(error) = copy_and_verify(&from, &to, *len, &mut report) {
            report
                .failures
                .push(format!("{}: {error}", relative.display()));
        }
    }
    // Empty directories are part of the folder too.
    for directory in walk_dirs(&source)? {
        let _ = std::fs::create_dir_all(destination.join(directory));
    }

    if !report.failures.is_empty() {
        progress("Some files could not be verified; the source was left untouched");
        return Ok(report);
    }

    // Publication: the drive uploads in the background; wait until nothing
    // is left unpublished, bounded by the caller's patience.
    let started = Instant::now();
    loop {
        let detail = request(
            transport,
            mirage_ipc::Command::RepositoryDetail {
                repository_id: spec.repository_id,
            },
        )?;
        let pending_bytes = detail["unpublished_payload_bytes"]
            .as_u64()
            .unwrap_or(u64::MAX);
        let pending = detail["unpublished_payloads"].as_u64().unwrap_or(u64::MAX);
        report.unpublished_bytes_remaining = pending_bytes;
        if pending_bytes == 0 && pending == 0 {
            report.published = true;
            break;
        }
        if started.elapsed() >= spec.wait_publish {
            break;
        }
        progress(&format!(
            "Uploading to Google Drive — {} left",
            format_bytes(pending_bytes)
        ));
        std::thread::sleep(Duration::from_secs(3));
    }

    if spec.delete_source {
        if report.published && report.verified == report.files {
            progress("Everything is verified in Google Drive — removing the local copy");
            remove_tree(&source)?;
            report.source_deleted = true;
        } else {
            progress("Not everything is published yet — the local copy stays");
        }
    }
    Ok(report)
}

fn copy_and_verify(
    from: &Path,
    to: &Path,
    len: u64,
    report: &mut OffloadReport,
) -> Result<(), MirageError> {
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent).map_err(MirageError::from)?;
    }
    let step = |what: &str, error: MirageError| {
        MirageError::new(error.kind, error.code, format!("{what}: {}", error.message))
    };
    let source_hash = hash_file(from).map_err(|e| step("reading the source", e))?;
    let existing_identical = std::fs::metadata(to)
        .ok()
        .filter(|meta| meta.is_file() && meta.len() == len)
        .map(|_| hash_file(to))
        .transpose()
        .map_err(|e| step("reading the existing copy on the drive", e))?
        .is_some_and(|hash| hash == source_hash);
    if existing_identical {
        report.skipped_identical += 1;
    } else {
        // CopyFileEx keeps timestamps; the drive persists them.
        std::fs::copy(from, to).map_err(|e| step("copying to the drive", MirageError::from(e)))?;
        report.copied += 1;
    }
    // Read back through the drive — this is the copy the user will keep.
    let copied_hash = hash_file(to).map_err(|e| step("reading back from the drive", e))?;
    if copied_hash != source_hash {
        return Err(MirageError::integrity_mismatch(
            "the copy on the drive does not match the source",
        ));
    }
    report.verified += 1;
    Ok(())
}

fn hash_file(path: &Path) -> Result<[u8; 32], MirageError> {
    let mut file = std::fs::File::open(path).map_err(MirageError::from)?;
    let mut hasher = blake3::Hasher::new();
    std::io::copy(&mut file, &mut hasher).map_err(MirageError::from)?;
    Ok(*hasher.finalize().as_bytes())
}

/// Every regular file under `root` as (relative path, length); reparse
/// points (junctions, symlinks) are refused so a loop or a redirect can never
/// pull in data from elsewhere.
fn walk_files(root: &Path) -> Result<Vec<(PathBuf, u64)>, MirageError> {
    let mut files = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(&directory).map_err(MirageError::from)? {
            let entry = entry.map_err(MirageError::from)?;
            let path = entry.path();
            let metadata = std::fs::symlink_metadata(&path).map_err(MirageError::from)?;
            if is_reparse(&metadata) {
                return Err(MirageError::invalid_argument(format!(
                    "{} is a link or junction; offload refuses to follow it",
                    path.display()
                )));
            }
            if metadata.is_dir() {
                pending.push(path);
            } else if metadata.is_file() {
                let relative = path
                    .strip_prefix(root)
                    .map_err(|_| MirageError::internal_invariant("walk left its root"))?
                    .to_path_buf();
                files.push((relative, metadata.len()));
            }
        }
    }
    files.sort();
    Ok(files)
}

fn walk_dirs(root: &Path) -> Result<Vec<PathBuf>, MirageError> {
    let mut dirs = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(&directory).map_err(MirageError::from)? {
            let entry = entry.map_err(MirageError::from)?;
            let path = entry.path();
            let metadata = std::fs::symlink_metadata(&path).map_err(MirageError::from)?;
            if metadata.is_dir() && !is_reparse(&metadata) {
                if let Ok(relative) = path.strip_prefix(root) {
                    dirs.push(relative.to_path_buf());
                }
                pending.push(path);
            }
        }
    }
    Ok(dirs)
}

fn remove_tree(root: &Path) -> Result<(), MirageError> {
    std::fs::remove_dir_all(root).map_err(MirageError::from)
}

fn validate_source(source: &Path) -> Result<PathBuf, MirageError> {
    let metadata = std::fs::symlink_metadata(source).map_err(MirageError::from)?;
    if !metadata.is_dir() || is_reparse(&metadata) {
        return Err(MirageError::invalid_argument(
            "the source must be a real folder (not a file, link, or junction)",
        ));
    }
    let canonical = source.canonicalize().map_err(MirageError::from)?;
    let text = canonical.to_string_lossy().into_owned();
    let text = text.strip_prefix("\\\\?\\").unwrap_or(&text).to_owned();
    if text.len() <= 3 {
        return Err(MirageError::invalid_argument(
            "offloading an entire disk is not supported; choose a folder",
        ));
    }
    Ok(PathBuf::from(text))
}

fn mount_root_of(mount_path: &str) -> PathBuf {
    let trimmed = mount_path.trim_end_matches(['\\', '/']);
    if trimmed.len() == 2 && trimmed.ends_with(':') {
        PathBuf::from(format!("{trimmed}\\"))
    } else {
        PathBuf::from(trimmed)
    }
}

fn starts_with_ci(path: &Path, prefix: &Path) -> bool {
    let path = path.to_string_lossy().to_ascii_lowercase();
    let prefix = prefix
        .to_string_lossy()
        .trim_end_matches('\\')
        .to_ascii_lowercase();
    path == prefix || path.starts_with(&format!("{prefix}\\"))
}

#[cfg(windows)]
fn is_reparse(metadata: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    metadata.file_attributes() & 0x400 != 0
}

#[cfg(not(windows))]
fn is_reparse(metadata: &std::fs::Metadata) -> bool {
    metadata.file_type().is_symlink()
}

fn request(
    transport: Option<&dyn ServiceTransport>,
    command: mirage_ipc::Command,
) -> Result<serde_json::Value, MirageError> {
    match transport {
        Some(transport) => service::request_with(transport, command),
        None => service::request_json(command),
    }
}

fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlap_and_mount_root_rules() {
        assert!(starts_with_ci(
            Path::new("N:\\Photos\\2024"),
            Path::new("N:\\")
        ));
        assert!(starts_with_ci(
            Path::new("n:\\photos"),
            Path::new("N:\\Photos")
        ));
        assert!(!starts_with_ci(
            Path::new("N:\\PhotosOld"),
            Path::new("N:\\Photos")
        ));
        assert_eq!(mount_root_of("N:"), PathBuf::from("N:\\"));
        assert_eq!(mount_root_of("N:\\"), PathBuf::from("N:\\"));
    }

    #[test]
    fn copy_and_verify_skips_identical_and_copies_new() {
        let dir = tempfile::tempdir().unwrap();
        let from = dir.path().join("a.bin");
        let to = dir.path().join("out").join("a.bin");
        std::fs::write(&from, b"hello world").unwrap();
        let mut report = OffloadReport::default();
        copy_and_verify(&from, &to, 11, &mut report).unwrap();
        assert_eq!(
            (report.copied, report.skipped_identical, report.verified),
            (1, 0, 1)
        );
        copy_and_verify(&from, &to, 11, &mut report).unwrap();
        assert_eq!(
            (report.copied, report.skipped_identical, report.verified),
            (1, 1, 2)
        );
        assert_eq!(std::fs::read(&to).unwrap(), b"hello world");
    }

    #[test]
    fn walk_refuses_links_and_lists_files_in_order() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("sub").join("b.txt"), b"b").unwrap();
        std::fs::write(dir.path().join("a.txt"), b"aa").unwrap();
        let files = walk_files(dir.path()).unwrap();
        assert_eq!(
            files,
            vec![
                (PathBuf::from("a.txt"), 2),
                (PathBuf::from("sub").join("b.txt"), 1)
            ]
        );
        assert_eq!(walk_dirs(dir.path()).unwrap(), vec![PathBuf::from("sub")]);
    }
}
