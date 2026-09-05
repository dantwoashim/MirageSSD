//! Deterministic discrete-event replay primitives.
#![forbid(unsafe_code)]

pub mod cache;
pub mod cache_adapter;
pub mod clock;
pub mod event;
pub mod held_out;
pub mod metrics;
pub mod model;
pub mod replay;
pub mod report;
pub mod sweep;

pub use cache::{CacheAccess, FixedPageCache};
pub use clock::SimTime;
pub use event::{Event, EventKind, ScheduledEvent};
pub use metrics::ReplayMetrics;
pub use model::{NetworkModel, NetworkOutcome, Simulator};
pub use replay::{BaselineReplay, ReplayConfig};
pub use report::{GATE_A_SYNTHETIC_WARNING, render_markdown};
pub use sweep::{ObjectiveWeights, SweepCase, SweepInput, SweepResult, run_sweep};
