use mirage_engine::{ObservationEvent, SessionObserver};
use mirage_types::{PageHash, SessionId};
use std::collections::BTreeSet;

#[test]
fn first_touch_and_violation_are_once_per_page() {
    let leased = PageHash::from_bytes([1; 32]);
    let absent = PageHash::from_bytes([2; 32]);
    let (observer, receiver) = SessionObserver::new(
        SessionId::from_bytes([3; 16]),
        true,
        BTreeSet::from([leased]),
        8,
    );
    for _ in 0..5 {
        observer.after_read(leased).expect("leased");
        observer.after_read(absent).expect("absent");
    }
    let events = receiver.try_iter().collect::<Vec<_>>();
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, ObservationEvent::FirstTouch { .. }))
            .count(),
        2
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, ObservationEvent::SealViolation { .. }))
            .count(),
        1
    );
    assert_eq!(observer.metrics().1, 1);
}
#[test]
fn saturation_never_loses_violation_count() {
    let (observer, _receiver) =
        SessionObserver::new(SessionId::from_bytes([4; 16]), true, BTreeSet::new(), 1);
    for value in 0..10 {
        observer
            .after_read(PageHash::from_bytes([value; 32]))
            .expect("read");
    }
    let (dropped, violations) = observer.metrics();
    assert!(dropped > 0);
    assert_eq!(violations, 10);
}
