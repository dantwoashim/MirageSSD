use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TouchEvent {
    pub page_ordinal: u32,
    pub elapsed_ns: u64,
}
pub struct IngestQueue {
    sender: SyncSender<TouchEvent>,
    dropped: AtomicU64,
}
impl IngestQueue {
    #[must_use]
    pub fn bounded(capacity: usize) -> (Self, Receiver<TouchEvent>) {
        let (sender, receiver) = sync_channel(capacity);
        (
            Self {
                sender,
                dropped: AtomicU64::new(0),
            },
            receiver,
        )
    }
    pub fn submit(&self, event: TouchEvent) -> bool {
        match self.sender.try_send(event) {
            Ok(()) => true,
            Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) => {
                self.dropped.fetch_add(1, Ordering::Relaxed);
                false
            }
        }
    }
    #[must_use]
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }
}
