use async_trait::async_trait;
use mirage_types::{BackendHealthState, CheckedRange, ContentHash, RepositoryId};
use tokio_util::sync::CancellationToken;

use crate::{
    BackendError, BackendRead, DeletionProof, FetchClass, ObjectKind, ObjectStat, RemoteObjectRef,
    UploadSource,
};

/// Immutable-object origin/archive interface. Implementations are normally repository-scoped.
#[async_trait]
pub trait ObjectBackend: Send + Sync {
    async fn read_range(
        &self,
        object: &RemoteObjectRef,
        range: CheckedRange,
        class: FetchClass,
        cancel: CancellationToken,
    ) -> Result<BackendRead, BackendError>;

    async fn put_immutable(
        &self,
        kind: ObjectKind,
        source: UploadSource,
        expected_hash: ContentHash,
        cancel: CancellationToken,
    ) -> Result<RemoteObjectRef, BackendError>;

    async fn stat(&self, object: &RemoteObjectRef) -> Result<ObjectStat, BackendError>;

    async fn enumerate_commits(
        &self,
        repository: RepositoryId,
    ) -> Result<Vec<RemoteObjectRef>, BackendError>;

    async fn delete_immutable(
        &self,
        object: &RemoteObjectRef,
        proof: &DeletionProof,
        cancel: CancellationToken,
    ) -> Result<(), BackendError>;

    async fn health(&self) -> BackendHealthState;
}
