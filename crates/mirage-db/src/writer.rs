use std::fmt;
use std::sync::mpsc::{SyncSender, sync_channel};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use mirage_types::{MirageError, RepositoryState, SessionState, UpdateState};
use rusqlite::Connection;

use crate::cache::{
    self, CacheShardSpec, CacheSlotRecord, CommitCacheSlotOutcome, ReserveCacheSlotOutcome,
};
use crate::disk_floor::{self, DiskFloor, DiskFloorRun};
use crate::error::writer_unavailable;
use crate::generation::{self, Activation, VerifiedGeneration};
use crate::namespace::{self, DirEntry, NamespaceNodeKind, NamespaceSeedNode};
use crate::operation::{self, OperationPayloadRecord, OperationRecord};
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
    SetRepositoryVolumeMode(
        mirage_types::RepositoryId,
        crate::repository::VolumeMode,
        Reply<()>,
    ),
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
    DiskFloorSet(DiskFloor, Reply<()>),
    DiskFloorClear(String, Reply<bool>),
    DiskFloorRecordRun(DiskFloorRun, Reply<()>),
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
    NamespacePin(RepositoryId, InodeId, i64, Reply<()>),
    NamespaceUnpin(RepositoryId, InodeId, Reply<bool>),
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
    NamespaceSeedManaged(
        RepositoryId,
        Vec<NamespaceSeedNode>,
        [u8; 32],
        i64,
        Reply<Option<[u8; 32]>>,
    ),
    NamespaceCheckpoint(RepositoryId, i64, Reply<(u64, [u8; 32])>),
    EnsureDeviceIdentity(i64, Reply<DeviceId>),
    PhysicalRegisterFile(PhysicalFileRecord, Reply<()>),
    PhysicalReserveExtent(PhysicalExtentRecord, PhysicalReservationRecord, Reply<()>),
    PhysicalCommitExtent([u8; 16], mirage_types::PageHash, [u8; 32], i64, Reply<()>),
    PhysicalReleaseExtent([u8; 16], i64, Reply<()>),
    PhysicalMarkExtentDead([u8; 16], i64, Reply<()>),
    PhysicalReviveExtent(
        [u8; 16],
        [u8; 16],
        i64,
        i64,
        mirage_types::PageHash,
        [u8; 32],
        i64,
        Reply<()>,
    ),
    PhysicalAdjustExtentPin([u8; 16], i64, Reply<()>),
    PhysicalReapReservations(i64, Reply<u64>),
    OperationBegin(OperationRecord, Vec<OperationPayloadRecord>, Reply<()>),
    OperationAllocSeq(RepositoryId, Reply<i64>),
    MutationCommit(
        Option<crate::operation::ExtentMutation>,
        OperationRecord,
        Vec<OperationPayloadRecord>,
        Option<crate::physical::PhysicalCommit>,
        i64,
        Reply<()>,
    ),
    OperationCommit([u8; 16], Reply<()>),
    OperationPayloadFlushed([u8; 16], [u8; 32], i64, Reply<()>),
    FlushGroupOpen(RepositoryId, i64, Reply<i64>),
    FlushGroupMark(i64, i64, Reply<u64>),
    OperationPublish(Vec<[u8; 16]>, Reply<u64>),
    OperationReclaimPending(RepositoryId, i64, Reply<u64>),
    /// Quiesce-time compaction: drop superseded extent versions and mark
    /// unreferenced journal payload extents dead; returns `(payload_id,
    /// length_bytes)` for each extent that transitioned.
    ExtentCompactVolume(RepositoryId, [u8; 16], i64, Reply<Vec<([u8; 16], i64)>>),
    /// Records one payload's remote publication; fails when the payload is
    /// no longer referenced by any byte extent.
    PayloadPublished(crate::payload_remote::PayloadRemoteObject, Reply<()>),
    ExtentReplace(
        RepositoryId,
        mirage_types::InodeId,
        i64,
        Vec<crate::extent::ByteExtent>,
        i64,
        Reply<()>,
    ),
    ExtentReplaceEof(
        RepositoryId,
        mirage_types::InodeId,
        i64,
        u64,
        Vec<crate::extent::ByteExtent>,
        i64,
        Reply<()>,
    ),
    RemoteHeadObserve(crate::remote_observation::RemoteHead, Reply<()>),
    RemoteChangeRecord(crate::remote_observation::RemoteChange, Reply<()>),
    DivergenceRecord(crate::remote_observation::Divergence, Reply<()>),
    DivergenceResolve(
        RepositoryId,
        crate::remote_observation::DivergenceStatus,
        i64,
        Reply<()>,
    ),
    LeaseDeclare(crate::workspace_lease::WorkspaceLease, Reply<[u8; 16]>),
    LeaseVerify([u8; 16], u64, u64, Vec<u8>, i64, Reply<()>),
    LeaseRevoke([u8; 16], i64, Reply<()>),
    GcBoundSet(crate::gc_bounds::GcBound, Reply<()>),
    GcUnreachableMark(RepositoryId, String, [u8; 32], i64, i64, Reply<()>),
    GcUnreachableClear(RepositoryId, String, Reply<()>),
    UploadSessionCreate(crate::upload_session::UploadSession, Reply<()>),
    UploadSessionAdvance(
        [u8; 16],
        crate::upload_session::SessionPhase,
        crate::upload_session::SessionPhase,
        Option<String>,
        Option<String>,
        u64,
        u64,
        Option<String>,
        i64,
        Reply<()>,
    ),
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
                        Command::SetRepositoryVolumeMode(id, mode, reply) => {
                            respond(
                                reply,
                                repository::set_volume_mode(&mut connection, id, mode),
                            );
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
                        Command::DiskFloorSet(value, reply) => {
                            respond(reply, disk_floor::set_floor(&mut connection, value));
                        }
                        Command::DiskFloorClear(root, reply) => {
                            respond(reply, disk_floor::clear_floor(&mut connection, &root));
                        }
                        Command::DiskFloorRecordRun(value, reply) => {
                            respond(reply, disk_floor::record_run(&mut connection, value));
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
                        Command::NamespacePin(volume_id, inode, pinned_ns, reply) => {
                            respond(
                                reply,
                                namespace::pin(&connection, volume_id, inode, pinned_ns),
                            );
                        }
                        Command::NamespaceUnpin(volume_id, inode, reply) => {
                            respond(reply, namespace::unpin(&connection, volume_id, inode));
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
                        Command::NamespaceSeedManaged(
                            volume_id,
                            nodes,
                            commit_hash,
                            now_ns,
                            reply,
                        ) => {
                            respond(
                                reply,
                                namespace::seed_managed_volume(
                                    &mut connection,
                                    volume_id,
                                    nodes,
                                    &commit_hash,
                                    now_ns,
                                ),
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
                        Command::PhysicalReviveExtent(
                            id,
                            file_id,
                            slot,
                            length,
                            hash,
                            checksum,
                            now,
                            reply,
                        ) => {
                            respond(
                                reply,
                                physical::revive_extent(
                                    &mut connection,
                                    &id,
                                    &file_id,
                                    slot,
                                    length,
                                    hash,
                                    checksum,
                                    now,
                                ),
                            );
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
                        Command::OperationBegin(operation, payloads, reply) => {
                            respond(
                                reply,
                                operation::begin_operation(&mut connection, &operation, &payloads),
                            );
                        }
                        Command::OperationAllocSeq(volume_id, reply) => {
                            respond(
                                reply,
                                operation::allocate_device_seq(&mut connection, volume_id),
                            );
                        }
                        Command::MutationCommit(
                            extents,
                            operation,
                            payloads,
                            physical,
                            now,
                            reply,
                        ) => {
                            respond(
                                reply,
                                operation::commit_mutation(
                                    &mut connection,
                                    extents.as_ref(),
                                    &operation,
                                    &payloads,
                                    physical.as_ref(),
                                    now,
                                ),
                            );
                        }
                        Command::OperationCommit(operation_id, reply) => {
                            respond(
                                reply,
                                operation::commit_operation(&mut connection, &operation_id),
                            );
                        }
                        Command::OperationPayloadFlushed(payload_id, checksum, now, reply) => {
                            respond(
                                reply,
                                operation::mark_payload_flushed(
                                    &mut connection,
                                    &payload_id,
                                    &checksum,
                                    now,
                                ),
                            );
                        }
                        Command::FlushGroupOpen(volume_id, now, reply) => {
                            respond(
                                reply,
                                operation::open_flush_group(&mut connection, volume_id, now),
                            );
                        }
                        Command::FlushGroupMark(group_id, now, reply) => {
                            respond(
                                reply,
                                operation::mark_group_flushed(&mut connection, group_id, now),
                            );
                        }
                        Command::OperationPublish(ids, reply) => {
                            respond(reply, operation::mark_published(&mut connection, &ids));
                        }
                        Command::OperationReclaimPending(volume_id, now, reply) => {
                            respond(
                                reply,
                                operation::reclaim_pending(&mut connection, volume_id, now),
                            );
                        }
                        Command::PayloadPublished(record, reply) => {
                            respond(
                                reply,
                                crate::payload_remote::record_payload_publication(
                                    &mut connection,
                                    &record,
                                ),
                            );
                        }
                        Command::ExtentCompactVolume(volume_id, file_id, now, reply) => {
                            respond(
                                reply,
                                crate::extent::compact_volume(
                                    &mut connection,
                                    volume_id,
                                    file_id,
                                    now,
                                ),
                            );
                        }
                        Command::ExtentReplace(volume, inode, version, extents, now, reply) => {
                            respond(
                                reply,
                                crate::extent::replace_extents(
                                    &mut connection,
                                    volume,
                                    inode,
                                    version,
                                    &extents,
                                    now,
                                ),
                            );
                        }
                        Command::ExtentReplaceEof(
                            volume,
                            inode,
                            version,
                            eof,
                            extents,
                            now,
                            reply,
                        ) => {
                            respond(
                                reply,
                                crate::extent::replace_extents_with_eof(
                                    &mut connection,
                                    volume,
                                    inode,
                                    version,
                                    eof,
                                    &extents,
                                    now,
                                ),
                            );
                        }
                        Command::RemoteHeadObserve(head, reply) => {
                            respond(
                                reply,
                                crate::remote_observation::observe_head(&mut connection, &head),
                            );
                        }
                        Command::RemoteChangeRecord(change, reply) => {
                            respond(
                                reply,
                                crate::remote_observation::record_change(&mut connection, &change),
                            );
                        }
                        Command::DivergenceRecord(divergence, reply) => {
                            respond(
                                reply,
                                crate::remote_observation::record_divergence(
                                    &mut connection,
                                    &divergence,
                                ),
                            );
                        }
                        Command::DivergenceResolve(volume_id, status, now, reply) => {
                            respond(
                                reply,
                                crate::remote_observation::resolve_divergence(
                                    &mut connection,
                                    volume_id,
                                    status,
                                    now,
                                ),
                            );
                        }
                        Command::LeaseDeclare(lease, reply) => {
                            respond(
                                reply,
                                crate::workspace_lease::declare(&mut connection, &lease),
                            );
                        }
                        Command::LeaseVerify(lease_id, bytes, pages, evidence, now, reply) => {
                            respond(
                                reply,
                                crate::workspace_lease::mark_verified(
                                    &mut connection,
                                    &lease_id,
                                    bytes,
                                    pages,
                                    evidence,
                                    now,
                                ),
                            );
                        }
                        Command::LeaseRevoke(lease_id, now, reply) => {
                            respond(
                                reply,
                                crate::workspace_lease::revoke(&mut connection, &lease_id, now),
                            );
                        }
                        Command::GcBoundSet(bound, reply) => {
                            respond(reply, crate::gc_bounds::set_bound(&mut connection, &bound));
                        }
                        Command::GcUnreachableMark(volume, key, at_commit, after, now, reply) => {
                            respond(
                                reply,
                                crate::gc_bounds::mark_unreachable(
                                    &mut connection,
                                    volume,
                                    &key,
                                    at_commit,
                                    after,
                                    now,
                                ),
                            );
                        }
                        Command::GcUnreachableClear(volume, key, reply) => {
                            respond(
                                reply,
                                crate::gc_bounds::clear_unreachable(&mut connection, volume, &key),
                            );
                        }
                        Command::UploadSessionCreate(session, reply) => {
                            respond(
                                reply,
                                crate::upload_session::create_session(&mut connection, &session),
                            );
                        }
                        Command::UploadSessionAdvance(
                            session_id,
                            expected_phase,
                            phase,
                            upload_id,
                            uri,
                            chunk_offset,
                            committed,
                            error_class,
                            now,
                            reply,
                        ) => {
                            respond(
                                reply,
                                crate::upload_session::advance_phase(
                                    &mut connection,
                                    &session_id,
                                    expected_phase,
                                    phase,
                                    upload_id.as_deref(),
                                    uri.as_deref(),
                                    chunk_offset,
                                    committed,
                                    error_class.as_deref(),
                                    now,
                                ),
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

    pub(crate) fn set_repository_volume_mode(
        &self,
        repository_id: mirage_types::RepositoryId,
        mode: crate::repository::VolumeMode,
    ) -> Result<(), MirageError> {
        self.request(|reply| Command::SetRepositoryVolumeMode(repository_id, mode, reply))
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

    pub(crate) fn disk_floor_set(&self, floor: DiskFloor) -> Result<(), MirageError> {
        self.request(|reply| Command::DiskFloorSet(floor, reply))
    }

    pub(crate) fn disk_floor_clear(&self, volume_root: String) -> Result<bool, MirageError> {
        self.request(|reply| Command::DiskFloorClear(volume_root, reply))
    }

    pub(crate) fn disk_floor_record_run(&self, run: DiskFloorRun) -> Result<(), MirageError> {
        self.request(|reply| Command::DiskFloorRecordRun(run, reply))
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

    /// Pins an inode; descendants of a pinned directory inherit protection.
    pub fn namespace_pin(
        &self,
        volume_id: RepositoryId,
        inode: InodeId,
        pinned_ns: i64,
    ) -> Result<(), MirageError> {
        self.request(|reply| Command::NamespacePin(volume_id, inode, pinned_ns, reply))
    }

    /// Removes a pin; `false` when the inode was not pinned.
    pub fn namespace_unpin(
        &self,
        volume_id: RepositoryId,
        inode: InodeId,
    ) -> Result<bool, MirageError> {
        self.request(|reply| Command::NamespaceUnpin(volume_id, inode, reply))
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

    /// Managed-volume variant of [`namespace_seed`](Self::namespace_seed):
    /// seed + durable marker in one transaction; returns the existing
    /// marker's index hash when the volume was already seeded.
    pub fn namespace_seed_managed(
        &self,
        volume_id: RepositoryId,
        nodes: Vec<NamespaceSeedNode>,
        commit_hash: [u8; 32],
        now_ns: i64,
    ) -> Result<Option<[u8; 32]>, MirageError> {
        self.request(|reply| {
            Command::NamespaceSeedManaged(volume_id, nodes, commit_hash, now_ns, reply)
        })
    }

    /// Records a payload's remote publication; fails when the payload is no
    /// longer referenced by extents (the upload is then orphaned).
    pub fn payload_published(
        &self,
        record: crate::payload_remote::PayloadRemoteObject,
    ) -> Result<(), MirageError> {
        self.request(|reply| Command::PayloadPublished(record, reply))
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

    /// Transitions a dead extent back to alive after its bytes were
    /// re-staged, or inserts a fresh alive row when compaction reaped it.
    #[allow(clippy::too_many_arguments)]
    pub fn physical_revive_extent(
        &self,
        extent_id: [u8; 16],
        file_id: [u8; 16],
        slot_index: i64,
        length_bytes: i64,
        page_hash: mirage_types::PageHash,
        checksum: [u8; 32],
        now_ns: i64,
    ) -> Result<(), MirageError> {
        self.request(|reply| {
            Command::PhysicalReviveExtent(
                extent_id,
                file_id,
                slot_index,
                length_bytes,
                page_hash,
                checksum,
                now_ns,
                reply,
            )
        })
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

    /// Journals a pending operation with its payload references.
    pub fn operation_begin(
        &self,
        operation: OperationRecord,
        payloads: Vec<OperationPayloadRecord>,
    ) -> Result<(), MirageError> {
        self.request(|reply| Command::OperationBegin(operation, payloads, reply))
    }

    /// Allocates the next durable device-local sequence for the volume.
    /// Serialized through the single writer so interleaved callers always
    /// receive distinct values; the reserved sequence stays allocated even
    /// when the operation is never journaled.
    pub fn operation_alloc_seq(&self, volume_id: RepositoryId) -> Result<i64, MirageError> {
        self.request(|reply| Command::OperationAllocSeq(volume_id, reply))
    }

    /// One atomic mutation commit: extent replacement + pending operation +
    /// already-flushed payload records + optional physical-extent commit +
    /// commit transition in a single transaction. Callers must fsync the
    /// payload file before this runs.
    pub fn mutation_commit(
        &self,
        extents: Option<crate::operation::ExtentMutation>,
        operation: OperationRecord,
        payloads: Vec<OperationPayloadRecord>,
        physical: Option<crate::physical::PhysicalCommit>,
        now_ns: i64,
    ) -> Result<(), MirageError> {
        self.request(|reply| {
            Command::MutationCommit(extents, operation, payloads, physical, now_ns, reply)
        })
    }

    /// Quiesce-time compaction: drops superseded extent versions and marks
    /// unreferenced journal payload extents dead. Returns the transitioned
    /// `(payload_id, length_bytes)` pairs for file/budget cleanup.
    pub fn extent_compact_volume(
        &self,
        volume_id: RepositoryId,
        journal_file_id: [u8; 16],
        now_ns: i64,
    ) -> Result<Vec<([u8; 16], i64)>, MirageError> {
        self.request(|reply| {
            Command::ExtentCompactVolume(volume_id, journal_file_id, now_ns, reply)
        })
    }

    /// Marks a payload durable after its bytes are flushed to disk.
    pub fn operation_payload_flushed(
        &self,
        payload_id: [u8; 16],
        checksum: [u8; 32],
        now_ns: i64,
    ) -> Result<(), MirageError> {
        self.request(|reply| Command::OperationPayloadFlushed(payload_id, checksum, now_ns, reply))
    }

    /// Commits a pending operation; fails closed when any payload is
    /// unflushed.
    pub fn operation_commit(&self, operation_id: [u8; 16]) -> Result<(), MirageError> {
        self.request(|reply| Command::OperationCommit(operation_id, reply))
    }

    /// Opens a flush group capturing the volume's committed operations.
    pub fn flush_group_open(
        &self,
        volume_id: RepositoryId,
        now_ns: i64,
    ) -> Result<i64, MirageError> {
        self.request(|reply| Command::FlushGroupOpen(volume_id, now_ns, reply))
    }

    /// Acknowledges local durability for every operation in the group.
    pub fn flush_group_mark(&self, group_id: i64, now_ns: i64) -> Result<u64, MirageError> {
        self.request(|reply| Command::FlushGroupMark(group_id, now_ns, reply))
    }

    /// Advances flushed operations to published after the remote commit.
    pub fn operation_publish(&self, operation_ids: Vec<[u8; 16]>) -> Result<u64, MirageError> {
        self.request(|reply| Command::OperationPublish(operation_ids, reply))
    }

    /// Reclaims pending operations after their payloads are deleted.
    pub fn operation_reclaim_pending(
        &self,
        volume_id: RepositoryId,
        now_ns: i64,
    ) -> Result<u64, MirageError> {
        self.request(|reply| Command::OperationReclaimPending(volume_id, now_ns, reply))
    }

    /// Records an observed remote head; a backwards cursor is rejected.
    pub fn remote_head_observe(
        &self,
        head: crate::remote_observation::RemoteHead,
    ) -> Result<(), MirageError> {
        self.request(|reply| Command::RemoteHeadObserve(head, reply))
    }

    /// Records one remote change under its cursor (idempotent on cursor).
    pub fn remote_change_record(
        &self,
        change: crate::remote_observation::RemoteChange,
    ) -> Result<(), MirageError> {
        self.request(|reply| Command::RemoteChangeRecord(change, reply))
    }

    /// Records a divergence; both histories stay visible.
    pub fn divergence_record(
        &self,
        divergence: crate::remote_observation::Divergence,
    ) -> Result<(), MirageError> {
        self.request(|reply| Command::DivergenceRecord(divergence, reply))
    }

    /// Marks a live divergence resolved; the record is kept for audit.
    pub fn divergence_resolve(
        &self,
        volume_id: RepositoryId,
        status: crate::remote_observation::DivergenceStatus,
        now_ns: i64,
    ) -> Result<(), MirageError> {
        self.request(|reply| Command::DivergenceResolve(volume_id, status, now_ns, reply))
    }

    /// Declares a workspace lease (idempotent on prefix).
    pub fn lease_declare(
        &self,
        lease: crate::workspace_lease::WorkspaceLease,
    ) -> Result<[u8; 16], MirageError> {
        self.request(|reply| Command::LeaseDeclare(lease, reply))
    }

    /// Marks a lease verified with measured coverage and evidence.
    pub fn lease_verify(
        &self,
        lease_id: [u8; 16],
        bytes_verified: u64,
        pages_pinned: u64,
        evidence: Vec<u8>,
        now_ns: i64,
    ) -> Result<(), MirageError> {
        self.request(|reply| {
            Command::LeaseVerify(
                lease_id,
                bytes_verified,
                pages_pinned,
                evidence,
                now_ns,
                reply,
            )
        })
    }

    /// Revokes a lease; covered pages become eviction candidates.
    pub fn lease_revoke(&self, lease_id: [u8; 16], now_ns: i64) -> Result<(), MirageError> {
        self.request(|reply| Command::LeaseRevoke(lease_id, now_ns, reply))
    }

    /// Sets the retention bound for a history stream (idempotent upsert).
    pub fn gc_bound_set(&self, bound: crate::gc_bounds::GcBound) -> Result<(), MirageError> {
        self.request(|reply| Command::GcBoundSet(bound, reply))
    }

    /// Marks an object unreachable at a verified commit; removal is gated on
    /// a later verified commit plus the reclamation window.
    pub fn gc_unreachable_mark(
        &self,
        volume_id: RepositoryId,
        object_key: &str,
        at_commit: [u8; 32],
        reclaim_after_ns: i64,
        now_ns: i64,
    ) -> Result<(), MirageError> {
        self.request(|reply| {
            Command::GcUnreachableMark(
                volume_id,
                object_key.to_string(),
                at_commit,
                reclaim_after_ns,
                now_ns,
                reply,
            )
        })
    }

    /// Clears a reclaimed candidate after the remote delete lands.
    pub fn gc_unreachable_clear(
        &self,
        volume_id: RepositoryId,
        object_key: &str,
    ) -> Result<(), MirageError> {
        self.request(|reply| Command::GcUnreachableClear(volume_id, object_key.to_string(), reply))
    }

    /// Creates a durable upload session before the first remote call.
    pub fn upload_session_create(
        &self,
        session: crate::upload_session::UploadSession,
    ) -> Result<(), MirageError> {
        self.request(|reply| Command::UploadSessionCreate(session, reply))
    }

    /// Advances a session's phase, upload cursor, and error class.
    #[allow(clippy::too_many_arguments)]
    /// Phase transitions compare-and-swap on `expected_phase` — a stale
    /// driver cannot move a session that recovery already advanced.
    #[allow(clippy::too_many_arguments)]
    pub fn upload_session_advance(
        &self,
        session_id: [u8; 16],
        expected_phase: crate::upload_session::SessionPhase,
        phase: crate::upload_session::SessionPhase,
        remote_upload_id: Option<String>,
        session_uri: Option<String>,
        chunk_offset: u64,
        committed_bytes: u64,
        error_class: Option<String>,
        now_ns: i64,
    ) -> Result<(), MirageError> {
        self.request(|reply| {
            Command::UploadSessionAdvance(
                session_id,
                expected_phase,
                phase,
                remote_upload_id,
                session_uri,
                chunk_offset,
                committed_bytes,
                error_class,
                now_ns,
                reply,
            )
        })
    }

    /// Atomically replaces an inode's extent set at `version`; the durable
    /// head EOF is derived from the extent set (0 for an empty set).
    pub fn extent_replace(
        &self,
        volume_id: RepositoryId,
        inode: mirage_types::InodeId,
        version: i64,
        extents: Vec<crate::extent::ByteExtent>,
        now_ns: i64,
    ) -> Result<(), MirageError> {
        self.request(|reply| {
            Command::ExtentReplace(volume_id, inode, version, extents, now_ns, reply)
        })
    }

    /// Atomically replaces an inode's extent set with an explicit logical
    /// EOF — required when the file extends past the last extent (a
    /// truncate-grow leaves a zero-filled hole).
    pub fn extent_replace_eof(
        &self,
        volume_id: RepositoryId,
        inode: mirage_types::InodeId,
        version: i64,
        eof: u64,
        extents: Vec<crate::extent::ByteExtent>,
        now_ns: i64,
    ) -> Result<(), MirageError> {
        self.request(|reply| {
            Command::ExtentReplaceEof(volume_id, inode, version, eof, extents, now_ns, reply)
        })
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
