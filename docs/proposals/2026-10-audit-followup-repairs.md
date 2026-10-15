# Follow-up audit repairs

Baseline: `bd4179f5393ea479348f0da2d7ab590131099cd8`. The supplied 22-item report was reviewed before changes: ten confirmed, ten partial, two unsupported. N01 and N12 were reproduced on disposable fixtures. This checklist distinguishes implementation from executed verification and preserves limitations.

| Finding | Repair / disposition | Verification |
|---|---|---|
| N01 | Replacement removes legacy bindings; finalizer also repairs older pending rows with restrictive bindings. | Seeded replacement/reopen and old pending-row recovery pass in the 16-test focused durability run. |
| N02 | Unregister removes restrictive dependencies and owned physical allocations transactionally; captures the configured journal before registration removal and deletes only captured owned IDs after a durability fence. | Repository suite four passed, including rollback, seeded names, sessions/updates, other-volume survival and stale physical-charge cleanup. Configured-root/unknown-file cleanup regression passed in the final workspace run. Failed physical unlink is returned as journal_files_pending; no durable automatic retry after unregister is claimed. |
| N03 | Agent reads the service mount_path and accepts only drive-letter roots. | Focused CLI regression passed. |
| N04 | Managed lookup/stat and enumeration use the same inode-derived identity, independent of the committed content ordinal. | Seeded enumeration/open identity and rename stability regression passed. |
| N05 | Logical sparse EOF is independent of dirty budget/floor; signed durable EOF is bounded and actual payload writes still enforce budget/floor/backpressure. | Sparse extension beyond budget allocates no payload, reads holes as zero, and excess actual write is refused with earlier bytes intact. Superseded payloads remain until explicit/quiesce compaction; automatic active-session reclaim is not implemented. |
| N06 | Shared reclaim mutex covers eviction candidate snapshot, durable dead transition and unlink; closed-handle check is atomic with open registration; restage placement and dead-row cleanup share exclusion. | Publication six passed, including injected DB failure preserving local bytes, locked-file durable retry, concurrent eviction charge-once and verified remote readback. Dead snapshots are loaded under exclusion, preventing stale cleanup after restage revival. Eviction retries failed physical unlinks through a volume-scoped published-dead query; locked-file retry preserves exact bytes and charge until successful removal. The logical evicted count advances at the durable dead transition, before physical space is freed. Global extent-reference checks preserve another volume's live payload; two-volume compaction regression passed. |
| N07 | Disk status attributes journal eviction candidates to each repository's configured cache volume, matching floor enforcement. | Source correction; final service/workspace gate covers compilation and existing disk-floor fixtures. No live multi-disk run claimed. |
| N08 | Materialization reuses the established service-wide arena capacity rather than resizing/conflicting with another repository budget; differing page size remains rejected. | Different-budget reuse regression passed in the final workspace run. |
| N09 | Unused conflicting FileContext definition replaced with the authoritative header. | Final debug native build passed with warnings treated as errors. |
| N10 | Unsupported buffer-invalidity claim: official WinFsp memfs uses the same deferred-buffer/response pattern. | No speculative change. |
| N11 | SetDelete and delete-on-close Create/Open reject nonempty directories before non-reporting Cleanup. | Mounted nonempty RemoveDirectory and delete-on-close rejection with byte preservation, followed by empty-directory deletion, passed in the final mounted workspace gate. |
| N12 | Live checkpoint scan excludes unlink-pending inodes without finalizing their open-handle lifetime. | Namespace suite 15 passed, including file and directory tombstones. |
| N13 | Status, login, logout, volume creation, tray metadata, agent launch and logon registration share the selected store. | Disposable metadata/delete and custom Run-command construction regressions passed; live OAuth/registry not exercised. |
| N14 | Tray refreshes account metadata from the selected store and only opens validated drive roots. | Drive-root validation regression passed; no interactive tray test. |
| N15 | Eight HTTP workers, sixteen queued sockets, prompt overflow rejection and five-second whole-request read deadline. | Idle-first/active-second loopback regression and joined worker shutdown passed. |
| N16 | Successful CLI installs persist a per-user selected root independently of job.json; status/actions share resolution, with live-job priority and legacy job compatibility. | Disposable persisted-selection, legacy-job and failed-selection regressions passed; no live game modified. |
| N17 | Published counters explicitly mean referenced remote coverage, including evicted payloads. Physical/evictable accounting continues to use the physical ledger and alive-filtered candidate query. | Post-eviction counter contract asserted in publication regression; no caller uses total published bytes as physical disk usage. |
| N18 | Retention bounds record policy only; namespace deltas/checkpoints are not pruned. | Remaining: no durable remote checkpoint acceptance/replay floor exists for safe pruning. Current local snapshots do not prove all consumers have advanced; deleting history without that proof is deferred. History can grow. |
| N19 | Blocking HTTP behind the async transport trait is an architectural limitation. | No broad executor rewrite in this repair. |
| N20 | Disk-floor pressure evicts eligible chunks in LRU order. Optional cache read/removal/placement errors do not discard downloaded verified bytes; cache failures are reported separately. | Locked corrupt-cache removal regression passed: cached bytes fail verification, remote bytes verify and satisfy read, failure counter increments. Existing chunk verification remains mandatory. |
| N21 | Foreground macOS mount supervision is intentional; AppKit does not call it. Missing unmount control is a product limitation. | No unsupported lifecycle rewrite. |
| N22 | Per-line log stat is an unmeasured optimization suggestion, not an established correctness defect. | No change. |

Existing invalid checkpoint documents are immutable historical records; filtering future checkpoints does not rewrite them. Pending seeded rows are repaired during exclusive-owner startup. No live installation or release artifact is modified by these changes.

The desktop game bridge keeps one selected installation per interactive user.
Existing CLI-only installations can be selected with `mirage game select --root <managed folder>`;
selection validates `game.json`, a ready `session.json`, and the recorded generation directory.
It does not scan drives. New failed installations do not overwrite a completed selection.
A live UI worker temporarily takes precedence for progress and prevents explicit reselection.

The agent remains one process per user. The selected token store is carried in its
launch arguments and persisted logon command. Changing the store while an older
agent is running takes effect after that agent exits or at the next Windows sign-in;
this repair does not terminate an existing agent. Tests construct the command without
writing startup registry values. Volume backend-seam tests no longer register an agent.

## Final verification

On Windows, `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets --locked -- -D warnings`, and the final `windows-msvc-debug` native rebuild passed. `cargo test --workspace --locked --no-fail-fast` exited zero: 735 passed, zero failed and 14 ignored across 208 result groups. The mounted namespace, delete-on-close, sparse-write and four parallel managed-game lifecycle tests passed. The ignored cases include explicitly invoked subprocess helpers, timing/large-copy probes, long endurance gates and the elevated volume-device flush test; their ignored status does not qualify those acceptance gates.

Desktop verification passed all 65 tests in 11 files, and the production build emitted exactly `index.html`, `assets/mirage-ui.js` and `assets/mirage-ui.css`. Final receipts and logs are retained locally under `target/audit-verification/followup-*`. This run does not establish Linux/macOS acceptance, elevated volume flushing, live OAuth/tray behavior, live gameplay or multi-disk performance.

Two earlier verification failures were retained in the local logs and resolved before the final run: the disposable crash fixture now holds its verified-reading child under an opt-in bounded watchdog while preserving the kill-before-child-observation race, and eviction again retries a failed physical unlink without losing its charge or byte-readback assertions. Final process inventory found no disposable game or mount hosts left running; existing user mounts were left alone.