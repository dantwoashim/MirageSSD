//! Reusable behavioral contract for local, cloud, and test backends.

use std::sync::Arc;

use bytes::Bytes;
use mirage_types::{CheckedRange, ContentHash, RepositoryId};
use tokio_util::sync::CancellationToken;

use crate::{
    BackendErrorClass, DeletionProof, FetchClass, ObjectBackend, ObjectKind, UploadSource,
};

const CONTRACT_MAX_BYTES: u64 = 1024 * 1024;

/// Runs the provider-neutral immutable-object contract against an isolated backend instance.
pub async fn assert_object_backend_contract(
    backend: Arc<dyn ObjectBackend>,
    repository: RepositoryId,
) {
    let pack_bytes = Bytes::from_static(b"0123456789abcdef");
    let pack_hash = content_hash(&pack_bytes);
    let pack = backend
        .put_immutable(
            ObjectKind::Pack,
            UploadSource::from_bytes(pack_bytes.clone()),
            pack_hash,
            CancellationToken::new(),
        )
        .await
        .expect("put immutable pack");
    assert_eq!(pack.content_hash, pack_hash);
    assert_eq!(pack.byte_length.as_u64(), 16);

    let stat = backend.stat(&pack).await.expect("stat immutable pack");
    assert_eq!(stat.byte_length, pack.byte_length);
    assert_eq!(stat.content_hash, pack_hash);
    assert_eq!(stat.kind, ObjectKind::Pack);

    let requested = CheckedRange::new(3, 7).expect("contract range");
    let read = backend
        .read_range(
            &pack,
            requested,
            FetchClass::BlockingRead,
            CancellationToken::new(),
        )
        .await
        .expect("read exact immutable range");
    assert_eq!(read.requested_range, requested);
    assert_eq!(read.received_length.as_u64(), 7);
    let received = read
        .collect_bounded(CONTRACT_MAX_BYTES)
        .await
        .expect("collect bounded range");
    assert_eq!(&received[..], b"3456789");

    let wrong_hash = ContentHash::from_bytes([0x55; 32]);
    let mismatch = backend
        .put_immutable(
            ObjectKind::Pack,
            UploadSource::from_bytes(Bytes::from_static(b"hash mismatch")),
            wrong_hash,
            CancellationToken::new(),
        )
        .await
        .expect_err("put must verify expected content hash");
    assert_eq!(mismatch.class, BackendErrorClass::Integrity);

    let commit_bytes = Bytes::from_static(b"contract commit");
    let commit_hash = content_hash(&commit_bytes);
    let commit = backend
        .put_immutable(
            ObjectKind::Commit,
            UploadSource::from_bytes(commit_bytes),
            commit_hash,
            CancellationToken::new(),
        )
        .await
        .expect("put immutable commit");
    let commits = backend
        .enumerate_commits(repository)
        .await
        .expect("enumerate commits");
    assert!(commits.iter().any(|candidate| candidate == &commit));

    let proof = DeletionProof {
        repository_id: repository,
        object_hash: pack_hash,
        retained_root_set_hash: ContentHash::from_bytes([0x77; 32]),
        validated_at_sequence: 1,
    };
    backend
        .delete_immutable(&pack, &proof, CancellationToken::new())
        .await
        .expect("delete with matching proof");
    let missing = backend
        .stat(&pack)
        .await
        .expect_err("deleted object is absent");
    assert_eq!(missing.class, BackendErrorClass::Missing);
}

#[must_use]
pub fn content_hash(bytes: &[u8]) -> ContentHash {
    ContentHash::from_bytes(*blake3::hash(bytes).as_bytes())
}

/// Invokes the async contract without requiring a downstream async test runtime.
pub fn run_object_backend_contract(backend: Arc<dyn ObjectBackend>, repository: RepositoryId) {
    futures_executor::block_on(assert_object_backend_contract(backend, repository));
}

#[macro_export]
macro_rules! object_backend_contract_tests {
    ($factory:path) => {
        #[test]
        fn object_backend_contract() {
            let (backend, repository) = $factory();
            $crate::test_contract::run_object_backend_contract(backend, repository);
        }
    };
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use mirage_types::{BackendHealthState, ByteCount};

    use super::*;
    use crate::{
        BackendByteStream, BackendError, BackendId, BackendRead, BackendResponseMetadata,
        ImmutableRevision, ObjectStat, ProviderObjectId, RemoteObjectRef,
    };

    #[derive(Clone)]
    struct StoredObject {
        reference: RemoteObjectRef,
        bytes: Bytes,
    }

    #[derive(Default)]
    struct MemoryBackend {
        objects: Mutex<BTreeMap<String, StoredObject>>,
    }

    impl MemoryBackend {
        fn lock(&self) -> std::sync::MutexGuard<'_, BTreeMap<String, StoredObject>> {
            self.objects
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
        }
    }

    #[async_trait]
    impl ObjectBackend for MemoryBackend {
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
                    "operation canceled",
                ));
            }
            let stored = self
                .lock()
                .get(object.provider_object_id.as_str())
                .cloned()
                .ok_or_else(|| BackendError::missing("immutable object is missing"))?;
            if range.end_exclusive() > stored.bytes.len() as u64 {
                return Err(BackendError::permanent(
                    "range exceeds immutable object length",
                ));
            }
            let start = usize::try_from(range.start())
                .map_err(|_| BackendError::permanent("range start is not addressable"))?;
            let end = usize::try_from(range.end_exclusive())
                .map_err(|_| BackendError::permanent("range end is not addressable"))?;
            let bytes = stored.bytes.slice(start..end);
            BackendRead::new(
                range,
                ByteCount::from_u64(bytes.len() as u64),
                BackendResponseMetadata {
                    provider_request_id: Some("memory-request".to_string()),
                    observed_revision: stored.reference.immutable_revision,
                    transport_status: None,
                },
                BackendByteStream::from_bytes(bytes),
            )
        }

        async fn put_immutable(
            &self,
            kind: ObjectKind,
            source: UploadSource,
            expected_hash: ContentHash,
            cancel: CancellationToken,
        ) -> Result<RemoteObjectRef, BackendError> {
            if cancel.is_cancelled() {
                return Err(BackendError::new(
                    BackendErrorClass::TransientTransport,
                    "operation canceled",
                ));
            }
            let bytes = source.collect_bounded(CONTRACT_MAX_BYTES).await?;
            if content_hash(&bytes) != expected_hash {
                return Err(BackendError::integrity("upload content hash mismatch"));
            }
            let provider_id = format!("{}-{expected_hash}", kind.as_str());
            let reference = RemoteObjectRef {
                backend_id: BackendId::new("memory").expect("static backend id"),
                provider_object_id: ProviderObjectId::new(provider_id.clone())
                    .expect("derived provider id"),
                immutable_revision: Some(
                    ImmutableRevision::new(expected_hash.to_string()).expect("hash revision"),
                ),
                byte_length: ByteCount::from_u64(bytes.len() as u64),
                content_hash: expected_hash,
                kind,
            };
            self.lock().insert(
                provider_id,
                StoredObject {
                    reference: reference.clone(),
                    bytes,
                },
            );
            Ok(reference)
        }

        async fn stat(&self, object: &RemoteObjectRef) -> Result<ObjectStat, BackendError> {
            let stored = self
                .lock()
                .get(object.provider_object_id.as_str())
                .cloned()
                .ok_or_else(|| BackendError::missing("immutable object is missing"))?;
            Ok(ObjectStat {
                byte_length: stored.reference.byte_length,
                content_hash: stored.reference.content_hash,
                immutable_revision: stored.reference.immutable_revision,
                kind: stored.reference.kind,
            })
        }

        async fn enumerate_commits(
            &self,
            _repository: RepositoryId,
        ) -> Result<Vec<RemoteObjectRef>, BackendError> {
            Ok(self
                .lock()
                .values()
                .filter(|stored| stored.reference.kind == ObjectKind::Commit)
                .map(|stored| stored.reference.clone())
                .collect())
        }

        async fn delete_immutable(
            &self,
            object: &RemoteObjectRef,
            proof: &DeletionProof,
            cancel: CancellationToken,
        ) -> Result<(), BackendError> {
            if cancel.is_cancelled() {
                return Err(BackendError::new(
                    BackendErrorClass::TransientTransport,
                    "operation canceled",
                ));
            }
            if proof.object_hash != object.content_hash {
                return Err(BackendError::permanent(
                    "deletion proof does not match object",
                ));
            }
            self.lock()
                .remove(object.provider_object_id.as_str())
                .ok_or_else(|| BackendError::missing("immutable object is missing"))?;
            Ok(())
        }

        async fn health(&self) -> BackendHealthState {
            BackendHealthState::Healthy
        }
    }

    fn memory_backend() -> (Arc<dyn ObjectBackend>, RepositoryId) {
        (
            Arc::new(MemoryBackend::default()),
            RepositoryId::from_bytes([0x44; 16]),
        )
    }

    crate::object_backend_contract_tests!(memory_backend);

    #[test]
    fn bounded_stream_rejects_truncation_and_overrun() {
        let truncated = BackendByteStream::new(
            futures_util::stream::once(async { Ok(Bytes::from_static(b"short")) }),
            10,
        );
        let error = futures_executor::block_on(truncated.collect_bounded(10))
            .expect_err("truncated stream");
        assert_eq!(error.class, BackendErrorClass::Integrity);

        let overrun = BackendByteStream::new(
            futures_util::stream::once(async { Ok(Bytes::from_static(b"too long")) }),
            2,
        );
        let error =
            futures_executor::block_on(overrun.collect_bounded(10)).expect_err("overrun stream");
        assert_eq!(error.class, BackendErrorClass::Integrity);
    }
}
