use std::collections::{BTreeMap, BTreeSet};

use mirage_index::{MountIndex, resolve_range};
use mirage_types::{MirageError, PageOrdinal, StableFileId};
use serde::{Deserialize, Serialize};

use crate::{NormalizedTouch, TouchKind, TraceEvent};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DataQuality {
    pub non_monotonic_timestamps: u64,
    pub unknown_files: u64,
    pub invalid_ranges: u64,
    pub clipped_ranges: u64,
    pub zero_length_reads: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizedTrace {
    pub original_events: Vec<TraceEvent>,
    pub touches: Vec<NormalizedTouch>,
    pub quality: DataQuality,
}

pub fn normalize_trace(
    index: &MountIndex,
    events: &[TraceEvent],
    sample_every_nth_repeat: Option<u32>,
) -> Result<NormalizedTrace, MirageError> {
    if sample_every_nth_repeat == Some(0) {
        return Err(MirageError::invalid_argument(
            "repeat sampling interval must be greater than zero",
        ));
    }
    let mut files = BTreeMap::new();
    for ordinal in 0..index.file_count() {
        let ordinal = u32::try_from(ordinal)
            .map_err(|_| MirageError::manifest_invalid("file count exceeds u32"))?;
        let file = index.file_by_index(ordinal)?;
        if files.insert(file.stable_id(), ordinal).is_some() {
            return Err(MirageError::manifest_invalid(
                "mount index contains duplicate stable file IDs",
            ));
        }
    }
    let mut quality = DataQuality::default();
    let mut touches = Vec::new();
    let mut seen = BTreeSet::<(StableFileId, PageOrdinal)>::new();
    let mut repeats = BTreeMap::<(StableFileId, PageOrdinal), u32>::new();
    let mut prior_timestamp = None;
    for event in events {
        if prior_timestamp.is_some_and(|prior| event.timestamp_ns < prior) {
            quality.non_monotonic_timestamps += 1;
        }
        prior_timestamp = Some(event.timestamp_ns);
        if event.length == 0 {
            quality.zero_length_reads += 1;
            continue;
        }
        let Some(&ordinal) = files.get(&event.stable_file_id) else {
            quality.unknown_files += 1;
            continue;
        };
        let file = index.file_by_index(ordinal)?;
        let Some(requested_end) = event.offset.checked_add(u64::from(event.length)) else {
            quality.invalid_ranges += 1;
            continue;
        };
        if event.offset >= file.logical_size() {
            quality.invalid_ranges += 1;
            continue;
        }
        if requested_end > file.logical_size() {
            quality.clipped_ranges += 1;
        }
        for span in resolve_range(file, event.offset, event.length as usize)? {
            let key = (event.stable_file_id, span.page_ordinal);
            let kind = if seen.insert(key) {
                Some(TouchKind::First)
            } else if let Some(interval) = sample_every_nth_repeat {
                let count = repeats.entry(key).or_default();
                *count = count.saturating_add(1);
                count
                    .is_multiple_of(interval)
                    .then_some(TouchKind::SampledRepeat)
            } else {
                None
            };
            if let Some(kind) = kind {
                touches.push(NormalizedTouch {
                    timestamp_ns: event.timestamp_ns,
                    stable_file_id: event.stable_file_id,
                    page_ordinal: span.page_ordinal,
                    kind,
                });
            }
        }
    }
    Ok(NormalizedTrace {
        original_events: events.to_vec(),
        touches,
        quality,
    })
}
