use std::path::Path;

use mirage_cache::{ArenaShard, CacheLayout, reconcile};
use mirage_db::Database;
use mirage_types::{ByteCount, MirageError};

use crate::output;

pub fn run(
    db_path: &Path,
    arena_path: &Path,
    page_size: u64,
    slot_count: u32,
    dry_run: bool,
    json: bool,
) -> Result<(), MirageError> {
    let layout = CacheLayout {
        page_size: ByteCount::from_u64(page_size),
        slot_count,
        db_journal_allowance: ByteCount::ZERO,
        filesystem_reserve: ByteCount::ZERO,
    };
    let db = Database::open(db_path)?;
    let shard = ArenaShard::open(arena_path, layout)?;
    let report = reconcile(&db, &shard, dry_run)?;
    let actions = report
        .actions
        .iter()
        .map(|(slot, action)| serde_json::json!({ "slot": slot, "action": format!("{action:?}") }))
        .collect::<Vec<_>>();
    if json {
        output::emit_success(
            &serde_json::json!({ "report_version": 1, "dry_run": dry_run, "inspected": report.inspected, "healthy_residents": report.healthy_residents, "planned_actions": actions, "repaired": report.repaired, "blocked": report.blocked }),
        )
    } else {
        println!(
            "cache check: {} inspected, {} planned, {} repaired, {} blocked",
            report.inspected,
            report.actions.len(),
            report.repaired,
            report.blocked.len()
        );
        Ok(())
    }
}
