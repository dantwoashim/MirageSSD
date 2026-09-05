use mirage_service::{
    ConversionAction, ConversionPhase, ConversionTransaction, RestorePhase, RestoreTransaction,
};

#[test]
fn conversion_never_reclaims_before_separate_verified_smoke_phase() {
    let mut transaction = ConversionTransaction::new();
    let phases = [
        ConversionPhase::Profiled,
        ConversionPhase::Classified,
        ConversionPhase::Budgeted,
        ConversionPhase::UploadedVerified,
        ConversionPhase::ProviderVerified,
        ConversionPhase::RollbackCreated,
        ConversionPhase::Confirmed,
        ConversionPhase::Mounted,
        ConversionPhase::SmokeTested,
    ];
    for next in phases {
        let current = transaction.phase();
        transaction.advance(current, next).expect("legal phase");
    }
    assert!(
        !transaction
            .dry_run()
            .contains(&ConversionAction::ReclaimOriginal)
    );
    transaction
        .advance(
            ConversionPhase::SmokeTested,
            ConversionPhase::ReclaimEligible,
        )
        .expect("separate reclaim gate");
    assert!(
        transaction
            .dry_run()
            .contains(&ConversionAction::ReclaimOriginal)
    );
}

#[test]
fn cancellation_and_restore_phase_skips_fail_closed() {
    let mut conversion = ConversionTransaction::new();
    conversion.cancel();
    assert!(
        conversion
            .advance(ConversionPhase::Scanned, ConversionPhase::Profiled)
            .is_err()
    );
    let mut restore = RestoreTransaction::new();
    assert!(
        restore
            .advance(RestorePhase::Planned, RestorePhase::Swapped)
            .is_err()
    );
    for next in [
        RestorePhase::Materialized,
        RestorePhase::Verified,
        RestorePhase::Unmounted,
        RestorePhase::Swapped,
        RestorePhase::Complete,
    ] {
        let current = restore.phase();
        restore.advance(current, next).expect("restore");
    }
}
