use mirage_types::{MirageError, PageHash};

use crate::{AdmissionContext, AdmissionDecision, AdmissionWeights};

pub struct AdmissionPolicy {
    weights: AdmissionWeights,
}
impl AdmissionPolicy {
    #[must_use]
    pub const fn new(weights: AdmissionWeights) -> Self {
        Self { weights }
    }
    pub fn compare(
        &self,
        incoming: PageHash,
        victim: PageHash,
        context: AdmissionContext,
        blocking: bool,
    ) -> Result<AdmissionDecision, MirageError> {
        self.weights.decide(incoming, victim, context, blocking)
    }
}
