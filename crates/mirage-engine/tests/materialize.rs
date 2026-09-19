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
#[derive(Default)]
struct Store {
    resident: Mutex<BTreeSet<u32>>,
    order: Mutex<Vec<(u32, bool)>>,
    checkpoints: Mutex<Vec<MaterializeProgress>>,
    fail_on: Option<u32>,
    cancel_after: Option<u32>,
}
#[async_trait]
impl CapsulePageStore for Store {
    fn generation(&self) -> GenerationId {
        GenerationId(7)
    }
    async fn reserve_all(&self, pages: &RoaringBitmap) -> Result<(), MirageError> {
        assert!(!pages.is_empty());
        Ok(())
    }
    async fn is_verified_resident(&self, page: u32) -> Result<bool, MirageError> {
        Ok(self.resident.lock().unwrap().contains(&page))
    }
    async fn fetch_verify_commit(
        &self,
        page: u32,
        mandatory: bool,
        cancel: &CancellationToken,
    ) -> Result<u64, MirageError> {
        if self.fail_on == Some(page) {
            return Err(MirageError::provider_unavailable("injected fetch failure"));
        }
        self.order.lock().unwrap().push((page, mandatory));
        self.resident.lock().unwrap().insert(page);
        if self.cancel_after == Some(page) {
            cancel.cancel();
        }
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
        ..Default::default()
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

#[test]
fn progress_checkpoints_are_bounded_and_final_progress_is_exact() {
    let store = Store::default();
    let mut plan = plan();
    plan.page_set = (0..129).collect();
    let progress = futures_executor::block_on(materialize_capsule(
        &plan,
        &store,
        &CancellationToken::new(),
    ))
    .unwrap();
    let checkpoints = store.checkpoints.lock().unwrap();
    assert!(
        checkpoints.len() <= 6,
        "{} checkpoints for 129 pages",
        checkpoints.len()
    );
    assert_eq!(checkpoints.last(), Some(&progress));
    assert_eq!(progress.verified_pages, 129);
    assert_eq!(progress.downloaded_bytes, 129 * 65536);
    assert_eq!(store.order.lock().unwrap()[0], (2, true));
}

#[test]
fn cancellation_checkpoints_every_committed_page_before_returning() {
    let store = Store {
        cancel_after: Some(2),
        ..Default::default()
    };
    let cancel = CancellationToken::new();
    assert!(futures_executor::block_on(materialize_capsule(&plan(), &store, &cancel)).is_err());
    let checkpoints = store.checkpoints.lock().unwrap();
    let progress = checkpoints.last().unwrap();
    assert_eq!(progress.verified_pages, 1);
    assert_eq!(progress.downloaded_pages, 1);
    assert_eq!(progress.failed_pages, 0);
    assert_eq!(*store.order.lock().unwrap(), [(2, true)]);
}

#[test]
fn fetch_failure_checkpoints_successes_and_resume_reuses_them() {
    let mut store = Store {
        fail_on: Some(3),
        ..Default::default()
    };
    assert!(
        futures_executor::block_on(materialize_capsule(
            &plan(),
            &store,
            &CancellationToken::new()
        ))
        .is_err()
    );
    let progress = *store.checkpoints.lock().unwrap().last().unwrap();
    assert_eq!(progress.verified_pages, 2);
    assert_eq!(progress.failed_pages, 1);
    store.fail_on = None;
    let resumed = futures_executor::block_on(materialize_capsule(
        &plan(),
        &store,
        &CancellationToken::new(),
    ))
    .unwrap();
    assert_eq!(resumed.already_resident, 2);
    assert_eq!(resumed.downloaded_pages, 1);
    assert_eq!(resumed.verified_pages, 3);
}
