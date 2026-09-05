# Pack encryption v1

Normal imports and update staging encrypt every virtual page with
XChaCha20-Poly1305. The repository content key is generated from the operating
system CSPRNG and is persisted only as a DPAPI LocalMachine-protected
`repository-key.dpapi` record bound to the repository ID. Plaintext keys are
zeroized and are never written to manifests, packs, logs, or command output.

Pack header flag bit 0 and index-entry flag byte 57 identify encrypted frames.
Every encrypted pack carries a random 128-bit pack identity in its header and
each encrypted frame; that identity is authenticated as AAD and cross-checked
by the reader.
Unknown flags fail closed. Each frame authenticates the repository ID, the
reserved v1 pack-domain ID, its absolute pack offset, plaintext BLAKE3 hash,
and plaintext length. The immutable pack content hash and signed repository
commit bind the complete pack object; moving a frame, changing its metadata,
using another repository/key, or modifying ciphertext therefore fails before
cache admission.

`mirage repo import` encrypts by default. `--unencrypted` exists only for
explicit compatibility workflows. Service registration authenticates every
manifest page before accepting an encrypted import. Materialization, remote
scheduler reads, update staging, and extraction use the same repository-bound
decryption context. A missing or inaccessible key fails closed and never falls
back to treating an encrypted frame as plaintext.
