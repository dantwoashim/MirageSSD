//! Durable byte-extent storage: versioned base/dirty/zero extents per inode.
//! Extents for a version are inserted atomically with the mutation; readers
//! pin a version so later writes never mutate a view in progress.

use mirage_types::{InodeId, MirageError, PageHash, RepositoryId};
use rusqlite::{Connection, OptionalExtension, params};

use crate::Database;
use crate::error::sqlite;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtentKind {
    /// Immutable pack content at `page_hash` + `base_offset`.
    Base,
    /// Locally written bytes in a journaled payload file.
    Dirty,
    /// Sparse zeroes; carries no bytes.
    Zero,
}

impl ExtentKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Base => "base",
            Self::Dirty => "dirty",
            Self::Zero => "zero",
        }
    }
    fn parse(value: &str) -> Result<Self, MirageError> {
        match value {
            "base" => Ok(Self::Base),
            "dirty" => Ok(Self::Dirty),
            "zero" => Ok(Self::Zero),
            _ => Err(MirageError::integrity_mismatch("extent kind is unknown")),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ByteExtent {
    pub extent_id: [u8; 16],
    pub volume_id: RepositoryId,
    pub inode: InodeId,
    pub version: i64,
    pub start: u64,
    pub length: u64,
    pub kind: ExtentKind,
    pub page_hash: Option<PageHash>,
    pub base_offset: Option<u64>,
    pub payload_id: Option<[u8; 16]>,
    /// Offset inside the staged payload where this slice's bytes begin; a
    /// dirty interval clipped mid-payload advances it so the surviving tail
    /// does not point at payload byte zero.
    pub payload_offset: Option<u64>,
    pub created_ns: i64,
}

impl ByteExtent {
    pub fn validate(&self) -> Result<(), MirageError> {
        if self.length == 0 {
            return Err(MirageError::invalid_argument("extent length is zero"));
        }
        let coherent = match self.kind {
            ExtentKind::Base => {
                self.page_hash.is_some()
                    && self.base_offset.is_some()
                    && self.payload_id.is_none()
                    && self.payload_offset.is_none()
            }
            ExtentKind::Dirty => {
                self.payload_id.is_some()
                    && self.payload_offset.is_some()
                    && self.page_hash.is_none()
                    && self.base_offset.is_none()
            }
            ExtentKind::Zero => {
                self.page_hash.is_none()
                    && self.base_offset.is_none()
                    && self.payload_id.is_none()
                    && self.payload_offset.is_none()
            }
        };
        if !coherent {
            return Err(MirageError::invalid_argument(
                "extent references do not match its kind",
            ));
        }
        Ok(())
    }
}

/// Replaces an inode's extent set at `version` in one transaction: all
/// writes for the version land or none do. The version head is durable even
/// when the extent set is empty — a truncate-to-zero leaves no rows — so the
/// newest-version marker and logical EOF live in `byte_extent_heads`.
pub fn replace_extents(
    connection: &mut Connection,
    volume_id: RepositoryId,
    inode: InodeId,
    version: i64,
    extents: &[ByteExtent],
    now_ns: i64,
) -> Result<(), MirageError> {
    let eof = extents
        .iter()
        .map(|extent| extent.start.saturating_add(extent.length))
        .max()
        .unwrap_or(0);
    replace_extents_with_eof(connection, volume_id, inode, version, eof, extents, now_ns)
}

/// Same as [`replace_extents`] with an explicit logical EOF. The EOF may
/// exceed the last extent's end — a truncate-grow leaves a hole the read
/// path fills with zeroes — so it cannot be derived from the rows.
pub fn replace_extents_with_eof(
    connection: &mut Connection,
    volume_id: RepositoryId,
    inode: InodeId,
    version: i64,
    eof: u64,
    extents: &[ByteExtent],
    now_ns: i64,
) -> Result<(), MirageError> {
    let transaction = connection
        .transaction()
        .map_err(|e| sqlite(e, "failed to begin extent replace"))?;
    transaction
        .execute(
            "DELETE FROM byte_extents
             WHERE volume_id = ?1 AND inode = ?2 AND version = ?3",
            params![
                volume_id.as_bytes().as_slice(),
                inode.as_bytes().as_slice(),
                version,
            ],
        )
        .map_err(|e| sqlite(e, "extent replace delete failed"))?;
    let mut previous_end = 0u64;
    for (index, extent) in extents.iter().enumerate() {
        extent.validate()?;
        if extent.volume_id != volume_id || extent.inode != inode {
            return Err(MirageError::invalid_argument(
                "extent does not belong to the target inode",
            ));
        }
        if extent.start.saturating_add(extent.length) > eof {
            return Err(MirageError::invalid_argument(
                "extent set extends past the declared EOF",
            ));
        }
        // Extents within a version must be ordered and non-overlapping.
        if index > 0 && extent.start < previous_end {
            return Err(MirageError::integrity_mismatch(
                "extents overlap within a version",
            ));
        }
        previous_end = extent.start.saturating_add(extent.length);
        transaction
            .execute(
                "INSERT INTO byte_extents
                 (extent_id, volume_id, inode, version, start, length, kind,
                  page_hash, base_offset, payload_id, payload_offset, created_ns)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                params![
                    extent.extent_id.as_slice(),
                    volume_id.as_bytes().as_slice(),
                    inode.as_bytes().as_slice(),
                    version,
                    extent.start as i64,
                    extent.length as i64,
                    extent.kind.as_str(),
                    extent.page_hash.map(|hash| hash.as_bytes().to_vec()),
                    extent.base_offset.map(|offset| offset as i64),
                    extent.payload_id.map(|id| id.to_vec()),
                    extent.payload_offset.map(|offset| offset as i64),
                    now_ns,
                ],
            )
            .map_err(|e| sqlite(e, "extent insert failed"))?;
    }
    // Monotonic version head: an out-of-order older version never lowers the
    // recorded head, and an empty extent set still records the new EOF.
    transaction
        .execute(
            "INSERT INTO byte_extent_heads (volume_id, inode, version, eof, updated_ns)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(volume_id, inode) DO UPDATE SET
                version = excluded.version, eof = excluded.eof,
                updated_ns = excluded.updated_ns
             WHERE excluded.version > byte_extent_heads.version",
            params![
                volume_id.as_bytes().as_slice(),
                inode.as_bytes().as_slice(),
                version,
                eof as i64,
                now_ns,
            ],
        )
        .map_err(|e| sqlite(e, "extent head write failed"))?;
    transaction
        .commit()
        .map_err(|e| sqlite(e, "extent replace commit failed"))
}

/// The durable per-inode head: newest committed version and its logical EOF.
pub fn extent_head(
    connection: &Connection,
    volume_id: RepositoryId,
    inode: InodeId,
) -> Result<Option<(i64, u64)>, MirageError> {
    connection
        .query_row(
            "SELECT version, eof FROM byte_extent_heads
             WHERE volume_id = ?1 AND inode = ?2",
            params![volume_id.as_bytes().as_slice(), inode.as_bytes().as_slice()],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    u64::try_from(row.get::<_, i64>(1)?)
                        .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(1, 0))?,
                ))
            },
        )
        .optional()
        .map_err(|e| sqlite(e, "extent head lookup failed"))
}

/// All extents for one inode version, ordered by start.
pub fn extents_at(
    connection: &Connection,
    volume_id: RepositoryId,
    inode: InodeId,
    version: i64,
) -> Result<Vec<ByteExtent>, MirageError> {
    let mut statement = connection
        .prepare(
            "SELECT extent_id, volume_id, inode, version, start, length, kind,
                    page_hash, base_offset, payload_id, payload_offset, created_ns
             FROM byte_extents
             WHERE volume_id = ?1 AND inode = ?2 AND version = ?3
             ORDER BY start",
        )
        .map_err(|e| sqlite(e, "extent scan prepare failed"))?;
    let rows = statement
        .query_map(
            params![
                volume_id.as_bytes().as_slice(),
                inode.as_bytes().as_slice(),
                version,
            ],
            |row| {
                let page_hash: Option<Vec<u8>> = row.get(7)?;
                let payload_id: Option<Vec<u8>> = row.get(9)?;
                Ok(ByteExtent {
                    extent_id: row
                        .get::<_, Vec<u8>>(0)?
                        .try_into()
                        .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(0, 16))?,
                    volume_id: RepositoryId::from_bytes(
                        row.get::<_, Vec<u8>>(1)?
                            .try_into()
                            .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(1, 16))?,
                    ),
                    inode: InodeId::from_bytes(
                        row.get::<_, Vec<u8>>(2)?
                            .try_into()
                            .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(2, 16))?,
                    ),
                    version: row.get(3)?,
                    start: u64::try_from(row.get::<_, i64>(4)?)
                        .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(4, 0))?,
                    length: u64::try_from(row.get::<_, i64>(5)?)
                        .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(5, 0))?,
                    kind: ExtentKind::parse(&row.get::<_, String>(6)?)
                        .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(6, 6))?,
                    page_hash: page_hash
                        .map(|bytes| bytes.try_into().map(PageHash::from_bytes))
                        .transpose()
                        .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(7, 32))?,
                    base_offset: row
                        .get::<_, Option<i64>>(8)?
                        .map(u64::try_from)
                        .transpose()
                        .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(8, 0))?,
                    payload_id: payload_id
                        .map(|bytes| bytes.try_into())
                        .transpose()
                        .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(9, 16))?,
                    // Pre-migration dirty rows carry no payload offset; the
                    // tail-corruption fix treats them as offset zero, which is
                    // only wrong for rows already written incorrectly.
                    payload_offset: row
                        .get::<_, Option<i64>>(10)?
                        .map(u64::try_from)
                        .transpose()
                        .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(10, 0))?,
                    created_ns: row.get(11)?,
                })
            },
        )
        .map_err(|e| sqlite(e, "extent scan failed"))?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|e| sqlite(e, "extent row decode failed"))
}

/// Latest written version for an inode, or `None` when the inode has no
/// extent history. Reads the durable head first — an empty version (truncate
/// to zero) has no extent rows — and falls back to extent rows written
/// before heads existed.
pub fn latest_version(
    connection: &Connection,
    volume_id: RepositoryId,
    inode: InodeId,
) -> Result<Option<i64>, MirageError> {
    connection
        .query_row(
            "SELECT MAX(version) FROM (
                 SELECT version FROM byte_extent_heads
                 WHERE volume_id = ?1 AND inode = ?2
                 UNION ALL
                 SELECT version FROM byte_extents
                 WHERE volume_id = ?1 AND inode = ?2)",
            params![volume_id.as_bytes().as_slice(), inode.as_bytes().as_slice(),],
            |row| row.get::<_, Option<i64>>(0),
        )
        .map_err(|e| sqlite(e, "extent version lookup failed"))
}

/// Extent ids still referenced by pinned (non-latest) versions — protected
/// until every snapshot reader releases them.
pub fn extents_before(
    connection: &Connection,
    volume_id: RepositoryId,
    inode: InodeId,
    min_version: i64,
) -> Result<Vec<[u8; 16]>, MirageError> {
    let mut statement = connection
        .prepare(
            "SELECT extent_id FROM byte_extents
             WHERE volume_id = ?1 AND inode = ?2 AND version < ?3",
        )
        .map_err(|e| sqlite(e, "old extent scan prepare failed"))?;
    let rows = statement
        .query_map(
            params![
                volume_id.as_bytes().as_slice(),
                inode.as_bytes().as_slice(),
                min_version,
            ],
            |row| {
                row.get::<_, Vec<u8>>(0)?
                    .try_into()
                    .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(0, 16))
            },
        )
        .map_err(|e| sqlite(e, "old extent scan failed"))?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|e| sqlite(e, "old extent row decode failed"))
}

/// Payload ids referenced by any `byte_extents` row of the volume — the set
/// a payload file must belong to before it may be deleted.
pub fn referenced_payload_ids(
    connection: &Connection,
    volume_id: RepositoryId,
) -> Result<std::collections::BTreeSet<[u8; 16]>, MirageError> {
    let mut statement = connection
        .prepare(
            "SELECT DISTINCT payload_id FROM byte_extents
             WHERE volume_id = ?1 AND payload_id IS NOT NULL",
        )
        .map_err(|e| sqlite(e, "referenced payload scan prepare failed"))?;
    let rows = statement
        .query_map([volume_id.as_bytes().as_slice()], |row| {
            row.get::<_, Vec<u8>>(0)?
                .try_into()
                .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(0, 16))
        })
        .map_err(|e| sqlite(e, "referenced payload scan failed"))?;
    rows.collect::<Result<std::collections::BTreeSet<_>, _>>()
        .map_err(|e| sqlite(e, "referenced payload row decode failed"))
}

/// Quiesce-time compaction for one volume: drops every `byte_extents` row
/// strictly older than its inode's durable head, then marks physical
/// extents of `journal_file_id` dead once no remaining row references their
/// payload. One transaction: either all superseded history and dead marks
/// land, or nothing does. Returns `(payload_id, length_bytes)` for each
/// extent that transitioned to dead — the caller deletes payload files and
/// releases budget only after this commit.
pub fn compact_volume(
    connection: &mut Connection,
    volume_id: RepositoryId,
    journal_file_id: [u8; 16],
    now_ns: i64,
) -> Result<Vec<([u8; 16], i64)>, MirageError> {
    let transaction = connection
        .transaction()
        .map_err(|e| sqlite(e, "failed to begin volume compaction"))?;
    let heads: Vec<(Vec<u8>, i64)> = {
        let mut statement = transaction
            .prepare("SELECT inode, version FROM byte_extent_heads WHERE volume_id = ?1")
            .map_err(|e| sqlite(e, "extent head scan prepare failed"))?;
        let rows = statement
            .query_map([volume_id.as_bytes().as_slice()], |row| {
                Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, i64>(1)?))
            })
            .map_err(|e| sqlite(e, "extent head scan failed"))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| sqlite(e, "extent head row decode failed"))?
    };
    for (inode, version) in &heads {
        transaction
            .execute(
                "DELETE FROM byte_extents
                 WHERE volume_id = ?1 AND inode = ?2 AND version < ?3",
                params![volume_id.as_bytes().as_slice(), inode, version],
            )
            .map_err(|e| sqlite(e, "superseded extent drop failed"))?;
    }
    let referenced: std::collections::BTreeSet<Vec<u8>> = {
        let mut statement = transaction
            .prepare(
                "SELECT DISTINCT payload_id FROM byte_extents
                 WHERE volume_id = ?1 AND payload_id IS NOT NULL",
            )
            .map_err(|e| sqlite(e, "referenced payload scan prepare failed"))?;
        let rows = statement
            .query_map([volume_id.as_bytes().as_slice()], |row| {
                row.get::<_, Vec<u8>>(0)
            })
            .map_err(|e| sqlite(e, "referenced payload scan failed"))?;
        rows.collect::<Result<std::collections::BTreeSet<_>, _>>()
            .map_err(|e| sqlite(e, "referenced payload row decode failed"))?
    };
    let alive: Vec<(Vec<u8>, i64)> = {
        let mut statement = transaction
            .prepare(
                "SELECT extent_id, length_bytes FROM physical_extents
                 WHERE file_id = ?1 AND state = 'alive'",
            )
            .map_err(|e| sqlite(e, "journal extent scan prepare failed"))?;
        let rows = statement
            .query_map([journal_file_id.as_slice()], |row| {
                Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, i64>(1)?))
            })
            .map_err(|e| sqlite(e, "journal extent scan failed"))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| sqlite(e, "journal extent row decode failed"))?
    };
    let mut dead = Vec::new();
    for (extent_id, length_bytes) in alive {
        if referenced.contains(&extent_id) {
            continue;
        }
        // Pinned or not-alive extents stay put — a failed mark leaves the
        // payload referenced by the ledger and out of the delete set.
        let changed = transaction
            .execute(
                "UPDATE physical_extents SET state = 'dead', updated_ns = ?1
                 WHERE extent_id = ?2 AND state = 'alive' AND pin_count = 0",
                params![now_ns, extent_id.as_slice()],
            )
            .map_err(|e| sqlite(e, "journal extent dead mark failed"))?;
        if changed == 1 {
            dead.push((
                extent_id
                    .try_into()
                    .map_err(|_| MirageError::integrity_mismatch("extent id length"))?,
                length_bytes,
            ));
        }
    }
    transaction
        .commit()
        .map_err(|e| sqlite(e, "volume compaction commit failed"))?;
    Ok(dead)
}

/// Drops versions strictly older than `keep_from` once no snapshot pins them.
pub fn drop_versions_before(
    connection: &mut Connection,
    volume_id: RepositoryId,
    inode: InodeId,
    keep_from: i64,
) -> Result<u64, MirageError> {
    let changed = connection
        .execute(
            "DELETE FROM byte_extents
             WHERE volume_id = ?1 AND inode = ?2 AND version < ?3",
            params![
                volume_id.as_bytes().as_slice(),
                inode.as_bytes().as_slice(),
                keep_from,
            ],
        )
        .map_err(|e| sqlite(e, "old extent drop failed"))?;
    u64::try_from(changed)
        .map_err(|_| MirageError::internal_invariant("extent drop count overflowed"))
}

impl Database {
    /// Payload ids referenced by any extent row of the volume.
    pub fn referenced_payload_ids(
        &self,
        volume_id: RepositoryId,
    ) -> Result<std::collections::BTreeSet<[u8; 16]>, MirageError> {
        self.reads()
            .with_connection(|connection| referenced_payload_ids(connection, volume_id))
    }

    /// Extents for one inode version in range order.
    pub fn extents_at(
        &self,
        volume_id: RepositoryId,
        inode: InodeId,
        version: i64,
    ) -> Result<Vec<ByteExtent>, MirageError> {
        self.reads()
            .with_connection(|connection| extents_at(connection, volume_id, inode, version))
    }

    /// Latest extent version for an inode.
    pub fn extent_latest_version(
        &self,
        volume_id: RepositoryId,
        inode: InodeId,
    ) -> Result<Option<i64>, MirageError> {
        self.reads()
            .with_connection(|connection| latest_version(connection, volume_id, inode))
    }

    /// The inode's durable head: `(newest version, logical EOF)`, or `None`
    /// when the inode has no extent history.
    pub fn extent_head(
        &self,
        volume_id: RepositoryId,
        inode: InodeId,
    ) -> Result<Option<(i64, u64)>, MirageError> {
        self.reads()
            .with_connection(|connection| extent_head(connection, volume_id, inode))
    }
}
