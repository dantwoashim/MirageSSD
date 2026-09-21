use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};

use futures_util::future::{self, Either};
use tokio_util::sync::CancellationToken;

use mirage_types::{FetchFailureCause, MirageError};

use crate::FetchPriority;

/// A fetch failure that fans out to every flight subscriber with its cause preserved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlightFailure {
    pub cause: FetchFailureCause,
    pub code: String,
}

impl FlightFailure {
    pub fn from_error(error: &MirageError) -> Self {
        Self {
            cause: FetchFailureCause::from_error(error),
            code: error.code.to_string(),
        }
    }

    pub fn into_error(self, context: &str) -> MirageError {
        let message = format!("{context}: {}", self.code);
        match self.cause {
            FetchFailureCause::ChecksumMismatch => MirageError::integrity_mismatch(message),
            FetchFailureCause::AuthorizationDenied => {
                MirageError::backend_permission_denied(message)
            }
            FetchFailureCause::SourceMissing => MirageError::remote_object_missing(message),
            FetchFailureCause::Timeout => MirageError::backend_unavailable(message),
            FetchFailureCause::DeadlineExceeded => MirageError::deadline_exceeded(message),
            FetchFailureCause::BudgetExceeded => MirageError::cache_full(message),
            FetchFailureCause::CallerCancelled => MirageError::cancelled(message),
            FetchFailureCause::MalformedResponse => MirageError::invalid_argument(message),
            FetchFailureCause::Internal => MirageError::internal_invariant(message),
        }
    }
}

pub type FlightResult = Result<(), FlightFailure>;

struct FlightState {
    result: Option<FlightResult>,
    wakers: Vec<Waker>,
}

/// Awaitable completion of a [`PageFlight`]; registers the task waker once.
pub struct FlightCompletion<'a> {
    flight: &'a PageFlight,
}

impl std::future::Future for FlightCompletion<'_> {
    type Output = FlightResult;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut state = self.flight.state.lock().expect("flight lock poisoned");
        if let Some(result) = &state.result {
            return Poll::Ready(result.clone());
        }
        if !state
            .wakers
            .iter()
            .any(|existing| existing.will_wake(cx.waker()))
        {
            state.wakers.push(cx.waker().clone());
        }
        Poll::Pending
    }
}

pub struct PageFlight {
    priority: AtomicU8,
    earliest_deadline_ns: AtomicU64,
    waiter_count: AtomicU32,
    required_waiters: AtomicU32,
    max_waiters: u32,
    admission_reason: AtomicBool,
    cancellation: CancellationToken,
    state: Mutex<FlightState>,
}
impl PageFlight {
    pub(crate) fn new(
        priority: FetchPriority,
        deadline_ns: u64,
        required: bool,
        max_waiters: u32,
    ) -> Self {
        Self {
            priority: AtomicU8::new(priority as u8),
            earliest_deadline_ns: AtomicU64::new(deadline_ns),
            waiter_count: AtomicU32::new(1),
            required_waiters: AtomicU32::new(u32::from(required)),
            max_waiters,
            admission_reason: AtomicBool::new(false),
            cancellation: CancellationToken::new(),
            state: Mutex::new(FlightState {
                result: None,
                wakers: Vec::new(),
            }),
        }
    }
    pub(crate) fn attach(
        &self,
        priority: FetchPriority,
        deadline_ns: u64,
        required: bool,
    ) -> Result<(), MirageError> {
        if self.waiter_count.load(Ordering::Acquire) >= self.max_waiters {
            return Err(MirageError::cache_full("flight subscriber limit reached"));
        }
        self.waiter_count.fetch_add(1, Ordering::AcqRel);
        if required {
            self.required_waiters.fetch_add(1, Ordering::AcqRel);
        }
        self.priority.fetch_min(priority as u8, Ordering::AcqRel);
        self.earliest_deadline_ns
            .fetch_min(deadline_ns, Ordering::AcqRel);
        Ok(())
    }
    /// Completes the flight exactly once; later calls are no-ops so a Drop
    /// guard can complete late safely. Wakers run after the lock is released.
    pub fn complete(&self, result: FlightResult) {
        let wakers = {
            let mut state = self.state.lock().expect("flight lock poisoned");
            if state.result.is_some() {
                return;
            }
            state.result = Some(result);
            std::mem::take(&mut state.wakers)
        };
        for waker in wakers {
            waker.wake();
        }
    }
    #[must_use]
    pub fn try_result(&self) -> Option<FlightResult> {
        self.state
            .lock()
            .expect("flight lock poisoned")
            .result
            .clone()
    }
    #[must_use]
    pub const fn completion(&self) -> FlightCompletion<'_> {
        FlightCompletion { flight: self }
    }
    /// Resolves with the flight result, or `None` when the caller's token fires first.
    pub async fn wait_or_cancel(&self, cancel: &CancellationToken) -> Option<FlightResult> {
        let completion = std::pin::pin!(self.completion());
        let cancelled = std::pin::pin!(cancel.cancelled());
        match future::select(completion, cancelled).await {
            Either::Left((result, _)) => Some(result),
            Either::Right(((), _)) => None,
        }
    }
    pub fn wait(&self) -> FlightResult {
        futures_executor::block_on(self.completion())
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
    flight: Arc<PageFlight>,
    required: bool,
    attached: bool,
}
impl FlightHandle {
    pub(crate) fn new(flight: Arc<PageFlight>, required: bool) -> Self {
        Self {
            flight,
            required,
            attached: true,
        }
    }
    #[must_use]
    pub const fn flight(&self) -> &Arc<PageFlight> {
        &self.flight
    }
    /// Detaches this waiter early; idempotent, and Drop then does nothing.
    pub fn detach(&mut self) {
        if self.attached {
            self.flight.detach(self.required);
            self.attached = false;
        }
    }
}
impl Drop for FlightHandle {
    fn drop(&mut self) {
        self.detach();
    }
}
