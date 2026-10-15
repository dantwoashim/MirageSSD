# Audit evidence and repair matrix

Baseline: `14d04c3dab997eb47c314a2960a1497b6f3faba9`. Reports are static review inputs, not independent verification of every claim. MS denotes the twenty-item audit; P denotes the twenty-eight-item pasted report.

Status distinguishes confirmed, partly true, stale/not a defect, and needs reproduction. Fix and regression evidence are recorded separately. Existing preview artifacts and live installations are outside this repair batch.

| ID | Claim | Classification / source evidence | Fix / regression evidence |
|---|---|---|---|
| MS-01 | Sticky sealing failures | Confirmed: segments.rs consumed failure and dropped jobs. | Ordered retained-job retry implemented; selective older-only SQLite failure plus reopened bytes pass. |
| MS-02 | Delete ordering | Confirmed: mutation.rs discarded before logical commit. | Transactional handle state and durable tombstone; rejected unlink retains data/openability regression passes. |
| MS-03 | Rename replacement policy | Confirmed: native rename ignored ReplaceIfExists. | Replacement flag and atomic closed-victim guard implemented; collision/source preservation FFI regression passes. Open-destination replacement remains conservatively rejected. |
| MS-04 | Null zero-length write | Confirmed: null/zero slice precondition violation. | Early zero-length return; FFI regression passes. |
| MS-05 | Whole-volume flush | Confirmed: native Flush rejected null context; WinFsp documents volume flush. | Checked all-volume drain/barrier with FFI success/failure regressions. Native callback invocation requires privileged device access; explicit elevated test remains ignored here. |
| MS-06 | Timestamp failure propagation | Confirmed: native SetBasicInfo ignored status. | Failure propagation fixed; final mounted injected timestamp failure and subsequent success pass. |
| MS-07 | File attributes | Confirmed static: attributes accepted but unstored. | Backlog: persisted semantics or explicit rejection. |
| MS-08 | Directory delete validation | Confirmed static: SetDelete defers emptiness check. | Backlog: rejectable validation and mounted test. |
| MS-09 | Explicit mtime close race | Confirmed: close clears timestamp policy before worker. | Job snapshot plus versioned serialized timestamp commit; after-enqueue/close retry test passes. |
| MS-10 | Creation-only timestamp overlay | Confirmed: zero overlay replaced unspecified modification time. | Stat fallback corrected; creation-only timestamp regression passes after reopen. |
| MS-11 | Reclaim accounting | Confirmed: failed removal decremented charge. | Successful-removal charging and durable Dead-row retry implemented; locked-file retry regression passes. Uncommitted failed disposal remains charged and restart-only, so partially fixed. |
| MS-12 | Sparse EOF growth | Partly true: logical EOF constrained by dirty budget. | Backlog: define sparse logical-capacity policy before relaxing admission. |
| MS-13 | Linux compilation | Confirmed hosted Linux Clippy diagnostics. | Platform cfg fixed; Linux execution unavailable because WSL disk missing; hosted gate pending. |
| MS-14 | Windows session fixtures | Confirmed hosted failures and local parallel detached-child race. | EOF preserves orphan mount; exclusive recovery and per-request exact executable learning implemented. All four normal-parallel lifecycle fixtures pass, including kill before child observation. |
| MS-15 | Optional Sophon cache failures | Confirmed static: Sophon disk insertion propagates optional error. | Backlog: classified bypass and cache failure test. |
| MS-16 | Game path identity | Confirmed short/long path aliases differ across junction and process APIs. | Physical-parent normalization with virtual suffix and component comparisons implemented; real 8.3-root lifecycle regression passes. |
| MS-17 | Preparation finalization | Confirmed static: preparation exits before shared finalizer. | Backlog: whole-operation finalization and registered status error visibility. |
| MS-18 | Status polling and accounting | Partly true: interval polls overlap and status scans are serial; cost unmeasured. | Backlog: slow-status concurrency fixture and coalescing. |
| MS-19 | Durability contract | Confirmed: group-write docs overstated durable acknowledgement. | Architecture and API contract now distinguish accepted writes, explicit local durability barriers, strict writes and remote completion. |
| MS-20 | Abrupt storage crash coverage | Confirmed helper limitation: stop drains queued jobs. | Actual child termination after volume barrier/unlink passes; precommit interruption and power loss remain unqualified. |
| P-01 | Create overwrite callback contract | Not reproduced: actual WinFsp overwrite callback flow handles CREATE_ALWAYS. | Final mounted 1000-byte file truncates to exact six-byte replacement; no blind callback pre-truncation added. |
| P-02 | Open deleted inode lifetime | Confirmed: deleted inode disappears while open handles remain. | Migration31 retained inode, final-close and exclusive-owner crash recovery; actual killed-child test passes. |
| P-03 | Drive403/408 retries | Confirmed static: native_http.rs retry status set excludes 403/408. | Backlog: classify retryable publisher errors with bounded response fixtures; not every 403 is transient. |
| P-04 | Repository unregister | Confirmed static: unregister_repository deletes generations without all dependent journal/session tables. | Backlog: transactional unregister coverage and failure fixture; reported state-root cleanup path needs separate reproduction. |
| P-05 | Cache disk attribution | Confirmed static: disk.rs attributes published payloads to state_root_volume. | Backlog: configured cache-volume accounting fixture and correction. |
| P-06 | WinFsp share-access contract | Incorrect premise: WinFsp kernel enforces share access before callbacks; callbacks lack ShareAccess. | Mounted exclusive open rejected second open with sharing violation and blocked delete. Direct FFI access policy is separate; no invented ABI argument. |
| P-07 | Release credentials forwarding | Confirmed static: release builder omits mandatory credential-file parameter. | Backlog: builder parameter forwarding and noninteractive packaging contract. |
| P-08 | Reclaimed byte response | Confirmed static: response lacks reclaimed_bytes consumed by UI/tray. | Backlog: actual byte result and contract test. |
| P-09 | HTTP bridge concurrency | Confirmed static: windows_host.rs listener handles requests serially with long read timeout. | Backlog: idle preconnection concurrency fixture and bounded request workers. |
| P-10 | Per-repository job status | Confirmed static: SharedState cache/offload job values are global. | Backlog: repository-scoped status and concurrent job contract test. |
| P-11 | Flight cancellation race | Needs deterministic reproduction: flight-map lookup and last-waiter cancellation use separate synchronization. | Backlog: controlled acquire/detach interleaving before changing ownership protocol. |
| P-12 | Fetch pool limits | Partly true: speculative pool permits one worker when shared_workers is one; byte accounting depends on priority. | Backlog: define single-worker demand reservation and priority admission fixture. |
| P-13 | ETW correlation lifecycle | Confirmed static: ETW correlator removes names without removing stale FIFO entries. | Backlog: bounded lifecycle stress and mapped-I/O observation fixture; actual lost profile impact unqualified. |
| P-14 | Unicode name ordering | Partly true: ordinal_key Unicode folding differs from CompareStringOrdinal; reader also uses explicit Windows comparison. | Backlog: end-to-end Unicode lookup/collision fixtures before format/index changes; not a verified blanket lookup failure. |
| P-15 | Cache metadata layout budget | Confirmed static: metadata_length adds a header already included by layout.metadata_bytes. | Backlog: persisted-format compatibility and budget migration design; do not silently shrink existing layout. |
| P-16 | Namespace pin lifetime | Partly true: namespace pin cleanup was missing; fixed-depth traversal can fail open on deep ancestry. | Delete/rename pin cleanup repaired in this tranche. Depth bound and root rendering remain backlog with deep-tree fixture needed. |
| P-17 | macOS DS_Store mount | Confirmed static: macOS mount rejects every nonempty mountpoint, including DS_Store. | Backlog: macOS runtime mount identity validation before allowing metadata-only directories. |
| P-18 | Multi-volume token cycle | Confirmed static: token refresh failure exits repository cycle; capacity token acquired per repository. | Backlog: multi-volume expiry/failure fixture and scoped retry; no authenticated multi-volume qualification here. |
| P-19 | Signer atomic write and ACL | Partly true: signer create-new/write/rename lacks failure cleanup and explicit ACL handling. | Backlog: atomic-write retry fixture and inherited ACL review; encrypted storage is not evidence of plaintext exposure. |
| P-20 | Predictor arithmetic and clustering | Partly true: predictor multiplication is unchecked; clustering criticism is not itself a correctness failure. | Backlog: reachable overflow/config bounds and clustering quality experiments. |
| P-21 | IPC feature-version diagnostics | Partly true: feature minimum and wire protocol version are compared in diagnostic path. | Backlog: old-service compatibility fixture; wire bump is not justified by this finding alone. |
| P-22 | Balanced range fetch | Stale: publisher.rs1260 already balances parallel ranges. | No fix for alleged 68-byte second round trip. |
| P-23 | Sophon HTTP pooling | Stale: reported sophon/http.rs absent; SophonSource owns shared Client. | No per-chunk client-construction fix needed. |
| P-24 | Diagnostic ZIP headers | Partly true: ZIP DOS date is zero and integer narrowing is unchecked. | Backlog: standards-valid header fixture and reachable size-bound analysis; overflow impact not reproduced. |
| P-25 | Process error capture | Partly stale: process open path explicitly restores error after CloseHandle. | Backlog: typed process-query failure fixture; no verified blanket clobber claim. Unknown game identities remain fail-closed. |
| P-26 | Forward-slash path redaction | Stale specific mechanism: reported redact_windows_user_path function is absent. | Current RegisteredRoots uses path containment and outside-root replacement. Broader diagnostics privacy remains separate review work. |
| P-27 | Schema and version drift | Partly stale: UI package already0.1.21; CDDL1 versus wire3 and license0.1.5 remain. | Backlog: schema/license drift; current UI version claim rejected. |
| P-28 | rclone packaging consistency | Partly true: scripts pin preserved miragessd2 while PS default ismiragessd3. | Backlog: confirm intended provider variant before changing packaging provenance. |

## Execution evidence

All 48 report IDs have an explicit classification: 30 confirmed (including static evidence), 12 partly true/stale, five stale/incorrect-premise/not reproduced, and one requiring deterministic reproduction. A static confirmation does not claim an executed failure fixture. Remaining work is retained in each row; this is a verified critical repair batch, not a claim that all findings are resolved.

- Implemented and fixture-tested: ordered sticky seal retries and retained failed jobs; null/zero writes; volume-wide local barriers; timestamp propagation and ordering; durable unlink and final-close/crash recovery; guarded closed-destination rename replacement; committed-payload retry/accounting; orphan game-host preservation, exclusive recovery, exact per-request executable learning and short/long path identity.
- Additive migration 31 retains logically unlinked inodes until final close or exclusive-owner startup recovery. Existing migration/checksum and restart fixtures pass. Open-destination replacement remains conservatively rejected. Failed uncommitted payload removal remains charged but needs restart for retry.
- Windows mounted coverage verifies overwrite truncation, replacement policy, kernel sharing enforcement, timestamp failure propagation and restart byte integrity. Opening the volume device without elevation is denied before the native whole-volume callback; its explicit privileged test is ignored here. The FFI volume success/failure barrier paths are tested.
- Actual child termination verifies recovery after an explicit volume barrier, including an unlinked open inode. This does not establish all precommit interruption phases, fsync failure behavior, hardware power-loss durability, or long-duration endurance.
- Hosted CI failure logs and final local logs are preserved under ignored `target/audit-verification/`. The Linux cfg correction is source-verified; Linux execution remains unavailable because the registered WSL disk is missing. Hosted Linux validation is pending.

Final Windows verification (2026-10-15), production code frozen before the final native rebuild:

| Gate | Outcome |
|---|---|
| `cargo fmt --all -- --check` | Exit 0 |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | Exit 0 |
| `cmake --build --preset windows-msvc-debug` | Exit 0; includes final FFI/classifier code |
| `cargo test --workspace --locked` | Exit 0; aggregate result summaries: 717 passed, 0 failed, 14 ignored |
| Storage durability regression binary | 12 passed; one child helper ignored by the outer harness and invoked by its abrupt-kill parent test |
| Parallel game lifecycle binary | Four passed with byte-read and learned-profile assertions intact |
| Managed mounted binary | Three passed; one elevated volume-device test explicitly ignored |
| Payload cache lifetime binary | Three passed; unlink preserves open-handle bytes, final close removes payload, cached handle does not pin the removed name |
| UI tests / build (unchanged UI) | 65 tests in 11 files passed; build emits exactly index.html, assets/mirage-ui.js and assets/mirage-ui.css |

The 14 ignored entries comprise two authenticated Drive probes, five timing/throughput probes, one mounted 512 MiB benchmark, three subprocess entry points, the elevated volume-device test, and two long-duration endurance gates. Subprocess entry points are exercised by their parent tests where applicable; ignored probes are not passes. The complete log is authoritative for individual names and reasons.

The first full run exposed an obsolete cache test expecting live-handle backing data to disappear on unlink. Its failure log is retained separately. The repaired test requires readable bytes until last close and retains the original name-recreation check; the subsequent full suite passes. Build concurrency was limited to two jobs in the task process after observed memory pressure; no global settings changed. Developers using a separate toolchain store can set `RUSTUP_HOME` for the build process; the repository remains pinned to Rust 1.94.0.

No release, installer, deployment, or live title qualification was performed for this audit batch. Existing release artifacts and user drafts remain untouched. Final process inventory contains only the two pre-existing filesystem hosts; all synthetic game/controller/mounted-test processes have exited.
