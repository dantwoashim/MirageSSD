ALTER TABLE inodes ADD COLUMN unlink_pending INTEGER NOT NULL DEFAULT 0 CHECK(unlink_pending IN (0, 1));
CREATE INDEX pending_inode_unlinks ON inodes(volume_id, unlink_pending) WHERE unlink_pending = 1;
