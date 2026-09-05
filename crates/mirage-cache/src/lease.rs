use std::sync::Arc;

use mirage_types::MirageError;

use crate::ResidentPage;

pub struct ResidentPageGuard {
    pub(crate) page: Arc<ResidentPage>,
}

impl ResidentPageGuard {
    #[must_use]
    pub fn logical_length(&self) -> u32 {
        self.page.record.logical_length
    }
    pub fn read_exact(&self, offset: u32, output: &mut [u8]) -> Result<(), MirageError> {
        self.page.shard.read_slot(
            self.page.record.slot_index,
            self.page.record.logical_length,
            offset,
            output,
        )
    }
}

impl Drop for ResidentPageGuard {
    fn drop(&mut self) {
        self.page.release();
    }
}
