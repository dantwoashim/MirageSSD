use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use mirage_types::{MirageError, PageHash};

use crate::flight::FlightResult;
use crate::{FetchPriority, FlightHandle, PageFlight};

const DEFAULT_MAX_WAITERS: u32 = 4096;

pub struct FlightAcquire {
    pub handle: FlightHandle,
    pub owner: bool,
}
#[derive(Clone)]
pub struct FlightMap {
    flights: Arc<Mutex<BTreeMap<PageHash, Arc<PageFlight>>>>,
    max_waiters: u32,
}
impl Default for FlightMap {
    fn default() -> Self {
        Self::with_limits(DEFAULT_MAX_WAITERS)
    }
}
impl FlightMap {
    #[must_use]
    pub fn with_limits(max_waiters: u32) -> Self {
        Self {
            flights: Arc::new(Mutex::new(BTreeMap::new())),
            max_waiters,
        }
    }
    pub fn acquire(
        &self,
        hash: PageHash,
        priority: FetchPriority,
        deadline_ns: u64,
        required: bool,
    ) -> Result<FlightAcquire, MirageError> {
        let mut flights = self
            .flights
            .lock()
            .map_err(|_| MirageError::internal_invariant("flight map lock poisoned"))?;
        if let Some(flight) = flights.get(&hash) {
            if !flight.cancellation().is_cancelled() && flight.try_result().is_none() {
                flight.attach(priority, deadline_ns, required)?;
                return Ok(FlightAcquire {
                    handle: FlightHandle::new(Arc::clone(flight), required),
                    owner: false,
                });
            }
            flights.remove(&hash);
        }
        let flight = Arc::new(PageFlight::new(
            priority,
            deadline_ns,
            required,
            self.max_waiters,
        ));
        flights.insert(hash, Arc::clone(&flight));
        Ok(FlightAcquire {
            handle: FlightHandle::new(flight, required),
            owner: true,
        })
    }
    pub fn complete(&self, hash: PageHash, result: FlightResult) -> Result<bool, MirageError> {
        let flight = self
            .flights
            .lock()
            .map_err(|_| MirageError::internal_invariant("flight map lock poisoned"))?
            .remove(&hash);
        if let Some(flight) = flight {
            flight.complete(result);
            Ok(true)
        } else {
            Ok(false)
        }
    }
    /// Completes `flight` and removes the map entry only when it still maps to
    /// that exact flight, so a replaced entry is never clobbered.
    pub fn complete_owned(
        &self,
        hash: PageHash,
        flight: &Arc<PageFlight>,
        result: FlightResult,
    ) -> Result<bool, MirageError> {
        let removed = {
            let mut flights = self
                .flights
                .lock()
                .map_err(|_| MirageError::internal_invariant("flight map lock poisoned"))?;
            match flights.get(&hash) {
                Some(stored) if Arc::ptr_eq(stored, flight) => {
                    flights.remove(&hash);
                    true
                }
                _ => false,
            }
        };
        flight.complete(result);
        Ok(removed)
    }
    #[must_use]
    pub fn len(&self) -> usize {
        self.flights.lock().map_or(0, |flights| flights.len())
    }
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}
