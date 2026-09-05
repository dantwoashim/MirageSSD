use mirage_cache::{PolicyCore, PolicyEvent, PolicyKind, PolicyOutcome};
use mirage_types::MirageError;

pub struct SimCachePolicy {
    core: PolicyCore,
}
impl SimCachePolicy {
    pub fn new(kind: PolicyKind, capacity: usize) -> Result<Self, MirageError> {
        Ok(Self {
            core: PolicyCore::new(kind, capacity)?,
        })
    }
    pub fn apply(&mut self, event: PolicyEvent) -> Result<PolicyOutcome, MirageError> {
        self.core.apply(event)
    }
    #[must_use]
    pub fn residents(&self) -> Vec<u64> {
        self.core.residents()
    }
}
