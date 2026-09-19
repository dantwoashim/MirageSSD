-- Durable remote publication sessions. A session row exists before the
-- first byte is sent, records the resumable-upload cursor (upload id,
-- session URI, chunk offset, committed bytes), the ordered next operations,
-- and the last classified error — so recovery can resume or roll back a
-- publication without re-sending confirmed bytes.
CREATE TABLE publication_sessions (
    session_id       BLOB PRIMARY KEY NOT NULL CHECK(length(session_id) = 16),
    volume_id        BLOB NOT NULL CHECK(length(volume_id) = 16),
    kind             TEXT NOT NULL CHECK(kind IN
        ('pack', 'manifest', 'commit', 'tombstone')),
    object_key       TEXT NOT NULL,
    content_hash     BLOB CHECK(content_hash IS NULL OR length(content_hash) = 32),
    phase            TEXT NOT NULL CHECK(phase IN
        ('created', 'initiated', 'uploading', 'uploaded', 'committed',
         'aborted', 'done')),
    remote_upload_id TEXT,
    session_uri      TEXT,
    chunk_offset     INTEGER NOT NULL DEFAULT 0 CHECK(chunk_offset >= 0),
    committed_bytes  INTEGER NOT NULL DEFAULT 0 CHECK(committed_bytes >= 0),
    total_bytes      INTEGER CHECK(total_bytes IS NULL OR total_bytes >= 0),
    next_ops         BLOB,
    error_class      TEXT,
    attempts         INTEGER NOT NULL DEFAULT 0 CHECK(attempts >= 0),
    created_ns       INTEGER NOT NULL,
    updated_ns       INTEGER NOT NULL
) STRICT;

-- Recovery scans unfinished sessions per volume in creation order.
CREATE INDEX publication_sessions_open
ON publication_sessions(volume_id, created_ns)
WHERE phase NOT IN ('committed', 'aborted', 'done');
