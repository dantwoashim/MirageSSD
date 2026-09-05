# Deterministic test corpora

`mirage-corpus-gen` defines synthetic data as a versioned profile, seed, file plan, and random-access byte oracle. The descriptor's `oracle_spec_hash` commits to those facts without reading or allocating the logical payload, so 100 GiB and larger cases remain cheap to schedule and identical across machines.

Profiles:

- `large-container`: one 100 GiB logical container.
- `many-files`: 100,000 deterministically named files with rank-inverse (Zipf s=1) sizes.
- `dedupe-versions`: two 8 GiB generations where exactly one page in each four-page group changes.
- `random-read`: a 16 GiB random-access byte oracle.
- `mmap`: 4 GiB of page-recognizable, alignment-friendly data.

Generate an on-demand descriptor:

```powershell
cargo run -p mirage-corpus-gen -- --profile large-container --seed 42 --output corpus
```

`--materialize` writes actual bytes in bounded 1 MiB buffers and refuses totals above `--max-materialized-bytes` (1 GiB by default). Use `--logical-bytes` to scale a profile for a physical test; raising the safety bound is always explicit. Oracle reads use `fill_at(seed, pattern, offset, buffer)` and are independent of request boundaries.
