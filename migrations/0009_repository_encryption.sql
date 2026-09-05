ALTER TABLE repositories
    ADD COLUMN content_encrypted INTEGER NOT NULL DEFAULT 0
    CHECK(content_encrypted IN (0, 1));
