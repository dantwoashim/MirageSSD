CREATE TABLE cache_pins (
    page_hash      BLOB NOT NULL CHECK(length(page_hash) = 32),
    reason         TEXT NOT NULL CHECK(reason IN ('mandatory', 'session', 'dirty', 'recovery')),
    owner_id       BLOB NOT NULL,
    PRIMARY KEY(page_hash, reason, owner_id),
    CHECK((reason IN ('session', 'dirty') AND length(owner_id) = 16) OR (reason IN ('mandatory', 'recovery') AND length(owner_id) = 0))
) STRICT;

CREATE INDEX cache_pins_page ON cache_pins(page_hash);
