//! Directory-backed object backend: objects live at `root/<provider_object_id>`
//! — the local pack mirror layout produced by `repo import`. Used as the
//! non-cloud provider seam for managed engines in tests and diagnostics.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use mirage_backend::{
    BackendByteStream, BackendError, BackendErrorClass, BackendRead, BackendResponseMetadata,
    DeletionProof, FetchClass, ObjectBackend, ObjectKind, ObjectStat, RemoteObjectRef,
    UploadSource,
};
use mirage_types::{BackendHealthState, ByteCount, CheckedRange, ContentHash, RepositoryId};
use tokio_util::sync::CancellationToken;

const MAX_READ: u64 = 64 * 1024 * 1024;

fn permanent(message: &str) -> BackendError {
    BackendError::new(BackendErrorClass::Permanent, message)
}

pub struct DirectoryObjectBackend {
    root: PathBuf,
    repository: RepositoryId,
}

impl DirectoryObjectBackend {
    pub fn new(root: &Path, repository: RepositoryId) -> Result<Self, BackendError> {
        if !root.is_dir() {
            return Err(permanent("local object root is not a directory"));
        }
        Ok(Self {
            root: root.to_path_buf(),
            repository,
        })
    }

    fn path_for(&self, object: &RemoteObjectRef) -> Result<PathBuf, BackendError> {
        if object.backend_id.as_str() != "drive" && object.backend_id.as_str() != "local" {
            return Err(permanent("object belongs to a different backend"));
        }
        let id = object.provider_object_id.as_str();
        if id.contains("..") || id.contains(':') || id.starts_with(['/', '\\']) {
            return Err(permanent("provider object id escapes the object root"));
        }
        Ok(self.root.join(id))
    }
}

impl std::fmt::Debug for DirectoryObjectBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DirectoryObjectBackend")
            .field("root", &self.root)
            .field("repository", &self.repository)
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl ObjectBackend for DirectoryObjectBackend {
    fn capabilities(&self) -> mirage_backend::BackendCapabilities {
        mirage_backend::BackendCapabilities::ARCHIVE
    }

    async fn read_range(
        &self,
        object: &RemoteObjectRef,
        range: CheckedRange,
        _class: FetchClass,
        cancel: CancellationToken,
    ) -> Result<BackendRead, BackendError> {
        if cancel.is_cancelled() {
            return Err(BackendError::new(
                BackendErrorClass::TransientTransport,
                "local read cancelled",
            ));
        }
        if range.len() > MAX_READ {
            return Err(permanent("local range exceeds the bounded read window"));
        }
        let path = self.path_for(object)?;
        let file = std::fs::File::open(&path).map_err(|error| {
            BackendError::new(BackendErrorClass::TransientTransport, error.to_string())
        })?;
        let file_len = file
            .metadata()
            .map_err(|error| BackendError::new(BackendErrorClass::Permanent, error.to_string()))?
            .len();
        if range.end_exclusive() > file_len {
            return Err(permanent("range exceeds local object length"));
        }
        use std::io::{Read, Seek, SeekFrom};
        let mut file = file;
        file.seek(SeekFrom::Start(range.start())).map_err(|error| {
            BackendError::new(BackendErrorClass::TransientTransport, error.to_string())
        })?;
        let mut bytes = vec![0u8; range.len() as usize];
        file.read_exact(&mut bytes).map_err(|error| {
            BackendError::new(BackendErrorClass::TransientTransport, error.to_string())
        })?;
        BackendRead::new(
            range,
            ByteCount::from_u64(range.len()),
            BackendResponseMetadata {
                provider_request_id: None,
                observed_revision: object.immutable_revision.clone(),
                transport_status: None,
            },
            BackendByteStream::from_bytes(bytes.into()),
        )
    }

    async fn put_immutable(
        &self,
        kind: ObjectKind,
        source: UploadSource,
        expected_hash: ContentHash,
        _cancel: CancellationToken,
    ) -> Result<RemoteObjectRef, BackendError> {
        // Idempotent content-addressed write: `payloads/<hash>.bin` under
        // the root. An existing object is verified by hash and reused, which
        // makes retried publishes after a crash single-object.
        if kind != ObjectKind::Payload {
            return Err(permanent("local object backend only stores payloads"));
        }
        let bytes = source
            .collect_bounded(MAX_READ)
            .await
            .map_err(|_| permanent("upload exceeds the local object limit"))?;
        if blake3::hash(&bytes).as_bytes() != expected_hash.as_bytes() {
            return Err(permanent("upload bytes disagree with the declared hash"));
        }
        let name = format!("payloads/{expected_hash}.bin");
        let target = self.root.join(&name);
        if target.exists() {
            let existing = std::fs::read(&target).map_err(|error| {
                BackendError::new(BackendErrorClass::TransientTransport, error.to_string())
            })?;
            if blake3::hash(&existing).as_bytes() != expected_hash.as_bytes() {
                return Err(permanent("existing local object hash mismatch"));
            }
        } else {
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent).map_err(|error| {
                    BackendError::new(BackendErrorClass::TransientTransport, error.to_string())
                })?;
            }
            let temporary =
                self.root
                    .join(format!(".{}.tmp-{}", expected_hash, std::process::id()));
            std::fs::write(&temporary, &bytes).map_err(|error| {
                BackendError::new(BackendErrorClass::TransientTransport, error.to_string())
            })?;
            std::fs::rename(&temporary, &target).map_err(|error| {
                BackendError::new(BackendErrorClass::TransientTransport, error.to_string())
            })?;
        }
        Ok(RemoteObjectRef {
            backend_id: mirage_backend::BackendId::new("local")
                .map_err(|_| permanent("backend id invalid"))?,
            provider_object_id: mirage_backend::ProviderObjectId::new(name)
                .map_err(|_| permanent("provider object id invalid"))?,
            immutable_revision: None,
            byte_length: ByteCount::from_u64(u64::try_from(bytes.len()).unwrap_or(u64::MAX)),
            content_hash: expected_hash,
            kind,
        })
    }

    async fn stat(&self, object: &RemoteObjectRef) -> Result<ObjectStat, BackendError> {
        let path = self.path_for(object)?;
        let len = std::fs::metadata(&path)
            .map_err(|error| {
                BackendError::new(BackendErrorClass::TransientTransport, error.to_string())
            })?
            .len();
        Ok(ObjectStat {
            byte_length: ByteCount::from_u64(len),
            content_hash: object.content_hash,
            immutable_revision: object.immutable_revision.clone(),
            kind: object.kind,
        })
    }

    async fn enumerate_commits(
        &self,
        _repository: RepositoryId,
    ) -> Result<Vec<RemoteObjectRef>, BackendError> {
        Err(permanent("local object backend does not enumerate commits"))
    }

    async fn delete_immutable(
        &self,
        _object: &RemoteObjectRef,
        _proof: &DeletionProof,
        _cancel: CancellationToken,
    ) -> Result<(), BackendError> {
        Err(permanent("local object backend is read-only"))
    }

    async fn health(&self) -> BackendHealthState {
        if self.root.is_dir() {
            BackendHealthState::Healthy
        } else {
            BackendHealthState::Unavailable
        }
    }
}
