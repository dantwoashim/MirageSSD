CREATE TABLE repositories (
    repository_id       BLOB PRIMARY KEY NOT NULL CHECK(length(repository_id) = 16),
    display_name        TEXT NOT NULL CHECK(length(display_name) BETWEEN 1 AND 256),
    local_root          TEXT NOT NULL CHECK(length(local_root) BETWEEN 1 AND 32767),
    active_generation   INTEGER,
    active_commit_hash  BLOB CHECK(active_commit_hash IS NULL OR length(active_commit_hash) = 32),
    state               TEXT NOT NULL CHECK(length(state) BETWEEN 1 AND 64),
    created_at_ns       INTEGER NOT NULL,
    updated_at_ns       INTEGER NOT NULL,
    CHECK((active_generation IS NULL) = (active_commit_hash IS NULL)),
    FOREIGN KEY(repository_id, active_generation, active_commit_hash)
        REFERENCES generations(repository_id, generation, commit_hash)
        DEFERRABLE INITIALLY DEFERRED
) STRICT, WITHOUT ROWID;

CREATE TABLE generations (
    repository_id       BLOB NOT NULL CHECK(length(repository_id) = 16),
    generation          INTEGER NOT NULL CHECK(generation >= 0),
    commit_hash         BLOB NOT NULL CHECK(length(commit_hash) = 32),
    manifest_hash       BLOB NOT NULL CHECK(length(manifest_hash) = 32),
    manifest_local_path TEXT NOT NULL CHECK(length(manifest_local_path) BETWEEN 1 AND 32767),
    mount_index_path    TEXT CHECK(mount_index_path IS NULL OR length(mount_index_path) BETWEEN 1 AND 32767),
    verified            INTEGER NOT NULL CHECK(verified IN (0, 1)),
    created_at_ns       INTEGER NOT NULL,
    PRIMARY KEY(repository_id, generation),
    UNIQUE(repository_id, generation, commit_hash),
    FOREIGN KEY(repository_id) REFERENCES repositories(repository_id) ON DELETE RESTRICT
) STRICT, WITHOUT ROWID;
