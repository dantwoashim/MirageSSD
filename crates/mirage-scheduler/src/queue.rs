use crate::{FetchPriority, FetchRequest, PriorityQueue};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct QueueMetrics {
    pub dropped_speculative: u64,
    pub cancelled_before_dequeue: u64,
}

pub struct SchedulerQueue {
    queue: PriorityQueue,
    capacity: usize,
    metrics: QueueMetrics,
}
impl SchedulerQueue {
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self {
            queue: PriorityQueue::default(),
            capacity,
            metrics: QueueMetrics::default(),
        }
    }
    pub fn enqueue(&mut self, request: FetchRequest) -> bool {
        if self.queue.len() >= self.capacity && request.priority.speculative() {
            request.cancellation.cancel();
            self.metrics.dropped_speculative += 1;
            return false;
        }
        self.queue.push(request);
        true
    }
    pub fn dequeue(&mut self) -> Option<FetchRequest> {
        let before = self.queue.len();
        let item = self.queue.pop();
        self.metrics.cancelled_before_dequeue +=
            (before.saturating_sub(self.queue.len() + usize::from(item.is_some()))) as u64;
        item
    }
    pub fn promote(&mut self, hash: mirage_types::PageHash, deadline_ns: u64) -> bool {
        self.queue
            .promote_page(hash, FetchPriority::P0Blocking, deadline_ns)
    }
    #[must_use]
    pub const fn metrics(&self) -> QueueMetrics {
        self.metrics
    }
}
