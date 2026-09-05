use mirage_backend::{BackendId, ObjectKind, ProviderObjectId, RemoteObjectRef};
use mirage_scheduler::{FetchPriority, FetchRequest, PageRequest, SchedulerQueue};
use mirage_types::{ByteCount, CheckedRange, ContentHash, PageHash};
use tokio_util::sync::CancellationToken;

fn request(priority: FetchPriority, sequence: u64, page: u8) -> FetchRequest {
    FetchRequest {
        pages: vec![PageRequest {
            hash: PageHash::from_bytes([page; 32]),
            encoded_range: CheckedRange::new(0, 10).expect("range"),
            logical_length: 10,
        }],
        object: RemoteObjectRef {
            backend_id: BackendId::new("local").expect("backend"),
            provider_object_id: ProviderObjectId::new("pack").expect("object"),
            immutable_revision: None,
            byte_length: ByteCount::from_u64(10),
            content_hash: ContentHash::from_bytes([1; 32]),
            kind: ObjectKind::Pack,
        },
        priority,
        deadline_ns: 100,
        sequence,
        cancellation: CancellationToken::new(),
        budget: None,
    }
}

#[test]
fn p0_preempts_stably_and_cancelled_work_is_skipped() {
    let mut queue = SchedulerQueue::new(3);
    let cancelled = request(FetchPriority::P4ReadAhead, 1, 1);
    cancelled.cancellation.cancel();
    assert!(queue.enqueue(cancelled));
    assert!(queue.enqueue(request(FetchPriority::P5IdleWarm, 2, 2)));
    assert!(queue.enqueue(request(FetchPriority::P0Blocking, 3, 3)));
    assert_eq!(
        queue.dequeue().expect("p0").priority,
        FetchPriority::P0Blocking
    );
    assert_eq!(queue.dequeue().expect("next").sequence, 2);
    assert_eq!(queue.metrics().cancelled_before_dequeue, 1);
}

#[test]
fn saturation_drops_speculation_and_promotion_reorders() {
    let mut queue = SchedulerQueue::new(1);
    assert!(queue.enqueue(request(FetchPriority::P4ReadAhead, 1, 1)));
    assert!(!queue.enqueue(request(FetchPriority::P5IdleWarm, 2, 2)));
    assert!(queue.promote(PageHash::from_bytes([1; 32]), 5));
    assert_eq!(
        queue.dequeue().expect("promoted").priority,
        FetchPriority::P0Blocking
    );
    assert_eq!(queue.metrics().dropped_speculative, 1);
}
