use mirage_cache::recency::RecencyEvent;
use mirage_cache::{Segment, SegmentedRecency};

#[test]
fn promotion_demotion_pins_and_victims_preserve_links() {
    let (mut policy, queue) = SegmentedRecency::new(1, 16);
    policy.insert(1, Segment::Probationary).expect("insert");
    policy.insert(2, Segment::Probationary).expect("insert");
    policy.insert(3, Segment::PrefetchedUnused).expect("insert");
    assert_eq!(policy.victim(), Some(3));
    assert!(queue.submit(RecencyEvent::Touch(1)));
    assert!(queue.submit(RecencyEvent::Touch(2)));
    policy.drain().expect("drain");
    assert_eq!(policy.segment(2), Some(Segment::Protected));
    assert_eq!(policy.segment(1), Some(Segment::Probationary));
    assert!(queue.submit(RecencyEvent::Pin(3)));
    policy.drain().expect("pin");
    assert_eq!(policy.segment(3), Some(Segment::Detached));
    assert!(queue.submit(RecencyEvent::Unpin(3)));
    policy.drain().expect("unpin");
    assert_eq!(policy.segment(3), Some(Segment::Probationary));
    policy.verify().expect("invariants");
}

#[test]
fn touch_queue_is_bounded_and_reports_overflow() {
    let (_policy, queue) = SegmentedRecency::new(1, 1);
    assert!(queue.submit(RecencyEvent::Touch(1)));
    assert!(!queue.submit(RecencyEvent::Touch(2)));
    assert_eq!(queue.metrics().overflow(), 1);
}
