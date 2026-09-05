# ADR 0006: Single-Writer Architecture with Conflict Escalation for V1

## Context

Game modding, game updates, and multi-client cloud synchronizations introduce potential write concurrency. When multiple processes or machines attempt to modify game files, generate new asset versions, or update local index metadata simultaneously, distributed conflict resolution becomes necessary.

Attempting to implement complex multi-master distributed consensus (e.g., distributed lock managers, three-way merge algorithms, or Raft clusters) in v1 introduces massive architectural complexity, latency penalties, and high risk of subtle data corruption bugs during game execution.

## Decision

MirageSSD v1 implements a strict single-writer architecture with explicit conflict preservation and escalation:
1. Single Writer Coordination: On a single machine, only one coordinator process holds write leases for a repository, sparse cache arena, and SQLite metadata database. All other local processes are read-only clients.
2. Conflict Preservation: If remote synchronization detects conflicting valid branches or diverging commit histories (e.g., two independent machines authored commits on top of the same parent commit), MirageSSD v1 never performs automatic silent merges, and never applies silent "Last-Write-Wins" (LWW) overwrites.
3. Explicit Escalation: Both conflicting branches and manifest states are preserved in the DAG and escalated to the user/orchestrator with structured diagnostic metadata for explicit manual resolution or branching choice.
4. Base + Page-Level Copy-on-Write Updates: Local game updates operate via base generation plus page-level copy-on-write, durable idempotent journal transitions, remote staging, and clean atomic activation to exactly generation N or N+1.

## Rejected Alternatives

- Multi-Writer Optimistic Concurrency with Silent Last-Write-Wins (LWW): Rejected. Silently discarding conflicting modifications can result in permanent loss of user game saves, mod configurations, or custom asset patches.
- Automatic Three-Way File Merging in V1: Rejected. Binary game asset containers and complex proprietary formats cannot be reliably auto-merged by generic diff algorithms without game-specific domain compilers.
- Distributed Lock Manager (DLM) over Cloud Backends: Rejected. Cloud backends (such as Google Drive) lack reliable, low-latency distributed locking semantics needed for distributed lock management.

## Consequences

- Positive: Highly reliable and provably safe correctness model for v1; zero chance of silent data corruption or accidental file loss.
- Positive: Centralized state transitions in a single validated module make lifecycle states easy to reason about and test.
- Negative: Multiple concurrent local tools/processes must route mutation requests through the single coordinator process via IPC.
- Negative: Concurrent writes across different offline devices require explicit user intervention to select or branch the active lineage.

## Validation Experiment

Execute concurrency and conflict stress tests:
1. Simulate concurrent write attempts by two simulated processes against the same repository, verifying that the lock coordinator serializes writes or rejects unauthorized concurrent writers.
2. Simulate a split-brain branch conflict where two distinct commits share the same parent, verifying that both commits are stored, neither is deleted or silently overwritten, and a structured conflict escalation report is emitted.

## Revisit Trigger

Revisit if post-v1 product requirements demand real-time multi-device seamless synchronization or collaborative live modding with automated domain-specific asset merge drivers.
