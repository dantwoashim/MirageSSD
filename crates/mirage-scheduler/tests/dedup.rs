use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use mirage_scheduler::{FetchPriority, FlightFailure, FlightMap};
use mirage_types::PageHash;

#[test]
fn thousand_waiters_share_one_owner_and_observe_completion() {
    let map = Arc::new(FlightMap::default());
    let owners = Arc::new(AtomicUsize::new(0));
    let hash = PageHash::from_bytes([8; 32]);
    let barrier = Arc::new(std::sync::Barrier::new(1001));
    let threads = (0..1000)
        .map(|_| {
            let map = Arc::clone(&map);
            let owners = Arc::clone(&owners);
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                let acquired = map
                    .acquire(hash, FetchPriority::P4ReadAhead, 100, true)
                    .expect("acquire");
                owners.fetch_add(usize::from(acquired.owner), Ordering::Relaxed);
                barrier.wait();
                acquired.handle.flight().wait()
            })
        })
        .collect::<Vec<_>>();
    barrier.wait();
    assert_eq!(owners.load(Ordering::Relaxed), 1);
    assert!(map.complete(hash, Ok(())).expect("complete"));
    for thread in threads {
        assert_eq!(thread.join().expect("thread"), Ok(()));
    }
    assert!(map.is_empty());
}

#[test]
fn errors_fan_out_and_retry_gets_new_owner() {
    let map = FlightMap::default();
    let hash = PageHash::from_bytes([9; 32]);
    let first = map
        .acquire(hash, FetchPriority::P5IdleWarm, 500, false)
        .expect("first");
    let second = map
        .acquire(hash, FetchPriority::P0Blocking, 10, true)
        .expect("second");
    assert!(first.owner);
    assert!(!second.owner);
    assert_eq!(
        first.handle.flight().priority(),
        FetchPriority::P0Blocking as u8
    );
    assert_eq!(first.handle.flight().earliest_deadline_ns(), 10);
    let failure = FlightFailure {
        code: "MIRAGE_REMOTE_UNAVAILABLE".into(),
    };
    assert!(map.complete(hash, Err(failure.clone())).expect("complete"));
    assert_eq!(second.handle.flight().wait(), Err(failure));
    assert!(
        map.acquire(hash, FetchPriority::P0Blocking, 5, true)
            .expect("retry")
            .owner
    );
}
