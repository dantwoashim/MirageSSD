-- Managed-volume payload publication ledger: each committed journal payload
-- may be published as one immutable encrypted Drive object; the row records
-- the remote identity and the verification material needed to fetch it back.
CREATE TABLE payload_remote_objects(
    volume_id          BLOB NOT NULL CHECK(length(volume_id) = 16),
    payload_id         BLOB PRIMARY KEY NOT NULL CHECK(length(payload_id) = 16),
    provider_object_id TEXT NOT NULL,
    immutable_revision TEXT,
    object_length      INTEGER NOT NULL CHECK(object_length > 0),
    object_hash        BLOB NOT NULL CHECK(length(object_hash) = 32),
    plaintext_length   INTEGER NOT NULL CHECK(plaintext_length >= 0),
    plaintext_hash     BLOB NOT NULL CHECK(length(plaintext_hash) = 32),
    frame_hashes       BLOB NOT NULL CHECK(length(frame_hashes) % 32 = 0),
    published_ns       INTEGER NOT NULL
);
CREATE INDEX payload_remote_objects_volume ON payload_remote_objects(volume_id, published_ns);
