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
