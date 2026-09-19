# Slot lifetime v1

Published slots of a sealed mounted generation cannot be reused. The capacity planner marks every cache-page candidate from a repository in any state other than `ReadyUnmounted` with `mounted_reader_exclusion`; such candidates are never selected, and their bytes are reported separately as `blocked.mounted_reader_bytes`. The check is recomputed at reclaim time, so a mount that starts after planning fails the eviction as a changed candidate.

The restriction exists because the service's capacity source and the WinFsp/FFI host each build their own `ResidentIndex` over the same arena. In-process atomics in two independently built indexes are not cross-process synchronization: a reader in the host process could hold a slot lease that the evictor's index cannot see, and the slot could be reused under a live reader.

Live adaptive eviction while mounted requires a qualified cross-process lease protocol before it can be re-enabled:

1. A reader obtains a lease for the mapped content and slot epoch before accessing bytes.
2. The writer can retire or reuse the slot only after all applicable pins and reader leases are gone.
3. Publishing a reused slot increments its epoch and uses release/acquire ordering.
4. A reader revalidates the mapping after acquiring the lease and retries if it changed.
5. A crashed reader's resources are reclaimed only after its owning process/job is confirmed dead, not merely because a heartbeat was late.

A shared bitmap or sequence counter alone does not prevent a slot from being reused after a reader has checked its generation. CFAPI pin intent is file-level and asynchronously hydrated; it cannot stand in for this range lease, and ProjFS dirty or full files must not be deleted as if they were disposable cache.
