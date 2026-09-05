use mirage_db::Database;
use mirage_types::{MirageError, PageHash};

use crate::ResidentIndex;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntegrityClass {
    Clean,
    PinnedClean,
    Dirty,
    Staged,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerifyOutcome {
    Absent,
    Verified,
    Quarantined,
    RecoveryRequired,
}

pub fn verify_page(
    index: &ResidentIndex,
    db: &Database,
    expected: PageHash,
    class: IntegrityClass,
) -> Result<VerifyOutcome, MirageError> {
    let Some(guard) = index.acquire(expected)? else {
        return Ok(VerifyOutcome::Absent);
    };
    let mut bytes = vec![0_u8; guard.logical_length() as usize];
    guard.read_exact(0, &mut bytes)?;
    drop(guard);
    if blake3::hash(&bytes).as_bytes() == expected.as_bytes() {
        return Ok(VerifyOutcome::Verified);
    }
    if matches!(
        class,
        IntegrityClass::PinnedClean | IntegrityClass::Dirty | IntegrityClass::Staged
    ) {
        return Ok(VerifyOutcome::RecoveryRequired);
    }
    index.quarantine_corrupt(db, expected)?;
    Ok(VerifyOutcome::Quarantined)
}
