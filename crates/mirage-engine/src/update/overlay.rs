use super::UpdateContext;
use async_trait::async_trait;
use mirage_types::{MirageError, StableFileId};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OverlayMutation {
    pub file_id: StableFileId,
    pub page_index: u32,
    pub bytes: Vec<u8>,
    pub file_size: u64,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WriteOutcome {
    pub bytes_written: u64,
    pub first_page: u32,
    pub page_count: u32,
    pub new_file_size: u64,
}
#[async_trait]
pub trait BasePageSource: Send + Sync {
    async fn read_base_page(
        &self,
        file: StableFileId,
        page: u32,
        page_size: u32,
        cancel: &CancellationToken,
    ) -> Result<Vec<u8>, MirageError>;
    fn base_file_size(&self, file: StableFileId) -> Result<u64, MirageError>;
}
#[async_trait]
pub trait OverlayJournal: Send + Sync {
    async fn persist_batch(
        &self,
        context: UpdateContext,
        mutations: &[OverlayMutation],
    ) -> Result<(), MirageError>;
}
pub struct OverlayStore {
    context: UpdateContext,
    source: Arc<dyn BasePageSource>,
    journal: Arc<dyn OverlayJournal>,
    pages: Mutex<BTreeMap<(StableFileId, u32), Vec<u8>>>,
    sizes: Mutex<BTreeMap<StableFileId, u64>>,
}
impl OverlayStore {
    pub fn new(
        context: UpdateContext,
        source: Arc<dyn BasePageSource>,
        journal: Arc<dyn OverlayJournal>,
    ) -> Result<Self, MirageError> {
        context.validate()?;
        Ok(Self {
            context,
            source,
            journal,
            pages: Mutex::new(BTreeMap::new()),
            sizes: Mutex::new(BTreeMap::new()),
        })
    }
    pub async fn write_range(
        &self,
        file_id: StableFileId,
        offset: u64,
        src: &[u8],
        cancel: &CancellationToken,
    ) -> Result<WriteOutcome, MirageError> {
        if src.is_empty() {
            return Err(MirageError::invalid_argument(
                "update write cannot be empty",
            ));
        }
        if cancel.is_cancelled() {
            return Err(MirageError::cancelled("update write cancelled"));
        }
        let end = offset
            .checked_add(src.len() as u64)
            .ok_or_else(|| MirageError::invalid_argument("update write range overflows"))?;
        let page_size = self.context.page_size as u64;
        let first = (offset / page_size) as u32;
        let last = ((end - 1) / page_size) as u32;
        let old_size = self
            .sizes
            .lock()
            .unwrap()
            .get(&file_id)
            .copied()
            .unwrap_or(self.source.base_file_size(file_id)?);
        let new_size = old_size.max(end);
        let existing = self.pages.lock().unwrap().clone();
        let mut mutations = Vec::new();
        for page in first..=last {
            let page_start = page as u64 * page_size;
            let write_start = offset.max(page_start);
            let write_end = end.min(page_start + page_size);
            let destination_start = (write_start - page_start) as usize;
            let source_start = (write_start - offset) as usize;
            let length = (write_end - write_start) as usize;
            let full = destination_start == 0 && length == page_size as usize;
            let mut bytes = if let Some(value) = existing.get(&(file_id, page)) {
                value.clone()
            } else if full {
                vec![0; page_size as usize]
            } else {
                let mut base = self
                    .source
                    .read_base_page(file_id, page, self.context.page_size, cancel)
                    .await?;
                if base.len() > page_size as usize {
                    return Err(MirageError::integrity_mismatch(
                        "base page exceeds update page size",
                    ));
                }
                base.resize(page_size as usize, 0);
                base
            };
            bytes[destination_start..destination_start + length]
                .copy_from_slice(&src[source_start..source_start + length]);
            mutations.push(OverlayMutation {
                file_id,
                page_index: page,
                bytes,
                file_size: new_size,
            });
        }
        if cancel.is_cancelled() {
            return Err(MirageError::cancelled(
                "update write cancelled before durable commit",
            ));
        }
        self.journal.persist_batch(self.context, &mutations).await?;
        {
            let mut pages = self.pages.lock().unwrap();
            for mutation in &mutations {
                pages.insert((file_id, mutation.page_index), mutation.bytes.clone());
            }
        }
        self.sizes.lock().unwrap().insert(file_id, new_size);
        Ok(WriteOutcome {
            bytes_written: src.len() as u64,
            first_page: first,
            page_count: last - first + 1,
            new_file_size: new_size,
        })
    }
    pub async fn read_range(
        &self,
        file_id: StableFileId,
        offset: u64,
        length: usize,
        cancel: &CancellationToken,
    ) -> Result<Vec<u8>, MirageError> {
        let size = self
            .sizes
            .lock()
            .unwrap()
            .get(&file_id)
            .copied()
            .unwrap_or(self.source.base_file_size(file_id)?);
        let end = offset
            .checked_add(length as u64)
            .ok_or_else(|| MirageError::invalid_argument("update read range overflows"))?;
        if end > size {
            return Err(MirageError::invalid_argument(
                "update read exceeds logical file",
            ));
        }
        let page_size = self.context.page_size as u64;
        let mut output = Vec::with_capacity(length);
        let pages = self.pages.lock().unwrap().clone();
        let mut position = offset;
        while position < end {
            let page = (position / page_size) as u32;
            let page_start = page as u64 * page_size;
            let bytes = if let Some(value) = pages.get(&(file_id, page)) {
                value.clone()
            } else {
                let mut base = self
                    .source
                    .read_base_page(file_id, page, self.context.page_size, cancel)
                    .await?;
                base.resize(page_size as usize, 0);
                base
            };
            let start = (position - page_start) as usize;
            let take = ((end - position) as usize).min(bytes.len() - start);
            output.extend_from_slice(&bytes[start..start + take]);
            position += take as u64;
        }
        Ok(output)
    }
    pub async fn truncate(&self, file_id: StableFileId, new_size: u64) -> Result<(), MirageError> {
        let mutation = OverlayMutation {
            file_id,
            page_index: u32::MAX,
            bytes: Vec::new(),
            file_size: new_size,
        };
        self.journal
            .persist_batch(self.context, std::slice::from_ref(&mutation))
            .await?;
        self.sizes.lock().unwrap().insert(file_id, new_size);
        Ok(())
    }
}
