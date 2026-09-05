use mirage_types::MirageError;

use crate::{ArenaShard, CacheLayout};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CacheUsage {
    pub resident_logical_bytes: u64,
    pub dirty_bytes: u64,
    pub pinned_bytes: u64,
    pub ntfs_allocated_bytes: u64,
    pub db_journal_allowance: u64,
    pub filesystem_reserve: u64,
    pub hard_budget_bytes: u64,
    pub headroom_bytes: u64,
}

impl CacheUsage {
    pub fn measure(
        shard: &ArenaShard,
        layout: CacheLayout,
        resident_logical_bytes: u64,
        dirty_bytes: u64,
        pinned_bytes: u64,
        hard_budget_bytes: u64,
    ) -> Result<Self, MirageError> {
        let allocated = shard.diagnostics()?.allocated_bytes;
        let committed = allocated
            .checked_add(layout.db_journal_allowance.as_u64())
            .and_then(|value| value.checked_add(layout.filesystem_reserve.as_u64()))
            .ok_or_else(|| MirageError::invalid_argument("cache physical accounting overflows"))?;
        Ok(Self {
            resident_logical_bytes,
            dirty_bytes,
            pinned_bytes,
            ntfs_allocated_bytes: allocated,
            db_journal_allowance: layout.db_journal_allowance.as_u64(),
            filesystem_reserve: layout.filesystem_reserve.as_u64(),
            hard_budget_bytes,
            headroom_bytes: hard_budget_bytes.saturating_sub(committed),
        })
    }
}
