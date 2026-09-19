//! Page-geometry benchmark on fresh synthetic fixtures (continuum proposal,
//! section 5 step 6 and B11). Measures the supported 64 KiB, 256 KiB and
//! 1 MiB page sizes on the same source bytes: pack/index/arena metadata
//! footprint, preparation time, resident read latency for aligned and
//! boundary-crossing 4 KiB reads and 1 MiB sequential reads, and computed
//! miss amplification. Results are synthetic-fixture measurements on this
//! machine, not game measurements.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use bytes::Bytes;
use mirage_cache::{ArenaShard, CacheLayout, ResidentIndex, insert_page, insert_reserved_pages};
use mirage_db::{CacheShardSpec, Database};
use mirage_index::compile_to_bytes;
use mirage_manifest::FileClass;
use mirage_pack::{ImportPlan, PlainPage, PlannedFile, import_local};
use mirage_simulator::{median_of_per_run_percentiles, percentile_nearest_rank, pooled_percentile};
use mirage_types::{ByteCount, GenerationId, PageHash, RepositoryId};
use serde_json::{Value, json};

const PAGE_SIZES: [u64; 3] = [64 * 1024, 256 * 1024, 1024 * 1024];
const SMALL_READ: usize = 4096;
const LARGE_READ: usize = 1024 * 1024;

struct Options {
    fixture_bytes: u64,
    runs: usize,
    small_reads: usize,
    large_reads: usize,
    direct: bool,
    coalesce: bool,
    batched: bool,
    out: Option<PathBuf>,
}

fn parse_options() -> Result<Options, String> {
    let mut options = Options {
        fixture_bytes: 256 * 1024 * 1024,
        runs: 5,
        small_reads: 20_000,
        large_reads: 200,
        direct: false,
        coalesce: false,
        batched: false,
        out: None,
    };
    let mut arguments = std::env::args().skip(1);
    while let Some(argument) = arguments.next() {
        let mut value = || arguments.next().ok_or(format!("{argument} needs a value"));
        match argument.as_str() {
            "--fixture-bytes" => {
                options.fixture_bytes = value()?.parse().map_err(|e| format!("{e}"))?
            }
            "--runs" => options.runs = value()?.parse().map_err(|e| format!("{e}"))?,
            "--small-reads" => {
                options.small_reads = value()?.parse().map_err(|e| format!("{e}"))?
            }
            "--large-reads" => {
                options.large_reads = value()?.parse().map_err(|e| format!("{e}"))?
            }
            "--direct" => options.direct = true,
            "--coalesce" => options.coalesce = true,
            "--batched" => options.batched = true,
            "--out" => options.out = Some(PathBuf::from(value()?)),
            other => return Err(format!("unknown argument {other}")),
        }
    }
    if options.runs == 0 || options.small_reads == 0 || options.large_reads == 0 {
        return Err("runs and read counts must be positive".into());
    }
    if options.fixture_bytes < 4 * 1024 * 1024 || !options.fixture_bytes.is_multiple_of(1024 * 1024)
    {
        return Err("fixture bytes must be a multiple of 1 MiB and at least 4 MiB".into());
    }
    Ok(options)
}

struct XorShift(u64);
impl XorShift {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    fn below(&mut self, bound: u64) -> u64 {
        self.next() % bound.max(1)
    }
}

fn generate_source(path: &Path, bytes: u64) -> Result<(), Box<dyn std::error::Error>> {
    let mut random = XorShift(0x4d49_5241_4745_0001);
    let mut buffer = vec![0_u8; 1024 * 1024];
    let mut file = std::fs::File::create(path)?;
    use std::io::Write;
    for _ in 0..bytes / buffer.len() as u64 {
        for chunk in buffer.chunks_mut(8) {
            let word = random.next().to_le_bytes();
            chunk.copy_from_slice(&word[..chunk.len()]);
        }
        file.write_all(&buffer)?;
    }
    file.sync_all()?;
    Ok(())
}

fn directory_bytes(path: &Path) -> u64 {
    std::fs::read_dir(path)
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .filter_map(|entry| entry.metadata().ok())
                .filter(|metadata| metadata.is_file())
                .map(|metadata| metadata.len())
                .sum()
        })
        .unwrap_or(0)
}

struct Fixture {
    shard: Arc<ArenaShard>,
    index: ResidentIndex,
    hashes: Vec<PageHash>,
    page_size: u64,
    footprint: Value,
    materialize_ns: u128,
}

fn build_fixture(
    root: &Path,
    source: &Path,
    page_size: u64,
    direct: bool,
    batched: bool,
) -> Result<Fixture, Box<dyn std::error::Error>> {
    let source_bytes = std::fs::metadata(source)?.len();
    let page_count = source_bytes.div_ceil(page_size);
    let import_root = root.join(format!("import-{page_size}"));
    let started = Instant::now();
    let imported = import_local(&ImportPlan {
        repository_id: RepositoryId::from_bytes([0x61; 16]),
        generation_id: GenerationId::ZERO,
        source_root: source.parent().ok_or("source has no parent")?.to_path_buf(),
        files: vec![PlannedFile {
            relative_path: source
                .file_name()
                .ok_or("source has no file name")?
                .to_string_lossy()
                .into_owned(),
            class: FileClass::VirtualContainer,
        }],
        page_size: u32::try_from(page_size)?,
        pack_target: 64 * 1024 * 1024,
        output_staging_directory: import_root.clone(),
        encryption: None,
    })?;
    let import_ns = started.elapsed().as_nanos();
    let index_bytes = compile_to_bytes(&imported.manifest)?.len() as u64;
    let pack_bytes = directory_bytes(&import_root);

    let layout = CacheLayout {
        page_size: ByteCount::from_u64(page_size),
        slot_count: u32::try_from(page_count + 1)?,
        db_journal_allowance: ByteCount::ZERO,
        filesystem_reserve: ByteCount::ZERO,
    };
    let arena_path = root.join(format!("arena-{page_size}.bin"));
    let shard = Arc::new(ArenaShard::create(&arena_path, layout)?);
    let db = Database::open(&root.join(format!("control-{page_size}.db")))?;
    db.register_cache_shard(CacheShardSpec {
        shard_id: 0,
        relative_path: format!("arena-{page_size}.bin"),
        page_size: layout.page_size,
        slot_count: layout.slot_count,
    })?;
    let source_data = std::fs::read(source)?;
    let insert_started = Instant::now();
    let mut hashes = Vec::with_capacity(page_count as usize);
    if batched {
        // The product's batched materializer: reserve up to 64 slots at once,
        // then insert them as one batch so consecutive slots are written as
        // contiguous runs.
        // insert_reserved_pages bounds a batch at 64 pages and 32 MiB.
        const BATCH_PAGES: usize = 64;
        const BATCH_BYTES: u64 = 32 * 1024 * 1024;
        let batch_pages = BATCH_PAGES.min((BATCH_BYTES / page_size).max(1) as usize);
        let mut chunks = source_data.chunks(page_size as usize).peekable();
        while chunks.peek().is_some() {
            let pages: Vec<PlainPage> = chunks
                .by_ref()
                .take(batch_pages)
                .map(|chunk| PlainPage::from_bytes(Bytes::copy_from_slice(chunk)))
                .collect();
            let outcomes = db.reserve_cache_slots_batch(
                pages
                    .iter()
                    .map(|page| (page.hash, page.bytes.len() as u32))
                    .collect(),
            )?;
            let mut batch = Vec::with_capacity(pages.len());
            for outcome in outcomes {
                let mirage_db::ReserveCacheSlotOutcome::Reserved(record) = outcome else {
                    return Err(mirage_types::MirageError::internal_invariant(
                        "fixture page was already resident",
                    )
                    .into());
                };
                let page = pages
                    .iter()
                    .find(|page| Some(page.hash) == record.page_hash)
                    .ok_or_else(|| {
                        mirage_types::MirageError::internal_invariant(
                            "reservation returned an unknown page hash",
                        )
                    })?;
                batch.push((record, page.bytes.as_ref()));
            }
            insert_reserved_pages(&db, Arc::clone(&shard), &batch, &())?;
            hashes.extend(pages.iter().map(|page| page.hash));
        }
    } else {
        for chunk in source_data.chunks(page_size as usize) {
            let page = PlainPage::from_bytes(Bytes::copy_from_slice(chunk));
            insert_page(&db, Arc::clone(&shard), page.hash, &page.bytes, &())?;
            hashes.push(page.hash);
        }
    }
    shard.flush()?;
    let insert_ns = insert_started.elapsed().as_nanos();
    drop(shard);
    let shard = Arc::new(if direct {
        ArenaShard::open_read_unbuffered(&arena_path, layout)?
    } else {
        ArenaShard::open(&arena_path, layout)?
    });
    let index = ResidentIndex::rebuild(&db, Arc::clone(&shard))?;
    let db_bytes = std::fs::metadata(root.join(format!("control-{page_size}.db")))
        .map(|m| m.len())
        .unwrap_or(0);
    let footprint = json!({
        "source_bytes": source_bytes,
        "page_count": page_count,
        "unique_pages": imported.report.unique_pages,
        "pack_bytes": pack_bytes,
        "pack_overhead_bytes": pack_bytes.saturating_sub(source_bytes),
        "mount_index_bytes": index_bytes,
        "mount_index_bytes_per_page": index_bytes as f64 / page_count as f64,
        "arena_logical_bytes": layout.logical_shard_bytes()?,
        "arena_metadata_bytes": layout.metadata_bytes()?,
        "arena_physical_extents": shard.physical_extent_count().ok(),
        "control_db_bytes": db_bytes,
        "import_ns": import_ns,
        "insert_ns": insert_ns,
    });
    Ok(Fixture {
        shard,
        index,
        hashes,
        page_size,
        footprint,
        materialize_ns: import_ns + insert_ns,
    })
}

/// Attribution counters for one measurement: how many requests took the
/// coalesced path or fell back, and how the wall time splits between lease
/// acquisition (`ResidentIndex::acquire`) and the arena I/O itself.
#[derive(Default)]
struct ReadStats {
    coalesced_requests: u64,
    fallback_requests: u64,
    lease_ns: u64,
    io_ns: u64,
}

fn read_span(
    fixture: &Fixture,
    logical_offset: u64,
    output: &mut [u8],
    coalesce: bool,
    stats: &mut ReadStats,
) -> Result<(), mirage_types::MirageError> {
    if coalesce {
        let first_page = (logical_offset / fixture.page_size) as usize;
        let last_page = ((logical_offset + output.len() as u64 - 1) / fixture.page_size) as usize;
        let mut guards = Vec::with_capacity(last_page - first_page + 1);
        let mut all_resident = true;
        let lease_started = Instant::now();
        for page in first_page..=last_page {
            match fixture.index.acquire(fixture.hashes[page])? {
                Some(guard) => guards.push(guard),
                None => {
                    all_resident = false;
                    break;
                }
            }
        }
        stats.lease_ns += lease_started.elapsed().as_nanos() as u64;
        if all_resident && mirage_cache::slots_are_contiguous(&guards) {
            stats.coalesced_requests += 1;
            let io_started = Instant::now();
            let result = mirage_cache::read_contiguous(
                &fixture.shard,
                &guards,
                (logical_offset % fixture.page_size) as u32,
                output,
            );
            stats.io_ns += io_started.elapsed().as_nanos() as u64;
            return result;
        }
        // Not a contiguous resident run: fall back to the per-page loop below.
        stats.fallback_requests += 1;
    }
    let mut done = 0_usize;
    while done < output.len() {
        let absolute = logical_offset + done as u64;
        let page = (absolute / fixture.page_size) as usize;
        let within = (absolute % fixture.page_size) as usize;
        let lease_started = Instant::now();
        let guard = fixture
            .index
            .acquire(fixture.hashes[page])?
            .ok_or_else(|| mirage_types::MirageError::internal_invariant("page not resident"))?;
        stats.lease_ns += lease_started.elapsed().as_nanos() as u64;
        let available = guard.logical_length() as usize - within;
        let take = available.min(output.len() - done);
        let io_started = Instant::now();
        guard.read_exact(within as u32, &mut output[done..done + take])?;
        stats.io_ns += io_started.elapsed().as_nanos() as u64;
        done += take;
    }
    Ok(())
}

struct LatencyRuns {
    per_run_ns: Vec<Vec<u64>>,
    errors: u64,
    stats: ReadStats,
}

fn measure(
    fixture: &Fixture,
    runs: usize,
    reads: usize,
    read_len: usize,
    coalesce: bool,
    offset_for: impl Fn(&mut XorShift, &Fixture) -> u64,
) -> LatencyRuns {
    let mut random = XorShift(0x0bad_5eed_0000_0001 ^ fixture.page_size);
    let mut output = vec![0_u8; read_len];
    let mut per_run_ns = Vec::with_capacity(runs);
    let mut errors = 0;
    let mut stats = ReadStats::default();
    for _ in 0..runs {
        let mut samples = Vec::with_capacity(reads);
        for _ in 0..reads {
            let offset = offset_for(&mut random, fixture);
            let started = Instant::now();
            let result = read_span(fixture, offset, &mut output, coalesce, &mut stats);
            let elapsed = started.elapsed().as_nanos() as u64;
            if result.is_ok() {
                samples.push(elapsed);
            } else {
                errors += 1;
            }
        }
        per_run_ns.push(samples);
    }
    LatencyRuns {
        per_run_ns,
        errors,
        stats,
    }
}

fn summarize(runs: &LatencyRuns) -> Value {
    let non_empty: Vec<Vec<u64>> = runs
        .per_run_ns
        .iter()
        .filter(|run| !run.is_empty())
        .cloned()
        .collect();
    if non_empty.is_empty() {
        return json!({ "errors": runs.errors, "samples": 0 });
    }
    let per_run = |pct: u8| -> Vec<u64> {
        non_empty
            .iter()
            .map(|run| {
                let mut sorted = run.clone();
                sorted.sort_unstable();
                percentile_nearest_rank(&sorted, pct).unwrap_or(0)
            })
            .collect()
    };
    let mean_ns = {
        let (sum, count) = non_empty
            .iter()
            .flatten()
            .fold((0_u128, 0_u128), |(s, c), v| (s + u128::from(*v), c + 1));
        (sum / count.max(1)) as u64
    };
    json!({
        "samples": non_empty.iter().map(Vec::len).sum::<usize>(),
        "errors": runs.errors,
        "mean_ns": mean_ns,
        "per_run_p50_ns": per_run(50),
        "per_run_p99_ns": per_run(99),
        "median_of_per_run_p99_ns": median_of_per_run_percentiles(&non_empty, 99).unwrap_or(0),
        "pooled_p99_ns": pooled_percentile(&non_empty, 99).unwrap_or(0),
        "pooled_p50_ns": pooled_percentile(&non_empty, 50).unwrap_or(0),
        "coalesced_requests": runs.stats.coalesced_requests,
        "fallback_requests": runs.stats.fallback_requests,
        "lease_ns_total": runs.stats.lease_ns,
        "io_ns_total": runs.stats.io_ns,
    })
}

fn main() {
    let options = match parse_options() {
        Ok(options) => options,
        Err(message) => {
            eprintln!("page_geometry: {message}");
            std::process::exit(2);
        }
    };
    if let Err(error) = run(&options) {
        eprintln!("page_geometry: {error}");
        std::process::exit(1);
    }
}

fn run(options: &Options) -> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let source_dir = root.path().join("source");
    std::fs::create_dir_all(&source_dir)?;
    let source = source_dir.join("fixture.bin");
    generate_source(&source, options.fixture_bytes)?;

    let mut results = Vec::new();
    for page_size in PAGE_SIZES {
        eprintln!("page_geometry: page_size={page_size} building fixture");
        let fixture = build_fixture(
            root.path(),
            &source,
            page_size,
            options.direct,
            options.batched,
        )?;
        let total = options.fixture_bytes;
        let aligned = measure(
            &fixture,
            options.runs,
            options.small_reads,
            SMALL_READ,
            options.coalesce,
            |r: &mut XorShift, _| r.below(total / SMALL_READ as u64) * SMALL_READ as u64,
        );
        let boundary = measure(
            &fixture,
            options.runs,
            options.small_reads,
            SMALL_READ,
            options.coalesce,
            |r: &mut XorShift, f: &Fixture| {
                let page = 1 + r.below(total / f.page_size - 1);
                page * f.page_size - (SMALL_READ as u64 / 2)
            },
        );
        let large = measure(
            &fixture,
            options.runs,
            options.large_reads,
            LARGE_READ,
            options.coalesce,
            |r: &mut XorShift, _| r.below(total / LARGE_READ as u64) * LARGE_READ as u64,
        );
        let encoded_per_page = fixture.footprint["pack_bytes"].as_u64().unwrap_or(0) as f64
            / fixture.footprint["page_count"].as_u64().unwrap_or(1) as f64;
        results.push(json!({
            "page_size": page_size,
            "footprint": fixture.footprint,
            "materialize_ns": fixture.materialize_ns,
            "aligned_4k_read": summarize(&aligned),
            "boundary_4k_read": summarize(&boundary),
            "sequential_1m_read": summarize(&large),
            "miss_amplification": {
                "decoded_bytes_per_4k_miss": page_size,
                "decoded_amplification": page_size as f64 / SMALL_READ as f64,
                "encoded_bytes_per_4k_miss_estimate": encoded_per_page,
                "note": "computed from geometry, not a measured origin transfer",
            },
        }));
        drop(fixture);
        let _ = std::fs::remove_dir_all(root.path().join(format!("import-{page_size}")));
        let _ = std::fs::remove_file(root.path().join(format!("arena-{page_size}.bin")));
    }
    let report = json!({
        "schema_version": 1,
        "tool": "page_geometry",
        "debug_build": cfg!(debug_assertions),
        "warm_buffered": !options.direct,
        "direct_unbuffered_arena": options.direct,
        "coalesced": options.coalesce,
        "batched_materializer": options.batched,
        "fixture_bytes": options.fixture_bytes,
        "runs": options.runs,
        "small_reads_per_run": options.small_reads,
        "large_reads_per_run": options.large_reads,
        "machine": std::env::var("COMPUTERNAME").unwrap_or_default(),
        "caveats": [
            "Synthetic random fixture on one machine; not a game workload.",
            "Read latencies include ResidentIndex lookup, lease acquisition, and arena I/O; no WinFsp or kernel path.",
            "Warm buffered reads measure the OS cache unless --direct is used.",
            "Changing page size must not reinterpret an existing arena; each geometry used a fresh fixture.",
        ],
        "results": results,
    });
    let text = serde_json::to_string_pretty(&report)?;
    match &options.out {
        Some(path) => std::fs::write(path, &text)?,
        None => println!("{text}"),
    }
    Ok(())
}
