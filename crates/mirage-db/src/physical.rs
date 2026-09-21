//! Physical allocation ledger: arena files, extents, and durable
//! reservations. A reservation row must exist before any bytes are written
//! to the arena, and residency is only claimed once the extent commits.

use mirage_types::{MirageError, PageHash};
use rusqlite::{Connection, OptionalExtension, params};

use crate::Database;
use crate::error::sqlite;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PhysicalExtentState {
    Reserved,
    Alive,
    Dead,
    Evicting,
}

impl PhysicalExtentState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Reserved => "reserved",
            Self::Alive => "alive",
            Self::Dead => "dead",
            Self::Evicting => "evicting",
        }
    }
    fn parse(value: &str) -> Result<Self, MirageError> {
        match value {
            "reserved" => Ok(Self::Reserved),
            "alive" => Ok(Self::Alive),
            "dead" => Ok(Self::Dead),
            "evicting" => Ok(Self::Evicting),
            _ => Err(MirageError::integrity_mismatch(
                "physical extent state is unknown",
            )),
        }
    }
}

#[derive(Debug, Clone)]
pub struct PhysicalFileRecord {
    pub file_id: [u8; 16],
    pub path: String,
    pub zone: i64,
    pub extent_bytes: i64,
    pub extent_count: i64,
    pub created_ns: i64,
}

#[derive(Debug, Clone)]
pub struct PhysicalExtentRecord {
    pub extent_id: [u8; 16],
    pub file_id: [u8; 16],
    pub slot_index: i64,
    pub length_bytes: i64,
    pub state: PhysicalExtentState,
    pub page_hash: Option<PageHash>,
    pub checksum: Option<[u8; 32]>,
    pub pin_count: i64,
    pub generation: i64,
    pub updated_ns: i64,
}

#[derive(Debug, Clone)]
pub struct PhysicalReservationRecord {
    pub extent_id: [u8; 16],
    pub owner_epoch: [u8; 16],
    pub expires_ns: i64,
}

/// A reserved extent to commit alive inside a larger transaction — used by
/// mutation commits so the physical ledger and the extent journal atomically
/// agree on a staged payload.
#[derive(Debug, Clone, Copy)]
pub struct PhysicalCommit {
    pub extent_id: [u8; 16],
    pub page_hash: PageHash,
    pub checksum: [u8; 32],
}

fn decode_extent(row: &rusqlite::Row<'_>) -> Result<(PhysicalExtentRecord,), rusqlite::Error> {
    let page_hash: Option<Vec<u8>> = row.get(5)?;
    let checksum: Option<Vec<u8>> = row.get(6)?;
    Ok((PhysicalExtentRecord {
        extent_id: row
            .get::<_, Vec<u8>>(0)?
            .try_into()
            .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(0, 16))?,
        file_id: row
            .get::<_, Vec<u8>>(1)?
            .try_into()
            .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(1, 16))?,
        slot_index: row.get(2)?,
        length_bytes: row.get(3)?,
        state: PhysicalExtentState::Reserved, // decoded below
        page_hash: page_hash
            .map(|bytes| bytes.try_into().map(PageHash::from_bytes))
            .transpose()
            .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(5, 32))?,
        checksum: checksum
            .map(|bytes| bytes.try_into())
            .transpose()
            .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(6, 32))?,
        pin_count: row.get(7)?,
        generation: row.get(8)?,
        updated_ns: row.get(9)?,
    },))
}

const EXTENT_COLUMNS: &str =
    "extent_id, file_id, slot_index, length_bytes, state, page_hash, checksum,
     pin_count, generation, updated_ns";

fn extent_from_row(row: &rusqlite::Row<'_>) -> Result<PhysicalExtentRecord, rusqlite::Error> {
    let (mut extent,) = decode_extent(row)?;
    extent.state = PhysicalExtentState::parse(&row.get::<_, String>(4)?)
        .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(4, 4))?;
    Ok(extent)
}

/// Registers or replaces an arena file description.
pub fn register_file(
    connection: &mut Connection,
    file: &PhysicalFileRecord,
) -> Result<(), MirageError> {
    connection
        .execute(
            "INSERT OR REPLACE INTO physical_files
             (file_id, path, zone, extent_bytes, extent_count, created_ns)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                file.file_id.as_slice(),
                file.path,
                file.zone,
                file.extent_bytes,
                file.extent_count,
                file.created_ns,
            ],
        )
        .map_err(|e| sqlite(e, "physical file registration failed"))?;
    Ok(())
}

/// Durably reserves an extent slot inside an arena. This row must exist
/// before any write touches the covered bytes.
pub fn reserve_extent(
    connection: &mut Connection,
    extent: &PhysicalExtentRecord,
    reservation: &PhysicalReservationRecord,
) -> Result<(), MirageError> {
    if extent.state != PhysicalExtentState::Reserved {
        return Err(MirageError::invalid_argument(
            "a new physical extent must start reserved",
        ));
    }
    let transaction = connection
        .transaction()
        .map_err(|e| sqlite(e, "failed to begin physical reservation"))?;
    // Free or dead slots may be reused; any other state conflicts.
    let occupying: Option<String> = transaction
        .query_row(
            "SELECT state FROM physical_extents WHERE file_id = ?1 AND slot_index = ?2",
            params![extent.file_id.as_slice(), extent.slot_index],
            |row| row.get(0),
        )
        .optional()
        .map_err(|e| sqlite(e, "physical slot lookup failed"))?;
    match occupying.as_deref() {
        None => {
            transaction
                .execute(
                    &format!(
                        "INSERT INTO physical_extents({EXTENT_COLUMNS})
                         VALUES (?1, ?2, ?3, ?4, 'reserved', NULL, NULL, 0, ?5, ?6)"
                    ),
                    params![
                        extent.extent_id.as_slice(),
                        extent.file_id.as_slice(),
                        extent.slot_index,
                        extent.length_bytes,
                        extent.generation,
                        extent.updated_ns,
                    ],
                )
                .map_err(|e| sqlite(e, "physical extent insert failed"))?;
        }
        Some("dead") => {
            transaction
                .execute(
                    "UPDATE physical_extents SET extent_id = ?1, state = 'reserved',
                        length_bytes = ?2, page_hash = NULL, checksum = NULL,
                        pin_count = 0, generation = ?3, updated_ns = ?4
                     WHERE file_id = ?5 AND slot_index = ?6 AND state = 'dead'",
                    params![
                        extent.extent_id.as_slice(),
                        extent.length_bytes,
                        extent.generation,
                        extent.updated_ns,
                        extent.file_id.as_slice(),
                        extent.slot_index,
                    ],
                )
                .map_err(|e| sqlite(e, "physical extent reclaim failed"))?;
        }
        Some(_) => {
            return Err(MirageError::repository_conflict(
                "physical slot is already occupied",
            ));
        }
    }
    transaction
        .execute(
            "INSERT OR REPLACE INTO physical_reservations(extent_id, owner_epoch, created_ns, expires_ns)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                extent.extent_id.as_slice(),
                reservation.owner_epoch.as_slice(),
                extent.updated_ns,
                reservation.expires_ns,
            ],
        )
        .map_err(|e| sqlite(e, "physical reservation insert failed"))?;
    transaction
        .commit()
        .map_err(|e| sqlite(e, "physical reservation commit failed"))
}

/// Commits a reserved extent to alive with its content hash and checksum;
/// only after this does the resident page exist for readers.
pub fn commit_extent(
    connection: &mut Connection,
    extent_id: &[u8; 16],
    page_hash: PageHash,
    checksum: [u8; 32],
    now_ns: i64,
) -> Result<(), MirageError> {
    let transaction = connection
        .transaction()
        .map_err(|e| sqlite(e, "failed to begin physical commit"))?;
    let changed = transaction
        .execute(
            "UPDATE physical_extents
             SET state = 'alive', page_hash = ?1, checksum = ?2, updated_ns = ?3
             WHERE extent_id = ?4 AND state = 'reserved'",
            params![
                page_hash.as_bytes().as_slice(),
                checksum.as_slice(),
                now_ns,
                extent_id.as_slice(),
            ],
        )
        .map_err(|e| sqlite(e, "physical extent commit failed"))?;
    if changed != 1 {
        return Err(MirageError::repository_conflict(
            "physical extent is not in a reservable state",
        ));
    }
    transaction
        .execute(
            "DELETE FROM physical_reservations WHERE extent_id = ?1",
            [extent_id.as_slice()],
        )
        .map_err(|e| sqlite(e, "physical reservation release failed"))?;
    transaction
        .commit()
        .map_err(|e| sqlite(e, "physical commit failed"))
}

/// Releases a reservation without committing bytes; the slot becomes dead.
/// This is the reservation-release path only — a live extent is never
/// killed through this API (eviction goes through `mark_extent_dead`, which
/// enforces the pin fence), and a pinned extent is refused even in the
/// reserved state.
pub fn release_extent(
    connection: &mut Connection,
    extent_id: &[u8; 16],
    now_ns: i64,
) -> Result<(), MirageError> {
    let transaction = connection
        .transaction()
        .map_err(|e| sqlite(e, "failed to begin physical release"))?;
    let changed = transaction
        .execute(
            "UPDATE physical_extents SET state = 'dead', updated_ns = ?1
             WHERE extent_id = ?2 AND state = 'reserved' AND pin_count = 0",
            params![now_ns, extent_id.as_slice()],
        )
        .map_err(|e| sqlite(e, "physical extent release failed"))?;
    if changed != 1 {
        // Either the extent is alive (committed — its bytes may be
        // referenced) or it is pinned; releasing either would lose live
        // data. Report the distinction honestly.
        let state: Option<String> = transaction
            .query_row(
                "SELECT state FROM physical_extents WHERE extent_id = ?1",
                [extent_id.as_slice()],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| sqlite(e, "physical extent lookup failed"))?;
        return Err(match state.as_deref() {
            Some("alive") => MirageError::repository_conflict(
                "committed extent cannot be released through the reservation path",
            ),
            Some("reserved") => MirageError::repository_conflict("reserved extent is pinned"),
            Some("dead") => MirageError::repository_conflict("extent is already dead"),
            Some(_) => MirageError::repository_conflict("extent is not releasable"),
            None => MirageError::repository_conflict("extent is missing"),
        });
    }
    transaction
        .execute(
            "DELETE FROM physical_reservations WHERE extent_id = ?1",
            [extent_id.as_slice()],
        )
        .map_err(|e| sqlite(e, "physical reservation release failed"))?;
    transaction
        .commit()
        .map_err(|e| sqlite(e, "physical release commit failed"))
}

/// Marks an extent dead after its bytes are no longer referenced. A pinned
/// extent cannot be transitioned.
pub fn mark_extent_dead(
    connection: &mut Connection,
    extent_id: &[u8; 16],
    now_ns: i64,
) -> Result<(), MirageError> {
    let changed = connection
        .execute(
            "UPDATE physical_extents SET state = 'dead', updated_ns = ?1
             WHERE extent_id = ?2 AND state = 'alive' AND pin_count = 0",
            params![now_ns, extent_id.as_slice()],
        )
        .map_err(|e| sqlite(e, "physical extent eviction failed"))?;
    if changed != 1 {
        return Err(MirageError::repository_conflict(
            "physical extent is pinned or not alive",
        ));
    }
    Ok(())
}

/// Pins or unpins an extent; a nonzero pin count locks it against eviction.
pub fn adjust_extent_pin(
    connection: &mut Connection,
    extent_id: &[u8; 16],
    delta: i64,
) -> Result<(), MirageError> {
    let changed = connection
        .execute(
            "UPDATE physical_extents SET pin_count = pin_count + ?1
             WHERE extent_id = ?2 AND state = 'alive' AND pin_count + ?1 >= 0",
            params![delta, extent_id.as_slice()],
        )
        .map_err(|e| sqlite(e, "physical extent pin update failed"))?;
    if changed != 1 {
        return Err(MirageError::repository_conflict(
            "physical extent pin cannot be applied",
        ));
    }
    Ok(())
}

/// Reaps reservations whose deadline passed; their extents become dead so
/// the slots can be reused by later allocations.
pub fn reap_expired_reservations(
    connection: &mut Connection,
    now_ns: i64,
) -> Result<u64, MirageError> {
    let transaction = connection
        .transaction()
        .map_err(|e| sqlite(e, "failed to begin reservation reap"))?;
    let expired: Vec<Vec<u8>> = transaction
        .prepare("SELECT extent_id FROM physical_reservations WHERE expires_ns <= ?1")
        .and_then(|mut statement| {
            statement
                .query_map([now_ns], |row| row.get::<_, Vec<u8>>(0))?
                .collect::<Result<Vec<_>, _>>()
        })
        .map_err(|e| sqlite(e, "expired reservation scan failed"))?;
    for extent_id in &expired {
        transaction
            .execute(
                "UPDATE physical_extents SET state = 'dead', updated_ns = ?1
                 WHERE extent_id = ?2 AND state = 'reserved'",
                params![now_ns, extent_id.as_slice()],
            )
            .map_err(|e| sqlite(e, "expired reservation extent release failed"))?;
        transaction
            .execute(
                "DELETE FROM physical_reservations WHERE extent_id = ?1",
                [extent_id.as_slice()],
            )
            .map_err(|e| sqlite(e, "expired reservation delete failed"))?;
    }
    let count = u64::try_from(expired.len())
        .map_err(|_| MirageError::internal_invariant("reservation reap count overflowed"))?;
    transaction
        .commit()
        .map_err(|e| sqlite(e, "reservation reap commit failed"))?;
    Ok(count)
}

/// All physical allocation state for in-memory index replay.
pub type PhysicalState = (
    Vec<PhysicalFileRecord>,
    Vec<PhysicalExtentRecord>,
    Vec<PhysicalReservationRecord>,
);

/// Loads all arena files, extents, and open reservations for replay into the
/// in-memory allocation index at startup.
pub fn load_physical_state(connection: &Connection) -> Result<PhysicalState, MirageError> {
    let files: Vec<PhysicalFileRecord> = {
        let mut statement = connection
            .prepare(
                "SELECT file_id, path, zone, extent_bytes, extent_count, created_ns
                 FROM physical_files",
            )
            .map_err(|e| sqlite(e, "physical file scan prepare failed"))?;
        statement
            .query_map([], |row| {
                Ok(PhysicalFileRecord {
                    file_id: row
                        .get::<_, Vec<u8>>(0)?
                        .try_into()
                        .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(0, 16))?,
                    path: row.get(1)?,
                    zone: row.get(2)?,
                    extent_bytes: row.get(3)?,
                    extent_count: row.get(4)?,
                    created_ns: row.get(5)?,
                })
            })
            .map_err(|e| sqlite(e, "physical file scan failed"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| sqlite(e, "physical file row failed"))?
    };
    let extents: Vec<PhysicalExtentRecord> = {
        let mut statement = connection
            .prepare(&format!("SELECT {EXTENT_COLUMNS} FROM physical_extents"))
            .map_err(|e| sqlite(e, "physical extent scan prepare failed"))?;
        statement
            .query_map([], extent_from_row)
            .map_err(|e| sqlite(e, "physical extent scan failed"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| sqlite(e, "physical extent row failed"))?
    };
    let reservations: Vec<PhysicalReservationRecord> = {
        let mut statement = connection
            .prepare("SELECT extent_id, owner_epoch, expires_ns FROM physical_reservations")
            .map_err(|e| sqlite(e, "physical reservation scan prepare failed"))?;
        statement
            .query_map([], |row| {
                Ok(PhysicalReservationRecord {
                    extent_id: row
                        .get::<_, Vec<u8>>(0)?
                        .try_into()
                        .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(0, 16))?,
                    owner_epoch: row
                        .get::<_, Vec<u8>>(1)?
                        .try_into()
                        .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(1, 16))?,
                    expires_ns: row.get(2)?,
                })
            })
            .map_err(|e| sqlite(e, "physical reservation scan failed"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| sqlite(e, "physical reservation row failed"))?
    };
    Ok((files, extents, reservations))
}

impl Database {
    /// Loads all physical allocation state for in-memory index replay.
    pub fn load_physical_state(&self) -> Result<PhysicalState, MirageError> {
        self.reads().with_connection(load_physical_state)
    }
}
