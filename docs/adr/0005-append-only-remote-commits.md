# ADR 0005: Append-Only Immutable Remote Commits and Manifests

## Context

Cloud storage backends (Google Drive, Amazon S3, Azure Blob Storage, Cloudflare R2) operate as distributed object stores with eventual consistency, differing rate limits, and lack of POSIX transactional filesystem locking.

If a repository design relies on mutable in-place metadata updates (e.g., updating a `manifest.json` file in-place), partial network failures, race conditions between multiple writers, or API rate limit timeouts can leave the remote repository in an inconsistent, unrecoverable state. Clients reading partially overwritten manifests would encounter corrupted chunk pointers or missing data blocks.

## Decision

MirageSSD enforces strictly immutable and append-only semantics for all remote repository structures:
1. Immutable Content Objects: All remote data packs (`.mpak`), index structures (`.midx`), and commit manifests (`.mcom`) are immutable and content-addressed via cryptographic BLAKE3 hashes. Once written, a remote object is never modified in place.
2. Cryptographic Commit Tree: History is structured as an append-only directed acyclic graph (DAG) of immutable commit objects, similar to Git. Each commit references immutable packs and manifests.
3. `LATEST` is Only a Hint: A mutable branch pointer or `LATEST` tag is provided purely as an operational hint for fast synchronization discovery. `LATEST` is never treated as authoritative recovery state. Recovery authority rests solely on the immutable commit DAG and verified cryptographic hashes.
4. Quarantine Interrupted Writes: Interrupted or partial remote uploads are ignored and safely isolated until garbage-collected; they never compromise existing committed generations.

## Rejected Alternatives

- Mutable In-Place Manifest Overwrites: Rejected. Any network disconnection or timeout during in-place overwrite can corrupt the active manifest and render the entire remote dataset inaccessible.
- Remote Distributed Lock Servers: Rejected. Requiring a dedicated coordination server (e.g., ZooKeeper, Redis lock) violates the requirement to support serverless, zero-cost cloud backends like Google Drive or standard S3 buckets.
- Destructive In-Place Delta Patching of Remote Packs: Rejected. Patching existing pack files in place destroys historical immutability, breaks active reader leases, and prevents rollback to earlier versions.

## Consequences

- Positive: Total crash consistency and partition tolerance on cloud backends; seamless support for parallel reads; zero possibility of corrupted historical commits.
- Positive: Safe CDN caching and local caching since objects are immutable and cacheable forever by hash.
- Negative: Remote storage consumption grows over time as new generations are published.
- Negative: Requires an explicit, coordinated remote garbage collection process to prune unreferenced historical generations and orphan packs.

## Validation Experiment

Execute a remote publishing fault-injection test against Google Drive and S3 mock backends:
1. Simulate abrupt client termination and network failure at various percentages of pack and manifest upload.
2. Verify that existing commits remain 100% valid and readable by other clients.
3. Verify that the client resumes or publishes a new immutable commit without data corruption or dangling references.

## Revisit Trigger

Revisit only if a cloud backend enforces strict write-rate limits or object count quotas that fundamentally prevent append-only DAG growth and mandate in-place mutation with backend-native atomic multi-file transactions.
