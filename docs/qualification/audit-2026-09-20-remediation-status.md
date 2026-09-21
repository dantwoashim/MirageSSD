# Audit remediation status — 2026-09-20

Working-tree status against the completion tasks in
`D:\MirageSSD-Audit-2026-09-20\COMPLETION_TASKS.md` (audit base
`ea9604e7e035572605df17f01d0417edc39f54ab`, same HEAD; the remediation work
is an uncommitted working tree on top of it).

**Acceptance status for every task is "pending A24 external qualification"**
— nothing here is accepted, released, installed, or externally qualified.
Implementation advanced; handoff acceptance incomplete; not released,
installed, externally qualified, or proven useful.

Schema range present: migrations `0001`–`0023` (`0020` extent heads +
payload offsets, `0021` repository volume mode, `0022` variable-length
physical files for the journal ledger, `0023` managed namespace seed
markers), all covered by `crates/mirage-db/tests/migrations.rs`.

Managed-volume namespace (post-e2e fixes): multi-component paths resolve
through component lists (`namespace_resolve_components`), seeded-file
deletes remove the `legacy_inode_map` binding before the inode (FK order),
and delete-pending closes finalize by inode identity (`namespace_entry`).
Known limitation: once a volume's namespace is seeded, a later mount with a
different index hash does not reseed — the adapter logs
"managed namespace seeded from <old>, index now <new>; generation change
not applied" and keeps the existing namespace authoritative.

External probe evidence: the audit probe crate at
`D:\MirageSSD-Audit-2026-09-20\probes` passes 26/26 tests plus doc-tests,
including `twoq_must_honor_one_page_capacity`,
`deadline_must_complete_shared_flight`, and the `restore_*` set; the rclone
cmount attribute-journal Go probe passes under the WinFsp CGO flags.

| Task | Implementation status | Evidence | Acceptance status |
|---|---|---|---|
| A01 native baseline + artifact evidence | Done | `adapter()` freshness check in `crates/mirage-ffi/tests/mounted_gate_managed.rs`; `managed_mount_writes_survive_restart`, `mounted_provider_matches_one_hundred_thousand_random_bytes`; Debug + Release adapter builds | pending A24 external qualification |
| A02 legacy provider safety/migration/packaging | Partial | cmount attribute-journal Go tests (`cmd/cmount/attributes_test.go`, `target/rclone-src-v1.75.1`) pass; ownership-verified store migration, case-only rename and package-rebuild acceptance not executed | pending A24 external qualification |
| A03 portable encrypted recovery | Partial | `RepoCommand::Extract --envelope/--secret-file` via `recovery::envelope_content_key`/`open_envelope`; `restore_*` probes; fresh-VM-no-DPAPI restore not run | pending A24 external qualification |
| A04 bound import/upload resources | Partial | streaming manifest extraction (`ManifestContentSource::copy_file`) bounds restore memory; 100 GiB import/upload RSS measurement not run | pending A24 external qualification |
| A05 exclusive volume owner | Done | `VolumeCoordinator::acquire` locks before mutable state; `failed_startup_recovery_never_reports_ready` proves failed reclaim keeps `Recovering` and `run()` never prints `MIRAGE_READY`; `volume.rs` epoch/takeover unit tests | pending A24 external qualification |
| A06 authoritative namespace + bounded readers | Done | durable namespace authoritative for managed lookup/stat/enumerate (mounted gate); `crates/mirage-db/tests/namespace.rs` (11 tests); million-entry scale not measured | pending A24 external qualification |
| A07 managed-format publication + restore primitives | Partial | namespace checkpoints/deltas durable (`mutations_record_deltas_and_checkpoints`, `checkpoint_document_decodes_with_live_entries`); signed managed snapshot publication to remote not wired end-to-end | pending A24 external qualification |
| A08 connect/enforce physical allocator | Partial | dirty-payload budget ledgered and enforced (`dirty_budget_bounds_writes_and_survives_restart`, `extent_compact`); clean/staged/metadata/WAL accounting beyond the dirty ledger not authoritative | pending A24 external qualification |
| A09 atomic mutation commit + recovery intents | Done | `commit_mutation` commits extents + operation + payload records + physical-extent commit in one transaction; `q2_journal_replay_idempotent`; managed-volume FFI tests | pending A24 external qualification |
| A10 byte extents, retire unsafe overlay | Done | `q1_extent_replay_across_restart` covers base/dirty-offset/zero slices, EOF, truncation across restart | pending A24 external qualification |
| A11 Windows handle/namespace semantics | Done | share/delete-pending/`SetBasicInfo` paths exercised by `managed_mount_writes_survive_restart` incl. `remove_file` and PowerShell `Remove-Item`; differential NTFS suite is external | pending A24 external qualification |
| A12 writable managed volume via WinFsp | Done | `managed_mount_writes_survive_restart` (create/write/flush/patch/rename/delete/restart/remount); service launches `--managed` from persisted mode (`launcher_argv_selects_cache_or_managed_mode`, `volume_mode_persists_unmounted_and_is_refused_while_mounted`) | pending A24 external qualification |
| A13 mounted misses to real shared provider | Partial — **blocked on user: credentials / external hosts** | bounded provider + transient path implemented (`provide_sync_*`, `shared_miss_path.rs`); a real Google-Drive-backed cloud-miss mount requires provisioned credentials/hosts | pending A24 external qualification |
| A14 scheduler bounds/deadlines/promotion | Done | `transient_fetch_is_rejected_when_the_pool_is_saturated`, `transient_fetch_expired_while_queued_is_deadline_exceeded`, `pool_saturation_is_a_typed_budget_failure`; probe `deadline_must_complete_shared_flight` | pending A24 external qualification |
| A15 account-bound refresh + cancellable transport | Partial | not touched this pass; Retry-After honoring, coalesced forced refresh, and blackhole/cancellation acceptance unproven | pending A24 external qualification |
| A16 verified remote publication | Partial | `publication_orders_objects_and_is_idempotent`, `commit_is_never_visible_before_every_referenced_object` on local backend; real Drive upload + checksum readback not run | pending A24 external qualification |
| A17 remote observation + conflict preservation | Partial | `q3_divergence_preserves_both_histories`, `repository_recovery` tests; live Drive-changes paging and designated-writer enforcement under real forks unproven | pending A24 external qualification |
| A18 verified working-space lease | Partial — **50 GiB physical-budget proof not run (scratch disk not authorized)** | `q4_verified_lease_is_offline_durable`, `space_lease` tests; the 30 GiB workspace inside a 50 GiB envelope acceptance is unmeasured | pending A24 external qualification |
| A19 safe streaming native exit | Done | `manifest_restore_streams_exact_bytes_and_resumes_after_interruption`, `manifest_restore_refuses_to_overwrite_preexisting_files`, `restore_*` probes; `--envelope/--secret-file` needs no running service; clean-machine full restore is external | pending A24 external qualification |
| A20 scan-resistant cache policy | Partial | `PolicyKind::TwoQ` + `twoq_*` tests + simulator parity + `twoq_must_honor_one_page_capacity` probe; runtime eviction planning (`mirage-service/capacity.rs`) intentionally still last-access — held-out measurement gate not run | pending A24 external qualification |
| A21 truthful state + coordinated shutdown | Partial | quiesce + compaction on unmount wired in `stop()`; truthful residency/pending-bytes/lease UI state not implemented | pending A24 external qualification |
| A22 bounded history + conservative reachability | Done | `mirage_engine_compact` at startup and post-quiesce; `compaction_drops_superseded_versions_and_marks_dead_payloads`, `compact_reclaims_superseded_payloads`, gate journal==referenced assertion; remote GC stays disabled by design | pending A24 external qualification |
| A23 benchmark + fault harness | Partial | no harness work in this pass; machine-readable workload evidence outstanding | pending A24 external qualification |
| A24 original qualification protocol | Not started — **blocked on user: credentials / external hosts / participants / time** | eight comparison arms, 72-hour workload, human-user stage, VM power-fault gates all require provisioned resources | pending A24 external qualification |

## Deviations from the handoff

- `extract_virtual_files`/`extract_virtual_files_with_encryption` are
  synchronous with an internal `futures_executor::block_on` per page — the
  CLI path nests inside another `block_on`, which panics under
  `LocalPool`; async callers were updated instead.
- Migration numbering differs from the handoff text (which assumed the last
  migration was 0019): this tree already had 0020 (`extent_heads`), so the
  new migrations are `0021` (repository volume mode) and `0022`
  (variable-length `physical_files.extent_bytes` for the journal ledger).
- The durable publication ledger table is `publication_sessions` (existing
  name), not a new table.
- `--managed <total-bytes> <free-bytes>` semantics: `free-bytes` is the
  dirty-payload budget, not remaining volume space.
- Journal physical extents use a per-engine monotonic `slot_index`
  (`UNIQUE(file_id, slot_index)` forbids the constant 0), and journal
  `physical_files` registration is skip-if-present rather than
  `INSERT OR REPLACE` (`physical_extents` has `ON DELETE RESTRICT`).
- `ExtentCompactVolume` returns `(payload_id, length_bytes)` pairs, not bare
  ids — the FFI needs lengths to release dirty budget.
- The `Recovering` coordinator state is only reachable via a crashed-owner
  record (orderly `Drop` marks `Unmounted`); the FFI test simulates it by
  restoring `"mounted"` into `volume-owner.json`.
- `LocalJournal::reclaim_pending` now propagates file-removal errors other
  than `NotFound` (previously swallowed); this is what lets a failed
  recovery refuse to report ready.
- 2Q is implemented in `mirage-cache` `PolicyCore` and exercised by unit,
  parity, and probe tests only — it is sequential/parity-verified and is
  deliberately **not** the runtime eviction policy yet.

## Live Google Drive evidence (2026-09-20, one developer host)

Run against a real Google account with the installed development build
(`dist-dev`, unsigned); this is developer evidence, not A24 qualification.

- `backend login` / `verify-live`: PKCE loopback consent, exact `drive.file`
  scope, DPAPI-protected refresh token, live token refresh and quota read.
- `backend gate-drive` (Gates D and F): encrypted signed generations
  published to Drive (126.6 MB uploaded), 200,000 random reads back
  (234.0 MB downloaded) with 0 incorrect bytes, eviction refetch, staging
  eviction, stale-hint fallback, resumable restart, injected HTTP 429 and
  disconnect, 254 provider requests / 0 retries.
- Managed volume over a Drive origin: `repo import` → `publish-drive` →
  `register` → `use-drive` → `set-volume-mode --managed` → `profile capture`
  → `capsule plan` → `capsule materialize` (5 pages fetched from Drive,
  hash-verified) → `mount`. Byte-exact read-back of virtualized files,
  create/patch/rename/delete/mkdir at root and in nested directories,
  deletions of seeded files, all persisted across unmount/remount.

Known gaps found by this run (open, not defects of the legacy rclone drive):

- The managed adapter has **no page-provider hook**: committed content is
  served only from pages already materialized into the local cache shard.
  A read of a non-resident page fails instead of fetching from Drive.
- Files the importer classifies as native-class (small files, unreviewed
  extensions) are listed with correct sizes on a managed mount but have no
  committed content and fail to read (`EINVAL`). A general-purpose workspace
  disk needs every imported file to be readable.
- Managed writes stay local (journal payloads + `byte_extents`); no path yet
  publishes dirty extents to Drive, so cloud capacity is used for committed
  generations only.
- A repository whose index changes after the namespace was seeded keeps the
  old namespace (stderr warning; migration 0023 seed marker).

## Paired hot-path benchmark vs the legacy rclone drive (2026-09-21, one host)

`scripts/bench-paired-mounts.ps1`, result `dist-dev/bench/paired-mounts-hot.json`.
Same workload on both mounts (200 x 4 KiB + 20 x 1 MiB + 2 x 64 MiB =
149 MiB written, hashed read-back, 1000 random 4 KiB reads, rename and
delete of all files), 10 interleaved iterations in randomized order.
Managed volume = `dist-dev` Release `mirage-fs.exe` on a copied state root
with a live Drive token; rclone = the user's production `M:` mount
(`--vfs-cache-mode full`). Medians (p25-p75):

| Metric | Managed | rclone |
| --- | --- | --- |
| write 149 MiB | 3.32 s (3.19-3.40) = 45.5 MB/s | 3.93 s (3.81-4.28) = 38.2 MB/s |
| sequential read + hash | 0.47 s (0.43-1.19) = 326 MB/s | 1.85 s (1.82-1.87) = 80.5 MB/s |
| random 4 KiB read | 727 us/op (678-757) | 11,370 us/op (10,435-11,585) |
| stat 222 files | 250 ms (243-274) | 189 ms (184-195) |
| listdir | 8.5 ms | 10.9 ms |
| rename 222 files | 1.24 s (1.22-1.26) | 331 s (322-335) |
| delete 222 files | 1.29 s (1.26-1.29) | 235 s (230-237) |
| incorrect bytes (files / random reads) | 0 / 0 | 0 / 0 |

Reading: both volumes served these reads from local cache (hot path). The
managed volume is faster on write, read, and random read, and two orders of
magnitude faster on rename/delete (rclone's VFS turns each into remote
operations); it is slower on per-file stat (about 1.1 ms vs 0.85 ms per
file, an SQLite lookup per stat). Not measured here: cold reads from Drive
on either side (the production rclone cache could not be evicted), upload
of written data (the managed volume did not publish writes at the time of
this run), long-running stability, and any other host. N=10 on one machine
is indicative evidence, not A24 qualification, and does not by itself
justify a "surpasses rclone" claim.
