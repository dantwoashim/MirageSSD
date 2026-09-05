use crate::correlate::TraceEvent;
use serde::{Deserialize, Serialize};

pub const SEGMENT_VERSION: u16 = 1;
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Segment {
    pub version: u16,
    pub sequence: u64,
    pub events: Vec<TraceEvent>,
    pub checksum: String,
}
