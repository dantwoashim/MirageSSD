# Persistent format rules

All security-sensitive persistent objects use canonical CBOR with definite-length arrays/maps and
ascending unsigned integer field keys. Encoders emit the shortest legal integer/length form and a
single representation for null, booleans, strings, byte strings, and signed timestamps. Hashes and
identifiers are fixed-size byte strings, never display-formatted hex inside CBOR.

Exact maps reject unknown fields and duplicate keys until an explicit, versioned compatibility rule
is approved. Decoders check the input byte ceiling and every declared count before allocation, use
checked offset/range arithmetic, reject invalid UTF-8 and unsafe paths, and validate semantic graph
invariants before returning an object. Indefinite-length data is not accepted.

Declared global ceilings are one million directories, one million files, sixteen million extents,
pages, and remote locations, 255 UTF-8 bytes per path component, 32,767 bytes per logical path,
`i64::MAX` logical file bytes, `u32::MAX` pages per extent, 1,024-byte provider object identifiers,
and 128-byte signatures. Callers may impose smaller decode budgets.

Logical page identity is the BLAKE3 hash and plaintext logical length. A remote location is separate:
backend/object/revision identity, whole-object length and hash, encoded offset/length, and codec.
Remote ranges must fit the immutable object using checked arithmetic.

Commit identity covers the exact canonical unsigned body and signature envelope. Parent hash,
sequence, manifest hash/object, referenced-pack-set hash, writer device, and optional update journal
are mandatory schema concepts. `LATEST` is not a commit field and never establishes chain validity.

Golden fixtures record exact bytes and BLAKE3 hashes. A format change requires a new schema version
and fixtures; historical bytes are never silently reinterpreted.
