//! Per-volume free-space floors and the bounded reclaim-run audit trail.

use mirage_types::MirageError;
use rusqlite::{Connection, OptionalExtension, params};

use crate::Database;
use crate::error::sqlite;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiskFloor {
    pub volume_root: String,
    pub floor_bytes: u64,
    pub hysteresis_bytes: u64,
    pub updated_ns: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiskFloorRun {
    pub volume_root: String,
    pub at_ns: i64,
    pub target_bytes: u64,
    pub freed_bytes: u64,
    pub outcome: String,
}

pub(crate) fn set_floor(connection: &mut Connection, floor: DiskFloor) -> Result<(), MirageError> {
    if floor.floor_bytes == 0 {
        return Err(MirageError::invalid_argument("disk floor must be positive"));
    }
    connection
        .execute(
            "INSERT INTO disk_floors(volume_root, floor_bytes, hysteresis_bytes, updated_ns)
             VALUES(?1, ?2, ?3, ?4)
             ON CONFLICT(volume_root) DO UPDATE SET
               floor_bytes = excluded.floor_bytes,
               hysteresis_bytes = excluded.hysteresis_bytes,
               updated_ns = excluded.updated_ns",
            params![
                floor.volume_root,
                i64::try_from(floor.floor_bytes)
                    .map_err(|_| MirageError::invalid_argument("disk floor overflows"))?,
                i64::try_from(floor.hysteresis_bytes).map_err(|_| {
                    MirageError::invalid_argument("disk floor hysteresis overflows")
                })?,
                floor.updated_ns,
            ],
        )
        .map_err(|e| sqlite(e, "disk floor upsert failed"))?;
    Ok(())
}

pub(crate) fn clear_floor(
    connection: &mut Connection,
    volume_root: &str,
) -> Result<bool, MirageError> {
    Ok(connection
        .execute(
            "DELETE FROM disk_floors WHERE volume_root = ?1",
            [volume_root],
        )
        .map_err(|e| sqlite(e, "disk floor delete failed"))?
        > 0)
}

/// Records one reclaim pass and prunes the audit trail to the newest 100 rows
/// per volume in the same transaction.
pub(crate) fn record_run(
    connection: &mut Connection,
    run: DiskFloorRun,
) -> Result<(), MirageError> {
    let transaction = connection
        .transaction()
        .map_err(|e| sqlite(e, "disk floor run transaction failed"))?;
    transaction
        .execute(
            "INSERT INTO disk_floor_runs(volume_root, at_ns, target_bytes, freed_bytes, outcome)
             VALUES(?1, ?2, ?3, ?4, ?5)",
            params![
                run.volume_root,
                run.at_ns,
                i64::try_from(run.target_bytes)
                    .map_err(|_| MirageError::invalid_argument("reclaim target overflows"))?,
                i64::try_from(run.freed_bytes)
                    .map_err(|_| MirageError::invalid_argument("reclaim freed overflows"))?,
                run.outcome,
            ],
        )
        .map_err(|e| sqlite(e, "disk floor run insert failed"))?;
    transaction
        .execute(
            "DELETE FROM disk_floor_runs WHERE volume_root = ?1 AND id NOT IN (
               SELECT id FROM disk_floor_runs WHERE volume_root = ?1
               ORDER BY at_ns DESC, id DESC LIMIT 100)",
            [&run.volume_root],
        )
        .map_err(|e| sqlite(e, "disk floor run prune failed"))?;
    transaction
        .commit()
        .map_err(|e| sqlite(e, "disk floor run commit failed"))
}

fn decode_floor(row: &rusqlite::Row<'_>) -> rusqlite::Result<DiskFloor> {
    Ok(DiskFloor {
        volume_root: row.get(0)?,
        floor_bytes: row.get::<_, i64>(1)? as u64,
        hysteresis_bytes: row.get::<_, i64>(2)? as u64,
        updated_ns: row.get(3)?,
    })
}

fn decode_run(row: &rusqlite::Row<'_>) -> rusqlite::Result<DiskFloorRun> {
    Ok(DiskFloorRun {
        volume_root: row.get(0)?,
        at_ns: row.get(1)?,
        target_bytes: row.get::<_, i64>(2)? as u64,
        freed_bytes: row.get::<_, i64>(3)? as u64,
        outcome: row.get(4)?,
    })
}

impl Database {
    pub fn set_disk_floor(&self, floor: DiskFloor) -> Result<(), MirageError> {
        self.writer().disk_floor_set(floor)
    }

    pub fn clear_disk_floor(&self, volume_root: &str) -> Result<bool, MirageError> {
        self.writer().disk_floor_clear(volume_root.to_owned())
    }

    pub fn record_disk_floor_run(&self, run: DiskFloorRun) -> Result<(), MirageError> {
        self.writer().disk_floor_record_run(run)
    }

    pub fn disk_floors(&self) -> Result<Vec<DiskFloor>, MirageError> {
        self.reads().with_connection(|connection| {
            let mut statement = connection
                .prepare(
                    "SELECT volume_root, floor_bytes, hysteresis_bytes, updated_ns
                     FROM disk_floors ORDER BY volume_root",
                )
                .map_err(|e| sqlite(e, "disk floor query failed"))?;
            let rows = statement
                .query_map([], decode_floor)
                .map_err(|e| sqlite(e, "disk floor query failed"))?;
            rows.collect::<Result<Vec<_>, _>>()
                .map_err(|e| sqlite(e, "disk floor row decode failed"))
        })
    }

    pub fn disk_floor(&self, volume_root: &str) -> Result<Option<DiskFloor>, MirageError> {
        self.reads().with_connection(|connection| {
            connection
                .query_row(
                    "SELECT volume_root, floor_bytes, hysteresis_bytes, updated_ns
                     FROM disk_floors WHERE volume_root = ?1",
                    [volume_root],
                    decode_floor,
                )
                .optional()
                .map_err(|e| sqlite(e, "disk floor lookup failed"))
        })
    }

    pub fn latest_disk_floor_run(
        &self,
        volume_root: &str,
    ) -> Result<Option<DiskFloorRun>, MirageError> {
        self.reads().with_connection(|connection| {
            connection
                .query_row(
                    "SELECT volume_root, at_ns, target_bytes, freed_bytes, outcome
                     FROM disk_floor_runs WHERE volume_root = ?1
                     ORDER BY at_ns DESC, id DESC LIMIT 1",
                    [volume_root],
                    decode_run,
                )
                .optional()
                .map_err(|e| sqlite(e, "disk floor run lookup failed"))
        })
    }
}
