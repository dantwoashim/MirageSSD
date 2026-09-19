//! Durable mutable namespace: 128-bit inode identities that survive renames,
//! directory entries keyed by folded name, and paged enumeration. Read paths
//! run on pooled read connections; mutations run inside the single writer
//! actor's transaction.

use mirage_types::{InodeId, MirageError, RepositoryId, fold_name, root_inode};
use rusqlite::{Connection, OptionalExtension, params};

use crate::error::sqlite;

const MAX_LIST_LIMIT: usize = 4096;
const MAX_RENAME_DEPTH: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NamespaceNodeKind {
    Directory,
    File,
}

impl NamespaceNodeKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Directory => "directory",
            Self::File => "file",
        }
    }
    fn parse(value: &str) -> Result<Self, MirageError> {
        match value {
            "directory" => Ok(Self::Directory),
            "file" => Ok(Self::File),
            _ => Err(MirageError::integrity_mismatch(
                "namespace node kind is unknown",
            )),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamespaceStat {
    pub inode: InodeId,
    pub kind: NamespaceNodeKind,
    pub size: u64,
    pub version_root: Option<[u8; 32]>,
    pub extent_root: Option<[u8; 32]>,
    pub created_ns: i64,
    pub modified_ns: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirEntry {
    pub inode: InodeId,
    pub display_name: String,
    pub folded_name: String,
    pub kind: NamespaceNodeKind,
    pub size: u64,
}

fn decode_inode(bytes: &[u8]) -> Result<InodeId, MirageError> {
    let array: [u8; 16] = bytes
        .try_into()
        .map_err(|_| MirageError::integrity_mismatch("namespace inode has invalid length"))?;
    Ok(InodeId::from_bytes(array))
}

fn decode_blob32(bytes: Option<Vec<u8>>) -> Result<Option<[u8; 32]>, MirageError> {
    bytes
        .map(|value| {
            <[u8; 32]>::try_from(value.as_slice()).map_err(|_| {
                MirageError::integrity_mismatch("namespace root hash has invalid length")
            })
        })
        .transpose()
}

/// Creates a volume's namespace and seeds its root directory inode.
pub fn create_volume(
    connection: &mut Connection,
    volume_id: RepositoryId,
    now_ns: i64,
) -> Result<InodeId, MirageError> {
    let root = root_inode(volume_id);
    let transaction = connection
        .transaction()
        .map_err(|e| sqlite(e, "failed to begin namespace volume transaction"))?;
    transaction
        .execute(
            "INSERT INTO namespace_volumes(volume_id, naming_version, root_inode, created_ns)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                volume_id.as_bytes().as_slice(),
                mirage_types::NAMING_POLICY_VERSION,
                root.as_bytes().as_slice(),
                now_ns,
            ],
        )
        .map_err(|e| sqlite(e, "failed to create namespace volume"))?;
    transaction
        .execute(
            "INSERT INTO inodes(volume_id, inode, kind, size, created_ns, modified_ns)
             VALUES (?1, ?2, 'directory', 0, ?3, ?3)",
            params![
                volume_id.as_bytes().as_slice(),
                root.as_bytes().as_slice(),
                now_ns
            ],
        )
        .map_err(|e| sqlite(e, "failed to seed namespace root"))?;
    transaction
        .commit()
        .map_err(|e| sqlite(e, "failed to commit namespace volume"))?;
    Ok(root)
}

/// Resolves one name under `parent` to its entry. `name` is validated and
/// folded under the volume's naming policy before lookup.
pub fn lookup(
    connection: &Connection,
    volume_id: RepositoryId,
    parent: InodeId,
    name: &str,
) -> Result<Option<DirEntry>, MirageError> {
    let folded = fold_name(name)?;
    connection
        .query_row(
            "SELECT d.child_inode, d.display_name, d.folded_name, i.kind, i.size
             FROM dirents d JOIN inodes i
               ON i.volume_id = d.volume_id AND i.inode = d.child_inode
             WHERE d.volume_id = ?1 AND d.parent_inode = ?2 AND d.folded_name = ?3",
            params![
                volume_id.as_bytes().as_slice(),
                parent.as_bytes().as_slice(),
                folded,
            ],
            |row| {
                let inode: Vec<u8> = row.get(0)?;
                let display: String = row.get(1)?;
                let folded: String = row.get(2)?;
                let kind: String = row.get(3)?;
                let size: i64 = row.get(4)?;
                Ok((inode, display, folded, kind, size))
            },
        )
        .optional()
        .map_err(|e| sqlite(e, "namespace lookup failed"))?
        .map(|(inode, display_name, folded_name, kind, size)| {
            Ok(DirEntry {
                inode: decode_inode(&inode)?,
                display_name,
                folded_name,
                kind: NamespaceNodeKind::parse(&kind)?,
                size: u64::try_from(size)
                    .map_err(|_| MirageError::integrity_mismatch("namespace size is negative"))?,
            })
        })
        .transpose()
}

/// Stats one inode without consulting names.
pub fn stat(
    connection: &Connection,
    volume_id: RepositoryId,
    inode: InodeId,
) -> Result<Option<NamespaceStat>, MirageError> {
    connection
        .query_row(
            "SELECT inode, kind, size, version_root, extent_root, created_ns, modified_ns
             FROM inodes WHERE volume_id = ?1 AND inode = ?2",
            params![volume_id.as_bytes().as_slice(), inode.as_bytes().as_slice()],
            |row| {
                Ok((
                    row.get::<_, Vec<u8>>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, Option<Vec<u8>>>(3)?,
                    row.get::<_, Option<Vec<u8>>>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, i64>(6)?,
                ))
            },
        )
        .optional()
        .map_err(|e| sqlite(e, "namespace stat failed"))?
        .map(
            |(inode, kind, size, version_root, extent_root, created_ns, modified_ns)| {
                Ok(NamespaceStat {
                    inode: decode_inode(&inode)?,
                    kind: NamespaceNodeKind::parse(&kind)?,
                    size: u64::try_from(size).map_err(|_| {
                        MirageError::integrity_mismatch("namespace size is negative")
                    })?,
                    version_root: decode_blob32(version_root)?,
                    extent_root: decode_blob32(extent_root)?,
                    created_ns,
                    modified_ns,
                })
            },
        )
        .transpose()
}

/// Pages children of `parent` in folded-name order after `marker`. Returns at
/// most `limit` entries, so a million-entry directory never materializes.
pub fn list_children(
    connection: &Connection,
    volume_id: RepositoryId,
    parent: InodeId,
    marker: Option<&str>,
    limit: usize,
) -> Result<Vec<DirEntry>, MirageError> {
    if limit == 0 || limit > MAX_LIST_LIMIT {
        return Err(MirageError::invalid_argument(
            "namespace listing limit is out of bounds",
        ));
    }
    let mut statement = connection
        .prepare(
            "SELECT d.child_inode, d.display_name, d.folded_name, i.kind, i.size
             FROM dirents d JOIN inodes i
               ON i.volume_id = d.volume_id AND i.inode = d.child_inode
             WHERE d.volume_id = ?1 AND d.parent_inode = ?2
               AND (?3 IS NULL OR d.folded_name > ?3)
             ORDER BY d.folded_name
             LIMIT ?4",
        )
        .map_err(|e| sqlite(e, "failed to prepare namespace listing"))?;
    let rows = statement
        .query_map(
            params![
                volume_id.as_bytes().as_slice(),
                parent.as_bytes().as_slice(),
                marker,
                i64::try_from(limit)
                    .map_err(|_| MirageError::invalid_argument("namespace limit overflows"))?,
            ],
            |row| {
                Ok((
                    row.get::<_, Vec<u8>>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, i64>(4)?,
                ))
            },
        )
        .map_err(|e| sqlite(e, "namespace listing failed"))?;
    let mut entries = Vec::new();
    for row in rows {
        let (inode, display_name, folded_name, kind, size) =
            row.map_err(|e| sqlite(e, "namespace listing row failed"))?;
        entries.push(DirEntry {
            inode: decode_inode(&inode)?,
            display_name,
            folded_name,
            kind: NamespaceNodeKind::parse(&kind)?,
            size: u64::try_from(size)
                .map_err(|_| MirageError::integrity_mismatch("namespace size is negative"))?,
        });
    }
    Ok(entries)
}

/// Resolves a `/`-separated relative path one component at a time, returning
/// the final inode. Returns `None` when any component is absent; invalid
/// names return an error rather than a miss.
pub fn resolve_path(
    connection: &Connection,
    volume_id: RepositoryId,
    path: &str,
) -> Result<Option<InodeId>, MirageError> {
    let mut current = root_inode(volume_id);
    for component in path.split('/').filter(|part| !part.is_empty()) {
        match lookup(connection, volume_id, current, component)? {
            Some(entry) => current = entry.inode,
            None => return Ok(None),
        }
    }
    Ok(Some(current))
}

/// Resolves a legacy path-derived identity through the explicit translation
/// map; legacy IDs are only valid in the legacy format.
pub fn resolve_legacy(
    connection: &Connection,
    volume_id: RepositoryId,
    legacy_path: &str,
) -> Result<Option<InodeId>, MirageError> {
    connection
        .query_row(
            "SELECT inode FROM legacy_inode_map WHERE volume_id = ?1 AND legacy_path = ?2",
            params![volume_id.as_bytes().as_slice(), legacy_path],
            |row| row.get::<_, Vec<u8>>(0),
        )
        .optional()
        .map_err(|e| sqlite(e, "legacy inode lookup failed"))?
        .map(|bytes| decode_inode(&bytes))
        .transpose()
}

/// Allocates the next inode identity for a volume. Deterministic from the
/// volume id and its monotonic allocation sequence, so allocator state is the
/// durable `next_sequence` column rather than a random source.
fn allocate_inode(
    transaction: &Connection,
    volume_id: RepositoryId,
) -> Result<InodeId, MirageError> {
    let sequence: i64 = transaction
        .query_row(
            "SELECT next_sequence FROM namespace_volumes WHERE volume_id = ?1",
            [volume_id.as_bytes().as_slice()],
            |row| row.get(0),
        )
        .optional()
        .map_err(|e| sqlite(e, "namespace allocator lookup failed"))?
        .ok_or_else(|| MirageError::invalid_argument("namespace volume does not exist"))?;
    transaction
        .execute(
            "UPDATE namespace_volumes SET next_sequence = next_sequence + 1
             WHERE volume_id = ?1 AND next_sequence = ?2",
            params![volume_id.as_bytes().as_slice(), sequence],
        )
        .map_err(|e| sqlite(e, "namespace allocator update failed"))?;
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"MirageSSD inode v1\0");
    hasher.update(volume_id.as_bytes());
    hasher.update(&sequence.to_le_bytes());
    Ok(InodeId::from_bytes(
        hasher.finalize().as_bytes()[..16]
            .try_into()
            .expect("blake3 output is 32 bytes"),
    ))
}

/// Inserts a new node and its directory entry atomically. Fails on a folded
/// name collision, an invalid name, or a missing parent.
pub fn create_node(
    connection: &mut Connection,
    volume_id: RepositoryId,
    parent: InodeId,
    display_name: &str,
    kind: NamespaceNodeKind,
    now_ns: i64,
) -> Result<DirEntry, MirageError> {
    let folded = fold_name(display_name)?;
    let transaction = connection
        .transaction()
        .map_err(|e| sqlite(e, "failed to begin namespace create"))?;
    let parent_stat = stat(&transaction, volume_id, parent)?
        .ok_or_else(|| MirageError::invalid_argument("namespace parent inode is missing"))?;
    if parent_stat.kind != NamespaceNodeKind::Directory {
        return Err(MirageError::invalid_argument(
            "namespace parent is not a directory",
        ));
    }
    let inode = allocate_inode(&transaction, volume_id)?;
    transaction
        .execute(
            "INSERT INTO inodes(volume_id, inode, kind, size, created_ns, modified_ns)
             VALUES (?1, ?2, ?3, 0, ?4, ?4)",
            params![
                volume_id.as_bytes().as_slice(),
                inode.as_bytes().as_slice(),
                kind.as_str(),
                now_ns,
            ],
        )
        .map_err(|e| sqlite(e, "namespace inode insert failed"))?;
    let changed = transaction
        .execute(
            "INSERT INTO dirents(volume_id, parent_inode, folded_name, display_name, child_inode)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                volume_id.as_bytes().as_slice(),
                parent.as_bytes().as_slice(),
                &folded,
                display_name,
                inode.as_bytes().as_slice(),
            ],
        )
        .map_err(|e| sqlite(e, "namespace dirent insert failed"))?;
    if changed != 1 {
        return Err(MirageError::repository_conflict(
            "namespace entry already exists",
        ));
    }
    record_delta(
        &transaction,
        volume_id,
        mirage_manifest::namespace::NamespaceOp::Create {
            parent,
            inode,
            display_name: display_name.to_owned(),
            folded_name: folded.clone(),
            directory: kind == NamespaceNodeKind::Directory,
            size: 0,
        },
        now_ns,
    )?;
    transaction
        .commit()
        .map_err(|e| sqlite(e, "namespace create commit failed"))?;
    Ok(DirEntry {
        inode,
        display_name: display_name.to_owned(),
        folded_name: folded,
        kind,
        size: 0,
    })
}

/// Renames or moves an entry. The child's inode identity is preserved.
/// Moving a directory into its own subtree is rejected; overwriting an
/// existing name requires the victim to be a file or an empty directory.
pub fn rename(
    connection: &mut Connection,
    volume_id: RepositoryId,
    from_parent: InodeId,
    from_name: &str,
    to_parent: InodeId,
    to_name: &str,
    now_ns: i64,
) -> Result<(), MirageError> {
    let from_folded = fold_name(from_name)?;
    let to_folded = fold_name(to_name)?;
    let transaction = connection
        .transaction()
        .map_err(|e| sqlite(e, "failed to begin namespace rename"))?;
    let entry = lookup(&transaction, volume_id, from_parent, from_name)?
        .ok_or_else(|| MirageError::invalid_argument("namespace rename source is missing"))?;
    let to_stat = stat(&transaction, volume_id, to_parent)?.ok_or_else(|| {
        MirageError::invalid_argument("namespace rename target parent is missing")
    })?;
    if to_stat.kind != NamespaceNodeKind::Directory {
        return Err(MirageError::invalid_argument(
            "namespace rename target parent is not a directory",
        ));
    }
    // Reject moving a directory into its own subtree: walk the destination's
    // ancestor chain; meeting the moved inode closes a cycle.
    if entry.kind == NamespaceNodeKind::Directory {
        let mut ancestor = to_parent;
        for _ in 0..MAX_RENAME_DEPTH {
            if ancestor == entry.inode {
                return Err(MirageError::repository_conflict(
                    "namespace rename would create a directory cycle",
                ));
            }
            let parent: Option<Vec<u8>> = transaction
                .query_row(
                    "SELECT parent_inode FROM dirents
                     WHERE volume_id = ?1 AND child_inode = ?2",
                    params![
                        volume_id.as_bytes().as_slice(),
                        ancestor.as_bytes().as_slice()
                    ],
                    |row| row.get(0),
                )
                .optional()
                .map_err(|e| sqlite(e, "namespace ancestor lookup failed"))?;
            match parent {
                Some(bytes) => ancestor = decode_inode(&bytes)?,
                None => break,
            }
        }
    }
    if let Some(victim) = lookup(&transaction, volume_id, to_parent, to_name)?
        && victim.inode != entry.inode
    {
        let victim_stat = stat(&transaction, volume_id, victim.inode)?
            .ok_or_else(|| MirageError::integrity_mismatch("namespace victim inode is missing"))?;
        if victim_stat.kind == NamespaceNodeKind::Directory {
            let child_count: i64 = transaction
                .query_row(
                    "SELECT COUNT(*) FROM dirents
                     WHERE volume_id = ?1 AND parent_inode = ?2",
                    params![
                        volume_id.as_bytes().as_slice(),
                        victim.inode.as_bytes().as_slice()
                    ],
                    |row| row.get(0),
                )
                .map_err(|e| sqlite(e, "namespace victim emptiness check failed"))?;
            if child_count != 0 {
                return Err(MirageError::repository_conflict(
                    "namespace rename target is a non-empty directory",
                ));
            }
        }
        transaction
            .execute(
                "DELETE FROM dirents WHERE volume_id = ?1 AND parent_inode = ?2 AND folded_name = ?3",
                params![
                    volume_id.as_bytes().as_slice(),
                    to_parent.as_bytes().as_slice(),
                    victim.folded_name,
                ],
            )
            .map_err(|e| sqlite(e, "namespace victim dirent removal failed"))?;
        transaction
            .execute(
                "DELETE FROM inodes WHERE volume_id = ?1 AND inode = ?2",
                params![
                    volume_id.as_bytes().as_slice(),
                    victim.inode.as_bytes().as_slice()
                ],
            )
            .map_err(|e| sqlite(e, "namespace victim inode removal failed"))?;
        record_delta(
            &transaction,
            volume_id,
            mirage_manifest::namespace::NamespaceOp::Delete {
                parent: to_parent,
                folded_name: victim.folded_name.clone(),
                inode: victim.inode,
            },
            now_ns,
        )?;
    }
    let moved = transaction
        .execute(
            "UPDATE dirents SET parent_inode = ?1, folded_name = ?2, display_name = ?3
             WHERE volume_id = ?4 AND parent_inode = ?5 AND folded_name = ?6",
            params![
                to_parent.as_bytes().as_slice(),
                &to_folded,
                to_name,
                volume_id.as_bytes().as_slice(),
                from_parent.as_bytes().as_slice(),
                &from_folded,
            ],
        )
        .map_err(|e| sqlite(e, "namespace rename move failed"))?;
    if moved != 1 {
        return Err(MirageError::repository_conflict(
            "namespace rename source vanished mid-transaction",
        ));
    }
    transaction
        .execute(
            "UPDATE inodes SET modified_ns = ?1 WHERE volume_id = ?2 AND inode = ?3",
            params![
                now_ns,
                volume_id.as_bytes().as_slice(),
                entry.inode.as_bytes().as_slice()
            ],
        )
        .map_err(|e| sqlite(e, "namespace rename inode touch failed"))?;
    record_delta(
        &transaction,
        volume_id,
        mirage_manifest::namespace::NamespaceOp::Rename {
            inode: entry.inode,
            from_parent,
            from_folded,
            to_parent,
            display_name: to_name.to_owned(),
            folded_name: to_folded,
        },
        now_ns,
    )?;
    transaction
        .commit()
        .map_err(|e| sqlite(e, "namespace rename commit failed"))
}

/// Deletes an entry. A directory must be empty; the inode row is removed with
/// its dirent so identity is never reused.
pub fn delete_node(
    connection: &mut Connection,
    volume_id: RepositoryId,
    parent: InodeId,
    name: &str,
    now_ns: i64,
) -> Result<(), MirageError> {
    let folded = fold_name(name)?;
    let transaction = connection
        .transaction()
        .map_err(|e| sqlite(e, "failed to begin namespace delete"))?;
    let entry = lookup(&transaction, volume_id, parent, name)?
        .ok_or_else(|| MirageError::invalid_argument("namespace delete target is missing"))?;
    if entry.kind == NamespaceNodeKind::Directory {
        let child_count: i64 = transaction
            .query_row(
                "SELECT COUNT(*) FROM dirents
                 WHERE volume_id = ?1 AND parent_inode = ?2",
                params![
                    volume_id.as_bytes().as_slice(),
                    entry.inode.as_bytes().as_slice()
                ],
                |row| row.get(0),
            )
            .map_err(|e| sqlite(e, "namespace delete emptiness check failed"))?;
        if child_count != 0 {
            return Err(MirageError::repository_conflict(
                "namespace delete target is a non-empty directory",
            ));
        }
    }
    transaction
        .execute(
            "DELETE FROM dirents WHERE volume_id = ?1 AND parent_inode = ?2 AND folded_name = ?3",
            params![
                volume_id.as_bytes().as_slice(),
                parent.as_bytes().as_slice(),
                &folded,
            ],
        )
        .map_err(|e| sqlite(e, "namespace dirent delete failed"))?;
    transaction
        .execute(
            "DELETE FROM inodes WHERE volume_id = ?1 AND inode = ?2",
            params![
                volume_id.as_bytes().as_slice(),
                entry.inode.as_bytes().as_slice()
            ],
        )
        .map_err(|e| sqlite(e, "namespace inode delete failed"))?;
    record_delta(
        &transaction,
        volume_id,
        mirage_manifest::namespace::NamespaceOp::Delete {
            parent,
            folded_name: folded,
            inode: entry.inode,
        },
        now_ns,
    )?;
    transaction
        .commit()
        .map_err(|e| sqlite(e, "namespace delete commit failed"))
}

/// Updates a file inode's size and content roots inside a metadata commit.
pub fn set_file_roots(
    connection: &mut Connection,
    volume_id: RepositoryId,
    inode: InodeId,
    size: u64,
    version_root: Option<[u8; 32]>,
    extent_root: Option<[u8; 32]>,
    now_ns: i64,
) -> Result<(), MirageError> {
    let transaction = connection
        .transaction()
        .map_err(|e| sqlite(e, "failed to begin namespace roots update"))?;
    let changed = transaction
        .execute(
            "UPDATE inodes
             SET size = ?1, version_root = ?2, extent_root = ?3, modified_ns = ?4
             WHERE volume_id = ?5 AND inode = ?6 AND kind = 'file'",
            params![
                i64::try_from(size)
                    .map_err(|_| MirageError::invalid_argument("namespace size overflows"))?,
                version_root.as_ref().map(|root| root.as_slice()),
                extent_root.as_ref().map(|root| root.as_slice()),
                now_ns,
                volume_id.as_bytes().as_slice(),
                inode.as_bytes().as_slice(),
            ],
        )
        .map_err(|e| sqlite(e, "namespace root update failed"))?;
    if changed != 1 {
        return Err(MirageError::invalid_argument(
            "namespace file inode is missing",
        ));
    }
    record_delta(
        &transaction,
        volume_id,
        mirage_manifest::namespace::NamespaceOp::SetRoots {
            inode,
            size,
            version_root,
            extent_root,
        },
        now_ns,
    )?;
    transaction
        .commit()
        .map_err(|e| sqlite(e, "namespace roots commit failed"))
}

/// Records the explicit translation of a legacy path-derived identity.
pub fn record_legacy(
    connection: &mut Connection,
    volume_id: RepositoryId,
    legacy_path: &str,
    inode: InodeId,
) -> Result<(), MirageError> {
    connection
        .execute(
            "INSERT OR REPLACE INTO legacy_inode_map(volume_id, legacy_path, inode)
             VALUES (?1, ?2, ?3)",
            params![
                volume_id.as_bytes().as_slice(),
                legacy_path,
                inode.as_bytes().as_slice(),
            ],
        )
        .map_err(|e| sqlite(e, "legacy inode map write failed"))?;
    Ok(())
}

impl crate::Database {
    /// Creates a volume namespace and returns its root inode.
    pub fn namespace_create_volume(
        &self,
        volume_id: RepositoryId,
        now_ns: i64,
    ) -> Result<InodeId, MirageError> {
        self.writer().create_namespace_volume(volume_id, now_ns)
    }

    pub fn namespace_lookup(
        &self,
        volume_id: RepositoryId,
        parent: InodeId,
        name: &str,
    ) -> Result<Option<DirEntry>, MirageError> {
        self.reads()
            .with_connection(|connection| lookup(connection, volume_id, parent, name))
    }

    pub fn namespace_stat(
        &self,
        volume_id: RepositoryId,
        inode: InodeId,
    ) -> Result<Option<NamespaceStat>, MirageError> {
        self.reads()
            .with_connection(|connection| stat(connection, volume_id, inode))
    }

    /// Paged directory enumeration: at most `limit` children after `marker`
    /// in folded-name order.
    pub fn namespace_list_children(
        &self,
        volume_id: RepositoryId,
        parent: InodeId,
        marker: Option<&str>,
        limit: usize,
    ) -> Result<Vec<DirEntry>, MirageError> {
        self.reads().with_connection(|connection| {
            list_children(connection, volume_id, parent, marker, limit)
        })
    }

    pub fn namespace_resolve_path(
        &self,
        volume_id: RepositoryId,
        path: &str,
    ) -> Result<Option<InodeId>, MirageError> {
        self.reads()
            .with_connection(|connection| resolve_path(connection, volume_id, path))
    }

    pub fn namespace_resolve_legacy(
        &self,
        volume_id: RepositoryId,
        legacy_path: &str,
    ) -> Result<Option<InodeId>, MirageError> {
        self.reads()
            .with_connection(|connection| resolve_legacy(connection, volume_id, legacy_path))
    }

    pub fn namespace_create(
        &self,
        volume_id: RepositoryId,
        parent: InodeId,
        name: &str,
        kind: NamespaceNodeKind,
        now_ns: i64,
    ) -> Result<DirEntry, MirageError> {
        self.writer()
            .namespace_create(volume_id, parent, name, kind, now_ns)
    }

    /// Renames or moves an entry; the inode identity is preserved.
    pub fn namespace_rename(
        &self,
        volume_id: RepositoryId,
        from_parent: InodeId,
        from_name: &str,
        to_parent: InodeId,
        to_name: &str,
        now_ns: i64,
    ) -> Result<(), MirageError> {
        self.writer().namespace_rename(
            volume_id,
            from_parent,
            from_name,
            to_parent,
            to_name,
            now_ns,
        )
    }

    pub fn namespace_delete(
        &self,
        volume_id: RepositoryId,
        parent: InodeId,
        name: &str,
        now_ns: i64,
    ) -> Result<(), MirageError> {
        self.writer()
            .namespace_delete(volume_id, parent, name, now_ns)
    }

    pub fn namespace_set_file_roots(
        &self,
        volume_id: RepositoryId,
        inode: InodeId,
        size: u64,
        version_root: Option<[u8; 32]>,
        extent_root: Option<[u8; 32]>,
        now_ns: i64,
    ) -> Result<(), MirageError> {
        self.writer().namespace_set_file_roots(
            volume_id,
            inode,
            size,
            version_root,
            extent_root,
            now_ns,
        )
    }

    /// Records the explicit legacy path-to-inode translation; legacy IDs are
    /// only valid in the legacy format.
    pub fn namespace_record_legacy(
        &self,
        volume_id: RepositoryId,
        legacy_path: &str,
        inode: InodeId,
    ) -> Result<(), MirageError> {
        self.writer()
            .namespace_record_legacy(volume_id, legacy_path, inode)
    }
}

/// One node of a bulk namespace seed: a normalized `/`-separated relative
/// path plus the node's kind and logical size.
#[derive(Debug, Clone)]
pub struct NamespaceSeedNode {
    pub path: String,
    pub is_directory: bool,
    pub size: u64,
    pub version_root: Option<[u8; 32]>,
}

/// Bulk-seeds a volume namespace from a verified manifest tree in a single
/// transaction: directories are created on demand, every node receives a
/// deterministic inode, and each node's relative path is recorded in the
/// legacy translation map. The whole seed rolls back on any failure.
pub fn seed_volume(
    connection: &mut Connection,
    volume_id: RepositoryId,
    nodes: impl IntoIterator<Item = NamespaceSeedNode>,
    now_ns: i64,
) -> Result<usize, MirageError> {
    let transaction = connection
        .transaction()
        .map_err(|e| sqlite(e, "failed to begin namespace seed"))?;
    let root = root_inode(volume_id);
    let exists: bool = transaction
        .query_row(
            "SELECT 1 FROM namespace_volumes WHERE volume_id = ?1",
            [volume_id.as_bytes().as_slice()],
            |_| Ok(()),
        )
        .optional()
        .map_err(|e| sqlite(e, "namespace volume lookup failed"))?
        .is_some();
    if exists {
        // A committed seed is a single transaction, so an existing volume is
        // always complete; re-seeding is an idempotent no-op.
        transaction
            .commit()
            .map_err(|e| sqlite(e, "namespace seed commit failed"))?;
        return Ok(0);
    }
    {
        transaction
            .execute(
                "INSERT INTO namespace_volumes(volume_id, naming_version, root_inode, created_ns)
                 VALUES (?1, ?2, ?3, ?4)",
                params![
                    volume_id.as_bytes().as_slice(),
                    mirage_types::NAMING_POLICY_VERSION,
                    root.as_bytes().as_slice(),
                    now_ns,
                ],
            )
            .map_err(|e| sqlite(e, "namespace volume insert failed"))?;
        transaction
            .execute(
                "INSERT INTO inodes(volume_id, inode, kind, size, created_ns, modified_ns)
                 VALUES (?1, ?2, 'directory', 0, ?3, ?3)",
                params![
                    volume_id.as_bytes().as_slice(),
                    root.as_bytes().as_slice(),
                    now_ns
                ],
            )
            .map_err(|e| sqlite(e, "namespace root insert failed"))?;
    }
    let mut sequence: i64 = transaction
        .query_row(
            "SELECT next_sequence FROM namespace_volumes WHERE volume_id = ?1",
            [volume_id.as_bytes().as_slice()],
            |row| row.get(0),
        )
        .map_err(|e| sqlite(e, "namespace allocator read failed"))?;
    let allocate = |sequence: &mut i64| -> Result<InodeId, MirageError> {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"MirageSSD inode v1\0");
        hasher.update(volume_id.as_bytes());
        hasher.update(&sequence.to_le_bytes());
        *sequence = sequence
            .checked_add(1)
            .ok_or_else(|| MirageError::internal_invariant("namespace allocator overflowed"))?;
        Ok(InodeId::from_bytes(
            hasher.finalize().as_bytes()[..16]
                .try_into()
                .expect("blake3 output is 32 bytes"),
        ))
    };
    let mut path_inode: std::collections::HashMap<String, InodeId> =
        std::collections::HashMap::new();
    path_inode.insert(String::new(), root);
    let mut seeded = 0usize;
    let mut insert_inode = transaction
        .prepare(
            "INSERT INTO inodes(volume_id, inode, kind, size, version_root, created_ns, modified_ns)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)",
        )
        .map_err(|e| sqlite(e, "namespace seed inode prepare failed"))?;
    let mut insert_dirent = transaction
        .prepare(
            "INSERT INTO dirents(volume_id, parent_inode, folded_name, display_name, child_inode)
             VALUES (?1, ?2, ?3, ?4, ?5)",
        )
        .map_err(|e| sqlite(e, "namespace seed dirent prepare failed"))?;
    let mut insert_legacy = transaction
        .prepare(
            "INSERT OR REPLACE INTO legacy_inode_map(volume_id, legacy_path, inode)
             VALUES (?1, ?2, ?3)",
        )
        .map_err(|e| sqlite(e, "namespace seed legacy prepare failed"))?;
    for node in nodes {
        let components: Vec<&str> = node
            .path
            .split('/')
            .filter(|part| !part.is_empty())
            .collect();
        if components.is_empty() {
            continue;
        }
        let mut prefix = String::new();
        let mut parent = root;
        for (depth, component) in components.iter().enumerate() {
            prefix = if prefix.is_empty() {
                (*component).to_owned()
            } else {
                format!("{prefix}/{component}")
            };
            let is_leaf = depth + 1 == components.len();
            if let Some(inode) = path_inode.get(&prefix) {
                if is_leaf {
                    return Err(MirageError::repository_conflict(
                        "namespace seed contains a duplicate path",
                    ));
                }
                parent = *inode;
                continue;
            }
            let folded = fold_name(component)?;
            let inode = allocate(&mut sequence)?;
            let kind = if is_leaf && !node.is_directory {
                NamespaceNodeKind::File
            } else {
                NamespaceNodeKind::Directory
            };
            let size = if is_leaf { node.size } else { 0 };
            insert_inode
                .execute(params![
                    volume_id.as_bytes().as_slice(),
                    inode.as_bytes().as_slice(),
                    kind.as_str(),
                    i64::try_from(size).map_err(|_| {
                        MirageError::invalid_argument("namespace seed size overflows")
                    })?,
                    node.version_root.as_ref().map(|root| root.as_slice()),
                    now_ns,
                ])
                .map_err(|e| sqlite(e, "namespace seed inode insert failed"))?;
            insert_dirent
                .execute(params![
                    volume_id.as_bytes().as_slice(),
                    parent.as_bytes().as_slice(),
                    &folded,
                    component,
                    inode.as_bytes().as_slice(),
                ])
                .map_err(|e| sqlite(e, "namespace seed dirent insert failed"))?;
            insert_legacy
                .execute(params![
                    volume_id.as_bytes().as_slice(),
                    &prefix,
                    inode.as_bytes().as_slice(),
                ])
                .map_err(|e| sqlite(e, "namespace seed legacy insert failed"))?;
            path_inode.insert(prefix.clone(), inode);
            if is_leaf {
                seeded += 1;
            }
            parent = inode;
        }
    }
    drop(insert_legacy);
    drop(insert_dirent);
    drop(insert_inode);
    transaction
        .execute(
            "UPDATE namespace_volumes SET next_sequence = ?1 WHERE volume_id = ?2",
            params![sequence, volume_id.as_bytes().as_slice()],
        )
        .map_err(|e| sqlite(e, "namespace allocator commit failed"))?;
    transaction
        .commit()
        .map_err(|e| sqlite(e, "namespace seed commit failed"))?;
    Ok(seeded)
}

// ---------------------------------------------------------------------------
// Task 6: device identity + versioned namespace history
// ---------------------------------------------------------------------------

/// Returns the durable device identity for this database, creating it on
/// first use inside the writer's transaction.
pub fn ensure_device_id(
    connection: &mut Connection,
    now_ns: i64,
) -> Result<mirage_types::DeviceId, MirageError> {
    if let Some(bytes) = connection
        .query_row(
            "SELECT device_id FROM device_identity WHERE singleton = 1",
            [],
            |row| row.get::<_, Vec<u8>>(0),
        )
        .optional()
        .map_err(|e| sqlite(e, "device identity lookup failed"))?
    {
        let array: [u8; 16] = bytes
            .try_into()
            .map_err(|_| MirageError::integrity_mismatch("device identity has invalid length"))?;
        return Ok(mirage_types::DeviceId::from_bytes(array));
    }
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes)
        .map_err(|_| MirageError::internal_invariant("device identity generation failed"))?;
    connection
        .execute(
            "INSERT INTO device_identity(singleton, device_id, created_ns) VALUES (1, ?1, ?2)",
            params![bytes.as_slice(), now_ns],
        )
        .map_err(|e| sqlite(e, "device identity insert failed"))?;
    Ok(mirage_types::DeviceId::from_bytes(bytes))
}

fn current_delta_segment(
    connection: &Connection,
    volume_id: RepositoryId,
) -> Result<(i64, [u8; 32]), MirageError> {
    connection
        .query_row(
            "SELECT checkpoint_seq, document_hash FROM namespace_checkpoints
             WHERE volume_id = ?1 ORDER BY checkpoint_seq DESC LIMIT 1",
            [volume_id.as_bytes().as_slice()],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Vec<u8>>(1)?)),
        )
        .optional()
        .map_err(|e| sqlite(e, "namespace checkpoint lookup failed"))?
        .map(|(seq, hash)| {
            let hash: [u8; 32] = hash.try_into().map_err(|_| {
                MirageError::integrity_mismatch("namespace checkpoint hash has invalid length")
            })?;
            Ok((seq, hash))
        })
        .transpose()
        .map(|value| value.unwrap_or((0, [0; 32])))
}

/// Appends one mutation op to the open delta segment inside the mutation's
/// own transaction, so history is durable exactly when the mutation is.
fn record_delta(
    transaction: &Connection,
    volume_id: RepositoryId,
    op: mirage_manifest::namespace::NamespaceOp,
    now_ns: i64,
) -> Result<(), MirageError> {
    let (checkpoint_seq, base_hash) = current_delta_segment(transaction, volume_id)?;
    let delta_seq: i64 = transaction
        .query_row(
            "SELECT COALESCE(MAX(delta_seq) + 1, 0) FROM namespace_deltas
             WHERE volume_id = ?1 AND checkpoint_seq = ?2",
            params![volume_id.as_bytes().as_slice(), checkpoint_seq],
            |row| row.get(0),
        )
        .map_err(|e| sqlite(e, "namespace delta sequence failed"))?;
    let op_name = match &op {
        mirage_manifest::namespace::NamespaceOp::Create { .. } => "create",
        mirage_manifest::namespace::NamespaceOp::Rename { .. } => "rename",
        mirage_manifest::namespace::NamespaceOp::Delete { .. } => "delete",
        mirage_manifest::namespace::NamespaceOp::SetRoots { .. } => "set_roots",
    };
    let payload =
        mirage_manifest::namespace::encode_delta(&mirage_manifest::namespace::NamespaceDelta {
            volume_id,
            base_document_hash: base_hash,
            delta_seq: u64::try_from(delta_seq).map_err(|_| {
                MirageError::internal_invariant("namespace delta sequence is negative")
            })?,
            ops: vec![op],
        })?;
    if payload.len() > 65_536 {
        return Err(MirageError::manifest_invalid(
            "namespace delta payload exceeds the durable bound",
        ));
    }
    let payload_hash = blake3::hash(&payload);
    transaction
        .execute(
            "INSERT INTO namespace_deltas(volume_id, checkpoint_seq, delta_seq, op,
                payload_hash, payload, created_ns)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                volume_id.as_bytes().as_slice(),
                checkpoint_seq,
                delta_seq,
                op_name,
                payload_hash.as_bytes().as_slice(),
                payload,
                now_ns,
            ],
        )
        .map_err(|e| sqlite(e, "namespace delta record failed"))?;
    Ok(())
}

/// Number of deltas pending against the newest checkpoint.
pub fn delta_backlog(connection: &Connection, volume_id: RepositoryId) -> Result<i64, MirageError> {
    let (checkpoint_seq, _) = current_delta_segment(connection, volume_id)?;
    connection
        .query_row(
            "SELECT COUNT(*) FROM namespace_deltas
             WHERE volume_id = ?1 AND checkpoint_seq = ?2",
            params![volume_id.as_bytes().as_slice(), checkpoint_seq],
            |row| row.get(0),
        )
        .map_err(|e| sqlite(e, "namespace delta backlog failed"))
}

/// Snapshots the live namespace into an immutable checkpoint document and
/// records it. The next checkpoint sequence is one above the previous.
/// Returns the checkpoint sequence and its canonical document hash.
pub fn create_checkpoint(
    connection: &mut Connection,
    volume_id: RepositoryId,
    now_ns: i64,
) -> Result<(u64, [u8; 32]), MirageError> {
    let transaction = connection
        .transaction()
        .map_err(|e| sqlite(e, "failed to begin namespace checkpoint"))?;
    let mut statement = transaction
        .prepare(
            "SELECT i.inode, d.parent_inode, d.folded_name, d.display_name,
                    i.kind, i.size, i.version_root, i.extent_root
             FROM inodes i
             LEFT JOIN dirents d
               ON d.volume_id = i.volume_id AND d.child_inode = i.inode
             WHERE i.volume_id = ?1
             ORDER BY i.inode",
        )
        .map_err(|e| sqlite(e, "namespace checkpoint scan failed"))?;
    let rows = statement
        .query_map([volume_id.as_bytes().as_slice()], |row| {
            Ok((
                row.get::<_, Vec<u8>>(0)?,
                row.get::<_, Option<Vec<u8>>>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, Option<Vec<u8>>>(6)?,
                row.get::<_, Option<Vec<u8>>>(7)?,
            ))
        })
        .map_err(|e| sqlite(e, "namespace checkpoint rows failed"))?;
    let mut nodes = Vec::new();
    for row in rows {
        let (inode, parent, folded, display, kind, size, version_root, extent_root) =
            row.map_err(|e| sqlite(e, "namespace checkpoint row decode failed"))?;
        let parent_bytes: Option<[u8; 16]> = parent
            .map(|bytes| bytes.try_into())
            .transpose()
            .map_err(|_| MirageError::integrity_mismatch("namespace parent has invalid length"))?;
        nodes.push(mirage_manifest::namespace::NamespaceNodeRecord {
            inode: decode_inode(&inode)?,
            parent: parent_bytes.map(InodeId::from_bytes),
            folded_name: folded.unwrap_or_default(),
            display_name: display.unwrap_or_default(),
            directory: kind == "directory",
            size: u64::try_from(size)
                .map_err(|_| MirageError::integrity_mismatch("namespace size is negative"))?,
            version_root: decode_blob32(version_root)?,
            extent_root: decode_blob32(extent_root)?,
        });
    }
    drop(statement);
    let (last_seq, _) = current_delta_segment(&transaction, volume_id)?;
    let checkpoint_seq = u64::try_from(last_seq).unwrap_or(0) + 1;
    let entry_count = nodes.len();
    let checkpoint = mirage_manifest::namespace::NamespaceCheckpoint {
        volume_id,
        checkpoint_seq,
        parent_commit: None,
        nodes,
    };
    let document = mirage_manifest::namespace::encode_checkpoint(&checkpoint)?;
    let document_hash = mirage_manifest::namespace::checkpoint_hash(&document);
    transaction
        .execute(
            "INSERT INTO namespace_checkpoints(volume_id, checkpoint_seq, document_hash,
                document, parent_commit, entry_count, created_ns)
             VALUES (?1, ?2, ?3, ?4, NULL, ?5, ?6)",
            params![
                volume_id.as_bytes().as_slice(),
                i64::try_from(checkpoint_seq).map_err(|_| {
                    MirageError::internal_invariant("namespace checkpoint sequence overflows")
                })?,
                document_hash.as_slice(),
                document,
                i64::try_from(entry_count).map_err(|_| {
                    MirageError::internal_invariant("namespace entry count overflows")
                })?,
                now_ns,
            ],
        )
        .map_err(|e| sqlite(e, "namespace checkpoint insert failed"))?;
    transaction
        .commit()
        .map_err(|e| sqlite(e, "namespace checkpoint commit failed"))?;
    Ok((checkpoint_seq, document_hash))
}

impl crate::Database {
    /// Durable device identity for this installation.
    pub fn device_id(&self, now_ns: i64) -> Result<mirage_types::DeviceId, MirageError> {
        self.writer().ensure_device_id(now_ns)
    }

    /// Number of namespace deltas pending against the newest checkpoint.
    pub fn namespace_delta_backlog(&self, volume_id: RepositoryId) -> Result<i64, MirageError> {
        self.reads()
            .with_connection(|connection| delta_backlog(connection, volume_id))
    }

    /// Writes an immutable namespace checkpoint for the volume; returns the
    /// checkpoint sequence and canonical document hash.
    pub fn namespace_checkpoint(
        &self,
        volume_id: RepositoryId,
        now_ns: i64,
    ) -> Result<(u64, [u8; 32]), MirageError> {
        self.writer().namespace_checkpoint(volume_id, now_ns)
    }

    /// Reads the newest checkpoint document bytes for replay or publication.
    pub fn namespace_latest_checkpoint_document(
        &self,
        volume_id: RepositoryId,
    ) -> Result<Option<Vec<u8>>, MirageError> {
        self.reads().with_connection(|connection| {
            connection
                .query_row(
                    "SELECT document FROM namespace_checkpoints
                     WHERE volume_id = ?1 ORDER BY checkpoint_seq DESC LIMIT 1",
                    [volume_id.as_bytes().as_slice()],
                    |row| row.get::<_, Vec<u8>>(0),
                )
                .optional()
                .map_err(|e| sqlite(e, "namespace checkpoint document read failed"))
        })
    }
}
