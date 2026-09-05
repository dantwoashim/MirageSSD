use std::sync::Arc;

use mirage_backend::{ObjectBackend, ObjectKind};
use mirage_backend_local::LocalObjectBackend;
use mirage_engine::{publish_base_generation, recover_repository};
use mirage_manifest::{FileClass, InMemoryTestSigner};
use mirage_pack::{ImportPlan, PlannedFile, import_local};
use mirage_types::{GenerationId, RepositoryId};

fn imported(
    source_root: &std::path::Path,
    output: &std::path::Path,
    repository: RepositoryId,
) -> mirage_pack::ImportedRepository {
    std::fs::write(source_root.join("asset.pak"), vec![0x5a; 192 * 1024]).expect("source asset");
    import_local(&ImportPlan {
        repository_id: repository,
        generation_id: GenerationId::ZERO,
        source_root: source_root.to_path_buf(),
        files: vec![PlannedFile {
            relative_path: "asset.pak".into(),
            class: FileClass::VirtualContainer,
        }],
        page_size: 64 * 1024,
        pack_target: 150 * 1024,
        output_staging_directory: output.to_path_buf(),
        encryption: None,
    })
    .expect("import")
}

#[test]
fn publication_orders_objects_and_is_idempotent() {
    let repository = RepositoryId::from_bytes([0x32; 16]);
    let source = tempfile::tempdir().expect("source");
    let import_parent = tempfile::tempdir().expect("import parent");
    let import_output = import_parent.path().join("import");
    let imported = imported(source.path(), &import_output, repository);
    let backend_root = tempfile::tempdir().expect("backend root");
    let backend = LocalObjectBackend::open(backend_root.path(), repository).expect("backend");
    let signer = InMemoryTestSigner::new([7; 16], [9; 32]);
    let first = futures_executor::block_on(publish_base_generation(
        &backend,
        &imported.packs,
        &imported.manifest,
        &signer,
    ))
    .expect("publish");
    let second = futures_executor::block_on(publish_base_generation(
        &backend,
        &imported.packs,
        &imported.manifest,
        &signer,
    ))
    .expect("idempotent publish");
    assert_eq!(first.commit, second.commit);
    assert_eq!(first.manifest, second.manifest);
    assert_eq!(first.packs, second.packs);
    for object in first.packs.iter().chain([&first.manifest, &first.commit]) {
        futures_executor::block_on(backend.stat(object)).expect("published object verifies");
    }
}

#[test]
fn fresh_backend_recovers_after_import_workspace_is_removed() {
    let repository = RepositoryId::from_bytes([0x33; 16]);
    let source = tempfile::tempdir().expect("source");
    let import_parent = tempfile::tempdir().expect("import parent");
    let import_output = import_parent.path().join("import");
    let imported = imported(source.path(), &import_output, repository);
    let backend_root = tempfile::tempdir().expect("backend root");
    let backend = LocalObjectBackend::open(backend_root.path(), repository).expect("backend");
    let signer = InMemoryTestSigner::new([3; 16], [4; 32]);
    let published = futures_executor::block_on(publish_base_generation(
        &backend,
        &imported.packs,
        &imported.manifest,
        &signer,
    ))
    .expect("publish");
    drop(imported);
    std::fs::remove_dir_all(&import_output).expect("remove import workspace");
    let reopened =
        LocalObjectBackend::open(backend_root.path(), repository).expect("reopen backend");
    let recovered = futures_executor::block_on(recover_repository(&reopened, repository, &signer))
        .expect("recover");
    assert_eq!(recovered.head_hash, published.commit_hash);
    assert_eq!(recovered.manifest.repository_id, repository);
    assert!(recovered.verified_pack_count > 0);
}

#[test]
fn commit_is_never_visible_before_every_referenced_object() {
    let repository = RepositoryId::from_bytes([0x34; 16]);
    let source = tempfile::tempdir().expect("source");
    let import_parent = tempfile::tempdir().expect("import parent");
    let imported = imported(
        source.path(),
        &import_parent.path().join("import"),
        repository,
    );
    let signer = InMemoryTestSigner::new([5; 16], [6; 32]);
    for fail_put in 1..=(imported.packs.len() + 1) {
        let backend_root = tempfile::tempdir().expect("backend root");
        let inner =
            Arc::new(LocalObjectBackend::open(backend_root.path(), repository).expect("backend"));
        let backend = FailPutBackend::new(Arc::clone(&inner), fail_put);
        assert!(
            futures_executor::block_on(publish_base_generation(
                &backend,
                &imported.packs,
                &imported.manifest,
                &signer
            ))
            .is_err()
        );
        let commits =
            futures_executor::block_on(inner.enumerate_commits(repository)).expect("enumerate");
        assert!(
            commits.is_empty(),
            "commit visible after failed prerequisite put {fail_put}"
        );
    }
}

struct FailPutBackend {
    inner: Arc<LocalObjectBackend>,
    fail_at: usize,
    puts: std::sync::atomic::AtomicUsize,
}

impl FailPutBackend {
    fn new(inner: Arc<LocalObjectBackend>, fail_at: usize) -> Self {
        Self {
            inner,
            fail_at,
            puts: std::sync::atomic::AtomicUsize::new(0),
        }
    }
}

#[async_trait::async_trait]
impl ObjectBackend for FailPutBackend {
    async fn read_range(
        &self,
        object: &mirage_backend::RemoteObjectRef,
        range: mirage_types::CheckedRange,
        class: mirage_backend::FetchClass,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Result<mirage_backend::BackendRead, mirage_backend::BackendError> {
        self.inner.read_range(object, range, class, cancel).await
    }
    async fn put_immutable(
        &self,
        kind: ObjectKind,
        source: mirage_backend::UploadSource,
        hash: mirage_types::ContentHash,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Result<mirage_backend::RemoteObjectRef, mirage_backend::BackendError> {
        let call = self.puts.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
        if call == self.fail_at {
            return Err(mirage_backend::BackendError::new(
                mirage_backend::BackendErrorClass::TransientTransport,
                "injected publication failure",
            ));
        }
        self.inner.put_immutable(kind, source, hash, cancel).await
    }
    async fn stat(
        &self,
        object: &mirage_backend::RemoteObjectRef,
    ) -> Result<mirage_backend::ObjectStat, mirage_backend::BackendError> {
        self.inner.stat(object).await
    }
    async fn enumerate_commits(
        &self,
        repository: RepositoryId,
    ) -> Result<Vec<mirage_backend::RemoteObjectRef>, mirage_backend::BackendError> {
        self.inner.enumerate_commits(repository).await
    }
    async fn delete_immutable(
        &self,
        object: &mirage_backend::RemoteObjectRef,
        proof: &mirage_backend::DeletionProof,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Result<(), mirage_backend::BackendError> {
        self.inner.delete_immutable(object, proof, cancel).await
    }
    async fn health(&self) -> mirage_types::BackendHealthState {
        self.inner.health().await
    }
}
