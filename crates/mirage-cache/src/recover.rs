use mirage_db::{CacheSlotRecord, CacheSlotState};

use crate::{SlotMetadata, SlotState};

pub(crate) fn metadata_matches(record: CacheSlotRecord, metadata: SlotMetadata) -> bool {
    record.generation == metadata.generation
        && record.logical_length == metadata.logical_length
        && record
            .page_hash
            .unwrap_or_else(|| mirage_types::PageHash::from_bytes([0; 32]))
            == metadata.page_hash
        && matches!(
            (record.state, metadata.state),
            (CacheSlotState::Free, SlotState::Free)
                | (CacheSlotState::Reserved, SlotState::Reserved)
                | (CacheSlotState::Resident, SlotState::Resident)
                | (CacheSlotState::Evicting, SlotState::Evicting)
                | (CacheSlotState::RetryDeallocate, SlotState::RetryDeallocate)
        )
}
