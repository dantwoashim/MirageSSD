# MIRIDX02 mount index format

`.midx` v2 is an immutable, little-endian, 64-byte-aligned mount snapshot. It is derived from a validated manifest and is never edited in place.

The 320-byte header contains `MIRIDX02`, version 2, repository and generation identities, page size, six fixed section entries, a format hash, the declared file length, and an index hash. The index hash is BLAKE3 over the complete file with header bytes 88 through 119 treated as zero. This removes self-reference while authenticating every other byte.

Sections appear in this order and use exact record widths: strings (1), directories (56), files (64), extents (32), pages (48), and remote locations (104). Every section offset is 64-byte aligned. Padding is zero. The section directory records kind, record width, byte offset, byte length, and record count. Readers reject unknown kinds or widths, duplicate or missing sections, overflow, overlap, misalignment, truncation, trailing-length contradictions, and hash mismatch before exposing a view.

All offsets and lengths are unsigned 64-bit values and are checked before conversion to process-sized indexes. No structure is transmuted from bytes; fields are decoded explicitly. Names and lookup keys are UTF-8 slices in the string table. Directory and file child ranges are sorted by the deterministic ordinal case-insensitive key and then original UTF-8 bytes. Original spelling is retained.

Unsafe is limited to two reviewed Windows/OS boundaries: the read-only mapping call isolated in `mapped_file.rs` and `CompareStringOrdinal` with length-bounded UTF-16 buffers. Parsing and record access remain safe Rust.
