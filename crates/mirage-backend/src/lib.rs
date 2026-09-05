//! Provider-neutral immutable-object contracts for MirageSSD backends.

#![forbid(unsafe_code)]

pub mod error;
pub mod object;
pub mod test_contract;
pub mod traits;

pub use error::{BackendError, BackendErrorClass};
pub use object::{
    BackendByteStream, BackendId, BackendRead, BackendResponseMetadata, DeletionProof, FetchClass,
    ImmutableRevision, ObjectKind, ObjectStat, ProviderObjectId, RemoteObjectRef, UploadSource,
};
pub use traits::ObjectBackend;
