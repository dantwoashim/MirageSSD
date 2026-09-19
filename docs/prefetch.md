# Optional file preparation

This is an opt-in read-through helper, not a new cache or an automatic predictor.
It reads explicitly selected files through the mounted drive. The existing VFS
owns cache admission, local edits, remote reads, invalidation and eviction.
There are no direct writes to cache files or metadata and no extra Drive login.

New installations include **MirageSSD Prepare Files** in the Start menu. It asks
you to select ZIP archives, then reads at most their first 4 KiB and last 128 KiB.
Overlapping ranges are merged. This can prepare directory metadata, not an entire
archive. Large ZIP directories may still need additional ordinary reads.

For an existing installation, run the source helper without reinstalling:

```powershell
.\scripts\prefetch-device.ps1 -Path 'M:\Archives\example.zip'
.\scripts\prefetch-device.ps1 -Path 'M:\Documents\large.pdf' -Mode Prefix
.\scripts\prefetch-device.ps1 -Path 'M:\Assets\small.bin' -Mode WholeFile -MaxBytes 33554432
```

`Prefix` reads at most 1 MiB per file. `WholeFile` skips files that cannot fit
entirely in the remaining requested-read budget; it does not silently report a
partial file as ready. Repeat `-Path` as a PowerShell array for up to 32 files.
Duplicate paths are processed once. `-PlanOnly` opens/stat-checks selected files
but issues no content reads; metadata requests can still reach the provider.

## Bounds and cancellation

- Default aggregate requested-read budget: 32 MiB; configurable up to 1 GiB.
- One helper per Windows user, one file at a time, a 64 KiB transfer buffer.
- Default timeout: 60 seconds. Active asynchronous reads receive cancellation.
  File opens and metadata calls are synchronous and may outlast this timeout.
- Ctrl+C stops the command; closing the helper does not stop the mounted drive.
- Files open read-only without write/delete sharing. Busy files fail rather than
  taking over an application's write handle. Links, alternate streams, folders
  and paths outside the installed volume are rejected.
- Local free space is checked before each content read. The installed reserve
  plus 512 MiB headroom must remain available. Older installations default to a
  24 GiB reserve. This is a check, not a physical-space reservation.

**Requested bytes are not a hard download or cache-growth limit.** The provider's
existing chunked reads and read-ahead can fetch more data and may continue after
the helper exits. Concurrent applications can consume disk space between checks.
Do not run preparation on a nearly full disk or to pin irreplaceable data.

Reports distinguish planned, skipped, cancelled and completed reads, report the
actual bytes returned to this helper, and leave network bytes unknown. They do
not assert that cached bytes remain resident, that a remote copy is durable, that
a remote file cannot change, or that applications are ready to run offline.
Reports contain selected paths; inspect/redact them before sharing.

## Performance evidence

No cloud or local-SSD speedup is claimed for this helper. The supplied acceleration
lab is a loopback experiment; its bundle result is not a benchmark of this mount.
Measure a chosen real workload with existing read-ahead first, then measure the
same workload with preparation, charging preparation time and extra downloaded
bytes. Do not clear a live cache to manufacture cold measurements.

Compression, auxiliary bundles, cache pinning/scan-resistant eviction, learned
profiles and direct provider range-prefetch APIs are not implemented here. They
need separate evidence and safety work; none is silently enabled.
