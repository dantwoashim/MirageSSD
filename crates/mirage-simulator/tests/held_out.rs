use mirage_predictor::{GameProfile, ObservationClass, PageObservation};
use mirage_simulator::held_out::{Baseline, evaluate};
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
                first_touch_delta_us: i as u64,
                class: ObservationClass::Demand,
            })
            .collect(),
        processes: vec![],
        dropped_event_count: 0,
    }
}
#[test]
fn folds_never_train_on_held_out_session() {
    let metrics = evaluate(&[
        profile("a", &[1, 2]),
        profile("b", &[1, 3]),
        profile("c", &[1, 4]),
    ])
    .unwrap();
    assert_eq!(metrics.len(), 12);
    for fold in metrics.chunks(4) {
        assert_eq!(fold[0].baseline, Baseline::NoPrefetch);
        assert_eq!(fold[0].hit_pages, 0);
        assert!(
            fold.iter()
                .all(|m| m.held_out_index == fold[0].held_out_index)
        );
    }
    assert!(
        metrics
            .iter()
            .filter(|m| m.baseline == Baseline::RecentUnion)
            .all(|m| m.violations == 1)
    );
}
