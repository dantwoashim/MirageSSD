-- Durable mutable namespace: 128-bit inode identities independent of names.
CREATE TABLE namespace_volumes (
    volume_id      BLOB PRIMARY KEY NOT NULL CHECK(length(volume_id) = 16),
    naming_version INTEGER NOT NULL CHECK(naming_version >= 1),
    root_inode     BLOB NOT NULL CHECK(length(root_inode) = 16),
    next_sequence  INTEGER NOT NULL DEFAULT 1 CHECK(next_sequence >= 1),
    created_ns     INTEGER NOT NULL
) STRICT;

CREATE TABLE inodes (
    volume_id    BLOB NOT NULL CHECK(length(volume_id) = 16),
    inode        BLOB NOT NULL CHECK(length(inode) = 16),
    kind         TEXT NOT NULL CHECK(kind IN ('directory', 'file')),
    size         INTEGER NOT NULL CHECK(size >= 0),
    version_root BLOB CHECK(version_root IS NULL OR length(version_root) = 32),
    extent_root  BLOB CHECK(extent_root IS NULL OR length(extent_root) = 32),
    created_ns   INTEGER NOT NULL,
    modified_ns  INTEGER NOT NULL,
    PRIMARY KEY (volume_id, inode),
    FOREIGN KEY (volume_id) REFERENCES namespace_volumes(volume_id) ON DELETE RESTRICT
) STRICT;

CREATE TABLE dirents (
    volume_id    BLOB NOT NULL CHECK(length(volume_id) = 16),
    parent_inode BLOB NOT NULL CHECK(length(parent_inode) = 16),
    folded_name  TEXT NOT NULL,
    display_name TEXT NOT NULL,
    child_inode  BLOB NOT NULL CHECK(length(child_inode) = 16),
    PRIMARY KEY (volume_id, parent_inode, folded_name),
    FOREIGN KEY (volume_id, parent_inode)
        REFERENCES inodes(volume_id, inode) ON DELETE RESTRICT,
    FOREIGN KEY (volume_id, child_inode)
        REFERENCES inodes(volume_id, inode) ON DELETE RESTRICT
) STRICT;

CREATE INDEX dirents_by_child ON dirents(volume_id, child_inode);

CREATE TABLE legacy_inode_map (
    volume_id   BLOB NOT NULL CHECK(length(volume_id) = 16),
    legacy_path TEXT NOT NULL,
    inode       BLOB NOT NULL CHECK(length(inode) = 16),
    PRIMARY KEY (volume_id, legacy_path),
    FOREIGN KEY (volume_id, inode)
        REFERENCES inodes(volume_id, inode) ON DELETE RESTRICT
) STRICT;
