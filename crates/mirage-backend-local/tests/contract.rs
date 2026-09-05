use std::sync::Arc;

use bytes::Bytes;
use mirage_backend::{
    BackendErrorClass, BackendId, DeletionProof, FetchClass, ObjectBackend, ObjectKind,
    ProviderObjectId, RemoteObjectRef, UploadSource,
};
use mirage_backend_local::LocalObjectBackend;
use mirage_types::{ByteCount, CheckedRange, ContentHash, RepositoryId};
use tokio_util::sync::CancellationToken;

fn factory() -> (Arc<dyn ObjectBackend>, RepositoryId) {
    let root = tempfile::tempdir().expect("temp root").keep();
    let repository = RepositoryId::from_bytes([0x31; 16]);
    (
        Arc::new(LocalObjectBackend::open(&root, repository).expect("backend")),
        repository,
    )
}

mirage_backend::object_backend_contract_tests!(factory);

fn put(backend: &LocalObjectBackend, bytes: &[u8], kind: ObjectKind) -> RemoteObjectRef {
    let hash = ContentHash::from_bytes(*blake3::hash(bytes).as_bytes());
    futures_executor::block_on(backend.put_immutable(
        kind,
        UploadSource::from_bytes(Bytes::copy_from_slice(bytes)),
        hash,
        CancellationToken::new(),
    ))
    .expect("put")
}

#[test]
fn traversal_ids_and_short_or_tampered_objects_are_rejected() {
    let root = tempfile::tempdir().expect("root");
    let repository = RepositoryId::from_bytes([1; 16]);
    let backend = LocalObjectBackend::open(root.path(), repository).expect("backend");
    let object = put(&backend, b"immutable local bytes", ObjectKind::Pack);
    let malicious = RemoteObjectRef {
        backend_id: BackendId::new("local").unwrap(),
        provider_object_id: ProviderObjectId::new("../outside.bin").unwrap(),
        immutable_revision: object.immutable_revision.clone(),
        byte_length: object.byte_length,
        content_hash: object.content_hash,
        kind: ObjectKind::Pack,
    };
    let error =
        futures_executor::block_on(backend.stat(&malicious)).expect_err("traversal rejected");
    assert_eq!(error.class, BackendErrorClass::Permanent);

    let path = backend.root().join(object.provider_object_id.as_str());
    std::fs::write(&path, b"short").expect("truncate object");
    let error = futures_executor::block_on(backend.read_range(
        &object,
        CheckedRange::new(0, object.byte_length.as_u64()).unwrap(),
        FetchClass::BlockingRead,
        CancellationToken::new(),
    ))
    .expect_err("short read rejected");
    assert_eq!(error.class, BackendErrorClass::Integrity);
}

#[test]
fn concurrent_identical_puts_are_idempotent() {
    let root = tempfile::tempdir().expect("root");
    let repository = RepositoryId::from_bytes([2; 16]);
    let backend = Arc::new(LocalObjectBackend::open(root.path(), repository).expect("backend"));
    let bytes = Bytes::from(vec![0x42; 512 * 1024]);
    let hash = ContentHash::from_bytes(*blake3::hash(&bytes).as_bytes());
    let handles: Vec<_> = (0..8)
        .map(|_| {
            let backend = Arc::clone(&backend);
            let bytes = bytes.clone();
            std::thread::spawn(move || {
                futures_executor::block_on(backend.put_immutable(
                    ObjectKind::Pack,
                    UploadSource::from_bytes(bytes),
                    hash,
                    CancellationToken::new(),
                ))
            })
        })
        .collect();
    let objects: Vec<_> = handles
        .into_iter()
        .map(|handle| handle.join().unwrap().unwrap())
        .collect();
    assert!(objects.windows(2).all(|pair| pair[0] == pair[1]));
    assert_eq!(
        std::fs::read_dir(backend.root().join("packs"))
            .unwrap()
            .count(),
        1
    );
}

#[test]
fn conflicting_existing_object_and_cancellation_fail_closed() {
    let root = tempfile::tempdir().expect("root");
    let repository = RepositoryId::from_bytes([3; 16]);
    let backend = LocalObjectBackend::open(root.path(), repository).expect("backend");
    let object = put(&backend, b"original", ObjectKind::Manifest);
    let path = backend.root().join(object.provider_object_id.as_str());
    std::fs::write(path, b"tampered").expect("tamper");
    let conflict = futures_executor::block_on(backend.put_immutable(
        ObjectKind::Manifest,
        UploadSource::from_bytes(Bytes::from_static(b"original")),
        object.content_hash,
        CancellationToken::new(),
    ))
    .expect_err("conflict");
    assert_eq!(conflict.class, BackendErrorClass::Integrity);

    let cancel = CancellationToken::new();
    cancel.cancel();
    let cancelled = futures_executor::block_on(backend.put_immutable(
        ObjectKind::Pack,
        UploadSource::from_bytes(Bytes::from_static(b"cancelled")),
        ContentHash::from_bytes(*blake3::hash(b"cancelled").as_bytes()),
        cancel,
    ))
    .expect_err("cancelled");
    assert_eq!(cancelled.class, BackendErrorClass::TransientTransport);
}

#[test]
fn deletion_requires_exact_repository_and_object_proof() {
    let root = tempfile::tempdir().expect("root");
    let repository = RepositoryId::from_bytes([4; 16]);
    let backend = LocalObjectBackend::open(root.path(), repository).expect("backend");
    let object = put(&backend, b"delete me", ObjectKind::Profile);
    let wrong = DeletionProof {
        repository_id: RepositoryId::from_bytes([5; 16]),
        object_hash: object.content_hash,
        retained_root_set_hash: ContentHash::from_bytes([0; 32]),
        validated_at_sequence: 1,
    };
    let error = futures_executor::block_on(backend.delete_immutable(
        &object,
        &wrong,
        CancellationToken::new(),
    ))
    .expect_err("wrong proof");
    assert_eq!(error.class, BackendErrorClass::Permanent);
    assert!(futures_executor::block_on(backend.stat(&object)).is_ok());
}

#[test]
fn contradictory_reference_length_never_causes_partial_reads() {
    let root = tempfile::tempdir().expect("root");
    let repository = RepositoryId::from_bytes([6; 16]);
    let backend = LocalObjectBackend::open(root.path(), repository).expect("backend");
    let mut object = put(&backend, b"12345678", ObjectKind::Pack);
    object.byte_length = ByteCount::from_u64(99);
    let error = futures_executor::block_on(backend.read_range(
        &object,
        CheckedRange::new(0, 8).unwrap(),
        FetchClass::BlockingRead,
        CancellationToken::new(),
    ))
    .expect_err("contradiction");
    assert_eq!(error.class, BackendErrorClass::Integrity);
}
