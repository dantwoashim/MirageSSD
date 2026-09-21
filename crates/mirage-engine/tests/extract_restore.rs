//! Standalone verified restore over a committed manifest: streaming,
//! resumable, hash-verified, and refusing to touch pre-existing files.

use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use futures_executor::block_on;
use mirage_backend::{
    BackendError, BackendRead, DeletionProof, FetchClass, ObjectBackend, ObjectKind, ObjectStat,
    RemoteObjectRef, UploadSource,
};
use mirage_backend_local::LocalObjectBackend;
use mirage_engine::native_restore::NativeRestore;
use mirage_engine::{ManifestContentSource, publish_base_generation, recover_repository};
use mirage_manifest::{FileClass, InMemoryTestSigner, RepositoryManifest};
use mirage_pack::{CompletedPack, PackReader};
use mirage_types::{BackendHealthState, CheckedRange, ContentHash, GenerationId, RepositoryId};
use tokio_util::sync::CancellationToken;

fn repository_id() -> RepositoryId {
    RepositoryId::from_bytes([0x42; 16])
}

fn signer() -> InMemoryTestSigner {
    InMemoryTestSigner::new([1; 16], [2; 32])
}

/// Imports `files` and publishes them to a fresh local backend, returning the
/// recovered manifest the CLI would extract from.
fn publish_fixture(
    files: &[(&str, Vec<u8>)],
) -> (tempfile::TempDir, LocalObjectBackend, RepositoryManifest) {
    let directory = tempfile::tempdir().expect("directory");
    let source_root = directory.path().join("source");
    std::fs::create_dir_all(&source_root).expect("source root");
    let mut planned = Vec::new();
    for (name, bytes) in files {
        std::fs::write(source_root.join(name), bytes).expect("source file");
        planned.push(mirage_pack::PlannedFile {
            relative_path: (*name).to_string(),
            class: FileClass::VirtualContainer,
        });
    }
    let imported = mirage_pack::import_local(&mirage_pack::ImportPlan {
        repository_id: repository_id(),
        generation_id: GenerationId::ZERO,
        source_root,
        files: planned,
        page_size: 64 * 1024,
        pack_target: 512 * 1024,
        output_staging_directory: directory.path().join("import"),
        encryption: None,
    })
    .expect("import");
    let mut packs = Vec::new();
    for entry in std::fs::read_dir(directory.path().join("import"))
        .expect("import dir")
        .filter_map(Result::ok)
    {
        let path = entry.path();
        if path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("pack-") && name.ends_with(".bin"))
        {
            let reader = PackReader::open_verified(&path).expect("pack reader");
            packs.push(CompletedPack {
                path,
                content_hash: reader.content_hash(),
                byte_length: reader.file_length(),
                entries: reader.entries().to_vec(),
            });
        }
    }
    let backend = LocalObjectBackend::open(&directory.path().join("backend"), repository_id())
        .expect("backend");
    block_on(publish_base_generation(
        &backend,
        &packs,
        &imported.manifest,
        &signer(),
    ))
    .expect("publish");
    let recovered =
        block_on(recover_repository(&backend, repository_id(), &signer())).expect("recover");
    (directory, backend, recovered.manifest)
}

/// Backend that fails its Nth `read_range` (0-based) so a restore can be
/// interrupted deterministically mid-file.
struct FailAfterBackend<B> {
    inner: B,
    calls: AtomicUsize,
    fail_at: usize,
}

#[async_trait]
impl<B: ObjectBackend> ObjectBackend for FailAfterBackend<B> {
    fn capabilities(&self) -> mirage_backend::BackendCapabilities {
        self.inner.capabilities()
    }

    async fn read_range(
        &self,
        object: &RemoteObjectRef,
        range: CheckedRange,
        class: FetchClass,
        cancel: CancellationToken,
    ) -> Result<BackendRead, BackendError> {
        if self.calls.fetch_add(1, Ordering::SeqCst) == self.fail_at {
            return Err(BackendError::new(
                mirage_backend::BackendErrorClass::TransientTransport,
                "injected read failure",
            ));
        }
        self.inner.read_range(object, range, class, cancel).await
    }

    async fn put_immutable(
        &self,
        kind: ObjectKind,
        source: UploadSource,
        expected_hash: ContentHash,
        cancel: CancellationToken,
    ) -> Result<RemoteObjectRef, BackendError> {
        self.inner
            .put_immutable(kind, source, expected_hash, cancel)
            .await
    }

    async fn stat(&self, object: &RemoteObjectRef) -> Result<ObjectStat, BackendError> {
        self.inner.stat(object).await
    }

    async fn enumerate_commits(
        &self,
        repository: RepositoryId,
    ) -> Result<Vec<RemoteObjectRef>, BackendError> {
        self.inner.enumerate_commits(repository).await
    }

    async fn delete_immutable(
        &self,
        object: &RemoteObjectRef,
        proof: &DeletionProof,
        cancel: CancellationToken,
    ) -> Result<(), BackendError> {
        self.inner.delete_immutable(object, proof, cancel).await
    }

    async fn health(&self) -> BackendHealthState {
        self.inner.health().await
    }
}

#[test]
fn manifest_restore_streams_exact_bytes_and_resumes_after_interruption() {
    let first: Vec<u8> = (0..140_000).map(|i| (i % 97) as u8).collect();
    let second: Vec<u8> = (0..70_000).map(|i| (i * 7 % 131) as u8).collect();
    let (dir, backend, manifest) =
        publish_fixture(&[("a.bin", first.clone()), ("b.bin", second.clone())]);
    let destination = tempfile::tempdir().expect("destination");

    // Interrupt the restore partway through the second file: the manifest
    // hash pre-pass reads every page once, then a.bin (3 pages) is copied
    // and b.bin fails on its first page.
    let total_pages = manifest.pages.len();
    let failing_backend =
        LocalObjectBackend::open(&dir.path().join("backend"), repository_id()).expect("backend");
    let failing = FailAfterBackend {
        inner: failing_backend,
        calls: AtomicUsize::new(0),
        fail_at: total_pages + 3,
    };
    let source = ManifestContentSource::new(&manifest, &failing, None).expect("source");
    let restore = NativeRestore::new(&source, destination.path());
    restore
        .run()
        .expect_err("injected failure must abort the run");
    // a.bin completed before the injected failure and is recorded durably.
    assert_eq!(
        std::fs::read(destination.path().join("a.bin")).expect("a.bin"),
        first
    );

    // Resume over a healthy backend: the completed file is skipped, the
    // interrupted file is re-exported, and the completeness check passes.
    let source = ManifestContentSource::new(&manifest, &backend, None).expect("source");
    let restore = NativeRestore::new(&source, destination.path());
    let recovered = restore.run().expect("resumed run");
    restore.verify_complete(&recovered).expect("complete");
    assert_eq!(
        std::fs::read(destination.path().join("b.bin")).expect("b.bin"),
        second
    );
}

#[test]
fn manifest_restore_refuses_to_overwrite_preexisting_files() {
    let (_dir, backend, manifest) = publish_fixture(&[("keep.bin", b"payload".to_vec())]);
    let destination = tempfile::tempdir().expect("destination");
    let original = b"user content that must survive";
    std::fs::write(destination.path().join("keep.bin"), original).expect("preexisting");
    let source = ManifestContentSource::new(&manifest, &backend, None).expect("source");
    let restore = NativeRestore::new(&source, destination.path());
    let error = restore.run().expect_err("preexisting file must refuse");
    assert_eq!(
        error.kind,
        mirage_types::MirageErrorKind::RepositoryConflict
    );
    assert_eq!(
        std::fs::read(destination.path().join("keep.bin")).expect("untouched"),
        original
    );
}
