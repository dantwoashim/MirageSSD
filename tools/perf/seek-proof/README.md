# Verified seeking in existing encrypted frames

An isolated research executable for smaller verified reads from Mirage's
unchanged `MENCv001` ciphertext. The preferred experiment verifies selected
plaintext against the **existing trusted BLAKE3 frame root and length**.

See the [findings and integration contract](../../../docs/experiments/2026-10-12-verified-encrypted-seek.md)
and [recorded evidence](../../../docs/experiments/verified-encrypted-seek-2026-10-12.json).

## Run

Use an already installed Rust toolchain with an MSVC linker on Windows. The
standalone workspace and lockfile avoid changing the product's dependencies.
The recorded run used Rust 1.94.0 and locally cached crates. `--offline` fails
if a dependency is missing; remove it for a deliberate dependency download.

From the repository root:

```powershell
# Point this at the actual installed toolchain, not a rustup download shim.
$probeToolchain = (Split-Path (Get-Command rustc).Source)
$probeRustcConfig = 'build.rustc="' + $probeToolchain + '/rustc.exe"'
& "$probeToolchain/cargo.exe" --config $probeRustcConfig build --release --offline --locked --manifest-path tools/perf/seek-proof/Cargo.toml --target-dir target/perf/seek-proof-build
if ($LASTEXITCODE -ne 0) { throw 'Research build failed' }

New-Item -ItemType Directory -Force target/perf/research-20261012 | Out-Null
& ./target/perf/seek-proof-build/release/mirage-seek-proof-research.exe |
    Set-Content -Encoding utf8 target/perf/research-20261012/seek-proof.json
if ($LASTEXITCODE -ne 0) { throw 'Correctness or CPU experiment failed' }

# Run separately from builds and other timing probes. Takes about 1 minute.
& ./target/perf/seek-proof-build/release/mirage-seek-proof-research.exe --transport |
    Set-Content -Encoding utf8 target/perf/research-20261012/seek-proof-transport.json
if ($LASTEXITCODE -ne 0) { throw 'HTTP experiment failed' }

# Validate accounting and archive complete results with source hashes.
python tools/perf/seek-proof/summarize.py --output docs/experiments/verified-encrypted-seek-2026-10-12.json
```

To check formatting without following imports into product source files:

```powershell
& "$probeToolchain/rustfmt.exe" --edition 2024 --config skip_children=true --check tools/perf/seek-proof/src/main.rs tools/perf/seek-proof/src/experiments.rs tools/perf/seek-proof/src/root_proof.rs tools/perf/seek-proof/src/root_experiments.rs tools/perf/seek-proof/src/root_proof/transport.rs
```

## What runs

- `root_proof.rs` implements seeking, tree proofs, bounded ranges, and verification
  against the existing root. It releases bytes only after all covered sectors
  have verified. Its proof codec is experimental, not Bao wire format.
- `root_experiments.rs` exercises boundary/random lengths, exact bytes,
  malformed or corrupted inputs, proof refinement from 64 KiB to 4 KiB, and
  rotating-order CPU measurements. This is the `existing_root_proof` JSON field.
- `root_proof/transport.rs` runs actual HTTP range requests against a temporary
  listener on `127.0.0.1`, with a shared 20 MiB/s body-rate cap and artificial
  response delays. It compares cached, serially fetched, and concurrently fetched
  catalogs. It closes the listener after the experiment.
- `main.rs` and `experiments.rs` also retain a comparison design with authenticated
  ciphertext-sector hashes. That alternative requires a new trusted sidecar
  commitment. It is not the preferred existing-root design.
- `summarize.py` checks result counts, checksums, geometry, and HTTP accounting,
  then archives raw observations and source hashes. Its machine annotation records
  the original host; update that annotation when recording runs on another host.

The executable imports the actual `mirage-crypto/src/aead.rs` and
`mirage-pack/src/encrypted_frame.rs` sources. Fixtures use a public test key,
generated plaintext, and random nonces. It does not read installed repository
keys, contact a cloud account, or mount a filesystem.

## Interpretation

The CPU experiment includes local response copying, cipher work, hash/proof
processing, and result copying. Catalog construction happens before timing.
The HTTP experiment includes catalog fetching where stated, but excludes initial
catalog generation. An old frame without a catalog must first be fully read and
verified; future uploads could generate catalog data from their local plaintext.

Catalog and proof sizes are serialized content sizes, not measured process RSS.
The simple boxed trees use additional heap memory. The shared bandwidth limiter
is a cap rather than a guarantee; Windows scheduling can lower delivered goodput.
The HTTP test has no TLS, Drive throttling, packet loss, or WinFsp. The results
are mechanism evidence and do not establish a production launch-time speedup.

Partial verification authenticates selected plaintext to the requested root;
it does not verify unread ciphertext or the original whole-frame AEAD tag.
The trusted root, length, and identity must come from publication metadata.
Production integration requires a specified proof codec, content/lifetime
binding, bounded admission, and separate accounting for partial residency.
