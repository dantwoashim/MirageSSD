-- Variable-length physical files: the managed journal file records
-- variable-size payload extents, so extent_bytes may be zero. Rebuilds
-- physical_files with a relaxed check; extent rows keep referencing it by
-- file_id.
CREATE TABLE physical_files_v2 (
    file_id        BLOB PRIMARY KEY NOT NULL CHECK(length(file_id) = 16),
    path           TEXT NOT NULL UNIQUE,
    zone           INTEGER NOT NULL,
    extent_bytes   INTEGER NOT NULL CHECK(extent_bytes >= 0),
    extent_count   INTEGER NOT NULL CHECK(extent_count >= 0),
    created_ns     INTEGER NOT NULL
) STRICT;

INSERT INTO physical_files_v2 (file_id, path, zone, extent_bytes, extent_count, created_ns)
SELECT file_id, path, zone, extent_bytes, extent_count, created_ns FROM physical_files;

DROP TABLE physical_files;
ALTER TABLE physical_files_v2 RENAME TO physical_files;
