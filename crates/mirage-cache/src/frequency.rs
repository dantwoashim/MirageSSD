use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};

use mirage_types::{MirageError, PageHash};

use crate::doorkeeper::{Doorkeeper, index};

const ROWS: usize = 4;

pub struct FrequencySketch {
    counters: Box<[AtomicU8]>,
    width: usize,
    mask: usize,
    seed: u64,
    events: AtomicU64,
    age_after: u64,
    aging: AtomicBool,
    doorkeeper: Doorkeeper,
}

impl FrequencySketch {
    pub fn new(width: usize, seed: u64, age_after: u64) -> Result<Self, MirageError> {
        if width < 16 || !width.is_power_of_two() || age_after == 0 {
            return Err(MirageError::invalid_argument(
                "frequency width and age interval are invalid",
            ));
        }
        let count = width
            .checked_mul(ROWS)
            .ok_or_else(|| MirageError::invalid_argument("frequency allocation overflows"))?;
        Ok(Self {
            counters: (0..count).map(|_| AtomicU8::new(0)).collect(),
            width,
            mask: width - 1,
            seed,
            events: AtomicU64::new(0),
            age_after,
            aging: AtomicBool::new(false),
            doorkeeper: Doorkeeper::new(width, seed ^ 0xd00f_5eed)?,
        })
    }

    pub fn record(&self, hash: PageHash) {
        self.events.fetch_add(1, Ordering::Relaxed);
        if !self.doorkeeper.admit(hash) {
            return;
        }
        for row in 0..ROWS {
            let cell = &self.counters[row * self.width + self.row_index(hash, row)];
            let _ = cell.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                Some(value.saturating_add(1))
            });
        }
    }

    #[must_use]
    pub fn estimate(&self, hash: PageHash) -> u32 {
        let counter = (0..ROWS)
            .map(|row| {
                self.counters[row * self.width + self.row_index(hash, row)].load(Ordering::Relaxed)
            })
            .min()
            .unwrap_or(0);
        u32::from(counter) + u32::from(self.doorkeeper.contains(hash))
    }

    pub fn age_if_needed(&self) -> bool {
        if self.events.load(Ordering::Relaxed) < self.age_after
            || self
                .aging
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Relaxed)
                .is_err()
        {
            return false;
        }
        if self.events.swap(0, Ordering::AcqRel) >= self.age_after {
            for counter in &self.counters {
                counter
                    .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                        Some(value >> 1)
                    })
                    .ok();
            }
            self.doorkeeper.clear();
        }
        self.aging.store(false, Ordering::Release);
        true
    }

    #[must_use]
    pub fn memory_bytes(&self) -> usize {
        self.counters.len() + self.width / 8
    }

    fn row_index(&self, hash: PageHash, row: usize) -> usize {
        index(
            hash,
            self.seed ^ (row as u64 + 1).wrapping_mul(0x9e37_79b9),
            self.mask,
        )
    }
}
