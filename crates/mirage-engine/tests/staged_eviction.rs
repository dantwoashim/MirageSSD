use async_trait::async_trait;
use bytes::Bytes;
use mirage_backend_local::LocalObjectBackend;
use mirage_crypto::aead::RepositoryKey;
use mirage_engine::update::*;
use mirage_pack::PackEncryption;
use mirage_types::{MirageError, PageHash, RepositoryId, StableFileId};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;
struct Source {
    pages: BTreeMap<(StableFileId, u32), (u64, Bytes)>,
}
#[async_trait]
impl DirtyPageSource for Source {
    async fn snapshot(
        &self,
        file: StableFileId,
        page: u32,
    ) -> Result<DirtyPageSnapshot, MirageError> {
        let (e, b) = &self.pages[&(file, page)];
        Ok(DirtyPageSnapshot {
            file_id: file,
            page_index: page,
            epoch: *e,
            bytes: b.clone(),
            hash: PageHash::from_bytes(*blake3::hash(b).as_bytes()),
        })
    }
    async fn current_epoch(&self, file: StableFileId, page: u32) -> Result<u64, MirageError> {
        Ok(self.pages[&(file, page)].0)
    }
}
#[derive(Default)]
struct Catalog(Mutex<Vec<StagedPageMapping>>);
#[async_trait]
impl StagingCatalog for Catalog {
    async fn persist_remote_mappings(&self, m: &[StagedPageMapping]) -> Result<(), MirageError> {
        self.0.lock().unwrap().extend_from_slice(m);
        Ok(())
    }
}
#[test]
fn remote_mapping_is_verified_and_persisted_before_release() {
    let file = StableFileId::from_u64(4);
    let source = Source {
        pages: [((file, 0), (1, Bytes::from(vec![8; 65536])))].into(),
    };
    let staging = tempfile::tempdir().unwrap();
    let staged = futures_executor::block_on(stage_stable_pages(
        &source,
        &[(file, 0)],
        staging.path(),
        65536,
        1024 * 1024,
        PackEncryption {
            repository_id: RepositoryId::from_bytes([4; 16]),
            key: Arc::new(RepositoryKey::from_bytes([0x44; 32])),
        },
    ))
    .unwrap();
    let remote = tempfile::tempdir().unwrap();
    let backend =
        LocalObjectBackend::open(remote.path(), RepositoryId::from_bytes([4; 16])).unwrap();
    let catalog = Catalog::default();
    let mappings = futures_executor::block_on(upload_staged_pack(
        &backend,
        &catalog,
        &staged,
        CancellationToken::new(),
    ))
    .unwrap();
    assert_eq!(mappings.len(), 1);
    assert_eq!(catalog.0.lock().unwrap().len(), 1);
    assert!(mappings[0].frame_length > 0);
}
