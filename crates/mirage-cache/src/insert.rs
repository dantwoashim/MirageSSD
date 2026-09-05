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
