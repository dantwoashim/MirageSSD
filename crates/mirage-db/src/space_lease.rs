use mirage_types::{MirageError, RepositoryId, SpaceLeaseId};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

use crate::Database;
use crate::error::sqlite;
use crate::value::{fixed, nonnegative, sqlite_integer};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpaceLeaseState {
    Preparing,
    Ready,
    Consumed,
    Released,
    Failed,
}

impl SpaceLeaseState {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Preparing => "preparing",
            Self::Ready => "ready",
            Self::Consumed => "consumed",
            Self::Released => "released",
            Self::Failed => "failed",
        }
    }

    fn parse(value: &str) -> Result<Self, MirageError> {
        match value {
            "preparing" => Ok(Self::Preparing),
            "ready" => Ok(Self::Ready),
            "consumed" => Ok(Self::Consumed),
            "released" => Ok(Self::Released),
            "failed" => Ok(Self::Failed),
            _ => Err(MirageError::integrity_mismatch(
                "stored Space Lease state is invalid",
            )),
        }
    }

    #[must_use]
    pub const fn active(self) -> bool {
        matches!(self, Self::Preparing | Self::Ready | Self::Consumed)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpaceLeaseEvent {
    ReclaimFinished,
    Consume,
    Release,
    Fail,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewSpaceLease {
    pub lease_id: SpaceLeaseId,
    pub repository_id: RepositoryId,
    pub target_volume_id: String,
    pub requested_bytes: u64,
    pub planned_reclaim_bytes: u64,
    pub created_at_ns: i64,
    pub expires_at_ns: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpaceLeaseRecord {
    pub lease_id: SpaceLeaseId,
    pub repository_id: RepositoryId,
    pub target_volume_id: String,
    pub requested_bytes: u64,
    pub planned_reclaim_bytes: u64,
    pub state: SpaceLeaseState,
    pub created_at_ns: i64,
    pub updated_at_ns: i64,
    pub expires_at_ns: i64,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct SpaceLeaseTransition {
    pub lease_id: SpaceLeaseId,
    pub expected: SpaceLeaseState,
    pub event: SpaceLeaseEvent,
    pub at_ns: i64,
}

impl Database {
    pub fn create_space_lease(&self, lease: NewSpaceLease) -> Result<(), MirageError> {
        self.writer.create_space_lease(lease)
    }

    pub fn transition_space_lease(
        &self,
        lease_id: SpaceLeaseId,
        expected: SpaceLeaseState,
        event: SpaceLeaseEvent,
        at_ns: i64,
    ) -> Result<SpaceLeaseState, MirageError> {
        self.writer.transition_space_lease(SpaceLeaseTransition {
            lease_id,
            expected,
            event,
            at_ns,
        })
    }

    pub fn load_space_lease(
        &self,
        lease_id: SpaceLeaseId,
    ) -> Result<Option<SpaceLeaseRecord>, MirageError> {
        self.reads.with_connection(|connection| {
            connection
                .query_row(
                    "SELECT repository_id, target_volume_id, requested_bytes, planned_reclaim_bytes, state, \
                            created_at_ns, updated_at_ns, expires_at_ns \
                     FROM space_leases WHERE lease_id = ?1",
                    [lease_id.as_bytes().as_slice()],
                    |row| {
                        Ok((
                            row.get::<_, Vec<u8>>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, i64>(2)?,
                            row.get::<_, i64>(3)?,
                            row.get::<_, String>(4)?,
                            row.get::<_, i64>(5)?,
                            row.get::<_, i64>(6)?,
                            row.get::<_, i64>(7)?,
                        ))
                    },
                )
                .optional()
                .map_err(|error| sqlite(error, "failed to load Space Lease"))?
                .map(
                    |(
                        repository_id,
                        target_volume_id,
                        requested_bytes,
                        planned_reclaim_bytes,
                        state,
                        created_at_ns,
                        updated_at_ns,
                        expires_at_ns,
                    )| {
                        Ok(SpaceLeaseRecord {
                            lease_id,
                            repository_id: RepositoryId::from_bytes(fixed(
                                repository_id,
                                "Space Lease repository ID",
                            )?),
                            target_volume_id,
                            requested_bytes: nonnegative(
                                requested_bytes,
                                "Space Lease requested bytes",
                            )?,
                            planned_reclaim_bytes: nonnegative(
                                planned_reclaim_bytes,
                                "Space Lease planned reclaim bytes",
                            )?,
                            state: SpaceLeaseState::parse(&state)?,
                            created_at_ns,
                            updated_at_ns,
                            expires_at_ns,
                        })
                    },
                )
                .transpose()
        })
    }

    pub fn active_space_lease_bytes(
        &self,
        repository_id: RepositoryId,
    ) -> Result<u64, MirageError> {
        self.reads.with_connection(|connection| {
            let bytes: i64 = connection
                .query_row(
                    "SELECT coalesce(sum(requested_bytes), 0) FROM space_leases \
                     WHERE repository_id = ?1 AND state IN ('preparing', 'ready', 'consumed')",
                    [repository_id.as_bytes().as_slice()],
                    |row| row.get(0),
                )
                .map_err(|error| sqlite(error, "failed to total active Space Leases"))?;
            nonnegative(bytes, "active Space Lease bytes")
        })
    }

    pub fn active_space_lease_bytes_for_volume(
        &self,
        target_volume_id: &str,
        at_ns: i64,
    ) -> Result<u64, MirageError> {
        if target_volume_id.is_empty() || target_volume_id.len() > 256 || at_ns < 0 {
            return Err(MirageError::invalid_argument(
                "Space Lease volume or accounting time is invalid",
            ));
        }
        self.reads.with_connection(|connection| {
            let bytes: i64 = connection
                .query_row(
                    "SELECT coalesce(sum(requested_bytes), 0) FROM space_leases \
                     WHERE target_volume_id = ?1 \
                       AND state IN ('preparing', 'ready', 'consumed') \
                       AND expires_at_ns > ?2",
                    params![target_volume_id, at_ns],
                    |row| row.get(0),
                )
                .map_err(|error| sqlite(error, "failed to total active volume Space Leases"))?;
            nonnegative(bytes, "active volume Space Lease bytes")
        })
    }
}

pub(crate) fn create(connection: &mut Connection, lease: NewSpaceLease) -> Result<(), MirageError> {
    if lease.requested_bytes == 0 {
        return Err(MirageError::invalid_argument(
            "Space Lease request must be greater than zero bytes",
        ));
    }
    if lease.target_volume_id.is_empty() || lease.target_volume_id.len() > 256 {
        return Err(MirageError::invalid_argument(
            "Space Lease target volume ID is invalid",
        ));
    }
    if lease.created_at_ns < 0 || lease.expires_at_ns <= lease.created_at_ns {
        return Err(MirageError::invalid_argument(
            "Space Lease timestamps are invalid",
        ));
    }
    connection
        .execute(
            "INSERT INTO space_leases( \
                lease_id, repository_id, target_volume_id, requested_bytes, planned_reclaim_bytes, state, \
                created_at_ns, updated_at_ns, expires_at_ns \
             ) VALUES (?1, ?2, ?3, ?4, ?5, 'preparing', ?6, ?6, ?7)",
            params![
                lease.lease_id.as_bytes().as_slice(),
                lease.repository_id.as_bytes().as_slice(),
                lease.target_volume_id,
                sqlite_integer(lease.requested_bytes, "Space Lease requested bytes")?,
                sqlite_integer(
                    lease.planned_reclaim_bytes,
                    "Space Lease planned reclaim bytes",
                )?,
                lease.created_at_ns,
                lease.expires_at_ns,
            ],
        )
        .map_err(|error| sqlite(error, "failed to create Space Lease"))?;
    Ok(())
}

pub(crate) fn transition(
    connection: &mut Connection,
    change: SpaceLeaseTransition,
) -> Result<SpaceLeaseState, MirageError> {
    if change.at_ns < 0 {
        return Err(MirageError::invalid_argument(
            "Space Lease transition timestamp is invalid",
        ));
    }
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|error| sqlite(error, "failed to begin Space Lease transition"))?;
    let stored: Option<(String, i64, i64)> = transaction
        .query_row(
            "SELECT state, updated_at_ns, expires_at_ns FROM space_leases WHERE lease_id = ?1",
            [change.lease_id.as_bytes().as_slice()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()
        .map_err(|error| sqlite(error, "failed to read Space Lease transition state"))?;
    let (stored, previous_at_ns, expires_at_ns) =
        stored.ok_or_else(|| MirageError::invalid_argument("Space Lease does not exist"))?;
    let current = SpaceLeaseState::parse(&stored)?;
    if current != change.expected || change.at_ns < previous_at_ns {
        return Err(MirageError::repository_conflict(
            "Space Lease transition has stale state or time",
        ));
    }
    if change.at_ns >= expires_at_ns
        && !matches!(
            change.event,
            SpaceLeaseEvent::Release | SpaceLeaseEvent::Fail
        )
    {
        return Err(MirageError::repository_conflict(
            "expired Space Lease can only be released or failed",
        ));
    }
    let next = next_state(current, change.event)?;
    let changed = transaction
        .execute(
            "UPDATE space_leases SET state = ?2, updated_at_ns = ?3 \
             WHERE lease_id = ?1 AND state = ?4 AND updated_at_ns = ?5",
            params![
                change.lease_id.as_bytes().as_slice(),
                next.as_str(),
                change.at_ns,
                current.as_str(),
                previous_at_ns,
            ],
        )
        .map_err(|error| sqlite(error, "failed to update Space Lease state"))?;
    if changed != 1 {
        return Err(MirageError::repository_conflict(
            "Space Lease changed concurrently",
        ));
    }
    transaction
        .commit()
        .map_err(|error| sqlite(error, "failed to commit Space Lease transition"))?;
    Ok(next)
}

fn next_state(
    current: SpaceLeaseState,
    event: SpaceLeaseEvent,
) -> Result<SpaceLeaseState, MirageError> {
    let next = match (current, event) {
        (SpaceLeaseState::Preparing, SpaceLeaseEvent::ReclaimFinished) => SpaceLeaseState::Ready,
        (SpaceLeaseState::Ready, SpaceLeaseEvent::Consume) => SpaceLeaseState::Consumed,
        (
            SpaceLeaseState::Preparing | SpaceLeaseState::Ready | SpaceLeaseState::Consumed,
            SpaceLeaseEvent::Release,
        ) => SpaceLeaseState::Released,
        (
            SpaceLeaseState::Preparing | SpaceLeaseState::Ready | SpaceLeaseState::Consumed,
            SpaceLeaseEvent::Fail,
        ) => SpaceLeaseState::Failed,
        _ => {
            return Err(MirageError::repository_conflict(
                "Space Lease state transition is invalid",
            ));
        }
    };
    Ok(next)
}
