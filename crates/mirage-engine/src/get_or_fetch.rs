use std::sync::Arc;

use crate::{FetchContext, PageProvider};
use mirage_backend::ObjectBackend;
use mirage_cache::{BudgetReservation, ReservationClass, ResidentPageGuard};
use mirage_scheduler::{
    FetchPriority, FetchWindow, FlightFailure, FlightMap, FrameMapping,
    fetch_window_with_encryption,
};
use mirage_types::{FetchFailureCause, MirageError, PageHash};

/// Completes a flight with `Internal` if the fetch job ever returns without
/// completing it; `complete` is idempotent so the normal path wins.
struct CompleteOnDrop {
    flights: FlightMap,
    hash: PageHash,
    flight: Arc<mirage_scheduler::PageFlight>,
}

impl Drop for CompleteOnDrop {
    fn drop(&mut self) {
        let _ = self.flights.complete_owned(
            self.hash,
            &self.flight,
            Err(FlightFailure {
                cause: FetchFailureCause::Internal,
                code: "MIRAGE_INTERNAL_INVARIANT".into(),
            }),
        );
    }
}

impl<B: ObjectBackend + 'static> PageProvider<B> {
    /// Pages are content-addressed by `PageHash` and publication goes through
    /// `ResidentIndex::install` keyed by hash, so a late completion cannot
    /// populate a different build's mapping.
    ///
    /// Lifecycle: admit identity, join or create the flight, reserve owner
    /// resources, fetch on the bounded pool, decode/authenticate, verify,
    /// place, publish, then complete all subscribers.
    pub async fn get_or_fetch(
        &self,
        hash: PageHash,
        context: FetchContext,
    ) -> Result<ResidentPageGuard, MirageError> {
        if let Some(guard) = self.index.acquire(hash)? {
            return Ok(guard);
        }
        let location = self.locations.get(hash)?.clone();
        let acquired = self.flights.acquire(
            hash,
            context.priority,
            context.deadline_ns,
            !context.priority.speculative(),
        )?;
        if let Some(guard) = self.index.acquire(hash)? {
            if acquired.owner {
                self.flights
                    .complete_owned(hash, acquired.handle.flight(), Ok(()))?;
            }
            return Ok(guard);
        }
        let mut handle = acquired.handle;
        if acquired.owner {
            let class = if context.priority == FetchPriority::P0Blocking {
                ReservationClass::Blocking
            } else if context.priority <= FetchPriority::P2Capsule {
                ReservationClass::Capsule
            } else {
                ReservationClass::Prefetch
            };
            let reservation = match self
                .budget
                .reserve(u64::from(location.logical_length), class)
            {
                Ok(reservation) => reservation,
                Err(error) => {
                    let failure = FlightFailure::from_error(&error);
                    self.flights
                        .complete_owned(hash, handle.flight(), Err(failure))?;
                    return Err(error);
                }
            };
            let window = FetchWindow {
                object: location.object,
                range: location.encoded_range,
                priority: context.priority,
                frames: vec![FrameMapping {
                    page_hash: hash,
                    window_offset: 0,
                    encoded_length: location.encoded_range.len(),
                }],
                gap_bytes: 0,
            };
            if let Err(error) = self.spawn_fetch(
                hash,
                Arc::clone(handle.flight()),
                window,
                reservation,
                context.deadline_ns,
            ) {
                let failure = FlightFailure::from_error(&error);
                let _ = self
                    .flights
                    .complete_owned(hash, handle.flight(), Err(failure));
                return Err(error);
            }
        }
        match handle.flight().wait_or_cancel(&context.cancellation).await {
            None => {
                handle.detach();
                return Err(MirageError::cancelled("page fetch cancelled by caller"));
            }
            Some(Ok(())) => {}
            Some(Err(failure)) => {
                return Err(failure.into_error("shared page fetch failed"));
            }
        }
        self.index.acquire(hash)?.ok_or_else(|| {
            MirageError::internal_invariant("verified fetch completed without resident page")
        })
    }

    fn spawn_fetch(
        &self,
        hash: PageHash,
        flight: Arc<mirage_scheduler::PageFlight>,
        window: FetchWindow,
        reservation: BudgetReservation,
        deadline_ns: u64,
    ) -> Result<(), MirageError> {
        let backend = Arc::clone(&self.backend);
        let db = self.db.clone();
        let shard = Arc::clone(&self.shard);
        let index = Arc::clone(&self.index);
        let encryption = self.encryption.clone();
        let flights = self.flights.clone();
        let fetch_cancel = flight.cancellation().clone();
        let max_window = self.max_window;
        let job_flight = Arc::clone(&flight);
        let priority = window.priority;
        self.pool.spawn(
            priority,
            deadline_ns,
            Box::new(move || {
                let _guard = CompleteOnDrop {
                    flights: flights.clone(),
                    hash,
                    flight: Arc::clone(&job_flight),
                };
                let result = futures_executor::block_on(fetch_window_with_encryption(
                    backend.as_ref(),
                    window,
                    &db,
                    shard,
                    &index,
                    fetch_cancel,
                    max_window,
                    encryption.as_ref(),
                ))
                .and_then(|results| {
                    results
                        .into_iter()
                        .next()
                        .ok_or_else(|| {
                            MirageError::internal_invariant("fetch returned no page result")
                        })?
                        .result
                });
                // A placed page converts its reservation into committed bytes; a
                // failed fetch releases them. A commit overflow is surfaced as the
                // flight's failure rather than silently dropped.
                let result = match result {
                    Ok(()) => reservation.commit(),
                    Err(error) => Err(error),
                };
                let _ = flights.complete_owned(
                    hash,
                    &job_flight,
                    result.map_err(|error| FlightFailure::from_error(&error)),
                );
            }),
        )
    }
}
