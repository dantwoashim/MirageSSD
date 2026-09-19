use std::sync::Arc;

use mirage_db::{
    CacheSlotRecord, CacheSlotState, CommitCacheSlotOutcome, Database, ReserveCacheSlotOutcome,
};
use mirage_types::{MirageError, PageHash};

use crate::reserve::SlotReservation;
use crate::slot::metadata_for;
use crate::{ArenaShard, SlotState};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InsertStep {
    Reserved,
    Written,
    Flushed,
    Rehashed,
    MetadataCommitted,
}

pub trait InsertHook {
    fn after(&self, step: InsertStep) -> Result<(), MirageError>;
}
impl InsertHook for () {
    fn after(&self, _step: InsertStep) -> Result<(), MirageError> {
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InsertOutcome {
    Inserted(CacheSlotRecord),
    Existing(CacheSlotRecord),
}

pub fn insert_page(
    db: &Database,
    shard: Arc<ArenaShard>,
    expected_hash: PageHash,
    bytes: &[u8],
    hook: &dyn InsertHook,
) -> Result<InsertOutcome, MirageError> {
    let logical_length = u32::try_from(bytes.len())
        .map_err(|_| MirageError::invalid_argument("cache page exceeds u32"))?;
    if logical_length == 0 || u64::from(logical_length) > shard.layout().page_size.as_u64() {
        return Err(MirageError::invalid_argument(
            "cache page length is outside the shard layout",
        ));
    }
    match db.reserve_cache_slot(expected_hash, logical_length)? {
        ReserveCacheSlotOutcome::Existing(existing) => Ok(InsertOutcome::Existing(existing)),
        ReserveCacheSlotOutcome::Reserved(record) => {
            if record.shard_id != 0 {
                return Err(MirageError::unsupported_layout(
                    "insertion shard routing is not configured",
                ));
            }
            let mut reservation = SlotReservation {
                db: db.clone(),
                shard: Arc::clone(&shard),
                record,
                committed: false,
            };
            shard.write_metadata(record.slot_index, metadata_for(record, SlotState::Reserved))?;
            shard.flush_metadata()?;
            hook.after(InsertStep::Reserved)?;
            shard.write_slot(record.slot_index, bytes)?;
            hook.after(InsertStep::Written)?;
            shard.flush()?;
            hook.after(InsertStep::Flushed)?;
            let mut verified = vec![0_u8; bytes.len()];
            shard.read_slot(record.slot_index, logical_length, 0, &mut verified)?;
            if PageHash::from_bytes(*blake3::hash(&verified).as_bytes()) != expected_hash {
                return Err(MirageError::integrity_mismatch(
                    "cache payload rehash differs from reservation",
                ));
            }
            hook.after(InsertStep::Rehashed)?;
            shard.write_metadata(
                record.slot_index,
                metadata_for(
                    CacheSlotRecord {
                        state: CacheSlotState::Resident,
                        ..record
                    },
                    SlotState::Resident,
                ),
            )?;
            shard.flush_metadata()?;
            match db.commit_cache_slot(record)? {
                CommitCacheSlotOutcome::Committed(resident) => {
                    reservation.committed = true;
                    hook.after(InsertStep::MetadataCommitted)?;
                    Ok(InsertOutcome::Inserted(resident))
                }
                CommitCacheSlotOutcome::Existing(existing) => {
                    let evicting = CacheSlotRecord {
                        state: CacheSlotState::Evicting,
                        ..record
                    };
                    shard.write_metadata(
                        record.slot_index,
                        metadata_for(evicting, SlotState::Evicting),
                    )?;
                    shard.flush_metadata()?;
                    if shard.deallocate_slot(record.slot_index).is_ok() {
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
                        shard.flush_metadata()?;
                        db.finish_cache_deallocation(evicting)?;
                    } else {
                        db.mark_cache_deallocation_retry(evicting)?;
                        return Err(MirageError::cache_full(
                            "duplicate slot deallocation requires retry",
                        ));
                    }
                    reservation.committed = true;
                    Ok(InsertOutcome::Existing(existing))
                }
            }
        }
    }
}

/// Writes a batch as maximal runs of consecutive slots, one I/O per run.
/// A run extends only while the previous page fills its slot, so no slot's
/// bytes ever land at a different offset than a per-slot write would use.
fn write_contiguous_runs(
    shard: &ArenaShard,
    pages: &[(CacheSlotRecord, &[u8])],
) -> Result<(), MirageError> {
    let page_size = shard.layout().page_size.as_u64();
    let mut ordered: Vec<&(CacheSlotRecord, &[u8])> = pages.iter().collect();
    ordered.sort_by_key(|(record, _)| record.slot_index);
    let mut cursor = 0;
    while cursor < ordered.len() {
        let mut end = cursor + 1;
        while end < ordered.len()
            && ordered[end - 1].1.len() as u64 == page_size
            && Some(ordered[end].0.slot_index) == ordered[end - 1].0.slot_index.checked_add(1)
        {
            end += 1;
        }
        if end - cursor == 1 {
            shard.write_slot(ordered[cursor].0.slot_index, ordered[cursor].1)?;
        } else {
            let run: Vec<u8> = ordered[cursor..end]
                .iter()
                .flat_map(|(_, bytes)| bytes.iter().copied())
                .collect();
            shard.write_slots_contiguous(
                ordered[cursor].0.slot_index,
                (end - cursor) as u32,
                &run,
            )?;
        }
        cursor = end;
    }
    Ok(())
}

pub fn insert_reserved_pages(
    db: &Database,
    shard: Arc<ArenaShard>,
    pages: &[(CacheSlotRecord, &[u8])],
    hook: &dyn InsertHook,
) -> Result<Vec<CacheSlotRecord>, MirageError> {
    if pages.is_empty() || pages.len() > 64 {
        return Err(MirageError::invalid_argument(
            "cache insertion batch must contain 1 to 64 pages",
        ));
    }
    let mut total = 0usize;
    let mut slots = std::collections::BTreeSet::new();
    let mut hashes = std::collections::BTreeSet::new();
    for (record, bytes) in pages {
        if record.state != CacheSlotState::Reserved
            || record.shard_id != 0
            || record.page_hash.is_none()
            || record.logical_length as usize != bytes.len()
            || bytes.is_empty()
            || bytes.len() as u64 > shard.layout().page_size.as_u64()
            || !slots.insert(record.slot_index)
            || !hashes.insert(record.page_hash)
        {
            return Err(MirageError::invalid_argument(
                "invalid or duplicate reserved cache batch page",
            ));
        }
        shard.slot_offset(record.slot_index)?;
        total = total.checked_add(bytes.len()).ok_or_else(|| {
            MirageError::invalid_argument("cache insertion batch length overflows")
        })?;
    }
    if total > 32 * 1024 * 1024 {
        return Err(MirageError::invalid_argument(
            "cache insertion batch exceeds 32 MiB",
        ));
    }
    let mut reservations: Vec<_> = pages
        .iter()
        .map(|(record, _)| SlotReservation {
            db: db.clone(),
            shard: Arc::clone(&shard),
            record: *record,
            committed: false,
        })
        .collect();
    for (record, _) in pages {
        shard.write_metadata(
            record.slot_index,
            metadata_for(*record, SlotState::Reserved),
        )?;
    }
    shard.flush_metadata()?;
    hook.after(InsertStep::Reserved)?;
    write_contiguous_runs(&shard, pages)?;
    hook.after(InsertStep::Written)?;
    shard.flush()?;
    hook.after(InsertStep::Flushed)?;
    let mut verified = Vec::new();
    for (record, bytes) in pages {
        verified.resize(bytes.len(), 0);
        shard.read_slot(record.slot_index, record.logical_length, 0, &mut verified)?;
        if Some(PageHash::from_bytes(*blake3::hash(&verified).as_bytes())) != record.page_hash {
            return Err(MirageError::integrity_mismatch(
                "cache batch payload rehash differs from reservation",
            ));
        }
    }
    hook.after(InsertStep::Rehashed)?;
    for (record, _) in pages {
        shard.write_metadata(
            record.slot_index,
            metadata_for(
                CacheSlotRecord {
                    state: CacheSlotState::Resident,
                    ..*record
                },
                SlotState::Resident,
            ),
        )?;
    }
    shard.flush_metadata()?;
    let committed =
        db.commit_cache_slots_batch(pages.iter().map(|(record, _)| *record).collect())?;
    for reservation in &mut reservations {
        reservation.committed = true;
    }
    hook.after(InsertStep::MetadataCommitted)?;
    Ok(committed)
}

pub fn insert_reserved_page(
    db: &Database,
    shard: Arc<ArenaShard>,
    record: CacheSlotRecord,
    expected_hash: PageHash,
    bytes: &[u8],
    hook: &dyn InsertHook,
) -> Result<InsertOutcome, MirageError> {
    if record.state != CacheSlotState::Reserved
        || record.page_hash != Some(expected_hash)
        || record.logical_length as usize != bytes.len()
        || record.shard_id != 0
        || bytes.is_empty()
        || bytes.len() as u64 > shard.layout().page_size.as_u64()
    {
        return Err(MirageError::invalid_argument(
            "reserved cache page does not match its payload",
        ));
    }
    let mut reservation = SlotReservation {
        db: db.clone(),
        shard: Arc::clone(&shard),
        record,
        committed: false,
    };
    shard.write_metadata(record.slot_index, metadata_for(record, SlotState::Reserved))?;
    shard.flush_metadata()?;
    hook.after(InsertStep::Reserved)?;
    shard.write_slot(record.slot_index, bytes)?;
    hook.after(InsertStep::Written)?;
    shard.flush()?;
    hook.after(InsertStep::Flushed)?;
    let mut verified = vec![0_u8; bytes.len()];
    shard.read_slot(record.slot_index, record.logical_length, 0, &mut verified)?;
    if PageHash::from_bytes(*blake3::hash(&verified).as_bytes()) != expected_hash {
        return Err(MirageError::integrity_mismatch(
            "cache payload rehash differs from batch reservation",
        ));
    }
    hook.after(InsertStep::Rehashed)?;
    shard.write_metadata(
        record.slot_index,
        metadata_for(
            CacheSlotRecord {
                state: CacheSlotState::Resident,
                ..record
            },
            SlotState::Resident,
        ),
    )?;
    shard.flush_metadata()?;
    match db.commit_cache_slot(record)? {
        CommitCacheSlotOutcome::Committed(resident) => {
            reservation.committed = true;
            hook.after(InsertStep::MetadataCommitted)?;
            Ok(InsertOutcome::Inserted(resident))
        }
        CommitCacheSlotOutcome::Existing(existing) => {
            let evicting = CacheSlotRecord {
                state: CacheSlotState::Evicting,
                ..record
            };
            shard.write_metadata(
                record.slot_index,
                metadata_for(evicting, SlotState::Evicting),
            )?;
            shard.flush_metadata()?;
            if shard.deallocate_slot(record.slot_index).is_ok() {
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
                shard.flush_metadata()?;
                db.finish_cache_deallocation(evicting)?;
            } else {
                db.mark_cache_deallocation_retry(evicting)?;
            }
            reservation.committed = true;
            Ok(InsertOutcome::Existing(existing))
        }
    }
}
