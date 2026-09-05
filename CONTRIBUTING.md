# Contributing

Keep changes focused, reproducible, and explicit about their effect on stored data. See the [build guide](docs/building.md) for prerequisites.

```powershell
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
```

For UI changes, run `npm ci`, `npm test`, and `npm run build` in `apps/mirage-ui`. The provider builder tests the patched mount package before compiling it.

Hardware, mounted-volume, endurance, and authenticated checks need their stated prerequisites. Unit-test success is not proof that those scenarios passed.

## Pull requests

Target `main`. Explain the behavior changed, verification performed, and any migration or recovery implications. Do not combine a functional fix with unrelated cleanup.

- Add regression tests for correctness changes.
- Preserve versioned formats and fixtures, or supply an explicit migration.
- Keep unsafe code inside documented operating-system or FFI boundaries.
- Never fabricate bytes, silently discard pending uploads, or erase originals during an ordinary copy.
- Distinguish cached-write completion from durable remote completion.
- Keep credentials, private paths, account identifiers, logs, and generated binaries out of commits.

Use disposable data for reproductions. Follow the [security policy](SECURITY.md) for sensitive failures.
