//! Durable SQLite control plane with checksummed migrations and one bounded writer actor.

#![forbid(unsafe_code)]

pub mod cache;
mod error;
pub mod generation;
pub mod integrity;
pub mod lease;
mod migrate;
mod open;
pub mod pin;
pub mod remote_object;
pub mod repository;
pub mod session;
pub mod space_lease;
mod state_codec;
pub mod uninstall;
pub mod update;
mod value;
mod writer;

use std::path::Path;

use mirage_types::MirageError;

pub use cache::{
    CacheShardSpec, CacheSlotRecord, CacheSlotState, CacheSnapshot, CommitCacheSlotOutcome,
    ReserveCacheSlotOutcome, load_cache_snapshot,
};
pub use generation::{ActiveGeneration, VerifiedGeneration};
pub use integrity::{DatabaseCheckReport, check_database};
pub use lease::LeaseSpec;
pub use open::{APPLICATION_ID, ReadPool};
pub use pin::{CachePinRecord, PersistentPinReason};
pub use remote_object::{
    BackendAccount, RemoteObjectRecord, UploadSession, UpsertRemoteObjectOutcome,
};
pub use repository::{NewRepository, RepositorySummary};
pub use session::{NewSealedSession, SessionProcess};
pub use space_lease::{NewSpaceLease, SpaceLeaseEvent, SpaceLeaseRecord, SpaceLeaseState};
pub use uninstall::{UninstallSafetyReport, check_uninstall_safety};
pub use update::{ActiveUpdate, NativeSnapshot, NewUpdateJournal, OverlayPage};
pub use writer::DbWriter;

#[derive(Debug, Clone)]
pub struct Database {
    writer: DbWriter,
    reads: ReadPool,
}

impl Database {
    pub fn open(path: &Path) -> Result<Self, MirageError> {
        let mut connection = open::writer_connection(path)?;
        migrate::apply_all(&mut connection)?;
        let report = integrity::quick_check_database(path)?;
        if !report.quick_check_ok || report.foreign_key_violation_count != 0 {
            return Err(MirageError::integrity_mismatch(
                "database startup recovery check failed",
            ));
        }
        let writer = DbWriter::start(connection)?;
        Ok(Self {
            writer,
            reads: ReadPool::new(path.to_path_buf()),
        })
    }

    #[must_use]
    pub const fn writer(&self) -> &DbWriter {
        &self.writer
    }

    #[must_use]
    pub const fn reads(&self) -> &ReadPool {
        &self.reads
    }
}
