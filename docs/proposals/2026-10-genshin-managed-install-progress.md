# Managed Genshin implementation evidence

This checklist records implementation separately from qualification. An unchecked
title acceptance gate means gameplay remains unqualified; the local package is
an unsigned experimental preview.

- [x] Audit repository, deployed read-only source and publisher acquisition prototype.
- [x] Restore scoped Rust 1.94.0 toolchain (including clippy/rustfmt).
- [x] Acquire and validate official publisher generation/language manifests.
- [x] Native installation/import and staged maintenance; previous generation retained.
- [x] Source-owned bounded learning, preparation, pins and exit retention.
- [x] Durable user-context session controller with exclusive recoverable cleanup.
- [x] Desktop/user-helper/native flow and physical-space accounting.
- [x] Correctness, failure and race fixtures.
- [x] Required Rust, native mounted-volume and UI verification.
- [x] New local preview artifacts with matching release notes and reproducible MSI.
- [x] Bounded real publisher native acquisition and independent full native hashes.
- [ ] Real title launch, launcher handoff, future updates/repair and gameplay qualification.
- [ ] Equal-budget full-local/LRU/learned route measurements.

## Build environment

Use the pinned repository `rust-toolchain.toml`. If rustup toolchains are moved
to another volume, its component temporary directory must be on that volume as
well. A process-scoped `RUSTUP_HOME` with both directories on one volume avoids
cross-device installation failures without changing machine defaults.

## Evidence so far

- Official read-only acquisition: global publisher version 7.1.0, build
  `sMXGW2ll3Fuu`, game plus English manifests; 150,287,592,051 logical bytes.
  Metadata was staged under ignored `target/`; no live game pointer changed.
- Source tests passed, including real debug WinFsp read-only mount/remount,
  corrupt chunk retry, concurrent misses, owner exclusion, pins, generation
  expiry and Windows locked-file cleanup recovery.
- Full workspace: 706 passed, zero failed, 12 ignored (timing/live-account probes).
  Formatting and workspace clippy passed. The final unpublished-staging retry
  fix then passed affected CLI/UI-host clippy and the entire CLI suite: 51 passed,
  zero failed, two existing live-account probes ignored.
- Desktop: 65 tests passed; production build emits exactly the three permitted
  files. Games endpoint/action, async failure and reopened-window contracts are
  covered. Games setup was rendered and inspected using a CDP screenshot.
- Three real synthetic executable/WinFsp session tests passed: detached launcher
  handoff, monitoring failure while a game still reads, and worker crash followed
  by next-worker cleanup recovery. Five controller fixtures cover worker locks,
  user data preservation, retirement, failed maintenance and repeated partial
  staging reclamation. The final runtime-path regression passed all 51 CLI tests
  with two existing live-account probes ignored; mounted fixtures deliberately
  remove inherited WinFsp paths. The controller resolves the installed SxS runtime
  for its child without changing the user environment.
- An isolated four-GiB allowance downloaded 349 publisher-native files. Independent
  length and whole-file MD5 checks matched every file. Native allocation was
  1,264,467,344 bytes and total managed allocation was 1,284,396,584 bytes before
  sampled asset reads. Manifest-native encoded chunks total 581,711,663 bytes;
  this is a manifest transfer figure, not a final network meter. Virtual asset
  logical length was 149,023,900,019 bytes; those assets were not fully downloaded.

- Final packaged native host mounted the independently acquired real publisher
  source. A 161,791-byte audio asset matched its whole-file MD5, with one verified
  CDN chunk (44,195 encoded bytes), no failures and one learned chunk. Explicit
  shutdown drained the owned host; Disposable cleanup reclaimed 163,840 allocated
  bytes and returned to ready with no error. The isolated verification tree retains
  1,284,397,264 allocated bytes, including native files and bounded metadata.

## Local package

Binary source commit: `ba63602d9e952e0d513e75cc0230dcb5e06e8a92`.
Documentation qualification updates after packaging are recorded separately.
The final local directory is `target/releases/0.1.21-final/public`; the earlier
candidate is explicitly obsolete. No artifacts were published.

- Setup SHA-256: `bed8ee52b8d8cc6c680add55894c8509087dd0a08ca634b9636cee54f347a103`.
- MSI SHA-256: `53a9d35623fc5d0cab335801f5e1597c7a6723e0fd2e5b4d8d7cc3559d29330b`.
- Two MSI builds with the same source epoch were byte-identical. Compiled upgrade,
  service lifecycle and uninstall contracts passed. The MSI File table excludes
  the synthetic game fixture executable; only the four production executables,
  three UI assets and intended runtime/configuration/icon files are included.
- Staged binaries matched their build outputs; CLI and native runtime report
  0.1.21. The CLI build hash matches the binary source commit. All packaged
  SHA256SUMS entries verified. MSI installation on a separate clean machine and
  Authenticode signing remain outside this local experimental package check.

## Implemented product boundaries

Game sessions run in a detached helper under the interactive user rather than
the privileged service account. The desktop bridge is token-authenticated;
per-game locks and a durable journal serialize operations. Native files come
from the current official publisher generation. Direct launch uses its native
executable. Full HoYoPlay installer/repair integration is not qualified.

Learning uses bounded generation-specific frequency, recency and measured read
cost to prepare the next session. In-play eviction remains demand LRU with pins
and free-space pressure reclaim. Live predictive prefetch is not implemented.
Original data, native files and user data are never part of payload cleanup.

## Qualification limits

No game has been launched or modified by this implementation work. Existing
published artifacts, original installations and read-only seeds are preserved.
Synthetic reads do not establish gameplay compatibility or smoothness.
