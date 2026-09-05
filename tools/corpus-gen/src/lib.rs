//! Deterministic corruption and range corpora shared by property and fuzz targets.

#![forbid(unsafe_code)]

pub mod manifest;
pub mod pattern;
pub mod range_cases;
pub mod tree;

pub use manifest::{CorpusDescriptor, describe};
pub use pattern::{PatternKind, fill_at, oracle_byte};
pub use tree::{CorpusFile, CorpusPlan, CorpusProfile, plan};
