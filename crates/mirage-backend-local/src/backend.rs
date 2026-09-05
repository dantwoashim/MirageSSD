use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;
use bytes::Bytes;
use futures_util::StreamExt;
use mirage_backend::{
    BackendByteStream, BackendError, BackendErrorClass, BackendId, BackendRead,
    BackendResponseMetadata, DeletionProof, FetchClass, ImmutableRevision, ObjectBackend,
    ObjectKind, ObjectStat, RemoteObjectRef, UploadSource,
};
use mirage_types::{BackendHealthState, ByteCount, CheckedRange, ContentHash, RepositoryId};
use tokio_util::sync::CancellationToken;

use crate::layout::{kind_directory, object_id, object_path, repository_root};

const READ_CHUNK_LIMIT: u64 = 16 * 1024 * 1024;
static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone)]
pub struct LocalObjectBackend {
    backend_id: BackendId,
    repository_id: RepositoryId,
    repository_root: PathBuf,
}

impl LocalObjectBackend {
    pub fn open(root: &Path, repository_id: RepositoryId) -> Result<Self, BackendError> {
        let backend_id = BackendId::new("local").map_err(|error| {
            local_error(BackendErrorClass::Permanent, "local backend ID is invalid")
                .with_source(error)
        })?;
        let repository_root = repository_root(root, repository_id);
        std::fs::create_dir_all(&repository_root)
            .map_err(|error| io_error(error, "cannot create local repository root"))?;
        let repository_root = repository_root
            .canonicalize()
            .map_err(|error| io_error(error, "cannot resolve local repository root"))?;
        Ok(Self {
            backend_id,
            repository_id,
            repository_root,
        })
    }

    #[must_use]
    pub const fn repository_id(&self) -> RepositoryId {
        self.repository_id
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.repository_root
    }

    fn validate_ref(&self, object: &RemoteObjectRef) -> Result<PathBuf, BackendError> {
        if object.backend_id != self.backend_id {
            return Err(local_error(
                BackendErrorClass::Permanent,
                "object belongs to a different backend",
            ));
        }
        object_path(&self.repository_root, object)
    }

    fn reference(&self, kind: ObjectKind, hash: ContentHash, length: u64) -> RemoteObjectRef {
        RemoteObjectRef {
            backend_id: self.backend_id.clone(),
            provider_object_id: object_id(kind, hash),
            immutable_revision: Some(
                ImmutableRevision::new(hash.to_string()).expect("hash revision is valid"),
            ),
            byte_length: ByteCount::from_u64(length),
            content_hash: hash,
            kind,
        }
    }
}

#[async_trait]
impl ObjectBackend for LocalObjectBackend {
    async fn read_range(
        &self,
        object: &RemoteObjectRef,
        range: CheckedRange,
        _class: FetchClass,
        cancel: CancellationToken,
    ) -> Result<BackendRead, BackendError> {
        cancelled(&cancel)?;
        if range.len() > READ_CHUNK_LIMIT {
            return Err(local_error(
                BackendErrorClass::Permanent,
                "local range exceeds the bounded read window",
            ));
        }
        let path = self.validate_ref(object)?;
        let mut file = File::open(path)
            .map_err(|error| io_error(error, "cannot open immutable local object"))?;
        let length = file
            .metadata()
            .map_err(|error| io_error(error, "cannot stat immutable local object"))?
            .len();
        if length != object.byte_length.as_u64() || range.end_exclusive() > length {
            return Err(local_error(
                BackendErrorClass::Integrity,
                "local immutable object length or requested range is invalid",
            ));
        }
        file.seek(SeekFrom::Start(range.start()))
            .map_err(|error| io_error(error, "cannot seek immutable local object"))?;
        let allocation = usize::try_from(range.len()).map_err(|_| {
            local_error(
                BackendErrorClass::Permanent,
                "local range cannot fit memory",
            )
        })?;
        let mut bytes = vec![0_u8; allocation];
        file.read_exact(&mut bytes)
            .map_err(|error| io_error(error, "immutable local object range is short"))?;
        cancelled(&cancel)?;
        BackendRead::new(
            range,
            ByteCount::from_u64(range.len()),
            BackendResponseMetadata {
                observed_revision: object.immutable_revision.clone(),
                ..BackendResponseMetadata::default()
            },
            BackendByteStream::from_bytes(Bytes::from(bytes)),
        )
    }

    async fn put_immutable(
        &self,
        kind: ObjectKind,
        source: UploadSource,
        expected_hash: ContentHash,
        cancel: CancellationToken,
    ) -> Result<RemoteObjectRef, BackendError> {
        cancelled(&cancel)?;
        let length = source.length.as_u64();
        if length == 0 {
            return Err(local_error(
                BackendErrorClass::Permanent,
                "immutable objects cannot be empty",
            ));
        }
        let directory = self.repository_root.join(kind_directory(kind));
        std::fs::create_dir_all(&directory)
            .map_err(|error| io_error(error, "cannot create local object directory"))?;
        let target = directory.join(format!("{expected_hash}.bin"));
        if target.exists() {
            verify_existing(&target, expected_hash, length)?;
            return Ok(self.reference(kind, expected_hash, length));
        }
        let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let temporary = directory.join(format!(
            ".{expected_hash}.tmp-{}-{sequence}",
            std::process::id()
        ));
        let result = async {
            let mut file = OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&temporary)
                .map_err(|error| io_error(error, "cannot create local upload staging file"))?;
            let mut stream = source.into_stream();
            let mut hasher = blake3::Hasher::new();
            let mut written = 0_u64;
            while let Some(chunk) = stream.next().await {
                cancelled(&cancel)?;
                let chunk = chunk?;
                written = written.checked_add(chunk.len() as u64).ok_or_else(|| {
                    local_error(
                        BackendErrorClass::Permanent,
                        "local upload length overflows",
                    )
                })?;
                hasher.update(&chunk);
                file.write_all(&chunk)
                    .map_err(|error| io_error(error, "cannot write local upload staging file"))?;
            }
            if written != length || hasher.finalize().as_bytes() != expected_hash.as_bytes() {
                return Err(local_error(
                    BackendErrorClass::Integrity,
                    "local upload length or content hash mismatch",
                ));
            }
            file.flush()
                .map_err(|error| io_error(error, "cannot flush local upload staging file"))?;
            file.sync_all()
                .map_err(|error| io_error(error, "cannot sync local upload staging file"))?;
            drop(file);
            match std::fs::rename(&temporary, &target) {
                Ok(()) => Ok(()),
                Err(_error) if target.exists() => {
                    verify_existing(&target, expected_hash, length)?;
                    std::fs::remove_file(&temporary).map_err(|remove| {
                        io_error(remove, "cannot remove duplicate local staging file")
                    })
                }
                Err(error) => Err(io_error(error, "cannot publish immutable local object")),
            }
        }
        .await;
        if result.is_err() {
            let _ = std::fs::remove_file(&temporary);
        }
        result?;
        Ok(self.reference(kind, expected_hash, length))
    }

    async fn stat(&self, object: &RemoteObjectRef) -> Result<ObjectStat, BackendError> {
        let path = self.validate_ref(object)?;
        let (hash, length) = hash_file(&path)?;
        if hash != object.content_hash || length != object.byte_length.as_u64() {
            return Err(local_error(
                BackendErrorClass::Integrity,
                "local immutable object identity changed",
            ));
        }
        Ok(ObjectStat {
            byte_length: ByteCount::from_u64(length),
            content_hash: hash,
            immutable_revision: object.immutable_revision.clone(),
            kind: object.kind,
        })
    }

    async fn enumerate_commits(
        &self,
        repository: RepositoryId,
    ) -> Result<Vec<RemoteObjectRef>, BackendError> {
        if repository != self.repository_id {
            return Ok(Vec::new());
        }
        let directory = self
            .repository_root
            .join(kind_directory(ObjectKind::Commit));
        if !directory.exists() {
            return Ok(Vec::new());
        }
        let mut objects = Vec::new();
        for entry in std::fs::read_dir(directory)
            .map_err(|error| io_error(error, "cannot enumerate local commits"))?
        {
            let path = entry
                .map_err(|error| io_error(error, "cannot read local commit entry"))?
                .path();
            if path.extension().and_then(|value| value.to_str()) != Some("bin") {
                continue;
            }
            let (hash, length) = hash_file(&path)?;
            objects.push(self.reference(ObjectKind::Commit, hash, length));
        }
        objects.sort_by(|left, right| {
            left.provider_object_id
                .as_str()
                .cmp(right.provider_object_id.as_str())
        });
        Ok(objects)
    }

    async fn delete_immutable(
        &self,
        object: &RemoteObjectRef,
        proof: &DeletionProof,
        cancel: CancellationToken,
    ) -> Result<(), BackendError> {
        cancelled(&cancel)?;
        if proof.repository_id != self.repository_id || proof.object_hash != object.content_hash {
            return Err(local_error(
                BackendErrorClass::Permanent,
                "deletion proof does not authorize this local object",
            ));
        }
        let path = self.validate_ref(object)?;
        std::fs::remove_file(path)
            .map_err(|error| io_error(error, "cannot delete immutable local object"))
    }

    async fn health(&self) -> BackendHealthState {
        if self.repository_root.is_dir() {
            BackendHealthState::Healthy
        } else {
            BackendHealthState::Unavailable
        }
    }
}

fn verify_existing(
    path: &Path,
    expected: ContentHash,
    expected_length: u64,
) -> Result<(), BackendError> {
    let (actual, length) = hash_file(path)?;
    if actual != expected || length != expected_length {
        return Err(local_error(
            BackendErrorClass::Integrity,
            "existing local object conflicts with immutable identity",
        ));
    }
    Ok(())
}

fn hash_file(path: &Path) -> Result<(ContentHash, u64), BackendError> {
    let mut file =
        File::open(path).map_err(|error| io_error(error, "cannot open immutable local object"))?;
    let mut hasher = blake3::Hasher::new();
    let mut buffer = vec![0_u8; 1024 * 1024];
    let mut length = 0_u64;
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| io_error(error, "cannot hash immutable local object"))?;
        if read == 0 {
            break;
        }
        length = length.checked_add(read as u64).ok_or_else(|| {
            local_error(
                BackendErrorClass::Permanent,
                "local object length overflows",
            )
        })?;
        hasher.update(&buffer[..read]);
    }
    Ok((
        ContentHash::from_bytes(*hasher.finalize().as_bytes()),
        length,
    ))
}

fn cancelled(token: &CancellationToken) -> Result<(), BackendError> {
    if token.is_cancelled() {
        Err(local_error(
            BackendErrorClass::TransientTransport,
            "local backend operation canceled",
        ))
    } else {
        Ok(())
    }
}

pub(crate) fn local_error(class: BackendErrorClass, message: &'static str) -> BackendError {
    BackendError::new(class, message)
}

fn io_error(error: std::io::Error, context: &'static str) -> BackendError {
    let class = match error.kind() {
        std::io::ErrorKind::NotFound => BackendErrorClass::Missing,
        std::io::ErrorKind::PermissionDenied => BackendErrorClass::Permission,
        std::io::ErrorKind::UnexpectedEof => BackendErrorClass::Integrity,
        std::io::ErrorKind::Interrupted | std::io::ErrorKind::WouldBlock => {
            BackendErrorClass::TransientTransport
        }
        _ => BackendErrorClass::Permanent,
    };
    local_error(class, context).with_source(error)
}
