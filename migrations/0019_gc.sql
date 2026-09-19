-- Bounded-history and garbage-collection bookkeeping. Retention bounds say
-- how much history each stream keeps; gc_runs record what each sweep
-- reclaimed and the boundary it honored; unreachable_candidates list remote
-- objects proven unreachable — physical removal is only legal after a newer
-- verified commit lands and the reclamation window has passed.
CREATE TABLE gc_bounds (
    volume_id    BLOB NOT NULL CHECK(length(volume_id) = 16),
    kind         TEXT NOT NULL CHECK(kind IN
        ('namespace_history', 'journal', 'pending_uploads', 'remote_objects')),
    retain_from  INTEGER NOT NULL,
    min_keep     INTEGER NOT NULL CHECK(min_keep >= 0),
    set_ns       INTEGER NOT NULL,
    PRIMARY KEY(volume_id, kind)
) STRICT;

CREATE TABLE gc_runs (
    run_id           BLOB PRIMARY KEY NOT NULL CHECK(length(run_id) = 16),
    volume_id        BLOB NOT NULL CHECK(length(volume_id) = 16),
    kind             TEXT NOT NULL,
    boundary         INTEGER NOT NULL,
    reclaimed_count  INTEGER NOT NULL DEFAULT 0,
    started_ns       INTEGER NOT NULL,
    completed_ns     INTEGER
) STRICT;

CREATE TABLE unreachable_candidates (
    volume_id     BLOB NOT NULL CHECK(length(volume_id) = 16),
    object_key    TEXT NOT NULL,
    -- The verified commit that made this object unreachable; physical
    -- removal is only legal once a later verified commit exists and the
    -- reclamation window has elapsed.
    unreachable_at_commit BLOB NOT NULL CHECK(length(unreachable_at_commit) = 32),
    detected_ns   INTEGER NOT NULL,
    reclaim_after_ns INTEGER NOT NULL,
    PRIMARY KEY(volume_id, object_key)
) STRICT;
