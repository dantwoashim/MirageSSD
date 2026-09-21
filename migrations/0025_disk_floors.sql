-- Per-disk free-space floors: a floor keeps at least floor_bytes free on a
-- volume root (e.g. 'D:\'); hysteresis prevents thrash at the boundary.
CREATE TABLE disk_floors(
    volume_root      TEXT PRIMARY KEY NOT NULL,
    floor_bytes      INTEGER NOT NULL CHECK(floor_bytes > 0),
    hysteresis_bytes INTEGER NOT NULL CHECK(hysteresis_bytes >= 0),
    updated_ns       INTEGER NOT NULL
) STRICT;

-- Bounded audit trail of reclaim passes; kept to the newest 100 per volume.
CREATE TABLE disk_floor_runs(
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    volume_root  TEXT NOT NULL,
    at_ns        INTEGER NOT NULL,
    target_bytes INTEGER NOT NULL CHECK(target_bytes >= 0),
    freed_bytes  INTEGER NOT NULL CHECK(freed_bytes >= 0),
    outcome      TEXT NOT NULL
) STRICT;
CREATE INDEX disk_floor_runs_volume ON disk_floor_runs(volume_root, at_ns);
