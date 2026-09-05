use mirage_types::MirageError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ConversionPhase {
    Scanned,
    Profiled,
    Classified,
    Budgeted,
    UploadedVerified,
    ProviderVerified,
    RollbackCreated,
    Confirmed,
    Mounted,
    SmokeTested,
    ReclaimEligible,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConversionAction {
    RenameSource,
    CreateMount,
    ReclaimOriginal,
}

/// Monotonic conversion state. Original bytes cannot become reclaimable in the mount transaction.
pub struct ConversionTransaction {
    phase: ConversionPhase,
    cancelled: bool,
}
impl ConversionTransaction {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            phase: ConversionPhase::Scanned,
            cancelled: false,
        }
    }
    #[must_use]
    pub const fn phase(&self) -> ConversionPhase {
        self.phase
    }
    pub fn cancel(&mut self) {
        self.cancelled = true;
    }
    pub fn advance(
        &mut self,
        expected: ConversionPhase,
        next: ConversionPhase,
    ) -> Result<(), MirageError> {
        if self.cancelled {
            return Err(MirageError::cancelled("conversion was cancelled"));
        }
        if self.phase != expected || next <= expected {
            return Err(MirageError::repository_conflict(
                "conversion phase is stale or non-monotonic",
            ));
        }
        if next == ConversionPhase::Confirmed && expected != ConversionPhase::RollbackCreated {
            return Err(MirageError::invalid_argument(
                "conversion requires a verified rollback point before confirmation",
            ));
        }
        if next == ConversionPhase::ReclaimEligible && expected != ConversionPhase::SmokeTested {
            return Err(MirageError::invalid_argument(
                "reclaim requires a successful mounted smoke test",
            ));
        }
        self.phase = next;
        Ok(())
    }
    #[must_use]
    pub fn dry_run(&self) -> Vec<ConversionAction> {
        let mut actions = vec![
            ConversionAction::RenameSource,
            ConversionAction::CreateMount,
        ];
        if self.phase == ConversionPhase::ReclaimEligible {
            actions.push(ConversionAction::ReclaimOriginal);
        }
        actions
    }
}
impl Default for ConversionTransaction {
    fn default() -> Self {
        Self::new()
    }
}
