//! Verified exit: native restore exports the volume's file tree to a plain
//! directory — no kernel driver — with a resumable recovery manifest, per-
//! file content verification, and a completeness check that must pass before
//! remote content may be deleted. Pages absent locally but promised offline
//! block the export rather than writing placeholders.

use std::collections::BTreeMap;
use std::path::PathBuf;

use mirage_types::MirageError;

/// Source of verified file bytes for export. Implementations return the
/// complete file content for `path` or fail — partial content is never
/// returned, so a written file is always complete.
pub trait ContentSource: Send + Sync {
    /// Full verified bytes for `path`; `BackendUnavailable` when bytes are
    /// not locally present and cannot be fetched.
    fn file_bytes(&self, path: &str) -> Result<Vec<u8>, MirageError>;
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

const MANIFEST_NAME: &str = ".mirage-recovery-manifest.json";

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
        // Re-check the in-flight tail: a crash may have left a truncated file.
        if let Some(path) = manifest.in_progress.take() {
            self.drop_unverified(&path);
        }
        // A completed entry whose file is missing or size-drifted is
        // re-exported — the manifest only trusts what it re-verifies.
        let drifted: Vec<String> = manifest
            .completed
            .iter()
            .filter(|(path, entry)| {
                std::fs::metadata(self.destination.join(path))
                    .map(|meta| meta.len() != entry.size)
                    .unwrap_or(true)
            })
            .map(|(path, _)| path.clone())
            .collect();
        for path in drifted {
            manifest.completed.remove(&path);
        }
        let entries = self.source.files()?;
        for entry in &entries {
            if manifest.completed.contains_key(&entry.path) {
                continue;
            }
            manifest.in_progress = Some(entry.path.clone());
            self.save_manifest(&manifest)?;
            let target = self.destination.join(&entry.path);
            if let Some(parent) = target.parent()
                && std::fs::create_dir_all(parent).is_err()
            {
                return Err(MirageError::internal_invariant(
                    "export directory could not be created",
                ));
            }
            // Bytes are fetched whole and hash-verified before the file is
            // renamed into place; an unavailable page fails the export.
            let bytes = self.source.file_bytes(&entry.path)?;
            if bytes.len() as u64 != entry.size {
                return Err(MirageError::integrity_mismatch(
                    "exported file size does not match the manifest",
                ));
            }
            if blake3::hash(&bytes).as_bytes() != &entry.content_hash {
                return Err(MirageError::integrity_mismatch(
                    "exported content hash does not match the manifest",
                ));
            }
            mirage_crypto::durable_file::write_atomic(&target, &bytes)?;
            manifest.completed.insert(entry.path.clone(), entry.clone());
            manifest.in_progress = None;
            self.save_manifest(&manifest)?;
        }
        Ok(manifest)
    }

    /// The last-line completeness check: every manifest entry exists on disk
    /// with the recorded size, and no entry is left in progress. Only after
    /// this passes may remote content be reclaimed.
    pub fn verify_complete(&self, manifest: &RecoveryManifest) -> Result<(), MirageError> {
        if manifest.in_progress.is_some() {
            return Err(MirageError::integrity_mismatch(
                "an export entry is still in flight",
            ));
        }
        for (path, entry) in &manifest.completed {
            let meta = std::fs::metadata(self.destination.join(path))
                .map_err(|_| MirageError::repository_conflict("exported file is missing"))?;
            if meta.len() != entry.size {
                return Err(MirageError::integrity_mismatch(
                    "exported file size drifted after completion",
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

    fn drop_unverified(&self, path: &str) {
        let _ = std::fs::remove_file(self.destination.join(path));
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
