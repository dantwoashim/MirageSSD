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

## Advertised capabilities and the read-only boundary

`ObjectBackend::capabilities()` is a required method. It states `mutation`
(`read_only` or `archive`), `bounded_range_reads`, `revision_identity`
(`requested` or `provider_attested`), and `recovery` (`objects_only` or
`enumerable_commits`). These are contract claims, not performance claims.

A publisher or third-party origin is wrapped in `ReadOnlyOrigin<B>`, which
delegates reads, `stat`, `enumerate_commits`, and `health`, refuses
`put_immutable` and `delete_immutable` with `Unsupported` before any provider
call, and advertises `mutation = read_only`. `publish_base_generation`,
`publish_successor_generation`, and `upload_staged_pack` call
`require_publish_capability` first and fail with `ProviderUnavailable` before
uploading anything, so a read adapter can never be mistaken for the archive
mid-transaction. `assert_read_only_origin_contract` is the reusable test.

Current advertisements: the local and Drive backends are `ARCHIVE`; the
Drive backend's `revision_identity` is `requested` (see check 11 below).

## E4 representation-proof gate

Every representation the backend returns must be proven before a byte is exposed or placed. The
checks and the layer that enforces each:

| # | Check | Enforcing layer |
|---|-------|-----------------|
| 1 | Full `200` body returned to a range request is rejected before collection | backend-drive HTTP layer (`read_exact`) |
| 2 | `206` Content-Range span not matching the request is rejected | backend-drive HTTP layer |
| 3 | Malformed or missing Content-Range is rejected | backend-drive HTTP layer |
| 4 | Body shorter than the declared length is rejected | mirage-backend stream layer |
| 5 | Body longer than the declared length is rejected | mirage-backend stream layer |
| 6 | Declared length above `maximum_window` is rejected before allocation | mirage-backend stream layer (`collect_bounded`) |
| 7 | Tampered frame bytes are rejected and never installed | scheduler validate+decode (`decode_expected_with_encryption`) |
| 8 | A frame whose declared plaintext length disagrees with the expected page is rejected | scheduler validate+decode |
| 9 | A frame mapping escaping the response body is rejected | scheduler `fetch_window` bounds check |
| 10 | Encrypted frames with a wrong AEAD tag or repository key are rejected | mirage-pack (`decode_encrypted_frame`) |
| 11 | Representation identity (see note below) | gap by design |
| 12 | Zero-length stream chunks are rejected | mirage-backend stream layer |

### Representation identity

`BackendResponseMetadata.observed_revision` records the requested immutable revision, not a
provider-reported one — the Drive adapter has no response header that proves which immutable
object revision served the bytes, and `fetch_window` performs no revision comparison. The effective
identity guard is per-frame content-hash verification: bytes that fail their expected `PageHash`
never reach placement. A future check would need a provider-reported object identity (such as an
eTag or revision header returned with the body) carried on `BackendResponseMetadata` and compared
against the request before collection.
