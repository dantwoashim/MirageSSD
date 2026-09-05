use async_trait::async_trait;
use mirage_predictor::capsule::CapsulePlan;
use mirage_types::{GenerationId, MirageError};
use roaring::RoaringBitmap;
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MaterializeProgress {
    pub total_pages: u64,
    pub already_resident: u64,
    pub downloaded_pages: u64,
    pub verified_pages: u64,
    pub failed_pages: u64,
    pub downloaded_bytes: u64,
}

#[async_trait]
pub trait CapsulePageStore: Send + Sync {
    fn generation(&self) -> GenerationId;
    async fn reserve_all(&self, pages: &RoaringBitmap) -> Result<(), MirageError>;
    async fn is_verified_resident(&self, page: u32) -> Result<bool, MirageError>;
    async fn fetch_verify_commit(
        &self,
        page: u32,
        mandatory: bool,
        cancel: &CancellationToken,
    ) -> Result<u64, MirageError>;
    async fn checkpoint(&self, progress: MaterializeProgress) -> Result<(), MirageError>;
}

pub async fn materialize_capsule(
    plan: &CapsulePlan,
    store: &dyn CapsulePageStore,
    cancel: &CancellationToken,
) -> Result<MaterializeProgress, MirageError> {
    if store.generation() != plan.generation {
        return Err(MirageError::repository_conflict(
            "capsule generation is no longer mounted",
        ));
    }
    store.reserve_all(&plan.page_set).await?;
    let mut progress = MaterializeProgress {
        total_pages: plan.page_set.len(),
        ..Default::default()
    };
    let optional = &plan.page_set - &plan.mandatory_set;
    for (mandatory, pages) in [(true, &plan.mandatory_set), (false, &optional)] {
        for page in pages {
            if cancel.is_cancelled() {
                store.checkpoint(progress).await?;
                return Err(MirageError::cancelled("capsule materialization cancelled"));
            }
            if store.is_verified_resident(page).await? {
                progress.already_resident += 1;
                progress.verified_pages += 1;
            } else {
                match store.fetch_verify_commit(page, mandatory, cancel).await {
                    Ok(bytes) => {
                        progress.downloaded_pages += 1;
                        progress.downloaded_bytes = progress.downloaded_bytes.saturating_add(bytes);
                        progress.verified_pages += 1;
                    }
                    Err(error) => {
                        progress.failed_pages += 1;
                        store.checkpoint(progress).await?;
                        return Err(error);
                    }
                }
            }
            store.checkpoint(progress).await?;
        }
    }
    if progress.verified_pages != progress.total_pages {
        return Err(MirageError::integrity_mismatch(
            "capsule materializer finished with missing pages",
        ));
    }
    if store.generation() != plan.generation {
        return Err(MirageError::repository_conflict(
            "generation changed during capsule materialization",
        ));
    }
    Ok(progress)
}
