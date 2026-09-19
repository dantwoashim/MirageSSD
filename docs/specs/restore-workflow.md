# Fresh restore workflow

Fresh restore starts from an empty state root and a user-confirmed repository trust verifier. It enumerates and validates the signed commit chain, downloads only commit and manifest metadata, validates every referenced pack by provider stat, compiles an immutable mount index, creates an empty sparse cache arena, and reports every native executable/library/configuration/mutable path required from a legitimate local installation.

It never synthesizes native shell files and never downloads whole packs. Mounting remains forbidden until a compatible local shell is supplied and separately validated.

## Recovery envelope

The machine-bound DPAPI key record (`repository-key.dpapi`) cannot survive a
machine loss. A recovery envelope (`MRECV001`) is a small portable file that
seals the repository content key — and optionally the commit-signer secret —
under an AEAD key derived from a user-held recovery secret (Argon2id +
XChaCha20-Poly1305). The envelope header binds the repository identity, KDF
parameters, and declared contents, so a tampered or mismatched envelope fails
authentication.

Commands:

- `mirage repo recovery export --import <dir> --envelope <file> --secret-file <file>`
  unwraps the DPAPI key record and writes a new envelope. Export is explicit
  and refuses to overwrite an existing envelope. Optionally
  `--signer-store <file>` includes signing authority.
- `mirage repo recovery verify --envelope <file> --secret-file <file>
  [--import <dir> | --repository-id <id>]` proves the envelope decrypts, that
  it carries a content key, and — when `--import` is supplied — that the key
  equals this repository's live key record. A successful verified run writes
  `recovery-verified.json` into the import directory; destructive reclamation
  of encrypted originals (native backup eviction) is blocked until that
  record exists and matches the current content key.
- `mirage repo recovery import --envelope <file> --secret-file <file>
  --destination <dir>` installs the recovered key into a fresh DPAPI record
  on the new machine. This restores read access only; resuming the old writer
  lineage is a separate deliberate operation.

Legacy `MREKv001` records contain signing authority only and are reported as
incomplete for encrypted repositories rather than silently accepted.

If both the machine-bound key and every recovery envelope plus its secret are
lost, encrypted repository content is unrecoverable — MirageSSD cannot bypass
its own encryption, and no service-side copy of the key exists. Recovery
secrets are never logged and never written to the repository or Drive.
