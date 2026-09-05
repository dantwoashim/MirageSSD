use mirage_db::{CacheSlotRecord, CacheSlotState, Database};
use mirage_types::MirageError;

use crate::recover::metadata_matches;
use crate::slot::metadata_for;
use crate::{ArenaShard, SlotState};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReconcileAction {
    ReclaimReserved,
    FinishEviction,
    RewriteFreeMetadata,
    QuarantineResident,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReconcileReport {
    pub inspected: u64,
    pub healthy_residents: u64,
    pub actions: Vec<(u32, ReconcileAction)>,
    pub repaired: u64,
    pub blocked: Vec<String>,
}

pub fn reconcile(
    db: &Database,
    shard: &ArenaShard,
    dry_run: bool,
) -> Result<ReconcileReport, MirageError> {
    let mut report = ReconcileReport {
        inspected: 0,
        healthy_residents: 0,
        actions: Vec::new(),
        repaired: 0,
        blocked: Vec::new(),
    };
    for record in db.load_cache_slots()? {
        report.inspected += 1;
        if record.shard_id != 0 || record.slot_index >= shard.layout().slot_count {
            report
                .blocked
                .push("cache DB references an unavailable shard or slot".into());
            continue;
        }
        let metadata = shard.read_metadata(record.slot_index);
        match record.state {
            CacheSlotState::Free => {
                if metadata
                    .as_ref()
                    .is_ok_and(|value| metadata_matches(record, *value))
                {
                    continue;
                }
                report
                    .actions
                    .push((record.slot_index, ReconcileAction::RewriteFreeMetadata));
                if !dry_run {
                    shard.deallocate_slot(record.slot_index)?;
                    write_free(shard, record)?;
                    report.repaired += 1;
                }
            }
            CacheSlotState::Reserved => {
                report
                    .actions
                    .push((record.slot_index, ReconcileAction::ReclaimReserved));
                if !dry_run {
                    db.release_cache_reservation(record)?;
                    let evicting = CacheSlotRecord {
                        state: CacheSlotState::Evicting,
                        ..record
                    };
                    shard.write_metadata(
                        record.slot_index,
                        metadata_for(evicting, SlotState::Evicting),
                    )?;
                    shard.flush_metadata()?;
                    shard.deallocate_slot(record.slot_index)?;
                    write_free(shard, record)?;
                    db.finish_cache_deallocation(evicting)?;
                    report.repaired += 1;
                }
            }
            CacheSlotState::Evicting | CacheSlotState::RetryDeallocate => {
                report
                    .actions
                    .push((record.slot_index, ReconcileAction::FinishEviction));
                if !dry_run {
                    shard.deallocate_slot(record.slot_index)?;
                    write_free(shard, record)?;
                    db.finish_cache_deallocation(record)?;
                    report.repaired += 1;
                }
            }
            CacheSlotState::Resident => {
                if metadata
                    .as_ref()
                    .is_ok_and(|value| metadata_matches(record, *value))
                {
                    report.healthy_residents += 1;
                } else {
                    report
                        .actions
                        .push((record.slot_index, ReconcileAction::QuarantineResident));
                    if !dry_run {
                        let evicting = db.begin_cache_eviction(record)?;
                        shard.write_metadata(
                            record.slot_index,
                            metadata_for(evicting, SlotState::Evicting),
                        )?;
                        shard.flush_metadata()?;
                        shard.deallocate_slot(record.slot_index)?;
                        write_free(shard, record)?;
                        db.finish_cache_deallocation(evicting)?;
                        report.repaired += 1;
                    }
                }
            }
        }
    }
    if !dry_run {
        shard.flush_metadata()?;
    }
    Ok(report)
}

fn write_free(shard: &ArenaShard, record: CacheSlotRecord) -> Result<(), MirageError> {
    shard.write_metadata(
        record.slot_index,
        metadata_for(
            CacheSlotRecord {
                state: CacheSlotState::Free,
                page_hash: None,
                logical_length: 0,
                ..record
            },
            SlotState::Free,
        ),
    )?;
    shard.flush_metadata()
}
