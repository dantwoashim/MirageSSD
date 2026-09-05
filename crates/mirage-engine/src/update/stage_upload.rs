use super::StagedPack;
use async_trait::async_trait;
use bytes::Bytes;
use mirage_backend::{ObjectBackend, ObjectKind, RemoteObjectRef, UploadSource};
use mirage_types::{MirageError, PageHash, StableFileId};
use tokio_util::sync::CancellationToken;
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagedPageMapping {
    pub file_id: StableFileId,
    pub page_index: u32,
    pub page_hash: PageHash,
    pub object: RemoteObjectRef,
    pub frame_offset: u64,
    pub frame_length: u64,
    pub logical_length: u32,
    pub codec: mirage_manifest::Codec,
}
#[async_trait]
pub trait StagingCatalog: Send + Sync {
    async fn persist_remote_mappings(
        &self,
        mappings: &[StagedPageMapping],
    ) -> Result<(), MirageError>;
}
pub async fn upload_staged_pack(
    backend: &dyn ObjectBackend,
    catalog: &dyn StagingCatalog,
    staged: &StagedPack,
    cancel: CancellationToken,
) -> Result<Vec<StagedPageMapping>, MirageError> {
    if cancel.is_cancelled() {
        return Err(MirageError::cancelled("staging upload cancelled"));
    }
    let bytes = std::fs::read(&staged.pack.path).map_err(MirageError::from)?;
    if bytes.len() as u64 != staged.pack.byte_length
        || blake3::hash(&bytes).as_bytes() != staged.pack.content_hash.as_bytes()
    {
        return Err(MirageError::integrity_mismatch(
            "staging pack changed before upload",
        ));
    }
    let object = backend
        .put_immutable(
            ObjectKind::Pack,
            UploadSource::from_bytes(Bytes::from(bytes)),
            staged.pack.content_hash,
            cancel.clone(),
        )
        .await?;
    let stat = backend.stat(&object).await?;
    if stat.content_hash != staged.pack.content_hash
        || stat.byte_length.as_u64() != staged.pack.byte_length
    {
        return Err(MirageError::integrity_mismatch(
            "uploaded staging pack failed verification",
        ));
    }
    let mut mappings = Vec::with_capacity(staged.pages.len());
    for page in &staged.pages {
        let entry = staged
            .pack
            .entries
            .iter()
            .find(|entry| entry.page_hash == page.hash)
            .ok_or_else(|| MirageError::internal_invariant("staging pack omitted dirty page"))?;
        mappings.push(StagedPageMapping {
            file_id: page.file_id,
            page_index: page.page_index,
            page_hash: page.hash,
            object: object.clone(),
            frame_offset: entry.frame_offset,
            frame_length: entry.frame_length,
            logical_length: entry.logical_length,
            codec: entry.codec,
        });
    }
    catalog.persist_remote_mappings(&mappings).await?;
    Ok(mappings)
}
