use std::collections::BTreeSet;

use mirage_predictor::{NormalizedTouch, TouchKind};
use mirage_simulator::{NetworkModel, ObjectiveWeights, SweepInput, run_sweep};
use mirage_types::{PageOrdinal, StableFileId};

fn touches() -> Vec<NormalizedTouch> {
    [0_u32, 1, 0, 2, 1, 3, 0]
        .into_iter()
        .enumerate()
        .map(|(index, page)| NormalizedTouch {
            timestamp_ns: index as u64 * 1_000_000,
            stable_file_id: StableFileId::from_u64(1),
            page_ordinal: PageOrdinal::from_u32(page),
            kind: TouchKind::First,
        })
        .collect()
}

#[test]
fn sweep_is_reproducible_complete_ordered_and_parameterized() {
    let networks = vec![
        NetworkModel {
            base_latency_ns: 1_000_000,
            jitter_ns: 0,
            jitter_seed: 1,
            bandwidth_bytes_per_second: 100_000_000,
            max_concurrency: 1,
            fail_fetches: BTreeSet::new(),
        },
        NetworkModel {
            base_latency_ns: 10_000_000,
            jitter_ns: 1000,
            jitter_seed: 2,
            bandwidth_bytes_per_second: 20_000_000,
            max_concurrency: 2,
            fail_fetches: BTreeSet::new(),
        },
    ];
    let input = SweepInput {
        page_bytes: vec![64 * 1024, 1024 * 1024],
        cache_pages: vec![2, 4, 8],
        networks,
        weights: ObjectiveWeights {
            blocking_ns: 5,
            remote_bytes: 2,
            cache_bytes: 1,
        },
    };
    let first = run_sweep(&input, &touches()).expect("first sweep");
    let second = run_sweep(&input, &touches()).expect("second sweep");
    assert_eq!(first, second);
    assert_eq!(first.len(), 12);
    assert!(
        first
            .iter()
            .enumerate()
            .all(|(ordinal, result)| result.case.ordinal == ordinal)
    );
    assert!(first.iter().any(|result| result.pareto_optimal));
    assert!(
        first
            .iter()
            .all(|result| result.metrics.peak_resident_pages <= result.case.cache_pages as u64)
    );
}
