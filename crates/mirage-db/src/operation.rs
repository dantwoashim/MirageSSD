//! Durable local-mutation journal. An operation row is created pending,
//! carries its payload references, and may only transition to `committed`
//! after every referenced payload has a durable-flush marker — the flush
//! fence between "bytes written" and "metadata may claim them". Committed
//! operations are grouped into flush groups; a successful
//! `FlushFileBuffers` marks the group flushed, which is a local durability
//! acknowledgement, never cloud completion.
//!
//! Recovery replays `committed`/`flushed` operations idempotently by
//! `operation_id`; payloads no longer referenced by any non-reclaimed
//! operation are reclaimable.

use mirage_types::{MirageError, RepositoryId};
use rusqlite::{Connection, OptionalExtension, params};

use crate::Database;
use crate::error::sqlite;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationKind {
    Create,
    Rename,
    Delete,
    Write,
    Truncate,
    Mkdir,
    Replace,
}

impl OperationKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Create => "create",
            Self::Rename => "rename",
            Self::Delete => "delete",
            Self::Write => "write",
            Self::Truncate => "truncate",
            Self::Mkdir => "mkdir",
            Self::Replace => "replace",
        }
    }
    fn parse(value: &str) -> Result<Self, MirageError> {
        match value {
            "create" => Ok(Self::Create),
            "rename" => Ok(Self::Rename),
            "delete" => Ok(Self::Delete),
            "write" => Ok(Self::Write),
            "truncate" => Ok(Self::Truncate),
            "mkdir" => Ok(Self::Mkdir),
            "replace" => Ok(Self::Replace),
            _ => Err(MirageError::integrity_mismatch("operation kind is unknown")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum OperationStatus {
    Pending,
    Committed,
    Flushed,
    Published,
    Reclaimed,
}

impl OperationStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Committed => "committed",
            Self::Flushed => "flushed",
            Self::Published => "published",
            Self::Reclaimed => "reclaimed",
        }
    }
    fn parse(value: &str) -> Result<Self, MirageError> {
        match value {
            "pending" => Ok(Self::Pending),
            "committed" => Ok(Self::Committed),
            "flushed" => Ok(Self::Flushed),
            "published" => Ok(Self::Published),
            "reclaimed" => Ok(Self::Reclaimed),
            _ => Err(MirageError::integrity_mismatch(
                "operation status is unknown",
            )),
        }
    }
}

#[derive(Debug, Clone)]
pub struct OperationRecord {
    pub operation_id: [u8; 16],
    pub device_seq: i64,
    pub volume_id: RepositoryId,
    pub base_commit: Option<[u8; 32]>,
    pub kind: OperationKind,
    pub payload: Vec<u8>,
    pub status: OperationStatus,
    pub flush_group: Option<i64>,
    pub depends_on: Option<[u8; 16]>,
    pub created_ns: i64,
}

#[derive(Debug, Clone)]
pub struct OperationPayloadRecord {
    pub payload_id: [u8; 16],
    pub operation_id: [u8; 16],
    pub path: String,
    pub bytes: i64,
    pub checksum: Option<[u8; 32]>,
    pub flushed_ns: Option<i64>,
}

/// Inserts a pending operation with its payload references. `device_seq`
/// must be the caller's next local sequence for the volume.
pub fn begin_operation(
    connection: &mut Connection,
    operation: &OperationRecord,
    payloads: &[OperationPayloadRecord],
) -> Result<(), MirageError> {
    if operation.status != OperationStatus::Pending {
        return Err(MirageError::invalid_argument(
            "a new operation must start pending",
        ));
    }
    let transaction = connection
        .transaction()
        .map_err(|e| sqlite(e, "failed to begin operation journal"))?;
    if let Some(parent) = operation.depends_on {
        let parent_status: Option<String> = transaction
            .query_row(
                "SELECT status FROM local_operations WHERE operation_id = ?1",
                [parent.as_slice()],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| sqlite(e, "operation dependency lookup failed"))?;
        if parent_status.is_none() {
            return Err(MirageError::repository_conflict(
                "operation depends on a missing operation",
            ));
        }
    }
    transaction
        .execute(
            "INSERT INTO local_operations
             (operation_id, device_seq, volume_id, base_commit, kind, payload,
              status, flush_group, depends_on, created_ns)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'pending', ?7, ?8, ?9)",
            params![
                operation.operation_id.as_slice(),
                operation.device_seq,
                operation.volume_id.as_bytes().as_slice(),
                operation.base_commit.map(|bytes| bytes.to_vec()),
                operation.kind.as_str(),
                operation.payload,
                operation.flush_group,
                operation.depends_on.map(|bytes| bytes.to_vec()),
                operation.created_ns,
            ],
        )
        .map_err(|e| sqlite(e, "operation insert failed"))?;
    for payload in payloads {
        transaction
            .execute(
                "INSERT INTO operation_payloads
                 (payload_id, operation_id, path, bytes, checksum, flushed_ns)
                 VALUES (?1, ?2, ?3, ?4, ?5, NULL)",
                params![
                    payload.payload_id.as_slice(),
                    operation.operation_id.as_slice(),
                    payload.path,
                    payload.bytes,
                    payload.checksum.map(|bytes| bytes.to_vec()),
                ],
            )
            .map_err(|e| sqlite(e, "operation payload insert failed"))?;
    }
    transaction
        .commit()
        .map_err(|e| sqlite(e, "operation journal commit failed"))
}

/// Marks a payload flushed after its bytes have been durably written.
/// Idempotent on payload id: re-marking a flushed payload is a no-op.
pub fn mark_payload_flushed(
    connection: &mut Connection,
    payload_id: &[u8; 16],
    checksum: &[u8; 32],
    now_ns: i64,
) -> Result<(), MirageError> {
    let changed = connection
        .execute(
            "UPDATE operation_payloads SET flushed_ns = ?1, checksum = ?2
             WHERE payload_id = ?3 AND flushed_ns IS NULL",
            params![now_ns, checksum.as_slice(), payload_id.as_slice()],
        )
        .map_err(|e| sqlite(e, "payload flush mark failed"))?;
    if changed == 0 {
        // Idempotent replay: already flushed or unknown — only unknown fails.
        let exists: Option<i64> = connection
            .query_row(
                "SELECT 1 FROM operation_payloads WHERE payload_id = ?1",
                [payload_id.as_slice()],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| sqlite(e, "payload lookup failed"))?;
        if exists.is_none() {
            return Err(MirageError::repository_conflict(
                "flushed payload is not journaled",
            ));
        }
    }
    Ok(())
}

/// Commits a pending operation; fails closed when any referenced payload has
/// not reached its durable-flush marker.
pub fn commit_operation(
    connection: &mut Connection,
    operation_id: &[u8; 16],
) -> Result<(), MirageError> {
    let unflushed: i64 = connection
        .query_row(
            "SELECT count(*) FROM operation_payloads
             WHERE operation_id = ?1 AND flushed_ns IS NULL",
            [operation_id.as_slice()],
            |row| row.get(0),
        )
        .map_err(|e| sqlite(e, "payload flush check failed"))?;
    if unflushed > 0 {
        return Err(MirageError::repository_conflict(
            "operation commits before its payloads are durable",
        ));
    }
    let changed = connection
        .execute(
            "UPDATE local_operations SET status = 'committed'
             WHERE operation_id = ?1 AND status = 'pending'",
            [operation_id.as_slice()],
        )
        .map_err(|e| sqlite(e, "operation commit failed"))?;
    if changed != 1 {
        return Err(MirageError::repository_conflict("operation is not pending"));
    }
    Ok(())
}

/// Opens a flush group: committed operations captured in one durability
/// fence. Returns the group id.
pub fn open_flush_group(
    connection: &mut Connection,
    volume_id: RepositoryId,
    now_ns: i64,
) -> Result<i64, MirageError> {
    let transaction = connection
        .transaction()
        .map_err(|e| sqlite(e, "failed to open flush group"))?;
    transaction
        .execute(
            "INSERT INTO flush_groups(volume_id, opened_ns, flushed_ns)
             VALUES (?1, ?2, NULL)",
            params![volume_id.as_bytes().as_slice(), now_ns],
        )
        .map_err(|e| sqlite(e, "flush group insert failed"))?;
    let group_id = transaction.last_insert_rowid();
    transaction
        .execute(
            "UPDATE local_operations SET flush_group = ?1
             WHERE volume_id = ?2 AND status = 'committed' AND flush_group IS NULL",
            params![group_id, volume_id.as_bytes().as_slice()],
        )
        .map_err(|e| sqlite(e, "flush group assignment failed"))?;
    transaction
        .commit()
        .map_err(|e| sqlite(e, "flush group commit failed"))?;
    Ok(group_id)
}

/// Marks every committed operation in the group flushed — the local
/// durability acknowledgement for `FlushFileBuffers`.
pub fn mark_group_flushed(
    connection: &mut Connection,
    group_id: i64,
    now_ns: i64,
) -> Result<u64, MirageError> {
    let transaction = connection
        .transaction()
        .map_err(|e| sqlite(e, "failed to flush group"))?;
    let changed = transaction
        .execute(
            "UPDATE local_operations SET status = 'flushed'
             WHERE flush_group = ?1 AND status = 'committed'",
            [group_id],
        )
        .map_err(|e| sqlite(e, "flush group operation update failed"))?;
    transaction
        .execute(
            "UPDATE flush_groups SET flushed_ns = ?1
             WHERE group_id = ?2 AND flushed_ns IS NULL",
            params![now_ns, group_id],
        )
        .map_err(|e| sqlite(e, "flush group mark failed"))?;
    transaction
        .commit()
        .map_err(|e| sqlite(e, "flush group commit failed"))?;
    u64::try_from(changed).map_err(|_| MirageError::internal_invariant("flush count overflowed"))
}

/// Advances flushed operations to published after their remote commit lands.
pub fn mark_published(
    connection: &mut Connection,
    operation_ids: &[[u8; 16]],
) -> Result<u64, MirageError> {
    let transaction = connection
        .transaction()
        .map_err(|e| sqlite(e, "failed to mark published"))?;
    let mut changed = 0usize;
    for id in operation_ids {
        changed += transaction
            .execute(
                "UPDATE local_operations SET status = 'published'
                 WHERE operation_id = ?1 AND status = 'flushed'",
                [id.as_slice()],
            )
            .map_err(|e| sqlite(e, "operation publish mark failed"))?;
    }
    transaction
        .commit()
        .map_err(|e| sqlite(e, "publish mark commit failed"))?;
    u64::try_from(changed).map_err(|_| MirageError::internal_invariant("publish count overflowed"))
}

/// Operations to replay at mount recovery: committed or flushed but not yet
/// published, in device sequence order. Pending operations were never
/// acknowledged and are ignored by replay (their payloads are reclaimable).
pub fn replayable(
    connection: &Connection,
    volume_id: RepositoryId,
) -> Result<Vec<OperationRecord>, MirageError> {
    let mut statement = connection
        .prepare(
            "SELECT operation_id, device_seq, volume_id, base_commit, kind,
                    payload, status, flush_group, depends_on, created_ns
             FROM local_operations
             WHERE volume_id = ?1 AND status IN ('committed', 'flushed')
             ORDER BY device_seq",
        )
        .map_err(|e| sqlite(e, "replay scan prepare failed"))?;
    let rows = statement
        .query_map([volume_id.as_bytes().as_slice()], |row| {
            let base: Option<Vec<u8>> = row.get(3)?;
            let depends: Option<Vec<u8>> = row.get(8)?;
            Ok(OperationRecord {
                operation_id: row
                    .get::<_, Vec<u8>>(0)?
                    .try_into()
                    .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(0, 16))?,
                device_seq: row.get(1)?,
                volume_id: RepositoryId::from_bytes(
                    row.get::<_, Vec<u8>>(2)?
                        .try_into()
                        .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(2, 16))?,
                ),
                base_commit: base
                    .map(|bytes| bytes.try_into())
                    .transpose()
                    .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(3, 32))?,
                kind: OperationKind::parse(&row.get::<_, String>(4)?)
                    .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(4, 4))?,
                payload: row.get(5)?,
                status: OperationStatus::parse(&row.get::<_, String>(6)?)
                    .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(6, 6))?,
                flush_group: row.get(7)?,
                depends_on: depends
                    .map(|bytes| bytes.try_into())
                    .transpose()
                    .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(8, 16))?,
                created_ns: row.get(9)?,
            })
        })
        .map_err(|e| sqlite(e, "replay scan failed"))?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|e| sqlite(e, "replay row decode failed"))
}

/// Payload ids referenced only by pending or reclaimed operations — safe to
/// delete during recovery. Payloads of committed/flushed/published
/// operations are never returned.
pub fn reclaimable_payloads(
    connection: &Connection,
    volume_id: RepositoryId,
) -> Result<Vec<[u8; 16]>, MirageError> {
    let mut statement = connection
        .prepare(
            "SELECT p.payload_id FROM operation_payloads p
             JOIN local_operations o ON o.operation_id = p.operation_id
             WHERE o.volume_id = ?1 AND o.status IN ('pending', 'reclaimed')",
        )
        .map_err(|e| sqlite(e, "reclaimable scan prepare failed"))?;
    let rows = statement
        .query_map([volume_id.as_bytes().as_slice()], |row| {
            row.get::<_, Vec<u8>>(0)?
                .try_into()
                .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(0, 16))
        })
        .map_err(|e| sqlite(e, "reclaimable scan failed"))?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|e| sqlite(e, "reclaimable row decode failed"))
}

/// Marks pending operations reclaimed once their payloads are gone; pending
/// operations were never acknowledged so reclamation is safe.
pub fn reclaim_pending(
    connection: &mut Connection,
    volume_id: RepositoryId,
    now_ns: i64,
) -> Result<u64, MirageError> {
    let _ = now_ns;
    let changed = connection
        .execute(
            "UPDATE local_operations SET status = 'reclaimed'
             WHERE volume_id = ?1 AND status = 'pending'",
            [volume_id.as_bytes().as_slice()],
        )
        .map_err(|e| sqlite(e, "pending operation reclaim failed"))?;
    u64::try_from(changed).map_err(|_| MirageError::internal_invariant("reclaim count overflowed"))
}

/// Allocates the next device-local sequence for the volume — durably, inside
/// the caller's transaction or its own. The counter lives in
/// `journal_sequences` so two interleaved callers can never observe the same
/// "next" value, and an abandoned sequence simply leaves a gap (gaps are
/// legal; duplicates are not). The counter never moves behind the journal's
/// recorded maximum, so compaction cannot make the allocator reuse a
/// sequence.
pub fn allocate_device_seq(
    connection: &mut Connection,
    volume_id: RepositoryId,
) -> Result<i64, MirageError> {
    let transaction = connection
        .transaction()
        .map_err(|e| sqlite(e, "failed to begin sequence allocation"))?;
    let journal_max: i64 = transaction
        .query_row(
            "SELECT COALESCE(MAX(device_seq) + 1, 0) FROM local_operations
             WHERE volume_id = ?1",
            [volume_id.as_bytes().as_slice()],
            |row| row.get(0),
        )
        .map_err(|e| sqlite(e, "device sequence lookup failed"))?;
    let allocated: i64 = transaction
        .query_row(
            "INSERT INTO journal_sequences (volume_id, next_seq) VALUES (?1, ?2)
             ON CONFLICT(volume_id) DO UPDATE SET
                next_seq = MAX(journal_sequences.next_seq, excluded.next_seq) + 1
             RETURNING next_seq - 1",
            params![volume_id.as_bytes().as_slice(), journal_max],
            |row| row.get(0),
        )
        .map_err(|e| sqlite(e, "device sequence allocation failed"))?;
    transaction
        .commit()
        .map_err(|e| sqlite(e, "sequence allocation commit failed"))?;
    Ok(allocated)
}

/// One extent mutation to land atomically with its journal operation.
pub struct ExtentMutation {
    pub volume_id: RepositoryId,
    pub inode: mirage_types::InodeId,
    pub version: i64,
    pub eof: u64,
    pub extents: Vec<crate::extent::ByteExtent>,
}

/// Atomic mutation commit: the extent replacement, the operation row, its
/// payload records (marked flushed — callers fsync the payload file before
/// this runs), and the pending→committed transition all land in a single
/// transaction. A crash can never leave durable metadata claiming extents
/// that were not recorded, or a committed operation whose payload references
/// are missing.
pub fn commit_mutation(
    connection: &mut Connection,
    extents: Option<&ExtentMutation>,
    operation: &OperationRecord,
    payloads: &[OperationPayloadRecord],
    physical: Option<&crate::physical::PhysicalCommit>,
    now_ns: i64,
) -> Result<(), MirageError> {
    if operation.status != OperationStatus::Pending {
        return Err(MirageError::invalid_argument(
            "a new operation must start pending",
        ));
    }
    let transaction = connection
        .transaction()
        .map_err(|e| sqlite(e, "failed to begin mutation commit"))?;
    if let Some(parent) = operation.depends_on {
        let parent_status: Option<String> = transaction
            .query_row(
                "SELECT status FROM local_operations WHERE operation_id = ?1",
                [parent.as_slice()],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| sqlite(e, "operation dependency lookup failed"))?;
        if parent_status.is_none() {
            return Err(MirageError::repository_conflict(
                "operation depends on a missing operation",
            ));
        }
    }
    transaction
        .execute(
            "INSERT INTO local_operations
             (operation_id, device_seq, volume_id, base_commit, kind, payload,
              status, flush_group, depends_on, created_ns)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'pending', ?7, ?8, ?9)",
            params![
                operation.operation_id.as_slice(),
                operation.device_seq,
                operation.volume_id.as_bytes().as_slice(),
                operation.base_commit.map(|bytes| bytes.to_vec()),
                operation.kind.as_str(),
                operation.payload,
                operation.flush_group,
                operation.depends_on.map(|bytes| bytes.to_vec()),
                operation.created_ns,
            ],
        )
        .map_err(|e| sqlite(e, "operation insert failed"))?;
    for payload in payloads {
        if payload.operation_id != operation.operation_id {
            return Err(MirageError::invalid_argument(
                "payload record belongs to a different operation",
            ));
        }
        // Payloads arrive durable: the staged file was fsynced before the
        // commit ran, so the durable-flush marker is recorded inline.
        transaction
            .execute(
                "INSERT INTO operation_payloads
                 (payload_id, operation_id, path, bytes, checksum, flushed_ns)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    payload.payload_id.as_slice(),
                    operation.operation_id.as_slice(),
                    payload.path,
                    payload.bytes,
                    payload.checksum.map(|bytes| bytes.to_vec()),
                    now_ns,
                ],
            )
            .map_err(|e| sqlite(e, "operation payload insert failed"))?;
    }
    if let Some(mutation) = extents {
        let mut previous_end = 0u64;
        for (index, extent) in mutation.extents.iter().enumerate() {
            extent.validate()?;
            if extent.volume_id != mutation.volume_id || extent.inode != mutation.inode {
                return Err(MirageError::invalid_argument(
                    "extent does not belong to the target inode",
                ));
            }
            if index > 0 && extent.start < previous_end {
                return Err(MirageError::integrity_mismatch(
                    "extents overlap within a version",
                ));
            }
            previous_end = extent.start.saturating_add(extent.length);
        }
        transaction
            .execute(
                "DELETE FROM byte_extents
                 WHERE volume_id = ?1 AND inode = ?2 AND version = ?3",
                params![
                    mutation.volume_id.as_bytes().as_slice(),
                    mutation.inode.as_bytes().as_slice(),
                    mutation.version,
                ],
            )
            .map_err(|e| sqlite(e, "extent replace delete failed"))?;
        for extent in &mutation.extents {
            transaction
                .execute(
                    "INSERT INTO byte_extents
                     (extent_id, volume_id, inode, version, start, length, kind,
                      page_hash, base_offset, payload_id, payload_offset, created_ns)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                    params![
                        extent.extent_id.as_slice(),
                        mutation.volume_id.as_bytes().as_slice(),
                        mutation.inode.as_bytes().as_slice(),
                        mutation.version,
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
        transaction
            .execute(
                "INSERT INTO byte_extent_heads (volume_id, inode, version, eof, updated_ns)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(volume_id, inode) DO UPDATE SET
                    version = excluded.version, eof = excluded.eof,
                    updated_ns = excluded.updated_ns
                 WHERE excluded.version > byte_extent_heads.version",
                params![
                    mutation.volume_id.as_bytes().as_slice(),
                    mutation.inode.as_bytes().as_slice(),
                    mutation.version,
                    mutation.eof as i64,
                    now_ns,
                ],
            )
            .map_err(|e| sqlite(e, "extent head write failed"))?;
    }
    let committed = transaction
        .execute(
            "UPDATE local_operations SET status = 'committed'
             WHERE operation_id = ?1 AND status = 'pending'",
            [operation.operation_id.as_slice()],
        )
        .map_err(|e| sqlite(e, "operation commit failed"))?;
    if committed != 1 {
        return Err(MirageError::repository_conflict("operation is not pending"));
    }
    if let Some(physical) = physical {
        // The dirty-payload ledger commits in the same transaction as the
        // mutation: the payload's bytes become ledger-visible exactly when
        // the extent rows referencing them do.
        let committed_extent = transaction
            .execute(
                "UPDATE physical_extents
                 SET state = 'alive', page_hash = ?1, checksum = ?2, updated_ns = ?3
                 WHERE extent_id = ?4 AND state = 'reserved'",
                params![
                    physical.page_hash.as_bytes().as_slice(),
                    physical.checksum.as_slice(),
                    now_ns,
                    physical.extent_id.as_slice(),
                ],
            )
            .map_err(|e| sqlite(e, "physical extent commit failed"))?;
        if committed_extent != 1 {
            return Err(MirageError::repository_conflict(
                "physical extent is not in a reservable state",
            ));
        }
        transaction
            .execute(
                "DELETE FROM physical_reservations WHERE extent_id = ?1",
                [physical.extent_id.as_slice()],
            )
            .map_err(|e| sqlite(e, "physical reservation release failed"))?;
    }
    transaction
        .commit()
        .map_err(|e| sqlite(e, "mutation commit failed"))
}

impl Database {
    /// Committed/flushed-but-unpublished operations for recovery replay.
    pub fn replayable_operations(
        &self,
        volume_id: RepositoryId,
    ) -> Result<Vec<OperationRecord>, MirageError> {
        self.reads()
            .with_connection(|connection| replayable(connection, volume_id))
    }

    /// Allocates the next device-local sequence for the volume's journal.
    /// The allocation is durable — serialized through the single writer —
    /// so two callers can never receive the same sequence.
    pub fn next_operation_seq(&self, volume_id: RepositoryId) -> Result<i64, MirageError> {
        self.writer().operation_alloc_seq(volume_id)
    }

    /// Payload ids whose operations never committed — safe to delete during
    /// recovery.
    pub fn reclaimable_operation_payloads(
        &self,
        volume_id: RepositoryId,
    ) -> Result<Vec<[u8; 16]>, MirageError> {
        self.reads()
            .with_connection(|connection| reclaimable_payloads(connection, volume_id))
    }
}
