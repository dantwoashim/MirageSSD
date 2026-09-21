-- Physical allocation envelope: arena files, extents, and durable
-- reservations. A reservation must be durable before any bytes are written;
-- page_state may only claim residency once the extent is committed alive.
CREATE TABLE physical_files (
    file_id        BLOB PRIMARY KEY NOT NULL CHECK(length(file_id) = 16),
    path           TEXT NOT NULL UNIQUE,
    zone           INTEGER NOT NULL,
    extent_bytes   INTEGER NOT NULL CHECK(extent_bytes > 0),
    extent_count   INTEGER NOT NULL CHECK(extent_count >= 0),
    created_ns     INTEGER NOT NULL
) STRICT;

CREATE TABLE physical_extents (
    extent_id    BLOB PRIMARY KEY NOT NULL CHECK(length(extent_id) = 16),
    file_id      BLOB NOT NULL REFERENCES physical_files(file_id) ON DELETE RESTRICT,
    slot_index   INTEGER NOT NULL CHECK(slot_index >= 0),
    length_bytes INTEGER NOT NULL CHECK(length_bytes > 0),
    state        TEXT NOT NULL CHECK(state IN ('reserved', 'alive', 'dead', 'evicting')),
    page_hash    BLOB CHECK(page_hash IS NULL OR length(page_hash) = 32),
    checksum     BLOB CHECK(checksum IS NULL OR length(checksum) = 32),
    pin_count    INTEGER NOT NULL DEFAULT 0 CHECK(pin_count >= 0),
    generation   INTEGER NOT NULL CHECK(generation >= 0),
    updated_ns   INTEGER NOT NULL,
    UNIQUE(file_id, slot_index)
) STRICT;

CREATE INDEX physical_extents_state
ON physical_extents(state, updated_ns);

CREATE TABLE physical_reservations (
    extent_id   BLOB PRIMARY KEY NOT NULL REFERENCES physical_extents(extent_id) ON DELETE RESTRICT,
    owner_epoch BLOB NOT NULL CHECK(length(owner_epoch) = 16),
    created_ns  INTEGER NOT NULL,
    expires_ns  INTEGER NOT NULL CHECK(expires_ns > created_ns)
) STRICT;
