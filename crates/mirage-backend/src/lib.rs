//! Provider-neutral immutable-object contracts for MirageSSD backends.

#![forbid(unsafe_code)]

pub mod capabilities;
pub mod error;
pub mod object;
pub mod read_only;
pub mod test_contract;
pub mod traits;
pub mod verify;

pub use capabilities::{
    BackendCapabilities, MutationCapability, RecoveryCapability, RevisionIdentity,
};
pub use error::{BackendError, BackendErrorClass};
pub use object::{
    BackendByteStream, BackendId, BackendRead, BackendResponseMetadata, DeletionProof, FetchClass,
    ImmutableRevision, ObjectKind, ObjectStat, ProviderObjectId, RemoteObjectRef, UploadSource,
};
pub use read_only::ReadOnlyOrigin;
pub use traits::ObjectBackend;
pub use verify::verify_object_bytes;
