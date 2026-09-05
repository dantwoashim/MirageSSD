CREATE TABLE service_metadata (
    key             TEXT PRIMARY KEY NOT NULL CHECK(length(key) BETWEEN 1 AND 128),
    value           BLOB NOT NULL CHECK(length(value) <= 65536),
    updated_at_ns   INTEGER NOT NULL
) STRICT, WITHOUT ROWID;
