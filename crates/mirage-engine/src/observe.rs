use mirage_types::{MirageError, PageHash, SessionId};
use std::collections::BTreeSet;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObservationEvent {
    FirstTouch { session: SessionId, page: PageHash },
    SealViolation { session: SessionId, page: PageHash },
}
#[derive(Debug, Default)]
pub struct TelemetryMetrics {
    pub dropped: AtomicU64,
    pub seal_violations: AtomicU64,
}
pub struct SessionObserver {
    session: SessionId,
    sealed: bool,
    leased: BTreeSet<PageHash>,
    seen: Mutex<BTreeSet<PageHash>>,
    violations: Mutex<BTreeSet<PageHash>>,
    sender: SyncSender<ObservationEvent>,
    metrics: TelemetryMetrics,
}
impl SessionObserver {
    #[must_use]
    pub fn new(
        session: SessionId,
        sealed: bool,
        leased: BTreeSet<PageHash>,
        capacity: usize,
    ) -> (Self, Receiver<ObservationEvent>) {
        let (sender, receiver) = sync_channel(capacity);
        (
            Self {
                session,
                sealed,
                leased,
                seen: Mutex::new(BTreeSet::new()),
                violations: Mutex::new(BTreeSet::new()),
                sender,
                metrics: TelemetryMetrics::default(),
            },
            receiver,
        )
    }
    pub fn after_read(&self, page: PageHash) -> Result<(), MirageError> {
        let first = self
            .seen
            .lock()
            .map_err(|_| MirageError::internal_invariant("observation set poisoned"))?
            .insert(page);
        if first {
            self.emit(ObservationEvent::FirstTouch {
                session: self.session,
                page,
            });
        }
        if self.sealed && !self.leased.contains(&page) {
            let violation = self
                .violations
                .lock()
                .map_err(|_| MirageError::internal_invariant("violation set poisoned"))?
                .insert(page);
            if violation {
                self.metrics.seal_violations.fetch_add(1, Ordering::Relaxed);
                self.emit(ObservationEvent::SealViolation {
                    session: self.session,
                    page,
                });
            }
        }
        Ok(())
    }
    fn emit(&self, event: ObservationEvent) {
        if matches!(
            self.sender.try_send(event),
            Err(TrySendError::Full(_) | TrySendError::Disconnected(_))
        ) {
            self.metrics.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }
    #[must_use]
    pub fn metrics(&self) -> (u64, u64) {
        (
            self.metrics.dropped.load(Ordering::Relaxed),
            self.metrics.seal_violations.load(Ordering::Relaxed),
        )
    }
}
