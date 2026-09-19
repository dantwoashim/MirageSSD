-- Verified working-space leases: a declared path prefix whose byte coverage
-- was proven before admission. A verified lease is durable — offline
-- readiness survives restart without re-verification — and revocation
-- drives eviction of the covered pages.
CREATE TABLE workspace_leases (
    lease_id        BLOB PRIMARY KEY NOT NULL CHECK(length(lease_id) = 16),
    volume_id       BLOB NOT NULL CHECK(length(volume_id) = 16),
    path_prefix     TEXT NOT NULL,
    bytes_declared  INTEGER NOT NULL CHECK(bytes_declared >= 0),
    bytes_verified  INTEGER NOT NULL DEFAULT 0 CHECK(bytes_verified >= 0),
    pages_pinned    INTEGER NOT NULL DEFAULT 0 CHECK(pages_pinned >= 0),
    status          TEXT NOT NULL CHECK(status IN
        ('declared', 'verifying', 'verified', 'revoked', 'expired')),
    evidence        BLOB,
    expires_ns      INTEGER,
    created_ns      INTEGER NOT NULL,
    verified_ns     INTEGER,
    UNIQUE(volume_id, path_prefix)
) STRICT;
