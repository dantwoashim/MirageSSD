# Verified small reads from existing encrypted Mirage payloads

2026-10-12. Executable prototype and controlled transport measurements.

Follow-up: the 64 KiB learned-catalog path is integrated into the managed reader
in [the October 13 upgrade](2026-10-13-verified-sector-upgrade.md). The original
prototype measurements below retain their original scope.

**Mirage's existing encrypted payloads can support much smaller authenticated
reads without rewriting their ciphertext.** A working prototype combines
XChaCha20 seeking with subtree proofs to the plaintext BLAKE3 hash Mirage already
stores. It uses the actual repository encoder, decoder, and key implementation.

For an aligned 4 KiB request in a 4 MiB frame, the prototype returns exact bytes
using 4,468 bytes of ciphertext, header, and proof instead of 4,194,372 bytes:
**a 938.76x reduction in response content**. At 64 KiB verification granularity, the
corresponding reduction is 63.76x. These are measured/generated representation
sizes, not whole-application speedup claims.

This changes the previous research decision. Smaller independent AEAD records
remain an option for a future format, but are not necessary to obtain smaller
authenticated reads from this existing representation. An auxiliary proof
catalog is necessary; it can be created during publication or from an ordinary
fully verified read of an old frame.

Implementation: [`tools/perf/seek-proof`](../../tools/perf/seek-proof/README.md).
Recorded evidence: [`verified-encrypted-seek-2026-10-12.json`](verified-encrypted-seek-2026-10-12.json).
The production read path and stored user data are unchanged.

## The mechanism

The current `MENCv001` frame has a 52-byte prefix, XChaCha20-Poly1305 ciphertext,
and a 16-byte tag. The plaintext is not additionally compressed by this encoder.
`PayloadRemoteObject.frame_hashes` already contains a BLAKE3 hash of each plaintext
frame. The normal decoder checks both the whole-frame AEAD and this content hash.

The prototype uses a different verification path for partial reads:

1. Obtain the **existing trusted plaintext hash and length** for the requested
   frame from publication metadata.
2. Obtain the frame header and the relevant BLAKE3 proof. These inputs may be
   untrusted; changing them cannot authorize different output bytes.
3. Read only the covering ciphertext sector(s) from the unchanged object.
4. Seek XChaCha20 to the appropriate keystream position and decrypt into a private
   temporary buffer.
5. Reconstruct the BLAKE3 root from the decrypted sector and its proof, using the
   requested position and trusted frame length. Release bytes only after every
   covered sector matches the existing root.

Within one encoded frame:

```text
ciphertext offset = 52 + aligned plaintext offset
keystream offset  = 64 + aligned plaintext offset
```

The 64-byte adjustment skips stream block zero, which the AEAD uses when deriving
its Poly1305 key. This behavior is explicit in the
[RustCrypto implementation used by Mirage](https://docs.rs/chacha20poly1305/0.10.1/src/chacha20poly1305/cipher.rs.html).
Provider-object addressing must additionally include the packed member offset
and encoded-frame offset, using checked arithmetic.

The proof is built from **non-root chaining values with absolute input offsets**,
not ordinary standalone hashes of each small sector. The BLAKE3 root flag is
used at the final whole-frame merge. Those details preserve the exact hash that
is already in Mirage's metadata. The implementation uses the installed BLAKE3
1.8.7 tree APIs, compares reconstructed roots with `blake3::hash`, and tests
non-power-of-two lengths and shortened final sectors.

This is a new application of established mechanisms to Mirage, not a claim of
inventing Merkle proofs, a cipher, or verified streaming. The
[Bao specification](https://github.com/oconnor663/bao/blob/master/docs/spec.md)
describes range verification against an existing BLAKE3 root, and
[bao-tree](https://docs.rs/bao-tree/latest/bao_tree/) supports grouped blocks and
range processing. The important project finding is that these mechanisms can
be combined with this legacy ciphertext representation.

## What was tested

The executable imports these production source files directly:

- `crates/mirage-crypto/src/aead.rs`
- `crates/mirage-pack/src/encrypted_frame.rs`

It creates disposable random-looking fixtures with a public test key, encodes
them using the actual encoder, and compares returned bytes against the originals.
It never reads an installed repository key or modifies a mounted volume.

The preferred existing-root prototype passed **10,742 exact-byte cases** and
**1,588 rejection cases**. It exercised 89 lengths: 25 explicit boundary cases
and 64 generated lengths, at 4 KiB, 64 KiB, and 256 KiB granularity. It compared
206 nontrivial reconstructed tree roots with the standard BLAKE3 hash.

Cases include empty input, cipher/chunk boundaries, sector boundaries, partial
final sectors, full-frame reads, random ranges, integer overflow, EOF violations,
wrong keys, stale/different roots, altered headers/nonces, altered proofs,
mispositioned ciphertext, short/oversized responses, and extra/missing proof
nodes. The original ciphertext is unchanged. A separate sidecar-authentication
experiment is retained as a comparison; the existing-root design avoids its
additional trust-anchor requirement.

The CPU benchmark runs 800 reads per variant in eight rotating-order rounds,
with identical offsets and exact output checks. It measures response-buffer
copies, decryption, hashing, proof processing, and output copying in process.
The recorded JSON has per-round medians and nearest-rank p99s; a small number of
short rounds is not a reliable product tail-latency study.

| Aligned 4 KiB read | Response content including header/proof | Measured local work, approximately |
|---|---:|---:|
| Full legacy frame | 4,194,372 bytes | 6 ms |
| 256 KiB sector and proof | 262,324 bytes | 0.26 ms |
| 64 KiB sector and proof | 65,780 bytes | 0.065 ms |
| 4 KiB sector and proof | 4,468 bytes | 0.006 ms |

The roughly thousandfold difference in the last row applies only to this
isolated cold-frame processing operation. It excludes application work,
WinFsp, backend delays, catalog creation, and persistent-cache management.

## The transport experiment

A real loopback HTTP range server serves the unchanged encrypted object and a
2,100-byte catalog. All responses share a 20 MiB/s body-bandwidth limiter. An
artificial delay before response headers varies from zero to two seconds. The
limiter is a cap; OS scheduling adds overhead, so delivered goodput can be lower.
Every request returns bytes that the client verifies against the fixture.

The baseline uses **one whole-frame request**, so these results do not give the
new design credit for fixing the old serialized 68-byte tail.

| Configured response delay | Whole frame | 64 KiB, catalog cached | Catalog then data, serial | Catalog and data, parallel |
|---:|---:|---:|---:|---:|
| 0 ms | 249.03 ms | 4.97 ms | 6.15 ms | 4.85 ms |
| 40 ms | 293.41 ms | 45.78 ms | 88.45 ms | 45.74 ms |
| 200 ms | 453.26 ms | 205.57 ms | 408.20 ms | 205.63 ms |
| 2,000 ms | 2,253.76 ms | 2,005.54 ms | 4,007.96 ms | 2,005.91 ms |

Entries are medians of four paired rounds for the first three settings and two
rounds for the two-second setting. The source is controlled HTTP on localhost,
not Google Drive or a WinFsp mount. Request/body accounting excludes HTTP/TCP
headers. The server has no TLS, cloud throttling, or packet loss.

At 40 ms, the cached-catalog case is about **6.4x faster**. At two seconds it is
only about **1.12x faster**, despite the large byte reduction. The long initial
wait remains. The serial-catalog case demonstrates a failure mode: adding a
dependent metadata request can make the new path slower than the old one.

The remedy tested here is fetching catalog and ciphertext concurrently when
geometry and object mapping are already known. Decryption results remain private
until proof validation succeeds. It costs two requests on a catalog miss but
largely overlaps their initial waits. Persisting useful proof metadata avoids
even that extra request on later data misses.

## A second result: refine knowledge instead of retaining every byte

A coarse proof can be refined after verifying only its covered region:

```text
existing 4 MiB root
    -> verify one 64 KiB region
    -> compute its 4 KiB subtree proofs locally
    -> retain useful fine proofs while discarding bulk data
    -> later fetch and authenticate only the requested 4 KiB
```

The prototype verified a nonzero-offset 64 KiB region, constructed its sixteen
4 KiB proofs, composed each with the outer proof, and successfully read all
sixteen regions against the original 4 MiB root. It rejected every corresponding
proof built with an incorrect zero-based chunk counter. No second full-frame
download was needed for refinement.

Each selected fine leaf needs four additional sibling values inside the 64 KiB
region: **128 bytes** beyond the coarse path. A complete proof to the 4 MiB root
has ten siblings, or 320 bytes. This enables selective refinement around actual
access instead of storing a fully fine-grained index for the whole archive.

The useful persistent object may therefore be a **small proof of how to verify
future bytes**, even after the bytes themselves have been evicted. This changes
the cost of a later miss. A joint policy can choose among retaining data,
retaining a compact proof/catalog, or retaining neither.

This is the larger research hypothesis: allocate a small amount of residency to
knowledge that makes future misses cheap, and refine that knowledge as real reads
reveal useful regions. Its end-to-end cache policy has not been implemented or
compared on real application traces yet.

## Metadata and bootstrap costs

The proof cannot be manufactured from a root hash alone. Someone must observe
the content once to build the necessary tree values. Three workable paths are:

1. Generate them while uploading a new frame, when its plaintext is already local.
2. Generate and retain them after a normal full-frame read of an old object has
   passed the existing AEAD and content-hash checks.
3. Backfill old objects deliberately, paying their read cost. This is useful only
   when expected later savings justify the work.

The second path needs no archive-wide migration. The first miss on an old,
unindexed frame still uses the old path; later misses can use verified seeking.
Pack remote proof metadata into bounded companion objects instead of creating
one tiny provider object per frame. Existing payload ciphertext stays intact.
The companion schema, discovery, recovery, and publication still require design
and versioning.

| Uniform verification unit | Sibling proof for one sector in 4 MiB | Flat leaf values plus header per frame | Same flat metadata per TiB |
|---:|---:|---:|---:|
| 4 KiB | 320 B | 32,820 B | 8,205 MiB |
| 16 KiB | 256 B | 8,244 B | 2,061 MiB |
| 64 KiB | 192 B | 2,100 B | 525 MiB |
| 256 KiB | 128 B | 564 B | 141 MiB |
| 1 MiB | 64 B | 180 B | 45 MiB |

These are serialized content sizes, excluding indices, encryption envelopes,
allocation slack, and lifetime metadata. The research code uses boxed tree
nodes for clarity, which consume more RAM than a compact flat catalog; these
numbers are not its measured heap footprint. A production budget must count
both representations while converting between them, or avoid that duplication.

Keeping one 372-byte header/proof witness for a frequently revisited 4 KiB region
is another possible tradeoff, instead of retaining every fine leaf for its frame.
Shared paths can be deduplicated, but the lookup/ownership structures also cost
space. No claim of a 939x reduction in the total cache follows from the transfer
measurement.

## Exact contract and remaining engineering work

The partial reader authenticates **selected plaintext against the requested
content root**. It does not validate unread ciphertext or the original full-frame
tag. The tests deliberately damage an unrelated sector and the old tag: a read
of a different, correctly proved region still succeeds. A full legacy decode
rejects that damaged frame. Partial success must never be recorded as whole-object
verification or complete materialization.

The trusted hash and length must originate from the correct repository,
generation, payload, and frame metadata. The network cannot choose them. Bao
specifically requires care around unverified lengths and EOF; this prototype
uses the already trusted length rather than accepting a length advertised by
the proof source. Empty-content roots are also checked. Private proof catalogs
should be encrypted; publishing finer plaintext commitments can expose more
information than publishing only a whole-frame hash.

The prototype's tree serialization is experimental and is not the Bao wire
format. Production integration needs a specified, independently checked proof
codec, or a suitable established Bao-tree implementation. It also needs bounded
catalog parsing, typed failures, and a clear `VerifiedRange` boundary so no
unverified decrypted data reaches a caller.

The next implementation should fit the existing architecture. These are the
specific integration points confirmed in the current source:

| Current location | Required change and reason |
|---|---|
| `crates/mirage-ffi/src/publisher.rs`, `RemotePayloadStore::read_inner` and `frame` | Choose whole-frame or verified-range work before spawning frame workers. Preserve sharing, bounded work, and exact assembly for requests crossing frames. |
| Same file, `fetch_frame` | Use the existing publication root/length and provider member offset; select only the covering ciphertext after a usable proof is available. The present implementation decodes and admits a complete frame. |
| Same file, `FrameCache` and `FrameFlights` | Introduce explicit range coverage and content identity. The old key promises a complete plaintext frame, so a partial result cannot reuse it. Charge proof bytes and temporary buffers as well as returned data. |
| `crates/mirage-ffi/src/read.rs`, successful remote-read branch | Replace unconditional scheduling of whole-payload restage with an admission decision based on demonstrated reuse, sequential demand, and a clean-cache budget. |
| `crates/mirage-ffi/src/write.rs`, `restage_evicted_payload` and `schedule_restage_with_lookahead` | Reserve the known payload size before fetching. The present code checks only whether the budget is already full before download, then checks the exact size and disk floor afterward. Its two-payload lookahead is enabled by ledger headroom. |
| `crates/mirage-db/src/payload_remote.rs`, `PayloadRemoteObject` | The trusted frame hashes and payload length already exist. Add derived proof discovery separately without redefining those content identities. |

For 32 MiB segments, restaging the triggering payload and two neighbors can
materialize up to 96 MiB if all are absent and admitted. This is source-derived
geometry, not a measured per-read download total. Deduplication and budgets
affect actual work. It is nevertheless a concrete reason to change read
admission alongside the representation: a 64 KiB foreground fetch alone cannot
reduce disk residency if it immediately schedules the same large restage.

Implementation sequence:

- Add an optional derived-proof catalog keyed by exact content identity and
  representation. Missing or unusable catalogs use the existing full-frame path.
- Add a range-read path before the full-frame miss path in `publisher.rs`; include
  packed-member offsets and last-frame lengths. Keep the ordinary full-frame path
  for efficient dense reads and compatibility.
- Use sector-level shared flights and byte reservations. A partial sector cannot
  be inserted into the old frame cache under a key that promises a complete frame.
- Retain verified sectors and useful proof metadata under explicit budgets.
  Allow transient full frames to serve bursts, then demote according to reuse.
- Change the post-read whole-payload restage decision. Otherwise `read.rs` can
  immediately download/materialize the payload that the new path avoided.
- Evaluate proof/data concurrency and streaming coalescing under the same global
  request and in-flight byte limits as foreground reads.
- Test a disposable mounted volume with the debug adapter rebuilt for FFI changes,
  then collect application traces. Production claims must include bootstrapping,
  metadata, request costs, exact bytes, peak allocation, and application stalls.

## Verification performed

The isolated release executable built offline with its checked-in lockfile.
Its exact-byte/rejection suite and controlled HTTP experiment completed. The
archival script checked all 32 CPU timing rows, all 56 HTTP observations,
matching output checksums, byte accounting, and source hashes. Isolated Rust
formatting and whitespace checks passed. No production Rust, adapter, or UI
source was changed in this follow-up, so full workspace/WinFsp qualification was
not run. The prototype has not been installed in the mounted read path.

The immediate result is a demonstrated way around a representation bottleneck
that appeared to require rewriting the archive. The broader hypothesis is a
cache that retains compact verification knowledge and selectively refines it,
so data eviction no longer necessarily restores a 4 MiB miss penalty.
