use std::sync::Arc;

use mirage_db::{CacheSlotRecord, CacheSlotState, Database};

use crate::slot::metadata_for;
use crate::{ArenaShard, SlotState};

pub(crate) struct SlotReservation {
    pub db: Database,
    pub shard: Arc<ArenaShard>,
    pub record: CacheSlotRecord,
    pub committed: bool,
}

impl SlotReservation {
    pub fn quarantine_and_deallocate(&mut self) {
        if self.committed {
            return;
        }
        if self.db.release_cache_reservation(self.record).is_err() {
            return;
        }
        let evicting = CacheSlotRecord {
            state: CacheSlotState::Evicting,
            ..self.record
        };
        let _ = self.shard.write_metadata(
            self.record.slot_index,
            metadata_for(evicting, SlotState::Evicting),
        );
        let _ = self.shard.flush_metadata();
        if self.shard.deallocate_slot(self.record.slot_index).is_ok() {
            let _ = self.shard.write_metadata(
                self.record.slot_index,
                metadata_for(
                    CacheSlotRecord {
                        state: CacheSlotState::Free,
                        page_hash: None,
                        logical_length: 0,
                        ..self.record
                    },
                    SlotState::Free,
                ),
            );
            let _ = self.shard.flush_metadata();
            let _ = self.db.finish_cache_deallocation(evicting);
        } else {
            let _ = self.db.mark_cache_deallocation_retry(evicting);
        }
        self.committed = true;
    }
}

impl Drop for SlotReservation {
    fn drop(&mut self) {
        self.quarantine_and_deallocate();
    }
}
