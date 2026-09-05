use mirage_types::{MirageError, RepositoryState, SessionState, UpdateState};

pub(crate) fn repository(value: &str) -> Result<RepositoryState, MirageError> {
    RepositoryState::ALL
        .iter()
        .copied()
        .find(|state| state.as_str() == value)
        .ok_or_else(|| MirageError::integrity_mismatch("database repository state is unknown"))
}

pub(crate) fn session(value: &str) -> Result<SessionState, MirageError> {
    SessionState::ALL
        .iter()
        .copied()
        .find(|state| state.as_str() == value)
        .ok_or_else(|| MirageError::integrity_mismatch("database session state is unknown"))
}

pub(crate) fn update(value: &str) -> Result<UpdateState, MirageError> {
    UpdateState::ALL
        .iter()
        .copied()
        .find(|state| state.as_str() == value)
        .ok_or_else(|| MirageError::integrity_mismatch("database update state is unknown"))
}
