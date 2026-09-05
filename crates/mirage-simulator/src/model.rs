use std::collections::{BTreeSet, BinaryHeap};

use mirage_types::MirageError;
use serde::{Deserialize, Serialize};

use crate::{Event, EventKind, ScheduledEvent, SimTime};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkModel {
    pub base_latency_ns: u64,
    pub jitter_ns: u64,
    pub jitter_seed: u64,
    pub bandwidth_bytes_per_second: u64,
    pub max_concurrency: u32,
    pub fail_fetches: BTreeSet<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NetworkOutcome {
    pub first_byte_at: SimTime,
    pub complete_at: SimTime,
    pub failed: bool,
}

impl NetworkModel {
    pub fn fetch(
        &self,
        start: SimTime,
        bytes: u64,
        ordinal: u64,
        concurrent: u32,
    ) -> Result<NetworkOutcome, MirageError> {
        if self.bandwidth_bytes_per_second == 0
            || concurrent == 0
            || concurrent > self.max_concurrency
            || self.max_concurrency == 0
        {
            return Err(MirageError::invalid_argument(
                "network model parameters are outside bounds",
            ));
        }
        let jitter = if self.jitter_ns == 0 {
            0
        } else {
            splitmix64(self.jitter_seed ^ ordinal) % (self.jitter_ns + 1)
        };
        let first_byte_at = start.checked_add(
            self.base_latency_ns
                .checked_add(jitter)
                .ok_or_else(|| MirageError::invalid_argument("network latency overflows"))?,
        )?;
        let numerator = u128::from(bytes)
            .checked_mul(1_000_000_000)
            .and_then(|value| value.checked_mul(u128::from(concurrent)))
            .ok_or_else(|| MirageError::invalid_argument("network transfer duration overflows"))?;
        let denominator = u128::from(self.bandwidth_bytes_per_second);
        let duration = numerator.div_ceil(denominator);
        let duration = u64::try_from(duration)
            .map_err(|_| MirageError::invalid_argument("network transfer duration exceeds u64"))?;
        Ok(NetworkOutcome {
            first_byte_at,
            complete_at: first_byte_at.checked_add(duration)?,
            failed: self.fail_fetches.contains(&ordinal),
        })
    }
}

#[derive(Debug, Default)]
pub struct Simulator {
    now: SimTime,
    next_sequence: u64,
    queue: BinaryHeap<ScheduledEvent>,
    cancelled_requests: BTreeSet<u64>,
}

impl Simulator {
    #[must_use]
    pub const fn now(&self) -> SimTime {
        self.now
    }
    pub fn schedule(&mut self, at: SimTime, event: Event) -> Result<u64, MirageError> {
        if at < self.now {
            return Err(MirageError::invalid_argument(
                "event cannot be scheduled in the past",
            ));
        }
        let sequence = self.next_sequence;
        self.next_sequence = self
            .next_sequence
            .checked_add(1)
            .ok_or_else(|| MirageError::internal_invariant("event sequence overflows"))?;
        self.queue.push(ScheduledEvent {
            at,
            sequence,
            event,
        });
        Ok(sequence)
    }
    pub fn cancel_request(&mut self, request_id: u64) {
        self.cancelled_requests.insert(request_id);
    }
    pub fn next_event(&mut self) -> Option<ScheduledEvent> {
        while let Some(next) = self.queue.pop() {
            if self.cancelled_requests.contains(&next.event.request_id) {
                continue;
            }
            self.now = next.at;
            return Some(next);
        }
        None
    }
    pub fn one_page_miss(
        &mut self,
        arrival: SimTime,
        page_bytes: u64,
        network: &NetworkModel,
    ) -> Result<u64, MirageError> {
        let outcome = network.fetch(arrival, page_bytes, 0, 1)?;
        if outcome.failed {
            return Err(MirageError::backend_unavailable("scripted network failure"));
        }
        self.schedule(
            arrival,
            Event {
                kind: EventKind::ReadArrival,
                page: mirage_types::PageOrdinal::from_u32(0),
                request_id: 0,
            },
        )?;
        self.schedule(
            outcome.first_byte_at,
            Event {
                kind: EventKind::FetchFirstByte,
                page: mirage_types::PageOrdinal::from_u32(0),
                request_id: 0,
            },
        )?;
        self.schedule(
            outcome.complete_at,
            Event {
                kind: EventKind::FetchComplete,
                page: mirage_types::PageOrdinal::from_u32(0),
                request_id: 0,
            },
        )?;
        self.schedule(
            outcome.complete_at,
            Event {
                kind: EventKind::CacheInsert,
                page: mirage_types::PageOrdinal::from_u32(0),
                request_id: 0,
            },
        )?;
        self.schedule(
            outcome.complete_at,
            Event {
                kind: EventKind::LocalCompletion,
                page: mirage_types::PageOrdinal::from_u32(0),
                request_id: 0,
            },
        )?;
        Ok(outcome.complete_at.as_ns() - arrival.as_ns())
    }
}

const fn splitmix64(mut value: u64) -> u64 {
    value = value.wrapping_add(0x9e37_79b9_7f4a_7c15);
    value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    value ^ (value >> 31)
}
