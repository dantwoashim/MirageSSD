//! Independent byte evidence for immutable publication. Provider metadata
//! (including a client-written hash property) is not a substitute for this.
use futures_util::StreamExt;
use mirage_types::CheckedRange;
use tokio_util::sync::CancellationToken;

use crate::{BackendError, BackendErrorClass, FetchClass, ObjectBackend, RemoteObjectRef};

/// Bytes requested per readback GET. The body is hashed as it streams, so
/// memory stays at one transport chunk regardless of this size; what the
/// window controls is the number of sequential round trips (a 32 MiB
/// payload took 32 serial Drive requests at 1 MiB).
const VERIFY_WINDOW: u64 = 16 * 1024 * 1024;

pub async fn verify_object_bytes(
    backend: &dyn ObjectBackend,
    object: &RemoteObjectRef,
    cancel: CancellationToken,
) -> Result<(), BackendError> {
    let mut offset = 0_u64;
    let mut hasher = blake3::Hasher::new();
    while offset < object.byte_length.as_u64() {
        if cancel.is_cancelled() {
            return Err(BackendError::new(
                BackendErrorClass::TransientTransport,
                "publication readback cancelled",
            ));
        }
        let length = (object.byte_length.as_u64() - offset).min(VERIFY_WINDOW);
        let range = CheckedRange::new(offset, length)
            .map_err(|_| BackendError::integrity("publication verification range overflow"))?;
        let read = backend
            .read_range(object, range, FetchClass::Maintenance, cancel.child_token())
            .await?;
        if read.requested_range != range || read.received_length.as_u64() != length {
            return Err(BackendError::integrity(
                "publication readback returned a different range",
            ));
        }
        let mut remaining = length;
        let mut stream = read.stream;
        while let Some(chunk) = stream.next().await {
            if cancel.is_cancelled() {
                return Err(BackendError::new(
                    BackendErrorClass::TransientTransport,
                    "publication readback cancelled",
                ));
            }
            let chunk = chunk?;
            remaining = remaining.checked_sub(chunk.len() as u64).ok_or_else(|| {
                BackendError::integrity("publication readback exceeded its range")
            })?;
            hasher.update(&chunk);
        }
        if remaining != 0 {
            return Err(BackendError::integrity("publication readback ended early"));
        }
        offset += length;
    }
    if hasher.finalize().as_bytes() != object.content_hash.as_bytes() {
        return Err(BackendError::integrity(
            "published bytes do not match their content hash",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::*;
    use async_trait::async_trait;
    use bytes::Bytes;
    use mirage_types::{BackendHealthState, ByteCount, ContentHash, RepositoryId};
    use std::sync::atomic::{AtomicU64, Ordering};

    struct UntrustedBackend {
        bytes: Bytes,
        largest_read: AtomicU64,
    }
    #[async_trait]
    impl ObjectBackend for UntrustedBackend {
        fn capabilities(&self) -> BackendCapabilities {
            BackendCapabilities::ARCHIVE
        }
        async fn read_range(
            &self,
            _: &RemoteObjectRef,
            range: CheckedRange,
            _: FetchClass,
            _: CancellationToken,
        ) -> Result<BackendRead, BackendError> {
            self.largest_read.fetch_max(range.len(), Ordering::Relaxed);
            let bytes = self
                .bytes
                .slice(range.start() as usize..range.end_exclusive() as usize);
            BackendRead::new(
                range,
                ByteCount::from_u64(range.len()),
                BackendResponseMetadata::default(),
                BackendByteStream::new(
                    futures_util::stream::once(async move { Ok(bytes) }),
                    range.len(),
                ),
            )
        }
        async fn stat(&self, object: &RemoteObjectRef) -> Result<ObjectStat, BackendError> {
            // This provider repeats the client's claimed hash, even for wrong bytes.
            Ok(ObjectStat {
                byte_length: object.byte_length,
                content_hash: object.content_hash,
                immutable_revision: None,
                kind: object.kind,
            })
        }
        async fn put_immutable(
            &self,
            _: ObjectKind,
            _: UploadSource,
            _: ContentHash,
            _: CancellationToken,
        ) -> Result<RemoteObjectRef, BackendError> {
            Err(BackendError::permanent("readback fixture is read-only"))
        }
        async fn enumerate_commits(
            &self,
            _: RepositoryId,
        ) -> Result<Vec<RemoteObjectRef>, BackendError> {
            Ok(Vec::new())
        }
        async fn delete_immutable(
            &self,
            _: &RemoteObjectRef,
            _: &DeletionProof,
            _: CancellationToken,
        ) -> Result<(), BackendError> {
            Err(BackendError::permanent("readback fixture is read-only"))
        }
        async fn health(&self) -> BackendHealthState {
            BackendHealthState::Healthy
        }
    }
    fn object(bytes: &[u8]) -> RemoteObjectRef {
        RemoteObjectRef {
            backend_id: BackendId::new("fixture").unwrap(),
            provider_object_id: ProviderObjectId::new("object").unwrap(),
            immutable_revision: None,
            byte_length: ByteCount::from_u64(bytes.len() as u64),
            content_hash: ContentHash::from_bytes(*blake3::hash(bytes).as_bytes()),
            kind: ObjectKind::Payload,
        }
    }
    #[test]
    fn rejects_same_size_corruption_despite_matching_provider_metadata() {
        let backend = UntrustedBackend {
            bytes: Bytes::from_static(b"evil"),
            largest_read: AtomicU64::new(0),
        };
        let reference = object(b"good");
        assert_eq!(
            futures_executor::block_on(backend.stat(&reference))
                .unwrap()
                .content_hash,
            reference.content_hash
        );
        assert!(
            futures_executor::block_on(verify_object_bytes(
                &backend,
                &reference,
                CancellationToken::new()
            ))
            .is_err()
        );
    }
    #[test]
    fn verifies_in_bounded_windows_and_cancels_before_io() {
        let bytes = vec![9; (VERIFY_WINDOW * 2 + 3) as usize];
        let reference = object(&bytes);
        let backend = UntrustedBackend {
            bytes: bytes.into(),
            largest_read: AtomicU64::new(0),
        };
        let cancelled = CancellationToken::new();
        cancelled.cancel();
        assert!(
            futures_executor::block_on(verify_object_bytes(&backend, &reference, cancelled))
                .is_err()
        );
        assert_eq!(backend.largest_read.load(Ordering::Relaxed), 0);
        futures_executor::block_on(verify_object_bytes(
            &backend,
            &reference,
            CancellationToken::new(),
        ))
        .unwrap();
        assert_eq!(backend.largest_read.load(Ordering::Relaxed), VERIFY_WINDOW);
    }
}
