use async_trait::async_trait;
use bytes::Bytes;
use mirage_pack::{PackEncryption, PackWriter, PackWriterOptions, PlainPage};
use mirage_types::{MirageError, PageHash, StableFileId};
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirtyPageSnapshot {
    pub file_id: StableFileId,
    pub page_index: u32,
    pub epoch: u64,
    pub bytes: Bytes,
    pub hash: PageHash,
}
#[derive(Debug)]
pub struct StagedPack {
    pub pack: mirage_pack::CompletedPack,
    pub pages: Vec<DirtyPageSnapshot>,
}
#[async_trait]
pub trait DirtyPageSource: Send + Sync {
    async fn snapshot(
        &self,
        file: StableFileId,
        page: u32,
    ) -> Result<DirtyPageSnapshot, MirageError>;
    async fn current_epoch(&self, file: StableFileId, page: u32) -> Result<u64, MirageError>;
}
pub async fn stage_stable_pages(
    source: &dyn DirtyPageSource,
    keys: &[(StableFileId, u32)],
    directory: &Path,
    page_size: u32,
    target_size: u64,
    encryption: PackEncryption,
) -> Result<StagedPack, MirageError> {
    if keys.is_empty() {
        return Err(MirageError::invalid_argument(
            "staging requires dirty pages",
        ));
    }
    let mut unique = keys.to_vec();
    unique.sort();
    unique.dedup();
    let mut snapshots = Vec::with_capacity(unique.len());
    for &(file, page) in &unique {
        let snapshot = source.snapshot(file, page).await?;
        if snapshot.file_id != file
            || snapshot.page_index != page
            || snapshot.bytes.is_empty()
            || snapshot.bytes.len() > page_size as usize
            || blake3::hash(&snapshot.bytes).as_bytes() != snapshot.hash.as_bytes()
        {
            return Err(MirageError::integrity_mismatch(
                "dirty page snapshot identity is invalid",
            ));
        }
        snapshots.push(snapshot);
    }
    let mut writer = PackWriter::create_encrypted(
        directory,
        PackWriterOptions {
            page_size,
            target_size,
            align_frames_4k: true,
        },
        encryption,
    )?;
    for snapshot in &snapshots {
        writer.append_page(&PlainPage {
            hash: snapshot.hash,
            logical_len: snapshot.bytes.len() as u32,
            bytes: snapshot.bytes.clone(),
        })?;
    }
    let pack = writer.finish()?;
    for snapshot in &snapshots {
        if source
            .current_epoch(snapshot.file_id, snapshot.page_index)
            .await?
            != snapshot.epoch
        {
            return Err(MirageError::repository_conflict(
                "dirty page changed during staging",
            ));
        }
    }
    Ok(StagedPack {
        pack,
        pages: snapshots,
    })
}
