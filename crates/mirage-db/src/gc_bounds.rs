//! Bounded-history retention bounds and unreachable-object bookkeeping.
//! Retention bounds bound checkpoint/delta/journal growth; unreachable
//! candidates record objects provably unreachable from the retained commit
//! set, with removal gated on a later verified commit plus a reclamation
//! window.

use mirage_types::{MirageError, RepositoryId};
use rusqlite::{Connection, OptionalExtension, params};

use crate::Database;
use crate::error::sqlite;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GcKind {
    NamespaceHistory,
    Journal,
    PendingUploads,
    RemoteObjects,
}

impl GcKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NamespaceHistory => "namespace_history",
            Self::Journal => "journal",
            Self::PendingUploads => "pending_uploads",
            Self::RemoteObjects => "remote_objects",
        }
    }
}

#[derive(Debug, Clone)]
pub struct GcBound {
    pub volume_id: RepositoryId,
    pub kind: GcKind,
    /// Boundary value (e.g. retain deltas at/after this checkpoint seq).
    pub retain_from: i64,
    /// Entries always kept regardless of boundary.
    pub min_keep: i64,
    pub set_ns: i64,
}

#[derive(Debug, Clone)]
pub struct UnreachableCandidate {
    pub volume_id: RepositoryId,
    pub object_key: String,
    pub unreachable_at_commit: [u8; 32],
    pub detected_ns: i64,
    pub reclaim_after_ns: i64,
}

/// Sets (idempotently) the retention bound for a history stream.
pub fn set_bound(connection: &mut Connection, bound: &GcBound) -> Result<(), MirageError> {
    connection
        .execute(
            "INSERT INTO gc_bounds(volume_id, kind, retain_from, min_keep, set_ns)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(volume_id, kind) DO UPDATE SET
                retain_from = excluded.retain_from,
                min_keep = excluded.min_keep,
                set_ns = excluded.set_ns",
            params![
                bound.volume_id.as_bytes().as_slice(),
                bound.kind.as_str(),
                bound.retain_from,
                bound.min_keep,
                bound.set_ns,
            ],
        )
        .map_err(|e| sqlite(e, "gc bound write failed"))?;
    Ok(())
}

/// The retention bound for a stream, if any.
pub fn bound(
    connection: &Connection,
    volume_id: RepositoryId,
    kind: GcKind,
) -> Result<Option<GcBound>, MirageError> {
    connection
        .query_row(
            "SELECT retain_from, min_keep, set_ns FROM gc_bounds
             WHERE volume_id = ?1 AND kind = ?2",
            params![volume_id.as_bytes().as_slice(), kind.as_str()],
            |row| {
                Ok(GcBound {
                    volume_id,
                    kind,
                    retain_from: row.get(0)?,
                    min_keep: row.get(1)?,
                    set_ns: row.get(2)?,
                })
            },
        )
        .optional()
        .map_err(|e| sqlite(e, "gc bound lookup failed"))
}

/// Records a completed sweep for audit.
pub fn record_run(
    connection: &mut Connection,
    volume_id: RepositoryId,
    kind: GcKind,
    boundary: i64,
    reclaimed: u64,
    started_ns: i64,
    now_ns: i64,
) -> Result<(), MirageError> {
    let mut run_id = [0u8; 16];
    let _ = getrandom::fill(&mut run_id);
    connection
        .execute(
            "INSERT INTO gc_runs
             (run_id, volume_id, kind, boundary, reclaimed_count, started_ns, completed_ns)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                run_id.as_slice(),
                volume_id.as_bytes().as_slice(),
                kind.as_str(),
                boundary,
                reclaimed as i64,
                started_ns,
                now_ns,
            ],
        )
        .map_err(|e| sqlite(e, "gc run record failed"))?;
    Ok(())
}

/// Marks an object unreachable at a verified commit; physical removal is
/// only legal once a later verified commit exists AND `now >= reclaim_after`.
pub fn mark_unreachable(
    connection: &mut Connection,
    volume_id: RepositoryId,
    object_key: &str,
    at_commit: [u8; 32],
    reclaim_after_ns: i64,
    now_ns: i64,
) -> Result<(), MirageError> {
    connection
        .execute(
            "INSERT OR REPLACE INTO unreachable_candidates
             (volume_id, object_key, unreachable_at_commit, detected_ns, reclaim_after_ns)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                volume_id.as_bytes().as_slice(),
                object_key,
                at_commit.as_slice(),
                now_ns,
                reclaim_after_ns,
            ],
        )
        .map_err(|e| sqlite(e, "unreachable mark failed"))?;
    Ok(())
}

/// Candidates safe to physically remove: a verified commit strictly newer
/// than `unreachable_at_commit` exists and the window elapsed.
pub fn reclaimable(
    connection: &Connection,
    volume_id: RepositoryId,
    latest_verified_commit: &[u8; 32],
    now_ns: i64,
) -> Result<Vec<String>, MirageError> {
    let mut statement = connection
        .prepare(
            "SELECT object_key FROM unreachable_candidates
             WHERE volume_id = ?1 AND unreachable_at_commit != ?2
               AND reclaim_after_ns <= ?3
             ORDER BY object_key",
        )
        .map_err(|e| sqlite(e, "reclaimable scan prepare failed"))?;
    let rows = statement
        .query_map(
            params![
                volume_id.as_bytes().as_slice(),
                latest_verified_commit.as_slice(),
                now_ns,
            ],
            |row| row.get::<_, String>(0),
        )
        .map_err(|e| sqlite(e, "reclaimable scan failed"))?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|e| sqlite(e, "reclaimable row decode failed"))
}

/// Drops the candidate once the remote object is gone.
pub fn clear_unreachable(
    connection: &mut Connection,
    volume_id: RepositoryId,
    object_key: &str,
) -> Result<(), MirageError> {
    connection
        .execute(
            "DELETE FROM unreachable_candidates WHERE volume_id = ?1 AND object_key = ?2",
            params![volume_id.as_bytes().as_slice(), object_key],
        )
        .map_err(|e| sqlite(e, "unreachable clear failed"))?;
    Ok(())
}

impl Database {
    /// The retention bound for a history stream.
    pub fn gc_bound(
        &self,
        volume_id: RepositoryId,
        kind: GcKind,
    ) -> Result<Option<GcBound>, MirageError> {
        self.reads()
            .with_connection(|connection| bound(connection, volume_id, kind))
    }

    /// Candidates safe to physically remove now.
    pub fn gc_reclaimable(
        &self,
        volume_id: RepositoryId,
        latest_verified_commit: &[u8; 32],
        now_ns: i64,
    ) -> Result<Vec<String>, MirageError> {
        self.reads().with_connection(|connection| {
            reclaimable(connection, volume_id, latest_verified_commit, now_ns)
        })
    }
}
