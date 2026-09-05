# ADR 0007: DPAPI unsafe boundary

Windows DPAPI is called only from `mirage-crypto`. Its small unsafe surface converts checked Rust slices to `DATA_BLOB`, forbids UI, copies returned bytes immediately, frees Windows-owned memory, and zeroizes decrypted buffers. No other crate may call DPAPI directly.
