ALTER TABLE repositories
    ADD COLUMN volume_mode TEXT NOT NULL DEFAULT 'legacy'
    CHECK(volume_mode IN ('legacy', 'managed'));
