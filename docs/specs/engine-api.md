# Platform-neutral read engine API

`ReadEngine` is `Send + Sync` and object-safe. `open_file` is deliberately synchronous: file-index
resolution against an immutable mount generation cannot perform backend discovery or wait on the
network. `read_into` is asynchronous and writes exact verified bytes directly into a caller-owned
buffer.

`FileHandleContext` pins a file index, generation identity, logical size, and platform-neutral
access mask. Win32 desired-access bits and status codes are mapped only by the future FFI adapter.

Every `ReadContext` carries P0-P6-equivalent priority, a hard deadline, cancellation token, process
role, cache mode, and access-pattern hint. Cancellation and deadline expiry are stable explicit
errors; an expired filesystem request is never retried forever.

`ReadOutcome` reports exact transferred bytes, the highest-cost cache tier used, every logical page
touched, and an optional `SealViolation` naming the exact file/page. A missing required page is never
reported as a successful zero-filled read.

The API has no WinFsp or Windows dependency. The compile contract implements an object-safe memory
engine, reads across a 1 MiB page boundary, checks exact bytes and page evidence, and verifies
cancellation/deadline failures on the normal cross-platform test runner.

## Shared miss path

`PageProvider::get_or_fetch` admits by content identity, then joins or creates a `PageFlight`
before reserving anything: only the owner reserves budget, so N duplicate readers charge one
reservation. The owner submits the fetch to a bounded, priority-ordered worker pool (fixed workers
and queue depth; queued jobs pop the lowest `FetchPriority` class first, then the earliest
deadline, then FIFO within a class; saturation is a typed `CacheFull`, never an unbounded queue or
a silent drop). Speculative priorities draw from a smaller queue credit (`speculative_queue_depth`,
which may be zero) so speculative work can never starve demand, and the owner then becomes an
ordinary waiter. The job runs fetch, decode/authenticate, verify, place, publish, then completes
every subscriber. Cancellation detaches a subscriber; the underlying fetch is cancelled only when
no remaining subscriber needs it, and dropping the owner's future cannot strand waiters. Failures
fan out with their cause preserved — checksum, authorization, missing source, timeout, budget
exhaustion, and caller cancellation stay distinct — and a completed or already-cancelled flight is
never joined again: a new caller becomes the owner of a fresh flight.

## Write-behind durability

Managed writable volumes buffer writes into per-inode segments (one payload
file per up-to-32 MiB of contiguous data) instead of committing a durable
extent version per cache-manager chunk. A segment becomes durable when it
seals, which happens on any of: `FlushFileBuffers`, file close on a
write-capable handle, segment roll-over (a non-contiguous write or the
32 MiB ceiling), or two seconds of write idle. Truncate and delete are also
seal points — truncate drains the inode first, delete discards the unsealed
tail.

A crash loses at most the unsealed tail of an open segment: durable extent
versions are committed from a map snapshot taken when the segment closes, so
no durable version can ever reference an unsealed payload file. Orphan
payload files left by a crash are swept at the next mount.

## Managed payload reads and learned verification knowledge

The managed WinFsp payload path uses `RemotePayloadStore` for published journal
payloads whose local `.payload` file has been evicted. This is distinct from
the platform-neutral `PageProvider` scheduler above.

An unindexed frame is fetched completely and verified using the unchanged
`MENCv001` AEAD decoder and its trusted publication BLAKE3 root and length.
After verification, the reader derives a compact catalog of absolute-offset
BLAKE3 subtree chaining values. Catalogs are local, ephemeral, bounded, and
derived only from fully authenticated content; they do not alter publication
metadata, ciphertext, or the on-disk database schema.

After full-frame data eviction, a sparse miss with a retained catalog fetches
only covering 64 KiB ciphertext sectors. XChaCha20 seeks past the AEAD key
derivation block, decrypts into a private zeroized-on-failure buffer, and proves
each selected sector to the existing trusted plaintext root. Bytes are
released only after all requested sectors pass. This authenticates the selected
plaintext; it does not validate unread ciphertext or the original whole-frame
tag. Full-frame demand and whole-payload restaging retain complete AEAD checks.
Missing catalogs fall back to full-frame verification; invalid sectors fail
with integrity errors, without being cached or silently substituted.

Data and proof keys bind payload identity, frame index, root, plaintext length,
representation hash, and packed-member offset. Object/frame/range arithmetic is
checked; backend replies must have the exact requested range and byte count.
Explicit contradictory revision evidence is rejected. Publication roots remain
the trust anchor for providers that echo rather than attest revision identity.

Default cache content budgets are 32 MiB of shared full frames, 8 MiB of
verified sectors, and 8 MiB of catalog allocations. LRU eviction is incremental;
oversized entries are not admitted. Active readers retain immutable shared
references outside cache locks. These budgets exclude cache bookkeeping,
publication records, returned buffers, and active miss buffers, so they are not
a process-RSS ceiling. A store-wide gate bounds eight simultaneous miss owners
and at most two background owners. A full-frame owner uses at most four balanced
backend range requests, including its envelope in those requests. Separate
whole-frame and sector flights prevent partial coverage from masquerading as a
complete frame. Cached single-frame reads avoid worker spawning and whole-frame
copying. Cancellation on the existing cancelable API remains between batches;
this does not introduce per-request deadline cancellation into the FFI API.

Sparse isolated reads do not schedule whole-payload disk restaging. Restaging
is admitted for complete-payload demand, frame-sized demand, or at least 1 MiB
of contiguous demand across multiple reads on one open handle. A restage owner
reserves the exact published payload length under the shared dirty-ledger lock
before fetching. Failed/cancelled work releases the temporary reservation;
successful placement converts it to resident accounting. Disk-floor checks
still apply before fetching and before placement. The existing bounded
lookahead applies only after this demand admission, and still shares the dirty
journal budget; a separate durable clean-sector arena is not implemented.

## Publisher chunk reads

The native read-only `--sophon` mode projects pinned publisher manifests and
reads through Mirage's `SophonSource`, without rclone or an HTTP helper. Its
chunk verification, seed reuse, bounded admission and lifecycle are specified
in [sophon-source-v1.md](sophon-source-v1.md). This is a separate source
representation: publisher MD5 chunks do not inherit the encrypted reader's
BLAKE3 sector proofs or readiness guarantees.
