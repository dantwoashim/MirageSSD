use std::fmt;
use std::sync::mpsc::{SyncSender, sync_channel};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use mirage_types::{MirageError, RepositoryState, SessionState, UpdateState};
use rusqlite::Connection;

use crate::cache::{
    self, CacheShardSpec, CacheSlotRecord, CommitCacheSlotOutcome, ReserveCacheSlotOutcome,
};
use crate::error::writer_unavailable;
use crate::generation::{self, Activation, VerifiedGeneration};
use crate::namespace::{self, DirEntry, NamespaceNodeKind, NamespaceSeedNode};
use crate::physical::{self, PhysicalExtentRecord, PhysicalFileRecord, PhysicalReservationRecord};
use crate::pin::{self, PersistentPinReason};
use crate::remote_object::{
    self, BackendAccount, RemoteObjectRecord, UploadAdvance, UploadSession,
    UpsertRemoteObjectOutcome,
};
use crate::repository::{self, NewRepository, RepositoryStateChange};
use crate::session::{
    self, FinishSession, NewSealedSession, SealViolation, SessionProcess, SessionTransition,
};
use crate::space_lease::{self, NewSpaceLease, SpaceLeaseState, SpaceLeaseTransition};
use crate::update::{self, NativeSnapshot, NewUpdateJournal, OverlayPage, UpdateTransition};
use mirage_types::{DeviceId, InodeId, RepositoryId};

const COMMAND_CAPACITY: usize = 64;
type Reply<T> = SyncSender<Result<T, MirageError>>;

enum Command {
    PinCachePages(PersistentPinReason, Vec<mirage_types::PageHash>, Reply<()>),
    ReleaseCachePins(PersistentPinReason, Reply<()>),
    RegisterCacheShard(CacheShardSpec, Reply<()>),
    ReserveCacheSlot(mirage_types::PageHash, u32, Reply<ReserveCacheSlotOutcome>),
    ReserveCacheSlotsBatch(
        Vec<(mirage_types::PageHash, u32)>,
        Reply<Vec<ReserveCacheSlotOutcome>>,
    ),
    CommitCacheSlot(CacheSlotRecord, Reply<CommitCacheSlotOutcome>),
    CommitCacheSlotsBatch(Vec<CacheSlotRecord>, Reply<Vec<CacheSlotRecord>>),
    ReleaseCacheReservation(CacheSlotRecord, Reply<()>),
    BeginCacheEviction(CacheSlotRecord, Reply<CacheSlotRecord>),
    FinishCacheDeallocation(CacheSlotRecord, Reply<()>),
    MarkCacheDeallocationRetry(CacheSlotRecord, Reply<()>),
    CreateRepository(NewRepository, Reply<()>),
    SetRepositoryOwnerSid(mirage_types::RepositoryId, String, String, Reply<()>),
    SetRepositoryState(RepositoryStateChange, Reply<RepositoryState>),
    InsertVerifiedGeneration(VerifiedGeneration, Reply<()>),
    ActivateGeneration(Activation, Reply<()>),
    RegisterBackendAccount(BackendAccount, Reply<()>),
    UpsertRemoteObject(RemoteObjectRecord, Reply<UpsertRemoteObjectOutcome>),
    BeginUploadSession(UploadSession, Reply<()>),
    AdvanceUploadSession(UploadAdvance, Reply<UploadSession>),
    CreateSessionWithLeases(NewSealedSession, Reply<()>),
    RecordSessionProcess(SessionProcess, Reply<()>),
    TransitionSession(SessionTransition, Reply<SessionState>),
    MarkSealViolation(SealViolation, Reply<u64>),
    FinishSession(FinishSession, Reply<()>),
    CreateSpaceLease(NewSpaceLease, Reply<()>),
    TransitionSpaceLease(SpaceLeaseTransition, Reply<SpaceLeaseState>),
    CreateNamespaceVolume(RepositoryId, i64, Reply<InodeId>),
    NamespaceCreate(
        RepositoryId,
        InodeId,
        String,
        NamespaceNodeKind,
        i64,
        Reply<DirEntry>,
    ),
    NamespaceRename(
        RepositoryId,
        InodeId,
        String,
        InodeId,
        String,
        i64,
        Reply<()>,
    ),
    NamespaceDelete(RepositoryId, InodeId, String, i64, Reply<()>),
    NamespaceSetFileRoots(
        RepositoryId,
        InodeId,
        u64,
        Option<[u8; 32]>,
        Option<[u8; 32]>,
        i64,
        Reply<()>,
    ),
    NamespaceRecordLegacy(RepositoryId, String, InodeId, Reply<()>),
    NamespaceSeed(RepositoryId, Vec<NamespaceSeedNode>, i64, Reply<usize>),
    NamespaceCheckpoint(RepositoryId, i64, Reply<(u64, [u8; 32])>),
    EnsureDeviceIdentity(i64, Reply<DeviceId>),
    PhysicalRegisterFile(PhysicalFileRecord, Reply<()>),
    PhysicalReserveExtent(PhysicalExtentRecord, PhysicalReservationRecord, Reply<()>),
    PhysicalCommitExtent([u8; 16], mirage_types::PageHash, [u8; 32], i64, Reply<()>),
    PhysicalReleaseExtent([u8; 16], i64, Reply<()>),
    PhysicalMarkExtentDead([u8; 16], i64, Reply<()>),
    PhysicalAdjustExtentPin([u8; 16], i64, Reply<()>),
    PhysicalReapReservations(i64, Reply<u64>),
    CreateUpdateJournal(NewUpdateJournal, Reply<()>),
    UpsertOverlayPage(OverlayPage, Reply<()>),
    UpsertNativeSnapshot(NativeSnapshot, Reply<()>),
    TransitionUpdate(UpdateTransition, Reply<UpdateState>),
    Shutdown(SyncSender<()>),
}

struct WriterInner {
    sender: SyncSender<Command>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl Drop for WriterInner {
    fn drop(&mut self) {
        let (reply, received) = sync_channel(1);
        if self.sender.send(Command::Shutdown(reply)).is_ok() {
            let _ = received.recv();
        }
        if let Ok(thread) = self.thread.get_mut()
            && let Some(thread) = thread.take()
        {
            let _ = thread.join();
        }
    }
}

#[derive(Clone)]
pub struct DbWriter {
    inner: Arc<WriterInner>,
}

impl fmt::Debug for DbWriter {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DbWriter")
            .field("command_capacity", &COMMAND_CAPACITY)
            .finish_non_exhaustive()
    }
}

impl DbWriter {
    pub(crate) fn start(mut connection: Connection) -> Result<Self, MirageError> {
        let (sender, receiver) = sync_channel(COMMAND_CAPACITY);
        let thread = thread::Builder::new()
            .name("mirage-db-writer".to_string())
            .spawn(move || {
                while let Ok(command) = receiver.recv() {
                    match command {
                        Command::PinCachePages(reason, pages, reply) => {
                            respond(reply, pin::pin(&mut connection, reason, pages));
                        }
                        Command::ReleaseCachePins(reason, reply) => {
                            respond(reply, pin::release(&mut connection, reason));
                        }
                        Command::RegisterCacheShard(value, reply) => {
                            respond(reply, cache::register_shard(&mut connection, value));
                        }
                        Command::ReserveCacheSlot(hash, length, reply) => {
                            respond(reply, cache::reserve(&mut connection, hash, length));
                        }
                        Command::ReserveCacheSlotsBatch(requests, reply) => {
                            respond(reply, cache::reserve_batch(&mut connection, requests));
                        }
                        Command::CommitCacheSlot(value, reply) => {
                            respond(reply, cache::commit_slot(&mut connection, value));
                        }
                        Command::CommitCacheSlotsBatch(records, reply) => {
                            respond(reply, cache::commit_batch(&mut connection, records));
                        }
                        Command::ReleaseCacheReservation(value, reply) => {
                            respond(reply, cache::release(&mut connection, value));
                        }
                        Command::BeginCacheEviction(value, reply) => {
                            respond(reply, cache::begin_eviction(&mut connection, value));
                        }
                        Command::FinishCacheDeallocation(value, reply) => {
                            respond(reply, cache::finish_deallocation(&mut connection, value));
                        }
                        Command::MarkCacheDeallocationRetry(value, reply) => {
                            respond(reply, cache::mark_retry(&mut connection, value));
                        }
                        Command::CreateRepository(value, reply) => {
                            respond(reply, repository::create(&mut connection, value));
                        }
                        Command::SetRepositoryOwnerSid(id, expected, new, reply) => {
                            respond(
                                reply,
                                repository::set_owner_sid(&mut connection, id, expected, new),
                            );
                        }
                        Command::SetRepositoryState(value, reply) => {
                            respond(reply, repository::set_state(&mut connection, value));
                        }
                        Command::InsertVerifiedGeneration(value, reply) => {
                            respond(reply, generation::insert_verified(&mut connection, value));
                        }
                        Command::ActivateGeneration(value, reply) => {
                            respond(reply, generation::activate(&mut connection, value));
                        }
                        Command::RegisterBackendAccount(value, reply) => {
                            respond(
                                reply,
                                remote_object::register_account(&mut connection, value),
                            );
                        }
                        Command::UpsertRemoteObject(value, reply) => {
                            respond(reply, remote_object::upsert_object(&mut connection, value));
                        }
                        Command::BeginUploadSession(value, reply) => {
                            respond(reply, remote_object::begin_upload(&mut connection, value));
                        }
                        Command::AdvanceUploadSession(value, reply) => {
                            respond(reply, remote_object::advance_upload(&mut connection, value));
                        }
                        Command::CreateSessionWithLeases(value, reply) => {
                            respond(reply, session::create_with_leases(&mut connection, value));
                        }
                        Command::RecordSessionProcess(value, reply) => {
                            respond(reply, session::record_process(&mut connection, value));
                        }
                        Command::TransitionSession(value, reply) => {
                            respond(reply, session::transition_state(&mut connection, value));
                        }
                        Command::MarkSealViolation(value, reply) => {
                            respond(reply, session::mark_violation(&mut connection, value));
                        }
                        Command::FinishSession(value, reply) => {
                            respond(reply, session::finish(&mut connection, value));
                        }
                        Command::CreateSpaceLease(value, reply) => {
                            respond(reply, space_lease::create(&mut connection, value));
                        }
                        Command::TransitionSpaceLease(value, reply) => {
                            respond(reply, space_lease::transition(&mut connection, value));
                        }
                        Command::CreateNamespaceVolume(volume_id, now_ns, reply) => {
                            respond(
                                reply,
                                namespace::create_volume(&mut connection, volume_id, now_ns),
                            );
                        }
                        Command::NamespaceCreate(volume_id, parent, name, kind, now_ns, reply) => {
                            respond(
                                reply,
                                namespace::create_node(
                                    &mut connection,
                                    volume_id,
                                    parent,
                                    &name,
                                    kind,
                                    now_ns,
                                ),
                            );
                        }
                        Command::NamespaceRename(
                            volume_id,
                            from_parent,
                            from_name,
                            to_parent,
                            to_name,
                            now_ns,
                            reply,
                        ) => {
                            respond(
                                reply,
                                namespace::rename(
                                    &mut connection,
                                    volume_id,
                                    from_parent,
                                    &from_name,
                                    to_parent,
                                    &to_name,
                                    now_ns,
                                ),
                            );
                        }
                        Command::NamespaceDelete(volume_id, parent, name, now_ns, reply) => {
                            respond(
                                reply,
                                namespace::delete_node(
                                    &mut connection,
                                    volume_id,
                                    parent,
                                    &name,
                                    now_ns,
                                ),
                            );
                        }
                        Command::NamespaceSetFileRoots(
                            volume_id,
                            inode,
                            size,
                            version_root,
                            extent_root,
                            now_ns,
                            reply,
                        ) => {
                            respond(
                                reply,
                                namespace::set_file_roots(
                                    &mut connection,
                                    volume_id,
                                    inode,
                                    size,
                                    version_root,
                                    extent_root,
                                    now_ns,
                                ),
                            );
                        }
                        Command::NamespaceRecordLegacy(volume_id, path, inode, reply) => {
                            respond(
                                reply,
                                namespace::record_legacy(&mut connection, volume_id, &path, inode),
                            );
                        }
                        Command::NamespaceCheckpoint(volume_id, now_ns, reply) => {
                            respond(
                                reply,
                                namespace::create_checkpoint(&mut connection, volume_id, now_ns),
                            );
                        }
                        Command::EnsureDeviceIdentity(now_ns, reply) => {
                            respond(reply, namespace::ensure_device_id(&mut connection, now_ns));
                        }
                        Command::NamespaceSeed(volume_id, nodes, now_ns, reply) => {
                            respond(
                                reply,
                                namespace::seed_volume(&mut connection, volume_id, nodes, now_ns),
                            );
                        }
                        Command::PhysicalRegisterFile(record, reply) => {
                            respond(reply, physical::register_file(&mut connection, &record));
                        }
                        Command::PhysicalReserveExtent(extent, reservation, reply) => {
                            respond(
                                reply,
                                physical::reserve_extent(&mut connection, &extent, &reservation),
                            );
                        }
                        Command::PhysicalCommitExtent(id, hash, checksum, now, reply) => {
                            respond(
                                reply,
                                physical::commit_extent(&mut connection, &id, hash, checksum, now),
                            );
                        }
                        Command::PhysicalReleaseExtent(id, now, reply) => {
                            respond(reply, physical::release_extent(&mut connection, &id, now));
                        }
                        Command::PhysicalMarkExtentDead(id, now, reply) => {
                            respond(reply, physical::mark_extent_dead(&mut connection, &id, now));
                        }
                        Command::PhysicalAdjustExtentPin(id, delta, reply) => {
                            respond(
                                reply,
                                physical::adjust_extent_pin(&mut connection, &id, delta),
                            );
                        }
                        Command::PhysicalReapReservations(now, reply) => {
                            respond(
                                reply,
                                physical::reap_expired_reservations(&mut connection, now),
                            );
                        }
                        Command::CreateUpdateJournal(value, reply) => {
                            respond(reply, update::create_journal(&mut connection, value));
                        }
                        Command::UpsertOverlayPage(value, reply) => {
                            respond(reply, update::upsert_overlay(&mut connection, value));
                        }
                        Command::UpsertNativeSnapshot(value, reply) => {
                            respond(reply, update::upsert_snapshot(&mut connection, value));
                        }
                        Command::TransitionUpdate(value, reply) => {
                            respond(reply, update::transition_state(&mut connection, value));
                        }
                        Command::Shutdown(reply) => {
                            let _ = reply.send(());
                            break;
                        }
                    }
                }
            })
            .map_err(|error| {
                MirageError::internal_invariant("failed to start database writer")
                    .with_source(error)
            })?;
        Ok(Self {
            inner: Arc::new(WriterInner {
                sender,
                thread: Mutex::new(Some(thread)),
            }),
        })
    }

    pub fn register_cache_shard(&self, value: CacheShardSpec) -> Result<(), MirageError> {
        self.request(|reply| Command::RegisterCacheShard(value, reply))
    }

    pub fn pin_cache_pages(
        &self,
        reason: PersistentPinReason,
        pages: Vec<mirage_types::PageHash>,
    ) -> Result<(), MirageError> {
        self.request(|reply| Command::PinCachePages(reason, pages, reply))
    }

    pub fn release_cache_pins(&self, reason: PersistentPinReason) -> Result<(), MirageError> {
        self.request(|reply| Command::ReleaseCachePins(reason, reply))
    }

    pub fn reserve_cache_slot(
        &self,
        hash: mirage_types::PageHash,
        length: u32,
    ) -> Result<ReserveCacheSlotOutcome, MirageError> {
        self.request(|reply| Command::ReserveCacheSlot(hash, length, reply))
    }

    pub fn reserve_cache_slots_batch(
        &self,
        requests: Vec<(mirage_types::PageHash, u32)>,
    ) -> Result<Vec<ReserveCacheSlotOutcome>, MirageError> {
        self.request(|reply| Command::ReserveCacheSlotsBatch(requests, reply))
    }

    pub fn commit_cache_slot(
        &self,
        value: CacheSlotRecord,
    ) -> Result<CommitCacheSlotOutcome, MirageError> {
        self.request(|reply| Command::CommitCacheSlot(value, reply))
    }

    pub fn commit_cache_slots_batch(
        &self,
        records: Vec<CacheSlotRecord>,
    ) -> Result<Vec<CacheSlotRecord>, MirageError> {
        self.request(|reply| Command::CommitCacheSlotsBatch(records, reply))
    }

    pub fn release_cache_reservation(&self, value: CacheSlotRecord) -> Result<(), MirageError> {
        self.request(|reply| Command::ReleaseCacheReservation(value, reply))
    }

    pub fn begin_cache_eviction(
        &self,
        value: CacheSlotRecord,
    ) -> Result<CacheSlotRecord, MirageError> {
        self.request(|reply| Command::BeginCacheEviction(value, reply))
    }

    pub fn finish_cache_deallocation(&self, value: CacheSlotRecord) -> Result<(), MirageError> {
        self.request(|reply| Command::FinishCacheDeallocation(value, reply))
    }

    pub fn mark_cache_deallocation_retry(&self, value: CacheSlotRecord) -> Result<(), MirageError> {
        self.request(|reply| Command::MarkCacheDeallocationRetry(value, reply))
    }

    pub fn create_repository(&self, value: NewRepository) -> Result<(), MirageError> {
        self.request(|reply| Command::CreateRepository(value, reply))
    }

    pub fn set_repository_owner_sid(
        &self,
        repository_id: mirage_types::RepositoryId,
        expected_owner_sid: String,
        new_owner_sid: String,
    ) -> Result<(), MirageError> {
        self.request(|reply| {
            Command::SetRepositoryOwnerSid(repository_id, expected_owner_sid, new_owner_sid, reply)
        })
    }

    pub(crate) fn set_repository_state(
        &self,
        value: RepositoryStateChange,
    ) -> Result<RepositoryState, MirageError> {
        self.request(|reply| Command::SetRepositoryState(value, reply))
    }

    pub fn insert_verified_generation(&self, value: VerifiedGeneration) -> Result<(), MirageError> {
        self.request(|reply| Command::InsertVerifiedGeneration(value, reply))
    }

    pub(crate) fn activate_generation(&self, value: Activation) -> Result<(), MirageError> {
        self.request(|reply| Command::ActivateGeneration(value, reply))
    }

    pub fn register_backend_account(&self, value: BackendAccount) -> Result<(), MirageError> {
        self.request(|reply| Command::RegisterBackendAccount(value, reply))
    }

    pub fn upsert_remote_object(
        &self,
        value: RemoteObjectRecord,
    ) -> Result<UpsertRemoteObjectOutcome, MirageError> {
        self.request(|reply| Command::UpsertRemoteObject(value, reply))
    }

    pub fn begin_upload_session(&self, value: UploadSession) -> Result<(), MirageError> {
        self.request(|reply| Command::BeginUploadSession(value, reply))
    }

    pub(crate) fn advance_upload_session(
        &self,
        value: UploadAdvance,
    ) -> Result<UploadSession, MirageError> {
        self.request(|reply| Command::AdvanceUploadSession(value, reply))
    }

    pub fn create_session_with_leases(&self, value: NewSealedSession) -> Result<(), MirageError> {
        self.request(|reply| Command::CreateSessionWithLeases(value, reply))
    }

    pub fn record_session_process(&self, value: SessionProcess) -> Result<(), MirageError> {
        self.request(|reply| Command::RecordSessionProcess(value, reply))
    }

    pub(crate) fn transition_session_state(
        &self,
        value: SessionTransition,
    ) -> Result<SessionState, MirageError> {
        self.request(|reply| Command::TransitionSession(value, reply))
    }

    pub(crate) fn mark_seal_violation(&self, value: SealViolation) -> Result<u64, MirageError> {
        self.request(|reply| Command::MarkSealViolation(value, reply))
    }

    pub(crate) fn finish_session(&self, value: FinishSession) -> Result<(), MirageError> {
        self.request(|reply| Command::FinishSession(value, reply))
    }

    pub fn create_space_lease(&self, value: NewSpaceLease) -> Result<(), MirageError> {
        self.request(|reply| Command::CreateSpaceLease(value, reply))
    }

    pub(crate) fn transition_space_lease(
        &self,
        value: SpaceLeaseTransition,
    ) -> Result<SpaceLeaseState, MirageError> {
        self.request(|reply| Command::TransitionSpaceLease(value, reply))
    }

    pub fn create_namespace_volume(
        &self,
        volume_id: RepositoryId,
        now_ns: i64,
    ) -> Result<InodeId, MirageError> {
        self.request(|reply| Command::CreateNamespaceVolume(volume_id, now_ns, reply))
    }

    pub fn namespace_create(
        &self,
        volume_id: RepositoryId,
        parent: InodeId,
        name: &str,
        kind: NamespaceNodeKind,
        now_ns: i64,
    ) -> Result<DirEntry, MirageError> {
        let name = name.to_owned();
        self.request(|reply| Command::NamespaceCreate(volume_id, parent, name, kind, now_ns, reply))
    }

    pub fn namespace_rename(
        &self,
        volume_id: RepositoryId,
        from_parent: InodeId,
        from_name: &str,
        to_parent: InodeId,
        to_name: &str,
        now_ns: i64,
    ) -> Result<(), MirageError> {
        let from_name = from_name.to_owned();
        let to_name = to_name.to_owned();
        self.request(|reply| {
            Command::NamespaceRename(
                volume_id,
                from_parent,
                from_name,
                to_parent,
                to_name,
                now_ns,
                reply,
            )
        })
    }

    pub fn namespace_delete(
        &self,
        volume_id: RepositoryId,
        parent: InodeId,
        name: &str,
        now_ns: i64,
    ) -> Result<(), MirageError> {
        let name = name.to_owned();
        self.request(|reply| Command::NamespaceDelete(volume_id, parent, name, now_ns, reply))
    }

    pub fn namespace_set_file_roots(
        &self,
        volume_id: RepositoryId,
        inode: InodeId,
        size: u64,
        version_root: Option<[u8; 32]>,
        extent_root: Option<[u8; 32]>,
        now_ns: i64,
    ) -> Result<(), MirageError> {
        self.request(|reply| {
            Command::NamespaceSetFileRoots(
                volume_id,
                inode,
                size,
                version_root,
                extent_root,
                now_ns,
                reply,
            )
        })
    }

    pub fn namespace_record_legacy(
        &self,
        volume_id: RepositoryId,
        legacy_path: &str,
        inode: InodeId,
    ) -> Result<(), MirageError> {
        let legacy_path = legacy_path.to_owned();
        self.request(|reply| Command::NamespaceRecordLegacy(volume_id, legacy_path, inode, reply))
    }

    /// Bulk-seeds a volume's namespace from a verified manifest tree in one
    /// transaction; idempotent once a volume exists.
    pub fn namespace_seed(
        &self,
        volume_id: RepositoryId,
        nodes: Vec<NamespaceSeedNode>,
        now_ns: i64,
    ) -> Result<usize, MirageError> {
        self.request(|reply| Command::NamespaceSeed(volume_id, nodes, now_ns, reply))
    }

    /// Snapshots the live namespace into an immutable checkpoint document;
    /// returns the new checkpoint sequence and its content hash.
    pub fn namespace_checkpoint(
        &self,
        volume_id: RepositoryId,
        now_ns: i64,
    ) -> Result<(u64, [u8; 32]), MirageError> {
        self.request(|reply| Command::NamespaceCheckpoint(volume_id, now_ns, reply))
    }

    /// Returns this installation's durable device identity, minting it on
    /// first use.
    pub fn ensure_device_id(&self, now_ns: i64) -> Result<DeviceId, MirageError> {
        self.request(|reply| Command::EnsureDeviceIdentity(now_ns, reply))
    }

    pub fn physical_register_file(&self, record: PhysicalFileRecord) -> Result<(), MirageError> {
        self.request(|reply| Command::PhysicalRegisterFile(record, reply))
    }

    /// Durably reserves an extent; the reservation must exist before any
    /// bytes are written to the covered arena range.
    pub fn physical_reserve_extent(
        &self,
        extent: PhysicalExtentRecord,
        reservation: PhysicalReservationRecord,
    ) -> Result<(), MirageError> {
        self.request(|reply| Command::PhysicalReserveExtent(extent, reservation, reply))
    }

    /// Commits a written extent alive once its checksum is verified.
    pub fn physical_commit_extent(
        &self,
        extent_id: [u8; 16],
        page_hash: mirage_types::PageHash,
        checksum: [u8; 32],
        now_ns: i64,
    ) -> Result<(), MirageError> {
        self.request(|reply| {
            Command::PhysicalCommitExtent(extent_id, page_hash, checksum, now_ns, reply)
        })
    }

    pub fn physical_release_extent(
        &self,
        extent_id: [u8; 16],
        now_ns: i64,
    ) -> Result<(), MirageError> {
        self.request(|reply| Command::PhysicalReleaseExtent(extent_id, now_ns, reply))
    }

    /// Transitions an alive extent to dead; pinned extents are fenced off.
    pub fn physical_mark_extent_dead(
        &self,
        extent_id: [u8; 16],
        now_ns: i64,
    ) -> Result<(), MirageError> {
        self.request(|reply| Command::PhysicalMarkExtentDead(extent_id, now_ns, reply))
    }

    /// Pins (+1) or unpins (-1) an alive extent against eviction.
    pub fn physical_adjust_extent_pin(
        &self,
        extent_id: [u8; 16],
        delta: i64,
    ) -> Result<(), MirageError> {
        self.request(|reply| Command::PhysicalAdjustExtentPin(extent_id, delta, reply))
    }

    /// Reaps expired reservations, returning their extents to dead.
    pub fn physical_reap_reservations(&self, now_ns: i64) -> Result<u64, MirageError> {
        self.request(|reply| Command::PhysicalReapReservations(now_ns, reply))
    }

    pub fn create_update_journal(&self, value: NewUpdateJournal) -> Result<(), MirageError> {
        self.request(|reply| Command::CreateUpdateJournal(value, reply))
    }

    pub fn upsert_overlay_page(&self, value: OverlayPage) -> Result<(), MirageError> {
        self.request(|reply| Command::UpsertOverlayPage(value, reply))
    }

    pub fn upsert_native_snapshot(&self, value: NativeSnapshot) -> Result<(), MirageError> {
        self.request(|reply| Command::UpsertNativeSnapshot(value, reply))
    }

    pub(crate) fn transition_update_state(
        &self,
        value: UpdateTransition,
    ) -> Result<UpdateState, MirageError> {
        self.request(|reply| Command::TransitionUpdate(value, reply))
    }

    fn request<T>(&self, command: impl FnOnce(Reply<T>) -> Command) -> Result<T, MirageError> {
        let (reply, received) = sync_channel(1);
        self.inner
            .sender
            .send(command(reply))
            .map_err(|_| writer_unavailable())?;
        received.recv().map_err(|_| writer_unavailable())?
    }
}

fn respond<T>(reply: Reply<T>, result: Result<T, MirageError>) {
    let _ = reply.send(result);
}
