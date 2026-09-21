use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use mirage_backend::{
    BackendCapabilities, BackendError, BackendRead, DeletionProof, FetchClass, ObjectBackend,
    ObjectKind, ObjectStat, RemoteObjectRef, UploadSource,
};
use mirage_types::{BackendHealthState, CheckedRange, ContentHash, RepositoryId};
use tokio_util::sync::CancellationToken;
use zeroize::Zeroizing;

use crate::backend::DriveObjectBackend;
use crate::http::HttpTransport;
use crate::native_http::{NativeHttpTransport, RetryingHttpTransport};

/// A Drive backend whose bearer token can be rotated at runtime: every call
/// clones the current inner backend out of the lock, drops the lock, and
/// delegates, so a token swap never blocks or aborts an in-flight read.
pub struct RefreshableDriveBackend {
    inner: RwLock<Arc<DriveObjectBackend>>,
    repository: RepositoryId,
    transport: Arc<dyn Fn() -> Result<Arc<dyn HttpTransport>, BackendError> + Send + Sync>,
    max_attempts: u32,
}

impl RefreshableDriveBackend {
    pub fn new(token: Zeroizing<String>, repository: RepositoryId) -> Result<Self, BackendError> {
        Self::with_transport_factory(
            token,
            repository,
            Arc::new(|| Ok(Arc::new(NativeHttpTransport::new()?) as Arc<dyn HttpTransport>)),
            5,
        )
    }

    /// Test/embedding seam: the transport factory is invoked on construction
    /// and on every `replace_token`.
    pub fn with_transport_factory(
        token: Zeroizing<String>,
        repository: RepositoryId,
        transport: Arc<dyn Fn() -> Result<Arc<dyn HttpTransport>, BackendError> + Send + Sync>,
        max_attempts: u32,
    ) -> Result<Self, BackendError> {
        let inner = Self::build(&transport, token, repository, max_attempts)?;
        Ok(Self {
            inner: RwLock::new(inner),
            repository,
            transport,
            max_attempts,
        })
    }

    fn build(
        transport: &Arc<dyn Fn() -> Result<Arc<dyn HttpTransport>, BackendError> + Send + Sync>,
        token: Zeroizing<String>,
        repository: RepositoryId,
        max_attempts: u32,
    ) -> Result<Arc<DriveObjectBackend>, BackendError> {
        let base = transport()?;
        let retrying = RetryingHttpTransport::new(base, max_attempts)?;
        Ok(Arc::new(DriveObjectBackend::new(
            Arc::new(retrying),
            token,
            repository,
        )?))
    }

    /// Builds a fresh transport+backend for `token` and swaps it in; a build
    /// failure keeps the previous backend live.
    pub fn replace_token(&self, token: Zeroizing<String>) -> Result<(), BackendError> {
        let next = Self::build(&self.transport, token, self.repository, self.max_attempts)?;
        let mut slot = self
            .inner
            .write()
            .map_err(|_| BackendError::permanent("Drive backend lock poisoned"))?;
        *slot = next;
        Ok(())
    }

    fn current(&self) -> Result<Arc<DriveObjectBackend>, BackendError> {
        self.inner
            .read()
            .map(|guard| Arc::clone(&guard))
            .map_err(|_| BackendError::permanent("Drive backend lock poisoned"))
    }
}

impl std::fmt::Debug for RefreshableDriveBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RefreshableDriveBackend")
            .field("repository", &self.repository)
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl ObjectBackend for RefreshableDriveBackend {
    fn capabilities(&self) -> BackendCapabilities {
        match self.current() {
            Ok(backend) => backend.capabilities(),
            Err(_) => BackendCapabilities::ARCHIVE,
        }
    }

    async fn read_range(
        &self,
        object: &RemoteObjectRef,
        range: CheckedRange,
        class: FetchClass,
        cancel: CancellationToken,
    ) -> Result<BackendRead, BackendError> {
        self.current()?
            .read_range(object, range, class, cancel)
            .await
    }

    async fn put_immutable(
        &self,
        kind: ObjectKind,
        source: UploadSource,
        expected_hash: ContentHash,
        cancel: CancellationToken,
    ) -> Result<RemoteObjectRef, BackendError> {
        self.current()?
            .put_immutable(kind, source, expected_hash, cancel)
            .await
    }

    async fn stat(&self, object: &RemoteObjectRef) -> Result<ObjectStat, BackendError> {
        self.current()?.stat(object).await
    }

    async fn enumerate_commits(
        &self,
        repository: RepositoryId,
    ) -> Result<Vec<RemoteObjectRef>, BackendError> {
        self.current()?.enumerate_commits(repository).await
    }

    async fn delete_immutable(
        &self,
        object: &RemoteObjectRef,
        proof: &DeletionProof,
        cancel: CancellationToken,
    ) -> Result<(), BackendError> {
        self.current()?
            .delete_immutable(object, proof, cancel)
            .await
    }

    async fn health(&self) -> BackendHealthState {
        match self.current() {
            Ok(backend) => backend.health().await,
            Err(_) => BackendHealthState::Unavailable,
        }
    }
}
