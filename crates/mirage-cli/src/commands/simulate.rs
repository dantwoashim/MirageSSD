use std::collections::BTreeSet;
use std::io::Write;
use std::path::Path;

use mirage_index::MountIndex;
use mirage_predictor::{NormalizedTouch, TraceBlockDecoder, normalize_trace};
use mirage_simulator::{
    BaselineReplay, NetworkModel, ObjectiveWeights, ReplayConfig, SweepInput, render_markdown,
    run_sweep,
};
use mirage_types::MirageError;

use crate::output;

const MAX_SESSION_EVENTS: usize = 10_000_000;

pub fn run(
    trace: &Path,
    index: &Path,
    cache_pages: usize,
    latency_ms: u64,
    bandwidth: u64,
    json: bool,
) -> Result<(), MirageError> {
    let (touches, page_bytes) = load_touches(trace, index)?;
    let network = network(latency_ms, bandwidth, 0)?;
    let mut replay = BaselineReplay::new(ReplayConfig {
        cache_pages,
        page_bytes,
        pinned_pages: BTreeSet::new(),
        network,
    })?;
    for touch in touches {
        replay.process(touch)?;
    }
    let metrics = replay.finish();
    if json {
        output::emit_success(
            &serde_json::json!({ "report_version": 1, "parameters": { "cache_pages": cache_pages, "page_bytes": page_bytes, "latency_ms": latency_ms, "bandwidth_bytes_per_second": bandwidth }, "metrics": metrics }),
        )
    } else {
        println!(
            "baseline: {} hits, {} misses, {} blocking ns",
            metrics.hits, metrics.misses, metrics.blocking_ns
        );
        Ok(())
    }
}

pub struct SweepArgs<'a> {
    pub trace: &'a Path,
    pub index: &'a Path,
    pub page_bytes: Vec<u64>,
    pub cache_pages: Vec<usize>,
    pub latency_ms: Vec<u64>,
    pub bandwidth_bytes_per_second: u64,
    pub blocking_weight: u64,
    pub remote_weight: u64,
    pub cache_weight: u64,
    pub markdown: &'a Path,
    pub json: bool,
}

pub fn sweep(args: SweepArgs<'_>) -> Result<(), MirageError> {
    let (touches, trace_page_bytes) = load_touches(args.trace, args.index)?;
    if !args.page_bytes.contains(&trace_page_bytes) {
        return Err(MirageError::invalid_argument(
            "page-byte sweep must include the trace/index page size",
        ));
    }
    let networks = args
        .latency_ms
        .iter()
        .enumerate()
        .map(|(ordinal, latency)| {
            network(*latency, args.bandwidth_bytes_per_second, ordinal as u64)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let weights = ObjectiveWeights {
        blocking_ns: args.blocking_weight,
        remote_bytes: args.remote_weight,
        cache_bytes: args.cache_weight,
    };
    let results = run_sweep(
        &SweepInput {
            page_bytes: args.page_bytes,
            cache_pages: args.cache_pages,
            networks,
            weights,
        },
        &touches,
    )?;
    let markdown = render_markdown(&results, weights)?;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(args.markdown)
        .map_err(MirageError::from)?;
    file.write_all(markdown.as_bytes())
        .map_err(MirageError::from)?;
    file.sync_all().map_err(MirageError::from)?;
    if args.json {
        output::emit_success(
            &serde_json::json!({ "report_version": 1, "warning": mirage_simulator::GATE_A_SYNTHETIC_WARNING, "weights": weights, "results": results, "markdown": args.markdown }),
        )
    } else {
        println!(
            "simulator sweep complete: {} cases; report written",
            results.len()
        );
        Ok(())
    }
}

fn load_touches(trace: &Path, index: &Path) -> Result<(Vec<NormalizedTouch>, u64), MirageError> {
    let index = MountIndex::open(index)?;
    let file = std::fs::File::open(trace).map_err(MirageError::from)?;
    let mut decoder = TraceBlockDecoder::new(file)?;
    if u64::from(decoder.header.page_size) != index.header().page_size {
        return Err(MirageError::invalid_argument(
            "trace and index page sizes differ",
        ));
    }
    let mut events = Vec::new();
    while let Some(block) = decoder.read_block()? {
        if events.len().saturating_add(block.len()) > MAX_SESSION_EVENTS {
            return Err(MirageError::invalid_argument(
                "trace exceeds session event bound",
            ));
        }
        events.extend(block);
    }
    let normalized = normalize_trace(&index, &events, Some(1))?;
    if normalized.quality.non_monotonic_timestamps != 0
        || normalized.quality.unknown_files != 0
        || normalized.quality.invalid_ranges != 0
    {
        return Err(MirageError::invalid_argument(
            "trace contains impossible timestamps, files, or ranges",
        ));
    }
    Ok((normalized.touches, index.header().page_size))
}

fn network(latency_ms: u64, bandwidth: u64, seed: u64) -> Result<NetworkModel, MirageError> {
    Ok(NetworkModel {
        base_latency_ns: latency_ms
            .checked_mul(1_000_000)
            .ok_or_else(|| MirageError::invalid_argument("latency overflows nanoseconds"))?,
        jitter_ns: 0,
        jitter_seed: seed,
        bandwidth_bytes_per_second: bandwidth,
        max_concurrency: 1,
        fail_fetches: BTreeSet::new(),
    })
}
