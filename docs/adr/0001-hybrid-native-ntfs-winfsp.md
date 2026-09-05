# ADR 0001: Hybrid Native NTFS and WinFsp Virtualization Boundary

## Context

PC games consist of heterogeneous files with vastly different access patterns, update frequencies, and security constraints:
1. Executables (`.exe`), dynamic link libraries (`.dll`), launcher processes, and anti-cheat drivers/services require native Windows execution semantics, high-frequency low-latency metadata operations, and memory-mapping compatibility. Anti-cheat systems (e.g., HoYoKProtect, Easy Anti-Cheat, BattlEye) actively monitor process binaries and kernel hooks; intercepting or virtualizing these files introduces high risks of false positives, driver initialization failures, or anti-cheat bans.
2. Asset archives, package files (`.pak`, `.pck`, `.bundle`), audio banks, and video containers are large, predominantly immutable, and read-heavy. These files account for 90–95% of total game installation footprint and are ideal candidates for sparse on-demand caching and virtualization.

Virtualizing the entire game root via Windows Cloud Files API (CFAPI) or full filesystem virtualization introduces major drawbacks: CFAPI requires full placeholder hydration or complex kernel-mode filter interactions that fail to provide efficient sub-file random reads, while full user-mode filesystem interception of binaries introduces anti-cheat friction and latency penalties on OS loader operations.

## Decision

MirageSSD adopts a hybrid filesystem boundary model:
1. Native NTFS for execution and mutable state: All executables, DLLs, game launchers, anti-cheat drivers/services, configuration files, save games, and frequently mutated logs remain stored directly on native NTFS.
2. WinFsp user-mode virtualization for immutable assets: Virtualization is strictly confined to selected, immutable asset subtrees and container files. WinFsp user-mode filesystem handlers serve virtualized asset directories, intercepting random-read I/O requests and mapping them to the local sparse cache.
3. CFAPI is explicitly not the production default for large random-read asset containers.

Under no circumstances will MirageSSD inject code into game processes, bypass anti-cheat mechanisms, modify process memory, deploy hidden kernel drivers, or falsify security/integrity results.

## Rejected Alternatives

- Full Game Directory Virtualization via Windows Cloud Files API (CFAPI): Rejected. CFAPI is designed for file-level sync/hydration (e.g., OneDrive) rather than high-performance, sub-file byte-range random access into multi-gigabyte asset containers. Hydrating 100 GB pak files before reading a 1 MiB texture introduces unacceptable latency.
- Full Filesystem Virtualization for All Game Files via WinFsp: Rejected. Intercepting binary execution and anti-cheat driver loading through user-mode filesystem callbacks creates anti-cheat trip risks, adds context-switch overhead to code paging, and complicates OS loader interactions.
- Kernel-Mode Mini-Filter Driver: Rejected. Writing a custom kernel driver requires EV code signing, kernel attestation, extensive WHQL certification, and carries severe stability risks (BSODs) and anti-cheat conflicts.

## Consequences

- Positive: Zero anti-cheat compatibility risk on executables and drivers; native loader performance for binaries; focused attack surface on read-only asset subtrees where WinFsp excels.
- Positive: Clear boundary for isolation, making debugging and differential testing straightforward.
- Negative: Requires cataloging and splitting game directory structures during installation/conversion to distinguish native NTFS paths from virtualized asset paths.
- Negative: Requires path redirection, NTFS directory junctions, or game-specific asset directory mount points.

## Validation Experiment

Execute automated launch and gameplay test suites across target game titles (e.g., Genshin Impact, Unreal Engine 5 benchmarks) comparing:
1. Pure native NTFS baseline.
2. Hybrid mount (native binaries + WinFsp virtualized asset subtrees).
3. Full directory WinFsp virtualization.

Metrics to record: Anti-cheat driver initialization success rate (must be 100%), binary load latency, asset open latency (P95/P99 < 1 ms on local hit), and memory consumption.

## Revisit Trigger

Revisit this decision if Microsoft introduces an official, anti-cheat-certified user-mode filesystem projection API with native support for sub-file range sparse hydration without file-level blocking.
