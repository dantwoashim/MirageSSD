# ADR 0004: Strictly Bounded Sparse Fixed-Slot SSD Cache Arena

## Context

Managing hundreds of thousands of individual cached page files on NTFS causes significant filesystem overhead:
1. File handle creation, open/close churn, and MFT (Master File Table) growth degrade filesystem metadata performance.
2. NTFS cluster slack wastes disk space when storing loose chunk files.
3. Traditional dynamic caches that rely on asynchronous garbage collection frequently overshoot allocated disk quotas, risking disk-full errors that crash games or the operating system.
4. Attempting to build a custom in-memory RAM page cache duplicates the work already performed by the Windows Cache Manager / Standby List, wasting system RAM on gaming PCs.

## Decision

MirageSSD manages a single, strictly bounded sparse fixed-slot SSD cache arena:
1. Single Sparse Arena File: The local SSD cache is allocated as a single sparse file with uniform, fixed-size slots corresponding to logical page units (1 MiB).
2. Hard Physical Cache Envelope: The configured cache capacity is an absolute physical envelope. Committed bytes, reserved bytes, dirty bytes, staged bytes, journal bytes, spill space, and NTFS cluster allocation rounding all count toward this hard physical limit. Physical usage must never exceed the budget.
3. Windows Cache Manager as RAM Tier: MirageSSD delegates DRAM caching to the Windows Cache Manager via standard file mapping and read operations, rather than allocating a separate large user-space RAM cache.
4. Strict Non-Evictable Leases: Active session pins, dirty pins, and in-flight read leases are strictly non-evictable under all eviction pressures.
5. Bitmapped Fast Slot Indexing: Cache slot availability and page residency are tracked using lock-free / bitmapped memory-mapped structures, allowing O(1) slot resolution without SQLite queries or network calls in the hot read path.

## Rejected Alternatives

- Loose Individual File Cache (One file per page/chunk): Rejected. Flooding NTFS with 100,000+ files bloats the MFT, fragments disk storage, and incurs measurable file open/lookup overhead.
- Soft-Limit Cache with Background Pruning: Rejected. Bursts of high-speed downloads can easily exceed soft thresholds before background pruning runs, causing disk-exhaustion errors.
- Custom User-Space RAM Page Cache: Rejected. Double-caching data in user-space RAM starves the game and graphics drivers of system memory while ignoring OS-level virtual memory optimizations.

## Consequences

- Positive: Zero NTFS MFT bloat and zero file-handle thrashing; deterministic O(1) slot offset calculation (`slot_index * slot_size`).
- Positive: Guaranteed protection against exceeding the user's allocated SSD storage budget.
- Negative: Sparse file allocation requires NTFS sparse file support (`FSCTL_SET_SPARSE` and `FSCTL_SET_ZERO_DATA`).
- Negative: Requires precise accounting of journal, staging, and cluster rounding overhead to maintain the hard envelope guarantee.

## Validation Experiment

Execute an SSD stress test:
1. Allocate an 8 GiB sparse cache arena with a hard limit.
2. Run concurrent random writes and reads simulating 100 GB of total virtual asset access with rapid churn and concurrent pin reservations.
3. Measure physical disk space allocated (`GetCompressedFileSizeW`), verifying that physical bytes on disk never exceed 8 GiB under any workload condition.
4. Verify that pinned slots are never overwritten or invalidated during forced eviction passes.

## Revisit Trigger

Revisit this design if MirageSSD is ported to non-NTFS platforms lacking robust sparse-file primitives (e.g., specific FAT32/exFAT external storage) or if direct raw-partition block storage is implemented for dedicated gaming consoles.
