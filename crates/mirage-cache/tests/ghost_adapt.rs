use mirage_cache::adapt::Adaptation;
use mirage_cache::{GhostHistory, GhostKind};
use mirage_types::PageHash;

#[test]
fn history_is_bounded_tracks_reuse_and_resets() {
    let mut history = GhostHistory::new(6).expect("history");
    for value in 0..10 {
        history.record_eviction(GhostKind::Window, PageHash::from_bytes([value; 32]), 100);
    }
    assert!(history.len() <= 2);
    assert_eq!(
        history.reuse(PageHash::from_bytes([9; 32])),
        Some(GhostKind::Window)
    );
    assert_eq!(history.metrics().evicted_before_reuse_bytes, 100);
    history.reset();
    assert_eq!(history.len(), 0);
    assert_eq!(history.metrics().evicted_before_reuse_bytes, 0);
}

#[test]
fn adaptation_obeys_bounds_and_hysteresis_step() {
    let mut tuning = Adaptation::new(4, 6, 2, 8, 1).expect("tuning");
    for _ in 0..20 {
        tuning.ghost_hit(GhostKind::Window);
        tuning.ghost_hit(GhostKind::Protected);
    }
    assert_eq!((tuning.window_target, tuning.protected_target), (8, 8));
    tuning.ghost_hit(GhostKind::Probationary);
    assert_eq!((tuning.window_target, tuning.protected_target), (7, 7));
}
