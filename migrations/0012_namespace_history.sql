-- Device identity and versioned namespace history: immutable checkpoints
-- plus bounded authenticated deltas with parent-commit linkage.
CREATE TABLE device_identity (
    singleton  INTEGER PRIMARY KEY NOT NULL CHECK(singleton = 1),
    device_id  BLOB NOT NULL CHECK(length(device_id) = 16),
    created_ns INTEGER NOT NULL
) STRICT;

CREATE TABLE namespace_checkpoints (
    volume_id      BLOB NOT NULL CHECK(length(volume_id) = 16),
    checkpoint_seq INTEGER NOT NULL CHECK(checkpoint_seq >= 0),
    document_hash  BLOB NOT NULL CHECK(length(document_hash) = 32),
    document       BLOB NOT NULL,
    parent_commit  BLOB CHECK(parent_commit IS NULL OR length(parent_commit) = 32),
    entry_count    INTEGER NOT NULL CHECK(entry_count >= 0),
    created_ns     INTEGER NOT NULL,
    PRIMARY KEY (volume_id, checkpoint_seq)
) STRICT;

CREATE TABLE namespace_deltas (
    volume_id      BLOB NOT NULL CHECK(length(volume_id) = 16),
    checkpoint_seq INTEGER NOT NULL CHECK(checkpoint_seq >= 0),
    delta_seq      INTEGER NOT NULL CHECK(delta_seq >= 0),
    op             TEXT NOT NULL CHECK(op IN ('create', 'rename', 'delete', 'set_roots')),
    payload_hash   BLOB NOT NULL CHECK(length(payload_hash) = 32),
    payload        BLOB NOT NULL CHECK(length(payload) <= 65536),
    created_ns     INTEGER NOT NULL,
    PRIMARY KEY (volume_id, checkpoint_seq, delta_seq)
) STRICT;
