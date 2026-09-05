# MirageSSD Error Catalog and Taxonomy Specification

This specification defines the canonical error taxonomy, stable machine-readable error codes, and retry semantics for all MirageSSD subsystems.

## Invariants

1. **No Ad Hoc Strings**: Subsystems and downstream crates must never invent untyped error strings or new uncataloged machine error codes.
2. **Safe Public Envelopes**: Diagnostic error source chains and file system paths containing potential secrets/tokens are stripped before serialization into public envelopes.
3. **Explicit Retry Semantics**: Every failure explicitly categorizes retry behavior using `RetryDisposition`. I/O failures are never blindly treated as retryable.

## Canonical Error Kinds and Codes

| Error Kind | Stable Machine Code | Default Retry | Description |
| :--- | :--- | :--- | :--- |
| `InvalidArgument` | `MIRAGE_INVALID_ARGUMENT` | `never` | Malformed parameters, invalid bounds, or unparseable non-canonical inputs. |
| `UnsupportedLayout` | `MIRAGE_UNSUPPORTED_LAYOUT` | `never` | Unsupported manifest layout version, compression codec, or archive format. |
| `ManifestInvalid` | `MIRAGE_MANIFEST_INVALID` | `never` | Manifest file corruption, corrupted header, or unreadable index table. |
| `IntegrityMismatch` | `MIRAGE_INTEGRITY_MISMATCH` | `never` | BLAKE3 checksum, cryptographic hash, or byte length mismatch. |
| `CacheFull` | `MIRAGE_CACHE_FULL` | `user_action` | Fixed-slot SSD cache arena exhausted without evictable slots. |
| `BackendUnauthenticated` | `MIRAGE_BACKEND_UNAUTHENTICATED` | `user_action` | Expired, invalid, or missing remote storage credentials / tokens. |
| `BackendPermissionDenied` | `MIRAGE_BACKEND_PERMISSION_DENIED` | `user_action` | Valid identity lacks permission for the requested backend operation. |
| `BackendRateLimited` | `MIRAGE_BACKEND_RATE_LIMITED` | `backoff` | Cloud provider quota exhaustion or HTTP 429 response. |
| `BackendUnavailable` | `MIRAGE_BACKEND_UNAVAILABLE` | `backoff` | Temporary network partition, cloud outage, or gateway timeout. |
| `RemoteObjectMissing` | `MIRAGE_REMOTE_OBJECT_MISSING` | `never` | Requested remote pack, object, manifest, or commit not found. |
| `RepositoryConflict` | `MIRAGE_REPOSITORY_CONFLICT` | `user_action` | Divergent repository branch or concurrent writer detected. |
| `UpdateActive` | `MIRAGE_UPDATE_ACTIVE` | `backoff` | Operation blocked by an active update transaction or staging lock. |
| `ProviderUnavailable` | `MIRAGE_PROVIDER_UNAVAILABLE` | `user_action` | Cloud provider or driver backend is unconfigured or unavailable. |
| `Io` | `MIRAGE_IO_ERROR` | `never` | Underlying OS/NTFS file system error (specific transient errors may retry). |
| `Cancelled` | `MIRAGE_CANCELLED` | `never` | Caller explicitly cancelled this operation; the expired request itself is not retried. |
| `DeadlineExceeded` | `MIRAGE_DEADLINE_EXCEEDED` | `never` | Caller deadline elapsed; bounded filesystem requests never retry forever. |
| `NotImplemented` | `MIRAGE_NOT_IMPLEMENTED` | `never` | Stable command or capability exists but has not reached its implementation milestone. |
| `InternalInvariant` | `MIRAGE_INTERNAL_INVARIANT` | `never` | Internal assertion or logic invariant failure. |

## Retry Dispositions

- `Never`: Non-retryable error; repeated attempts will fail identically.
- `Immediate`: Transient lock contention or interrupted syscall; retry immediately without backoff.
- `Backoff`: Transient network backpressure or rate limit; retry with exponential backoff and jitter.
- `After(Duration)`: Explicit cooldown specified by provider. It serializes as
  `{"after":{"seconds":1,"nanoseconds":500000000}}`; this stable shape never exposes the
  implementation fields of `std::time::Duration`.
- `UserAction`: Requires operator/user resolution (e.g. authentication prompt, freeing disk space) before re-attempting.

## Public Serialization Envelope

Public wire responses serialize as:

```json
{
  "code": "MIRAGE_INTEGRITY_MISMATCH",
  "kind": "IntegrityMismatch",
  "message": "BLAKE3 hash mismatch for page #42",
  "retry": "never"
}
```

The underlying diagnostic cause chain (`source`) is strictly omitted from public serialized representations.
