# Contributing

Keep changes focused, reproducible, and explicit about their effect on stored data. See the [build guide](docs/building.md) for prerequisites.

On Windows, install WinFsp and build the native adapter before running the full
workspace tests; the suite includes a real mounted-volume check.

```powershell
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
```

For UI changes, run `npm ci`, `npm test`, and `npm run build` in `apps/mirage-ui`. The provider builder tests the patched mount package before compiling it.

Hardware, mounted-volume, endurance, and authenticated checks need their stated prerequisites. Unit-test success is not proof that those scenarios passed.

The Windows mounted-volume test invokes the **debug** adapter. Rebuild `windows-msvc-debug` after changing the adapter or FFI; rebuilding only the release binary does not update that test's executable.

For filesystem performance measurements, use release binaries and identical request sizes, offsets, concurrency, and verification settings on native and mounted paths. Alternate their run order, separate cached from unbuffered I/O, and keep builds/tests out of the measurement interval. Report application read failures separately from filesystem cache misses: Windows Cache Manager can issue read-ahead beyond the application's requested ranges.

When editor buffers and on-disk files disagree after an external formatter, confirm changes with `git diff` and filesystem searches before building. Windows canonical `\\?\` paths can be used with file tools to inspect and update the actual files. Verify that targeted test filters run the intended tests rather than accepting a zero-test result.

## Pull requests

Target `main`. Explain the behavior changed, verification performed, and any migration or recovery implications. Do not combine a functional fix with unrelated cleanup.

- Add regression tests for correctness changes.
- Preserve versioned formats and fixtures, or supply an explicit migration.
- Keep unsafe code inside documented operating-system or FFI boundaries.
- Never fabricate bytes, silently discard pending uploads, or erase originals during an ordinary copy.
- Distinguish cached-write completion from durable remote completion.
- Keep credentials, private paths, account identifiers, logs, and generated binaries out of commits.

Use disposable data for reproductions. Follow the [security policy](SECURITY.md) for sensitive failures.
