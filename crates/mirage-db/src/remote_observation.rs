//! Remote-observation ledger: the newest remote head per volume, the durable
//! changes cursor (rejected if it moves backwards), and divergence records
//! that keep local and remote histories both visible after a split.

use mirage_types::{MirageError, RepositoryId};
use rusqlite::{Connection, OptionalExtension, params};

use crate::Database;
use crate::error::sqlite;

#[derive(Debug, Clone)]
pub struct RemoteHead {
    pub volume_id: RepositoryId,
    pub head_commit: [u8; 32],
    pub head_seq: i64,
    pub changes_cursor: String,
    pub observed_ns: i64,
}

#[derive(Debug, Clone)]
pub struct RemoteChange {
    pub volume_id: RepositoryId,
    pub cursor: String,
    pub change_kind: String,
    pub payload: Vec<u8>,
    pub observed_ns: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Divergence {
    pub volume_id: RepositoryId,
    pub base_commit: Option<[u8; 32]>,
    pub local_head: [u8; 32],
    pub remote_head: [u8; 32],
    pub status: DivergenceStatus,
    pub detected_ns: i64,
    pub resolved_ns: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DivergenceStatus {
    Diverged,
    Reconciled,
    Abandoned,
}

impl DivergenceStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Diverged => "diverged",
            Self::Reconciled => "reconciled",
            Self::Abandoned => "abandoned",
        }
    }
    fn parse(value: &str) -> Result<Self, MirageError> {
        match value {
            "diverged" => Ok(Self::Diverged),
            "reconciled" => Ok(Self::Reconciled),
            "abandoned" => Ok(Self::Abandoned),
            _ => Err(MirageError::integrity_mismatch(
                "divergence status is unknown",
            )),
        }
    }
}

/// Observes a remote head; the changes cursor must be monotonically
/// non-decreasing — a backwards cursor means the remote history was rewritten
/// and is rejected rather than adopted.
pub fn observe_head(connection: &mut Connection, head: &RemoteHead) -> Result<(), MirageError> {
    let existing: Option<String> = connection
        .query_row(
            "SELECT changes_cursor FROM remote_heads WHERE volume_id = ?1",
            [head.volume_id.as_bytes().as_slice()],
            |row| row.get(0),
        )
        .optional()
        .map_err(|e| sqlite(e, "remote head lookup failed"))?;
    if let Some(previous) = existing
        && head.changes_cursor < previous
    {
        return Err(MirageError::integrity_mismatch(
            "remote changes cursor moved backwards",
        ));
    }
    connection
        .execute(
            "INSERT INTO remote_heads(volume_id, head_commit, head_seq, changes_cursor, observed_ns)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(volume_id) DO UPDATE SET
                head_commit = excluded.head_commit,
                head_seq = excluded.head_seq,
                changes_cursor = excluded.changes_cursor,
                observed_ns = excluded.observed_ns",
            params![
                head.volume_id.as_bytes().as_slice(),
                head.head_commit.as_slice(),
                head.head_seq,
                head.changes_cursor,
                head.observed_ns,
            ],
        )
        .map_err(|e| sqlite(e, "remote head observe failed"))?;
    Ok(())
}

/// Records one remote change under its cursor; same-cursor replays are
/// idempotent.
pub fn record_change(
    connection: &mut Connection,
    change: &RemoteChange,
) -> Result<(), MirageError> {
    connection
        .execute(
            "INSERT OR IGNORE INTO remote_changes
             (volume_id, cursor, change_kind, payload, observed_ns)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                change.volume_id.as_bytes().as_slice(),
                change.cursor,
                change.change_kind,
                change.payload,
                change.observed_ns,
            ],
        )
        .map_err(|e| sqlite(e, "remote change record failed"))?;
    Ok(())
}

/// Changes after `cursor` in cursor order — the replay stream.
pub fn changes_after(
    connection: &Connection,
    volume_id: RepositoryId,
    cursor: &str,
) -> Result<Vec<RemoteChange>, MirageError> {
    let mut statement = connection
        .prepare(
            "SELECT volume_id, cursor, change_kind, payload, observed_ns
             FROM remote_changes WHERE volume_id = ?1 AND cursor > ?2
             ORDER BY cursor",
        )
        .map_err(|e| sqlite(e, "remote change scan prepare failed"))?;
    let rows = statement
        .query_map(params![volume_id.as_bytes().as_slice(), cursor], |row| {
            Ok(RemoteChange {
                volume_id: RepositoryId::from_bytes(
                    row.get::<_, Vec<u8>>(0)?
                        .try_into()
                        .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(0, 16))?,
                ),
                cursor: row.get(1)?,
                change_kind: row.get(2)?,
                payload: row.get(3)?,
                observed_ns: row.get(4)?,
            })
        })
        .map_err(|e| sqlite(e, "remote change scan failed"))?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|e| sqlite(e, "remote change row decode failed"))
}

/// Records a divergence: the local and remote heads share a base but neither
/// is the other's ancestor. Both histories stay visible; nothing is
/// overwritten.
pub fn record_divergence(
    connection: &mut Connection,
    divergence: &Divergence,
) -> Result<(), MirageError> {
    connection
        .execute(
            "INSERT INTO divergence_state
             (volume_id, base_commit, local_head, remote_head, status, detected_ns, resolved_ns)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL)
             ON CONFLICT(volume_id) DO UPDATE SET
                base_commit = excluded.base_commit,
                local_head = excluded.local_head,
                remote_head = excluded.remote_head,
                status = excluded.status,
                detected_ns = excluded.detected_ns,
                resolved_ns = excluded.resolved_ns",
            params![
                divergence.volume_id.as_bytes().as_slice(),
                divergence.base_commit.map(|bytes| bytes.to_vec()),
                divergence.local_head.as_slice(),
                divergence.remote_head.as_slice(),
                divergence.status.as_str(),
                divergence.detected_ns,
            ],
        )
        .map_err(|e| sqlite(e, "divergence record failed"))?;
    Ok(())
}

/// The volume's live divergence, if any.
pub fn divergence(
    connection: &Connection,
    volume_id: RepositoryId,
) -> Result<Option<Divergence>, MirageError> {
    connection
        .query_row(
            "SELECT base_commit, local_head, remote_head, status, detected_ns, resolved_ns
             FROM divergence_state WHERE volume_id = ?1",
            [volume_id.as_bytes().as_slice()],
            |row| {
                let base: Option<Vec<u8>> = row.get(0)?;
                Ok(Divergence {
                    volume_id,
                    base_commit: base
                        .map(|bytes| bytes.try_into())
                        .transpose()
                        .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(0, 32))?,
                    local_head: row
                        .get::<_, Vec<u8>>(1)?
                        .try_into()
                        .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(1, 32))?,
                    remote_head: row
                        .get::<_, Vec<u8>>(2)?
                        .try_into()
                        .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(2, 32))?,
                    status: DivergenceStatus::parse(&row.get::<_, String>(3)?)
                        .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(3, 4))?,
                    detected_ns: row.get(4)?,
                    resolved_ns: row.get(5)?,
                })
            },
        )
        .optional()
        .map_err(|e| sqlite(e, "divergence lookup failed"))
}

/// Marks a divergence reconciled without deleting the record — history stays.
pub fn resolve_divergence(
    connection: &mut Connection,
    volume_id: RepositoryId,
    status: DivergenceStatus,
    now_ns: i64,
) -> Result<(), MirageError> {
    if status == DivergenceStatus::Diverged {
        return Err(MirageError::invalid_argument(
            "a divergence cannot resolve back to diverged",
        ));
    }
    let changed = connection
        .execute(
            "UPDATE divergence_state SET status = ?1, resolved_ns = ?2
             WHERE volume_id = ?3 AND status = 'diverged'",
            params![status.as_str(), now_ns, volume_id.as_bytes().as_slice(),],
        )
        .map_err(|e| sqlite(e, "divergence resolve failed"))?;
    if changed == 0 {
        return Err(MirageError::repository_conflict(
            "no live divergence for the volume",
        ));
    }
    Ok(())
}

impl Database {
    /// The volume's live divergence, if any.
    pub fn live_divergence(
        &self,
        volume_id: RepositoryId,
    ) -> Result<Option<Divergence>, MirageError> {
        self.reads()
            .with_connection(|connection| divergence(connection, volume_id))
    }

    /// Remote changes after `cursor`, in order.
    pub fn remote_changes_after(
        &self,
        volume_id: RepositoryId,
        cursor: &str,
    ) -> Result<Vec<RemoteChange>, MirageError> {
        self.reads()
            .with_connection(|connection| changes_after(connection, volume_id, cursor))
    }
}
