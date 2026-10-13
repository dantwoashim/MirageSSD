# Native publisher-chunk source

The `--sophon` read-only WinFsp mode uses `SophonSource` in `mirage-ffi` for
namespace lookup, directory enumeration and reads. It fetches the publisher's
compressed objects directly over HTTPS with Rust/reqwest and bounded zstd
decoding. Neither rclone nor a local HTTP/Python origin is on this path.

## Representation and identity

The configuration names pinned local exported Sophon JSON manifests and an
explicit set of exported path prefixes. These are operator-supplied trust
inputs; the adapter does not authenticate a remotely supplied manifest. Every
selected asset requires safe relative paths, a valid MD5 identity and contiguous
chunk coverage equal to its file length. Duplicate case-folded names and
file/directory collisions are refused. Chunk sizes are capped at 4 MiB decoded
and 8 MiB compressed. Redirects and unsolicited content encodings are refused;
response collection and decoding are capped before verification.

All bytes released to readers come from complete chunks that match the pinned
decompressed length and publisher MD5. A read-only seed is checked the same way
as a decoded network chunk, including when the seed is sparse, incomplete or
stale. This preserves byte identity against the supplied manifest. MD5 metadata
does not supply Mirage's trusted BLAKE3 root or encrypted-sector proof contract.
The adapter cannot independently seek inside an ordinary compressed chunk.

The owned chunk cache key binds the source prefix, chunk name, compressed
length, decoded length and expected MD5. A new representation therefore cannot
reuse a pathname-sized stale entry. Cached chunk bytes are rechecked when read
from disk; verified immutable Arcs can be shared from memory.

## Admission and placement

Duplicate requests share one flight and its result. Failures complete waiters
and remove the flight, allowing a later clean retry. No failed chunk is cached.
Up to eight local miss owners and eight network owners run concurrently, with
separate gates so cold network work cannot consume local read admission. Reads
request only overlapping chunks, with at most four useful chunk workers per
wave and no speculative whole-file restaging. Single-chunk reads run inline.

Memory residency is byte-bounded; seed file handles have a separate 128-handle
LRU. Owned disk placement is serialized under one budget ledger, evicts only
hash-named files under `chunks-v1`, checks measured Windows free space and uses
an fsynced temporary file plus rename. Failed or refused placement leaves
verified bytes available transiently. A cache-root owner lock excludes another
process. Seed trees are read-only, outside this disposable ledger, and their
bytes are never duplicated merely to serve a cache hit.

The configured disk budget excludes existing read-only seed data; preserving
the seed does not reclaim its disk allocation. Memory budgets exclude caller
buffers, active decode/fetch buffers, namespace metadata and Windows caching.

## Lifecycle

Native invocation:

```text
mirage-fs <mount> <source-config.json> <storage-root> <owner-sid> --sophon <total-bytes> <free-bytes> [--label <name>]
```

Configuration fields are `manifests`, `export_prefixes`, `seed_roots`,
`cache_root`, `disk_budget_bytes`, `disk_free_floor_bytes` and
`memory_budget_bytes`. `scripts/start-sophon-mount.ps1` starts a detached hidden
host and refuses explicit restarts while the configured game process runs.
Only its exact executable/mount/source-mode process is selected for restart.

The visible generation is immutable for the host's lifetime. Close the game,
prepare and validate new manifests, then restart the source mount to activate
an update. The official launcher still owns native executable and patch data;
this source does not fabricate its verification result. Manifest acquisition,
automatic launcher/update coordination and service/UI registration are not
implemented by this adapter. Compatibility and gameplay require per-title
qualification.
