use mirage_fault_inject::update_matrix::{
    ALL_UPDATE_BOUNDARIES, RecoveryAction, UpdateBoundary, VisibleGeneration, expected_recovery,
};

#[test]
fn every_update_crash_boundary_is_all_or_nothing_and_recoverable() {
    for boundary in ALL_UPDATE_BOUNDARIES {
        let recovery = expected_recovery(boundary);
        match boundary {
            UpdateBoundary::BeforeJournal => {
                assert_eq!(recovery.visible, VisibleGeneration::Previous);
                assert_eq!(recovery.action, RecoveryAction::DiscardUnacknowledged);
                assert!(!recovery.acknowledged_dirty_is_durable);
            }
            UpdateBoundary::AfterJournalFlush | UpdateBoundary::AfterStagingUpload => {
                assert_eq!(recovery.visible, VisibleGeneration::Previous);
                assert_eq!(recovery.action, RecoveryAction::ResumeStaging);
                assert!(recovery.acknowledged_dirty_is_durable);
            }
            UpdateBoundary::AfterCommitUpload => {
                assert_eq!(recovery.visible, VisibleGeneration::Previous);
                assert_eq!(recovery.action, RecoveryAction::ResumeActivation);
                assert!(recovery.acknowledged_dirty_is_durable);
            }
            UpdateBoundary::AfterGenerationSwitch | UpdateBoundary::AfterMountSmokeTest => {
                assert_eq!(recovery.visible, VisibleGeneration::Updated);
                assert_eq!(recovery.action, RecoveryAction::ConfirmUpdated);
                assert!(recovery.acknowledged_dirty_is_durable);
            }
        }
    }
}
