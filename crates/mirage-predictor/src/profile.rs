use std::collections::BTreeSet;

use mirage_types::{ManifestHash, MirageError, RepositoryId};
use serde::{Deserialize, Serialize};

const MAX_OBSERVATIONS: usize = 10_000_000;
const MAX_PROCESSES: usize = 1_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservationClass {
    Demand,
    MandatoryAdmission,
    Opportunistic,
    Sequential,
    Predicted,
    Pinned,
    Maintenance,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProfileProcessRole {
    Launcher,
    Game,
    AntiCheat,
    CrashReporter,
    Updater,
    Helper,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PageObservation {
    pub file_index: u32,
    pub page_ordinal: u32,
    pub first_touch_delta_us: u64,
    pub class: ObservationClass,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessRecord {
    pub stable_index: u32,
    pub role: ProfileProcessRole,
    pub redacted_image_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GameProfile {
    pub format_version: u32,
    pub repository_id: RepositoryId,
    pub manifest_hash: ManifestHash,
    pub label: String,
    pub page_observations: Vec<PageObservation>,
    pub processes: Vec<ProcessRecord>,
    pub dropped_event_count: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionSummary {
    pub observation_count: u64,
    pub unique_page_count: u64,
    pub process_count: u64,
    pub last_touch_delta_us: u64,
    pub dropped_event_count: u64,
}

impl GameProfile {
    pub fn validate(&self) -> Result<(), MirageError> {
        if self.format_version != 1
            || self.label.is_empty()
            || self.label.len() > 255
            || self.page_observations.is_empty()
            || self.page_observations.len() > MAX_OBSERVATIONS
            || self.processes.len() > MAX_PROCESSES
            || self
                .processes
                .iter()
                .any(|process| process.redacted_image_path.len() > 32_767)
        {
            return Err(MirageError::invalid_argument(
                "game profile is outside profile-v1 bounds",
            ));
        }
        if self
            .page_observations
            .windows(2)
            .any(|pair| pair[1].first_touch_delta_us < pair[0].first_touch_delta_us)
        {
            return Err(MirageError::invalid_argument(
                "profile observations are not time ordered",
            ));
        }
        Ok(())
    }

    pub fn summary(&self) -> Result<SessionSummary, MirageError> {
        self.validate()?;
        let unique = self
            .page_observations
            .iter()
            .map(|observation| (observation.file_index, observation.page_ordinal))
            .collect::<BTreeSet<_>>()
            .len();
        Ok(SessionSummary {
            observation_count: self.page_observations.len() as u64,
            unique_page_count: unique as u64,
            process_count: self.processes.len() as u64,
            last_touch_delta_us: self
                .page_observations
                .last()
                .map_or(0, |observation| observation.first_touch_delta_us),
            dropped_event_count: self.dropped_event_count,
        })
    }
}
