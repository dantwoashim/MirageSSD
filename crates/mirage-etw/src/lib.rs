//! Bounded Windows file-I/O profiling primitives.
#![deny(unsafe_code)]

pub mod correlate;
pub mod filter;
pub mod quality;
pub mod segment;
pub mod session;
#[cfg(windows)]
#[allow(unsafe_code)]
mod session_windows;
pub mod writer;

pub use session::{CapturedSession, EtwSession, SessionMetrics};
