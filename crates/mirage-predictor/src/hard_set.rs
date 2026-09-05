use crate::GameProfile;
use mirage_types::MirageError;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct PageKey {
    pub file_index: u32,
    pub page_ordinal: u32,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HardReason {
    StartupFrequency,
    Manual,
    SevereStall,
    ScanOrMap,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HardPage {
    pub page: PageKey,
    pub reasons: BTreeSet<HardReason>,
    pub source_sessions: u32,
}
#[derive(Debug, Clone)]
pub struct HardSetPolicy {
    pub startup_window_us: u64,
    pub minimum_session_count: u32,
    pub minimum_session_ratio_millionths: u32,
}

pub fn build(
    profiles: &[GameProfile],
    policy: &HardSetPolicy,
    manual: &BTreeSet<PageKey>,
    severe_stalls: &BTreeSet<PageKey>,
    scan_or_map: &BTreeSet<PageKey>,
) -> Result<Vec<HardPage>, MirageError> {
    if profiles.is_empty()
        || policy.minimum_session_count == 0
        || policy.minimum_session_ratio_millionths > 1_000_000
    {
        return Err(MirageError::invalid_argument(
            "invalid hard-set policy or empty profile set",
        ));
    }
    for profile in profiles {
        profile.validate()?;
    }
    let mut counts = BTreeMap::<PageKey, u32>::new();
    for profile in profiles {
        let session: BTreeSet<_> = profile
            .page_observations
            .iter()
            .filter(|o| o.first_touch_delta_us <= policy.startup_window_us)
            .map(|o| PageKey {
                file_index: o.file_index,
                page_ordinal: o.page_ordinal,
            })
            .collect();
        for page in session {
            *counts.entry(page).or_default() += 1;
        }
    }
    let ratio_count = ((profiles.len() as u64 * policy.minimum_session_ratio_millionths as u64)
        .div_ceil(1_000_000)) as u32;
    let threshold = policy.minimum_session_count.max(ratio_count);
    let mut pages = BTreeMap::<PageKey, HardPage>::new();
    for (page, count) in counts {
        if count >= threshold {
            pages.insert(
                page,
                HardPage {
                    page,
                    reasons: [HardReason::StartupFrequency].into(),
                    source_sessions: count,
                },
            );
        }
    }
    for (set, reason) in [
        (manual, HardReason::Manual),
        (severe_stalls, HardReason::SevereStall),
        (scan_or_map, HardReason::ScanOrMap),
    ] {
        for &page in set {
            let entry = pages.entry(page).or_insert(HardPage {
                page,
                reasons: BTreeSet::new(),
                source_sessions: 0,
            });
            entry.reasons.insert(reason);
        }
    }
    Ok(pages.into_values().collect())
}
