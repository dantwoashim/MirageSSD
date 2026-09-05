//! Crash-safe fixed-slot sparse cache arena.
#![deny(unsafe_code)]

pub mod accounting;
pub mod adapt;
pub mod admission;
pub mod arena;
pub mod budget;
pub mod doorkeeper;
pub mod evict;
pub mod format;
pub mod frequency;
pub mod ghost;
pub mod index;
pub mod insert;
pub mod lease;
pub mod pin;
pub mod policy;
pub mod policy_core;
pub mod recency;
pub mod reconcile;
pub mod recover;
pub mod reservation_ledger;
pub mod reserve;
pub mod resident;
pub mod scrub;
pub mod segments;
pub mod session_pin;
pub mod shard;
pub mod slot;
pub mod space_lease;
pub mod verify;
#[cfg(windows)]
#[allow(unsafe_code)]
mod windows_sparse;

pub use accounting::CacheUsage;
pub use admission::{AdmissionContext, AdmissionDecision, AdmissionWeights};
pub use budget::{BudgetConfig, ReservationClass};
pub use format::{ArenaHeader, CacheLayout, SlotMetadata, SlotState};
pub use frequency::FrequencySketch;
pub use ghost::{GhostHistory, GhostKind, GhostMetrics};
pub use index::ResidentIndex;
pub use insert::{InsertHook, InsertOutcome, InsertStep, insert_page, insert_reserved_page};
pub use lease::ResidentPageGuard;
pub use pin::{PinReason, PinRegistry};
pub use policy_core::{PolicyCore, PolicyEvent, PolicyKind, PolicyOutcome};
pub use reconcile::{ReconcileAction, ReconcileReport, reconcile};
pub use reservation_ledger::{BudgetReservation, ReservationLedger, ReservationSnapshot};
pub use resident::ResidentPage;
pub use scrub::{ScrubReport, scrub_batch};
pub use segments::{Segment, SegmentedRecency, TouchQueue, TouchQueueMetrics};
pub use shard::{ArenaShard, SparseDiagnostics};
pub use space_lease::{
    BlockedCapacity, CapacitySnapshot, ReclaimCandidate, ReclaimId, SpaceLeasePlan,
    plan_space_lease,
};
pub use verify::{IntegrityClass, VerifyOutcome, verify_page};
