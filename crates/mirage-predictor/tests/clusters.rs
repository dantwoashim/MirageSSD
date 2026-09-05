use mirage_predictor::coaccess::{TimedPage, cluster};
fn event(t: u64, p: u64) -> TimedPage {
    TimedPage {
        timestamp_us: t,
        page: p,
        scan: false,
    }
}
#[test]
fn phases_stay_separate_noise_is_ignored_and_size_is_bounded() {
    let sessions = vec![
        vec![event(0, 1), event(1, 2), event(100, 8), event(101, 9)],
        vec![event(0, 1), event(2, 2), event(100, 8), event(102, 9)],
        vec![event(0, 1), event(50, 8)],
    ];
    let clusters = cluster(&sessions, 5, 2, 2).unwrap();
    assert_eq!(clusters.len(), 2);
    assert!(
        clusters
            .iter()
            .all(|c| c.pages.len() == 2 && c.session_support == 2)
    );
    assert_eq!(clusters, cluster(&sessions, 5, 2, 2).unwrap());
}
