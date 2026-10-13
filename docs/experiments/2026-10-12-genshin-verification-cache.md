# Genshin integrity verification: cache retention and sequential profile

The running game reported `File num to verify = 2580`. Its actual asset
junctions lead to the external Sophon/rclone runtime, not the Mirage encrypted
engine. A complete integrity scan reads remote content; a small-read proof
cannot eliminate that work.

## Observed cause

The main mount still had a 6 GiB maximum disk cache, 1 MiB fixed range
requests, 1 MiB memory buffering, and no disk read-ahead. Its cleanup log showed
7.201 GiB shrinking to 5.980 GiB during verification. Previously checked bytes
could therefore be evicted before reuse. Videos had no disk cache.

After the large Where Winds Meet data directory was removed, D: had about
137 GiB free. The official base and Japanese manifests expose approximately
94.77 GiB of blocks and audio and 42.45 GiB of videos. These are exported asset
sizes, not a proven byte count for this particular in-game scan.

A 10-second active-check observation served 337,413,918 bytes and fetched
334,851,979 compressed CDN bytes: approximately 32.18 and 31.93 MiB/s,
respectively, with no source errors. Source statistics have nested `counters`;
an initial shell calculation read the wrong level and printed zero. These
rates are calculated from the actual recorded counter differences.

## Applied runtime changes

The user chose to restart verification and confirmed that Genshin was closed.
The external startup and checked-launch scripts now support `Play` and
`Verify` profiles, and an explicit `-RestartMounts` option that refuses to run
while `GenshinImpact` exists. Only rclone processes matching the exact runtime
executable, HTTP remote, and either configured mount argument are stopped.
The origin and all existing cache bytes are preserved.

| Setting | Previous main | New Play | New Verify |
|---|---|---|---|
| Main disk cache ceiling | 6 GiB | 100 GiB | 100 GiB |
| Video disk cache ceiling | Disabled | 16 GiB, separate directory | 16 GiB, separate directory |
| Disk free-space target | 2 GiB | 15 GiB | 15 GiB |
| Range size / maximum | 1 / 1 MiB | 1 / 1 MiB | 8 / 8 MiB |
| Parallel range streams per reader | 0 | 0 | 2 |
| Memory buffer per open file | 1 MiB main, 32 MiB video | 1 MiB | 2 MiB |
| Extra disk read-ahead | 0 | 0 | 16 MiB |

The cache ceilings allocate space as bytes are read; they do not reserve the
entire budget immediately. Both mounts enforce the same free-space target,
which is a soft target checked periodically. Open files and concurrent growth
can temporarily exceed limits. The video budget intentionally cannot retain
all exported videos. See the primary [rclone mount documentation](https://rclone.org/commands/rclone_mount/).

Explicit profile changes persist for subsequent startup. Ordinary checked
startup retains existing mounts. Changing profile requires the game to be
closed and the restart option; running startup alone does not silently replace
a live mount. Desktop shortcuts `Genshin - verify files` and
`Genshin - play mode` apply the respective profile and open official HoYoPlay.
After verification, Play restores small random-read settings while retaining
the larger disk cache. The existing checked-startup shortcut remains usable.

Local backups are in the runtime's `run` directory, with names ending
`before-verification-profile-20261012.ps1`. No original asset, official
verification state, or native game executable was changed.

## Validation and limits

Windows PowerShell 5.1 successfully started both mounts and confirmed complete
directory listings. Both explicit Verify and ordinary checked startup passed
the startup-read probes. New rclone process IDs and recorded arguments confirm
that the configured profile is active.

Three complete previously uncached block files before the change read at
14.52, 30.58, and 30.77 MiB/s. Three different uncached files after it read at
22.28, 36.23, and 5.78 MiB/s. All six matched their official full-file MD5 and
length. Different files, transient CDN delays, and possible decompressed chunk
overlap prevent a controlled throughput comparison. The slow post-change
sample prevents claiming a reliable cold-download speedup from these probes.

Repeating the three post-change files produced 319.92, 331.88, and 307.49
MiB/s including application reads and MD5 hashing, with zero CDN bytes and
matching hashes. The aggregate source-byte difference of 262 bytes came from
the statistics request itself. Windows cache can also contribute to warm
rates; these are not guaranteed disk-only rates or gameplay frame times.

Raw probes are under `target/perf/genshin-fix-20261012/`:
`verify-before.json`, `verify-after.json`, and `verify-warm.json`. The durable
combined record is adjacent to this report.

The demonstrated improvement is retaining checked content across the pass
and reusing it at local speeds. First reads of uncached files still depend on
CDN throughput. The game must perform its own complete integrity check;
these samples do not certify all 2,580 files or successful gameplay.

## Resumed live verification

Genshin restarted through official HoYoPlay and its fresh log entered the
2,580-file verification phase. With no probe reads running during the final
10.078-second observation, the source served 31.56 MiB/s and fetched 31.61
compressed CDN MiB/s, with zero errors and zero missing objects. This is
approximately the previous live cold-transfer rate; the profile has not
demonstrated an increase in available download bandwidth.

The main cache's cleanup reports grew from 6.081 to 6.733 to 8.341 GiB, each
retaining the full reported amount. This confirms that the old 6 GiB eviction
ceiling is no longer active during the real check. The video cache correctly
retained a complete 581,792-byte sampled video that matched its manifest MD5.

The three baseline files also matched their full MD5 after the mount restart,
but that follow-up overlapped real game verification and reported CDN traffic.
It is not evidence of a purely cached reread. Only the earlier isolated warm
repeat supports the zero-CDN warm measurement.

The restart guard was exercised while the real game was running: it refused
the profile change with the expected close-game error, and both mount PIDs
remained unchanged. Raw follow-up records are `verify-live.json` and
`verify-cache-reuse-and-video.json`. Workspace builds were not rerun: this
change affects external PowerShell runtime settings and experiment reports,
not Rust, the native adapter, or UI code.
