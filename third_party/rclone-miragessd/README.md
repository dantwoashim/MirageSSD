# MirageSSD rclone Windows-attribute patch

MirageSSD's writable Google Drive volume uses a narrowly patched build of
[rclone](https://github.com/rclone/rclone). Two reproducible baselines are
kept side by side:

| Variant | Upstream | Commit | Patch | Status |
|---|---|---|---|---|
| `v1.75.0-miragessd2` | v1.75.0 | `9ee9d0a0cafd5e5fe3b271d2280b090ab6e64048` | `rclone-v1.75.0.patch` | Preserved shipping baseline; do not edit |
| `v1.75.1-miragessd3` | v1.75.1 | `687d264b689b8c49a67e2e52a8a5e0caa01c04ce` | `rclone-v1.75.1.patch` | Current provider |

The upstream WinFsp/cgofuse mount did not implement `Chflags`, so Windows
hidden, read-only, system, and archive attributes disappeared immediately.
The patch implements `Chflags`, returns the saved flags from `Getattr`, and
keeps the sidecar state correct across rename and deletion. The per-Drive-root
sidecar uses an append journal, avoiding whole-map rewrites during large
copies. Individual attribute updates rely on the Windows write cache instead
of forcing one physical flush per file; a clean unmount flushes the journal.
The sidecar is included by the PC backup. File contents continue to live in
Google Drive; no attribute state is stored inside user files.

The `miragessd3` patch additionally:

- rolls back in-memory rename/remove state when the journal append fails, so
  memory and durable state stay coherent;
- compacts the journal when it crosses the threshold during a running
  session, not only at mount time;
- binds each store to the Drive account plus root identity through
  `MIRAGESSD_ATTRIBUTES_BINDING` (journal format v3) and rejects a store
  recorded for a different identity. The launcher selects the store path and
  performs binding-checked migration of older stores.

Build and test the exact source with:

```powershell
.\scripts\build-rclone-miragessd.ps1                      # current provider
.\scripts\build-rclone-miragessd.ps1 -Variant miragessd2  # archived baseline
```

The upstream rclone source is [MIT-licensed](https://github.com/rclone/rclone/blob/9ee9d0a0cafd5e5fe3b271d2280b090ab6e64048/COPYING).
The build script fetches that exact source and applies the published patch.
The package builder includes the upstream license, source archive, and patch;
dependencies and WinFsp retain their respective licenses. MirageSSD's own
Apache-2.0 license does not replace these notices.
