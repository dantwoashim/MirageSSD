# ADR 0007: Recovery policy

Status: accepted for local storage; external-provider evidence pending.

MirageSSD recovers from durable truth, never from optimistic in-memory state.
Signed commit chains determine remote authority. SQLite WAL and verified cache
slot metadata determine local authority. Incomplete cache payloads, staging
objects, and clean corrupt pages are disposable. Dirty overlays, active update
journals, retained commit roots, and sealed-session leases are never discarded
automatically.

Recovery is idempotent and bounded. Ambiguous signed forks, corrupt protected
data, missing retained remote objects, or unverifiable dirty state enter a named
repair-required condition. They must not be silently resolved by choosing newer
timestamps or deleting evidence.

This decision is frozen only after the 72-hour release run plus installed
service, WinFsp, Drive-disconnect, and machine-restart validation all pass.
