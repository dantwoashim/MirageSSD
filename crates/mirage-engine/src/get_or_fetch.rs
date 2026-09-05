use crate::{FetchContext, PageProvider};
use mirage_backend::ObjectBackend;
use mirage_cache::{ReservationClass, ResidentPageGuard};
use mirage_scheduler::{
    FetchPriority, FetchWindow, FlightFailure, FrameMapping, fetch_window_with_encryption,
};
use mirage_types::{MirageError, PageHash};

impl<B: ObjectBackend> PageProvider<B> {
    pub async fn get_or_fetch(
        &self,
        hash: PageHash,
        context: FetchContext,
    ) -> Result<ResidentPageGuard, MirageError> {
        if let Some(guard) = self.index.acquire(hash)? {
            return Ok(guard);
        }
        let location = self.locations.get(hash)?.clone();
        let class = if context.priority == FetchPriority::P0Blocking {
            ReservationClass::Blocking
        } else if context.priority <= FetchPriority::P2Capsule {
            ReservationClass::Capsule
        } else {
            ReservationClass::Prefetch
        };
        let reservation = self
            .budget
            .reserve(u64::from(location.logical_length), class)?;
        let acquired = self.flights.acquire(
            hash,
            context.priority,
            context.deadline_ns,
            !context.priority.speculative(),
        )?;
        if let Some(guard) = self.index.acquire(hash)? {
            drop(reservation);
            drop(acquired);
            return Ok(guard);
        }
        if acquired.owner {
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
            let result = fetch_window_with_encryption(
                self.backend.as_ref(),
                window,
                &self.db,
                self.shard.clone(),
                &self.index,
                context.cancellation,
                self.max_window,
                self.encryption.as_ref(),
            )
            .await
            .and_then(|results| {
                results
                    .into_iter()
                    .next()
                    .ok_or_else(|| {
                        MirageError::internal_invariant("fetch returned no page result")
                    })?
                    .result
            });
            let completion = result.as_ref().map(|_| ()).map_err(|error| FlightFailure {
                code: error.code.to_string(),
            });
            self.flights.complete(hash, completion)?;
            result?;
        } else {
            acquired.handle.flight().wait().map_err(|failure| {
                MirageError::backend_unavailable(format!(
                    "shared page fetch failed: {}",
                    failure.code
                ))
            })?;
        }
        drop(reservation);
        self.index.acquire(hash)?.ok_or_else(|| {
            MirageError::internal_invariant("verified fetch completed without resident page")
        })
    }
}
