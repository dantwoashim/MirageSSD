# MirageSSD rclone Windows-attribute patch

MirageSSD's writable Google Drive volume uses a narrowly patched build of
[rclone](https://github.com/rclone/rclone) `v1.75.0`, pinned to commit
`9ee9d0a0cafd5e5fe3b271d2280b090ab6e64048`.
The current optimized provider identifies itself as `v1.75.0-miragessd2`.

The upstream WinFsp/cgofuse mount did not implement `Chflags`, so Windows
hidden, read-only, system, and archive attributes disappeared immediately.
The patch implements `Chflags`, returns the saved flags from `Getattr`, and
keeps the sidecar state correct across rename and deletion. The per-Drive-root
sidecar uses an append journal, avoiding whole-map rewrites during large
copies. Individual attribute updates rely on the Windows write cache instead
of forcing one physical flush per file; a clean unmount flushes the journal and
mount-time compaction remains atomic and synchronous. The sidecar is included
by the PC backup. File contents continue to live in Google Drive; no attribute
state is stored inside user files.

Build and test the exact source with:

```powershell
.\scripts\build-rclone-miragessd.ps1
```

The upstream rclone source is [MIT-licensed](https://github.com/rclone/rclone/blob/9ee9d0a0cafd5e5fe3b271d2280b090ab6e64048/COPYING).
The build script fetches that exact source and applies the published patch.
The package builder includes the upstream license, source archive, and patch;
dependencies and WinFsp retain their respective licenses. MirageSSD's own
Apache-2.0 license does not replace these notices.
