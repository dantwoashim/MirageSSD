use std::cell::RefCell;

use mirage_engine::update::{
    ActivationBackend, ActivationPlan, GenerationMounter, activate_generation,
};
use mirage_types::{CommitHash, GenerationId, MirageError, RepositoryId};

struct Store {
    active: RefCell<Option<(GenerationId, CommitHash)>>,
    fail_switch: bool,
}
impl ActivationBackend for Store {
    fn active(&self, _: RepositoryId) -> Result<Option<(GenerationId, CommitHash)>, MirageError> {
        Ok(*self.active.borrow())
    }
    fn switch(
        &self,
        _: RepositoryId,
        target: (GenerationId, CommitHash),
        expected: Option<(GenerationId, CommitHash)>,
        _: i64,
    ) -> Result<(), MirageError> {
        if self.fail_switch {
            return Err(MirageError::internal_invariant("injected switch failure"));
        }
        if *self.active.borrow() != expected {
            return Err(MirageError::repository_conflict("stale activation"));
        }
        *self.active.borrow_mut() = Some(target);
        Ok(())
    }
}
struct Mount {
    fail_target: Option<GenerationId>,
    mounted: Vec<GenerationId>,
    quiesced: bool,
}
impl GenerationMounter for Mount {
    fn quiesce(&mut self) -> Result<(), MirageError> {
        self.quiesced = true;
        Ok(())
    }
    fn mount_and_smoke_test(&mut self, generation: GenerationId) -> Result<(), MirageError> {
        self.mounted.push(generation);
        if self.fail_target == Some(generation) {
            Err(MirageError::internal_invariant("injected mount failure"))
        } else {
            Ok(())
        }
    }
}
fn plan() -> ActivationPlan {
    ActivationPlan {
        repository: RepositoryId::from_bytes([1; 16]),
        generation: GenerationId::from_u64(2),
        commit: CommitHash::from_bytes([2; 32]),
        timestamp_ns: 5,
    }
}

#[test]
fn activation_succeeds_after_quiesce() {
    let old = (GenerationId::from_u64(1), CommitHash::from_bytes([1; 32]));
    let store = Store {
        active: RefCell::new(Some(old)),
        fail_switch: false,
    };
    let mut mount = Mount {
        fail_target: None,
        mounted: vec![],
        quiesced: false,
    };
    let report = activate_generation(&store, &mut mount, plan()).expect("activate");
    assert!(mount.quiesced);
    assert_eq!(report.previous, Some(old));
    assert_eq!(report.active.0, GenerationId::from_u64(2));
}

#[test]
fn mount_failure_rolls_pointer_back() {
    let old = (GenerationId::from_u64(1), CommitHash::from_bytes([1; 32]));
    let store = Store {
        active: RefCell::new(Some(old)),
        fail_switch: false,
    };
    let mut mount = Mount {
        fail_target: Some(GenerationId::from_u64(2)),
        mounted: vec![],
        quiesced: false,
    };
    assert!(activate_generation(&store, &mut mount, plan()).is_err());
    assert_eq!(*store.active.borrow(), Some(old));
    assert_eq!(
        mount.mounted,
        vec![GenerationId::from_u64(2), GenerationId::from_u64(1)]
    );
}

#[test]
fn durable_switch_failure_never_mounts_target() {
    let store = Store {
        active: RefCell::new(None),
        fail_switch: true,
    };
    let mut mount = Mount {
        fail_target: None,
        mounted: vec![],
        quiesced: false,
    };
    assert!(activate_generation(&store, &mut mount, plan()).is_err());
    assert!(mount.mounted.is_empty());
}
