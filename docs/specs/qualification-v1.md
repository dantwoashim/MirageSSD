# Release qualification v1

The release-qualification harness proves the managed-volume durability
contracts end-to-end. Each scenario maps to a handoff acceptance criterion;
a release candidate must pass every scenario plus the full workspace gates.

## Gates

```powershell
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/test-capability-inventory.ps1
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/test_release_qualification.ps1
```

Mounted-volume tests additionally require WinFsp and a rebuilt
`windows-msvc-debug` adapter.

## Fault matrix

`crates/mirage-engine/tests/release_qualification.rs` runs the required
scenarios:

| Scenario | Proves |
| --- | --- |
| Q1 extent replay | Durable versioned extents replay after restart; base, dirty (with mid-extent payload offsets), and zero slices read correctly; truncation extension is durable. |
| Q2 journal idempotency | Pending operations are not replayable, committed ones are, and a flush fence acknowledges durability for everything committed before it. |
| Q3 divergence preservation | A concurrent remote commit records a divergence that keeps local and remote heads visible; nothing is overwritten. |
| Q4 lease durability | A verified workspace lease admits content after restart without re-verification; revocation denies admission. |
| Q5 export completeness | Native restore refuses to finish while bytes are unavailable and `verify_complete` gates remote deletion. |

## Workload coverage

- `mirage-simulator` replay/sweep/held-out drivers compare cache policies on
  equal budgets with abstention (ADR-compliant baselines).
- `tests/integration` covers sealed-synthetic, cache-endurance, and 72h
  endurance workloads on the rclone legacy product.
- The capability inventory (`scripts/test-capability-inventory.ps1`) is the
  read-only platform gate.

## Environment limits

The WinFsp SDK is not required for the fault matrix — it exercises the
durable machinery directly. Native adapter compilation is verified only on
machines with the SDK installed.
