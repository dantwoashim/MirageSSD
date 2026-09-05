use mirage_types::MirageError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestorePhase {
    Planned,
    Materialized,
    Verified,
    Unmounted,
    Swapped,
    Complete,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestoreAction {
    MaterializeNewDirectory,
    VerifyEveryFile,
    Unmount,
    AtomicSwap,
    RemoveMirageMetadata,
}

pub struct RestoreTransaction {
    phase: RestorePhase,
}
impl RestoreTransaction {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            phase: RestorePhase::Planned,
        }
    }
    #[must_use]
    pub const fn phase(&self) -> RestorePhase {
        self.phase
    }
    pub fn advance(
        &mut self,
        expected: RestorePhase,
        next: RestorePhase,
    ) -> Result<(), MirageError> {
        if self.phase != expected {
            return Err(MirageError::repository_conflict(
                "restore phase changed concurrently",
            ));
        }
        let legal = matches!(
            (expected, next),
            (RestorePhase::Planned, RestorePhase::Materialized)
                | (RestorePhase::Materialized, RestorePhase::Verified)
                | (RestorePhase::Verified, RestorePhase::Unmounted)
                | (RestorePhase::Unmounted, RestorePhase::Swapped)
                | (RestorePhase::Swapped, RestorePhase::Complete)
        );
        if !legal {
            return Err(MirageError::invalid_argument(
                "restore transition would bypass byte verification or unmount",
            ));
        }
        self.phase = next;
        Ok(())
    }
    #[must_use]
    pub const fn plan() -> [RestoreAction; 5] {
        [
            RestoreAction::MaterializeNewDirectory,
            RestoreAction::VerifyEveryFile,
            RestoreAction::Unmount,
            RestoreAction::AtomicSwap,
            RestoreAction::RemoveMirageMetadata,
        ]
    }
}
impl Default for RestoreTransaction {
    fn default() -> Self {
        Self::new()
    }
}
