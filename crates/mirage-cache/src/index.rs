use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};

use mirage_db::{CacheSlotRecord, CacheSlotState, Database};
use mirage_types::{MirageError, PageHash};

use crate::{ArenaShard, PinRegistry, ResidentPage, ResidentPageGuard};

#[derive(Default)]
pub struct ResidentIndex {
    pages: RwLock<BTreeMap<PageHash, Arc<ResidentPage>>>,
    pins: PinRegistry,
}

impl ResidentIndex {
    pub fn rebuild(db: &Database, shard: Arc<ArenaShard>) -> Result<Self, MirageError> {
        let mut index = Self::from_resident_records(db.load_resident_cache_slots()?, shard)?;
        index.pins = PinRegistry::rebuild(db)?;
        Ok(index)
    }

    pub fn from_resident_records(
        records: Vec<CacheSlotRecord>,
        shard: Arc<ArenaShard>,
    ) -> Result<Self, MirageError> {
        let mut pages = BTreeMap::new();
        for record in records {
            if record.state != CacheSlotState::Resident {
                return Err(MirageError::integrity_mismatch(
                    "cache snapshot contains a non-resident slot",
                ));
            }
            if record.shard_id != 0 {
                return Err(MirageError::unsupported_layout(
                    "resident shard routing is not configured",
                ));
            }
            let hash = record.page_hash.ok_or_else(|| {
                MirageError::integrity_mismatch("resident cache slot has no hash")
            })?;
            if pages
                .insert(
                    hash,
                    Arc::new(ResidentPage::new(record, Arc::clone(&shard))),
                )
                .is_some()
            {
                return Err(MirageError::integrity_mismatch(
                    "duplicate resident page hash",
                ));
            }
        }
        Ok(Self {
            pages: RwLock::new(pages),
            pins: PinRegistry::default(),
        })
    }
    pub fn install(
        &self,
        record: mirage_db::CacheSlotRecord,
        shard: Arc<ArenaShard>,
    ) -> Result<(), MirageError> {
        if record.state != CacheSlotState::Resident {
            return Err(MirageError::invalid_argument(
                "only resident slots can enter the index",
            ));
        }
        let hash = record
            .page_hash
            .ok_or_else(|| MirageError::invalid_argument("resident slot has no hash"))?;
        let mut pages = self
            .pages
            .write()
            .map_err(|_| MirageError::internal_invariant("resident index lock poisoned"))?;
        if pages.contains_key(&hash) {
            return Ok(());
        }
        pages.insert(hash, Arc::new(ResidentPage::new(record, shard)));
        Ok(())
    }
    pub fn acquire(&self, hash: PageHash) -> Result<Option<ResidentPageGuard>, MirageError> {
        let page = self
            .pages
            .read()
            .map_err(|_| MirageError::internal_invariant("resident index lock poisoned"))?
            .get(&hash)
            .cloned();
        match page {
            Some(page) => page.acquire(),
            None => Ok(None),
        }
    }
    pub(crate) fn page(&self, hash: PageHash) -> Result<Option<Arc<ResidentPage>>, MirageError> {
        Ok(self
            .pages
            .read()
            .map_err(|_| MirageError::internal_invariant("resident index lock poisoned"))?
            .get(&hash)
            .cloned())
    }
    pub(crate) fn remove(&self, hash: PageHash) -> Result<(), MirageError> {
        self.pages
            .write()
            .map_err(|_| MirageError::internal_invariant("resident index lock poisoned"))?
            .remove(&hash);
        Ok(())
    }
    #[must_use]
    pub fn len(&self) -> usize {
        self.pages.read().map_or(0, |pages| pages.len())
    }
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    #[must_use]
    pub const fn pins(&self) -> &PinRegistry {
        &self.pins
    }

    pub fn hashes(&self) -> Result<Vec<PageHash>, MirageError> {
        Ok(self
            .pages
            .read()
            .map_err(|_| MirageError::internal_invariant("resident index lock poisoned"))?
            .keys()
            .copied()
            .collect())
    }

    pub fn active_read_leases(&self, hash: PageHash) -> Result<Option<u32>, MirageError> {
        Ok(self.page(hash)?.map(|page| page.active_read_leases()))
    }
}
