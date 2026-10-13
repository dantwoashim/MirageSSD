# Genshin migration to the native Mirage source

Genshin's block, audio and video junctions now point to one native Mirage
WinFsp mount. Its Rust source adapter downloads and decodes Sophon chunks
directly; both previous rclone mounts and the Python origin were stopped.
The checked launcher and logon startup now select the native source. The
existing Play/Verify shortcuts remain usable; both use demand-only verified
chunk scheduling rather than the old rclone profile settings.

This is a deployed Mirage filesystem component from the working source tree,
alongside the existing installed Drive-backed volume. It is not a new MSI
release or a service/UI-registered publisher volume. The source implementation
and configuration/lifecycle contract are in
[sophon-source-v1.md](../specs/sophon-source-v1.md).

## Implemented behavior

- Pinned-manifest namespace, safe path validation and exact chunk coverage.
- HTTPS compressed-length checks, bounded collection/decompression, an 8 MiB
  maximum decoder window, and decoded-length/publisher-MD5 verification.
- Content/representation-bound chunk identity and shared miss flights.
- Separate eight-owner local and network gates, with four useful chunks per
  read wave; network waits cannot consume local seed admission.
- Shared immutable data, a 64 MiB memory-residency budget in this deployment,
  and 128 cached seed file handles.
- A 4 GiB owned chunk-cache ceiling, a measured 15 GiB free-space floor, atomic
  durable placement, corruption rechecks and process ownership exclusion.
- Read-only reuse of the existing approximately 69.582 GiB main cache. Valid
  seed chunks are never copied into the new disk cache just to serve a hit.

The retained seed allocation is separate from the new 4 GiB ceiling. This
migration prevents the old 100 GiB growing-cache policy from continuing; it
does not reclaim the seed's storage. Active buffers, metadata, Windows caching
and filesystem overhead are separate from the configured residency budgets.

## Verification

`cargo fmt --all -- --check`,
`cargo clippy --workspace --all-targets --locked -j 2 -- -D warnings`, and
`cargo test --workspace --locked -j 2` passed.
The Debug native adapter was rebuilt against the final FFI library. The suite
included the existing 100,000-byte mounted read, managed buffered-copy/restart
and published-eviction checks, plus the new source mount/remount test.
The Release adapter was built and copied into the game runtime with its
required installed WinFsp/MSVC runtime DLLs.

Six new source tests cover seed boundary reads/EOF/case lookup, malformed
generations, shared download/corrupt decode/clean retry/disk restart, stale
seed rejection and owned-cache eviction, FFI read/write refusal, and a real
read-only Windows mount/remount. Local seed reads also succeed with all eight
network admissions occupied.

On the actual data:

| Check | Result |
|---|---|
| Selected small reads against the previous mount | 12/12 matched |
| Complete selected blocks/audio/video | 6/6 full MD5 and lengths matched; 53,394,240 bytes |
| Uncached `.usm` cutscene fetched directly from the CDN | 6,735,616 bytes matched its full MD5; 7 compressed chunks |
| That cutscene after remount | Full MD5 matched; 7 disk hits and zero CDN chunks |
| Complete assets through the game's changed junctions | The same 6/6 full files matched |
| Existing checked startup, including Play shortcut arguments | Passed |
| Legacy helper retirement | Two rclone processes and one Python origin stopped |

The uncached cutscene application read plus MD5 took 1.350 seconds, and its
post-remount read took 0.034 seconds. These are individual warm/cold observations,
not a controlled rclone comparison or gameplay timings. The previously fetched
six-file Release read took 0.262 seconds; Windows caching can contribute.
The initial rclone small-read run populated caches, so comparing its 14.896
seconds against the later Mirage probe is not a valid speedup claim.

The deployed `mirage-fs.exe` SHA-256 is
`E062B96AA96B37C07F2EE8074D41A8A7670560DB95ED4F8C99C1A60F9A4E0D50`.
After the probes, its sampled working set was 104,210,432 bytes; this is a
single observation, not a peak-memory bound. New owned disk data was 6,735,616
bytes. Raw evidence and final check logs are under
`target/perf/sophon-integration-20261013/`.

## Recovery and limits

The runtime retains its previous startup scripts and junction targets. Its
`run/rollback-native-cutover.ps1` checks that Genshin is closed, restores the
previous helpers/junctions and stops only the matching native source host.
Both cache trees are preserved. The applied cutover was also validated by
reading complete files through the game's actual junctions after the legacy
helpers had stopped.

The user has not launched the game since migration. Title/login, gameplay
frame times, first visits and official update/repair behavior remain unqualified.
The source pins the loaded manifest generation; update activation requires
closing the game and restarting with validated manifests. It does not acquire
new manifests automatically or replace the official launcher's verification.

Publisher chunks retain their actual compression granularity. They do not
use the encrypted reader's learned 64 KiB proofs. Cold reads still depend on
CDN availability, and existing seed coverage is not an offline-readiness claim.
