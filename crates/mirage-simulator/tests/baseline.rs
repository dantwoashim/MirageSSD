use mirage_predictor::{NormalizedTouch, TouchKind};
use mirage_simulator::{BaselineReplay, FixedPageCache, NetworkModel, ReplayConfig};
use mirage_types::{PageOrdinal, StableFileId};
use std::collections::BTreeSet;

fn page(value: u32) -> PageOrdinal {
    PageOrdinal::from_u32(value)
}
fn touch(time: u64, value: u32) -> NormalizedTouch {
    NormalizedTouch {
        timestamp_ns: time,
        stable_file_id: StableFileId::from_u64(1),
        page_ordinal: page(value),
        kind: TouchKind::First,
    }
}

#[test]
fn lru_sequence_and_hard_capacity_match_hand_calculation() {
    let mut cache = FixedPageCache::new(2, []).expect("cache");
    let sequence = [1, 2, 1, 3, 2];
    let hits = sequence.map(|value| {
        let result = cache.access(page(value)).expect("access");
        assert!(cache.len() <= cache.capacity());
        result.hit
    });
    assert_eq!(hits, [false, false, true, false, false]);
    let mut pinned = FixedPageCache::new(1, [page(9)]).expect("pinned");
    assert!(pinned.access(page(10)).is_err());
}

#[test]
fn no_prefetch_replay_metrics_are_exact() {
    let network = NetworkModel {
        base_latency_ns: 10,
        jitter_ns: 0,
        jitter_seed: 0,
        bandwidth_bytes_per_second: 1_000_000_000,
        max_concurrency: 1,
        fail_fetches: BTreeSet::new(),
    };
    let mut replay = BaselineReplay::new(ReplayConfig {
        cache_pages: 2,
        page_bytes: 100,
        pinned_pages: BTreeSet::new(),
        network,
    })
    .expect("replay");
    for touch in [
        touch(0, 1),
        touch(1_000, 2),
        touch(2_000, 1),
        touch(3_000, 3),
        touch(4_000, 2),
    ] {
        replay.process(touch).expect("process");
    }
    let metrics = replay.finish();
    assert_eq!((metrics.hits, metrics.misses), (1, 4));
    assert_eq!(metrics.remote_bytes, 400);
    assert_eq!(metrics.cache_writes, 4);
    assert_eq!(metrics.eviction_before_reuse, 1);
    assert_eq!(metrics.peak_resident_pages, 2);
    assert_eq!(metrics.blocking_ns, 440);
}
