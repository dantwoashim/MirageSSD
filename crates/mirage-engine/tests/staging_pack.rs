use async_trait::async_trait;
use bytes::Bytes;
use mirage_crypto::aead::RepositoryKey;
use mirage_engine::update::{DirtyPageSnapshot, DirtyPageSource, stage_stable_pages};
use mirage_pack::PackEncryption;
use mirage_types::{MirageError, PageHash, RepositoryId, StableFileId};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

fn encryption() -> PackEncryption {
    PackEncryption {
        repository_id: RepositoryId::from_bytes([9; 16]),
        key: Arc::new(RepositoryKey::from_bytes([0x33; 32])),
    }
}
struct Source {
    pages: BTreeMap<(StableFileId, u32), (u64, Bytes)>,
    race: AtomicBool,
}
#[async_trait]
impl DirtyPageSource for Source {
    async fn snapshot(
        &self,
        file: StableFileId,
        page: u32,
    ) -> Result<DirtyPageSnapshot, MirageError> {
        let (epoch, bytes) = self.pages.get(&(file, page)).unwrap();
        Ok(DirtyPageSnapshot {
            file_id: file,
            page_index: page,
            epoch: *epoch,
            hash: PageHash::from_bytes(*blake3::hash(bytes).as_bytes()),
            bytes: bytes.clone(),
        })
    }
    async fn current_epoch(&self, file: StableFileId, page: u32) -> Result<u64, MirageError> {
        let epoch = self.pages[&(file, page)].0;
        Ok(if self.race.load(Ordering::SeqCst) {
            epoch + 1
        } else {
            epoch
        })
    }
}
#[test]
fn stable_epochs_pack_and_dedupe_pages() {
    let file = StableFileId::from_u64(1);
    let bytes = Bytes::from(vec![7; 65536]);
    let source = Source {
        pages: [((file, 0), (3, bytes.clone())), ((file, 1), (4, bytes))].into(),
        race: AtomicBool::new(false),
    };
    let root = tempfile::tempdir().unwrap();
    let staged = futures_executor::block_on(stage_stable_pages(
        &source,
        &[(file, 1), (file, 0), (file, 0)],
        root.path(),
        65536,
        1024 * 1024,
        encryption(),
    ))
    .unwrap();
    assert_eq!(staged.pages.len(), 2);
    assert_eq!(staged.pack.entries.len(), 1);
}
#[test]
fn epoch_race_rejects_pack_acceptance() {
    let file = StableFileId::from_u64(2);
    let source = Source {
        pages: [((file, 0), (1, Bytes::from(vec![1; 65536])))].into(),
        race: AtomicBool::new(true),
    };
    let root = tempfile::tempdir().unwrap();
    assert!(
        futures_executor::block_on(stage_stable_pages(
            &source,
            &[(file, 0)],
            root.path(),
            65536,
            1024 * 1024,
            encryption(),
        ))
        .is_err()
    );
}
