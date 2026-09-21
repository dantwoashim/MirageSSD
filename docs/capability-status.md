# Capability status and evidence baseline

Frozen 2026-09-19 against repository state `5958d6a2ceb8fd04daa949e89249cf3d851eb963`
plus the engineering-handoff audit bundle (source SHA-256
`ef7c52c28574250e8cff7ef11d863558a651356d2ca99684304e4a65774f0ac0`).

Every externally visible capability is classified honestly:

| Class | Meaning |
|---|---|
| shipping | Present in the released product and exercised end to end |
| integrated native | Built into a shipped binary, exercising real OS/provider paths |
| library-only | Compiles and is unit-tested, but no shipping path drives it |
| simulated | Evaluated in the simulator or on synthetic traces only |
| externally qualified | Verified against the real external system (WinFsp, Drive) |

## Evidence baseline

| Artifact | Identity |
|---|---|
| Patched provider (baseline) | rclone `v1.75.0` commit `9ee9d0a0cafd5e5fe3b271d2280b090ab6e64048`, patch `rclone-v1.75.0.patch` SHA-256 `043a6483adc9d4ee572c94ec22087c2ff644f8bfbcf787c8246d658613cb1e42`, identifies as `v1.75.0-miragessd2` |
| Patched provider (current) | rclone `v1.75.1` commit `687d264b689b8c49a67e2e52a8a5e0caa01c04ce`, patch `rclone-v1.75.1.patch` SHA-256 `62faee771c9334ae2b322a7741336e77b482670e04b398fff82b9db98578d3f8`, identifies as `v1.75.1-miragessd3`; built binary SHA-256 `52f248340bda3d4271cefc19e3064eb91b4a59987cfa91d95bc4e45260b1c2d8` (this machine) |
| Provider build flags | `CGO_ENABLED=0 go build -trimpath -ldflags '-s -w -X fs.VersionSuffix=<variant>' -tags cmount`; `go test -tags cmount ./cmd/cmount` gate |
| Native adapter | `native/winfsp-adapter` via `cmake --preset windows-msvc-debug` against WinFsp 2.1.25156 |
| Engine | `cargo build --release --locked -p mirage-cli` (Rust 1.94.0) |

## Shipping mount invocation

`crates/mirage-cli/src/commands/device_drive.rs` launches the patched provider
with the exact argument list in `mount_arguments` (test-pinned). Effective
configuration: `--vfs-cache-mode full`, `--vfs-write-back 5s`,
`--vfs-cache-poll-interval 15s`, `--dir-cache-time 72h`, `--vfs-refresh`,
`--poll-interval 1m`, `--vfs-fast-fingerprint`, `--vfs-links`,
`--buffer-size 16Mi`, `--vfs-read-ahead`/`--vfs-read-chunk-size`/
`--vfs-read-chunk-streams` from options, `--drive-chunk-size 64Mi`,
`--drive-pacer-min-sleep 10ms`, `--drive-pacer-burst 200`, `--transfers 8`,
`--checkers 16`, `--vfs-case-insensitive`, `--attr-timeout 1s`. The attribute
store path and `MIRAGESSD_ATTRIBUTES_BINDING` derive from the Drive account ID
and remote root.

## Network-dependent mount preflight

`mount` runs `rclone mkdir <remote>` before mounting. That call is
network-dependent: it creates the remote folder when missing and, just as
importantly, verifies that the stored OAuth account and client binding still
authorize against the live provider before a volume is presented. It is kept
deliberately. Removing it without an identity-safe offline path would let a
stale or switched credential produce a mount that fails later or binds
attributes intended for a different account. An offline start requires a
durably recorded account+root binding check, which the managed-volume
readiness path (spec `docs/specs/readiness-v1.md`) is designed to provide.

## Component classification

| Component | Class | Evidence |
|---|---|---|
| Writable Drive mount via patched rclone | shipping | `device_drive.rs`; installer + lifecycle scripts |
| Windows attribute sidecar (journal) | integrated native | `cmd/cmount` tests run on Windows/WinFsp |
| WinFsp native adapter (read path) | integrated native | `native/winfsp-adapter`; mounted-volume test in workspace suite |
| Native write callbacks | implemented, mounted-gate-tested, not externally qualified | `service.cpp` writable callbacks; `managed_mount_writes_survive_restart` |
| `.midx` index, page arena, `PageProvider` | library-only | `crates/mirage-engine`; no shipping consumer |
| Readiness/space-lease machinery | library-only | `readiness.rs`, `space_lease.rs`; service plan path |
| Immutable pack publisher | library-only | `repository_writer.rs`; used by local import, not Drive publication |
| Experimental write overlay | library-only, unsafe for production | `update/overlay.rs`: no mutation serialization; truncate leaks stale extents |
| Cache policies beyond LRU-K prototype | simulated | `mirage-simulator` trace evaluation only |
| Predictor/prefetch | simulated | `mirage-predictor`; no offline guarantee |
| Drive object backend | integrated native | `mirage-backend-drive`; `RefreshableDriveBackend` drives on-demand page fetches inside managed mounts |
| Managed on-demand Drive page fetch | implemented, mounted-gate-tested, not externally qualified | `mirage_engine_create_managed_drive` + host stdin `TOKEN` protocol; non-resident committed pages fetch hash-verified through the bounded scheduler pool |
| Drive token lifecycle | integrated native | tokens are ~1-hour-lived; push with `mirage backend supply-token <id>` or run `mirage backend token-agent`; an expired token fails non-resident reads (unavailable) until refreshed — resident pages and all writes keep working; `repo import --pack-all` packs every regular file so all bytes are fetchable |
| rclone VFS dirty-write recovery | externally qualified | upstream `vfscache` behavior; pending-upload checks in lifecycle scripts |
| Managed mutable namespace, journal, workspace lease | implemented, mounted-gate-tested, not externally qualified | durable namespace + extent journal; `managed_mount_writes_survive_restart`; `docs/qualification/2026-09-20-audit-remediation-status.md` |
