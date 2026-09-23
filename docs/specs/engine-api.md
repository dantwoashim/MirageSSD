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
