use std::collections::BTreeSet;

use mirage_predictor::NormalizedTouch;
use mirage_types::{MirageError, PageOrdinal};

use crate::metrics::percentile;
use crate::{FixedPageCache, NetworkModel, ReplayMetrics, SimTime};

#[derive(Debug, Clone)]
pub struct ReplayConfig {
    pub cache_pages: usize,
    pub page_bytes: u64,
    pub pinned_pages: BTreeSet<PageOrdinal>,
    pub network: NetworkModel,
}

pub struct BaselineReplay {
    config: ReplayConfig,
    cache: FixedPageCache,
    blocked_until: SimTime,
    hits: u64,
    misses: u64,
    blocking_ns: u64,
    remote_bytes: u64,
    writes: u64,
    eviction_before_reuse: u64,
    peak: usize,
    stalls: Vec<u64>,
    evicted: BTreeSet<PageOrdinal>,
}

impl BaselineReplay {
    pub fn new(config: ReplayConfig) -> Result<Self, MirageError> {
        let cache = FixedPageCache::new(config.cache_pages, config.pinned_pages.iter().copied())?;
        let peak = cache.len();
        Ok(Self {
            config,
            cache,
            blocked_until: SimTime::ZERO,
            hits: 0,
            misses: 0,
            blocking_ns: 0,
            remote_bytes: 0,
            writes: 0,
            eviction_before_reuse: 0,
            peak,
            stalls: Vec::new(),
            evicted: BTreeSet::new(),
        })
    }
    pub fn process(&mut self, touch: NormalizedTouch) -> Result<(), MirageError> {
        let access = self.cache.access(touch.page_ordinal)?;
        if access.hit {
            self.hits += 1;
            self.stalls.push(0);
            return Ok(());
        }
        self.misses += 1;
        if self.evicted.remove(&touch.page_ordinal) {
            self.eviction_before_reuse += 1;
        }
        if let Some(victim) = access.evicted {
            self.evicted.insert(victim);
        }
        let arrival = SimTime::from_ns(touch.timestamp_ns).max(self.blocked_until);
        let outcome =
            self.config
                .network
                .fetch(arrival, self.config.page_bytes, self.misses - 1, 1)?;
        if outcome.failed {
            return Err(MirageError::backend_unavailable(
                "scripted network failure during replay",
            ));
        }
        let stall = outcome
            .complete_at
            .as_ns()
            .saturating_sub(touch.timestamp_ns);
        self.blocked_until = outcome.complete_at;
        self.blocking_ns = self
            .blocking_ns
            .checked_add(stall)
            .ok_or_else(|| MirageError::invalid_argument("blocking time overflows"))?;
        self.remote_bytes = self
            .remote_bytes
            .checked_add(self.config.page_bytes)
            .ok_or_else(|| MirageError::invalid_argument("remote byte count overflows"))?;
        self.writes += 1;
        self.stalls.push(stall);
        self.peak = self.peak.max(self.cache.len());
        Ok(())
    }
    pub fn finish(mut self) -> ReplayMetrics {
        let accesses = self.hits + self.misses;
        let hit_rate = if accesses == 0 {
            0.0
        } else {
            self.hits as f64 / accesses as f64
        };
        let mut p95 = self.stalls.clone();
        ReplayMetrics {
            accesses,
            hits: self.hits,
            misses: self.misses,
            hit_rate,
            blocking_ns: self.blocking_ns,
            p95_stall_ns: percentile(&mut p95, 95),
            p99_stall_ns: percentile(&mut self.stalls, 99),
            remote_bytes: self.remote_bytes,
            cache_writes: self.writes,
            eviction_before_reuse: self.eviction_before_reuse,
            peak_resident_pages: self.peak as u64,
        }
    }
}
