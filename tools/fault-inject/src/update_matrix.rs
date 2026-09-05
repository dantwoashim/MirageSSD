#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateBoundary {
    BeforeJournal,
    AfterJournalFlush,
    AfterStagingUpload,
    AfterCommitUpload,
    AfterGenerationSwitch,
    AfterMountSmokeTest,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VisibleGeneration {
    Previous,
    Updated,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoveryAction {
    DiscardUnacknowledged,
    ResumeStaging,
    ResumeActivation,
    ConfirmUpdated,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExpectedRecovery {
    pub visible: VisibleGeneration,
    pub action: RecoveryAction,
    pub acknowledged_dirty_is_durable: bool,
}

#[must_use]
pub const fn expected_recovery(boundary: UpdateBoundary) -> ExpectedRecovery {
    match boundary {
        UpdateBoundary::BeforeJournal => ExpectedRecovery {
            visible: VisibleGeneration::Previous,
            action: RecoveryAction::DiscardUnacknowledged,
            acknowledged_dirty_is_durable: false,
        },
        UpdateBoundary::AfterJournalFlush | UpdateBoundary::AfterStagingUpload => {
            ExpectedRecovery {
                visible: VisibleGeneration::Previous,
                action: RecoveryAction::ResumeStaging,
                acknowledged_dirty_is_durable: true,
            }
        }
        UpdateBoundary::AfterCommitUpload => ExpectedRecovery {
            visible: VisibleGeneration::Previous,
            action: RecoveryAction::ResumeActivation,
            acknowledged_dirty_is_durable: true,
        },
        UpdateBoundary::AfterGenerationSwitch | UpdateBoundary::AfterMountSmokeTest => {
            ExpectedRecovery {
                visible: VisibleGeneration::Updated,
                action: RecoveryAction::ConfirmUpdated,
                acknowledged_dirty_is_durable: true,
            }
        }
    }
}

pub const ALL_UPDATE_BOUNDARIES: [UpdateBoundary; 6] = [
    UpdateBoundary::BeforeJournal,
    UpdateBoundary::AfterJournalFlush,
    UpdateBoundary::AfterStagingUpload,
    UpdateBoundary::AfterCommitUpload,
    UpdateBoundary::AfterGenerationSwitch,
    UpdateBoundary::AfterMountSmokeTest,
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_boundary_exposes_exactly_one_generation() {
        for boundary in ALL_UPDATE_BOUNDARIES {
            let recovery = expected_recovery(boundary);
            assert!(matches!(
                recovery.visible,
                VisibleGeneration::Previous | VisibleGeneration::Updated
            ));
            if recovery.action != RecoveryAction::DiscardUnacknowledged {
                assert!(recovery.acknowledged_dirty_is_durable);
            }
        }
    }
}
