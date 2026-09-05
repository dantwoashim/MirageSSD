use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};

use mirage_predictor::NormalizedTouch;
use mirage_types::MirageError;
use serde::{Deserialize, Serialize};

use crate::{BaselineReplay, NetworkModel, ReplayConfig, ReplayMetrics};

const MAX_SWEEP_CASES: usize = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObjectiveWeights {
    pub blocking_ns: u64,
    pub remote_bytes: u64,
    pub cache_bytes: u64,
}

#[derive(Debug, Clone)]
pub struct SweepInput {
    pub page_bytes: Vec<u64>,
    pub cache_pages: Vec<usize>,
    pub networks: Vec<NetworkModel>,
    pub weights: ObjectiveWeights,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SweepCase {
    pub ordinal: usize,
    pub page_bytes: u64,
    pub cache_pages: usize,
    pub network: NetworkModel,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SweepResult {
    pub case: SweepCase,
    pub metrics: ReplayMetrics,
    pub objective: u128,
    pub pareto_optimal: bool,
}

pub fn run_sweep(
    input: &SweepInput,
    touches: &[NormalizedTouch],
) -> Result<Vec<SweepResult>, MirageError> {
    if input.page_bytes.is_empty()
        || input.cache_pages.is_empty()
        || input.networks.is_empty()
        || (input.weights.blocking_ns | input.weights.remote_bytes | input.weights.cache_bytes) == 0
    {
        return Err(MirageError::invalid_argument(
            "sweep dimensions and explicit objective weights must be non-zero",
        ));
    }
    let count = input
        .page_bytes
        .len()
        .checked_mul(input.cache_pages.len())
        .and_then(|value| value.checked_mul(input.networks.len()))
        .ok_or_else(|| MirageError::invalid_argument("sweep case count overflows"))?;
    if count > MAX_SWEEP_CASES {
        return Err(MirageError::invalid_argument(
            "sweep exceeds the bounded case count",
        ));
    }
    let mut cases = Vec::with_capacity(count);
    for &page_bytes in &input.page_bytes {
        if page_bytes == 0 {
            return Err(MirageError::invalid_argument(
                "sweep page bytes must be non-zero",
            ));
        }
        for &cache_pages in &input.cache_pages {
            if cache_pages == 0 {
                return Err(MirageError::invalid_argument(
                    "sweep cache pages must be non-zero",
                ));
            }
            for network in &input.networks {
                cases.push(SweepCase {
                    ordinal: cases.len(),
                    page_bytes,
                    cache_pages,
                    network: network.clone(),
                });
            }
        }
    }
    let cases = Arc::new(cases);
    let touches = Arc::new(touches.to_vec());
    let cursor = AtomicUsize::new(0);
    let output = Mutex::new(Vec::with_capacity(count));
    let workers = std::thread::available_parallelism()
        .map_or(1, usize::from)
        .min(count.max(1));
    std::thread::scope(|scope| {
        for _ in 0..workers {
            let cases = Arc::clone(&cases);
            let touches = Arc::clone(&touches);
            let cursor = &cursor;
            let output = &output;
            let weights = input.weights;
            scope.spawn(move || {
                loop {
                    let index = cursor.fetch_add(1, Ordering::Relaxed);
                    let Some(case) = cases.get(index) else {
                        break;
                    };
                    let result = simulate_case(case.clone(), &touches, weights);
                    output
                        .lock()
                        .expect("sweep result mutex poisoned")
                        .push(result);
                }
            });
        }
    });
    let raw = output
        .into_inner()
        .map_err(|_| MirageError::internal_invariant("sweep result mutex poisoned"))?;
    let mut results = raw.into_iter().collect::<Result<Vec<_>, _>>()?;
    results.sort_by_key(|result| result.case.ordinal);
    mark_pareto(&mut results);
    Ok(results)
}

fn simulate_case(
    case: SweepCase,
    touches: &[NormalizedTouch],
    weights: ObjectiveWeights,
) -> Result<SweepResult, MirageError> {
    let mut replay = BaselineReplay::new(ReplayConfig {
        cache_pages: case.cache_pages,
        page_bytes: case.page_bytes,
        pinned_pages: Default::default(),
        network: case.network.clone(),
    })?;
    for touch in touches {
        replay.process(*touch)?;
    }
    let metrics = replay.finish();
    let cache_bytes = (case.cache_pages as u128)
        .checked_mul(case.page_bytes as u128)
        .ok_or_else(|| MirageError::invalid_argument("cache byte objective overflows"))?;
    let objective = u128::from(metrics.blocking_ns)
        .checked_mul(u128::from(weights.blocking_ns))
        .and_then(|value| {
            value.checked_add(u128::from(metrics.remote_bytes) * u128::from(weights.remote_bytes))
        })
        .and_then(|value| value.checked_add(cache_bytes * u128::from(weights.cache_bytes)))
        .ok_or_else(|| MirageError::invalid_argument("sweep objective overflows"))?;
    Ok(SweepResult {
        case,
        metrics,
        objective,
        pareto_optimal: false,
    })
}

fn mark_pareto(results: &mut [SweepResult]) {
    for index in 0..results.len() {
        let candidate = &results[index];
        let candidate_cache =
            candidate.case.cache_pages as u128 * candidate.case.page_bytes as u128;
        results[index].pareto_optimal = !(0..results.len()).any(|other| {
            if other == index {
                return false;
            }
            let challenger = &results[other];
            let challenger_cache =
                challenger.case.cache_pages as u128 * challenger.case.page_bytes as u128;
            challenger_cache <= candidate_cache
                && challenger.metrics.blocking_ns <= candidate.metrics.blocking_ns
                && challenger.metrics.remote_bytes <= candidate.metrics.remote_bytes
                && (challenger_cache < candidate_cache
                    || challenger.metrics.blocking_ns < candidate.metrics.blocking_ns
                    || challenger.metrics.remote_bytes < candidate.metrics.remote_bytes)
        });
    }
}
