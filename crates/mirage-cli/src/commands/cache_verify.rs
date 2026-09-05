use std::path::Path;
use std::sync::Arc;

use mirage_cache::{
    ArenaShard, CacheLayout, IntegrityClass, PinReason, ResidentIndex, VerifyOutcome, verify_page,
};
use mirage_db::Database;
use mirage_types::{ByteCount, MirageError};

use crate::output;

pub fn run(
    db_path: &Path,
    arena_path: &Path,
    page_size: u64,
    slot_count: u32,
    max_pages: Option<usize>,
    json: bool,
) -> Result<(), MirageError> {
    let layout = CacheLayout {
        page_size: ByteCount::from_u64(page_size),
        slot_count,
        db_journal_allowance: ByteCount::ZERO,
        filesystem_reserve: ByteCount::ZERO,
    };
    let db = Database::open(db_path)?;
    let shard = Arc::new(ArenaShard::open(arena_path, layout)?);
    let index = ResidentIndex::rebuild(&db, shard)?;
    let mut verified = 0_usize;
    let mut quarantined = 0_usize;
    let mut recovery_required = 0_usize;
    for hash in index
        .hashes()?
        .into_iter()
        .take(max_pages.unwrap_or(usize::MAX))
    {
        let reasons = index.pins().reasons(hash)?;
        let class = if reasons
            .iter()
            .any(|reason| matches!(reason, PinReason::Dirty(_)))
        {
            IntegrityClass::Dirty
        } else if reasons.is_empty() {
            IntegrityClass::Clean
        } else {
            IntegrityClass::PinnedClean
        };
        match verify_page(&index, &db, hash, class)? {
            VerifyOutcome::Verified => verified += 1,
            VerifyOutcome::Quarantined => quarantined += 1,
            VerifyOutcome::RecoveryRequired => recovery_required += 1,
            VerifyOutcome::Absent => {}
        }
    }
    if json {
        output::emit_success(
            &serde_json::json!({ "report_version": 1, "verified": verified, "quarantined": quarantined, "recovery_required": recovery_required }),
        )
    } else {
        println!(
            "cache verify: {verified} verified, {quarantined} quarantined, {recovery_required} recovery-required"
        );
        Ok(())
    }
}
