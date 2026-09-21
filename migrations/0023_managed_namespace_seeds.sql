-- Managed namespace seed markers: once a volume's namespace has been seeded
-- from a compiled mount index, the durable namespace is authoritative and the
-- seed must never be replayed (replays would resurrect deleted entries).
-- commit_hash stores the index content hash the seed was taken from; a later
-- mount with a different index hash must not reseed, so the marker both
-- suppresses and records provenance.
CREATE TABLE managed_namespace_seeds(
    volume_id   BLOB PRIMARY KEY,
    commit_hash BLOB NOT NULL CHECK(length(commit_hash) = 32),
    seeded_ns   INTEGER NOT NULL
);
