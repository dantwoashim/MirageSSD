//! Working-space admission: a declared path prefix must be verified — its
//! coverage measured and the lease marked — before content under it is
//! admitted to the workspace. Verification is durable, so an admitted
//! workspace stays usable across a crash without re-verifying (the offline
//! guarantee). Revoked leases deny admission and release their pins.

use mirage_db::{Database, LeaseStatus, WorkspaceLease};
use mirage_types::{MirageError, RepositoryId};

/// Admission gate over durable workspace leases.
pub struct WorkspaceAdmission<'a> {
    db: &'a Database,
    volume: RepositoryId,
}

impl<'a> WorkspaceAdmission<'a> {
    pub fn new(db: &'a Database, volume: RepositoryId) -> Self {
        Self { db, volume }
    }

    /// Declares a workspace over `path_prefix`. Idempotent on the prefix.
    pub fn declare(
        &self,
        path_prefix: &str,
        bytes: u64,
        now_ns: i64,
    ) -> Result<[u8; 16], MirageError> {
        let mut lease_id = [0u8; 16];
        getrandom::fill(&mut lease_id)
            .map_err(|_| MirageError::internal_invariant("lease id entropy failed"))?;
        let lease = WorkspaceLease {
            lease_id,
            volume_id: self.volume,
            path_prefix: path_prefix.to_string(),
            bytes_declared: bytes,
            bytes_verified: 0,
            pages_pinned: 0,
            status: LeaseStatus::Declared,
            evidence: Vec::new(),
            expires_ns: None,
            created_ns: now_ns,
            verified_ns: None,
        };
        self.db.writer().lease_declare(lease)
    }

    /// Verifies a declared lease: `verify` measures the covered bytes and
    /// pinned pages; the lease is only admitted when the measurement lands.
    /// The evidence is stored durably so a restart keeps the lease verified.
    pub fn verify(
        &self,
        lease_id: [u8; 16],
        bytes_verified: u64,
        pages_pinned: u64,
        evidence: Vec<u8>,
        now_ns: i64,
    ) -> Result<(), MirageError> {
        self.db
            .writer()
            .lease_verify(lease_id, bytes_verified, pages_pinned, evidence, now_ns)
    }

    /// Admission check: `path` is admitted only under a live verified lease.
    /// Denial happens before any evidence-free path can pass.
    pub fn admit(&self, path: &str, now_ns: i64) -> Result<bool, MirageError> {
        self.db.lease_admitted(self.volume, path, now_ns)
    }

    /// Revokes a lease; covered pages become eviction candidates.
    pub fn revoke(&self, lease_id: [u8; 16], now_ns: i64) -> Result<(), MirageError> {
        self.db.writer().lease_revoke(lease_id, now_ns)
    }

    /// All leases for the volume — readiness reporting and eviction sweeps.
    pub fn leases(&self) -> Result<Vec<WorkspaceLease>, MirageError> {
        self.db.workspace_leases(self.volume)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> (tempfile::TempDir, Database, RepositoryId) {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(&dir.path().join("control.db")).unwrap();
        let volume = RepositoryId::from_bytes([0x51; 16]);
        (dir, db, volume)
    }

    #[test]
    fn admission_requires_verified_lease() {
        let (_d, db, volume) = setup();
        let admission = WorkspaceAdmission::new(&db, volume);
        // No lease → denied.
        assert!(!admission.admit("src/main.rs", 1).unwrap());
        // Declared but unverified → denied.
        let lease = admission.declare("src", 4096, 1).unwrap();
        assert!(!admission.admit("src/main.rs", 1).unwrap());
        // Verified → admitted; nested paths included.
        admission
            .verify(lease, 4096, 2, b"proof".to_vec(), 2)
            .unwrap();
        assert!(admission.admit("src/main.rs", 3).unwrap());
        assert!(admission.admit("src", 3).unwrap());
        assert!(!admission.admit("other/file", 3).unwrap());
    }

    #[test]
    fn verified_lease_survives_restart_and_revocation_denies() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("control.db");
        let volume = RepositoryId::from_bytes([0x52; 16]);
        let lease_id;
        {
            let db = Database::open(&db_path).unwrap();
            let admission = WorkspaceAdmission::new(&db, volume);
            lease_id = admission.declare("src", 100, 1).unwrap();
            admission
                .verify(lease_id, 100, 1, b"e".to_vec(), 2)
                .unwrap();
            assert!(admission.admit("src/a", 3).unwrap());
        }
        // Crash-and-restart: the verified lease still admits — offline
        // guarantee without re-verification.
        let db = Database::open(&db_path).unwrap();
        let admission = WorkspaceAdmission::new(&db, volume);
        assert!(admission.admit("src/a", 4).unwrap());
        // Revocation denies and keeps the record.
        admission.revoke(lease_id, 5).unwrap();
        assert!(!admission.admit("src/a", 6).unwrap());
        assert_eq!(admission.leases().unwrap()[0].status, LeaseStatus::Revoked);
    }
}
