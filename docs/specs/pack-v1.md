# Mirage immutable pack v1

Pack v1 is a deterministic, append-then-seal container for independently verifiable logical pages. All integers are little-endian. Writers use temporary files, flush and verify the complete pack, then rename to `pack-<content-hash>.bin`. Once named, pack bytes are immutable.

## Layout

1. A 64-byte pack header records magic `MRGPACK1`, version 1, page size, index offset/length, entry count, encryption flag, per-pack identity, and CRC32 over the meaningful header prefix. Plain packs require a zero pack identity; encrypted packs require a nonzero random identity.
2. Frames follow in physical append order. A writer may zero-pad each frame start to 4 KiB alignment; padding is not part of any frame range.
3. The fixed-width index contains 64-byte entries sorted strictly by plaintext page hash. Each entry records the frame's physical range, logical and encoded lengths, and codec.
4. A 128-byte footer records the BLAKE3 index hash, BLAKE3 hash of every byte before the footer, final pack length, and CRC32. Reserved bytes must be zero. The immutable object identity and `pack-<hash>.bin` name use a separate BLAKE3 hash over the complete file including this footer, avoiding any circular self-hash.

## Frame v1

The 64-byte frame header contains magic `MPF1`, version, flags, plaintext BLAKE3 hash, plaintext length, encoded length, codec, encryption-metadata length, and header CRC32. Week 6 permits only codec `none`, zero flags, and no encryption metadata. The payload immediately follows. Lengths are bounded before allocation and every decoded payload must match its plaintext hash.

## Verification and reads

`PackReader::open_verified` checks header/footer CRCs, file length, index and content hashes, strict index ordering, non-overlapping physical ranges, page-size bounds, and range containment. Page lookup is binary search. Coalesced range plans sort physical ranges, reject overlaps/out-of-bounds frames, and merge only when both the gap and total window remain within caller bounds. A page may be decoded from a larger range only when its exact indexed frame slice is fully present.

Authenticated encrypted frames and the DPAPI repository-key lifecycle are
specified in [pack-encryption-v1.md](pack-encryption-v1.md). Encryption is the
normal import and update path; the original plain frame remains readable for
explicit compatibility imports.

## Crash and recovery contract

Each writer uses a cryptographically unique `pack-building-<id>.tmp` path, so concurrent import and update writers never delete or overwrite each other's work. An incomplete temporary file has no trusted footer and is never accepted as an immutable pack. Cancellation removes only the calling writer's own temporary file; completed verified packs remain resumable. Source data is never deleted or modified.
