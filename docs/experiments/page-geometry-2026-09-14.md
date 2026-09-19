# Page-geometry measurement on a synthetic fixture (2026-09-14)

Status: measured on one machine with a synthetic random fixture. Not a game
workload, not a WinFsp or kernel-path measurement, and not a format decision.

Harness: `tools/bench-runner/src/bin/page_geometry.rs`, release build.
Raw results: `page-geometry-2026-09-14.json` (warm, buffered arena handle) and
`page-geometry-direct-2026-09-14.json` (`FILE_FLAG_NO_BUFFERING` arena handle).
Fixture: 256 MiB of xorshift bytes, imported once per page size into fresh
packs, a fresh arena, and a fresh control database; 5 runs; 20,000 aligned and
20,000 boundary-crossing 4 KiB reads and 200 sequential 1 MiB reads per run.
Latencies cover `ResidentIndex::acquire`, lease acquisition, and the arena
`pread`; percentiles are nearest-rank, and the p99 column is the median of the
five per-run p99 values (pooled p99 is in the JSON).

## Footprint

| Page | Pack bytes over source | Per page | Mount index bytes | Per page | Arena metadata | Control DB |
|---:|---:|---:|---:|---:|---:|---:|
| 64 KiB | 18,080,782 (6.7%) | 4,414 | 623,872 | 152.3 | 266,304 | 598,016 |
| 256 KiB | 4,520,974 (1.7%) | 4,415 | 156,928 | 153.3 | 69,696 | 253,952 |
| 1 MiB | 1,131,021 (0.4%) | 4,418 | 40,192 | 157.0 | 20,544 | 4,096 |

Pack overhead is approximately 4.4 KiB per page regardless of page size, so the
relative cost is dominated by page count. This is a property of the current
pack-v1 frame layout on this fixture; it is the concrete number that a smaller
verification unit would have to beat (proposal B11, section 2.4).

## Preparation

| Page | Import (ns) | Insert into arena (ns) | Total (s) |
|---:|---:|---:|---:|
| 64 KiB | 1,980,487,600 | 18,502,051,800 | 20.5 |
| 256 KiB | 1,687,676,800 | 5,970,498,200 | 7.7 |
| 1 MiB | 1,602,784,100 | 2,329,759,100 | 3.9 |

Insertion uses one `insert_page` per page, so the 64 KiB geometry pays about
4.5 ms per page in database and metadata work. The batched materializer
(`insert_reserved_pages`) is not exercised here; this is an upper bound on
per-page overhead, not the product's preparation time.

## Resident read latency (ns)

Buffered handle:

| Page | Aligned 4 KiB p50 / mean / p99 | Boundary 4 KiB p50 / mean / p99 | Sequential 1 MiB p50 / mean / p99 |
|---:|---|---|---|
| 64 KiB | 114,900 / 71,437 / 179,600 | 11,500 / 15,767 / 35,700 | 413,800 / 993,987 / 3,919,000 |
| 256 KiB | 113,900 / 69,368 / 175,000 | 10,800 / 13,346 / 34,100 | 357,700 / 663,604 / 1,961,700 |
| 1 MiB | 113,400 / 69,736 / 175,300 | 12,400 / 12,410 / 19,800 | 343,000 / 569,345 / 1,371,700 |

Unbuffered handle:

| Page | Aligned 4 KiB p50 / mean / p99 | Boundary 4 KiB p50 / mean / p99 | Sequential 1 MiB p50 / mean / p99 |
|---:|---|---|---|
| 64 KiB | 107,600 / 110,960 / 181,700 | 215,700 / 220,354 / 338,200 | 3,898,200 / 3,909,169 / 4,205,700 |
| 256 KiB | 105,100 / 110,637 / 175,400 | 210,300 / 217,750 / 356,100 | 1,383,000 / 1,409,926 / 1,972,700 |
| 1 MiB | 103,700 / 108,616 / 180,400 | 206,500 / 213,965 / 348,000 | 790,800 / 800,611 / 889,700 |

Observations, limited to this fixture:

- Random aligned 4 KiB reads across 256 MiB cost about 105-115 us at the median
  on both handles, independent of page size. The buffered mean below its median
  shows a bimodal distribution: a fast cached mode and a device-latency mode.
  The buffered handle did not keep the whole arena cached during the run.
- Boundary-crossing reads issue two arena reads. They look fast on the buffered
  handle only because the fixture has few boundary locations, which stay hot;
  the unbuffered handle shows the honest cost, roughly twice an aligned read.
- Sequential 1 MiB reads scale with the number of arena reads per request:
  16 at 64 KiB, 4 at 256 KiB, 1 at 1 MiB. On the unbuffered handle the median
  drops from 3.9 ms to 0.79 ms across the three geometries. This table shows
  the uncoalesced cost; the coalesced path is measured below.

## Coalesced adjacent-slot reads (unbuffered handle, `--coalesce`)

Raw results: `page-geometry-direct-coalesced-2026-09-14.json`, and the
attributed re-run `page-geometry-direct-coalesced-attributed-2026-09-14.json`
(adds fallback counts, lease/I/O time split, and arena extent counts). Same
fixture and run parameters. The harness acquires every lease for the request first and, when
the slots are consecutive, issues one arena read through
`mirage_cache::read_contiguous` (proposal B09) while holding all the leases;
otherwise it falls back to the per-page loop. Product code uses the same
function in the engine multi-span path and in the WinFsp host resident path.

| Page | Boundary 4 KiB p50 / mean / p99 | Sequential 1 MiB p50 / mean / p99 | Sequential 1 MiB uncoalesced p50 |
|---:|---|---|---:|
| 64 KiB | 125,600 / 136,453 / 257,100 | 1,328,600 / 1,331,454 / 1,620,700 | 3,898,200 |
| 256 KiB | 115,100 / 123,960 / 229,000 | 805,300 / 808,080 / 883,200 | 1,383,000 |
| 1 MiB | 111,700 / 121,701 / 237,600 | 814,600 / 939,908 / 949,600 | 790,800 |

Observations, limited to this fixture:

- Boundary-crossing 4 KiB reads now cost one arena read at every geometry:
  the median falls from about 210 us to about 112-126 us, in line with an
  aligned read.
- Sequential 1 MiB reads at 256 KiB pages now match the 1 MiB geometry
  (0.81 ms vs 0.79 ms uncoalesced 1 MiB). At 64 KiB the median drops from
  3.90 ms to 1.33 ms but does not reach 0.8 ms. An attributed re-run
  (`page-geometry-direct-coalesced-attributed-2026-09-14.json`, same
  parameters, counters added to the harness) rules out two explanations:
  every one of the 1,000 sequential requests and 100,000 boundary requests
  took the coalesced path (zero fallbacks), and acquiring the 16 leases cost
  11.8 us per request, 0.9% of the 1.32 ms. The residual is inside the single
  arena `pread` (1.27 ms at 64 KiB slots vs 0.85 ms at 1 MiB slots for the
  same 1 MiB window). The cause is physical layout, confirmed by counting the
  arena's allocated NTFS extents (`FSCTL_GET_RETRIEVAL_POINTERS`, now
  reported as `arena_physical_extents`): 3,729 extents for 4,096 pages at
  64 KiB, 1,027 for 1,024 at 256 KiB, 265 for 256 at 1 MiB. The sparse
  fixed-slot arena is allocated about one extent per `insert_page` call, so a
  coalesced 1 MiB logical read at 64 KiB slots still spans roughly 15
  physical extents. The fix is contiguous preallocation of the run during
  ordered materialization (the same locality work that took the earlier
  capsule arena from 1,055 extents to 95), not a page-size change. Numbers
  in this attributed run differ from the previous coalesced run by 2-6% at
  the median, which is the same run-to-run noise noted below.

## Contiguous-run writes in the batched materializer (`--batched`)

Raw results: `page-geometry-direct-coalesced-batched-2026-09-14.json`. Same
fixture, unbuffered handle, coalesced reads. The fixture is now materialized
through the product's batched path (`reserve_cache_slots_batch` +
`insert_reserved_pages`, up to 64 pages and 32 MiB per batch) instead of one
`insert_page` per page, and `insert_reserved_pages` now writes each maximal
run of consecutive slots with one `write_slots_contiguous` call (proposal B13,
"contiguous-run write coalescing"). A run extends only while the previous
page fills its slot, so every slot's bytes land exactly where a per-slot write
would put them; the crash-point tests cover the batch at every step.

| Page | Arena extents, per-page insert → batched | Insert time (s) | Sequential 1 MiB p50 / mean / p99 | Boundary 4 KiB p50 / p99 |
|---:|---:|---:|---|---|
| 64 KiB | 3,729 → 265 | 22.4 → 4.1 | 833,000 / 845,576 / 994,500 | 122,100 / 262,600 |
| 256 KiB | 1,027 → 265 | 6.7 → 1.8 | 809,300 / 821,502 / 954,200 | 119,400 / 269,800 |
| 1 MiB | 265 → 635 | 2.5 → 3.5 | 867,800 / 870,348 / 1,066,300 | 129,500 / 264,100 |

Observations, limited to this fixture:

- Large-read cost is now independent of page size: a coalesced sequential
  1 MiB read costs 0.81-0.87 ms at the median at every geometry (it was
  3.90 ms at 64 KiB before coalescing and 1.29 ms after coalescing alone).
  The 64 KiB geometry pays 265 extents for 4,096 pages, one per 64-page batch.
- The 1 MiB geometry got worse in this run, not better: 635 extents and 3.5 s
  of insert time against 265 and 2.5 s with per-page inserts. Its batches are
  32 MiB single writes, and the volume's free space at the time did not offer
  32 MiB contiguous runs, so NTFS split them. That is a property of the test
  volume, not of the format; it also shows that a larger write is not
  automatically a more contiguous one. A materializer that wants contiguity
  on a fragmented volume has to ask for it (preallocation or smaller runs
  matched to the free-space map), which is not implemented.
- Materialization at 64 KiB is 5.5x faster batched, because the database and
  metadata work is amortized over the batch. This is the product's actual
  preparation path; the per-page numbers in the "Preparation" table above are
  the upper bound they were described as.
- Zero fallbacks and zero errors across all three geometries.
- The 1 MiB geometry's run 4 contains one 14.9 ms sample (pooled p99
  2,295,600 ns against a median-of-runs p99 of 949,600 ns). A single stall of
  this size is why the p99 column reports the median of per-run p99 values and
  why one run is not a certification.
- Aligned 4 KiB reads are unchanged in mechanism (one read either way); their
  p99 in this run (222-243 us) is higher than in the earlier direct run
  (175-182 us), which bounds the run-to-run noise on this machine at roughly
  25% at the tail.

## Miss amplification (computed, not measured)

A 4 KiB miss decodes one page: 16x, 64x, or 256x the requested bytes, with the
encoded transfer about 4.4 KiB larger than the page. No origin transfer was
measured here.

## What this does and does not decide

It confirms the proposal's expectation that smaller pages lower miss
amplification and raise per-page index, pack, and preparation costs, and it
quantifies those costs on this machine. It does not select a page size for a
title: that needs the file-size and access-concentration census (section 5,
step 5) and a real-title measurement of the coalesced path, whose benefit here
depends on ordered materialization producing adjacent slots and on the volume
having contiguous free space. With coalesced reads and contiguous-run writes,
page size no longer determines large-read latency on this fixture, which
leaves miss amplification, per-page metadata, and preparation time as the
quantities a page-size decision trades off. Changing page size must use a fresh
arena; no existing arena was reinterpreted.
