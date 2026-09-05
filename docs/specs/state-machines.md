# MirageSSD state machines

All lifecycle changes pass through the pure transition functions in `mirage-types::transition`.
Adapters may persist or display states, but they must not invent transitions or update integer state
columns directly. Wire identifiers are stable lowercase strings.

Repository progression is import, immutable base upload/verification, unmounted readiness, mount,
admission/play or update, with explicit degraded, conflict, error, and recovery paths. Recovery is
the only ordinary exit from conflict/error.

Page progression is `absent -> fetching -> resident_clean`; clean pages may be pinned or evicted.
Writes progress through dirty, staging, staged-remote, committed-remote, and promotion into the new
resident generation. A verification failure quarantines any state holding or moving bytes. Only
verified local states are readable; pinned pages are never evictable; dirty/staged states require a
journal. Arena `FREE` and `RESERVED_WRITE` are persistence projections of logical `Absent` and
`Fetching`, not a second independent lifecycle.

Session progression is planned, reserved, materialized, verified, sealed-ready, launching, active,
helper drain, and completion. Seal violations and aborts are explicit terminal outcomes.

Update states reproduce architecture section 16.8. Forward events are idempotent at their target so
crash recovery can replay them. Terminal update journals remain durable audit evidence. Backend
health is observation-driven and never changes repository correctness by itself.

The contract test enumerates every state/event Cartesian product for all five machines. Each pair
returns exactly one known next state or a structured rejection naming the machine, state, and event.
