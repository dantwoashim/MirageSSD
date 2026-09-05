use std::collections::BTreeMap;

use mirage_types::MirageError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicyKind {
    Lru,
    SegmentedLru,
    TinyLfuHybrid,
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

pub struct PolicyCore {
    kind: PolicyKind,
    capacity: usize,
    clock: u64,
    entries: BTreeMap<u64, Entry>,
    history: BTreeMap<u64, u32>,
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
    #[must_use]
    pub fn residents(&self) -> Vec<u64> {
        self.entries.keys().copied().collect()
    }
}
