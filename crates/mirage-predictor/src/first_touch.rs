use mirage_types::{PageOrdinal, StableFileId};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TouchKind {
    First,
    SampledRepeat,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NormalizedTouch {
    pub timestamp_ns: u64,
    pub stable_file_id: StableFileId,
    pub page_ordinal: PageOrdinal,
    pub kind: TouchKind,
}
