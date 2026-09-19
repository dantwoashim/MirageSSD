# ADR 0008: Capability-selected presentation and compiled readiness

Status: accepted as the planning rule; backend selection remains an experimental decision.
Supersedes the rationale, not the prohibitions, of ADR 0001.

## Context

ADR 0001 chose WinFsp for immutable assets and rejected the Cloud Files API on
the claim that CFAPI "requires full placeholder hydration". The documented range
APIs make that blanket claim wrong: CFAPI can hydrate ranges, but `PARTIAL` is
unsupported, `PROGRESSIVE` may keep growing while handles stay open, pins are
file-level intent, and `CfUpdatePlaceholder` needs explicit invalidation ranges.
Correcting the rationale does not show that CFAPI or ProjFS beats the measured
WinFsp implementation. See `docs/proposals/2026-09-continuum-architecture.md`.

## Decision

1. Presentation is selected per file or supported subtree from a backend's
   measured capability descriptor (`BackendCapability` in `mirage-engine`), not
   from a preferred filesystem brand. A backend enters the candidate list only
   with `qualified = true`, which is set by E0-E2 evidence, never by API presence.
2. Readiness is compiled (`compile_readiness`) and admitted in a fixed order:
   compatibility, spatial, source, temporal. When no candidate passes, the
   verdict is `Unsupported` naming the failing constraints; the system never
   advertises readiness it cannot price.
3. Whole-file and progressive hydration backends are priced by their full-file
   allocation closure. Between-session dehydration is housekeeping, not the
   budget mechanism; the spatial envelope must hold at every admitted allocation.
4. The measured WinFsp path stays the baseline until a replacement wins the same
   bake-off on the same bytes and access trace.
5. ADR 0001's prohibitions remain in force: no process injection, no anti-cheat
   bypass, no hidden drivers, no fabricated integrity results, and no intentional
   missing-data faults against protected public accounts.

## Consequences

- Predictions advise preparation; they are not trust authority. An empirical
  scope is admitted as `profiled_adaptive` with visible miss and stall reporting,
  never as an offline guarantee.
- Cache-page reclaim is refused while a repository is mounted until a qualified
  cross-process reader lease exists (`docs/specs/slot-lifetime-v1.md`).
- The capability inventory script records platform facts read-only; nothing in
  this decision enables Windows features or registers sync roots.

## Revisit trigger

Revisit when E1/E2 results for CFAPI or ProjFS on disposable fixtures pass the
allocation-closure, latency, compatibility, and recovery gates, or when an
authorized publisher asset interface is available for a pilot title.
