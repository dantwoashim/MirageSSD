use std::collections::HashSet;

use mirage_types::{
    BackendHealthEvent, BackendHealthState, PageEvent, PageState, RepositoryEvent, RepositoryState,
    SessionEvent, SessionState, StateMachine, UpdateEvent, UpdateState, transition_backend_health,
    transition_page, transition_repository, transition_session, transition_update,
};

macro_rules! assert_cartesian_contract {
    ($states:expr, $events:expr, $transition:ident, $machine:expr) => {{
        let state_names: HashSet<_> = $states.iter().map(|state| state.as_str()).collect();
        assert_eq!(
            state_names.len(),
            $states.len(),
            "state identifiers must be unique"
        );
        let event_names: HashSet<_> = $events.iter().map(|event| event.as_str()).collect();
        assert_eq!(
            event_names.len(),
            $events.len(),
            "event identifiers must be unique"
        );

        let mut covered = 0usize;
        for &state in $states {
            for &event in $events {
                covered += 1;
                match $transition(state, event) {
                    Ok(next) => {
                        assert!($states.contains(&next), "transition returned unknown state")
                    }
                    Err(error) => {
                        assert_eq!(error.machine, $machine);
                        assert_eq!(error.from, state.as_str());
                        assert_eq!(error.event, event.as_str());
                        assert!(!error.to_string().is_empty());
                    }
                }
            }
        }
        assert_eq!(covered, $states.len() * $events.len());
    }};
}

#[test]
fn every_repository_state_event_pair_has_one_result() {
    assert_cartesian_contract!(
        RepositoryState::ALL,
        RepositoryEvent::ALL,
        transition_repository,
        StateMachine::Repository
    );
}

#[test]
fn every_page_state_event_pair_has_one_result() {
    assert_cartesian_contract!(
        PageState::ALL,
        PageEvent::ALL,
        transition_page,
        StateMachine::Page
    );
}

#[test]
fn every_session_state_event_pair_has_one_result() {
    assert_cartesian_contract!(
        SessionState::ALL,
        SessionEvent::ALL,
        transition_session,
        StateMachine::Session
    );
}

#[test]
fn every_update_state_event_pair_has_one_result() {
    assert_cartesian_contract!(
        UpdateState::ALL,
        UpdateEvent::ALL,
        transition_update,
        StateMachine::Update
    );
}

#[test]
fn every_backend_health_state_event_pair_has_one_result() {
    assert_cartesian_contract!(
        BackendHealthState::ALL,
        BackendHealthEvent::ALL,
        transition_backend_health,
        StateMachine::BackendHealth
    );
}

#[test]
fn documented_happy_paths_are_exact() {
    assert_eq!(
        transition_repository(RepositoryState::Uninitialized, RepositoryEvent::StartImport),
        Ok(RepositoryState::Importing)
    );
    assert_eq!(
        transition_page(PageState::Fetching, PageEvent::FetchVerified),
        Ok(PageState::ResidentClean)
    );
    assert_eq!(
        transition_session(SessionState::Verifying, SessionEvent::VerificationPassed),
        Ok(SessionState::SealedReady)
    );
    assert_eq!(
        transition_update(UpdateState::CommitUploaded, UpdateEvent::CommitVerified),
        Ok(UpdateState::CommitVerified)
    );
    assert_eq!(
        transition_backend_health(
            BackendHealthState::Offline,
            BackendHealthEvent::ProbeSucceeded
        ),
        Ok(BackendHealthState::Healthy)
    );
}

#[test]
fn helpers_enforce_read_eviction_and_journal_invariants() {
    assert!(PageState::SessionPinned.is_readable());
    assert!(!PageState::SessionPinned.is_evictable());
    assert!(PageState::DirtyLocal.requires_journal());
    assert!(!PageState::Quarantined.is_readable());
    assert!(SessionState::Completed.is_terminal());
    assert!(UpdateState::Committed.is_terminal());
    assert!(UpdateState::Committed.requires_journal());
    assert!(!BackendHealthState::Unauthenticated.is_readable());
}
