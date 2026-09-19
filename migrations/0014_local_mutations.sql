-- Durable local mutation journal: every offline-capable namespace or data
-- mutation is an operation row with a device-local sequence, its base commit,
-- payload references, publication status, and flush-group membership. Data
-- payloads carry a checksum and a durable-flush marker; an operation cannot
-- reach 'committed' until every referenced payload is flushed, so a committed
-- metadata reference never claims undurable bytes.
CREATE TABLE local_operations (
    operation_id BLOB PRIMARY KEY NOT NULL CHECK(length(operation_id) = 16),
    device_seq   INTEGER NOT NULL,
    volume_id    BLOB NOT NULL CHECK(length(volume_id) = 16),
    base_commit  BLOB CHECK(base_commit IS NULL OR length(base_commit) = 32),
    kind         TEXT NOT NULL CHECK(kind IN
        ('create', 'rename', 'delete', 'write', 'truncate', 'mkdir', 'replace')),
    payload      BLOB NOT NULL,
    status       TEXT NOT NULL CHECK(status IN
        ('pending', 'committed', 'flushed', 'published', 'reclaimed')),
    flush_group  INTEGER REFERENCES flush_groups(group_id) ON DELETE RESTRICT,
    depends_on   BLOB CHECK(depends_on IS NULL OR length(depends_on) = 16),
    created_ns   INTEGER NOT NULL,
    UNIQUE(volume_id, device_seq)
) STRICT;

CREATE INDEX local_operations_status
ON local_operations(status, device_seq);

CREATE TABLE operation_payloads (
    payload_id   BLOB PRIMARY KEY NOT NULL CHECK(length(payload_id) = 16),
    operation_id BLOB NOT NULL REFERENCES local_operations(operation_id) ON DELETE RESTRICT,
    path         TEXT NOT NULL,
    bytes        INTEGER NOT NULL CHECK(bytes >= 0),
    checksum     BLOB CHECK(checksum IS NULL OR length(checksum) = 32),
    flushed_ns   INTEGER
) STRICT;

CREATE TABLE flush_groups (
    group_id     INTEGER PRIMARY KEY NOT NULL,
    volume_id    BLOB NOT NULL CHECK(length(volume_id) = 16),
    opened_ns    INTEGER NOT NULL,
    flushed_ns   INTEGER
) STRICT;

-- Unflushed payloads must not outlive their operation.
CREATE INDEX operation_payloads_pending
ON operation_payloads(operation_id)
WHERE flushed_ns IS NULL;
