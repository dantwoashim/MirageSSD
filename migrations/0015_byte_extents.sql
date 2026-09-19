-- Versioned byte extents per inode. Base extents reference immutable pack
-- content, dirty extents reference journal payloads, and zero extents carry
-- no bytes at all. Each mutation bumps the version so readers can pin a
-- snapshot while writers keep appending.
CREATE TABLE byte_extents (
    extent_id   BLOB PRIMARY KEY NOT NULL CHECK(length(extent_id) = 16),
    volume_id   BLOB NOT NULL CHECK(length(volume_id) = 16),
    inode       BLOB NOT NULL CHECK(length(inode) = 16),
    version     INTEGER NOT NULL CHECK(version >= 0),
    start       INTEGER NOT NULL CHECK(start >= 0),
    length      INTEGER NOT NULL CHECK(length > 0),
    kind        TEXT NOT NULL CHECK(kind IN ('base', 'dirty', 'zero')),
    page_hash   BLOB CHECK(page_hash IS NULL OR length(page_hash) = 32),
    base_offset INTEGER CHECK(base_offset IS NULL OR base_offset >= 0),
    payload_id  BLOB CHECK(payload_id IS NULL OR length(payload_id) = 16),
    created_ns  INTEGER NOT NULL,
    CHECK(
        (kind = 'base'  AND page_hash IS NOT NULL AND base_offset IS NOT NULL AND payload_id IS NULL)
     OR (kind = 'dirty' AND payload_id IS NOT NULL AND page_hash IS NULL AND base_offset IS NULL)
     OR (kind = 'zero'  AND page_hash IS NULL AND base_offset IS NULL AND payload_id IS NULL)
    )
) STRICT;

CREATE INDEX byte_extents_range
ON byte_extents(volume_id, inode, version, start);
