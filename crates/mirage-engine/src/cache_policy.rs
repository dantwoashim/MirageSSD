//! Scan-resistant admission: a 2Q policy (A1in FIFO + A1out ghost + Am LRU)
//! plus a sequential bypass. A page enters probationary space on first touch
//! and only promotes into protected space when a *repeat* reference proves
//! reuse — a sequential scan through cold data churns A1in without evicting
//! protected pages. Streams detected as purely sequential bypass admission
//! entirely.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};

use mirage_types::PageHash;

/// 2Q admission policy over a bounded page capacity.
pub struct TwoQ {
    /// Probationary FIFO: first-touch pages live here briefly.
    a1in: VecDeque<PageHash>,
    a1in_set: HashSet<PageHash>,
    a1in_capacity: usize,
    /// Ghost entries: pages evicted from A1in that were seen once.
    a1out: VecDeque<PageHash>,
    a1out_set: HashSet<PageHash>,
    a1out_capacity: usize,
    /// Protected LRU: pages with proven reuse.
    am_order: BTreeMap<u64, PageHash>,
    am_index: HashMap<PageHash, u64>,
    am_capacity: usize,
    tick: u64,
}

/// What a reference resolved to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Admission {
    /// Page is resident already; no action.
    Hit,
    /// Admit into probationary space.
    Probation,
    /// Admit directly into protected space (ghost hit).
    Protect,
    /// Evict this candidate to make room.
    Evict(PageHash),
    /// Sequential stream — do not admit at all.
    Bypass,
}

impl TwoQ {
    /// `capacity` is the total resident-page budget. A1in gets ~25%, Am the
    /// rest, and the ghost list tracks ~50% of capacity worth of history.
    pub fn new(capacity: usize) -> Self {
        Self {
            a1in: VecDeque::new(),
            a1in_set: HashSet::new(),
            a1in_capacity: (capacity / 4).max(1),
            a1out: VecDeque::new(),
            a1out_set: HashSet::new(),
            a1out_capacity: (capacity / 2).max(1),
            am_order: BTreeMap::new(),
            am_index: HashMap::new(),
            am_capacity: capacity.saturating_sub(capacity / 4).max(1),
            tick: 0,
        }
    }

    /// True when the page is admitted anywhere.
    pub fn resident(&self, hash: &PageHash) -> bool {
        self.a1in_set.contains(hash) || self.am_index.contains_key(hash)
    }

    /// Records a reference; the returned admission decision drives eviction.
    pub fn touch(&mut self, hash: PageHash) -> Admission {
        self.tick += 1;
        if self.am_index.contains_key(&hash) {
            // Protected LRU refresh.
            if let Some(old) = self.am_index.insert(hash, self.tick) {
                self.am_order.remove(&old);
            }
            self.am_order.insert(self.tick, hash);
            return Admission::Hit;
        }
        if self.a1in_set.contains(&hash) {
            // Second touch inside the probationary window — promote.
            self.a1in_set.remove(&hash);
            if let Some(pos) = self.a1in.iter().position(|entry| *entry == hash) {
                self.a1in.remove(pos);
            }
            return self.promote(hash);
        }
        if self.a1out_set.contains(&hash) {
            // Ghost hit: the page was evicted but reused — protect it.
            self.a1out_set.remove(&hash);
            if let Some(pos) = self.a1out.iter().position(|entry| *entry == hash) {
                self.a1out.remove(pos);
            }
            return self.promote(hash);
        }
        // First touch: probationary admission, possibly evicting into ghost.
        let evicted = if self.a1in.len() >= self.a1in_capacity {
            self.a1in.pop_front()
        } else {
            None
        };
        if let Some(evicted) = evicted {
            self.a1in_set.remove(&evicted);
            self.a1out.push_back(evicted);
            self.a1out_set.insert(evicted);
            if self.a1out.len() > self.a1out_capacity
                && let Some(ghost) = self.a1out.pop_front()
            {
                self.a1out_set.remove(&ghost);
            }
        }
        self.a1in.push_back(hash);
        self.a1in_set.insert(hash);
        evicted
            .map(Admission::Evict)
            .unwrap_or(Admission::Probation)
    }

    /// Removes a page from every queue (evicted or invalidated).
    pub fn remove(&mut self, hash: &PageHash) {
        self.a1in_set.remove(hash);
        self.a1in.retain(|entry| entry != hash);
        self.a1out_set.remove(hash);
        self.a1out.retain(|entry| entry != hash);
        if let Some(tick) = self.am_index.remove(hash) {
            self.am_order.remove(&tick);
        }
    }

    /// The least-recently-used protected page — eviction candidate for
    /// pressure relief. A1in eviction happens inside `touch`; protected
    /// space is only trimmed when Am overflows.
    pub fn protected_lru(&self) -> Option<PageHash> {
        self.am_order.keys().next().map(|tick| self.am_order[tick])
    }

    fn promote(&mut self, hash: PageHash) -> Admission {
        let evicted = if self.am_index.len() >= self.am_capacity {
            self.am_order
                .keys()
                .next()
                .copied()
                .and_then(|tick| self.am_order.remove(&tick))
                .inspect(|page| {
                    self.am_index.remove(page);
                })
        } else {
            None
        };
        self.am_index.insert(hash, self.tick);
        self.am_order.insert(self.tick, hash);
        evicted.map(Admission::Evict).unwrap_or(Admission::Protect)
    }

    /// Sizes for diagnostics: (probationary, protected, ghost).
    pub fn occupancy(&self) -> (usize, usize, usize) {
        (self.a1in.len(), self.am_index.len(), self.a1out.len())
    }
}

/// Detects purely sequential streams per key (file handle, inode) so bulk
/// sequential traffic bypasses admission entirely — scan resistance.
#[derive(Default)]
pub struct SequentialDetector {
    /// key → (next expected offset, run length)
    runs: HashMap<u64, (u64, u64)>,
    /// Streams at/above this many sequential bytes bypass admission.
    pub bypass_threshold: u64,
}

impl SequentialDetector {
    pub fn new(bypass_threshold: u64) -> Self {
        Self {
            runs: HashMap::new(),
            bypass_threshold,
        }
    }

    /// Records a read of `length` at `offset` for stream `key`; returns true
    /// when the stream is now sequential enough to bypass admission.
    pub fn observe(&mut self, key: u64, offset: u64, length: u64) -> bool {
        let entry = self.runs.entry(key).or_insert((offset, 0));
        if offset == entry.0 {
            entry.0 = offset + length;
            entry.1 += length;
        } else {
            // Non-sequential access resets the run — random readers keep
            // normal admission.
            *entry = (offset + length, length);
        }
        entry.1 >= self.bypass_threshold
    }

    /// Drops stream state for a closed handle.
    pub fn close(&mut self, key: u64) {
        self.runs.remove(&key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hash(byte: u8) -> PageHash {
        PageHash::from_bytes([byte; 32])
    }

    #[test]
    fn sequential_scan_does_not_evict_protected_pages() {
        let mut policy = TwoQ::new(8);
        // Establish reuse: touch h1..h4 twice each so they promote.
        for byte in 1..=4u8 {
            policy.touch(hash(byte));
            policy.touch(hash(byte));
        }
        let (probation, protected, _) = policy.occupancy();
        assert_eq!(protected, 4, "reused pages must promote");
        let _ = probation;
        // A sequential scan of cold pages floods probation but protected
        // pages survive.
        for byte in 10..30u8 {
            policy.touch(hash(byte));
        }
        for byte in 1..=4u8 {
            assert!(
                policy.resident(&hash(byte)),
                "protected page {byte} must survive the scan"
            );
        }
    }

    #[test]
    fn ghost_hit_promotes_to_protected() {
        let mut policy = TwoQ::new(4);
        policy.touch(hash(1)); // probation
        // Force h1 out of A1in into the ghost list, staying inside its bound.
        policy.touch(hash(2));
        policy.touch(hash(3));
        // Re-touching a ghosted page promotes it straight to protected.
        let decision = policy.touch(hash(1));
        assert!(matches!(decision, Admission::Protect | Admission::Evict(_)));
        assert!(policy.am_index.contains_key(&hash(1)));
    }

    #[test]
    fn sequential_detector_bypasses_only_long_runs() {
        let mut detector = SequentialDetector::new(64);
        assert!(!detector.observe(7, 0, 32));
        // Random access resets the run — no bypass.
        assert!(!detector.observe(7, 1024, 32));
        assert!(!detector.observe(7, 2048, 32));
        // Sequential run crosses the threshold.
        assert!(detector.observe(7, 2080, 64));
    }
}
