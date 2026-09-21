use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};

use mirage_types::MirageError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicyKind {
    Lru,
    SegmentedLru,
    TinyLfuHybrid,
    TwoQ,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicyEvent {
    Access {
        id: u64,
        blocking: bool,
        prefetched: bool,
    },
    Pin(u64),
    Unpin(u64),
    Remove(u64),
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicyOutcome {
    Hit,
    Admitted { evicted: Option<u64> },
    Rejected,
    Updated,
}

#[derive(Debug, Clone, Copy)]
struct Entry {
    last: u64,
    frequency: u32,
    prefetched_unused: bool,
    pinned: bool,
}

/// 2Q queue state: a probationary FIFO (~25% of capacity), a bounded ghost
/// history, and a protected LRU. Membership lives here; residency and pin
/// state live in `entries`.
#[derive(Debug, Default)]
struct TwoQQueues {
    a1in: VecDeque<u64>,
    a1in_set: HashSet<u64>,
    a1out: VecDeque<u64>,
    a1out_set: HashSet<u64>,
    am_order: BTreeMap<u64, u64>,
    am_index: HashMap<u64, u64>,
}

impl TwoQQueues {
    fn drop_id(&mut self, id: u64) {
        self.a1in_set.remove(&id);
        self.a1in.retain(|entry| *entry != id);
        self.a1out_set.remove(&id);
        self.a1out.retain(|entry| *entry != id);
        if let Some(tick) = self.am_index.remove(&id) {
            self.am_order.remove(&tick);
        }
    }

    fn ghost(&mut self, id: u64, bound: usize) {
        self.a1out.push_back(id);
        self.a1out_set.insert(id);
        while self.a1out.len() > bound {
            if let Some(ghost) = self.a1out.pop_front() {
                self.a1out_set.remove(&ghost);
            }
        }
    }
}

pub struct PolicyCore {
    kind: PolicyKind,
    capacity: usize,
    clock: u64,
    entries: BTreeMap<u64, Entry>,
    history: BTreeMap<u64, u32>,
    twoq: Option<TwoQQueues>,
}
impl PolicyCore {
    pub fn new(kind: PolicyKind, capacity: usize) -> Result<Self, MirageError> {
        if capacity == 0 {
            return Err(MirageError::invalid_argument("policy capacity is zero"));
        }
        Ok(Self {
            kind,
            capacity,
            clock: 0,
            entries: BTreeMap::new(),
            history: BTreeMap::new(),
            twoq: (kind == PolicyKind::TwoQ).then(TwoQQueues::default),
        })
    }
    pub fn apply(&mut self, event: PolicyEvent) -> Result<PolicyOutcome, MirageError> {
        self.clock = self.clock.saturating_add(1);
        match event {
            PolicyEvent::Pin(id) => {
                if let Some(entry) = self.entries.get_mut(&id) {
                    entry.pinned = true;
                }
                Ok(PolicyOutcome::Updated)
            }
            PolicyEvent::Unpin(id) => {
                if let Some(entry) = self.entries.get_mut(&id) {
                    entry.pinned = false;
                }
                Ok(PolicyOutcome::Updated)
            }
            PolicyEvent::Remove(id) => {
                self.entries.remove(&id);
                if let Some(queues) = self.twoq.as_mut() {
                    queues.drop_id(id);
                }
                Ok(PolicyOutcome::Updated)
            }
            PolicyEvent::Access {
                id,
                blocking,
                prefetched,
            } => self.access(id, blocking, prefetched),
        }
    }
    fn access(
        &mut self,
        id: u64,
        blocking: bool,
        prefetched: bool,
    ) -> Result<PolicyOutcome, MirageError> {
        let frequency = self.history.entry(id).or_default();
        *frequency = frequency.saturating_add(1);
        let incoming_frequency = *frequency;
        if self.kind == PolicyKind::TwoQ {
            return self.access_twoq(id, prefetched, incoming_frequency);
        }
        if let Some(entry) = self.entries.get_mut(&id) {
            entry.last = self.clock;
            entry.frequency = incoming_frequency;
            entry.prefetched_unused = false;
            return Ok(PolicyOutcome::Hit);
        }
        let mut evicted = None;
        if self.entries.len() == self.capacity {
            let candidate = self
                .entries
                .iter()
                .filter(|(_, entry)| !entry.pinned)
                .min_by_key(|(id, entry)| {
                    let prefetch_rank =
                        if self.kind == PolicyKind::SegmentedLru && entry.prefetched_unused {
                            0
                        } else {
                            1
                        };
                    let frequency_rank = if self.kind == PolicyKind::TinyLfuHybrid {
                        entry.frequency
                    } else {
                        0
                    };
                    (prefetch_rank, frequency_rank, entry.last, **id)
                })
                .map(|(id, entry)| (*id, *entry))
                .ok_or_else(|| MirageError::cache_full("all policy residents are pinned"))?;
            if self.kind == PolicyKind::TinyLfuHybrid
                && !blocking
                && incoming_frequency <= candidate.1.frequency
            {
                return Ok(PolicyOutcome::Rejected);
            }
            self.entries.remove(&candidate.0);
            evicted = Some(candidate.0);
        }
        self.entries.insert(
            id,
            Entry {
                last: self.clock,
                frequency: incoming_frequency,
                prefetched_unused: prefetched,
                pinned: false,
            },
        );
        Ok(PolicyOutcome::Admitted { evicted })
    }

    /// 2Q admission: a resident touch hits; a probationary or ghost repeat
    /// promotes into protected space; a cold touch enters probation. Cold
    /// scans churn the small probationary queue instead of the protected
    /// working set. Probationary evictees leave residency into the ghost
    /// history; every victim reported in `Admitted` is a true residency loss.
    fn access_twoq(
        &mut self,
        id: u64,
        prefetched: bool,
        incoming_frequency: u32,
    ) -> Result<PolicyOutcome, MirageError> {
        let tick = self.clock;
        {
            let queues = self.twoq.as_mut().expect("twoq state");
            if let Some(old) = queues.am_index.get(&id).copied() {
                queues.am_order.remove(&old);
                queues.am_order.insert(tick, id);
                queues.am_index.insert(id, tick);
                if let Some(entry) = self.entries.get_mut(&id) {
                    entry.last = tick;
                    entry.frequency = incoming_frequency;
                    entry.prefetched_unused = false;
                }
                return Ok(PolicyOutcome::Hit);
            }
            if queues.a1in_set.contains(&id) {
                queues.a1in_set.remove(&id);
                queues.a1in.retain(|entry| *entry != id);
                queues.am_index.insert(id, tick);
                queues.am_order.insert(tick, id);
                if let Some(entry) = self.entries.get_mut(&id) {
                    entry.last = tick;
                    entry.frequency = incoming_frequency;
                    entry.prefetched_unused = false;
                }
                return Ok(PolicyOutcome::Hit);
            }
            if queues.a1out_set.remove(&id) {
                queues.a1out.retain(|entry| *entry != id);
                queues.am_index.insert(id, tick);
                queues.am_order.insert(tick, id);
            } else {
                queues.a1in.push_back(id);
                queues.a1in_set.insert(id);
            }
        }
        self.entries.insert(
            id,
            Entry {
                last: tick,
                frequency: incoming_frequency,
                prefetched_unused: prefetched,
                pinned: false,
            },
        );
        let probationary_bound = (self.capacity / 4).max(1);
        let mut evicted = None;
        loop {
            let (probationary, total) = {
                let queues = self.twoq.as_ref().expect("twoq state");
                (queues.a1in.len(), queues.a1in.len() + queues.am_index.len())
            };
            if probationary <= probationary_bound && total <= self.capacity {
                break;
            }
            let Some(victim) = self.twoq_evict_one(id, probationary > probationary_bound) else {
                // A refused admission must not leave the incoming id resident.
                self.entries.remove(&id);
                self.twoq.as_mut().expect("twoq state").drop_id(id);
                return Err(MirageError::cache_full("all policy residents are pinned"));
            };
            evicted = Some(victim);
        }
        Ok(PolicyOutcome::Admitted { evicted })
    }

    /// Evicts one non-pinned resident other than `exclude`: the oldest
    /// probationary entry when the probationary queue is over its bound
    /// (which ghosts it), otherwise the protected LRU. Returns `None` when
    /// nothing is evictable.
    fn twoq_evict_one(&mut self, exclude: u64, prefer_probationary: bool) -> Option<u64> {
        let pinned = |entries: &BTreeMap<u64, Entry>, id: u64| {
            entries.get(&id).is_some_and(|entry| entry.pinned)
        };
        let ghost_bound = self.capacity;
        let queues = self.twoq.as_mut().expect("twoq state");
        if prefer_probationary
            && let Some(position) = queues
                .a1in
                .iter()
                .position(|candidate| *candidate != exclude && !pinned(&self.entries, *candidate))
        {
            let victim = queues.a1in.remove(position).expect("positioned");
            queues.a1in_set.remove(&victim);
            queues.ghost(victim, ghost_bound);
            self.entries.remove(&victim);
            return Some(victim);
        }
        if let Some((&tick, &victim)) = queues
            .am_order
            .iter()
            .find(|(_, candidate)| **candidate != exclude && !pinned(&self.entries, **candidate))
        {
            queues.am_order.remove(&tick);
            queues.am_index.remove(&victim);
            self.entries.remove(&victim);
            return Some(victim);
        }
        if let Some(position) = queues
            .a1in
            .iter()
            .position(|candidate| *candidate != exclude && !pinned(&self.entries, *candidate))
        {
            let victim = queues.a1in.remove(position).expect("positioned");
            queues.a1in_set.remove(&victim);
            queues.ghost(victim, ghost_bound);
            self.entries.remove(&victim);
            return Some(victim);
        }
        None
    }

    #[must_use]
    pub fn residents(&self) -> Vec<u64> {
        self.entries.keys().copied().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn access(policy: &mut PolicyCore, id: u64) -> Result<PolicyOutcome, MirageError> {
        policy.apply(PolicyEvent::Access {
            id,
            blocking: true,
            prefetched: false,
        })
    }

    #[test]
    fn twoq_capacity_one_admits_and_evicts() {
        let mut policy = PolicyCore::new(PolicyKind::TwoQ, 1).expect("policy");
        assert_eq!(
            access(&mut policy, 1).expect("access"),
            PolicyOutcome::Admitted { evicted: None }
        );
        assert_eq!(
            access(&mut policy, 2).expect("access"),
            PolicyOutcome::Admitted { evicted: Some(1) }
        );
        assert_eq!(policy.residents(), vec![2]);
    }

    #[test]
    fn twoq_scan_does_not_evict_a_retouched_hot_set() {
        for kind in [PolicyKind::TwoQ, PolicyKind::Lru] {
            let mut policy = PolicyCore::new(kind, 8).expect("policy");
            // Establish reuse so the hot set proves itself before the scan.
            for burst in 0..2 {
                for id in 1..=4_u64 {
                    let _ = access(&mut policy, id).expect("hot touch");
                    let _ = burst;
                }
            }
            for _burst in 0..3 {
                for cold in 100..180_u64 {
                    let _ = access(&mut policy, cold).expect("cold touch");
                }
                if kind == PolicyKind::TwoQ {
                    for id in 1..=4_u64 {
                        assert!(
                            policy.residents().contains(&id),
                            "2Q must keep hot id {id} resident through the scan"
                        );
                        access(&mut policy, id).expect("hot retouch");
                    }
                } else {
                    // Plain LRU loses the hot set mid-scan; it is only
                    // re-admitted on the next explicit touch.
                    assert!(
                        !(1..=4_u64).all(|id| policy.residents().contains(&id)),
                        "LRU must have evicted part of the hot set during the scan"
                    );
                    for id in 1..=4_u64 {
                        let _ = access(&mut policy, id);
                    }
                }
            }
        }
    }

    #[test]
    fn twoq_pinned_residents_are_never_evicted() {
        let mut policy = PolicyCore::new(PolicyKind::TwoQ, 4).expect("policy");
        // Promote two ids into protected space, then pin everything resident.
        for id in [1_u64, 2] {
            access(&mut policy, id).expect("first touch");
            access(&mut policy, id).expect("promote");
        }
        access(&mut policy, 3).expect("probationary");
        for id in [1_u64, 2, 3] {
            policy.apply(PolicyEvent::Pin(id)).expect("pin");
        }
        let error = access(&mut policy, 4).expect_err("fenced");
        assert_eq!(error.kind, mirage_types::MirageErrorKind::CacheFull);
        for id in [1_u64, 2, 3] {
            assert!(policy.residents().contains(&id));
        }
        // Unpinning one probationary entry frees it as a victim.
        policy.apply(PolicyEvent::Unpin(3)).expect("unpin");
        assert_eq!(
            access(&mut policy, 4).expect("access"),
            PolicyOutcome::Admitted { evicted: Some(3) }
        );
    }
}
