ALTER TABLE repositories
    ADD COLUMN owner_sid TEXT NOT NULL DEFAULT 'S-1-5-18'
    CHECK(length(owner_sid) BETWEEN 5 AND 256);

CREATE INDEX repositories_owner_sid ON repositories(owner_sid);
