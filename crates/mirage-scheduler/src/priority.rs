use std::cmp::Ordering;
use std::collections::BinaryHeap;

use crate::FetchRequest;

struct Queued(FetchRequest);
impl PartialEq for Queued {
    fn eq(&self, other: &Self) -> bool {
        self.0.priority == other.0.priority
            && self.0.deadline_ns == other.0.deadline_ns
            && self.0.sequence == other.0.sequence
    }
}
impl Eq for Queued {}
impl PartialOrd for Queued {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Queued {
    fn cmp(&self, other: &Self) -> Ordering {
        other
            .0
            .priority
            .cmp(&self.0.priority)
            .then_with(|| other.0.deadline_ns.cmp(&self.0.deadline_ns))
            .then_with(|| other.0.sequence.cmp(&self.0.sequence))
    }
}

#[derive(Default)]
pub struct PriorityQueue {
    heap: BinaryHeap<Queued>,
}
impl PriorityQueue {
    pub fn push(&mut self, request: FetchRequest) {
        self.heap.push(Queued(request));
    }
    pub fn pop(&mut self) -> Option<FetchRequest> {
        while let Some(Queued(request)) = self.heap.pop() {
            if !request.cancellation.is_cancelled() {
                return Some(request);
            }
        }
        None
    }
    #[must_use]
    pub fn len(&self) -> usize {
        self.heap.len()
    }
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.heap.is_empty()
    }
    pub fn promote_page(
        &mut self,
        hash: mirage_types::PageHash,
        priority: crate::FetchPriority,
        deadline_ns: u64,
    ) -> bool {
        let mut entries = self.heap.drain().collect::<Vec<_>>();
        let mut changed = false;
        for Queued(request) in &mut entries {
            if request.pages.iter().any(|page| page.hash == hash) && priority < request.priority {
                request.priority = priority;
                request.deadline_ns = request.deadline_ns.min(deadline_ns);
                changed = true;
            }
        }
        self.heap.extend(entries);
        changed
    }
}
