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
