use mirage_types::{
    GenerationId, MirageError, RepositoryEvent, RepositoryId, RepositoryState,
    transition_repository,
};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::JoinHandle;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RepositorySnapshot {
    pub repository_id: RepositoryId,
    pub state: RepositoryState,
    pub generation: GenerationId,
    pub operation_sequence: u64,
}
enum Message {
    Snapshot(Sender<RepositorySnapshot>),
    Transition {
        expected_state: RepositoryState,
        expected_generation: GenerationId,
        event: RepositoryEvent,
        reply: Sender<Result<RepositorySnapshot, MirageError>>,
    },
    Shutdown(Sender<RepositorySnapshot>),
}
pub struct RepositoryActor {
    sender: Sender<Message>,
    thread: Option<JoinHandle<()>>,
}
impl RepositoryActor {
    pub fn start(
        repository_id: RepositoryId,
        state: RepositoryState,
        generation: GenerationId,
    ) -> Self {
        let (sender, receiver) = mpsc::channel();
        let thread = std::thread::Builder::new()
            .name(format!("mirage-repository-{repository_id}"))
            .spawn(move || {
                run(
                    receiver,
                    RepositorySnapshot {
                        repository_id,
                        state,
                        generation,
                        operation_sequence: 0,
                    },
                )
            })
            .expect("repository actor thread creation");
        Self {
            sender,
            thread: Some(thread),
        }
    }
    pub fn snapshot(&self) -> Result<RepositorySnapshot, MirageError> {
        let (tx, rx) = mpsc::channel();
        self.sender
            .send(Message::Snapshot(tx))
            .map_err(|_| MirageError::internal_invariant("repository actor stopped"))?;
        rx.recv()
            .map_err(|_| MirageError::internal_invariant("repository actor did not reply"))
    }
    pub fn transition(
        &self,
        expected_state: RepositoryState,
        expected_generation: GenerationId,
        event: RepositoryEvent,
    ) -> Result<RepositorySnapshot, MirageError> {
        let (tx, rx) = mpsc::channel();
        self.sender
            .send(Message::Transition {
                expected_state,
                expected_generation,
                event,
                reply: tx,
            })
            .map_err(|_| MirageError::internal_invariant("repository actor stopped"))?;
        rx.recv()
            .map_err(|_| MirageError::internal_invariant("repository actor did not reply"))?
    }
    pub fn shutdown(mut self) -> Result<RepositorySnapshot, MirageError> {
        self.shutdown_inner()
    }
    fn shutdown_inner(&mut self) -> Result<RepositorySnapshot, MirageError> {
        let (tx, rx) = mpsc::channel();
        self.sender
            .send(Message::Shutdown(tx))
            .map_err(|_| MirageError::internal_invariant("repository actor stopped"))?;
        let state = rx
            .recv()
            .map_err(|_| MirageError::internal_invariant("repository actor did not drain"))?;
        if let Some(thread) = self.thread.take() {
            thread
                .join()
                .map_err(|_| MirageError::internal_invariant("repository actor panicked"))?;
        }
        Ok(state)
    }
}
impl std::fmt::Debug for RepositoryActor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RepositoryActor").finish_non_exhaustive()
    }
}
impl Drop for RepositoryActor {
    fn drop(&mut self) {
        if self.thread.is_some() {
            let _ = self.shutdown_inner();
        }
    }
}
fn run(receiver: Receiver<Message>, mut snapshot: RepositorySnapshot) {
    while let Ok(message) = receiver.recv() {
        match message {
            Message::Snapshot(reply) => {
                let _ = reply.send(snapshot);
            }
            Message::Transition {
                expected_state,
                expected_generation,
                event,
                reply,
            } => {
                let result = if snapshot.state != expected_state
                    || snapshot.generation != expected_generation
                {
                    Err(MirageError::repository_conflict(
                        "stale repository state or generation",
                    ))
                } else {
                    transition_repository(snapshot.state, event)
                        .map_err(|_| {
                            MirageError::repository_conflict("repository transition is invalid")
                        })
                        .map(|state| {
                            snapshot.state = state;
                            snapshot.operation_sequence =
                                snapshot.operation_sequence.saturating_add(1);
                            snapshot
                        })
                };
                let _ = reply.send(result);
            }
            Message::Shutdown(reply) => {
                if matches!(
                    snapshot.state,
                    RepositoryState::Importing
                        | RepositoryState::UploadingBase
                        | RepositoryState::VerifyingBase
                        | RepositoryState::Mounting
                        | RepositoryState::AdmittingSession
                        | RepositoryState::Updating
                ) {
                    snapshot.state = RepositoryState::Recovering;
                }
                let _ = reply.send(snapshot);
                break;
            }
        }
    }
}
