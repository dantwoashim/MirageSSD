CREATE TABLE update_journals (
    update_id           BLOB PRIMARY KEY NOT NULL CHECK(length(update_id) = 16),
    repository_id       BLOB NOT NULL CHECK(length(repository_id) = 16),
    base_generation     INTEGER NOT NULL CHECK(base_generation >= 0),
    target_generation   INTEGER NOT NULL CHECK(target_generation > base_generation),
    state               TEXT NOT NULL CHECK(length(state) BETWEEN 1 AND 64),
    journal_path        TEXT NOT NULL CHECK(length(journal_path) BETWEEN 1 AND 32767),
    created_at_ns       INTEGER NOT NULL,
    updated_at_ns       INTEGER NOT NULL,
    FOREIGN KEY(repository_id, base_generation) REFERENCES generations(repository_id, generation)
) STRICT, WITHOUT ROWID;

CREATE UNIQUE INDEX one_nonterminal_update_per_repository
ON update_journals(repository_id)
WHERE state NOT IN ('committed', 'rolled_back');

CREATE TABLE overlay_pages (
    update_id           BLOB NOT NULL CHECK(length(update_id) = 16),
    file_id             INTEGER NOT NULL CHECK(file_id >= 0),
    page_index          INTEGER NOT NULL CHECK(page_index >= 0),
    state               TEXT NOT NULL CHECK(length(state) BETWEEN 1 AND 64),
    arena_id            INTEGER,
    slot_index          INTEGER,
    page_hash           BLOB CHECK(page_hash IS NULL OR length(page_hash) = 32),
    staging_object_key  BLOB CHECK(staging_object_key IS NULL OR length(staging_object_key) = 32),
    staging_offset      INTEGER,
    encoded_length      INTEGER,
    PRIMARY KEY(update_id, file_id, page_index),
    FOREIGN KEY(update_id) REFERENCES update_journals(update_id) ON DELETE CASCADE,
    CHECK((arena_id IS NULL) = (slot_index IS NULL)),
    CHECK(staging_offset IS NULL OR staging_offset >= 0),
    CHECK(encoded_length IS NULL OR encoded_length > 0)
) STRICT, WITHOUT ROWID;

CREATE TABLE native_snapshots (
    update_id       BLOB NOT NULL CHECK(length(update_id) = 16),
    relative_path   TEXT NOT NULL CHECK(length(relative_path) BETWEEN 1 AND 32767),
    snapshot_path   TEXT NOT NULL CHECK(length(snapshot_path) BETWEEN 1 AND 32767),
    byte_length     INTEGER NOT NULL CHECK(byte_length >= 0),
    content_hash    BLOB NOT NULL CHECK(length(content_hash) = 32),
    PRIMARY KEY(update_id, relative_path),
    FOREIGN KEY(update_id) REFERENCES update_journals(update_id) ON DELETE CASCADE
) STRICT, WITHOUT ROWID;

CREATE TABLE journal_events (
    update_id       BLOB NOT NULL CHECK(length(update_id) = 16),
    sequence        INTEGER NOT NULL CHECK(sequence >= 0),
    event_kind      TEXT NOT NULL CHECK(length(event_kind) BETWEEN 1 AND 64),
    details         TEXT NOT NULL CHECK(length(details) <= 4096),
    created_at_ns   INTEGER NOT NULL,
    PRIMARY KEY(update_id, sequence),
    FOREIGN KEY(update_id) REFERENCES update_journals(update_id) ON DELETE CASCADE
) STRICT, WITHOUT ROWID;
