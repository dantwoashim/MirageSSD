use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use mirage_db::CacheSlotRecord;
use mirage_types::{MirageError, PageHash};

use crate::{ArenaShard, ResidentPageGuard};

const RESIDENT: u64 = 2_u64 << 32;
const EVICTING: u64 = 3_u64 << 32;
const LEASE_MASK: u64 = u32::MAX as u64;

pub struct ResidentPage {
    pub(crate) record: CacheSlotRecord,
    pub(crate) shard: Arc<ArenaShard>,
    gate: AtomicU64,
}

impl ResidentPage {
    pub(crate) fn new(record: CacheSlotRecord, shard: Arc<ArenaShard>) -> Self {
        Self {
            record,
            shard,
            gate: AtomicU64::new(RESIDENT),
        }
    }
    #[must_use]
    pub fn hash(&self) -> PageHash {
        self.record.page_hash.expect("resident page has hash")
    }
    pub fn acquire(self: &Arc<Self>) -> Result<Option<ResidentPageGuard>, MirageError> {
        loop {
            let current = self.gate.load(Ordering::Acquire);
            if current >> 32 != 2 {
                return Ok(None);
            }
            if current & LEASE_MASK == LEASE_MASK {
                return Err(MirageError::cache_full(
                    "resident page lease count is saturated",
                ));
            }
            if self
                .gate
                .compare_exchange_weak(current, current + 1, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                return Ok(Some(ResidentPageGuard {
                    page: Arc::clone(self),
                }));
            }
        }
    }
    pub(crate) fn release(&self) {
        let previous = self.gate.fetch_sub(1, Ordering::AcqRel);
        debug_assert_eq!(previous >> 32, 2);
        debug_assert!(previous & LEASE_MASK > 0);
    }
    #[must_use]
    pub fn active_read_leases(&self) -> u32 {
        let current = self.gate.load(Ordering::Acquire);
        if current >> 32 == 2 {
            (current & LEASE_MASK) as u32
        } else {
            0
        }
    }
    pub(crate) fn begin_eviction(&self) -> Result<(), MirageError> {
        self.gate
            .compare_exchange(RESIDENT, EVICTING, Ordering::AcqRel, Ordering::Acquire)
            .map(|_| ())
            .map_err(|_| {
                MirageError::cache_full(
                    "resident page has active read leases or is already evicting",
                )
            })
    }
    pub(crate) fn cancel_eviction(&self) {
        let result =
            self.gate
                .compare_exchange(EVICTING, RESIDENT, Ordering::AcqRel, Ordering::Acquire);
        debug_assert!(result.is_ok());
    }
}
