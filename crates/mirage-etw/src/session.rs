#[cfg(not(windows))]
use mirage_types::MirageError;

use crate::correlate::TraceEvent;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CapturedSession {
    pub metrics: SessionMetrics,
    pub events: Vec<TraceEvent>,
    pub unknown_paths: u64,
    pub dropped_events: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SessionMetrics {
    pub events_lost: u32,
    pub realtime_buffers_lost: u32,
    pub buffers_written: u32,
}

#[cfg(windows)]
pub use crate::session_windows::EtwSession;

#[cfg(not(windows))]
#[derive(Debug)]
pub struct EtwSession;
#[cfg(not(windows))]
impl EtwSession {
    pub fn start(_: &str, _: u32, _: u32) -> Result<Self, MirageError> {
        Err(MirageError::provider_unavailable(
            "ETW profiling requires Windows",
        ))
    }
    pub fn stop(self) -> Result<SessionMetrics, MirageError> {
        Err(MirageError::provider_unavailable(
            "ETW profiling requires Windows",
        ))
    }
    pub fn start_capture(_: &str, _: u32, _: u32, _: usize) -> Result<Self, MirageError> {
        Err(MirageError::provider_unavailable(
            "ETW profiling requires Windows",
        ))
    }
    pub fn stop_capture(self) -> Result<CapturedSession, MirageError> {
        Err(MirageError::provider_unavailable(
            "ETW profiling requires Windows",
        ))
    }
}
