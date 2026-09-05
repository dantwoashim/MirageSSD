CREATE TABLE cache_shards (
    shard_id          INTEGER PRIMARY KEY NOT NULL,
    relative_path     TEXT NOT NULL UNIQUE,
    page_size         INTEGER NOT NULL CHECK(page_size > 0),
    slot_count        INTEGER NOT NULL CHECK(slot_count > 0),
    format_version    INTEGER NOT NULL CHECK(format_version = 1)
) STRICT;

CREATE TABLE cache_slots (
    shard_id          INTEGER NOT NULL REFERENCES cache_shards(shard_id) ON DELETE RESTRICT,
    slot_index        INTEGER NOT NULL CHECK(slot_index >= 0),
    generation        INTEGER NOT NULL CHECK(generation >= 0),
    state             INTEGER NOT NULL CHECK(state BETWEEN 0 AND 4),
    page_hash         BLOB CHECK(page_hash IS NULL OR length(page_hash) = 32),
    logical_length    INTEGER NOT NULL CHECK(logical_length >= 0),
    PRIMARY KEY(shard_id, slot_index),
    CHECK((state = 0 AND page_hash IS NULL) OR (state IN (1, 2, 3, 4) AND page_hash IS NOT NULL))
) STRICT;

CREATE UNIQUE INDEX cache_slots_resident_hash
ON cache_slots(page_hash)
WHERE state = 2;
