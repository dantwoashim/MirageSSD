use mirage_service::RepositoryActor;
use mirage_types::{GenerationId, RepositoryEvent, RepositoryId, RepositoryState};
use std::sync::Arc;
#[test]
fn conflicting_commands_serialize_and_stale_expectations_fail() {
    let actor = Arc::new(RepositoryActor::start(
        RepositoryId::from_bytes([1; 16]),
        RepositoryState::ReadyUnmounted,
        GenerationId(7),
    ));
    let handles: Vec<_> = (0..8)
        .map(|_| {
            let actor = actor.clone();
            std::thread::spawn(move || {
                actor.transition(
                    RepositoryState::ReadyUnmounted,
                    GenerationId(7),
                    RepositoryEvent::MountRequested,
                )
            })
        })
        .collect();
    let successes = handles
        .into_iter()
        .map(|handle| handle.join().unwrap())
        .filter(Result::is_ok)
        .count();
    assert_eq!(successes, 1);
    let snapshot = actor.snapshot().unwrap();
    assert_eq!(snapshot.state, RepositoryState::Mounting);
    assert_eq!(snapshot.operation_sequence, 1);
    drop(actor);
}
#[test]
fn shutdown_marks_inflight_mutation_for_recovery() {
    let actor = RepositoryActor::start(
        RepositoryId::from_bytes([2; 16]),
        RepositoryState::Updating,
        GenerationId(9),
    );
    assert_eq!(actor.shutdown().unwrap().state, RepositoryState::Recovering);
}
