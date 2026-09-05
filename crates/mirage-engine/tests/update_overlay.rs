use async_trait::async_trait;
use mirage_engine::update::*;
use mirage_types::{GenerationId, MirageError, RepositoryId, StableFileId, UpdateId};
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;
const PAGE: usize = 65536;
struct Source {
    bytes: Vec<u8>,
    offline: bool,
}
#[async_trait]
impl BasePageSource for Source {
    async fn read_base_page(
        &self,
        _: StableFileId,
        page: u32,
        _: u32,
        _: &CancellationToken,
    ) -> Result<Vec<u8>, MirageError> {
        if self.offline {
            return Err(MirageError::backend_unavailable("offline"));
        }
        let start = page as usize * PAGE;
        Ok(self
            .bytes
            .get(start..(start + PAGE).min(self.bytes.len()))
            .unwrap_or(&[])
            .to_vec())
    }
    fn base_file_size(&self, _: StableFileId) -> Result<u64, MirageError> {
        Ok(self.bytes.len() as u64)
    }
}
#[derive(Default)]
struct Journal {
    batches: Mutex<Vec<Vec<OverlayMutation>>>,
}
#[async_trait]
impl OverlayJournal for Journal {
    async fn persist_batch(
        &self,
        _: UpdateContext,
        m: &[OverlayMutation],
    ) -> Result<(), MirageError> {
        self.batches.lock().unwrap().push(m.to_vec());
        Ok(())
    }
}
fn context() -> UpdateContext {
    UpdateContext {
        update_id: UpdateId::from_bytes([1; 16]),
        repository_id: RepositoryId::from_bytes([2; 16]),
        base_generation: GenerationId(1),
        target_generation: GenerationId(2),
        page_size: PAGE as u32,
    }
}
#[test]
fn randomized_cross_page_writes_match_byte_vector_oracle() {
    let original: Vec<u8> = (0..PAGE * 3).map(|i| (i % 251) as u8).collect();
    let journal = Arc::new(Journal::default());
    let store = OverlayStore::new(
        context(),
        Arc::new(Source {
            bytes: original.clone(),
            offline: false,
        }),
        journal.clone(),
    )
    .unwrap();
    let file = StableFileId::from_u64(7);
    let mut oracle = original;
    for i in 0..200 {
        let offset = (i * 7919 % (PAGE * 3 - 500)) as u64;
        let bytes = vec![(i % 255) as u8; (i * 37 % 499) + 1];
        futures_executor::block_on(store.write_range(
            file,
            offset,
            &bytes,
            &CancellationToken::new(),
        ))
        .unwrap();
        oracle[offset as usize..offset as usize + bytes.len()].copy_from_slice(&bytes);
    }
    let actual = futures_executor::block_on(store.read_range(
        file,
        0,
        oracle.len(),
        &CancellationToken::new(),
    ))
    .unwrap();
    assert_eq!(actual, oracle);
    assert_eq!(journal.batches.lock().unwrap().len(), 200);
}
#[test]
fn full_page_overwrite_skips_offline_base_fetch() {
    let store = OverlayStore::new(
        context(),
        Arc::new(Source {
            bytes: vec![1; PAGE],
            offline: true,
        }),
        Arc::new(Journal::default()),
    )
    .unwrap();
    let file = StableFileId::from_u64(8);
    let bytes = vec![9; PAGE];
    futures_executor::block_on(store.write_range(file, 0, &bytes, &CancellationToken::new()))
        .unwrap();
    assert_eq!(
        futures_executor::block_on(store.read_range(file, 0, PAGE, &CancellationToken::new()))
            .unwrap(),
        bytes
    );
}
