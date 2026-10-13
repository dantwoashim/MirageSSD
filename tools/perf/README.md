# Mounted-drive benchmarks

Two workloads measure a mounted MirageSSD volume against native NTFS on the
same host:

- **Bench.cs** — large file: 128 MiB sequential write + flush, buffered and
  unbuffered sequential reads with SHA-256 verification, and 1,000 verified
  random 4 KiB reads.
- **SmallFiles.cs** — 2,000 small files (4 KiB by default): create, read back
  with verification, and metadata overhead.

## Run

Against a temporary managed volume mounted through the real WinFsp host
(requires the release adapter and WinFsp installed):

```powershell
$env:MIRAGE_FS_EXE = 'D:\MirageSSD-Publish\build\windows-msvc-release\native\winfsp-adapter\Release\mirage-fs.exe'
cargo test --release --locked -p mirage-ffi --test bench_mounted -- --ignored --nocapture
```

Or against any already-mounted root (e.g. `R:\`), pairing it with a native
NTFS reference over alternating rounds:

```powershell
.\tools\perf\run-managed-bench.ps1 -Target R:\
```

Each run writes `target\perf\<timestamp>\summary.json` (medians with min/max
per phase) and prints `results: <dir>`.

A single small-file phase (no rounds) runs via `run-smallfiles-once.ps1` by
pointing `MIRAGE_BENCH_SCRIPT` at it:

```powershell
$env:MIRAGE_BENCH_SCRIPT = 'D:\MirageSSD-Publish\tools\perf\run-smallfiles-once.ps1'
cargo test --release --locked -p mirage-ffi --test bench_mounted -- --ignored --nocapture
```

## Tracing

Set `MIRAGE_FS_TRACE=<file>` before starting `mirage-fs.exe`: the adapter
writes a TSV of per-callback and per-FFI-call counts and timings to `<file>`,
and the engine writes its own inner breakdown to `<file>.ffi.tsv`. Both are
rewritten every 2 seconds (survives a host kill) and carry
`kind / name / count / total_us / mean_us / max_us` columns, sorted by total.

## Probes

Ignored-by-default microbenchmarks — run explicitly:

```powershell
cargo test --release --locked -p mirage-ffi --test small_file_probe -- --ignored --nocapture
cargo test --release --locked -p mirage-ffi --test publish_drain_probe -- --ignored --nocapture
cargo test --release --locked -p mirage-db --test commit_floor_probe -- --ignored --nocapture
```

- `small_file_probe` — per-file lookup/write/close and read-phase cost
  through the FFI without a mount (`PROBE_FILES`/`PROBE_BYTES` optional).
- `publish_drain_probe` — remote objects created and publish drain time for
  N small files against the local directory backend (`PROBE_FILES`/
  `PROBE_BYTES`).
- `commit_floor_probe` — raw durable-commit and statement-cache floor costs
  for the control-plane database.

## Caveats

Single host; Windows Defender real-time scanning affects small-file numbers;
results are indicative, not a qualification.

## Cache and latency research (2026-10-12)

The [research report](../../docs/experiments/2026-10-12-cache-and-latency-research.md)
records measured mechanisms, synthetic policy results, prior art, and falsifiable
experiments. Captured results and hashes are in
[`cache-research-2026-10-12.json`](../../docs/experiments/cache-research-2026-10-12.json).

- `range_schedule_probe.py --rustc <installed-rustc.exe>` extracts the actual
  range scheduler and compares it with a balanced partition using a mock
  backend. It verifies exact bytes and the serialized 68-byte tail request.
- `frame_hit_probe.rs` is a standalone optimized Rust benchmark that independently
  varies thread creation and full-frame copying for a warm 4 KiB read.
- `cache_research.py` compares clear-all, LRU, MRU, SIEVE, a recurrence predictor,
  and a future-aware oracle on deterministic synthetic traces.
- `cache_compression_probe.py` measures independent zlib-1 blocks on bounded
  local source/executable samples and an incompressible control.

Python harnesses use the standard library. Outputs default to
`target/perf/research-20261012/`. Run timing probes serially; full commands and
limitations are in the report. These experiments do not measure mounted-product
speedups, and synthetic cache miss rates are not real-workload savings.

### Follow-up: verified seeking without rewriting payloads

[`seek-proof`](seek-proof/README.md) imports Mirage's actual encrypted-frame
encoder and decoder, then demonstrates small authenticated reads from unchanged
ciphertext using the existing BLAKE3 plaintext root. It includes exact-byte and
corruption checks, selective proof refinement, CPU measurements, and a controlled
HTTP range experiment with shared bandwidth limits. The
[follow-up report](../../docs/experiments/2026-10-12-verified-encrypted-seek.md)
corrects the earlier assumption that smaller authenticated transfers necessarily
require a new payload format. Build and run recipes are in its README; run its
timings serially with other workloads stopped.

### Managed reader integration (2026-10-13)

The [integrated upgrade](../../docs/experiments/2026-10-13-verified-sector-upgrade.md)
implements learned 64 KiB verified-sector reads from unchanged legacy frames.
`cargo test --locked -p mirage-ffi --test cold_read -- --nocapture` exercises
real publication fixtures, packed offsets, small later misses, shared flights,
bounded concurrent owners, corruption/retry, and sparse restage admission.
`cargo test --locked -p mirage-pack --lib` exercises the crypto/tree geometry.
These tests measure exact bytes and invariants, not cloud or gameplay speedups.
Rebuild the debug adapter before running the full mounted-volume suite.

## Native Sophon source

`cargo test --locked -p mirage-ffi --lib sophon -- --nocapture` includes verified
seed ranges, shared cold misses, corrupt decode/refetch, disk eviction and
restart, FFI lookup/read, and a real Windows read-only mount/remount. Build the
debug adapter first. The source contract is
[sophon-source-v1.md](../../docs/specs/sophon-source-v1.md).

`probe-sophon-mount.py --mount <path> --manifest <export.json> --asset <name>
--report <target/perf/result.json>` verifies selected whole assets. Repeat
`--manifest` and `--asset` as needed. It reports application reads plus MD5;
alternate candidate order and distinguish seed, memory, owned disk and CDN
reads. Existing frozen seed storage is separate from the new cache budget.
