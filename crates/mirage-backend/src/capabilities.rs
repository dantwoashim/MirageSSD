//! Explicitly advertised backend capabilities.
//!
//! A backend states what it can do; callers that need a capability check for
//! it before acting instead of discovering an `Unsupported` error mid-transaction.
//! Advertising a capability is a claim about the contract, not a measurement of
//! the provider's performance.

use serde::{Deserialize, Serialize};

/// Whether the backend may create or delete immutable objects.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MutationCapability {
    /// Reads only. `put_immutable` and `delete_immutable` fail with
    /// `BackendErrorClass::Unsupported` before contacting the provider.
    ReadOnly,
    /// Mirage-owned archive: may publish and, with a deletion proof, delete.
    Archive,
}

/// What identity the backend can attach to the bytes it returns.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RevisionIdentity {
    /// `observed_revision` echoes the requested revision; the per-frame content
    /// hash is the only identity proof (see backend-contract.md, check 11).
    Requested,
    /// The provider returns an object identity with the body that is compared
    /// against the request before bytes are collected.
    ProviderAttested,
}

/// Whether the backend can serve as a recovery source for a repository.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryCapability {
    /// Objects can be read by reference but the commit history cannot be
    /// enumerated; recovery needs an external catalog.
    ObjectsOnly,
    /// `enumerate_commits` lists the repository's commit objects, so the
    /// catalog can be rebuilt from the backend alone.
    EnumerableCommits,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct BackendCapabilities {
    pub mutation: MutationCapability,
    /// The backend honors bounded byte-range reads (a full-body reply to a
    /// range request is rejected, never silently collected).
    pub bounded_range_reads: bool,
    pub revision_identity: RevisionIdentity,
    pub recovery: RecoveryCapability,
}

impl BackendCapabilities {
    /// A Mirage-owned archive with bounded range reads and enumerable commits.
    pub const ARCHIVE: Self = Self {
        mutation: MutationCapability::Archive,
        bounded_range_reads: true,
        revision_identity: RevisionIdentity::Requested,
        recovery: RecoveryCapability::EnumerableCommits,
    };

    #[must_use]
    pub const fn read_only(self) -> Self {
        Self {
            mutation: MutationCapability::ReadOnly,
            ..self
        }
    }

    #[must_use]
    pub const fn can_publish(&self) -> bool {
        matches!(self.mutation, MutationCapability::Archive)
    }
}
