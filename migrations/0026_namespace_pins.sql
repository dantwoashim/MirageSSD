-- Folder/file pins: a pinned inode (and everything under a pinned
-- directory) is never evicted by disk-floor or budget-pressure reclaim.
CREATE TABLE namespace_pins(
    volume_id BLOB NOT NULL,
    inode     BLOB PRIMARY KEY NOT NULL,
    pinned_ns INTEGER NOT NULL
) STRICT;
CREATE INDEX namespace_pins_volume ON namespace_pins(volume_id);
