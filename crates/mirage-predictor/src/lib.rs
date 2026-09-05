//! Bounded trace and prediction inputs.
#![forbid(unsafe_code)]
pub mod analyze;
pub mod capsule;
pub mod coaccess;
pub mod first_touch;
pub mod hard_set;
pub mod ingest_queue;
pub mod normalize;
pub mod optimizer;
pub mod profile;
pub mod trace;
pub mod transition;
pub mod version_transfer;
pub use first_touch::{NormalizedTouch, TouchKind};
pub use normalize::{DataQuality, NormalizedTrace, normalize_trace};
pub use profile::{
    GameProfile, ObservationClass, PageObservation, ProcessRecord, ProfileProcessRole,
    SessionSummary,
};
pub use trace::{TraceBlockDecoder, TraceBlockEncoder, TraceEvent, TraceHeader};
