-- Byte-extent durability fix-ups and journal sequence allocation:
-- * dirty extents carry the offset inside their staged payload so a split
--   tail keeps pointing at the right bytes;
-- * a per-inode head row records the newest version and logical EOF even
--   when the version's extent set is empty (truncate-to-zero);
-- * the operation journal allocates device sequences through a durable
--   counter so interleaved writers can never share a sequence;
-- * remote change entries are ordered by a local sequence — provider page
--   tokens are opaque and many changes share one page cursor.
ALTER TABLE byte_extents ADD COLUMN payload_offset INTEGER
    CHECK(payload_offset IS NULL OR payload_offset >= 0);

CREATE TABLE byte_extent_heads (
    volume_id  BLOB NOT NULL CHECK(length(volume_id) = 16),
    inode      BLOB NOT NULL CHECK(length(inode) = 16),
    version    INTEGER NOT NULL CHECK(version >= 0),
    eof        INTEGER NOT NULL CHECK(eof >= 0),
    updated_ns INTEGER NOT NULL,
    PRIMARY KEY (volume_id, inode)
) STRICT;

CREATE TABLE journal_sequences (
    volume_id BLOB PRIMARY KEY NOT NULL CHECK(length(volume_id) = 16),
    next_seq  INTEGER NOT NULL CHECK(next_seq >= 0)
) STRICT;

-- Per-entry remote change log: page cursors are opaque, so entries carry a
-- device-local sequence. Replay deduplicates on (cursor, kind, payload) so a
-- repeated page never double-applies.
CREATE TABLE remote_change_entries (
    volume_id   BLOB NOT NULL CHECK(length(volume_id) = 16),
    entry_seq   INTEGER NOT NULL,
    cursor      TEXT NOT NULL,
    change_kind TEXT NOT NULL,
    payload     BLOB NOT NULL,
    observed_ns INTEGER NOT NULL,
    PRIMARY KEY (volume_id, entry_seq),
    UNIQUE (volume_id, cursor, change_kind, payload)
) STRICT;

-- Carry over any page-keyed entries recorded before this schema; ordering by
-- cursor is only for migration determinism, not semantics.
INSERT INTO remote_change_entries (volume_id, entry_seq, cursor, change_kind, payload, observed_ns)
SELECT volume_id,
       ROW_NUMBER() OVER (PARTITION BY volume_id ORDER BY cursor, observed_ns),
       cursor, change_kind, payload, observed_ns
FROM remote_changes;
