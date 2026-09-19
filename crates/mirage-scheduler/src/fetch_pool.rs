//! Bounded, priority-ordered worker pool so a fetch is never owned by a
//! caller's future lifetime. Speculative work holds a separate queue credit so
//! it cannot starve demand.

use std::cmp::Ordering;
use std::collections::BinaryHeap;
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
use std::sync::{Arc, Condvar, Mutex};

use mirage_types::MirageError;

use crate::FetchPriority;

pub type Job = Box<dyn FnOnce() + Send + 'static>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FetchPoolConfig {
    pub workers: usize,
    pub queue_depth: usize,
    /// Queue slots speculative work may occupy; 0 means speculative jobs never
    /// queue. Never exceeds `queue_depth`.
    pub speculative_queue_depth: usize,
    /// Bytes of fetch body that may be queued or in flight at once; 0
    /// disables the byte budget.
    pub max_in_flight_bytes: u64,
}

impl FetchPoolConfig {
    fn validate(self) -> Result<(), MirageError> {
        if self.workers == 0 || self.queue_depth == 0 {
            return Err(MirageError::invalid_argument(
                "fetch pool requires at least one worker and one queue slot",
            ));
        }
        if self.speculative_queue_depth > self.queue_depth {
            return Err(MirageError::invalid_argument(
                "speculative fetch queue cannot exceed the fetch queue depth",
            ));
        }
        Ok(())
    }
}

/// `BinaryHeap` pops the maximum, so ordering is reversed: the lowest
/// `FetchPriority` value (P0 demand) is greatest, then the earliest deadline,
/// then the lowest sequence (FIFO within a class).
struct QueuedJob {
    priority: FetchPriority,
    deadline_ns: u64,
    sequence: u64,
    bytes: u64,
    job: Job,
}

impl PartialEq for QueuedJob {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}
impl Eq for QueuedJob {}
impl PartialOrd for QueuedJob {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for QueuedJob {
    fn cmp(&self, other: &Self) -> Ordering {
        other
            .priority
            .cmp(&self.priority)
            .then_with(|| other.deadline_ns.cmp(&self.deadline_ns))
            .then_with(|| other.sequence.cmp(&self.sequence))
    }
}

struct PoolState {
    queue: BinaryHeap<QueuedJob>,
    queued_speculative: usize,
    running: usize,
    /// Speculative jobs currently running; at least one worker slot stays
    /// protected for demand so foreground reads never queue behind a fully
    /// speculative fleet.
    running_speculative: usize,
    bytes_in_flight: u64,
    closed: bool,
    next_sequence: u64,
}

struct Shared {
    state: Mutex<PoolState>,
    wake: Condvar,
    completed: AtomicU64,
    panicked: AtomicU64,
    expired: AtomicU64,
}

/// A panicking job still counts as completed for bookkeeping: the reservation
/// slot ran to a definite end (success or panic), never leaks `running`.
struct RunGuard<'a> {
    shared: &'a Shared,
    speculative: bool,
    bytes: u64,
}
impl Drop for RunGuard<'_> {
    fn drop(&mut self) {
        self.shared.completed.fetch_add(1, AtomicOrdering::AcqRel);
        if let Ok(mut state) = self.shared.state.lock() {
            state.running -= 1;
            state.bytes_in_flight = state.bytes_in_flight.saturating_sub(self.bytes);
            if self.speculative {
                state.running_speculative -= 1;
            }
        }
        self.shared.wake.notify_all();
    }
}

/// Bounded fetch pool. Jobs run on plain `std::thread` workers and contain
/// their own `block_on`; dropping the pool closes the queue and lets workers
/// exit without joining threads that may be blocked on a network read. Queued
/// but unstarted jobs are dropped on close.
pub struct FetchPool {
    shared: Arc<Shared>,
    config: FetchPoolConfig,
}

impl FetchPool {
    pub fn new(config: FetchPoolConfig) -> Result<Arc<Self>, MirageError> {
        config.validate()?;
        let shared = Arc::new(Shared {
            state: Mutex::new(PoolState {
                queue: BinaryHeap::new(),
                queued_speculative: 0,
                running: 0,
                running_speculative: 0,
                bytes_in_flight: 0,
                closed: false,
                next_sequence: 0,
            }),
            wake: Condvar::new(),
            completed: AtomicU64::new(0),
            panicked: AtomicU64::new(0),
            expired: AtomicU64::new(0),
        });
        let shared_workers = config.workers;
        for _ in 0..config.workers {
            let shared = Arc::clone(&shared);
            std::thread::spawn(move || {
                loop {
                    let job_meta = {
                        let mut state = match shared.state.lock() {
                            Ok(state) => state,
                            Err(_) => return,
                        };
                        loop {
                            if state.closed {
                                return;
                            }
                            let now = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .map(|elapsed| elapsed.as_nanos() as u64)
                                .unwrap_or(0);
                            match state.queue.peek() {
                                Some(queued) if queued.deadline_ns < now => {
                                    // Deadlines are enforced, not only
                                    // sorted: an expired job never runs.
                                    let expired = state.queue.pop().expect("peeked job");
                                    if expired.priority.speculative() {
                                        state.queued_speculative -= 1;
                                    }
                                    shared.expired.fetch_add(1, AtomicOrdering::AcqRel);
                                }
                                Some(queued)
                                    if queued.priority.speculative()
                                        && state.running_speculative
                                            >= shared_workers.saturating_sub(1).max(1) =>
                                {
                                    // The protected demand slot stays free:
                                    // a fully speculative fleet parks until
                                    // a worker frees up.
                                    state = match shared.wake.wait(state) {
                                        Ok(state) => state,
                                        Err(_) => return,
                                    };
                                }
                                Some(_) => {
                                    let queued = state.queue.pop().expect("peeked job");
                                    if queued.priority.speculative() {
                                        state.queued_speculative -= 1;
                                        state.running_speculative += 1;
                                    }
                                    state.running += 1;
                                    state.bytes_in_flight += queued.bytes;
                                    break (
                                        queued.job,
                                        queued.priority.speculative(),
                                        queued.bytes,
                                    );
                                }
                                None => {
                                    state = match shared.wake.wait(state) {
                                        Ok(state) => state,
                                        Err(_) => return,
                                    };
                                }
                            }
                        }
                    };
                    let _guard = RunGuard {
                        shared: &shared,
                        speculative: job_meta.1,
                        bytes: job_meta.2,
                    };
                    if std::panic::catch_unwind(std::panic::AssertUnwindSafe(job_meta.0)).is_err() {
                        shared.panicked.fetch_add(1, AtomicOrdering::AcqRel);
                    }
                }
            });
        }
        Ok(Arc::new(Self { shared, config }))
    }

    pub fn spawn(
        &self,
        priority: FetchPriority,
        deadline_ns: u64,
        job: Job,
    ) -> Result<(), MirageError> {
        self.spawn_metered(priority, deadline_ns, 0, job)
    }

    /// Spawns a job that holds `bytes` of the in-flight byte budget while it
    /// is queued or running; speculative work beyond the budget is rejected.
    pub fn spawn_metered(
        &self,
        priority: FetchPriority,
        deadline_ns: u64,
        bytes: u64,
        job: Job,
    ) -> Result<(), MirageError> {
        let mut state = self
            .shared
            .state
            .lock()
            .map_err(|_| MirageError::internal_invariant("fetch pool lock poisoned"))?;
        if state.closed {
            return Err(MirageError::internal_invariant(
                "fetch worker pool is shut down",
            ));
        }
        if state.queue.len() >= self.config.queue_depth {
            return Err(MirageError::cache_full("fetch worker pool is saturated"));
        }
        if priority.speculative() && state.queued_speculative >= self.config.speculative_queue_depth
        {
            return Err(MirageError::cache_full(
                "speculative fetch credits are exhausted",
            ));
        }
        if priority.speculative() && self.config.max_in_flight_bytes > 0 {
            let queued_bytes: u64 = state.queue.iter().map(|queued| queued.bytes).sum();
            if state
                .bytes_in_flight
                .saturating_add(queued_bytes)
                .saturating_add(bytes)
                > self.config.max_in_flight_bytes
            {
                return Err(MirageError::cache_full(
                    "in-flight fetch byte budget is exhausted",
                ));
            }
        }
        let sequence = state.next_sequence;
        state.next_sequence = state.next_sequence.wrapping_add(1);
        if priority.speculative() {
            state.queued_speculative += 1;
        }
        state.queue.push(QueuedJob {
            priority,
            deadline_ns,
            sequence,
            bytes,
            job,
        });
        drop(state);
        self.shared.wake.notify_one();
        Ok(())
    }

    #[must_use]
    pub fn queued(&self) -> usize {
        self.shared
            .state
            .lock()
            .map(|state| state.queue.len())
            .unwrap_or(0)
    }

    #[must_use]
    pub fn running(&self) -> usize {
        self.shared
            .state
            .lock()
            .map(|state| state.running)
            .unwrap_or(0)
    }

    /// Jobs that ran to a definite end, including jobs that panicked.
    #[must_use]
    pub fn completed(&self) -> u64 {
        self.shared.completed.load(AtomicOrdering::Acquire)
    }

    /// Jobs that panicked; they are included in `completed`.
    #[must_use]
    pub fn panicked(&self) -> u64 {
        self.shared.panicked.load(AtomicOrdering::Acquire)
    }

    /// Queued jobs dropped because their deadline passed before a worker
    /// reached them.
    #[must_use]
    pub fn expired(&self) -> u64 {
        self.shared.expired.load(AtomicOrdering::Acquire)
    }
}

impl Drop for FetchPool {
    fn drop(&mut self) {
        if let Ok(mut state) = self.shared.state.lock() {
            state.closed = true;
            self.shared.wake.notify_all();
        }
    }
}
