# Readiness v1

Readiness is a set of distinct promises that must never be blended behind a single "ready" flag. A readiness record names exactly one mode and binds the evidence that justifies it.

## Modes

| Mode | Required evidence | Honest promise |
|---|---|---|
| Verified scope | Complete, authenticated dependency closure for a declared scope; verified residency; protected lifetimes | No origin fetch is needed for those declared immutable bytes while the contract holds |
| Profiled/adaptive play | Version-bound observations, a tested cache plan, and an available origin | Qualified best-effort streaming with measured miss and stall rates; not unlimited offline coverage |
| Full-local compatibility | All required files materialized and verified on native storage | Native file presentation, at the cost of its actual local footprint |
| Maintenance | Explicit update/verify/restore operation and its own reservations | The operation may consume time, bandwidth, and additional storage, all reported separately |
| Unsupported under this budget | No plan meets compatibility, space, and delivery constraints | Refuse to claim ready; preserve originals and explain which constraint failed |

A pinned prepared set is completely resident learned content; it is not a complete dependency closure and is never reported as verified scope.

## Scope completeness

Every record carries a scope identifier and a completeness flag: `complete` means the dependency closure is authenticated and whole; `empirical` means the scope is a bounded observation set whose misses are measured, not excluded. `verified_scope` requires `complete`; `profiled_adaptive` requires `empirical`. A record is only issued for a supported plan, so `unsupported` never validates as a stored record.

## Spatial admission

Mirage-managed storage enforces at all times:

```
allocated + reserved_new_allocation + dirty_staging + rollback_retention + journal_and_metadata + filesystem_slack <= B_managed
```

The categories must not double-count the same reservation and must not omit simultaneous copies: a retained arena copy and an NTFS hydrated copy are two allocations even when their content hash is identical. A sparse logical length is not physical allocation, and checking usage only after eviction is not a hard budget test. A separate RAM credit pool and a separate total-game-footprint view are kept alongside the managed envelope.

## Temporal admission

For every required unit `i`, readiness requires `verified_available_time(i) <= required_time(i)`. The latest-start estimate sums queue delay, source time-to-first-byte, encoded transfer bytes divided by effective goodput, decoding, verification, placement, and a measured safety margin. Predictors must supply lead time, not just a popularity rank.

## Fetch failure taxonomy

Checksum failure, authorization failure, source disappearance, timeout, budget exhaustion, and caller cancellation are different causes and stay distinct end to end: `checksum_mismatch`, `authorization_denied`, `source_missing`, `timeout`, `budget_exhausted`, `caller_cancelled`, plus `malformed_response` and `internal` for the remainder. Never substitute zeros, false EOF, or fake success.

## Readiness record

The record binds: repository, generation, and manifest digest; configuration label and profile schema version; scope identifier and completeness; the mode; required verification units, bytes, and native-file count; the presentation backend; the spatial envelope and managed budget; temporal lead time; RAM credit and in-flight limits; backend, OS, driver, and runtime qualification versions; the pin generation; and the conditions that invalidate readiness.

A signed record authenticates its issuer and contents. It does not prove that a heuristic model predicts every future read or that an internet connection cannot fail.

## Compilation

`compile_readiness` is a pure function: it validates the scope, prices every backend candidate in preference order, and returns the first `Ready` record or an `Unsupported` plan listing every rejection. It mutates nothing and never issues a record for `unsupported` mode.

Each candidate is evaluated in a fixed order; the first refusing constraint rejects it:

1. **Compatibility** — an unqualified backend (E0–E2 gates not passed for this title/version) is rejected outright.
2. **Spatial** — the file closure is priced by the backend's hydration granularity: `page` pays `required_bytes`; `whole_file` and `progressive_file` pay the full `logical_size`; and `whole_file_pin_only` backends (file-level pin intent such as CFAPI) pay `logical_size` for `complete` scopes regardless. Native files always pay `logical_size` minus verified-resident bytes. The missing bytes are added to `reserved_new_allocation`, unit metadata to `journal_and_metadata`, and the resulting envelope must admit the managed budget.
3. **Source** — if any required byte is missing and no authorized origin is available, the plan is rejected. Empirical scopes always require an origin: adaptive play may touch unobserved bytes.
4. **Temporal** — empirical scopes only. The scope must declare a lead time, and the latest-start estimate (queue delay, source TTFB, encoded transfer at effective goodput, decode, verify, placement, safety margin) must verify the largest required unit before the lead expires.

Mode selection: `complete` scope on `native_files` with every virtual file priced at full closure (or no virtual files) yields `full_local`; `complete` otherwise yields `verified_scope`; `empirical` yields `profiled_adaptive`. When no virtual files exist the presentation is `native_files` regardless of the candidate.

## Service integration

`plan` (`crates/mirage-service/src/runtime.rs`) compiles a readiness verdict for every capsule plan it emits. The scope id is the capsule identifier; completeness is `complete` for a full-volume plan and `empirical` otherwise, with a temporal lead of the profile startup window. Files are priced per required page from the mount index, and resident bytes come from the resident cache slots. The candidate list holds the single measured WinFsp baseline per ADR 0008, and the origin estimate uses the declared `ASSUMED_*` constants shared with the simulator's network model — assumptions, not measurements; origin reachability is re-checked at admission. On `Ready`, the record is persisted beside the plan as `capsules/{capsule_id}.readiness.json` and summarized in the plan response. An `Unsupported` verdict fails `plan` with the refusing constraint class preserved: `spatial` becomes `CacheFull`, `compatibility` becomes `UnsupportedLayout`, and `source`/`temporal` become `BackendUnavailable`.
