use crate::{SlotMetadata, SlotState};
use mirage_db::CacheSlotRecord;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SlotRef(pub CacheSlotRecord);

pub(crate) fn metadata_for(record: CacheSlotRecord, state: SlotState) -> SlotMetadata {
    SlotMetadata {
        generation: record.generation,
        state,
        page_hash: record
            .page_hash
            .unwrap_or_else(|| mirage_types::PageHash::from_bytes([0; 32])),
        logical_length: record.logical_length,
    }
}
