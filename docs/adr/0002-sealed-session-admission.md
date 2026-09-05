# ADR 0002: Sealed Session Admission and Pinning for Seamless Mode

## Context

Modern game engines (such as Unreal Engine, Unity, and proprietary open-world engines) load assets synchronously or on tight asynchronous deadlines during active gameplay. Cache misses during gameplay that trigger on-demand network downloads introduce unpredictable latency spikes (100 ms to several seconds), leading to severe frame stutter, missing audio, pop-in textures, or hard game freezes.

For a virtualized storage system to claim "Seamless Mode" gameplay, it cannot rely on aggregate hit-rate statistics (e.g., "99% hit rate"). A 99% hit rate still allows hundreds of missed reads during an intense combat sequence, which destroys the user experience and violates gaming performance expectations.

## Decision

MirageSSD enforces a strict admission and pinning gate for Seamless Mode:
1. 100% Capsule Residency Gate: Seamless Mode is admitted only after the mandatory hard set (engine core, shaders, common UI, initial world segment) and selected Sealed Session Capsule are 100% resident in the local SSD cache.
2. Integrity and Reservation: Every page in the capsule must be BLAKE3-verified and capacity-reserved in the sparse cache before admission.
3. Un-evictable Session Pins: All pages belonging to the active sealed session are marked with active session pins. Session pins, dirty pins, and active read leases cannot be evicted under any cache pressure while the session is live.
4. Strict Seal Violation Semantics: Any read request within an admitted sealed session that encounters an absent page is treated as an explicit seal violation bug in the capsule predictor/packager, not as a normal or acceptable cache miss.

## Rejected Alternatives

- Pure On-Demand Streaming During Gameplay: Rejected. High-latency network roundtrips and bandwidth jitter inevitably cause visible asset stalls and audio dropouts.
- Optimistic Session Admission (Admit at 90% and Stream Remainder): Rejected. Game asset loaders do not gracefully wait for background network hydration; missing assets cause frame drops or asset fallback errors.
- Dynamic Background Eviction of Active Capsule Pages: Rejected. Evicting pages from an active gameplay session under low disk pressure risks evicting frequently or unpredictably accessed game assets, violating deterministic zero-stutter guarantees.

## Consequences

- Positive: Zero-stutter read performance during active gameplay; read latency is bounded strictly by local SSD/NTFS performance.
- Positive: Clear, unambiguous operational semantics: missing pages in a sealed session are identified and tracked as seal violations.
- Negative: Requires an upfront prefetch and verification phase prior to entering Seamless Mode.
- Negative: Requires accurate capsule definition, offline profiling, and storage reservation accounting to ensure capsules fit within the user's allocated cache quota.

## Validation Experiment

Execute an automated 30-minute scripted gameplay replay harness (e.g., Genshin Impact world traversal and combat) in two modes:
1. Baseline: Admitted Seamless Session with 100% pinned capsule under an artificially severed network connection (100% packet loss).
2. Unsealed on-demand streaming over simulated 50 Mbps / 40 ms RTT network.

Metrics: Measure P99 and P99.9 frame times, I/O wait latency, and count of seal violations (must be exactly 0 in sealed mode).

## Revisit Trigger

Revisit this policy only if high-speed low-latency interconnects (e.g., local 10 GbE SAN or ultra-low-latency local edge nodes) consistently demonstrate sub-5 ms P99.9 page fault completion without impacting engine rendering pipelines.
