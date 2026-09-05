# Backend-neutral immutable object contract

`ObjectBackend` is repository-scoped and exposes exact range reads, immutable puts, stat, commit
enumeration, proof-gated deletion, and health. It contains no Drive file-ID type, OAuth type, folder
path, HTTP client, or provider-specific error. Implementations translate provider details at their
boundary.

`RemoteObjectRef` stores a bounded backend identifier, opaque provider object identifier, optional
immutable revision identity, exact byte length, BLAKE3 content hash, and immutable object kind.
Mutable hints and writer leases are not immutable object kinds.

Every read declares its requested checked range and received length. `BackendByteStream` then
enforces that exact bound while chunks are consumed: empty chunks, truncation, and overrun are
integrity failures. Collection additionally requires a caller-supplied maximum. Upload sources use
the same exact bounded-stream contract.

Errors classify authentication, permission, rate limit, missing data, transient transport,
integrity, unsupported operation, and permanent failure. Their conversion into the shared Mirage
catalog preserves retry semantics. Diagnostic sources are excluded from public envelopes.

Deletion requires a `DeletionProof` naming the repository, object hash, retained-root-set hash, and
validated sequence. Backends must still validate proof/object consistency; orchestration and later
GC milestones establish retained-root authority.

`object_backend_contract_tests!` runs the same put/hash/stat/exact-range/enumeration/deletion checks
against local and cloud implementations. Day 8 proves the contract using an isolated memory
backend; no production storage implementation is introduced here.
