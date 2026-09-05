use async_trait::async_trait;
use mirage_engine::{AdmissionStore, admit_sealed_session};
use mirage_predictor::capsule::*;
use mirage_types::{GenerationId, MirageError, RepositoryId, SessionId};
use roaring::RoaringBitmap;
use std::sync::Mutex;
fn bm(v: &[u32]) -> RoaringBitmap {
    v.iter().copied().collect()
}
fn plan() -> CapsulePlan {
    CapsulePlan::new(CapsuleDraft {
        repository_id: RepositoryId::from_bytes([1; 16]),
        generation: GenerationId(7),
        profile_key: ProfileKey("test".into()),
        page_set: bm(&[1, 2]),
        mandatory_set: bm(&[1]),
        frontier_set: bm(&[2]),
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
    generation: Mutex<GenerationId>,
    resident: bool,
    events: Mutex<Vec<&'static str>>,
}
#[async_trait]
impl AdmissionStore for Store {
    fn generation(&self) -> GenerationId {
        *self.generation.lock().unwrap()
    }
    async fn state_allows_admission(&self) -> Result<bool, MirageError> {
        Ok(true)
    }
    async fn all_verified_resident(&self, _: &RoaringBitmap) -> Result<bool, MirageError> {
        Ok(self.resident)
    }
    async fn create_session_and_leases(&self, _: &CapsulePlan) -> Result<SessionId, MirageError> {
        self.events.lock().unwrap().push("db");
        Ok(SessionId::from_bytes([9; 16]))
    }
    async fn apply_memory_pins(&self, _: SessionId, _: &RoaringBitmap) -> Result<(), MirageError> {
        self.events.lock().unwrap().push("pins");
        Ok(())
    }
    async fn provider_ready(&self) -> Result<bool, MirageError> {
        Ok(false)
    }
    async fn mark_sealed_ready(&self, _: SessionId) -> Result<(), MirageError> {
        self.events.lock().unwrap().push("ready");
        Ok(())
    }
}
#[test]
fn offline_complete_capsule_is_sealed_only_after_leases_and_pins() {
    let store = Store {
        generation: Mutex::new(GenerationId(7)),
        resident: true,
        events: Mutex::new(vec![]),
    };
    let admitted = futures_executor::block_on(admit_sealed_session(&plan(), &store)).unwrap();
    assert_eq!(admitted.pinned_pages, 2);
    assert_eq!(*store.events.lock().unwrap(), vec!["db", "pins", "ready"]);
}
#[test]
fn missing_page_never_creates_session() {
    let store = Store {
        generation: Mutex::new(GenerationId(7)),
        resident: false,
        events: Mutex::new(vec![]),
    };
    assert!(futures_executor::block_on(admit_sealed_session(&plan(), &store)).is_err());
    assert!(store.events.lock().unwrap().is_empty());
}
