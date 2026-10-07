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
