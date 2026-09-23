-- Per-repository local cache placement: the directory (on the user's chosen
-- disk) that holds the managed volume's journal payloads. Repositories
-- without a row keep the historical default <state root>\journal.
CREATE TABLE repository_cache_roots(
    repository_id BLOB PRIMARY KEY NOT NULL CHECK(length(repository_id) = 16)
        REFERENCES repositories(repository_id) ON DELETE CASCADE,
    cache_root    TEXT NOT NULL CHECK(length(cache_root) BETWEEN 3 AND 32767),
    updated_ns    INTEGER NOT NULL
) STRICT;
