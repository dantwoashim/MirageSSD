//! Core domain types, range contracts, identifiers, and error taxonomy for MirageSSD.

#![forbid(unsafe_code)]

pub mod bytes;
pub mod error;
pub mod hash;
pub mod id;
pub mod range;
pub mod retry;
pub mod state;
pub mod transition;

pub use bytes::{ByteCount, FileOffset, PageOrdinal, SlotIndex};
pub use error::{MirageError, MirageErrorKind, PublicErrorEnvelope};
pub use hash::{CommitHash, ContentHash, ManifestHash, PageHash};
pub use id::{
    CapsuleId, DeviceId, GenerationId, PackId, RepositoryId, SessionId, SpaceLeaseId, StableFileId,
    UpdateId,
};
pub use range::{CheckedRange, PageSlice};
pub use retry::RetryDisposition;
pub use state::{BackendHealthState, PageState, RepositoryState, SessionState, UpdateState};
pub use transition::{
    BackendHealthEvent, PageEvent, RepositoryEvent, SessionEvent, StateMachine, TransitionError,
    UpdateEvent, transition_backend_health, transition_page, transition_repository,
    transition_session, transition_update,
};

/// Placeholder version check or marker for foundational setup.
#[must_use]
pub const fn is_supported() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_supported() {
        assert!(is_supported());
    }
}
