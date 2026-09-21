//! Verified working-space leases: a declared path prefix whose coverage is
//! proven before admission. The lease row is durable, so a verified lease
//! survives restart without re-verification — the offline guarantee —
//! while `revoked`/`expired` leases fail admission.

use mirage_types::{MirageError, RepositoryId};
use rusqlite::{Connection, OptionalExtension, params};

use crate::Database;
use crate::error::sqlite;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeaseStatus {
    Declared,
    Verifying,
    Verified,
    Revoked,
    Expired,
}

impl LeaseStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Declared => "declared",
            Self::Verifying => "verifying",
            Self::Verified => "verified",
            Self::Revoked => "revoked",
            Self::Expired => "expired",
        }
    }
    fn parse(value: &str) -> Result<Self, MirageError> {
        match value {
            "declared" => Ok(Self::Declared),
            "verifying" => Ok(Self::Verifying),
            "verified" => Ok(Self::Verified),
            "revoked" => Ok(Self::Revoked),
            "expired" => Ok(Self::Expired),
            _ => Err(MirageError::integrity_mismatch("lease status is unknown")),
        }
    }
}

#[derive(Debug, Clone)]
pub struct WorkspaceLease {
    pub lease_id: [u8; 16],
    pub volume_id: RepositoryId,
    pub path_prefix: String,
    pub bytes_declared: u64,
    pub bytes_verified: u64,
    pub pages_pinned: u64,
    pub status: LeaseStatus,
    pub evidence: Vec<u8>,
    pub expires_ns: Option<i64>,
    pub created_ns: i64,
    pub verified_ns: Option<i64>,
}

/// Declares a lease (idempotent on `(volume, prefix)`); the row exists before
/// verification work starts.
pub fn declare(
    connection: &mut Connection,
    lease: &WorkspaceLease,
) -> Result<[u8; 16], MirageError> {
    if lease.status != LeaseStatus::Declared {
        return Err(MirageError::invalid_argument(
            "a new lease must start declared",
        ));
    }
    connection
        .execute(
            "INSERT INTO workspace_leases
             (lease_id, volume_id, path_prefix, bytes_declared, bytes_verified,
              pages_pinned, status, evidence, expires_ns, created_ns, verified_ns)
             VALUES (?1, ?2, ?3, ?4, 0, 0, 'declared', ?5, ?6, ?7, NULL)
             ON CONFLICT(volume_id, path_prefix) DO NOTHING",
            params![
                lease.lease_id.as_slice(),
                lease.volume_id.as_bytes().as_slice(),
                lease.path_prefix,
                lease.bytes_declared as i64,
                lease.evidence,
                lease.expires_ns,
                lease.created_ns,
            ],
        )
        .map_err(|e| sqlite(e, "lease declare failed"))?;
    Ok(
        lease_by_prefix(connection, lease.volume_id, &lease.path_prefix)?
            .map(|existing| existing.lease_id)
            .unwrap_or(lease.lease_id),
    )
}

/// Marks a lease verified with its measured coverage and evidence.
pub fn mark_verified(
    connection: &mut Connection,
    lease_id: &[u8; 16],
    bytes_verified: u64,
    pages_pinned: u64,
    evidence: Vec<u8>,
    now_ns: i64,
) -> Result<(), MirageError> {
    let changed = connection
        .execute(
            "UPDATE workspace_leases SET
                status = 'verified', bytes_verified = ?1, pages_pinned = ?2,
                evidence = ?3, verified_ns = ?4
             WHERE lease_id = ?5 AND status IN ('declared', 'verifying')",
            params![
                bytes_verified as i64,
                pages_pinned as i64,
                evidence,
                now_ns,
                lease_id.as_slice(),
            ],
        )
        .map_err(|e| sqlite(e, "lease verify failed"))?;
    if changed != 1 {
        return Err(MirageError::repository_conflict(
            "lease is not awaiting verification",
        ));
    }
    Ok(())
}

/// Revokes a lease: subsequent admission under it is denied and covered
/// pages become eviction candidates.
pub fn revoke(
    connection: &mut Connection,
    lease_id: &[u8; 16],
    now_ns: i64,
) -> Result<(), MirageError> {
    let changed = connection
        .execute(
            "UPDATE workspace_leases SET status = 'revoked', verified_ns = COALESCE(verified_ns, ?1)
             WHERE lease_id = ?2 AND status != 'revoked'",
            params![now_ns, lease_id.as_slice()],
        )
        .map_err(|e| sqlite(e, "lease revoke failed"))?;
    if changed == 0 {
        return Err(MirageError::repository_conflict("lease is already revoked"));
    }
    Ok(())
}

/// Whether `path` is admitted: covered by at least one verified,
/// unexpired lease whose prefix contains it.
pub fn is_admitted(
    connection: &Connection,
    volume_id: RepositoryId,
    path: &str,
    now_ns: i64,
) -> Result<bool, MirageError> {
    // Prefix match is literal — LIKE would treat `_`/`%` inside a stored
    // lease prefix as wildcards and admit unrelated directories.
    connection
        .query_row(
            "SELECT 1 FROM workspace_leases
             WHERE volume_id = ?1 AND status = 'verified'
               AND (expires_ns IS NULL OR expires_ns > ?2)
               AND (?3 = path_prefix
                    OR (length(?3) > length(path_prefix)
                        AND substr(?3, 1, length(path_prefix)) = path_prefix
                        AND substr(?3, length(path_prefix) + 1, 1) = '/')
                    OR path_prefix = '')
             LIMIT 1",
            params![volume_id.as_bytes().as_slice(), now_ns, path],
            |row| row.get::<_, i64>(0),
        )
        .optional()
        .map(|present| present.is_some())
        .map_err(|e| sqlite(e, "lease admission check failed"))
}

fn lease_by_prefix(
    connection: &Connection,
    volume_id: RepositoryId,
    prefix: &str,
) -> Result<Option<WorkspaceLease>, MirageError> {
    connection
        .query_row(
            "SELECT lease_id, bytes_declared, bytes_verified, pages_pinned,
                    status, evidence, expires_ns, created_ns, verified_ns
             FROM workspace_leases WHERE volume_id = ?1 AND path_prefix = ?2",
            params![volume_id.as_bytes().as_slice(), prefix],
            |row| {
                Ok(WorkspaceLease {
                    lease_id: row
                        .get::<_, Vec<u8>>(0)?
                        .try_into()
                        .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(0, 16))?,
                    volume_id,
                    path_prefix: prefix.to_string(),
                    bytes_declared: u64::try_from(row.get::<_, i64>(1)?)
                        .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(1, 0))?,
                    bytes_verified: u64::try_from(row.get::<_, i64>(2)?)
                        .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(2, 0))?,
                    pages_pinned: u64::try_from(row.get::<_, i64>(3)?)
                        .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(3, 0))?,
                    status: LeaseStatus::parse(&row.get::<_, String>(4)?)
                        .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(4, 7))?,
                    evidence: row.get::<_, Option<Vec<u8>>>(5)?.unwrap_or_default(),
                    expires_ns: row.get(6)?,
                    created_ns: row.get(7)?,
                    verified_ns: row.get(8)?,
                })
            },
        )
        .optional()
        .map_err(|e| sqlite(e, "lease lookup failed"))
}

/// All leases for a volume — readiness reporting and eviction sweeps.
pub fn leases(
    connection: &Connection,
    volume_id: RepositoryId,
) -> Result<Vec<WorkspaceLease>, MirageError> {
    let mut statement = connection
        .prepare(
            "SELECT lease_id, path_prefix, bytes_declared, bytes_verified,
                    pages_pinned, status, evidence, expires_ns, created_ns, verified_ns
             FROM workspace_leases WHERE volume_id = ?1 ORDER BY path_prefix",
        )
        .map_err(|e| sqlite(e, "lease scan prepare failed"))?;
    let rows = statement
        .query_map([volume_id.as_bytes().as_slice()], |row| {
            Ok((
                row.get::<_, Vec<u8>>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, Option<Vec<u8>>>(6)?,
                row.get::<_, Option<i64>>(7)?,
                row.get::<_, i64>(8)?,
                row.get::<_, Option<i64>>(9)?,
            ))
        })
        .map_err(|e| sqlite(e, "lease scan failed"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| sqlite(e, "lease row decode failed"))?;
    rows.into_iter()
        .map(
            |(
                id,
                prefix,
                declared,
                verified,
                pinned,
                status,
                evidence,
                expires,
                created,
                verified_ns,
            )| {
                Ok(WorkspaceLease {
                    lease_id: id
                        .try_into()
                        .map_err(|_| MirageError::integrity_mismatch("lease id is malformed"))?,
                    volume_id,
                    path_prefix: prefix,
                    bytes_declared: u64::try_from(declared).map_err(|_| {
                        MirageError::integrity_mismatch("declared bytes are negative")
                    })?,
                    bytes_verified: u64::try_from(verified).map_err(|_| {
                        MirageError::integrity_mismatch("verified bytes are negative")
                    })?,
                    pages_pinned: u64::try_from(pinned).map_err(|_| {
                        MirageError::integrity_mismatch("pinned pages are negative")
                    })?,
                    status: LeaseStatus::parse(&status)?,
                    evidence: evidence.unwrap_or_default(),
                    expires_ns: expires,
                    created_ns: created,
                    verified_ns,
                })
            },
        )
        .collect()
}

impl Database {
    /// Whether `path` sits under a live verified lease.
    pub fn lease_admitted(
        &self,
        volume_id: RepositoryId,
        path: &str,
        now_ns: i64,
    ) -> Result<bool, MirageError> {
        self.reads()
            .with_connection(|connection| is_admitted(connection, volume_id, path, now_ns))
    }

    /// All leases for a volume.
    pub fn workspace_leases(
        &self,
        volume_id: RepositoryId,
    ) -> Result<Vec<WorkspaceLease>, MirageError> {
        self.reads()
            .with_connection(|connection| leases(connection, volume_id))
    }
}
