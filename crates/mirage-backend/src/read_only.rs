//! Read-only origin boundary.
//!
//! A publisher or third-party origin is wrapped in [`ReadOnlyOrigin`] so it
//! cannot be handed to a code path that publishes or deletes. Mutations fail
//! with `BackendErrorClass::Unsupported` before any provider call, and the
//! advertised capabilities say so.

use std::sync::Arc;

use async_trait::async_trait;
use mirage_types::{BackendHealthState, CheckedRange, ContentHash, RepositoryId};
use tokio_util::sync::CancellationToken;

use crate::{
    BackendCapabilities, BackendError, BackendErrorClass, BackendRead, DeletionProof, FetchClass,
    ObjectBackend, ObjectKind, ObjectStat, RemoteObjectRef, UploadSource,
};

pub struct ReadOnlyOrigin<B: ObjectBackend + ?Sized> {
    inner: Arc<B>,
}

impl<B: ObjectBackend + ?Sized> ReadOnlyOrigin<B> {
    #[must_use]
    pub fn new(inner: Arc<B>) -> Self {
        Self { inner }
    }

    #[must_use]
    pub fn inner(&self) -> &Arc<B> {
        &self.inner
    }
}

fn refused(operation: &str) -> BackendError {
    BackendError::new(
        BackendErrorClass::Unsupported,
        format!("{operation} refused: origin is read-only"),
    )
}

#[async_trait]
impl<B: ObjectBackend + ?Sized> ObjectBackend for ReadOnlyOrigin<B> {
    fn capabilities(&self) -> BackendCapabilities {
        self.inner.capabilities().read_only()
    }

    async fn read_range(
        &self,
        object: &RemoteObjectRef,
        range: CheckedRange,
        class: FetchClass,
        cancel: CancellationToken,
    ) -> Result<BackendRead, BackendError> {
        self.inner.read_range(object, range, class, cancel).await
    }

    async fn put_immutable(
        &self,
        _kind: ObjectKind,
        _source: UploadSource,
        _expected_hash: ContentHash,
        _cancel: CancellationToken,
    ) -> Result<RemoteObjectRef, BackendError> {
        Err(refused("put_immutable"))
    }

    async fn stat(&self, object: &RemoteObjectRef) -> Result<ObjectStat, BackendError> {
        self.inner.stat(object).await
    }

    async fn enumerate_commits(
        &self,
        repository: RepositoryId,
    ) -> Result<Vec<RemoteObjectRef>, BackendError> {
        self.inner.enumerate_commits(repository).await
    }

    async fn delete_immutable(
        &self,
        _object: &RemoteObjectRef,
        _proof: &DeletionProof,
        _cancel: CancellationToken,
    ) -> Result<(), BackendError> {
        Err(refused("delete_immutable"))
    }

    async fn health(&self) -> BackendHealthState {
        self.inner.health().await
    }
}
