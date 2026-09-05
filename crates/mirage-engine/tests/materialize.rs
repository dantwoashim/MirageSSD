use async_trait::async_trait;
use mirage_engine::{CapsulePageStore, MaterializeProgress, materialize_capsule};
use mirage_predictor::capsule::*;
use mirage_types::{GenerationId, MirageError, RepositoryId};
use roaring::RoaringBitmap;
use std::collections::BTreeSet;
use std::sync::Mutex;
use tokio_util::sync::CancellationToken;
fn bm(v: &[u32]) -> RoaringBitmap {
    v.iter().copied().collect()
}
fn plan() -> CapsulePlan {
    CapsulePlan::new(CapsuleDraft {
        repository_id: RepositoryId::from_bytes([1; 16]),
        generation: GenerationId(7),
        profile_key: ProfileKey("test".into()),
        page_set: bm(&[1, 2, 3]),
        mandatory_set: bm(&[2]),
        frontier_set: bm(&[3]),
        page_size: 65536,
        risk: RiskEstimate {
            held_out_violation_millionths: 0,
            unseen_branch_mass_millionths: 0,
            data_quality_millionths: 0,
            version_transfer_confidence_millionths: 1_000_000,
        },
        reasons: vec![],
    })
    .unwrap()
}
struct Store {
    resident: Mutex<BTreeSet<u32>>,
    order: Mutex<Vec<(u32, bool)>>,
    checkpoints: Mutex<Vec<MaterializeProgress>>,
}
#[async_trait]
impl CapsulePageStore for Store {
    fn generation(&self) -> GenerationId {
        GenerationId(7)
    }
    async fn reserve_all(&self, pages: &RoaringBitmap) -> Result<(), MirageError> {
        assert_eq!(pages.len(), 3);
        Ok(())
    }
    async fn is_verified_resident(&self, page: u32) -> Result<bool, MirageError> {
        Ok(self.resident.lock().unwrap().contains(&page))
    }
    async fn fetch_verify_commit(
        &self,
        page: u32,
        mandatory: bool,
        _: &CancellationToken,
    ) -> Result<u64, MirageError> {
        self.order.lock().unwrap().push((page, mandatory));
        self.resident.lock().unwrap().insert(page);
        Ok(65536)
    }
    async fn checkpoint(&self, p: MaterializeProgress) -> Result<(), MirageError> {
        self.checkpoints.lock().unwrap().push(p);
        Ok(())
    }
}
#[test]
fn reserves_then_downloads_mandatory_first_and_resumes_resident() {
    let store = Store {
        resident: Mutex::new([1].into()),
        order: Mutex::new(vec![]),
        checkpoints: Mutex::new(vec![]),
    };
    let progress = futures_executor::block_on(materialize_capsule(
        &plan(),
        &store,
        &CancellationToken::new(),
    ))
    .unwrap();
    assert_eq!(progress.verified_pages, 3);
    assert_eq!(progress.already_resident, 1);
    assert_eq!(*store.order.lock().unwrap(), vec![(2, true), (3, false)]);
    assert_eq!(store.checkpoints.lock().unwrap().last(), Some(&progress));
}
