CREATE TABLE space_leases (
    lease_id                 BLOB PRIMARY KEY NOT NULL CHECK(length(lease_id) = 16),
    repository_id            BLOB NOT NULL REFERENCES repositories(repository_id) ON DELETE RESTRICT,
    target_volume_id         TEXT NOT NULL CHECK(length(target_volume_id) BETWEEN 1 AND 256),
    requested_bytes          INTEGER NOT NULL CHECK(requested_bytes > 0),
    planned_reclaim_bytes    INTEGER NOT NULL CHECK(planned_reclaim_bytes >= 0),
    state                    TEXT NOT NULL CHECK(state IN ('preparing', 'ready', 'consumed', 'released', 'failed')),
    created_at_ns            INTEGER NOT NULL,
    updated_at_ns            INTEGER NOT NULL,
    expires_at_ns            INTEGER NOT NULL,
    CHECK(updated_at_ns >= created_at_ns),
    CHECK(expires_at_ns > created_at_ns)
) STRICT;

CREATE INDEX space_leases_volume_state_expiry
ON space_leases(target_volume_id, state, expires_at_ns);

CREATE INDEX space_leases_repository_state
ON space_leases(repository_id, state);
