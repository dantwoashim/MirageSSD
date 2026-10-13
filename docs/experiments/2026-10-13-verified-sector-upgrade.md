# MirageSSD verified-sector production integration

The October 12 verified-seek prototype is now integrated into the managed
encrypted-payload source path. This report distinguishes implementation,
validation, and installation; it does not claim a universal gameplay speedup.

## Resulting behavior

- The first unindexed frame read uses complete existing AEAD and plaintext-hash
  verification. It learns a compact catalog of 64 KiB subtree chaining values.
- Knowledge is retained independently of data. A later sparse data miss can
  fetch and authenticate one 64 KiB sector of unchanged legacy ciphertext.
- Full frames and concurrent flight results use shared immutable buffers.
  Cache hits no longer clone a whole frame under the shared cache lock.
- Byte-bounded LRU caches evict individual old entries. Defaults are 32 MiB of
  full-frame data, 8 MiB of sector data, and 8 MiB of proof allocations, compared
  with the former 128 MiB full-frame data ceiling. This is a 62.5% reduction in
  those configured residency budgets, not a measured RSS reduction. Bookkeeping,
  publication records, caller output, and active miss buffers remain additional.
- Whole-frame fetching uses at most four balanced ranges. The 68-byte frame
  envelope is included, avoiding the former fifth serialized request.
- A global eight-owner miss gate applies across readers; background restaging
  can occupy at most two owners, leaving capacity for unrelated demand.
- Isolated sparse reads no longer restage whole payloads and neighbors.
  Dense/sequential demand can still trigger the existing bounded lookahead.
- Restaging reserves its exact known payload size before downloading, and
  releases the reservation on failure or cancellation.

The trust boundary and limits are specified in
[engine-api.md](../specs/engine-api.md). No published ciphertext, original data,
database schema, OAuth configuration, or native game files are migrated.

## Evidence

The first focused suite passed eight FFI unit tests, eight cold-read integration
tests, and two verified-sector crypto/geometry tests. It recorded:

| Actual integrated path | Observed backend request bytes |
|---|---:|
| Bootstrap for a 4 MiB legacy frame | 4,194,372 in four requests |
| Later sparse miss with learned catalog | 65,536 in one request |
| Sixteen overlapping readers of another sector | One shared request |

The final bootstrap fixture retained 131,072 sector bytes and a 2,184-byte catalog
after the shared-read test, with whole-frame residency deliberately disabled
to exercise data eviction. The later-miss byte reduction is approximately 64x;
the first bootstrap still pays the complete legacy transfer. This is not a
64x launch or frame-rate improvement.

Tests cover exact sector bytes across non-power-of-two frame lengths and short
final sectors; ciphertext, catalog, nonce, key, root, identity, length, and
alignment corruption; rejection of a damaged whole-frame tag during restaging;
clean retry after a failed sector; sparse restage abstention; shared-flight
deduplication; and existing dense-read/restage behavior. Additional integrated
coverage exercises cross-frame sectors and the global concurrency limit.

Raw validation logs are under `target/perf/upgrade-20261013/`. The debug native
adapter must be rebuilt before mounted-volume tests because its linked Rust FFI
implementation changed. Final workspace and installation results are recorded
below after execution.

## Practical limits

Catalogs in this implementation are ephemeral. A remount or catalog eviction
requires bootstrap again. Persistent catalog recovery, publication-time remote
proofs, adaptive fine 4 KiB proofs, an independently budgeted durable clean-sector
arena, and real-title qualification remain future work. Full scans still read
all content; this cannot skip Genshin's official integrity verification.

Current Genshin streaming mounts use the separate Sophon runtime and do not run
this encrypted Mirage reader. The existing Genshin verification settings remain
separate. This upgrade improves Mirage-managed published payloads.

## Final qualification and installed result

The final source passed `cargo fmt --all -- --check`, workspace Clippy with
warnings denied, and `cargo test --workspace --locked`. The latter included
the rebuilt debug adapter's real mounted-volume tests, all ten cold-read tests,
and both verified-sector tests. Live-cloud probes and extended endurance gates
remained ignored. UI validation passed 60 tests and emitted exactly the three
required assets. Optimized CLI, service, UI host, and native filesystem host
built successfully. The generated MSI passed the compiled upgrade-contract
check. An existing read-probe precedence warning was corrected by adding
parentheses without changing its behavior; other pre-existing work was retained.

The tested development build was installed into the existing Windows product,
retaining version 0.1.19. All four installed executable SHA-256 hashes matched
the candidate. The service and filesystem host started new processes, G:
remounted, and every configured volume retained its previous commit identity,
publication object/byte totals, and mounted/unmounted state.

The first same-version install attempt returned 1638 before changing binaries;
the old drive was restored. Correcting the repair mode to `vamus` recached the
new package and forced replacement of the tested files, using Microsoft's
[documented reinstall options](https://learn.microsoft.com/en-us/windows/win32/msi/reinstallmode).
The successful attempt returned 3010: Windows requests a reboot to finish
deferred installer cleanup of files held in use. No reboot was forced.

Six selected 4 KiB reads from three real archived video files on the installed
G: drive matched the independently chunk-verified official Sophon source.
First reads took 3.60–3.89 seconds; subsequent offsets in the cached frames
took 0.78–0.88 milliseconds. These are smoke observations, not an A/B benchmark
or evidence that cold first-read latency has been eliminated. The earlier block
probe found no samples because this archive contains audio/videos rather than
the separately streamed block tree; it was not counted as a passed read probe.

The local candidate, executable backup, installer logs, and independent
installation record are under `target/upgrades/verified-sector-20261013/`.
The installation helper handles successful reboot-required completion and uses
the supported unmount/remount API. A durable, non-private evidence summary is
in `verified-sector-upgrade-2026-10-13.json` beside this report.
