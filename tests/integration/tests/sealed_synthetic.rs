use async_trait::async_trait;
use mirage_engine::{
    AdmissionStore, CapsulePageStore, MaterializeProgress, admit_sealed_session,
    materialize_capsule,
};
use mirage_predictor::capsule::*;
use mirage_types::{GenerationId, MirageError, RepositoryId, SessionId};
use roaring::RoaringBitmap;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use tokio_util::sync::CancellationToken;
fn bm(v: &[u32]) -> RoaringBitmap {
    v.iter().copied().collect()
}
fn page_bytes(page: u32) -> Vec<u8> {
    (0..4096)
        .map(|i| (page as usize * 31 + i) % 251)
        .map(|v| v as u8)
        .collect()
}
fn plan(pages: &[u32]) -> CapsulePlan {
    let set = bm(pages);
    CapsulePlan::new(CapsuleDraft {
        repository_id: RepositoryId::from_bytes([5; 16]),
        generation: GenerationId(11),
        profile_key: ProfileKey("synthetic".into()),
        page_set: set.clone(),
        mandatory_set: set,
        frontier_set: RoaringBitmap::new(),
        page_size: 4096,
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
struct Harness {
    network: AtomicBool,
    calls: AtomicU64,
    resident: Mutex<BTreeMap<u32, Vec<u8>>>,
    reserved: Mutex<BTreeSet<u32>>,
    pinned: Mutex<BTreeSet<u32>>,
    ready: AtomicBool,
}
#[async_trait]
impl CapsulePageStore for Harness {
    fn generation(&self) -> GenerationId {
        GenerationId(11)
    }
    async fn reserve_all(&self, p: &RoaringBitmap) -> Result<(), MirageError> {
        self.reserved.lock().unwrap().extend(p);
        Ok(())
    }
    async fn is_verified_resident(&self, p: u32) -> Result<bool, MirageError> {
        Ok(self.resident.lock().unwrap().get(&p) == Some(&page_bytes(p)))
    }
    async fn fetch_verify_commit(
        &self,
        p: u32,
        _: bool,
        _: &CancellationToken,
    ) -> Result<u64, MirageError> {
        if !self.network.load(Ordering::SeqCst) {
            return Err(MirageError::backend_unavailable("network cut"));
        }
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.resident.lock().unwrap().insert(p, page_bytes(p));
        Ok(4096)
    }
    async fn checkpoint(&self, _: MaterializeProgress) -> Result<(), MirageError> {
        Ok(())
    }
}
#[async_trait]
impl AdmissionStore for Harness {
    fn generation(&self) -> GenerationId {
        GenerationId(11)
    }
    async fn state_allows_admission(&self) -> Result<bool, MirageError> {
        Ok(true)
    }
    async fn all_verified_resident(&self, p: &RoaringBitmap) -> Result<bool, MirageError> {
        Ok(p.iter()
            .all(|page| self.resident.lock().unwrap().get(&page) == Some(&page_bytes(page))))
    }
    async fn create_session_and_leases(&self, _: &CapsulePlan) -> Result<SessionId, MirageError> {
        Ok(SessionId::from_bytes([6; 16]))
    }
    async fn apply_memory_pins(&self, _: SessionId, p: &RoaringBitmap) -> Result<(), MirageError> {
        self.pinned.lock().unwrap().extend(p);
        Ok(())
    }
    async fn provider_ready(&self) -> Result<bool, MirageError> {
        Ok(self.network.load(Ordering::SeqCst))
    }
    async fn mark_sealed_ready(&self, _: SessionId) -> Result<(), MirageError> {
        self.ready.store(true, Ordering::SeqCst);
        Ok(())
    }
}
fn harness() -> Harness {
    Harness {
        network: AtomicBool::new(true),
        calls: AtomicU64::new(0),
        resident: Mutex::new(BTreeMap::new()),
        reserved: Mutex::new(BTreeSet::new()),
        pinned: Mutex::new(BTreeSet::new()),
        ready: AtomicBool::new(false),
    }
}
#[test]
fn complete_capsule_is_byte_exact_after_network_cut_and_pressure() {
    let store = harness();
    let plan = plan(&[1, 2, 3, 4]);
    futures_executor::block_on(materialize_capsule(
        &plan,
        &store,
        &CancellationToken::new(),
    ))
    .unwrap();
    let calls = store.calls.load(Ordering::SeqCst);
    store.network.store(false, Ordering::SeqCst);
    futures_executor::block_on(admit_sealed_session(&plan, &store)).unwrap();
    for page in &plan.page_set {
        assert_eq!(
            store.resident.lock().unwrap().get(&page).unwrap(),
            &page_bytes(page)
        );
    }
    for page in 100..200 {
        if !store.pinned.lock().unwrap().contains(&page) {
            store.resident.lock().unwrap().remove(&page);
        }
    }
    assert_eq!(store.calls.load(Ordering::SeqCst), calls);
    assert!(store.ready.load(Ordering::SeqCst));
    assert_eq!(store.pinned.lock().unwrap().len(), 4);
}
#[test]
fn omitted_page_is_detected_not_hidden() {
    let store = harness();
    let complete = plan(&[1, 2, 3]);
    futures_executor::block_on(materialize_capsule(
        &plan(&[1, 2]),
        &store,
        &CancellationToken::new(),
    ))
    .unwrap();
    store.network.store(false, Ordering::SeqCst);
    assert!(futures_executor::block_on(admit_sealed_session(&complete, &store)).is_err());
    assert!(!store.ready.load(Ordering::SeqCst));
}
