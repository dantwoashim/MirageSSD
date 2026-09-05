use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecencyEvent {
    Touch(u32),
    Pin(u32),
    Unpin(u32),
}

#[derive(Debug, Default)]
pub struct TouchQueueMetrics {
    overflow: AtomicU64,
}
impl TouchQueueMetrics {
    #[must_use]
    pub fn overflow(&self) -> u64 {
        self.overflow.load(Ordering::Relaxed)
    }
}

#[derive(Clone)]
pub struct TouchQueue {
    sender: SyncSender<RecencyEvent>,
    metrics: Arc<TouchQueueMetrics>,
}
impl TouchQueue {
    pub fn bounded(capacity: usize) -> (Self, Receiver<RecencyEvent>) {
        let (sender, receiver) = sync_channel(capacity);
        let metrics = Arc::new(TouchQueueMetrics::default());
        (Self { sender, metrics }, receiver)
    }
    pub fn submit(&self, event: RecencyEvent) -> bool {
        match self.sender.try_send(event) {
            Ok(()) => true,
            Err(TrySendError::Full(_)) => {
                self.metrics.overflow.fetch_add(1, Ordering::Relaxed);
                false
            }
            Err(TrySendError::Disconnected(_)) => false,
        }
    }
    #[must_use]
    pub fn metrics(&self) -> Arc<TouchQueueMetrics> {
        Arc::clone(&self.metrics)
    }
}
