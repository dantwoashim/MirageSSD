use mirage_predictor::analyze::analyze;
use mirage_predictor::{GameProfile, ObservationClass, PageObservation};
use mirage_types::{ManifestHash, RepositoryId};
fn profile(label: &str, pages: &[u32]) -> GameProfile {
    GameProfile {
        format_version: 1,
        repository_id: RepositoryId::from_bytes([1; 16]),
        manifest_hash: ManifestHash::from_bytes([2; 32]),
        label: label.into(),
        page_observations: pages
            .iter()
            .enumerate()
            .map(|(i, p)| PageObservation {
                file_index: 0,
                page_ordinal: *p,
                first_touch_delta_us: i as u64 * 100_000,
                class: ObservationClass::Demand,
            })
            .collect(),
        processes: vec![],
        dropped_event_count: 0,
    }
}
#[test]
fn recurring_set_and_whole_scan_are_reported() {
    let r = analyze(
        &[profile("a", &[0, 1, 2, 3]), profile("b", &[0, 1, 2, 3, 4])],
        65536,
        150_000,
        5,
    )
    .unwrap();
    assert_eq!(r.intersection_pages, 4);
    assert_eq!(r.startup_pages, 2);
    assert!(r.broad_scan_detected);
    assert!(r.sequential_transitions > r.random_transitions);
}
