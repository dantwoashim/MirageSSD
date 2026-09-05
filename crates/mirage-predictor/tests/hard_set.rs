use mirage_predictor::hard_set::{HardReason, HardSetPolicy, PageKey, build};
use mirage_predictor::{GameProfile, ObservationClass, PageObservation};
use mirage_types::{ManifestHash, RepositoryId};
use std::collections::BTreeSet;

fn profile(label: &str, pages: &[(u32, u64)]) -> GameProfile {
    GameProfile {
        format_version: 1,
        repository_id: RepositoryId::from_bytes([1; 16]),
        manifest_hash: ManifestHash::from_bytes([2; 32]),
        label: label.into(),
        page_observations: pages
            .iter()
            .map(|(page, time)| PageObservation {
                file_index: 0,
                page_ordinal: *page,
                first_touch_delta_us: *time,
                class: ObservationClass::Demand,
            })
            .collect(),
        processes: Vec::new(),
        dropped_event_count: 0,
    }
}
#[test]
fn frequency_manual_and_singletons_are_explainable() {
    let profiles = [
        profile("a", &[(1, 10), (2, 20)]),
        profile("b", &[(1, 15), (3, 20)]),
        profile("c", &[(1, 30)]),
    ];
    let manual = [PageKey {
        file_index: 0,
        page_ordinal: 9,
    }]
    .into_iter()
    .collect();
    let pages = build(
        &profiles,
        &HardSetPolicy {
            startup_window_us: 100,
            minimum_session_count: 2,
            minimum_session_ratio_millionths: 500_000,
        },
        &manual,
        &BTreeSet::new(),
        &BTreeSet::new(),
    )
    .unwrap();
    assert_eq!(pages.len(), 2);
    assert_eq!(pages[0].source_sessions, 3);
    assert!(pages[0].reasons.contains(&HardReason::StartupFrequency));
    assert!(pages[1].reasons.contains(&HardReason::Manual));
}
