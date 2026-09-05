use async_trait::async_trait;
use mirage_predictor::capsule::CapsulePlan;
use mirage_types::{GenerationId, MirageError, SessionId};
use roaring::RoaringBitmap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdmittedSession {
    pub session_id: SessionId,
    pub generation: GenerationId,
    pub pinned_pages: u64,
}
#[async_trait]
pub trait AdmissionStore: Send + Sync {
    fn generation(&self) -> GenerationId;
    async fn state_allows_admission(&self) -> Result<bool, MirageError>;
    async fn all_verified_resident(&self, pages: &RoaringBitmap) -> Result<bool, MirageError>;
    async fn create_session_and_leases(&self, plan: &CapsulePlan)
    -> Result<SessionId, MirageError>;
    async fn apply_memory_pins(
        &self,
        session: SessionId,
        pages: &RoaringBitmap,
    ) -> Result<(), MirageError>;
    async fn provider_ready(&self) -> Result<bool, MirageError>;
    async fn mark_sealed_ready(&self, session: SessionId) -> Result<(), MirageError>;
}
pub async fn admit_sealed_session(
    plan: &CapsulePlan,
    store: &dyn AdmissionStore,
) -> Result<AdmittedSession, MirageError> {
    let generation = store.generation();
    if generation != plan.generation {
        return Err(MirageError::repository_conflict(
            "capsule generation is not mounted",
        ));
    }
    if !store.state_allows_admission().await? {
        return Err(MirageError::update_active(
            "repository update or recovery blocks admission",
        ));
    }
    if !store.all_verified_resident(&plan.page_set).await? {
        return Err(MirageError::integrity_mismatch(
            "capsule contains a missing or corrupt page",
        ));
    }
    let session = store.create_session_and_leases(plan).await?;
    store.apply_memory_pins(session, &plan.page_set).await?;
    if store.generation() != generation {
        return Err(MirageError::repository_conflict(
            "generation changed during admission",
        ));
    }
    if !store.provider_ready().await? && !store.all_verified_resident(&plan.page_set).await? {
        return Err(MirageError::backend_unavailable(
            "origin is offline and capsule is incomplete",
        ));
    }
    store.mark_sealed_ready(session).await?;
    Ok(AdmittedSession {
        session_id: session,
        generation,
        pinned_pages: plan.page_set.len(),
    })
}
