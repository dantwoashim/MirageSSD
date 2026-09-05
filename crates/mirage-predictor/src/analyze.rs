use crate::GameProfile;
use crate::hard_set::PageKey;
use mirage_types::MirageError;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileReport {
    pub session_count: u32,
    pub unique_pages: u64,
    pub union_bytes: u64,
    pub intersection_pages: u64,
    pub startup_pages: u64,
    pub sequential_transitions: u64,
    pub random_transitions: u64,
    pub broad_scan_detected: bool,
    pub dropped_events: u64,
    pub peak_reads_per_second: u64,
}
pub fn analyze(
    profiles: &[GameProfile],
    page_size: u64,
    startup_window_us: u64,
    total_candidate_pages: u64,
) -> Result<ProfileReport, MirageError> {
    if profiles.is_empty() || page_size == 0 {
        return Err(MirageError::invalid_argument(
            "profile analysis requires sessions and page size",
        ));
    }
    let mut union = BTreeSet::<PageKey>::new();
    let mut intersection: Option<BTreeSet<PageKey>> = None;
    let mut startup = BTreeSet::new();
    let mut sequential = 0;
    let mut random = 0;
    let mut dropped = 0_u64;
    let mut peak = 0_u64;
    for profile in profiles {
        profile.validate()?;
        dropped = dropped.saturating_add(profile.dropped_event_count);
        let pages: BTreeSet<_> = profile
            .page_observations
            .iter()
            .map(|o| PageKey {
                file_index: o.file_index,
                page_ordinal: o.page_ordinal,
            })
            .collect();
        union.extend(pages.iter().copied());
        intersection = Some(match intersection {
            None => pages.clone(),
            Some(current) => current.intersection(&pages).copied().collect(),
        });
        startup.extend(
            profile
                .page_observations
                .iter()
                .filter(|o| o.first_touch_delta_us <= startup_window_us)
                .map(|o| PageKey {
                    file_index: o.file_index,
                    page_ordinal: o.page_ordinal,
                }),
        );
        for pair in profile.page_observations.windows(2) {
            if pair[0].file_index == pair[1].file_index
                && pair[0].page_ordinal.checked_add(1) == Some(pair[1].page_ordinal)
            {
                sequential += 1
            } else {
                random += 1
            }
        }
        let mut buckets = BTreeMap::<u64, u64>::new();
        for observation in &profile.page_observations {
            *buckets
                .entry(observation.first_touch_delta_us / 1_000_000)
                .or_default() += 1;
        }
        peak = peak.max(buckets.into_values().max().unwrap_or(0));
    }
    let unique_pages = union.len() as u64;
    Ok(ProfileReport {
        session_count: profiles.len() as u32,
        unique_pages,
        union_bytes: unique_pages
            .checked_mul(page_size)
            .ok_or_else(|| MirageError::invalid_argument("working set bytes overflow"))?,
        intersection_pages: intersection.map_or(0, |s| s.len() as u64),
        startup_pages: startup.len() as u64,
        sequential_transitions: sequential,
        random_transitions: random,
        broad_scan_detected: total_candidate_pages > 0
            && unique_pages.saturating_mul(100) >= total_candidate_pages.saturating_mul(80),
        dropped_events: dropped,
        peak_reads_per_second: peak,
    })
}
