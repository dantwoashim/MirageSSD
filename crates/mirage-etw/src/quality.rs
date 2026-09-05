use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DataQuality {
    pub accepted_events: u64,
    pub unknown_paths: u64,
    pub dropped_events: u64,
    pub etw_events_lost: u64,
}
impl DataQuality {
    pub fn total_observed(self) -> u64 {
        self.accepted_events
            .saturating_add(self.unknown_paths)
            .saturating_add(self.dropped_events)
            .saturating_add(self.etw_events_lost)
    }
}
