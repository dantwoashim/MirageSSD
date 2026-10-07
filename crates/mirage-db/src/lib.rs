//! Durable SQLite control plane with checksummed migrations and one bounded writer actor.

#![forbid(unsafe_code)]

pub mod cache;
pub mod cache_root;
pub mod disk_floor;
mod error;
pub mod extent;
pub mod gc_bounds;
pub mod generation;
pub mod integrity;
pub mod lease;
mod migrate;
pub mod namespace;
mod open;
pub mod operation;
pub mod payload_remote;
pub mod physical;
pub mod pin;
pub mod remote_object;
pub mod remote_observation;
pub mod repository;
pub mod session;
pub mod space_lease;
mod state_codec;
pub mod uninstall;
pub mod update;
pub mod upload_session;
mod value;
pub mod workspace_lease;
mod writer;

use std::path::Path;

use mirage_types::MirageError;

pub use cache::{
    CacheShardSpec, CacheSlotRecord, CacheSlotState, CacheSnapshot, CommitCacheSlotOutcome,
    ReserveCacheSlotOutcome, load_cache_snapshot,
};
pub use disk_floor::{DiskFloor, DiskFloorRun};
pub use extent::{ByteExtent, ExtentKind};
pub use gc_bounds::{GcBound, GcKind, UnreachableCandidate};
pub use generation::{ActiveGeneration, VerifiedGeneration};
pub use integrity::{DatabaseCheckReport, check_database};
pub use lease::LeaseSpec;
pub use namespace::{DirEntry, NamespaceNodeKind, NamespaceSeedNode, NamespaceStat};
pub use open::{APPLICATION_ID, Durability, ReadPool};
pub use operation::{
    ExtentMutation, JournalEntry, OperationKind, OperationPayloadRecord, OperationRecord,
    OperationStatus, SequencedMutation,
};
pub use physical::{
    PhysicalCommit, PhysicalExtentRecord, PhysicalExtentState, PhysicalFileRecord,
    PhysicalReservationRecord,
};
pub use pin::{CachePinRecord, PersistentPinReason};
pub use remote_object::{
    BackendAccount, RemoteObjectRecord, UploadSession, UpsertRemoteObjectOutcome,
};
pub use remote_observation::{Divergence, DivergenceStatus, RemoteChange, RemoteHead};
pub use repository::{NewRepository, RepositorySummary, VolumeMode};
pub use session::{NewSealedSession, SessionProcess};
pub use space_lease::{NewSpaceLease, SpaceLeaseEvent, SpaceLeaseRecord, SpaceLeaseState};
pub use uninstall::{UninstallSafetyReport, check_uninstall_safety};
pub use update::{ActiveUpdate, NativeSnapshot, NewUpdateJournal, OverlayPage};
pub use upload_session::{SessionKind, SessionPhase, UploadSession as PublicationSession};
pub use workspace_lease::{LeaseStatus, WorkspaceLease};
pub use writer::DbWriter;

#[derive(Debug, Clone)]
pub struct Database {
    writer: DbWriter,
    reads: ReadPool,
}

impl Database {
    /// Strict durability: every commit fsyncs (synchronous=FULL).
    pub fn open(path: &Path) -> Result<Self, MirageError> {
        Self::open_with_durability(path, Durability::Strict)
    }

    /// `Group` commits under synchronous=NORMAL on the writer connection
    /// (WAL unchanged — commits still reach the OS immediately) and relies
    /// on the writer's 250 ms barrier plus explicit
    /// [`durability_barrier`](DbWriter::durability_barrier) calls for
    /// power-loss durability.
    pub fn open_with_durability(path: &Path, durability: Durability) -> Result<Self, MirageError> {
        let mut connection = open::writer_connection(path, durability)?;
        migrate::apply_all(&mut connection)?;
        let report = integrity::quick_check_database(path)?;
        if !report.quick_check_ok || report.foreign_key_violation_count != 0 {
            return Err(MirageError::integrity_mismatch(
                "database startup recovery check failed",
            ));
        }
        let writer = DbWriter::start(connection, durability)?;
        Ok(Self {
            writer,
            reads: ReadPool::new(path.to_path_buf()),
        })
    }

    /// Durability barrier: one FULL commit making every earlier commit
    /// durable — a no-op cost-wise in Strict mode.
    pub fn durability_barrier(&self) -> Result<(), MirageError> {
        self.writer.durability_barrier()
    }

    /// Current barrier epoch — test/diagnostic accessor.
    #[doc(hidden)]
    pub fn durability_barrier_seq(&self) -> Result<i64, MirageError> {
        self.reads.with_connection(|connection| {
            connection
                .prepare_cached("SELECT seq FROM durability_barrier WHERE id = 1")
                .and_then(|mut stmt| stmt.query_row([], |row| row.get(0)))
                .map_err(|error| crate::error::sqlite(error, "failed to read barrier epoch"))
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
