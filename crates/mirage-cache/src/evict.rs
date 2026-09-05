use mirage_db::{CacheSlotRecord, CacheSlotState, Database};
use mirage_types::{MirageError, PageHash};

use crate::slot::metadata_for;
use crate::{ResidentIndex, SlotState};

impl ResidentIndex {
    pub fn evict(&self, db: &Database, hash: PageHash) -> Result<bool, MirageError> {
        self.evict_inner(db, hash, false, |page| {
            page.shard.deallocate_slot(page.record.slot_index)
        })
    }

    pub fn quarantine_corrupt(&self, db: &Database, hash: PageHash) -> Result<bool, MirageError> {
        self.evict_inner(db, hash, true, |page| {
            page.shard.deallocate_slot(page.record.slot_index)
        })
    }

    pub fn evict_with_deallocator(
        &self,
        db: &Database,
        hash: PageHash,
        deallocate: impl FnOnce(&crate::ResidentPage) -> Result<(), MirageError>,
    ) -> Result<bool, MirageError> {
        self.evict_inner(db, hash, false, deallocate)
    }

    fn evict_inner(
        &self,
        db: &Database,
        hash: PageHash,
        ignore_pins: bool,
        deallocate: impl FnOnce(&crate::ResidentPage) -> Result<(), MirageError>,
    ) -> Result<bool, MirageError> {
        let Some(page) = self.page(hash)? else {
            return Ok(false);
        };
        if !ignore_pins && self.pins().is_pinned(hash)? {
            return Err(MirageError::cache_full(
                "pinned resident page cannot be evicted",
            ));
        }
        page.begin_eviction()?;
        let evicting = match db.begin_cache_eviction(page.record) {
            Ok(value) => value,
            Err(error) => {
                page.cancel_eviction();
                return Err(error);
            }
        };
        page.shard.write_metadata(
            page.record.slot_index,
            metadata_for(evicting, SlotState::Evicting),
        )?;
        page.shard.flush_metadata()?;
        self.remove(hash)?;
        match deallocate(&page) {
            Ok(()) => {
                page.shard.write_metadata(
                    page.record.slot_index,
                    metadata_for(
                        CacheSlotRecord {
                            state: CacheSlotState::Free,
                            page_hash: None,
                            logical_length: 0,
                            ..evicting
                        },
                        SlotState::Free,
                    ),
                )?;
                page.shard.flush_metadata()?;
                db.finish_cache_deallocation(evicting)?;
            }
            Err(error) => {
                db.mark_cache_deallocation_retry(CacheSlotRecord {
                    state: CacheSlotState::Evicting,
                    ..evicting
                })?;
                return Err(error);
            }
        }
        Ok(true)
    }
}
