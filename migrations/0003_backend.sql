CREATE TABLE backend_accounts (
    backend_id           TEXT PRIMARY KEY NOT NULL CHECK(length(backend_id) BETWEEN 1 AND 64),
    provider             TEXT NOT NULL CHECK(length(provider) BETWEEN 1 AND 64),
    account_subject_hash BLOB NOT NULL CHECK(length(account_subject_hash) = 32),
    state                TEXT NOT NULL CHECK(length(state) BETWEEN 1 AND 64),
    updated_at_ns        INTEGER NOT NULL
) STRICT, WITHOUT ROWID;

CREATE TABLE remote_objects (
    backend_id          TEXT NOT NULL,
    object_key         BLOB NOT NULL CHECK(length(object_key) = 32),
    provider_file_id   TEXT NOT NULL CHECK(length(provider_file_id) BETWEEN 1 AND 1024),
    provider_revision  TEXT CHECK(provider_revision IS NULL OR length(provider_revision) BETWEEN 1 AND 512),
    object_kind        INTEGER NOT NULL CHECK(object_kind BETWEEN 0 AND 4),
    byte_length        INTEGER NOT NULL CHECK(byte_length > 0),
    content_hash       BLOB NOT NULL CHECK(length(content_hash) = 32),
    state              TEXT NOT NULL CHECK(length(state) BETWEEN 1 AND 64),
    created_at_ns      INTEGER NOT NULL,
    PRIMARY KEY(backend_id, object_key),
    FOREIGN KEY(backend_id) REFERENCES backend_accounts(backend_id) ON DELETE RESTRICT
) STRICT, WITHOUT ROWID;

CREATE INDEX remote_objects_content_hash ON remote_objects(backend_id, content_hash, object_kind);
CREATE UNIQUE INDEX remote_objects_provider_identity
    ON remote_objects(backend_id, provider_file_id, provider_revision);

CREATE TABLE upload_sessions (
    backend_id          TEXT NOT NULL,
    upload_id           BLOB NOT NULL CHECK(length(upload_id) = 16),
    object_key          BLOB NOT NULL CHECK(length(object_key) = 32),
    provider_session_id TEXT NOT NULL CHECK(length(provider_session_id) BETWEEN 1 AND 1024),
    committed_offset    INTEGER NOT NULL CHECK(committed_offset >= 0),
    total_length        INTEGER NOT NULL CHECK(total_length > 0),
    state               TEXT NOT NULL CHECK(length(state) BETWEEN 1 AND 64),
    updated_at_ns       INTEGER NOT NULL,
    PRIMARY KEY(backend_id, upload_id),
    FOREIGN KEY(backend_id) REFERENCES backend_accounts(backend_id) ON DELETE RESTRICT,
    CHECK(committed_offset <= total_length)
) STRICT, WITHOUT ROWID;

CREATE INDEX upload_sessions_object_key ON upload_sessions(backend_id, object_key);
