-- Remote observation state: the newest remote head this device has seen,
-- the durable changes cursor, and divergence records. A diverged volume
-- keeps both histories visible — local operations and remote commits —
-- rather than overwriting either.
CREATE TABLE remote_heads (
    volume_id    BLOB PRIMARY KEY NOT NULL CHECK(length(volume_id) = 16),
    head_commit  BLOB NOT NULL CHECK(length(head_commit) = 32),
    head_seq     INTEGER NOT NULL CHECK(head_seq >= 0),
    changes_cursor TEXT NOT NULL,
    observed_ns  INTEGER NOT NULL
) STRICT;

CREATE TABLE remote_changes (
    volume_id    BLOB NOT NULL CHECK(length(volume_id) = 16),
    cursor       TEXT NOT NULL,
    change_kind  TEXT NOT NULL,
    payload      BLOB NOT NULL,
    observed_ns  INTEGER NOT NULL,
    PRIMARY KEY(volume_id, cursor)
) STRICT;

CREATE TABLE divergence_state (
    volume_id     BLOB PRIMARY KEY NOT NULL CHECK(length(volume_id) = 16),
    base_commit   BLOB CHECK(base_commit IS NULL OR length(base_commit) = 32),
    local_head    BLOB NOT NULL CHECK(length(local_head) = 32),
    remote_head   BLOB NOT NULL CHECK(length(remote_head) = 32),
    status        TEXT NOT NULL CHECK(status IN
        ('diverged', 'reconciled', 'abandoned')),
    detected_ns   INTEGER NOT NULL,
    resolved_ns   INTEGER
) STRICT;
