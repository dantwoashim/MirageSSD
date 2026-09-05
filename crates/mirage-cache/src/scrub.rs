use mirage_db::Database;
use mirage_types::MirageError;

use crate::{IntegrityClass, ResidentIndex, VerifyOutcome, verify_page};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScrubReport {
    pub selected: usize,
    pub verified: usize,
    pub quarantined: usize,
    pub paused: bool,
}

pub fn scrub_batch(
    index: &ResidentIndex,
    db: &Database,
    max_pages: usize,
    gameplay_active: bool,
) -> Result<ScrubReport, MirageError> {
    if gameplay_active || max_pages == 0 {
        return Ok(ScrubReport {
            selected: 0,
            verified: 0,
            quarantined: 0,
            paused: true,
        });
    }
    let hashes = index.hashes()?;
    let mut report = ScrubReport {
        selected: 0,
        verified: 0,
        quarantined: 0,
        paused: false,
    };
    for hash in hashes.into_iter().take(max_pages) {
        report.selected += 1;
        match verify_page(index, db, hash, IntegrityClass::Clean)? {
            VerifyOutcome::Verified => report.verified += 1,
            VerifyOutcome::Quarantined => report.quarantined += 1,
            VerifyOutcome::Absent | VerifyOutcome::RecoveryRequired => {}
        }
    }
    Ok(report)
}
