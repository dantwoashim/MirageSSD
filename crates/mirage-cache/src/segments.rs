use std::collections::BTreeMap;
use std::sync::mpsc::Receiver;

use mirage_types::MirageError;

use crate::recency::RecencyEvent;
pub use crate::recency::{TouchQueue, TouchQueueMetrics};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Segment {
    Window,
    Probationary,
    Protected,
    PrefetchedUnused,
    Detached,
}

#[derive(Debug, Clone, Copy)]
struct Node {
    segment: Segment,
    prev: Option<u32>,
    next: Option<u32>,
}
#[derive(Debug, Clone, Copy, Default)]
struct List {
    head: Option<u32>,
    tail: Option<u32>,
    len: usize,
}

pub struct SegmentedRecency {
    nodes: BTreeMap<u32, Node>,
    lists: [List; 4],
    protected_target: usize,
    receiver: Receiver<RecencyEvent>,
}

impl SegmentedRecency {
    pub fn new(protected_target: usize, queue_capacity: usize) -> (Self, TouchQueue) {
        let (queue, receiver) = TouchQueue::bounded(queue_capacity);
        (
            Self {
                nodes: BTreeMap::new(),
                lists: [List::default(); 4],
                protected_target,
                receiver,
            },
            queue,
        )
    }
    pub fn insert(&mut self, slot: u32, segment: Segment) -> Result<(), MirageError> {
        if segment == Segment::Detached || self.nodes.contains_key(&slot) {
            return Err(MirageError::invalid_argument(
                "recency insertion is invalid",
            ));
        }
        self.nodes.insert(
            slot,
            Node {
                segment: Segment::Detached,
                prev: None,
                next: None,
            },
        );
        self.push_head(slot, segment)
    }
    pub fn drain(&mut self) -> Result<usize, MirageError> {
        let events = self.receiver.try_iter().collect::<Vec<_>>();
        for event in &events {
            self.apply(*event)?;
        }
        Ok(events.len())
    }
    pub fn victim(&self) -> Option<u32> {
        [
            Segment::PrefetchedUnused,
            Segment::Probationary,
            Segment::Window,
            Segment::Protected,
        ]
        .into_iter()
        .find_map(|segment| self.list(segment).tail)
    }
    pub fn segment(&self, slot: u32) -> Option<Segment> {
        self.nodes.get(&slot).map(|node| node.segment)
    }
    pub fn verify(&self) -> Result<(), MirageError> {
        let mut seen = 0_usize;
        for segment in [
            Segment::Window,
            Segment::Probationary,
            Segment::Protected,
            Segment::PrefetchedUnused,
        ] {
            let list = self.list(segment);
            let mut current = list.head;
            let mut previous = None;
            let mut count = 0;
            while let Some(slot) = current {
                let node = self
                    .nodes
                    .get(&slot)
                    .ok_or_else(|| MirageError::internal_invariant("recency link is missing"))?;
                if node.segment != segment || node.prev != previous {
                    return Err(MirageError::internal_invariant("recency list is corrupt"));
                }
                previous = Some(slot);
                current = node.next;
                count += 1;
                if count > self.nodes.len() {
                    return Err(MirageError::internal_invariant(
                        "recency list contains a cycle",
                    ));
                }
            }
            if previous != list.tail || count != list.len {
                return Err(MirageError::internal_invariant(
                    "recency endpoints disagree",
                ));
            }
            seen += count;
        }
        let detached = self
            .nodes
            .values()
            .filter(|node| node.segment == Segment::Detached)
            .count();
        if seen + detached != self.nodes.len() {
            return Err(MirageError::internal_invariant(
                "recency membership disagrees",
            ));
        }
        Ok(())
    }
    fn apply(&mut self, event: RecencyEvent) -> Result<(), MirageError> {
        let slot = match event {
            RecencyEvent::Touch(slot) | RecencyEvent::Pin(slot) | RecencyEvent::Unpin(slot) => slot,
        };
        let Some(segment) = self.segment(slot) else {
            return Ok(());
        };
        match event {
            RecencyEvent::Pin(_) => self.detach(slot),
            RecencyEvent::Unpin(_) => {
                if segment == Segment::Detached {
                    self.push_head(slot, Segment::Probationary)?;
                }
                Ok(())
            }
            RecencyEvent::Touch(_) => {
                let target = match segment {
                    Segment::Probationary | Segment::PrefetchedUnused => Segment::Protected,
                    Segment::Window => Segment::Window,
                    Segment::Protected => Segment::Protected,
                    Segment::Detached => return Ok(()),
                };
                self.detach(slot)?;
                self.push_head(slot, target)?;
                if self.list(Segment::Protected).len > self.protected_target
                    && let Some(tail) = self.list(Segment::Protected).tail
                {
                    self.detach(tail)?;
                    self.push_head(tail, Segment::Probationary)?;
                }
                Ok(())
            }
        }
    }
    fn push_head(&mut self, slot: u32, segment: Segment) -> Result<(), MirageError> {
        let index = segment_index(segment)?;
        let old = self.lists[index].head;
        {
            let node = self
                .nodes
                .get_mut(&slot)
                .ok_or_else(|| MirageError::internal_invariant("recency node missing"))?;
            node.segment = segment;
            node.prev = None;
            node.next = old;
        }
        if let Some(old) = old {
            self.nodes.get_mut(&old).expect("head exists").prev = Some(slot);
        } else {
            self.lists[index].tail = Some(slot);
        }
        self.lists[index].head = Some(slot);
        self.lists[index].len += 1;
        Ok(())
    }
    fn detach(&mut self, slot: u32) -> Result<(), MirageError> {
        let node = *self
            .nodes
            .get(&slot)
            .ok_or_else(|| MirageError::internal_invariant("recency node missing"))?;
        if node.segment == Segment::Detached {
            return Ok(());
        }
        let index = segment_index(node.segment)?;
        if let Some(prev) = node.prev {
            self.nodes.get_mut(&prev).expect("previous exists").next = node.next;
        } else {
            self.lists[index].head = node.next;
        }
        if let Some(next) = node.next {
            self.nodes.get_mut(&next).expect("next exists").prev = node.prev;
        } else {
            self.lists[index].tail = node.prev;
        }
        self.lists[index].len -= 1;
        *self.nodes.get_mut(&slot).expect("node exists") = Node {
            segment: Segment::Detached,
            prev: None,
            next: None,
        };
        Ok(())
    }
    fn list(&self, segment: Segment) -> &List {
        &self.lists[segment_index(segment).expect("attached segment")]
    }
}
fn segment_index(segment: Segment) -> Result<usize, MirageError> {
    match segment {
        Segment::Window => Ok(0),
        Segment::Probationary => Ok(1),
        Segment::Protected => Ok(2),
        Segment::PrefetchedUnused => Ok(3),
        Segment::Detached => Err(MirageError::invalid_argument(
            "detached is not a victim list",
        )),
    }
}
