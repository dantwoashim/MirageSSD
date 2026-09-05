# ADR 0003: Separation of Page Storage Identity and Adaptive Network Fetch Windows

## Context

Storage systems and network transport protocols have conflicting optimal granularities:
- Local storage deduplication, bitmap indexing, integrity verification, and cache slot management operate most efficiently on a deterministic, uniform logical block size (e.g., 1 MiB pages). Fixed page sizes eliminate memory fragmentation, simplify sparse file slot calculation, and allow fine-grained content-addressable BLAKE3 hashing.
- Network transport over HTTP/2, HTTP/3, and cloud object stores (e.g., Google Drive, S3) suffers severe protocol overhead if transferred as individual 1 MiB requests. High bandwidth-delay product (BDP) connections require coalesced, multi-megabyte range requests (e.g., 8 MiB to 32 MiB) to achieve link saturation.

Coupling the physical storage slot size directly to the network fetch request size compromises both subsystems: large storage blocks lead to cache bloat and poor deduplication, while small network requests underutilize available bandwidth.

## Decision

MirageSSD strictly decouples storage page identity/residency from adaptive network fetch windows and prediction clusters:
1. Storage Page Identity: The fundamental unit of local storage indexing, sparse cache allocation, and cryptographic verification is a fixed 1 MiB logical page (with configurable benchmarks). Each page has a unique deterministic hash and residency state in the local index (`.midx`).
2. Adaptive Network Fetch Windows: The network download scheduler dynamically coalesces contiguous or predicted missing pages into adaptive multi-page fetch windows (e.g., 4 MiB to 32 MiB) tailored to current connection latency, jitter, and available bandwidth.
3. Verification Pipeline: Network ranges received from the transport layer are sliced and verified individually as 1 MiB logical pages through a staged decode/verification pipeline (range/status -> encoded length -> decode/decrypt -> logical length -> BLAKE3) before being admitted to the sparse cache.

## Rejected Alternatives

- Variable-Sized Storage Blocks Matching Fetch Windows: Rejected. Dynamic block allocation in the local cache leads to fragmentation, complex free-list management, and defeats cross-version block deduplication.
- Fixed 1 MiB Network Range Requests: Rejected. Issuing separate HTTP requests for every 1 MiB page results in excessive header overhead, connection serialization, and poor bandwidth utilization on high-speed consumer internet connections.
- Tying Prediction Clusters Directly to Disk Layout: Rejected. Predictive access clusters shift based on player behavior and game patches, whereas disk storage layouts must remain deterministic, immutable, and index-stable.

## Consequences

- Positive: Storage engine remains simple, robust, and fragment-free with O(1) slot addressing; network scheduler can adaptively optimize throughput without changing storage structures.
- Positive: Failed or corrupted network ranges can be partially rescued at page granularity if valid sub-ranges pass individual BLAKE3 verification.
- Negative: Requires an in-memory staging and assembly pipeline to slice multi-page wire responses into verified individual cache pages.

## Validation Experiment

Run network ingest benchmarks across simulated bandwidths (10 Mbps, 50 Mbps, 250 Mbps, 1 Gbps) and simulated latencies (5 ms, 40 ms, 120 ms RTT):
1. Measure throughput with 1 MiB fixed network chunks vs adaptive coalesced windows (4 MiB - 32 MiB).
2. Measure CPU overhead of page slicing, decompression, and BLAKE3 verification (target: < 5% CPU utilization at 1 Gbps wire speed).

## Revisit Trigger

Revisit if future storage hardware (e.g., next-generation NVMe DirectStorage extensions) standardizes on hardware-accelerated variable block sizes with zero-copy network-to-GPU ingress.
