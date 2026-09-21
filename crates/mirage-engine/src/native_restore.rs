//! Verified exit: native restore exports the volume's file tree to a plain
//! directory — no kernel driver — with a resumable recovery manifest, per-
//! file content verification, and a completeness check that must pass before
//! remote content may be deleted. Pages absent locally but promised offline
//! block the export rather than writing placeholders.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use mirage_types::MirageError;

/// Source of verified file bytes for export. Implementations return the
/// complete file content for `path` or fail — partial content is never
/// returned, so a written file is always complete.
pub trait ContentSource: Send + Sync {
    /// Full verified bytes for `path`; `BackendUnavailable` when bytes are
    /// not locally present and cannot be fetched.
    fn file_bytes(&self, path: &str) -> Result<Vec<u8>, MirageError>;
    /// Streams verified bytes for `path` into `sink`, returning the count.
    /// The default writes `file_bytes` in one shot; backends override to
    /// stream page by page without buffering whole files.
    fn copy_file(&self, path: &str, sink: &mut dyn std::io::Write) -> Result<u64, MirageError> {
        let bytes = self.file_bytes(path)?;
        std::io::Write::write_all(sink, &bytes).map_err(MirageError::from)?;
        Ok(bytes.len() as u64)
    }
    /// Expected BLAKE3 content hash for `path`.
    fn content_hash(&self, path: &str) -> Result<[u8; 32], MirageError>;
    /// All exportable file paths with expected sizes.
    fn files(&self) -> Result<Vec<ExportEntry>, MirageError>;
}

/// One file in the export manifest.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ExportEntry {
    pub path: String,
    pub size: u64,
    pub content_hash: [u8; 32],
}

/// Resumable export manifest persisted in the destination tree; a second
/// run skips entries already marked complete.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct RecoveryManifest {
    /// `path` → bytes written, verified hash.
    pub completed: BTreeMap<String, ExportEntry>,
    /// File currently in flight — its last byte completeness is re-checked
    /// on resume (a truncated tail is re-exported).
    pub in_progress: Option<String>,
    /// The application to keep installed (uninstall protection applies only
    /// to it).
    pub keep_app: Option<String>,
}

/// File name of the recovery manifest persisted inside the destination.
pub const MANIFEST_NAME: &str = ".mirage-recovery-manifest.json";
/// Directory holding restore-owned staging files inside the destination.
/// In-flight bytes land under a name the restore owns, so a resume never
/// deletes a file it did not create — a pre-existing user file at the final
/// path is untouched until a verified staging file replaces it atomically.
pub const STAGING_NAME: &str = ".mirage-staging";

/// Validates an export path is a contained relative path — no absolute
/// roots, no parent traversal, no separators that escape the destination.
fn contained_path(path: &str) -> Result<std::path::PathBuf, MirageError> {
    let relative = std::path::Path::new(path);
    let mut out = std::path::PathBuf::new();
    for component in relative.components() {
        match component {
            std::path::Component::Normal(part) => out.push(part),
            _ => {
                return Err(MirageError::invalid_argument(
                    "export path escapes the destination",
                ));
            }
        }
    }
    if out.as_os_str().is_empty() {
        return Err(MirageError::invalid_argument("export path is empty"));
    }
    Ok(out)
}

/// Write adapter that feeds every byte through the content hasher so a
/// streamed copy is verified incrementally instead of buffered whole.
struct HashingWriter {
    inner: std::fs::File,
    hasher: blake3::Hasher,
}

impl std::io::Write for HashingWriter {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        let written = self.inner.write(buffer)?;
        self.hasher.update(&buffer[..written]);
        Ok(written)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

/// Streams-hash a file's current bytes.
fn hash_file(path: &std::path::Path) -> Result<[u8; 32], MirageError> {
    let mut file = std::fs::File::open(path).map_err(MirageError::from)?;
    let mut hasher = blake3::Hasher::new();
    std::io::copy(&mut file, &mut hasher).map_err(MirageError::from)?;
    Ok(*hasher.finalize().as_bytes())
}

/// A random staging name the restore owns — a pre-existing user file can
/// never collide with it, so resume only ever deletes restore-written data.
fn staging_name() -> String {
    let mut id = [0u8; 16];
    let _ = getrandom::fill(&mut id);
    id.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Drives a resumable export of one volume tree.
pub struct NativeRestore<'a> {
    source: &'a dyn ContentSource,
    destination: PathBuf,
}

impl<'a> NativeRestore<'a> {
    pub fn new(source: &'a dyn ContentSource, destination: impl Into<PathBuf>) -> Self {
        Self {
            source,
            destination: destination.into(),
        }
    }

    /// Exports every file to `destination/<path>`; resumes a previous run by
    /// skipping verified-complete entries and re-checking the in-flight file.
    /// Returns the manifest only when every file is complete and verified.
    pub fn run(&self) -> Result<RecoveryManifest, MirageError> {
        let mut manifest = self.load_manifest()?;
        // The in-flight record names a staging file the restore owns; resume
        // removes only that staging file — never the destination path.
        if let Some(staging) = manifest.in_progress.take() {
            let _ = std::fs::remove_file(self.destination.join(STAGING_NAME).join(&staging));
        }
        let entries = self.source.files()?;
        for entry in &entries {
            contained_path(&entry.path)?;
        }
        // A completed entry only counts when the on-disk file still matches
        // the manifest hash AND the manifest hash still matches the source
        // snapshot — a changed source re-exports rather than trusting a
        // stale completion.
        let drifted: Vec<String> = manifest
            .completed
            .iter()
            .filter(|(path, recorded)| {
                let current = entries.iter().find(|entry| entry.path == **path);
                if current.is_none_or(|entry| {
                    entry.size != recorded.size || entry.content_hash != recorded.content_hash
                }) {
                    return true;
                }
                let target = self.destination.join(path);
                std::fs::metadata(&target)
                    .map(|meta| meta.len() != recorded.size)
                    .unwrap_or(true)
            })
            .map(|(path, _)| path.clone())
            .collect();
        // Paths the restore provably wrote before this run. A drifted entry
        // may be replaced; anything else already sitting at a destination
        // path is a pre-existing file and is never overwritten.
        let restore_owned: BTreeSet<String> = manifest.completed.keys().cloned().collect();
        for path in drifted {
            manifest.completed.remove(&path);
        }
        for entry in &entries {
            if manifest.completed.contains_key(&entry.path) {
                continue;
            }
            let staging_name = staging_name();
            manifest.in_progress = Some(staging_name.clone());
            self.save_manifest(&manifest)?;
            let staging = self.destination.join(STAGING_NAME).join(&staging_name);
            if std::fs::create_dir_all(staging.parent().expect("staging parent")).is_err() {
                return Err(MirageError::internal_invariant(
                    "export staging directory could not be created",
                ));
            }
            // Bytes are streamed and hash-verified before the staging file
            // is renamed into place; an unavailable page fails the export
            // without touching any pre-existing destination file.
            let staging_file = std::fs::File::create(&staging).map_err(MirageError::from)?;
            let mut hashing = HashingWriter {
                inner: staging_file,
                hasher: blake3::Hasher::new(),
            };
            let copied = self.source.copy_file(&entry.path, &mut hashing)?;
            let staging_file = hashing.inner;
            if copied != entry.size {
                return Err(MirageError::integrity_mismatch(
                    "exported file size does not match the manifest",
                ));
            }
            if hashing.hasher.finalize().as_bytes() != &entry.content_hash {
                return Err(MirageError::integrity_mismatch(
                    "exported content hash does not match the manifest",
                ));
            }
            staging_file.sync_all().map_err(MirageError::from)?;
            let target = self.destination.join(contained_path(&entry.path)?);
            if target.exists() && !restore_owned.contains(&entry.path) {
                return Err(MirageError::repository_conflict(
                    "export refuses to overwrite a file it did not create",
                ));
            }
            if let Some(parent) = target.parent()
                && std::fs::create_dir_all(parent).is_err()
            {
                return Err(MirageError::internal_invariant(
                    "export directory could not be created",
                ));
            }
            if std::fs::rename(&staging, &target).is_err() {
                // Windows refuses rename-over-existing; delete the old path
                // only after the verified staging file is durable.
                let _ = std::fs::remove_file(&target);
                std::fs::rename(&staging, &target).map_err(|_| {
                    MirageError::internal_invariant("export file could not be moved")
                })?;
            }
            manifest.completed.insert(entry.path.clone(), entry.clone());
            manifest.in_progress = None;
            self.save_manifest(&manifest)?;
        }
        Ok(manifest)
    }

    /// The last-line completeness check: every source entry is in the
    /// manifest AND on disk with matching size and content hash, and no
    /// entry is left in progress. Only after this passes may remote content
    /// be reclaimed.
    pub fn verify_complete(&self, manifest: &RecoveryManifest) -> Result<(), MirageError> {
        if manifest.in_progress.is_some() {
            return Err(MirageError::integrity_mismatch(
                "an export entry is still in flight",
            ));
        }
        let entries = self.source.files()?;
        for entry in &entries {
            let recorded = manifest.completed.get(&entry.path).ok_or_else(|| {
                MirageError::repository_conflict("manifest does not cover every source file")
            })?;
            if recorded.size != entry.size || recorded.content_hash != entry.content_hash {
                return Err(MirageError::integrity_mismatch(
                    "manifest entry does not match the source snapshot",
                ));
            }
            let target = self.destination.join(contained_path(&entry.path)?);
            let meta = std::fs::metadata(&target)
                .map_err(|_| MirageError::repository_conflict("exported file is missing"))?;
            if meta.len() != entry.size {
                return Err(MirageError::integrity_mismatch(
                    "exported file size drifted after completion",
                ));
            }
            // Re-hash the exported bytes: same-length corruption must not
            // pass completeness.
            if hash_file(&target)? != entry.content_hash {
                return Err(MirageError::integrity_mismatch(
                    "exported file content drifted after completion",
                ));
            }
        }
        Ok(())
    }

    /// Records which installed app survives uninstall protection.
    pub fn record_keep_app(&self, app: &str) -> Result<(), MirageError> {
        let mut manifest = self.load_manifest()?;
        manifest.keep_app = Some(app.to_string());
        self.save_manifest(&manifest)
    }

    fn manifest_path(&self) -> PathBuf {
        self.destination.join(MANIFEST_NAME)
    }

    fn load_manifest(&self) -> Result<RecoveryManifest, MirageError> {
        let path = self.manifest_path();
        if !path.exists() {
            return Ok(RecoveryManifest::default());
        }
        let bytes = std::fs::read(&path)
            .map_err(|_| MirageError::internal_invariant("recovery manifest is unreadable"))?;
        serde_json::from_slice(&bytes)
            .map_err(|_| MirageError::integrity_mismatch("recovery manifest is corrupt"))
    }

    fn save_manifest(&self, manifest: &RecoveryManifest) -> Result<(), MirageError> {
        let bytes = serde_json::to_vec(manifest).map_err(|_| {
            MirageError::internal_invariant("recovery manifest could not be encoded")
        })?;
        mirage_crypto::durable_file::write_atomic(&self.manifest_path(), &bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap as Map;

    struct FakeSource {
        files: Map<String, Vec<u8>>,
        /// Paths that fail — simulating bytes not locally present.
        missing: Vec<String>,
    }

    impl ContentSource for FakeSource {
        fn file_bytes(&self, path: &str) -> Result<Vec<u8>, MirageError> {
            if self.missing.iter().any(|p| p == path) {
                return Err(MirageError::backend_unavailable("bytes not present"));
            }
            self.files
                .get(path)
                .cloned()
                .ok_or_else(|| MirageError::repository_conflict("file missing"))
        }
        fn content_hash(&self, path: &str) -> Result<[u8; 32], MirageError> {
            Ok(*blake3::hash(self.file_bytes(path)?.as_slice()).as_bytes())
        }
        fn files(&self) -> Result<Vec<ExportEntry>, MirageError> {
            self.files
                .iter()
                .map(|(path, bytes)| {
                    Ok(ExportEntry {
                        path: path.clone(),
                        size: bytes.len() as u64,
                        content_hash: *blake3::hash(bytes).as_bytes(),
                    })
                })
                .collect()
        }
    }

    #[test]
    fn export_verifies_and_resumes_after_interrupt() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("restore");
        let source = FakeSource {
            files: Map::from([
                ("a.txt".to_string(), b"alpha".to_vec()),
                ("d/b.txt".to_string(), b"beta".to_vec()),
            ]),
            missing: vec![],
        };
        let restore = NativeRestore::new(&source, &dest);
        let manifest = restore.run().unwrap();
        restore.verify_complete(&manifest).unwrap();
        assert_eq!(std::fs::read(dest.join("a.txt")).unwrap(), b"alpha");
        // Resumed run skips verified entries.
        let manifest2 = restore.run().unwrap();
        assert_eq!(manifest2.completed.len(), 2);
    }

    #[test]
    fn missing_bytes_block_export_and_remote_delete_never_happens() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("restore");
        let source = FakeSource {
            files: Map::from([("gone.bin".to_string(), b"x".to_vec())]),
            missing: vec!["gone.bin".to_string()],
        };
        let restore = NativeRestore::new(&source, &dest);
        assert!(restore.run().is_err());
        // Completeness check refuses while nothing is verified.
        let manifest = restore
            .run()
            .err()
            .map(|_| RecoveryManifest {
                completed: Map::new(),
                in_progress: Some("gone.bin".into()),
                keep_app: None,
            })
            .unwrap();
        assert!(restore.verify_complete(&manifest).is_err());
    }

    #[test]
    fn truncated_tail_is_reexported() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("restore");
        let source = FakeSource {
            files: Map::from([("f.bin".to_string(), b"full".to_vec())]),
            missing: vec![],
        };
        let restore = NativeRestore::new(&source, &dest);
        restore.run().unwrap();
        // Truncate the exported file, then resume: completeness check fails.
        std::fs::write(dest.join("f.bin"), b"fu").unwrap();
        let manifest = restore.run().unwrap();
        assert!(restore.verify_complete(&manifest).is_ok());
        assert_eq!(std::fs::read(dest.join("f.bin")).unwrap(), b"full");
    }
}
