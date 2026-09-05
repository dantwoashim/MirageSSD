use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use mirage_types::{MirageError, PageHash};

use crate::flight::FlightResult;
use crate::{FetchPriority, FlightHandle, PageFlight};

pub struct FlightAcquire {
    pub handle: FlightHandle,
    pub owner: bool,
}
#[derive(Default)]
pub struct FlightMap {
    flights: Mutex<BTreeMap<PageHash, Arc<PageFlight>>>,
}
impl FlightMap {
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
            flight.attach(priority, deadline_ns, required);
            return Ok(FlightAcquire {
                handle: FlightHandle::new(Arc::clone(flight), required),
                owner: false,
            });
        }
        let flight = Arc::new(PageFlight::new(priority, deadline_ns, required));
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
    #[must_use]
    pub fn len(&self) -> usize {
        self.flights.lock().map_or(0, |flights| flights.len())
    }
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}
