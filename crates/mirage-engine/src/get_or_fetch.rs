use std::sync::Arc;

use crate::{FetchContext, PageProvider};
use mirage_backend::ObjectBackend;
use mirage_cache::{BudgetReservation, ReservationClass, ResidentPageGuard};
use mirage_scheduler::{
    FetchPriority, FetchWindow, FlightFailure, FlightMap, FrameMapping,
    fetch_window_with_encryption,
};
use mirage_types::{FetchFailureCause, MirageError, PageHash};
use tokio_util::sync::CancellationToken;

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

/// One verified page served to the FFI read path: either an admitted arena
/// guard or a bounded transient page that was fetched and verified but not
/// placed because the cache is full.
pub enum ProviderPage {
    Placed(mirage_cache::ResidentPageGuard),
    Transient(Arc<mirage_pack::PlainPage>),
}

impl<B: ObjectBackend + 'static> PageProvider<B> {
    /// Synchronous provider entry for the WinFsp read path. An admitted fetch
    /// places the page; when the arena cannot admit it the page is fetched,
    /// authenticated, decoded, and hash-verified, then served transiently —
    /// unknown cloud content is never zero-filled.
    pub fn provide_sync(&self, hash: PageHash) -> Result<ProviderPage, MirageError> {
        let context = FetchContext {
            priority: FetchPriority::P0Blocking,
            deadline_ns: u64::MAX,
            cancellation: CancellationToken::new(),
        };
        match futures_executor::block_on(self.get_or_fetch(hash, context.clone())) {
            Ok(guard) => Ok(ProviderPage::Placed(guard)),
            Err(error) if matches!(error.kind, mirage_types::MirageErrorKind::CacheFull) => {
                futures_executor::block_on(self.fetch_transient(hash, context))
                    .map(|page| ProviderPage::Transient(Arc::new(page)))
            }
            Err(error) => Err(error),
        }
    }

    /// Fetches and verifies one page without admitting it to the arena. Still
    /// single-flight: a second caller waits on the owner's flight and then
    /// serves from the resident index or retries once.
    pub async fn fetch_transient(
        &self,
        hash: PageHash,
        context: FetchContext,
    ) -> Result<mirage_pack::PlainPage, MirageError> {
        for _ in 0..2 {
            if let Some(guard) = self.index.acquire(hash)? {
                let mut bytes = vec![0u8; guard.logical_length() as usize];
                guard.read_exact(0, &mut bytes)?;
                return Ok(mirage_pack::PlainPage::from_bytes(bytes.into()));
            }
            let location = self.locations.get(hash)?.clone();
            let acquired = self.flights.acquire(
                hash,
                context.priority,
                context.deadline_ns,
                !context.priority.speculative(),
            )?;
            let mut handle = acquired.handle;
            if !acquired.owner {
                match handle.flight().wait_or_cancel(&context.cancellation).await {
                    None => {
                        handle.detach();
                        return Err(MirageError::cancelled("page fetch cancelled by caller"));
                    }
                    Some(Ok(())) => continue,
                    Some(Err(failure)) => {
                        return Err(failure.into_error("shared page fetch failed"));
                    }
                }
            }
            let flight = Arc::clone(handle.flight());
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
            let result = mirage_scheduler::worker::fetch_transient_with_encryption(
                self.backend.as_ref(),
                window,
                context.cancellation.clone(),
                self.max_window,
                self.encryption.as_ref(),
            )
            .await
            .and_then(|mut pages| {
                pages
                    .pop()
                    .ok_or_else(|| MirageError::internal_invariant("fetch returned no page"))
            });
            let completion = match &result {
                Ok(_) => self.flights.complete_owned(hash, &flight, Ok(())),
                Err(error) => self.flights.complete_owned(
                    hash,
                    &flight,
                    Err(FlightFailure::from_error(error)),
                ),
            };
            let _ = completion;
            return result;
        }
        Err(MirageError::internal_invariant(
            "transient fetch did not converge after shared flight",
        ))
    }
}

/// Wraps a `PageProvider` as the coordinator-owned synchronous hook.
#[must_use]
pub fn provider_hook<B: ObjectBackend + 'static>(
    provider: Arc<PageProvider<B>>,
) -> Arc<crate::ProviderHook> {
    Arc::new(move |hash| provider.provide_sync(hash))
}
