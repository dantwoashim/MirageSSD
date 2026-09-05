# Release process

Releases are produced only from an exact `v<version>` tag and a clean checkout with `Cargo.lock` enforced. CI runs the complete workspace tests and strict Clippy before producing binaries, a lock-derived component inventory, the exact source commit, and SHA-256 checksums. MSI construction uses WiX Toolset 4.0.6, pinned in the release workflow.

The public workflow intentionally emits unsigned artifacts until a protected code-signing identity is configured. Promotion requires signing each executable and MSI with `installer/sign.ps1`, verification on a clean Windows machine, WinFsp license inclusion, symbols, installer construction, rollback rehearsal, and publication approval. Signing credentials must never enter repository variables, arguments printed to logs, or artifacts.
