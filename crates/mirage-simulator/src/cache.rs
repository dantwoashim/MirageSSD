use std::collections::{BTreeMap, BTreeSet};

use mirage_types::{MirageError, PageOrdinal};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CacheAccess {
    pub hit: bool,
    pub evicted: Option<PageOrdinal>,
}

#[derive(Debug)]
pub struct FixedPageCache {
    capacity: usize,
    clock: u64,
    resident: BTreeMap<PageOrdinal, u64>,
    pins: BTreeSet<PageOrdinal>,
}

impl FixedPageCache {
    pub fn new(
        capacity: usize,
        pins: impl IntoIterator<Item = PageOrdinal>,
    ) -> Result<Self, MirageError> {
        if capacity == 0 {
            return Err(MirageError::invalid_argument(
                "cache capacity must be non-zero",
            ));
        }
        let pins = pins.into_iter().collect::<BTreeSet<_>>();
        if pins.len() > capacity {
            return Err(MirageError::invalid_argument(
                "pinned pages exceed cache capacity",
            ));
        }
        let resident = pins
            .iter()
            .copied()
            .enumerate()
            .map(|(index, page)| (page, index as u64))
            .collect();
        Ok(Self {
            capacity,
            clock: pins.len() as u64,
            resident,
            pins,
        })
    }
    #[must_use]
    pub fn len(&self) -> usize {
        self.resident.len()
    }
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.resident.is_empty()
    }
    #[must_use]
    pub const fn capacity(&self) -> usize {
        self.capacity
    }
    pub fn access(&mut self, page: PageOrdinal) -> Result<CacheAccess, MirageError> {
        self.clock = self
            .clock
            .checked_add(1)
            .ok_or_else(|| MirageError::internal_invariant("cache access clock overflows"))?;
        if let Some(last) = self.resident.get_mut(&page) {
            *last = self.clock;
            return Ok(CacheAccess {
                hit: true,
                evicted: None,
            });
        }
        let evicted = if self.resident.len() == self.capacity {
            let victim = self
                .resident
                .iter()
                .filter(|(candidate, _)| !self.pins.contains(candidate))
                .min_by_key(|(candidate, last)| (**last, **candidate))
                .map(|(page, _)| *page)
                .ok_or_else(|| MirageError::cache_full("all cache slots are pinned"))?;
            self.resident.remove(&victim);
            Some(victim)
        } else {
            None
        };
        self.resident.insert(page, self.clock);
        debug_assert!(self.resident.len() <= self.capacity);
        Ok(CacheAccess {
            hit: false,
            evicted,
        })
    }
}
