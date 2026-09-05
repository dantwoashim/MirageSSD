use std::collections::{BTreeSet, VecDeque};

use mirage_types::{MirageError, PageHash};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GhostKind {
    Window,
    Probationary,
    Protected,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GhostMetrics {
    pub evicted_before_reuse_bytes: u64,
    pub reuse_latency_buckets: [u64; 8],
}

struct GhostSet {
    order: VecDeque<(PageHash, u64, u64)>,
    members: BTreeSet<PageHash>,
}
impl GhostSet {
    fn new() -> Self {
        Self {
            order: VecDeque::new(),
            members: BTreeSet::new(),
        }
    }
}

pub struct GhostHistory {
    sets: [GhostSet; 3],
    capacity_each: usize,
    sequence: u64,
    metrics: GhostMetrics,
}
impl GhostHistory {
    pub fn new(capacity: usize) -> Result<Self, MirageError> {
        if capacity < 3 {
            return Err(MirageError::invalid_argument(
                "ghost capacity must cover all segments",
            ));
        }
        Ok(Self {
            sets: [GhostSet::new(), GhostSet::new(), GhostSet::new()],
            capacity_each: capacity / 3,
            sequence: 0,
            metrics: GhostMetrics::default(),
        })
    }
    pub fn record_eviction(&mut self, kind: GhostKind, hash: PageHash, bytes: u64) {
        self.sequence = self.sequence.saturating_add(1);
        let set = &mut self.sets[index(kind)];
        if set.members.insert(hash) {
            set.order.push_back((hash, self.sequence, bytes));
        }
        while set.order.len() > self.capacity_each {
            if let Some((old, _, _)) = set.order.pop_front() {
                set.members.remove(&old);
            }
        }
    }
    pub fn reuse(&mut self, hash: PageHash) -> Option<GhostKind> {
        for (position, kind) in [
            GhostKind::Window,
            GhostKind::Probationary,
            GhostKind::Protected,
        ]
        .into_iter()
        .enumerate()
        {
            if self.sets[position].members.remove(&hash) {
                if let Some(offset) = self.sets[position]
                    .order
                    .iter()
                    .position(|(candidate, _, _)| *candidate == hash)
                {
                    let (_, sequence, bytes) = self.sets[position]
                        .order
                        .remove(offset)
                        .expect("position exists");
                    self.metrics.evicted_before_reuse_bytes = self
                        .metrics
                        .evicted_before_reuse_bytes
                        .saturating_add(bytes);
                    let delta = self.sequence.saturating_sub(sequence);
                    let bucket = usize::try_from(delta.max(1).ilog2()).unwrap_or(7).min(7);
                    self.metrics.reuse_latency_buckets[bucket] += 1;
                }
                return Some(kind);
            }
        }
        None
    }
    #[must_use]
    pub const fn metrics(&self) -> GhostMetrics {
        self.metrics
    }
    #[must_use]
    pub fn len(&self) -> usize {
        self.sets.iter().map(|set| set.order.len()).sum()
    }
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    pub fn reset(&mut self) {
        for set in &mut self.sets {
            set.order.clear();
            set.members.clear();
        }
        self.metrics = GhostMetrics::default();
    }
}
const fn index(kind: GhostKind) -> usize {
    match kind {
        GhostKind::Window => 0,
        GhostKind::Probationary => 1,
        GhostKind::Protected => 2,
    }
}
