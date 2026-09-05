use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, Ordering};
use std::sync::{Condvar, Mutex};

use tokio_util::sync::CancellationToken;

use crate::FetchPriority;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlightFailure {
    pub code: String,
}
pub type FlightResult = Result<(), FlightFailure>;

pub struct PageFlight {
    priority: AtomicU8,
    earliest_deadline_ns: AtomicU64,
    waiter_count: AtomicU32,
    required_waiters: AtomicU32,
    admission_reason: AtomicBool,
    cancellation: CancellationToken,
    result: Mutex<Option<FlightResult>>,
    ready: Condvar,
}
impl PageFlight {
    pub(crate) fn new(priority: FetchPriority, deadline_ns: u64, required: bool) -> Self {
        Self {
            priority: AtomicU8::new(priority as u8),
            earliest_deadline_ns: AtomicU64::new(deadline_ns),
            waiter_count: AtomicU32::new(1),
            required_waiters: AtomicU32::new(u32::from(required)),
            admission_reason: AtomicBool::new(false),
            cancellation: CancellationToken::new(),
            result: Mutex::new(None),
            ready: Condvar::new(),
        }
    }
    pub(crate) fn attach(&self, priority: FetchPriority, deadline_ns: u64, required: bool) {
        self.waiter_count.fetch_add(1, Ordering::Relaxed);
        if required {
            self.required_waiters.fetch_add(1, Ordering::Relaxed);
        }
        self.priority.fetch_min(priority as u8, Ordering::AcqRel);
        self.earliest_deadline_ns
            .fetch_min(deadline_ns, Ordering::AcqRel);
    }
    pub fn complete(&self, result: FlightResult) {
        *self.result.lock().expect("flight lock poisoned") = Some(result);
        self.ready.notify_all();
    }
    pub fn wait(&self) -> FlightResult {
        let mut result = self.result.lock().expect("flight lock poisoned");
        while result.is_none() {
            result = self.ready.wait(result).expect("flight lock poisoned");
        }
        result.clone().expect("result exists")
    }
    pub fn set_admission_reason(&self, present: bool) {
        self.admission_reason.store(present, Ordering::Release);
        if !present {
            self.maybe_cancel();
        }
    }
    #[must_use]
    pub fn priority(&self) -> u8 {
        self.priority.load(Ordering::Acquire)
    }
    #[must_use]
    pub fn earliest_deadline_ns(&self) -> u64 {
        self.earliest_deadline_ns.load(Ordering::Acquire)
    }
    #[must_use]
    pub const fn cancellation(&self) -> &CancellationToken {
        &self.cancellation
    }
    pub(crate) fn detach(&self, required: bool) {
        self.waiter_count.fetch_sub(1, Ordering::AcqRel);
        if required {
            self.required_waiters.fetch_sub(1, Ordering::AcqRel);
        }
        self.maybe_cancel();
    }
    fn maybe_cancel(&self) {
        if self.required_waiters.load(Ordering::Acquire) == 0
            && self.waiter_count.load(Ordering::Acquire) == 0
            && !self.admission_reason.load(Ordering::Acquire)
        {
            self.cancellation.cancel();
        }
    }
}

pub struct FlightHandle {
    flight: std::sync::Arc<PageFlight>,
    required: bool,
}
impl FlightHandle {
    pub(crate) fn new(flight: std::sync::Arc<PageFlight>, required: bool) -> Self {
        Self { flight, required }
    }
    #[must_use]
    pub const fn flight(&self) -> &std::sync::Arc<PageFlight> {
        &self.flight
    }
}
impl Drop for FlightHandle {
    fn drop(&mut self) {
        self.flight.detach(self.required);
    }
}
