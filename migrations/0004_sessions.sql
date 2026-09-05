CREATE TABLE sessions (
    session_id             BLOB PRIMARY KEY NOT NULL CHECK(length(session_id) = 16),
    repository_id          BLOB NOT NULL CHECK(length(repository_id) = 16),
    generation             INTEGER NOT NULL CHECK(generation >= 0),
    mode                   TEXT NOT NULL CHECK(mode IN ('sealed', 'balanced')),
    capsule_id             BLOB CHECK(capsule_id IS NULL OR length(capsule_id) = 16),
    state                  TEXT NOT NULL CHECK(length(state) BETWEEN 1 AND 64),
    expected_lease_count   INTEGER NOT NULL CHECK(expected_lease_count >= 0),
    started_at_ns          INTEGER NOT NULL,
    ended_at_ns            INTEGER,
    seal_violation_count   INTEGER NOT NULL DEFAULT 0 CHECK(seal_violation_count >= 0),
    first_violation_summary TEXT CHECK(first_violation_summary IS NULL OR length(first_violation_summary) <= 1024),
    FOREIGN KEY(repository_id, generation) REFERENCES generations(repository_id, generation)
) STRICT, WITHOUT ROWID;

CREATE TABLE session_processes (
    session_id      BLOB NOT NULL CHECK(length(session_id) = 16),
    process_id      INTEGER NOT NULL CHECK(process_id BETWEEN 1 AND 4294967295),
    started_at_ns   INTEGER NOT NULL,
    ended_at_ns     INTEGER,
    PRIMARY KEY(session_id, process_id),
    FOREIGN KEY(session_id) REFERENCES sessions(session_id) ON DELETE CASCADE
) STRICT, WITHOUT ROWID;

CREATE TABLE session_leases (
    session_id      BLOB NOT NULL CHECK(length(session_id) = 16),
    page_hash       BLOB NOT NULL CHECK(length(page_hash) = 32),
    reason          TEXT NOT NULL CHECK(length(reason) BETWEEN 1 AND 64),
    PRIMARY KEY(session_id, page_hash),
    FOREIGN KEY(session_id) REFERENCES sessions(session_id) ON DELETE CASCADE
) STRICT, WITHOUT ROWID;

CREATE TRIGGER sessions_sealed_ready_update_guard
BEFORE UPDATE OF state ON sessions
WHEN NEW.state = 'sealed_ready'
BEGIN
    SELECT CASE WHEN (
        SELECT count(*) FROM session_leases WHERE session_id = NEW.session_id
    ) != NEW.expected_lease_count THEN RAISE(ABORT, 'incomplete sealed session lease set') END;
END;

CREATE INDEX sessions_repository_state ON sessions(repository_id, state);
