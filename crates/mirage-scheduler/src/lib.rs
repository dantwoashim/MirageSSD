#![forbid(unsafe_code)]

pub mod backoff;
pub mod coalesce;
pub mod controller;
pub mod decode;
pub mod flight;
pub mod flight_map;
pub mod metrics;
pub mod priority;
pub mod queue;
pub mod request;
pub mod retry;
pub mod validate;
pub mod window;
pub mod worker;

pub use coalesce::coalesce;
pub use controller::{ConcurrencyController, ControllerInput, ControllerRecommendation};
pub use flight::{FlightFailure, FlightHandle, PageFlight};
pub use flight_map::{FlightAcquire, FlightMap};
pub use priority::PriorityQueue;
pub use queue::{QueueMetrics, SchedulerQueue};
pub use request::{FetchPriority, FetchRequest, PageRequest};
pub use retry::{RetryDecision, RetryPolicy};
pub use window::{FetchWindow, FrameMapping, WindowFrame};
pub use worker::{FrameResult, fetch_window, fetch_window_with_encryption};
