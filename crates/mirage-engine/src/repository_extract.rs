use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;
use std::sync::Mutex;

use mirage_backend::ObjectBackend;
use mirage_manifest::{FileRecord, RepositoryManifest};
use mirage_pack::{DecodedFrame, EncryptedFrameAad, PackReadEncryption, PlainPage};
use mirage_types::MirageError;
use tokio_util::sync::CancellationToken;

use crate::native_restore::{
    ContentSource, ExportEntry, MANIFEST_NAME, NativeRestore, RecoveryManifest, STAGING_NAME,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExtractReport {
    pub files_written: u64,
    pub bytes_written: u64,
}

/// `ContentSource` over a verified repository manifest: virtual files only,
/// bytes streamed page by page from the backend with per-page hash and
/// length revalidation. The manifest records no whole-file hash, so content
/// hashes are computed by streaming pages once and cached for reuse across
/// `files()`/`content_hash()`/`copy_file` calls.
pub struct ManifestContentSource<'a> {
    manifest: &'a RepositoryManifest,
    backend: &'a dyn ObjectBackend,
    encryption: Option<PackReadEncryption>,
    /// `path` → manifest file index, virtual files only.
    paths: BTreeMap<String, usize>,
    content_hashes: Mutex<BTreeMap<String, [u8; 32]>>,
}

impl<'a> ManifestContentSource<'a> {
    pub fn new(
        manifest: &'a RepositoryManifest,
        backend: &'a dyn ObjectBackend,
        encryption: Option<PackReadEncryption>,
    ) -> Result<Self, MirageError> {
        let mut paths = BTreeMap::new();
        for (index, file) in manifest.files.iter().enumerate() {
            if !file.class.is_virtual() {
                continue;
            }
            let relative = manifest_file_path(manifest, file.parent_directory, &file.name)?;
            paths.insert(relative.to_string_lossy().replace('\\', "/"), index);
        }
        Ok(Self {
            manifest,
            backend,
            encryption,
            paths,
            content_hashes: Mutex::new(BTreeMap::new()),
        })
    }

    fn file(&self, path: &str) -> Result<&'a FileRecord, MirageError> {
        let index = self
            .paths
            .get(path)
            .ok_or_else(|| MirageError::repository_conflict("file missing"))?;
        self.manifest
            .files
            .get(*index)
            .ok_or_else(|| MirageError::manifest_invalid("extract file index is invalid"))
    }

    /// Streams every verified page of `file` into `sink`, returning the byte
    /// count. Identical decode path to the old page-by-page extract.
    fn stream_file(&self, file: &FileRecord, sink: &mut dyn Write) -> Result<u64, MirageError> {
        let extent_start = file.extent_start as usize;
        let extent_end = extent_start
            .checked_add(file.extent_count as usize)
            .ok_or_else(|| MirageError::manifest_invalid("extract extent slice overflows"))?;
        let mut written = 0_u64;
        for extent in &self.manifest.extents[extent_start..extent_end] {
            let page_start = extent.page_start as usize;
            let page_end = page_start
                .checked_add(extent.page_count as usize)
                .ok_or_else(|| MirageError::manifest_invalid("extract page slice overflows"))?;
            for page in &self.manifest.pages[page_start..page_end] {
                let location = &self.manifest.remote_locations[page.remote_location as usize];
                let range = mirage_types::CheckedRange::new(
                    location.offset,
                    location.encoded_length.as_u64(),
                )?;
                let frame = futures_executor::block_on(async {
                    self.backend
                        .read_range(
                            &location.object,
                            range,
                            mirage_backend::FetchClass::Maintenance,
                            CancellationToken::new(),
                        )
                        .await?
                        .collect_bounded(32 * 1024 * 1024)
                        .await
                })?;
                let decoded = decode_frame(
                    &frame,
                    self.manifest,
                    location.offset,
                    page.plaintext_hash,
                    page.logical_length,
                    self.encryption.as_ref(),
                )?;
                if decoded.page.hash != page.plaintext_hash
                    || decoded.page.logical_len != page.logical_length
                {
                    return Err(MirageError::integrity_mismatch(
                        "extracted page differs from manifest",
                    ));
                }
                sink.write_all(&decoded.page.bytes)
                    .map_err(MirageError::from)?;
                written = written
                    .checked_add(decoded.page.bytes.len() as u64)
                    .ok_or_else(|| MirageError::invalid_argument("extract byte count overflows"))?;
            }
        }
        Ok(written)
    }

    fn compute_content_hash(&self, path: &str) -> Result<[u8; 32], MirageError> {
        if let Some(hash) = self
            .content_hashes
            .lock()
            .map_err(|_| MirageError::internal_invariant("content hash cache lock poisoned"))?
            .get(path)
        {
            return Ok(*hash);
        }
        let mut hasher = blake3::Hasher::new();
        self.stream_file(self.file(path)?, &mut hasher)?;
        let hash = *hasher.finalize().as_bytes();
        self.content_hashes
            .lock()
            .map_err(|_| MirageError::internal_invariant("content hash cache lock poisoned"))?
            .insert(path.to_string(), hash);
        Ok(hash)
    }
}

impl ContentSource for ManifestContentSource<'_> {
    fn file_bytes(&self, path: &str) -> Result<Vec<u8>, MirageError> {
        let mut bytes = Vec::new();
        self.copy_file(path, &mut bytes)?;
        Ok(bytes)
    }
    fn copy_file(&self, path: &str, sink: &mut dyn Write) -> Result<u64, MirageError> {
        self.stream_file(self.file(path)?, sink)
    }
    fn content_hash(&self, path: &str) -> Result<[u8; 32], MirageError> {
        self.compute_content_hash(path)
    }
    fn files(&self) -> Result<Vec<ExportEntry>, MirageError> {
        self.paths
            .keys()
            .map(|path| {
                let file = self.file(path)?;
                Ok(ExportEntry {
                    path: path.clone(),
                    size: file.logical_size.as_u64(),
                    content_hash: self.compute_content_hash(path)?,
                })
            })
            .collect()
    }
}

pub fn extract_virtual_files(
    backend: &dyn ObjectBackend,
    manifest: &RepositoryManifest,
    destination: &Path,
) -> Result<ExtractReport, MirageError> {
    extract_virtual_files_with_encryption(backend, manifest, destination, None)
}

/// Verified standalone restore: streams every virtual file through the
/// resumable `NativeRestore` engine — hash-verified staging files, resume
/// after interruption, refusal to overwrite files it did not create, and a
/// final completeness re-check before reporting success.
pub fn extract_virtual_files_with_encryption(
    backend: &dyn ObjectBackend,
    manifest: &RepositoryManifest,
    destination: &Path,
    encryption: Option<&PackReadEncryption>,
) -> Result<ExtractReport, MirageError> {
    std::fs::create_dir_all(destination).map_err(MirageError::from)?;
    let source = ManifestContentSource::new(manifest, backend, encryption.cloned())?;
    // Resume is for interrupted runs only: a destination already holding a
    // fully completed extract is a would-be overwrite and is refused.
    let manifest_path = destination.join(MANIFEST_NAME);
    if manifest_path.exists() {
        let prior: RecoveryManifest =
            serde_json::from_slice(&std::fs::read(&manifest_path).map_err(MirageError::from)?)
                .map_err(|_| MirageError::integrity_mismatch("recovery manifest is corrupt"))?;
        let entries = source.files()?;
        if prior.in_progress.is_none()
            && entries.iter().all(|entry| {
                prior.completed.get(&entry.path).is_some_and(|recorded| {
                    recorded.size == entry.size && recorded.content_hash == entry.content_hash
                })
            })
        {
            return Err(MirageError::repository_conflict(
                "destination already holds a completed extract",
            ));
        }
    }
    let restore = NativeRestore::new(&source, destination);
    let recovery = restore.run()?;
    restore.verify_complete(&recovery)?;
    // The destination must be an ordinary tree: once the extract is
    // verified complete its resume bookkeeping is removed. An empty
    // `.mirage-staging` is expected; a non-empty one means a stale partial
    // file survived and is an error, not a silent delete.
    std::fs::remove_file(&manifest_path).map_err(MirageError::from)?;
    std::fs::remove_dir(destination.join(STAGING_NAME)).map_err(MirageError::from)?;
    Ok(ExtractReport {
        files_written: recovery.completed.len() as u64,
        bytes_written: recovery.completed.values().map(|entry| entry.size).sum(),
    })
}

fn decode_frame(
    frame: &[u8],
    manifest: &RepositoryManifest,
    frame_offset: u64,
    hash: mirage_types::PageHash,
    logical_length: u32,
    encryption: Option<&PackReadEncryption>,
) -> Result<DecodedFrame, MirageError> {
    if !frame.starts_with(b"MENCv001") {
        return mirage_pack::decode_plain_frame(frame);
    }
    let encryption = encryption.ok_or_else(|| {
        MirageError::backend_unauthenticated("encrypted pack requires its repository key")
    })?;
    if encryption.repository_id != manifest.repository_id {
        return Err(MirageError::integrity_mismatch(
            "repository key context does not match manifest",
        ));
    }
    let plaintext = mirage_pack::decode_encrypted_frame(
        &encryption.key,
        frame,
        EncryptedFrameAad {
            repository: manifest.repository_id,
            pack_id: mirage_pack::encrypted_frame_pack_id(frame)?,
            frame_index: frame_offset,
            plaintext_hash: hash,
            plaintext_length: logical_length,
        },
    )?;
    Ok(DecodedFrame {
        page: PlainPage {
            hash,
            logical_len: logical_length,
            bytes: plaintext.into(),
        },
        codec: mirage_manifest::Codec::None,
    })
}

fn manifest_file_path(
    manifest: &RepositoryManifest,
    mut directory: u32,
    name: &str,
) -> Result<std::path::PathBuf, MirageError> {
    let mut components = vec![name.to_string()];
    loop {
        let record = manifest
            .directories
            .get(directory as usize)
            .ok_or_else(|| MirageError::manifest_invalid("extract directory index is invalid"))?;
        if record.parent.is_none() {
            break;
        }
        components.push(record.name.clone());
        directory = record.parent.expect("checked parent");
    }
    components.reverse();
    Ok(components.into_iter().collect())
}
