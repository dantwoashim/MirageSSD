# Mirage engine C ABI v1

All handles are opaque and owned by the caller until passed once to the matching destroy/close function. Null destruction is harmless; other invalid or reused pointers are caller bugs. UTF-16 strings are pointer-plus-code-unit-length and must not contain NUL, malformed surrogate pairs, or traversal components. Output pointers are written only on success. Every exported entry point contains Rust panics and returns a stable numeric `MirageStatus`; internal messages never cross the ABI.
