use std::sync::{Arc, Mutex};

use mirage_types::MirageError;

use crate::{BudgetConfig, ReservationClass};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReservationSnapshot {
    pub committed_bytes: u64,
    pub reserved_bytes: u64,
    pub dirty_reserved_bytes: u64,
    pub peak_envelope_bytes: u64,
}

#[derive(Debug)]
struct State {
    committed: u64,
    reserved: u64,
    dirty_reserved: u64,
    peak: u64,
}

#[derive(Debug)]
struct Inner {
    config: BudgetConfig,
    state: Mutex<State>,
}

#[derive(Debug, Clone)]
pub struct ReservationLedger {
    inner: Arc<Inner>,
}

impl ReservationLedger {
    pub fn new(config: BudgetConfig, committed_bytes: u64) -> Result<Self, MirageError> {
        config.validate()?;
        if committed_bytes > config.hard_bytes {
            return Err(MirageError::cache_full(
                "committed cache exceeds hard budget",
            ));
        }
        Ok(Self {
            inner: Arc::new(Inner {
                config,
                state: Mutex::new(State {
                    committed: committed_bytes,
                    reserved: 0,
                    dirty_reserved: 0,
                    peak: committed_bytes,
                }),
            }),
        })
    }
    pub fn reserve(
        &self,
        bytes: u64,
        class: ReservationClass,
    ) -> Result<BudgetReservation, MirageError> {
        if bytes == 0 {
            return Err(MirageError::invalid_argument("zero-byte cache reservation"));
        }
        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| MirageError::internal_invariant("reservation ledger lock poisoned"))?;
        let next_reserved = state
            .reserved
            .checked_add(bytes)
            .ok_or_else(|| MirageError::invalid_argument("reserved byte count overflows"))?;
        let envelope = state
            .committed
            .checked_add(next_reserved)
            .ok_or_else(|| MirageError::invalid_argument("cache envelope overflows"))?;
        let limit = match class {
            ReservationClass::Prefetch => self.inner.config.prefetch_soft_bytes,
            ReservationClass::DirtyUpdate | ReservationClass::Staging => {
                self.inner.config.hard_bytes
            }
            ReservationClass::Blocking | ReservationClass::Capsule => {
                self.inner.config.hard_bytes - self.inner.config.update_safety_reserve
            }
        };
        if envelope > limit {
            return Err(MirageError::cache_full(
                "cache reservation exceeds class budget",
            ));
        }
        let dirty_reserved = if matches!(
            class,
            ReservationClass::DirtyUpdate | ReservationClass::Staging
        ) {
            let value = state.dirty_reserved.checked_add(bytes).ok_or_else(|| {
                MirageError::invalid_argument("dirty reservation count overflows")
            })?;
            if value > self.inner.config.dirty_update_bytes {
                return Err(MirageError::cache_full(
                    "dirty update reservation exceeds its reserve",
                ));
            }
            value
        } else {
            state.dirty_reserved
        };
        state.reserved = next_reserved;
        state.dirty_reserved = dirty_reserved;
        state.peak = state.peak.max(envelope);
        drop(state);
        Ok(BudgetReservation {
            inner: Arc::clone(&self.inner),
            bytes,
            class,
            active: true,
        })
    }
    pub fn set_committed(&self, bytes: u64) -> Result<(), MirageError> {
        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| MirageError::internal_invariant("reservation ledger lock poisoned"))?;
        if bytes
            .checked_add(state.reserved)
            .is_none_or(|total| total > self.inner.config.hard_bytes)
        {
            return Err(MirageError::cache_full(
                "committed cache update exceeds hard envelope",
            ));
        }
        state.committed = bytes;
        state.peak = state.peak.max(bytes + state.reserved);
        Ok(())
    }
    pub fn snapshot(&self) -> Result<ReservationSnapshot, MirageError> {
        let state = self
            .inner
            .state
            .lock()
            .map_err(|_| MirageError::internal_invariant("reservation ledger lock poisoned"))?;
        Ok(ReservationSnapshot {
            committed_bytes: state.committed,
            reserved_bytes: state.reserved,
            dirty_reserved_bytes: state.dirty_reserved,
            peak_envelope_bytes: state.peak,
        })
    }
}

pub struct BudgetReservation {
    inner: Arc<Inner>,
    bytes: u64,
    class: ReservationClass,
    active: bool,
}
impl BudgetReservation {
    #[must_use]
    pub const fn bytes(&self) -> u64 {
        self.bytes
    }
    pub fn release(mut self) {
        self.release_inner();
    }
    fn release_inner(&mut self) {
        if !self.active {
            return;
        }
        if let Ok(mut state) = self.inner.state.lock() {
            state.reserved -= self.bytes;
            if matches!(
                self.class,
                ReservationClass::DirtyUpdate | ReservationClass::Staging
            ) {
                state.dirty_reserved -= self.bytes;
            }
        }
        self.active = false;
    }
}
impl Drop for BudgetReservation {
    fn drop(&mut self) {
        self.release_inner();
    }
}
